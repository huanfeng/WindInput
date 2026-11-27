//! 本机贡献的 diff、各设备贡献的物化、删除标记的吸收与回收（设计 §4.1–§4.4）。

use super::table::LearnedFile;
use super::{Contribution, Gen, Key, LearnKind, NormKey, Row};
use std::collections::{BTreeMap, BTreeSet};

/// 删除标记立出满这么久才可回收。
pub const TOMB_MIN_AGE_SECS: u64 = 2_592_000;

/// 文件变空时，基线至少这么多行才视为清空整类（立纪元），否则逐键立标记。
pub const CLEAR_MIN_ROWS: usize = 20;

/// 键的有效世代 = max(各设备该键的删除标记、`+` 行世代, 各设备的分组纪元)。
/// 行世代只可能在得知该世代后产生，计入它使标记回收后新世代的行仍然有效。
pub fn effective_gen(key: &NormKey, all: &[&Contribution]) -> Gen {
    let mut g = Gen::initial();
    for c in all {
        if c.epoch > g {
            g = c.epoch.clone();
        }
        if let Some((_, t)) = c.tombs.get(key)
            && *t > g
        {
            g = t.clone();
        }
        if let Some((_, r)) = c.rows.get(key)
            && r.gen_ > g
        {
            g = r.gen_.clone();
        }
    }
    g
}

/// 本机与已知贡献里出现过的最大世代（纪元、行、标记）。
fn max_known_gen(all: &[&Contribution]) -> Gen {
    let mut g = Gen::initial();
    for c in all {
        let gens = std::iter::once(&c.epoch)
            .chain(c.rows.values().map(|(_, r)| &r.gen_))
            .chain(c.tombs.values().map(|(_, t)| t));
        for x in gens {
            if *x > g {
                g = x.clone();
            }
        }
    }
    g
}

fn num(s: &str) -> u64 {
    s.trim().parse().unwrap_or(0)
}

fn field_idx(kind: LearnKind, name: &str) -> Option<usize> {
    kind.field_cols().iter().position(|c| *c == name)
}

fn field_num(kind: LearnKind, fields: &[String], name: &str) -> u64 {
    field_idx(kind, name)
        .and_then(|i| fields.get(i))
        .map_or(0, |s| num(s))
}

/// 取 F 的字段，`last_used` 与本机已有值取大。
fn take_fields(kind: LearnKind, mine: &[String], file: &[String]) -> Vec<String> {
    let mut out = file.to_vec();
    if let Some(i) = field_idx(kind, "last_used")
        && let (Some(m), Some(f)) = (mine.get(i), file.get(i))
        && num(m) > num(f)
    {
        out[i] = m.clone();
    }
    out
}

fn set_row(
    me: &mut Contribution,
    nk: &NormKey,
    key: &Key,
    gen_: Gen,
    count: u64,
    fields: Vec<String>,
) {
    let k = me
        .rows
        .get(nk)
        .map_or_else(|| key.clone(), |(k, _)| k.clone());
    me.rows.insert(
        nk.clone(),
        (
            k,
            Row {
                gen_,
                count,
                fields,
            },
        ),
    );
}

fn set_tomb(me: &mut Contribution, nk: &NormKey, key: &Key, gen_: Gen) {
    let k = me
        .tombs
        .get(nk)
        .map_or_else(|| key.clone(), |(k, _)| k.clone());
    me.tombs.insert(nk.clone(), (k, gen_));
}

/// 按 §4.4 把当前文件 `current` 相对基线 `base` 的变化计入本机贡献。
///
/// `peers` 必须是基线 `base` 物化时所依据的镜像（旧镜像）；新拉回的镜像只用于随后的
/// [`absorb_peer_deletes`] 与 [`materialize`]，否则用户打字时尚不知道的删除会让被删的词以小计数复活。
pub fn diff(
    me: &mut Contribution,
    current: &LearnedFile,
    base: &LearnedFile,
    now_ms: u64,
    dev: &str,
    peers: &[&Contribution],
) {
    let kind = me.kind;
    if current.rows.is_empty() && base.rows.len() >= CLEAR_MIN_ROWS {
        let mut all: Vec<&Contribution> = vec![&*me];
        all.extend_from_slice(peers);
        me.epoch = Gen::next(&max_known_gen(&all), now_ms, dev);
        me.rows.clear();
        me.tombs.clear();
        return;
    }
    let keys: BTreeSet<&NormKey> = current.rows.keys().chain(base.rows.keys()).collect();
    for nk in keys {
        let eff = {
            let mut all: Vec<&Contribution> = vec![&*me];
            all.extend_from_slice(peers);
            effective_gen(nk, &all)
        };
        match (current.rows.get(nk), base.rows.get(nk)) {
            (Some((k, fc, ff)), None) => set_row(me, nk, k, eff, *fc, ff.clone()),
            (None, Some((k, _, _))) => {
                me.rows.remove(nk);
                set_tomb(me, nk, k, Gen::next(&eff, now_ms, dev));
            }
            (Some((k, fc, ff)), Some((_, mc, mf))) => {
                let weight_lowered = field_idx(kind, "weight").is_some()
                    && field_num(kind, ff, "weight") < field_num(kind, mf, "weight");
                if fc < mc || weight_lowered {
                    let g = Gen::next(&eff, now_ms, dev);
                    set_tomb(me, nk, k, g.clone());
                    set_row(me, nk, k, g, *fc, ff.clone());
                } else if fc > mc || ff != mf {
                    let inc = fc - mc;
                    // 增量记在本机已有行上；该行若已被新知的删除作废，随后由 absorb 一并丢掉（删除优先）
                    match me.rows.get_mut(nk) {
                        Some((_, r)) => {
                            r.count = r.count.saturating_add(inc);
                            r.fields = take_fields(kind, &r.fields, ff);
                        }
                        _ => set_row(me, nk, k, eff, inc, ff.clone()),
                    }
                }
            }
            (None, None) => unreachable!(),
        }
    }
    let epoch = me.epoch.clone();
    me.rows.retain(|_, (_, r)| r.gen_ >= epoch);
    me.tombs.retain(|_, (_, g)| *g >= epoch);
}

/// 丢掉本机世代低于有效世代的 `+` 行（被别的设备删除或清空作废）。
pub fn absorb_peer_deletes(me: &mut Contribution, peers: &[&Contribution]) {
    let stale: Vec<NormKey> = {
        let mut all: Vec<&Contribution> = vec![&*me];
        all.extend_from_slice(peers);
        me.rows
            .iter()
            .filter(|(nk, (_, r))| r.gen_ < effective_gen(nk, &all))
            .map(|(nk, _)| nk.clone())
            .collect()
    };
    for nk in stale {
        me.rows.remove(&nk);
    }
}

/// 字段按数值取大或取小，同值时取字符串较小者，保证与输入顺序无关；空字段不参与，全空才输出空。
fn pick<'a>(vals: impl Iterator<Item = &'a str>, want_max: bool) -> String {
    vals.filter(|v| !v.trim().is_empty())
        .min_by(|a, b| {
            let o = num(a).cmp(&num(b));
            let o = if want_max { o.reverse() } else { o };
            o.then_with(|| a.cmp(b))
        })
        .unwrap_or("")
        .to_string()
}

/// 按 §4.3 物化全部设备贡献；头部、列序与已有键的写法取自 `template`。
/// 每台设备至多一份贡献，去重由调用方保证（重复传入会重复计数）。
pub fn materialize(kind: LearnKind, all: &[&Contribution], template: &LearnedFile) -> LearnedFile {
    let fcols = kind.field_cols();
    let keys: BTreeSet<&NormKey> = all.iter().flat_map(|c| c.rows.keys()).collect();
    let mut rows = BTreeMap::new();
    for nk in keys {
        let g = effective_gen(nk, all);
        let hit: Vec<&(Key, Row)> = all
            .iter()
            .filter_map(|c| c.rows.get(nk))
            .filter(|(_, r)| r.gen_ == g)
            .collect();
        if hit.is_empty() {
            continue;
        }
        // core 的 count 是 u32，超出的行会被当坏行跳过
        let count = hit
            .iter()
            .fold(0u64, |s, (_, r)| s.saturating_add(r.count))
            .min(u64::from(u32::MAX));
        let fields = fcols
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let vals = hit
                    .iter()
                    .map(|(_, r)| r.fields.get(i).map_or("", String::as_str));
                pick(vals, *name != "created_at")
            })
            .collect();
        let key = template.rows.get(nk).map_or_else(
            || {
                hit.iter()
                    .map(|(k, _)| k)
                    .min_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)))
                    .unwrap()
                    .clone()
            },
            |(k, _, _)| k.clone(),
        );
        rows.insert(nk.clone(), (key, count, fields));
    }
    LearnedFile {
        head: template.head.clone(),
        columns: template.columns.clone(),
        rows,
        bad_lines: template.bad_lines,
        kept: template.kept.clone(),
    }
}

/// 回收本机删除标记：立出满 30 天，且其他设备都已没有该键的旧世代 `+` 行。
pub fn gc_tombs(me: &mut Contribution, peers: &[&Contribution], now_ms: u64) {
    let min_age_ms = TOMB_MIN_AGE_SECS * 1000;
    me.tombs.retain(|nk, (_, g)| {
        now_ms < g.ms.saturating_add(min_age_ms)
            || peers
                .iter()
                .any(|p| p.rows.get(nk).is_some_and(|(_, r)| r.gen_ < *g))
    });
}
