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
#[path = "migrate_p2_tests.rs"]
mod tests;
