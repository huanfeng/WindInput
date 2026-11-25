//! 逐文件裁决（设计 §7 第 3 步）。纯函数：不做 IO。
//!
//! 记号：L = 本地当前内容 hash，B = 上次同步时的 hash（状态库），R = 各远端设备清单里
//! lamport 最大的那条。「远端有变」要求 R 比本机已处理过的版本（状态库里该路径的 lamport）更新，
//! 否则一台久未同步的设备的陈旧清单会把已删除/已更新的文件「复活」。

use crate::manifest::{Entry, Manifest};
use crate::merge::is_mergeable;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 本地独有变化（含新建/修改）：上传。
    Upload,
    /// 本地删除：发布删除标记。
    PublishDelete,
    /// 远端有变：下载并写本地。
    Download(Entry),
    /// 远端删除：删本地。
    DeleteLocal(Entry),
    /// 两边结果相同：只更新状态库。
    Adopt(Entry),
    /// 双方都改且内容不同、类型可合并：三方合并（失败则退化为冲突副本）。
    Merge(Entry),
    /// 多个互不吸收的并发远端版本（按 order 从新到旧）：引擎逐个合并或存冲突副本。
    Multi(Vec<Entry>),
    /// 双方都改且不可合并：本地胜出，远端版本存为冲突副本。
    ConflictCopy(Entry),
}

pub struct PlanInput<'a> {
    /// 本地范围内文件：路径 → 内容 hash。
    pub local: &'a BTreeMap<String, String>,
    /// 状态库：路径 → 上次同步后的条目（含删除标记）。
    pub state: &'a BTreeMap<String, Entry>,
    /// 其它设备各自最新的清单。
    pub remotes: &'a [&'a Manifest],
    pub in_scope: &'a dyn Fn(&str) -> bool,
}

pub fn decide(inp: &PlanInput) -> Vec<(String, Action)> {
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    paths.extend(inp.local.keys().map(String::as_str));
    paths.extend(inp.state.keys().map(String::as_str));
    for m in inp.remotes {
        paths.extend(m.files.keys().map(String::as_str));
    }
    let mut out = Vec::new();
    for p in paths {
        if !(inp.in_scope)(p) {
            continue;
        }
        if let Some(a) = decide_one(p, inp) {
            out.push((p.to_string(), a));
        }
    }
    out
}

fn decide_one(path: &str, inp: &PlanInput) -> Option<Action> {
    let l = inp.local.get(path).map(String::as_str);
    let st = inp.state.get(path);
    let b = st.and_then(Entry::value);
    let all: Vec<&Entry> = inp
        .remotes
        .iter()
        .filter_map(|m| m.files.get(path))
        .collect();
    let seen = st.map(|s| s.order()).unwrap_or((0, ""));
    let local_changed = l != b;
    let mut cs: Vec<&Entry> = all
        .iter()
        .copied()
        .filter(|r| r.order() > seen && r.value() != b)
        .collect();
    cs.sort_by(|a, b| b.order().cmp(&a.order()));
    cs.dedup_by(|later, earlier| later.hash == earlier.hash);
    // 去掉已被吸收的：存在更新的条目 X，其直接 base 或 absorbed 含 Y 的 hash。
    // 局限：只看直接一层，不沿 base 链传递。
    let live: Vec<&Entry> = cs
        .iter()
        .copied()
        .filter(|y| {
            !all.iter().any(|x| {
                x.order() > y.order()
                    && x.hash != y.hash
                    && (x.base.as_deref() == Some(y.hash.as_str()) || x.absorbed.contains(&y.hash))
            })
        })
        .collect();
    if live.len() > 1 {
        return Some(Action::Multi(live.into_iter().cloned().collect()));
    }
    let r = live.first().copied();
    match (local_changed, r) {
        (false, None) => None,
        (true, None) => Some(if l.is_some() {
            Action::Upload
        } else {
            Action::PublishDelete
        }),
        (false, Some(r)) => Some(if r.deleted {
            Action::DeleteLocal(r.clone())
        } else {
            Action::Download(r.clone())
        }),
        (true, Some(r)) => {
            let rv = r.value();
            Some(if l == rv {
                Action::Adopt(r.clone())
            } else if l.is_none() {
                // 本地删、远端改：保留改
                Action::Download(r.clone())
            } else if rv.is_none() {
                // 本地改、远端删：保留改
                Action::Upload
            } else if is_mergeable(path) {
                Action::Merge(r.clone())
            } else {
                Action::ConflictCopy(r.clone())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(hash: &str, lamport: u64, dev: &str) -> Entry {
        Entry {
            hash: hash.into(),
            base: None,
            deleted: hash.is_empty(),
            lamport,
            device: dev.into(),
            absorbed: vec![],
        }
    }
    fn man(dev: &str, files: &[(&str, Entry)]) -> Manifest {
        Manifest {
            device: dev.into(),
            gen_: 1,
            lamport: 0,
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            nonce: String::new(),
            prev: String::new(),
        }
    }
    fn run(
        local: &[(&str, &str)],
        state: &[(&str, Entry)],
        remotes: &[Manifest],
    ) -> Vec<(String, Action)> {
        let l = local
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let s = state
            .iter()
            .map(|(a, b)| (a.to_string(), b.clone()))
            .collect();
        let rs: Vec<&Manifest> = remotes.iter().collect();
        decide(&PlanInput {
            local: &l,
            state: &s,
            remotes: &rs,
            in_scope: &|_| true,
        })
    }

    #[test]
    fn local_only_and_remote_only() {
        let st = [("a", e("h1", 1, "me"))];
        assert_eq!(
            run(&[("a", "h2")], &st, &[]),
            vec![("a".into(), Action::Upload)]
        );
        let r = man("o", &[("a", e("h3", 5, "o"))]);
        assert_eq!(
            run(&[("a", "h1")], &st, &[r]),
            vec![("a".into(), Action::Download(e("h3", 5, "o")))]
        );
    }

    #[test]
    fn stale_remote_does_not_resurrect_deleted_file() {
        let st = [("a", e("", 6, "me"))]; // 我已删除，lamport 6
        let stale = man("o", &[("a", e("h1", 3, "o"))]);
        assert!(run(&[], &st, &[stale]).is_empty());
    }

    #[test]
    fn both_changed_kinds() {
        let st = [("a.toml", e("h0", 1, "me")), ("t.json", e("h0", 1, "me"))];
        let r = man(
            "o",
            &[("a.toml", e("hr", 4, "o")), ("t.json", e("hr", 4, "o"))],
        );
        let got = run(&[("a.toml", "hl"), ("t.json", "hl")], &st, &[r]);
        assert!(matches!(got[0].1, Action::Merge(_)));
        assert!(matches!(got[1].1, Action::ConflictCopy(_)));
    }

    #[test]
    fn both_converged_adopts() {
        let st = [("a", e("h0", 1, "me"))];
        let r = man("o", &[("a", e("h9", 4, "o"))]);
        assert!(matches!(
            run(&[("a", "h9")], &st, &[r])[0].1,
            Action::Adopt(_)
        ));
    }

    #[test]
    fn delete_vs_modify_keeps_modify() {
        let st = [("a", e("h0", 1, "me"))];
        let del = man("o", &[("a", e("", 4, "o"))]);
        assert_eq!(run(&[("a", "hl")], &st, &[del])[0].1, Action::Upload);
        let modi = man("o", &[("a", e("hr", 4, "o"))]);
        assert!(matches!(run(&[], &st, &[modi])[0].1, Action::Download(_)));
    }

    #[test]
    fn unabsorbed_versions_from_several_devices_are_all_candidates() {
        let st = [("a", e("h0", 1, "me"))];
        let r1 = man("o1", &[("a", e("h1", 2, "o1"))]);
        let r2 = man("o2", &[("a", e("h2", 3, "o2"))]);
        assert_eq!(
            run(&[("a", "h0")], &st, &[r1, r2])[0].1,
            Action::Multi(vec![e("h2", 3, "o2"), e("h1", 2, "o1")])
        );
    }

    #[test]
    fn concurrent_remote_versions_are_both_kept() {
        let st = [("a.toml", e("h0", 1, "me"))];
        let ra = man("A", &[("a.toml", e("hA", 5, "A"))]);
        let rc = man("C", &[("a.toml", e("hC", 7, "C"))]);
        match &run(&[("a.toml", "h0")], &st, &[ra, rc])[0].1 {
            Action::Multi(v) => assert_eq!(v.len(), 2),
            a => panic!("{a:?}"),
        }
    }

    #[test]
    fn absorbed_version_is_not_a_candidate() {
        let st = [("a.toml", e("h0", 1, "me"))];
        let ra = man("A", &[("a.toml", e("hA", 5, "A"))]);
        let mut merged = e("hM", 8, "C");
        merged.absorbed = vec!["hA".into()];
        let rc = man("C", &[("a.toml", merged.clone())]);
        assert_eq!(
            run(&[("a.toml", "h0")], &st, &[ra.clone(), rc])[0].1,
            Action::Download(merged)
        );
        // 直接 base 也算吸收
        let mut next = e("hN", 9, "C");
        next.base = Some("hA".into());
        let rc = man("C", &[("a.toml", next.clone())]);
        assert_eq!(
            run(&[("a.toml", "h0")], &st, &[ra, rc])[0].1,
            Action::Download(next)
        );
    }

    #[test]
    fn first_sync_with_identical_content_adopts() {
        let r = man("o", &[("a", e("h1", 2, "o"))]);
        assert!(matches!(
            run(&[("a", "h1")], &[], &[r])[0].1,
            Action::Adopt(_)
        ));
    }
}
