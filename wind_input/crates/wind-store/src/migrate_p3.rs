//! P3 迁移（设计 §7.2）：redb `SHADOW` → `user/<schema>/shadow.dict.yaml`（打开时做）；`PHRASES` →
//! `user/phrases.dict.yaml`（要系统短语 TOML 才分得清「系统行被改过没有」，见 phrases.rs，首次
//! `set_system_phrases` 时做）。文件已有内容 ⇒ 同组以文件为准（上一轮失败后的写入不被旧表盖掉）。
//! 各自完成写标记；旧表只读保留到 P4。

use crate::Store;
use crate::shadow::ShadowRecord;
use crate::store::SHADOW;
use std::collections::BTreeMap;
use std::path::Path;

pub const MIGRATED_P3_SHADOW: &str = "migrated-p3-shadow";
pub const MIGRATED_P3_PHRASES: &str = "migrated-p3-phrases";

/// 全部迁移完成时 [`Store::format_version_reached`] 的值。
pub const FULLY_MIGRATED_VERSION: u32 = 5;

/// 旧库链各阶段的完成标记（只在旧库链进行中作断点续做用；`format_version` 写到 5 后删除）。
pub fn all_migration_markers() -> [&'static str; 5] {
    ALL_MIGRATION_MARKERS
}

/// 全部迁移的完成标记。都在 ⇒ 旧 redb 不再需要打开。
pub(crate) const ALL_MIGRATION_MARKERS: [&str; 5] = [
    crate::migrate_p1::MIGRATED_P1,
    crate::migrate_p2::MIGRATED_P2,
    MIGRATED_P3_SHADOW,
    MIGRATED_P3_PHRASES,
    crate::migrate_p4::MIGRATED_P4,
];

impl Store {
    /// 短语迁移（`set_system_phrases` 里调）：标记缺失才做。
    pub(crate) fn migrate_p3_phrases_if_needed(
        &self,
        sys: &[crate::phrases::SystemPhrase],
    ) -> anyhow::Result<()> {
        let marker = self.state_dir().join(MIGRATED_P3_PHRASES);
        // 旧库链已完成（meta.toml 记过；写到 5 后标记被删）⇒ 不再读旧库
        if marker.is_file() || self.legacy_chain_done() || self.runtime().data().readonly() {
            return Ok(());
        }
        let n = self.merge_legacy_phrases(sys)?;
        wind_userdata::state::atomic_write(&marker, b"1\n")?;
        tracing::info!("P3 短语迁移完成：{n} 行从 redb 迁成文本");
        self.release_legacy_db_if_migrated();
        Ok(())
    }

    /// 按迁移进度应达到的 `format_version`：P1 → 2，+P2 → 3，+P3 → 4，+P4a → 5；P1 都没迁完
    /// ⇒ None（不升）。协调器打开库时、设定系统短语（触发短语迁移）之后各升一次。
    pub fn format_version_reached(&self) -> Option<u32> {
        if self.legacy_chain_done() {
            return Some(FULLY_MIGRATED_VERSION);
        }
        match (
            self.migrated_p1(),
            self.migrated_p2(),
            self.migrated_p3(),
            self.migrated_p4(),
        ) {
            (true, true, true, true) => Some(FULLY_MIGRATED_VERSION),
            (true, true, true, false) => Some(4),
            (true, true, false, _) => Some(3),
            (true, false, _, _) => Some(2),
            _ => None,
        }
    }

    /// 没有系统短语 TOML 时也要把短语迁过来：按当前（可能为空的）系统条目迁移，不改动它们。
    pub fn migrate_phrases_now(&self) -> anyhow::Result<()> {
        let sys = self
            .phrase_sys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.migrate_p3_phrases_if_needed(&sys)
    }

    /// P3 迁移是否已完成（Shadow 与短语两个标记都在）。协调器据此把 `format_version` 升到 4。
    pub fn migrated_p3(&self) -> bool {
        self.state_dir().join(MIGRATED_P3_SHADOW).is_file()
            && self.state_dir().join(MIGRATED_P3_PHRASES).is_file()
    }

    pub(crate) fn migrate_p3_shadow_if_needed(&self, state_dir: &Path) -> anyhow::Result<bool> {
        let marker = state_dir.join(MIGRATED_P3_SHADOW);
        if marker.is_file() {
            return Ok(false);
        }
        let by_schema: BTreeMap<String, Vec<(String, ShadowRecord)>> = self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(SHADOW)?;
            let mut out: BTreeMap<String, Vec<(String, ShadowRecord)>> = BTreeMap::new();
            for item in t.range::<&str>(..)? {
                let (k, v) = item?;
                let Some((schema, code)) = k.value().split_once('\u{0}') else {
                    continue;
                };
                let Ok(rec) = serde_json::from_slice::<ShadowRecord>(v.value()) else {
                    continue;
                };
                if !rec.is_empty() {
                    out.entry(schema.to_string())
                        .or_default()
                        .push((code.to_string(), rec));
                }
            }
            Ok(out)
        })?;
        let mut total = 0usize;
        for (schema, rules) in by_schema {
            if wind_userdata::engine::check_schema_id(&schema).is_err() {
                tracing::warn!("P3 迁移跳过非法方案 id 的 Shadow 规则");
                continue;
            }
            total += rules.len();
            self.merge_shadow_rules(&schema, rules)?;
        }
        wind_userdata::state::atomic_write(&marker, b"1\n")?;
        tracing::info!("P3 Shadow 迁移完成：{total} 组规则从 redb 迁成文本");
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;
    use crate::shadow::{ShadowPin, ShadowRecord};

    fn db(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("wind_mp3_{name}_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir_all(crate::store::derived_state_dir(&p));
        let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
        p
    }

    fn remove_marker(p: &std::path::Path, m: &str) {
        let _ = std::fs::remove_file(crate::store::derived_state_dir(p).join(m));
    }

    fn pin(w: &str, id: Option<&str>, pos: usize) -> ShadowPin {
        ShadowPin {
            word: w.into(),
            cand_id: id.map(String::from),
            position: pos,
        }
    }

    /// 旧 SHADOW 表迁成文本：每方案一文件、规则与顺序原样；完成写标记；重跑幂等。
    #[test]
    fn shadow_table_migrates_to_text() {
        let p = db("shadow");
        {
            let s = Store::open(&p).unwrap();
            s.legacy_put_shadow(
                "wb",
                "aa",
                &ShadowRecord {
                    pinned: vec![pin("工", None, 1), pin("恭", None, 0)],
                    deleted: vec!["式".into()],
                },
            )
            .unwrap();
            s.legacy_put_shadow(
                "pinyin",
                "",
                &ShadowRecord {
                    pinned: vec![pin("日期", Some("phrase:rq:{date}"), 2)],
                    deleted: vec![],
                },
            )
            .unwrap();
            // 造库时迁移已跑过（旧表当时为空）：删掉它写出的空文件与标记，模拟升级前
            let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
        }
        remove_marker(&p, super::MIGRATED_P3_SHADOW);
        let s = Store::open(&p).unwrap();
        assert!(s.state_dir().join(super::MIGRATED_P3_SHADOW).is_file());
        let r = s.get_shadow_rules("wb", "aa").unwrap().unwrap();
        assert_eq!(
            r.pinned.iter().map(|x| x.word.as_str()).collect::<Vec<_>>(),
            vec!["工", "恭"]
        );
        assert_eq!(r.deleted, vec!["式".to_string()]);
        assert_eq!(
            s.get_shadow_rules("pinyin", "").unwrap().unwrap().pinned[0].position,
            2
        );
        drop(s);
        remove_marker(&p, super::MIGRATED_P3_SHADOW);
        let s = Store::open(&p).unwrap();
        assert_eq!(
            s.get_shadow_rules("wb", "aa")
                .unwrap()
                .unwrap()
                .pinned
                .len(),
            2,
            "重跑不重复"
        );
    }

    /// 旧 PHRASES 表迁成文本：用户行原样；与 TOML 相同的系统行不写；被改过的系统行写成修改行。
    /// 没有 TOML（`set_system_phrases` 之前）不迁。
    #[test]
    fn phrases_table_migrates_with_toml_knowledge() {
        let p = db("phrases");
        {
            let s = Store::open(&p).unwrap();
            s.legacy_put_phrase("yx", "me@x.com", 1800, 0, true, false, false, "")
                .unwrap();
            s.legacy_put_phrase("rq", "{date}", 100, 0, true, true, false, "")
                .unwrap();
            s.legacy_put_phrase("sj", "{time}", 100, 0, false, true, false, "")
                .unwrap();
        }
        remove_marker(&p, super::MIGRATED_P3_PHRASES);
        let s = Store::open(&p).unwrap();
        assert!(!s.state_dir().join(super::MIGRATED_P3_PHRASES).is_file());
        let sys = |c: &str, t: &str| crate::phrases::SystemPhrase {
            code: c.into(),
            text: t.into(),
            weight: 100,
            position: 0,
            category: String::new(),
            enabled: true,
        };
        s.set_system_phrases(&[sys("rq", "{date}"), sys("sj", "{time}")])
            .unwrap();
        assert!(s.state_dir().join(super::MIGRATED_P3_PHRASES).is_file());
        assert!(s.migrated_p3());
        let f = std::fs::read_to_string(s.phrases_path()).unwrap();
        assert!(f.contains("yx\tme@x.com"), "{f}");
        assert!(!f.contains("rq\t"), "{f}");
        assert!(f.contains("sj\t{time}\t100\t0\t0\t\t1"), "{f}");
    }
}
