//! P2 迁移（设计 §7.2）：redb `USER_WORDS` 一次性迁成 `user/<schema>/user.dict.yaml`。
//! 每个方案先折回已有的待处理操作；文件不存在 ⇒ 按旧序整文件写出（快）；文件已在（上一轮迁移
//! 中途失败，或用户自己放了文本）⇒ 逐条合并（权重取 max、补边界，已有行不挪）。全部完成后写
//! `state/migrated-p2`；没有标记就整体重做（重做是合并，幂等）。旧表只读保留到 P4。

use crate::Store;
use crate::store::USER_WORDS;
use std::collections::BTreeMap;
use std::path::Path;
use wind_userdata::record::Target;
use wind_userdata::runtime::UserWord;

pub const MIGRATED_P2: &str = "migrated-p2";

impl Store {
    /// 旧表全部用户词，按方案分组，组内按旧导出口径排序（order 升序、稳定；order 0 的旧记录在前）。
    #[doc(hidden)]
    pub fn legacy_user_rows(&self) -> anyhow::Result<BTreeMap<String, Vec<UserWord>>> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(USER_WORDS)?;
            let mut out: BTreeMap<String, Vec<(UserWord, u32)>> = BTreeMap::new();
            for item in t.range::<&str>(..)? {
                let (k, v) = item?;
                let Some((schema, code, text)) = crate::user_words::split_key(k.value()) else {
                    continue;
                };
                let Some((w, _, _, b, o)) = crate::user_words::dec_val_ordered(v.value()) else {
                    continue;
                };
                if code.is_empty() || text.is_empty() {
                    continue;
                }
                out.entry(schema.to_string()).or_default().push((
                    UserWord {
                        code: code.into(),
                        text: text.into(),
                        weight: w,
                        boundary: b,
                        order: 0,
                    },
                    o,
                ));
            }
            Ok(out
                .into_iter()
                .map(|(schema, mut v)| {
                    v.sort_by_key(|(_, o)| *o);
                    (schema, v.into_iter().map(|(w, _)| w).collect())
                })
                .collect())
        })
    }

    pub(crate) fn migrate_p2_if_needed(&self, state_dir: &Path) -> anyhow::Result<bool> {
        let marker = state_dir.join(MIGRATED_P2);
        if marker.is_file() {
            return Ok(false);
        }
        let ud = self.runtime().data().clone();
        let mut total = 0usize;
        for (schema, rows) in self.legacy_user_rows()? {
            self.ensure_user_text(&schema);
            if ud.pending(Target::User, &schema) > 0 {
                ud.fold(Target::User, &schema)?;
            }
            total += rows.len();
            if self.user_text_path(&schema).is_file() {
                self.runtime().import_user_rows(&schema, &rows)?;
            } else {
                let flat: Vec<(String, String, u64, i32)> = rows
                    .into_iter()
                    .map(|w| (w.code, w.text, w.boundary, w.weight))
                    .collect();
                ud.rewrite_text(Target::User, &schema, &flat)?;
            }
            self.bump_words_gen(&schema);
        }
        wind_userdata::state::atomic_write(&marker, b"1\n")?;
        tracing::info!("P2 迁移完成：{total} 条用户词从 redb 迁成文本");
        Ok(true)
    }

    /// P2 迁移是否已完成（标记 `migrated-p2` 在）。协调器据此决定能否把 `format_version` 升到 3。
    pub fn migrated_p2(&self) -> bool {
        self.state_dir().join(MIGRATED_P2).is_file()
    }

    /// 测试 / 探针：往旧 `USER_WORDS` 表写一条（模拟升级前的库）。
    #[doc(hidden)]
    pub fn legacy_put_user(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        weight: i32,
        boundary: u64,
        order: u32,
    ) -> anyhow::Result<()> {
        let key = crate::user_words::enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            txn.open_table(USER_WORDS)?.insert(
                key.as_str(),
                crate::user_words::enc_val_ordered(weight, 0, 0, boundary, order).as_slice(),
            )?;
            txn.commit()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    fn db(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("wind_mp2_{name}_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir_all(crate::store::derived_state_dir(&p));
        let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
        p
    }

    /// 造一个「旧版」库：用户词只在 USER_WORDS 里、没有迁移标记。
    fn legacy(name: &str, rows: &[(&str, &str, &str, i32, u64, u32)]) -> std::path::PathBuf {
        let p = db(name);
        {
            let s = Store::open(&p).unwrap();
            for (schema, code, text, w, b, o) in rows {
                s.legacy_put_user(schema, code, text, *w, *b, *o).unwrap();
            }
        }
        let _ = std::fs::remove_file(crate::store::derived_state_dir(&p).join(super::MIGRATED_P2));
        p
    }

    /// 词、权重、边界、同码先后、特殊文本都原样迁过去。
    #[test]
    fn migration_preserves_words_order_and_boundary() {
        let p = legacy(
            "basic",
            &[
                ("pinyin", "ni", "泥", 1, 0b1, 3),
                ("pinyin", "ni", "你", 1, 0b1, 2),
                ("pinyin", "nihao", "你好", 9, 0b101, 1),
                ("pinyin", "xianning", "西安宁", 5, 0, 0),
                ("pinyin", "tag", "#话题", 5, 0, 4),
                ("wb", "aaaa", "工\t具\n行", 7, 0, 1),
            ],
        );
        let s = Store::open(&p).unwrap();
        assert!(s.migrated_p2());
        let f = std::fs::read_to_string(s.user_text_path("pinyin")).unwrap();
        let body: Vec<&str> = f
            .lines()
            .skip_while(|l| *l != "# no comment")
            .skip(1)
            .collect();
        assert_eq!(
            body,
            vec![
                "xianning\t西安宁\t5",
                "ni hao\t你好\t9",
                "ni\t你\t1",
                "ni\t泥\t1",
                "tag\t#话题\t5",
            ],
            "order 0 的旧记录在前（同旧导出口径）"
        );
        assert_eq!(
            s.get_user_words("wb", "aaaa").unwrap()[0].text,
            "工\t具\n行"
        );
        let mut ni = s.get_user_words("pinyin", "ni").unwrap();
        ni.sort_by_key(|r| r.order);
        assert_eq!(
            ni.iter()
                .map(|r| (r.text.as_str(), r.boundary))
                .collect::<Vec<_>>(),
            vec![("你", 0b1), ("泥", 0b1)]
        );
        assert_eq!(s.get_user_words("pinyin", "tag").unwrap()[0].text, "#话题");
        assert_eq!(
            s.get_user_words("pinyin", "nihao").unwrap()[0].boundary,
            0b101
        );
    }

    /// 标记写入前崩溃，重跑合并：不重复、不丢、不覆盖两次启动之间新加 / 改过的词。
    #[test]
    fn rerun_after_crash_merges_without_loss() {
        let p = legacy(
            "rerun",
            &[("wb", "a", "工", 1, 0, 1), ("wb", "b", "了", 1, 0, 2)],
        );
        {
            let s = Store::open(&p).unwrap();
            s.add_user_word("wb", "c", "以", 3, 0).unwrap();
            s.update_user_word_weight("wb", "a", "工", 50).unwrap();
        }
        let _ = std::fs::remove_file(crate::store::derived_state_dir(&p).join(super::MIGRATED_P2));
        let s = Store::open(&p).unwrap();
        assert!(s.migrated_p2());
        let mut all: Vec<(String, i32)> = s
            .search_user_words_prefix("wb", "", 0)
            .unwrap()
            .into_iter()
            .map(|r| (r.text, r.weight))
            .collect();
        all.sort();
        assert_eq!(
            all,
            vec![("了".into(), 1), ("以".into(), 3), ("工".into(), 50)]
        );
        s.runtime()
            .data()
            .fold(wind_userdata::record::Target::User, "wb")
            .unwrap();
        let f = std::fs::read_to_string(s.user_text_path("wb")).unwrap();
        assert_eq!(f.matches("a\t工\t").count(), 1, "不重复成行");
    }

    /// 没有旧用户词：只写标记，不建空文件。
    #[test]
    fn empty_legacy_writes_marker_only() {
        let p = legacy("empty", &[]);
        let s = Store::open(&p).unwrap();
        assert!(s.migrated_p2());
        assert!(!s.user_text_path("pinyin").exists());
    }
}
