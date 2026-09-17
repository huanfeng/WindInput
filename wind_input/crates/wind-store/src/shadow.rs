//! Shadow 规则存储（候选词条手动调序 / 删除）
//!
//! 与 Go 版本 `wind_input/internal/store/shadow.go` 对齐（简化）。
//! 按「方案 + 输入码」分组，记录用户对候选的置顶/前后移（pinned）与删除（deleted）。
//! 规则在词频排序之后应用，优先级最高。规则的「应用」由调用方（协调器）完成，
//! 本模块只负责规则的增删查与持久化，避免对候选类型产生依赖。

use crate::store::{SHADOW, Store};
use redb::ReadableTable;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// 单条置顶/移动规则：把 word 固定到 position（页内/列表内目标下标）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowPin {
    pub word: String,
    /// 候选稳定 id（动态短语用；非空时按 id 精准匹配，对齐 Go R2，见 store.md §5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cand_id: Option<String>,
    pub position: usize,
}

/// 某输入码下的 Shadow 规则集合
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShadowRecord {
    #[serde(default)]
    pub pinned: Vec<ShadowPin>,
    /// 被删除（屏蔽）的候选文本
    #[serde(default)]
    pub deleted: Vec<String>,
}

impl ShadowRecord {
    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty() && self.deleted.is_empty()
    }

    /// 一条规则是否指向同一个候选：`cand_id` 双方非空 → 按 id；否则按 word。
    ///
    /// ⚠ 动态短语（`date` 等）的 `word` 是**写入当天**的求值文本，逐日不同。凡「按用户当下
    /// 指定的候选去定位一条既有规则」的操作（去重、恢复默认、菜单灰显）都必须走本判据，
    /// 只比 word 会在次日失配——表现为规则删不掉、「恢复默认」菜单项恒灰。
    pub fn same_target(
        p_word: &str,
        p_id: &Option<String>,
        word: &str,
        cand_id: Option<&str>,
    ) -> bool {
        match (cand_id, p_id.as_deref()) {
            (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => a == b,
            _ => p_word == word,
        }
    }

    /// 置顶/移动：把 (word, cand_id) 固定到 position。LIFO（新规则插队首）；置顶优先于删除。
    /// 去重键见 [`Self::same_target`]。
    pub fn apply_pin(&mut self, word: &str, cand_id: Option<String>, position: usize) {
        let id_ref = cand_id.as_deref();
        self.pinned
            .retain(|p| !Self::same_target(&p.word, &p.cand_id, word, id_ref));
        self.deleted.retain(|d| d != word);
        self.pinned.insert(
            0,
            ShadowPin {
                word: word.to_string(),
                cand_id,
                position,
            },
        );
    }

    /// 删除（屏蔽）：word 不再出现；同时移除其置顶规则。
    pub fn apply_delete(&mut self, word: &str) {
        self.pinned.retain(|p| p.word != word);
        if !self.deleted.iter().any(|d| d == word) {
            self.deleted.push(word.to_string());
        }
    }

    /// 恢复默认：清除该候选的置顶与删除规则。定位判据见 [`Self::same_target`]——
    /// 动态短语必须按 `cand_id` 找，按 word 找会在次日删不掉自己写下的规则。
    pub fn apply_remove(&mut self, word: &str, cand_id: Option<&str>) {
        self.pinned
            .retain(|p| !Self::same_target(&p.word, &p.cand_id, word, cand_id));
        // deleted 无 id 维度（走 shadow 删除的只有静态候选，见 wind_candidate::apply_shadow）。
        self.deleted.retain(|d| d != word);
    }

    /// 是否存在指向该候选的规则（置顶或删除）——菜单「恢复默认」的可用性判据。
    pub fn has_target(&self, word: &str, cand_id: Option<&str>) -> bool {
        self.pinned
            .iter()
            .any(|p| Self::same_target(&p.word, &p.cand_id, word, cand_id))
            || self.deleted.iter().any(|d| d == word)
    }
}

/// Shadow 规则存储：key = "方案id\t输入码"
pub struct ShadowStore {
    map: RwLock<HashMap<String, ShadowRecord>>,
}

impl Default for ShadowStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ShadowStore {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
    }

    fn key(schema: &str, code: &str) -> String {
        format!("{}\t{}", schema, code)
    }

    /// 置顶/移动：把 word 固定到 position（0 = 首位）。LIFO：最新规则覆盖同词旧规则。
    pub fn pin(&self, schema: &str, code: &str, word: &str, position: usize) {
        let mut map = self.map.write().unwrap_or_else(|e| e.into_inner());
        map.entry(Self::key(schema, code))
            .or_default()
            .apply_pin(word, None, position);
    }

    /// 删除（屏蔽）：word 不再出现在该输入码的候选中。
    pub fn delete(&self, schema: &str, code: &str, word: &str) {
        let mut map = self.map.write().unwrap_or_else(|e| e.into_inner());
        map.entry(Self::key(schema, code))
            .or_default()
            .apply_delete(word);
    }

    /// 恢复默认：清除该 word 的置顶与删除规则。
    pub fn reset(&self, schema: &str, code: &str, word: &str) {
        let mut map = self.map.write().unwrap_or_else(|e| e.into_inner());
        let key = Self::key(schema, code);
        if let Some(rec) = map.get_mut(&key) {
            rec.apply_remove(word, None);
            if rec.is_empty() {
                map.remove(&key);
            }
        }
    }

    /// 该 word 是否有 Shadow 规则（置顶或删除）
    pub fn has_rule(&self, schema: &str, code: &str, word: &str) -> bool {
        let map = self.map.read().unwrap_or_else(|e| e.into_inner());
        match map.get(&Self::key(schema, code)) {
            Some(rec) => rec.has_target(word, None),
            None => false,
        }
    }

    /// 取某输入码的规则副本（无则 None）
    pub fn get_rules(&self, schema: &str, code: &str) -> Option<ShadowRecord> {
        let map = self.map.read().unwrap_or_else(|e| e.into_inner());
        map.get(&Self::key(schema, code)).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// 从 JSON 文件加载（不存在则静默忽略）
    pub fn load_from_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if let Ok(parsed) = serde_json::from_str::<HashMap<String, ShadowRecord>>(&content) {
            *self.map.write().unwrap_or_else(|e| e.into_inner()) = parsed;
        }
        Ok(())
    }

    /// 保存到 JSON 文件（原子写）
    pub fn save_to_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = {
            let map = self.map.read().unwrap_or_else(|e| e.into_inner());
            serde_json::to_string_pretty(&*map).unwrap_or_else(|_| "{}".to_string())
        };
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

// ───────────────────────── redb Shadow ops（新后端，满足 dict §9 契约）─────────────────────────
//
// key=`"{schema}\0{code}"`，value=ShadowRecord 的 JSON（每码规则稀疏、写入低频，JSON 足够）。
// 规则的「应用」（pin/delete 落到候选）仍由引擎排序阶段负责（dict.md：Shadow 是 Provider 非层）。

fn shadow_key(schema: &str, code: &str) -> String {
    format!("{schema}\u{0}{code}")
}

impl Store {
    /// 读改写一条 code 的 Shadow 规则；改完为空则删除该键（单写事务）。
    fn modify_shadow(
        &self,
        schema: &str,
        code: &str,
        f: impl FnOnce(&mut ShadowRecord),
    ) -> anyhow::Result<()> {
        let key = shadow_key(schema, code);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(SHADOW)?;
                let mut rec: ShadowRecord = t
                    .get(key.as_str())?
                    .and_then(|g| serde_json::from_slice(g.value()).ok())
                    .unwrap_or_default();
                f(&mut rec);
                if rec.is_empty() {
                    t.remove(key.as_str())?;
                } else {
                    let bytes = serde_json::to_vec(&rec)?;
                    t.insert(key.as_str(), bytes.as_slice())?;
                }
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 置顶/移动候选（cand_id 非空=动态短语按 id 匹配）。
    pub fn pin_shadow(
        &self,
        schema: &str,
        code: &str,
        word: &str,
        cand_id: Option<&str>,
        position: usize,
    ) -> anyhow::Result<()> {
        self.modify_shadow(schema, code, |rec| {
            rec.apply_pin(word, cand_id.map(String::from), position)
        })
    }

    /// 删除（屏蔽）候选。
    pub fn delete_shadow(&self, schema: &str, code: &str, word: &str) -> anyhow::Result<()> {
        self.modify_shadow(schema, code, |rec| rec.apply_delete(word))
    }

    /// 移除某候选的 Shadow 规则（恢复默认）。`cand_id` 非空时按 id 定位——动态短语的 word
    /// 逐日变化，只按 word 会删不掉自己昨天写下的规则（见 `ShadowRecord::same_target`）。
    pub fn remove_shadow_rule(
        &self,
        schema: &str,
        code: &str,
        word: &str,
        cand_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.modify_shadow(schema, code, |rec| rec.apply_remove(word, cand_id))
    }

    /// 取某 code 的 Shadow 规则（无则 None）。
    pub fn get_shadow_rules(
        &self,
        schema: &str,
        code: &str,
    ) -> anyhow::Result<Option<ShadowRecord>> {
        let key = shadow_key(schema, code);
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(SHADOW)?;
            Ok(t.get(key.as_str())?
                .and_then(|g| serde_json::from_slice(g.value()).ok()))
        })
    }

    /// 导出某方案全部 shadow 规则为 jsonl（每行 {"code","rec"}）。
    pub fn export_shadow_jsonl(&self, schema: &str) -> anyhow::Result<String> {
        let rules = self.list_shadow_rules(schema)?;
        let mut out = String::new();
        for (code, rec) in rules {
            out.push_str(&serde_json::to_string(
                &serde_json::json!({ "code": code, "rec": rec }),
            )?);
            out.push('\n');
        }
        Ok(out)
    }

    /// 从 jsonl 导入 shadow 规则（逐条 replay pin/delete，天然 upsert）。
    /// 返回 (imported=重放的规则条数, skipped=非法行数)。
    pub fn import_shadow_jsonl(&self, schema: &str, text: &str) -> anyhow::Result<(usize, usize)> {
        let normalized = wind_utils::text::normalize_input(text);
        let text = normalized.as_ref();
        let mut imported = 0usize;
        let mut skipped = 0usize;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                skipped += 1;
                continue;
            };
            let (Some(code), Some(rec)) = (
                v.get("code").and_then(|x| x.as_str()),
                v.get("rec")
                    .and_then(|x| serde_json::from_value::<ShadowRecord>(x.clone()).ok()),
            ) else {
                skipped += 1;
                continue;
            };
            // 存储序 index 0 = 最新（apply_pin LIFO 插队首），反向重放才能还原原顺序。
            for p in rec.pinned.iter().rev() {
                self.pin_shadow(schema, code, &p.word, p.cand_id.as_deref(), p.position)?;
                imported += 1;
            }
            for w in &rec.deleted {
                self.delete_shadow(schema, code, w)?;
                imported += 1;
            }
        }
        Ok((imported, skipped))
    }

    /// 导出某方案全部 shadow 规则为动作行（wdict shadow 段用；对齐 Go 的 del/pin）。
    ///
    /// pinned 按存储序逆序（LIFO，index 0 = 最新）输出 pin 行，重放时最后 pin 回队首还原原序；
    /// deleted 输出 del 行。
    pub fn export_shadow_actions(
        &self,
        schema: &str,
    ) -> anyhow::Result<Vec<crate::wdict::ShadowActionIo>> {
        let rules = self.list_shadow_rules(schema)?;
        let mut out = Vec::new();
        for (code, rec) in rules {
            for p in rec.pinned.iter().rev() {
                out.push(crate::wdict::ShadowActionIo {
                    action: "pin".into(),
                    code: code.clone(),
                    word: p.word.clone(),
                    position: p.position as i32,
                    cand_id: p.cand_id.clone(),
                });
            }
            for w in &rec.deleted {
                out.push(crate::wdict::ShadowActionIo {
                    action: "del".into(),
                    code: code.clone(),
                    word: w.clone(),
                    position: 0,
                    cand_id: None,
                });
            }
        }
        Ok(out)
    }

    /// 从动作行导入 shadow 规则（逐行 replay pin/delete，天然 upsert）。返回重放条数。
    pub fn import_shadow_actions(
        &self,
        schema: &str,
        actions: &[crate::wdict::ShadowActionIo],
    ) -> anyhow::Result<usize> {
        let mut n = 0usize;
        for a in actions {
            match a.action.as_str() {
                "pin" => {
                    let pos = a.position.max(0) as usize;
                    self.pin_shadow(schema, &a.code, &a.word, a.cand_id.as_deref(), pos)?;
                    n += 1;
                }
                "del" => {
                    self.delete_shadow(schema, &a.code, &a.word)?;
                    n += 1;
                }
                _ => {}
            }
        }
        Ok(n)
    }

    /// 清空某方案全部 shadow 规则（单写事务），返回删除键数。
    pub fn clear_shadow(&self, schema: &str) -> anyhow::Result<usize> {
        let prefix = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let n;
            {
                let mut t = txn.open_table(SHADOW)?;
                let keys: Vec<String> = {
                    let mut ks = Vec::new();
                    for item in t.range(prefix.as_str()..)? {
                        let (k, _) = item?;
                        let key = k.value();
                        if !key.starts_with(&prefix) {
                            break;
                        }
                        ks.push(key.to_string());
                    }
                    ks
                };
                n = keys.len();
                for k in &keys {
                    t.remove(k.as_str())?;
                }
            }
            txn.commit()?;
            Ok(n)
        })
    }

    /// 列举某方案下所有 code 的 Shadow 规则（设置页用）。返回 (code, 规则) 列表。
    pub fn list_shadow_rules(&self, schema: &str) -> anyhow::Result<Vec<(String, ShadowRecord)>> {
        let prefix = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(SHADOW)?;
            let mut out = Vec::new();
            for item in t.range(prefix.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&prefix) {
                    break;
                }
                let code = &key[prefix.len()..];
                if let Ok(rec) = serde_json::from_slice::<ShadowRecord>(v.value()) {
                    out.push((code.to_string(), rec));
                }
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn shadow_jsonl_roundtrip_and_clear() {
        let path = tmp("wind_sh_io.redb");
        let s = Store::open(&path).unwrap();
        s.pin_shadow("wb", "aaaa", "恭", Some("c1"), 0).unwrap();
        s.pin_shadow("wb", "aaaa", "敬", None, 1).unwrap(); // 后置顶 → 存储序 index 0
        s.delete_shadow("wb", "bbbb", "删词").unwrap();
        let text = s.export_shadow_jsonl("wb").unwrap();
        assert_eq!(text.lines().count(), 2);

        let path2 = tmp("wind_sh_io2.redb");
        let s2 = Store::open(&path2).unwrap();
        let (imported, skipped) = s2.import_shadow_jsonl("wb", &text).unwrap();
        assert_eq!(skipped, 0);
        assert!(imported >= 3);
        let rules = s2.list_shadow_rules("wb").unwrap();
        assert_eq!(rules.len(), 2);
        let pinned = rules.iter().find(|(c, _)| c == "aaaa").unwrap();
        // round-trip 保序：LIFO 存储序（最新在前）导入后不反转
        assert_eq!(pinned.1.pinned.len(), 2);
        assert_eq!(pinned.1.pinned[0].word, "敬");
        assert_eq!(pinned.1.pinned[0].position, 1);
        assert_eq!(pinned.1.pinned[1].word, "恭");
        assert_eq!(pinned.1.pinned[1].position, 0);

        assert_eq!(s.clear_shadow("wb").unwrap(), 2);
        assert!(s.list_shadow_rules("wb").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn test_pin_delete_reset() {
        let s = ShadowStore::new();
        s.pin("wubi", "aaaa", "恭恭敬敬", 0);
        s.delete("wubi", "bbbb", "某词");
        assert!(s.has_rule("wubi", "aaaa", "恭恭敬敬"));
        assert!(s.has_rule("wubi", "bbbb", "某词"));

        let r = s.get_rules("wubi", "aaaa").unwrap();
        assert_eq!(r.pinned.len(), 1);
        assert_eq!(r.pinned[0].position, 0);

        // 删除优先级：pin 后再 delete 同词，pin 被移除
        s.delete("wubi", "aaaa", "恭恭敬敬");
        let r = s.get_rules("wubi", "aaaa").unwrap();
        assert!(r.pinned.is_empty());
        assert_eq!(r.deleted, vec!["恭恭敬敬".to_string()]);

        // 恢复默认清除规则
        s.reset("wubi", "aaaa", "恭恭敬敬");
        assert!(!s.has_rule("wubi", "aaaa", "恭恭敬敬"));
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = std::env::temp_dir().join("wind_shadow_roundtrip.json");
        let _ = std::fs::remove_file(&tmp);
        let a = ShadowStore::new();
        a.pin("py", "nihao", "你好", 0);
        a.delete("py", "ceshi", "测试");
        a.save_to_file(&tmp).unwrap();

        let b = ShadowStore::new();
        b.load_from_file(&tmp).unwrap();
        assert!(b.has_rule("py", "nihao", "你好"));
        assert!(b.has_rule("py", "ceshi", "测试"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_redb_shadow_ops() {
        let path = std::env::temp_dir().join("wind_shadow_redb.redb");
        let _ = std::fs::remove_file(&path);
        let s = Store::open(&path).unwrap();

        // pin + delete 不同码
        s.pin_shadow("wb", "aaaa", "恭恭敬敬", None, 0).unwrap();
        s.delete_shadow("wb", "bbbb", "某词").unwrap();
        let r = s.get_shadow_rules("wb", "aaaa").unwrap().unwrap();
        assert_eq!(r.pinned.len(), 1);
        assert_eq!(r.pinned[0].position, 0);
        assert!(
            s.get_shadow_rules("wb", "bbbb").unwrap().unwrap().deleted == vec!["某词".to_string()]
        );

        // pin 后 delete 同词 → pin 被移除、转为 deleted
        s.delete_shadow("wb", "aaaa", "恭恭敬敬").unwrap();
        let r = s.get_shadow_rules("wb", "aaaa").unwrap().unwrap();
        assert!(r.pinned.is_empty());
        assert_eq!(r.deleted, vec!["恭恭敬敬".to_string()]);

        // remove → 规则清空后该键删除
        s.remove_shadow_rule("wb", "aaaa", "恭恭敬敬", None)
            .unwrap();
        assert!(s.get_shadow_rules("wb", "aaaa").unwrap().is_none());

        // cand_id 动态短语：按 id 去重
        s.pin_shadow("wb", "zz", "日期", Some("phrase:zz:date"), 0)
            .unwrap();
        s.pin_shadow("wb", "zz", "日期改", Some("phrase:zz:date"), 1)
            .unwrap();
        let r = s.get_shadow_rules("wb", "zz").unwrap().unwrap();
        assert_eq!(r.pinned.len(), 1, "同 cand_id 应去重为 1 条");
        assert_eq!(r.pinned[0].word, "日期改");

        let _ = std::fs::remove_file(&path);
    }

    /// 动态短语（`date`）的完整生命周期：**规则里的 word 是写入当天的求值文本**，
    /// 次日用户看到的候选文本已全变。去重 / 恢复默认 / 菜单灰显三处都必须按 cand_id 定位，
    /// 否则表现为「昨天调好今天被还原，且既清不掉也改不动」。
    ///
    /// ⚠ 这个测试必须用**两个不同的 word**（昨天/今天）才有判别力——若前后都传同一个 word，
    /// 按 word 匹配的旧实现也会全绿。
    #[test]
    fn dynamic_phrase_rule_located_by_cand_id_across_days() {
        let path = tmp("wind_shadow_dyn_phrase.redb");
        let s = Store::open(&path).unwrap();
        let id = "phrase:date:$Y-$MM-$DD";

        // 昨天：把 date 的 `$Y-$MM-$DD` 那条置顶，word 记的是昨天的求值结果。
        s.pin_shadow("wb", "date", "2026-07-28", Some(id), 0)
            .unwrap();

        // 今天：候选文本已变成 2026-07-29，而规则里存的仍是昨天的 2026-07-28。
        // 判别力全在这两行——按 id 查得到，只按今天的文本查不到（旧实现的失效点）。
        let rec = s.get_shadow_rules("wb", "date").unwrap().unwrap();
        assert!(rec.has_target("2026-07-29", Some(id)), "按 id 应跨日命中");
        assert!(
            !rec.has_target("2026-07-29", None),
            "只按当日文本必失配——这正是「昨天调好今天被还原」的成因"
        );

        // 今天：同一条短语再次置顶到别的位置 → 应**更新**原规则而非新增一条。
        s.pin_shadow("wb", "date", "2026-07-29", Some(id), 2)
            .unwrap();
        let rec = s.get_shadow_rules("wb", "date").unwrap().unwrap();
        assert_eq!(rec.pinned.len(), 1, "同 id 跨日应去重，而不是逐日堆积");
        assert_eq!(rec.pinned[0].position, 2);
        assert_eq!(rec.pinned[0].word, "2026-07-29", "word 刷新为当次文本");

        // 明天：文本又变了，按 id 仍能删掉。
        s.remove_shadow_rule("wb", "date", "2026-07-30", Some(id))
            .unwrap();
        assert!(
            s.get_shadow_rules("wb", "date").unwrap().is_none(),
            "按 cand_id 恢复默认应删得掉跨日规则"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 反向闸门：静态候选（无 id）的规则仍按 word 定位，不受上面的改动影响。
    #[test]
    fn static_candidate_rule_still_located_by_word() {
        let path = tmp("wind_shadow_static_word.redb");
        let s = Store::open(&path).unwrap();
        s.pin_shadow("wb", "aaaa", "工", None, 0).unwrap();
        let rec = s.get_shadow_rules("wb", "aaaa").unwrap().unwrap();
        assert!(rec.has_target("工", None));
        assert!(!rec.has_target("恭", None));
        // 传了 id 但规则侧无 id → 落回 word 比较，仍命中。
        assert!(rec.has_target("工", Some("phrase:x:y")));
        s.remove_shadow_rule("wb", "aaaa", "工", None).unwrap();
        assert!(s.get_shadow_rules("wb", "aaaa").unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_list_shadow_rules() {
        let path = std::env::temp_dir().join("wind_shadow_list.redb");
        let _ = std::fs::remove_file(&path);
        let s = Store::open(&path).unwrap();
        s.pin_shadow("wb", "aaaa", "恭恭敬敬", None, 0).unwrap();
        s.delete_shadow("wb", "bbbb", "某词").unwrap();
        s.pin_shadow("py", "nihao", "你好", None, 0).unwrap();

        let mut wb = s.list_shadow_rules("wb").unwrap();
        wb.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(wb.len(), 2, "wb 应有 aaaa/bbbb 两个 code");
        assert_eq!(wb[0].0, "aaaa");
        assert_eq!(wb[0].1.pinned.len(), 1);
        assert_eq!(wb[1].0, "bbbb");
        assert_eq!(wb[1].1.deleted, vec!["某词".to_string()]);
        // 跨方案隔离
        assert_eq!(s.list_shadow_rules("py").unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
