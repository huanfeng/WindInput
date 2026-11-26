//! 贡献文件的文本格式、底稿/增量与压缩。
//!
//! 首行 `#wind-learn 1 <kind> epoch=<gen>`；`+` 行 = 键两列、世代、计数、各字段列，
//! `-` 行 = 键两列、世代；字段间一个制表符，行尾 `\n`，按规范化键排序。

use super::{Contribution, Gen, Key, LearnKind, Row, norm_key};

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    #[error("贡献文件版本高于本程序支持")]
    Version,
    #[error("贡献文件格式错误：{0}")]
    Parse(String),
    #[error("解压失败")]
    Inflate,
}

pub fn encode(c: &Contribution) -> String {
    let mut s = format!(
        "#wind-learn {VERSION} {} epoch={}\n",
        c.kind.as_str(),
        c.epoch.render()
    );
    for (k, r) in c.rows.values() {
        s.push_str(&format!(
            "+\t{}\t{}\t{}\t{}",
            k.0,
            k.1,
            r.gen_.render(),
            r.count
        ));
        for f in &r.fields {
            s.push('\t');
            s.push_str(f);
        }
        s.push('\n');
    }
    for (k, g) in c.tombs.values() {
        s.push_str(&format!("-\t{}\t{}\t{}\n", k.0, k.1, g.render()));
    }
    s
}

fn perr<T>(msg: impl Into<String>) -> Result<T, FormatError> {
    Err(FormatError::Parse(msg.into()))
}

pub fn decode(kind: LearnKind, s: &str) -> Result<Contribution, FormatError> {
    let mut lines = s.lines();
    let head = lines.next().unwrap_or("");
    let parts: Vec<&str> = head.split(' ').collect();
    if parts.len() != 4 || parts[0] != "#wind-learn" {
        return perr("缺少文件头");
    }
    let ver: u32 = parts[1].parse().or_else(|_| perr::<u32>("版本号无效"))?;
    if ver > VERSION {
        return Err(FormatError::Version);
    }
    if parts[2] != kind.as_str() {
        return perr(format!("种类不符：{}", parts[2]));
    }
    let epoch = parts[3]
        .strip_prefix("epoch=")
        .and_then(Gen::parse)
        .map_or_else(|| perr("epoch 无效"), Ok)?;

    let mut c = Contribution {
        kind,
        epoch,
        rows: Default::default(),
        tombs: Default::default(),
    };
    let nfields = kind.field_cols().len();
    for (i, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        let ln = i + 2;
        let cols: Vec<&str> = line.split('\t').collect();
        let gen_at = |idx: usize| {
            Gen::parse(cols[idx]).map_or_else(|| perr(format!("第 {ln} 行世代无效")), Ok)
        };
        match cols[0] {
            "+" => {
                if cols.len() != 5 + nfields {
                    return perr(format!("第 {ln} 行列数不符"));
                }
                let gen_ = gen_at(3)?;
                let count: u64 = cols[4]
                    .parse()
                    .or_else(|_| perr::<u64>(format!("第 {ln} 行计数无效")))?;
                let key = Key(cols[1].to_string(), cols[2].to_string());
                let row = Row {
                    gen_,
                    count,
                    fields: cols[5..].iter().map(|f| f.to_string()).collect(),
                };
                c.rows.insert(norm_key(kind, &key), (key, row));
            }
            "-" => {
                if cols.len() != 4 {
                    return perr(format!("第 {ln} 行列数不符"));
                }
                let g = gen_at(3)?;
                let key = Key(cols[1].to_string(), cols[2].to_string());
                c.tombs.insert(norm_key(kind, &key), (key, g));
            }
            _ => return perr(format!("第 {ln} 行类型未知")),
        }
    }
    Ok(c)
}

/// 底稿叠加增量：`+` 行按键覆盖；`-` 行写入标记，世代不小于底稿行时同时去掉该行；epoch 取大。
pub fn overlay(base: &Contribution, delta: &Contribution) -> Contribution {
    let mut out = base.clone();
    for (nk, (k, r)) in &delta.rows {
        out.rows.insert(nk.clone(), (k.clone(), r.clone()));
    }
    for (nk, (k, g)) in &delta.tombs {
        if out.rows.get(nk).is_some_and(|(_, r)| *g >= r.gen_) {
            out.rows.remove(nk);
        }
        out.tombs.insert(nk.clone(), (k.clone(), g.clone()));
    }
    if delta.epoch > out.epoch {
        out.epoch = delta.epoch.clone();
    }
    out
}

/// `full` 相对 `last_base` 的增量：新增或变化的 `+` 行与 `-` 标记，epoch 取 `full` 的。
pub fn split(full: &Contribution, last_base: &Contribution) -> Contribution {
    // 底稿有而 full 没有的行，必须有标记或纪元提升相伴（Task 4 的 diff 保证）。
    debug_assert!(
        last_base.rows.keys().all(|k| full.rows.contains_key(k)
            || full.tombs.contains_key(k)
            || full.epoch > last_base.epoch),
        "行消失却无标记"
    );
    Contribution {
        kind: full.kind,
        epoch: full.epoch.clone(),
        rows: full
            .rows
            .iter()
            .filter(|(k, v)| last_base.rows.get(*k) != Some(v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        tombs: full
            .tombs
            .iter()
            .filter(|(k, v)| last_base.tombs.get(*k) != Some(v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    }
}

/// 增量过多、过大或底稿过旧时应合并成新底稿。
pub fn should_compact(
    base_rows: usize,
    delta_rows: usize,
    delta_bytes: usize,
    base_age_secs: u64,
) -> bool {
    delta_rows * 4 > base_rows || delta_bytes > 256 * 1024 || base_age_secs > 7 * 24 * 3600
}

pub fn deflate(b: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(b, 6)
}

pub fn inflate(b: &[u8]) -> Result<Vec<u8>, FormatError> {
    miniz_oxide::inflate::decompress_to_vec(b).map_err(|_| FormatError::Inflate)
}
