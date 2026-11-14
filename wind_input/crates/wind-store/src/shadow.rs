//! Shadow 规则存储（候选词条手动调序 / 删除）
//!
//! 与 Go 版本 `wind_input/internal/store/shadow.go` 对齐（简化）。
//! 按「方案 + 输入码」分组，记录用户对候选的置顶/前后移（pinned）与删除（deleted）。
//! 规则在词频排序之后应用，优先级最高。规则的「应用」由调用方（协调器）完成，
//! 本模块只负责规则的增删查与持久化，避免对候选类型产生依赖。

use crate::store::{SHADOW, Store};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

// ───────────────────────── Shadow 规则文本（P3）─────────────────────────
//
// 真源 `<user>/<schema>/shadow.dict.yaml`（设计 §2.2）：`columns: [code, text, action, position,
// cand_id]`。同一编码的规则是一组行：pin 行在前（newest first，同 `ShadowRecord.pinned`），
// delete 行在后；`position` 是目标槽位。内存持有解析结果，按键路径读零 IO；写入按组补丁、整文件
// 提交。规则的「应用」（pin/delete 落到候选）仍由引擎排序阶段负责（dict.md：Shadow 是 Provider）。

/// Shadow 文件的列。
pub const SHADOW_COLS: &[&str] = &["code", "text", "action", "position", "cand_id"];

/// 「屏蔽这个词」的规则键：拼音 / 英文数据方案里 `code = *` 的 `delete` 行表示**所有编码**都不出这段
/// 文字（按文字删除，见设计 system-dict-patch-and-candidate-adjust §4a、§8 第 11 条）。它在这两个桶里
/// 不是输入码，[`Store::get_shadow_rules`] 不按它返回规则。其它方案里 `*` 是普通编码（码表通配键
/// 浏览态）。英文桶的输入码是小写化的英文缓冲：英文方案码元只有字母，临英缓冲以字母开头，只有
/// 「触发键进临英 + `allow_symbols` 放行 `*`」才可能打出恰为 `*` 的缓冲——那时英文词库无候选，
/// 能调的只有原文条，协调器对这个码不写规则（`candidate_op_in`）。
pub const BLOCK_ALL_CODES: &str = "*";

/// 按文字屏蔽只在这些数据方案里有：拼音族折叠后的 `pinyin`，与英文方案 / 临时英文共用的 `english`。
pub const BLOCK_SCHEMAS: &[&str] = &["pinyin", "english"];

/// `schema` 是否支持按文字屏蔽。
pub fn supports_block(schema: &str) -> bool {
    BLOCK_SCHEMAS.contains(&schema)
}

/// `schema` 里的 `code` 是不是屏蔽组。
fn is_block_group(schema: &str, code: &str) -> bool {
    supports_block(schema) && code == BLOCK_ALL_CODES
}

/// 能否按文字屏蔽：至少两个字。单字全局屏蔽等于这个字打不出，只按编码隐藏。
///
/// 「字」按用户看到的算：变体选择符、肤色修饰、组合附加符、标签字符、零宽（不）连接符不单独成字，
/// 零宽连接符后面那个字并进前一个（`👨‍👩‍👧`），一对区域指示符是一面旗。
pub fn is_blockable_word(text: &str) -> bool {
    let mut n = 0;
    let mut after_zwj = false;
    let mut half_flag = false;
    for c in text.chars() {
        if c == '\u{200D}' {
            after_zwj = true;
            continue;
        }
        if is_attached_mark(c) {
            continue;
        }
        if std::mem::take(&mut after_zwj) {
            continue;
        }
        if ('\u{1F1E6}'..='\u{1F1FF}').contains(&c) {
            half_flag = !half_flag;
            if !half_flag {
                continue; // 旗的后一半
            }
        } else {
            half_flag = false;
        }
        n += 1;
        if n >= 2 {
            return true;
        }
    }
    false
}

/// 附在前一个字上、自己不成字的码位。
fn is_attached_mark(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'      // 组合附加符
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20FF}'    // 含组合键帽 U+20E3
        | '\u{FE00}'..='\u{FE0F}'    // 变体选择符
        | '\u{FE20}'..='\u{FE2F}'
        | '\u{200C}'                 // 零宽不连接符
        | '\u{1F3FB}'..='\u{1F3FF}'  // 肤色修饰
        | '\u{E0000}'..='\u{E007F}'  // 标签字符
        | '\u{E0100}'..='\u{E01EF}') // 变体选择符补充
}

/// 屏蔽集合缓存：算它时的表内容（只比指针）+ 结果。
pub(crate) type BlockedCache = (
    std::sync::Arc<wind_userdata::text::table::TableDoc>,
    std::sync::Arc<std::collections::HashSet<String>>,
);

fn record_of_rows<'a>(
    doc: &wind_userdata::text::table::TableDoc,
    rows: impl Iterator<Item = &'a wind_userdata::text::table::TableRow>,
) -> ShadowRecord {
    let mut rec = ShadowRecord::default();
    for r in rows {
        let word = doc.get(r, "text").to_string();
        match doc.get(r, "action") {
            "pin" => {
                let id = doc.get(r, "cand_id");
                rec.pinned.push(ShadowPin {
                    word,
                    cand_id: (!id.is_empty()).then(|| id.to_string()),
                    position: doc.get(r, "position").trim().parse().unwrap_or(0),
                });
            }
            "delete" if !rec.deleted.contains(&word) => rec.deleted.push(word),
            _ => {}
        }
    }
    rec
}

/// 某方案全部规则，按编码在文件里首次出现的先后。
fn records_of(doc: &wind_userdata::text::table::TableDoc) -> Vec<(String, ShadowRecord)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&wind_userdata::text::table::TableRow>> = HashMap::new();
    for r in &doc.rows {
        let code = doc.get(r, "code").to_string();
        let g = groups.entry(code.clone()).or_default();
        if g.is_empty() {
            order.push(code);
        }
        g.push(r);
    }
    order
        .into_iter()
        .map(|c| {
            let rec = record_of_rows(doc, groups[&c].iter().copied());
            (c, rec)
        })
        .filter(|(_, r)| !r.is_empty())
        .collect()
}

/// 一组规则 → 文件行（按 `SHADOW_COLS` 列序）。
fn rows_of(code: &str, rec: &ShadowRecord) -> Vec<Vec<String>> {
    let pins = rec.pinned.iter().map(|p| {
        vec![
            code.to_string(),
            p.word.clone(),
            "pin".into(),
            p.position.to_string(),
            p.cand_id.clone().unwrap_or_default(),
        ]
    });
    let dels = rec.deleted.iter().map(|w| {
        vec![
            code.to_string(),
            w.clone(),
            "delete".into(),
            String::new(),
            String::new(),
        ]
    });
    pins.chain(dels).collect()
}

impl Store {
    /// 某数据方案的 Shadow 文件：`<text_dir>/<schema>/shadow.dict.yaml`。
    pub fn shadow_path(&self, schema: &str) -> std::path::PathBuf {
        self.user_text_dir().join(schema).join("shadow.dict.yaml")
    }

    /// 该方案的 Shadow 表（首次访问时读文件，之后常驻）。
    pub(crate) fn shadow_table(
        &self,
        schema: &str,
    ) -> anyhow::Result<std::sync::Arc<wind_userdata::text::table::TextTable>> {
        wind_userdata::engine::check_schema_id(schema)?;
        let mut m = self.shadows.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = m.get(schema) {
            return Ok(t.clone());
        }
        let t = std::sync::Arc::new(wind_userdata::text::table::TextTable::open(
            self.shadow_path(schema),
            SHADOW_COLS,
            "shadow",
        ));
        m.insert(schema.to_string(), t.clone());
        Ok(t)
    }

    /// 读改写若干编码的规则，一次提交（改完为空的组整组删掉）。
    fn modify_shadows(
        &self,
        schema: &str,
        codes: &[&str],
        f: &mut dyn FnMut(&str, &mut ShadowRecord),
    ) -> anyhow::Result<()> {
        let t = self.shadow_table(schema)?;
        self.check_not_editing(t.path())?;
        // 先按文件现状重新解析（手改优先），再在最新内容上改
        t.sync();
        let doc = t.doc();
        let all: HashMap<String, ShadowRecord> = records_of(&doc).into_iter().collect();
        let mut edits = Vec::with_capacity(codes.len());
        for &code in codes {
            let mut rec = all.get(code).cloned().unwrap_or_default();
            f(code, &mut rec);
            edits.push((vec![code.to_string()], rows_of(code, &rec)));
        }
        t.edit(&edits, &["code"])
    }

    fn modify_shadow(
        &self,
        schema: &str,
        code: &str,
        f: impl FnOnce(&mut ShadowRecord),
    ) -> anyhow::Result<()> {
        let mut f = Some(f);
        self.modify_shadows(schema, &[code], &mut |_, rec| {
            if let Some(f) = f.take() {
                f(rec)
            }
        })
    }

    /// 前移 / 后移：把 `(word, cand_id)` 钉到 `to`；邻居 `(nb_word, nb_id)` 若有**自己的**固定
    /// 规则，一并改写到 `here`（交换）。一次提交。
    ///
    /// 「邻居有规则」按读端口径判（`wind_candidate::ShadowPinRule` 的匹配）：规则带 id 只认 id，
    /// 不回退文本——动态短语的规则记的是写入当天的求值文本，回退会把碰巧同文的邻居当成它，
    /// 改写甚至清掉那条 id 规则。
    #[allow(clippy::too_many_arguments)]
    pub fn move_shadow_swapping(
        &self,
        schema: &str,
        code: &str,
        word: &str,
        cand_id: Option<&str>,
        to: usize,
        neighbor: Option<(&str, Option<&str>)>,
        here: usize,
    ) -> anyhow::Result<()> {
        self.modify_shadow(schema, code, |rec| {
            if let Some((nb_word, nb_id)) = neighbor {
                let hit = rec
                    .pinned
                    .iter()
                    .find(|p| match p.cand_id.as_deref() {
                        Some(id) if !id.is_empty() => nb_id == Some(id),
                        _ => p.word == nb_word,
                    })
                    .map(|p| (p.word.clone(), p.cand_id.clone()));
                if let Some((w, id)) = hit
                    && !ShadowRecord::same_target(&w, &id, word, cand_id)
                {
                    rec.apply_pin(&w, id, here);
                }
            }
            rec.apply_pin(word, cand_id.map(String::from), to)
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

    /// `words`（存储域的 `(扁平码, 文本)`）命中的删除规则，每条为 `(规则键, 文本, 命中的词条码)`。
    ///
    /// 规则键是**输入码**，与词条码不一定同形（全拼保留手动分隔符 `'`、ü 可能敲成 `v`），由
    /// `key_forms` 给出键在词条码域里的候选形态，任一形态命中即算。只看 `delete`，置顶不算。
    /// 按文件现状判（先 sync，手改优先）。
    fn deleted_hits(
        &self,
        schema: &str,
        words: &[(&str, &str)],
        key_forms: &dyn Fn(&str) -> Vec<String>,
    ) -> anyhow::Result<Vec<(String, String, String)>> {
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let want: std::collections::HashSet<(&str, &str)> = words.iter().copied().collect();
        let t = self.shadow_table(schema)?;
        t.sync();
        let doc = t.doc();
        let mut out = Vec::new();
        for r in doc.rows.iter().filter(|r| doc.get(r, "action") == "delete") {
            let (code, text) = (doc.get(r, "code"), doc.get(r, "text"));
            // 屏蔽行不看编码：同文的词条一律算命中（单字不按文字屏蔽，同 `blocked_words`）。
            if is_block_group(schema, code) {
                if is_blockable_word(text) {
                    out.extend(
                        words
                            .iter()
                            .filter(|(_, t)| *t == text)
                            .map(|(c, t)| (code.to_string(), t.to_string(), c.to_string())),
                    );
                }
                continue;
            }
            if let Some(k) = key_forms(code)
                .into_iter()
                .find(|k| want.contains(&(k.as_str(), text)))
            {
                out.push((code.to_string(), text.to_string(), k));
            }
        }
        Ok(out)
    }

    /// 撤销 `words` 命中的删除规则（判据见 [`Self::deleted_hits`]），返回撤掉的条数。单条显式加词后
    /// 调用：读端按文本过滤、不看来源，规则留着会把加回来的同文用户词一并滤掉。无命中时不写文件。
    pub fn clear_deleted_for_words(
        &self,
        schema: &str,
        words: &[(&str, &str)],
        key_forms: &dyn Fn(&str) -> Vec<String>,
    ) -> anyhow::Result<usize> {
        let hits: std::collections::HashSet<(String, String)> = self
            .deleted_hits(schema, words, key_forms)?
            .into_iter()
            .map(|(code, text, _)| (code, text))
            .collect();
        if hits.is_empty() {
            return Ok(0);
        }
        let mut codes: Vec<&str> = hits.iter().map(|(c, _)| c.as_str()).collect();
        codes.sort_unstable();
        codes.dedup();
        self.modify_shadows(schema, &codes, &mut |code, rec| {
            rec.deleted
                .retain(|d| !hits.contains(&(code.to_string(), d.clone())));
        })?;
        Ok(hits.len())
    }

    /// `words` 里被删除规则隐藏的那些（返回 `(词条码, 文本)`），只读。批量导入 / 全部转正不撤规则，
    /// 用它在结果里报数。
    pub fn words_hidden_by_shadow(
        &self,
        schema: &str,
        words: &[(&str, &str)],
        key_forms: &dyn Fn(&str) -> Vec<String>,
    ) -> anyhow::Result<std::collections::HashSet<(String, String)>> {
        Ok(self
            .deleted_hits(schema, words, key_forms)?
            .into_iter()
            .map(|(_, text, word_code)| (word_code, text))
            .collect())
    }

    /// 取某 code 的 Shadow 规则（无则 None）。按键路径：只读内存，不碰文件、不碰 redb。
    /// 拼音桶的屏蔽组（[`BLOCK_ALL_CODES`]）不是输入码，不按它返回，见 [`Self::blocked_words`]。
    pub fn get_shadow_rules(
        &self,
        schema: &str,
        code: &str,
    ) -> anyhow::Result<Option<ShadowRecord>> {
        if is_block_group(schema, code) {
            return Ok(None);
        }
        let doc = self.shadow_table(schema)?.doc();
        let rec = record_of_rows(&doc, doc.rows.iter().filter(|r| doc.get(r, "code") == code));
        Ok((!rec.is_empty()).then_some(rec))
    }

    /// 屏蔽这个词：写一条 `code = *` 的删除规则（该数据方案下所有编码都不出这段文字）。
    /// 单字拒绝——见 [`is_blockable_word`]。
    pub fn block_word(&self, schema: &str, text: &str) -> anyhow::Result<()> {
        anyhow::ensure!(supports_block(schema), "只有拼音与英文能按文字屏蔽");
        anyhow::ensure!(is_blockable_word(text), "单字不能按文字屏蔽");
        self.delete_shadow(schema, BLOCK_ALL_CODES, text)
    }

    /// 拼音 / 英文数据方案被屏蔽的文字（`code = *` 的删除行，单字 / 单字母不算；其它方案恒空）。按键路径：
    /// 只读内存，表内容不变时直接返回上次的结果（比一次指针）；写入或 [`Self::sync_shadows`]
    /// 换了表内容后重算。
    pub fn blocked_words(
        &self,
        schema: &str,
    ) -> anyhow::Result<std::sync::Arc<std::collections::HashSet<String>>> {
        if !supports_block(schema) {
            return Ok(Default::default());
        }
        let doc = self.shadow_table(schema)?.doc();
        let mut cache = self.blocked_words.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((d, set)) = cache.get(schema)
            && std::sync::Arc::ptr_eq(d, &doc)
        {
            return Ok(set.clone());
        }
        let set: std::collections::HashSet<String> = doc
            .rows
            .iter()
            .filter(|r| doc.get(r, "code") == BLOCK_ALL_CODES && doc.get(r, "action") == "delete")
            .map(|r| doc.get(r, "text"))
            .filter(|t| is_blockable_word(t))
            .map(str::to_string)
            .collect();
        let set = std::sync::Arc::new(set);
        cache.insert(schema.to_string(), (doc, set.clone()));
        Ok(set)
    }

    /// 单个方案的 Shadow 文件重新载入；变了返回 true。
    pub(crate) fn shadow_table_synced(&self, schema: &str) -> bool {
        let fresh = !self
            .shadows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(schema);
        match self.shadow_table(schema) {
            Ok(t) => t.sync() || (fresh && !t.doc().rows.is_empty()),
            Err(_) => false,
        }
    }

    /// 手改的 Shadow 文件重新载入（启动后新出现的方案目录也算），返回变了的方案。
    pub fn sync_shadows(&self) -> Vec<String> {
        let mut schemas: std::collections::BTreeSet<String> = self
            .shadows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        schemas.extend(self.shadow_schemas_on_disk());
        // 首次载入的表本身就是「新内容」
        schemas
            .into_iter()
            .filter(|s| self.shadow_table_synced(s))
            .collect()
    }

    /// 用户目录下有 Shadow 文件的方案。
    pub(crate) fn shadow_schemas_on_disk(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.user_text_dir()) {
            for e in rd.flatten() {
                if e.path().join("shadow.dict.yaml").is_file()
                    && let Some(n) = e.file_name().to_str()
                {
                    out.push(n.to_string());
                }
            }
        }
        out
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

    /// 一批 (code, 动作) 在内存里重放后一次提交（备份还原、wdict 导入）。
    fn replay_shadow(
        &self,
        schema: &str,
        actions: &[(String, ShadowAction)],
    ) -> anyhow::Result<usize> {
        let mut codes: Vec<&str> = actions.iter().map(|(c, _)| c.as_str()).collect();
        codes.sort_unstable();
        codes.dedup();
        let mut n = 0usize;
        self.modify_shadows(schema, &codes, &mut |code, rec| {
            for (c, a) in actions {
                if c != code {
                    continue;
                }
                match a {
                    ShadowAction::Pin(p) => rec.apply_pin(&p.word, p.cand_id.clone(), p.position),
                    ShadowAction::Delete(w) => rec.apply_delete(w),
                }
                n += 1;
            }
        })?;
        Ok(n)
    }

    /// 从 jsonl 导入 shadow 规则（重放 pin/delete，天然 upsert；一次提交）。
    /// 返回 (imported=重放的规则条数, skipped=非法行数)。
    pub fn import_shadow_jsonl(&self, schema: &str, text: &str) -> anyhow::Result<(usize, usize)> {
        let normalized = wind_utils::text::normalize_input(text);
        let text = normalized.as_ref();
        let mut skipped = 0usize;
        let mut actions = Vec::new();
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
                actions.push((code.to_string(), ShadowAction::Pin(p.clone())));
            }
            for w in &rec.deleted {
                actions.push((code.to_string(), ShadowAction::Delete(w.clone())));
            }
        }
        let imported = self.replay_shadow(schema, &actions)?;
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

    /// 从动作行导入 shadow 规则（重放 pin/delete，天然 upsert；一次提交）。返回重放条数。
    pub fn import_shadow_actions(
        &self,
        schema: &str,
        actions: &[crate::wdict::ShadowActionIo],
    ) -> anyhow::Result<usize> {
        let acts: Vec<(String, ShadowAction)> = actions
            .iter()
            .filter_map(|a| match a.action.as_str() {
                "pin" => Some((
                    a.code.clone(),
                    ShadowAction::Pin(ShadowPin {
                        word: a.word.clone(),
                        cand_id: a.cand_id.clone(),
                        position: a.position.max(0) as usize,
                    }),
                )),
                "del" => Some((a.code.clone(), ShadowAction::Delete(a.word.clone()))),
                _ => None,
            })
            .collect();
        self.replay_shadow(schema, &acts)
    }

    /// 清空某方案全部 shadow 规则（整文件替换），返回清掉的编码组数。
    pub fn clear_shadow(&self, schema: &str) -> anyhow::Result<usize> {
        let t = self.shadow_table(schema)?;
        self.check_not_editing(t.path())?;
        let n = records_of(&t.doc()).len();
        if n > 0 || t.path().is_file() {
            t.replace_all(&[])?;
        }
        Ok(n)
    }

    /// 列举某方案下所有 code 的 Shadow 规则（设置页用）。返回 (code, 规则) 列表，按 code 排序。
    pub fn list_shadow_rules(&self, schema: &str) -> anyhow::Result<Vec<(String, ShadowRecord)>> {
        let mut v = records_of(&self.shadow_table(schema)?.doc());
        v.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(v)
    }

    /// 迁移用：整组写入文件里还没有的编码（文件里已有的组以文件为准），一次提交。
    pub(crate) fn merge_shadow_rules(
        &self,
        schema: &str,
        rules: Vec<(String, ShadowRecord)>,
    ) -> anyhow::Result<()> {
        let t = self.shadow_table(schema)?;
        self.check_not_editing(t.path())?;
        let have: std::collections::HashSet<String> =
            records_of(&t.doc()).into_iter().map(|(c, _)| c).collect();
        let edits: Vec<_> = rules
            .iter()
            .filter(|(c, _)| !have.contains(c))
            .map(|(c, r)| (vec![c.clone()], rows_of(c, r)))
            .collect();
        if edits.is_empty() {
            return Ok(());
        }
        t.edit(&edits, &["code"])
    }

    /// 测试 / 探针：往旧 `SHADOW` 表写一条（模拟升级前的库）。
    #[doc(hidden)]
    pub fn legacy_put_shadow(
        &self,
        schema: &str,
        code: &str,
        rec: &ShadowRecord,
    ) -> anyhow::Result<()> {
        let key = format!("{schema}\u{0}{code}");
        let bytes = serde_json::to_vec(rec)?;
        self.with_db(|db| {
            let txn = db.begin_write()?;
            txn.open_table(SHADOW)?
                .insert(key.as_str(), bytes.as_slice())?;
            txn.commit()?;
            Ok(())
        })
    }
}

enum ShadowAction {
    Pin(ShadowPin),
    Delete(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    /// 交换只认邻居自己的规则：带 id 的规则不按文本回退；邻居有规则时改写到 `here`。
    #[test]
    fn move_shadow_swapping_matches_like_reader() {
        let path = tmp("wind_sh_swap.redb");
        let s = Store::open(&path).unwrap();
        s.pin_shadow("ct", "aa", "乙", Some("phrase:aa:x"), 7)
            .unwrap();
        s.move_shadow_swapping("ct", "aa", "甲", None, 1, Some(("乙", None)), 0)
            .unwrap();
        let rec = s.get_shadow_rules("ct", "aa").unwrap().unwrap();
        let pos = |w: &str, id: Option<&str>| {
            rec.pinned
                .iter()
                .find(|p| p.word == w && p.cand_id.as_deref() == id)
                .map(|p| p.position)
        };
        assert_eq!(pos("乙", Some("phrase:aa:x")), Some(7), "id 规则原样");
        assert_eq!(pos("甲", None), Some(1));
        assert_eq!(rec.pinned.len(), 2);

        s.pin_shadow("ct", "aa", "丙", None, 1).unwrap();
        s.move_shadow_swapping("ct", "aa", "丁", None, 1, Some(("丙", None)), 2)
            .unwrap();
        let rec = s.get_shadow_rules("ct", "aa").unwrap().unwrap();
        let at = |w: &str| rec.pinned.iter().find(|p| p.word == w).map(|p| p.position);
        assert_eq!(
            (at("丁"), at("丙")),
            (Some(1), Some(2)),
            "邻居规则改写到原位"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 只撤同 (码, 文) 的删除规则：同码别的词、同文别的码、置顶都不动；键经 `key_norm` 归一后比。
    #[test]
    fn clear_deleted_for_words_only_touches_matching_deletes() {
        let path = tmp("wind_sh_clear_del.redb");
        let s = Store::open(&path).unwrap();
        s.delete_shadow("py", "nihao", "你好").unwrap();
        s.delete_shadow("py", "nihao", "拟好").unwrap();
        s.delete_shadow("py", "ni'hao", "你好").unwrap();
        s.delete_shadow("py", "nh", "你好").unwrap();
        s.pin_shadow("py", "nihao", "你号", None, 0).unwrap();
        let strip = |k: &str| vec![k.replace('\'', "")];

        assert_eq!(
            s.clear_deleted_for_words("py", &[("nihao", "你好")], &strip)
                .unwrap(),
            2,
            "`nihao` 与 `ni'hao` 两条同文删除都该撤"
        );
        let nihao = s.get_shadow_rules("py", "nihao").unwrap().unwrap();
        assert_eq!(nihao.deleted, vec!["拟好".to_string()]);
        assert_eq!(nihao.pinned.len(), 1, "置顶不动");
        assert!(s.get_shadow_rules("py", "ni'hao").unwrap().is_none());
        assert_eq!(
            s.get_shadow_rules("py", "nh").unwrap().unwrap().deleted,
            vec!["你好".to_string()],
            "别的编码（简拼）上的删除不动"
        );

        // 只读计数：按词条去重（`nh` 上的规则不算 `nihao` 的），不改文件
        s.delete_shadow("py", "nihao", "你好").unwrap();
        s.delete_shadow("py", "ni'hao", "你好").unwrap();
        let hidden = s
            .words_hidden_by_shadow(
                "py",
                &[("nihao", "你好"), ("nihao", "拟好"), ("hao", "好")],
                &strip,
            )
            .unwrap();
        assert_eq!(
            hidden.len(),
            2,
            "「你好」两条规则只算一个词，加「拟好」，实际 {hidden:?}"
        );
        assert!(
            s.get_shadow_rules("py", "ni'hao").unwrap().is_some(),
            "计数不撤规则"
        );

        // 恒等归一：`ni'hao` 不再等于 `nihao`
        s.delete_shadow("wb", "a'b", "工").unwrap();
        assert_eq!(
            s.clear_deleted_for_words("wb", &[("ab", "工")], &|k: &str| vec![k.to_string()])
                .unwrap(),
            0
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 命中按文件现状判：表已载入后文件被外部改过（这里拿另一份库生成的规则文件覆盖），
    /// 不先 sync 就看不见新写进去的删除规则。
    /// 屏蔽行（`code = *`）：进屏蔽集合、不当输入码匹配；单字不收；加词按文字撤、批量计数按文字算。
    #[test]
    fn block_word_rows() {
        let path = tmp("wind_sh_block.redb");
        let s = Store::open(&path).unwrap();
        s.block_word("pinyin", "时间").unwrap();
        assert!(s.block_word("pinyin", "是").is_err(), "单字不能按文字屏蔽");
        s.delete_shadow("pinyin", "shijian", "实践").unwrap();
        let set = s.blocked_words("pinyin").unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.contains("时间"));
        assert!(
            std::sync::Arc::ptr_eq(&set, &s.blocked_words("pinyin").unwrap()),
            "表没变时复用缓存"
        );
        assert!(
            s.get_shadow_rules("pinyin", BLOCK_ALL_CODES)
                .unwrap()
                .is_none(),
            "屏蔽组不是输入码"
        );
        assert!(
            s.list_shadow_rules("pinyin")
                .unwrap()
                .iter()
                .any(|(c, r)| c == BLOCK_ALL_CODES && r.deleted == vec!["时间".to_string()]),
            "规则列表照常列出屏蔽组"
        );

        let same = |k: &str| vec![k.to_string()];
        let hidden = s
            .words_hidden_by_shadow("pinyin", &[("sj", "时间"), ("shijian", "时间")], &same)
            .unwrap();
        assert_eq!(hidden.len(), 2, "按文字算，任何编码都命中：{hidden:?}");
        assert_eq!(
            s.clear_deleted_for_words("pinyin", &[("shijian", "时间")], &same)
                .unwrap(),
            1
        );
        assert!(
            s.blocked_words("pinyin").unwrap().is_empty(),
            "加词撤销屏蔽"
        );
        assert_eq!(
            s.get_shadow_rules("pinyin", "shijian")
                .unwrap()
                .unwrap()
                .deleted,
            vec!["实践".to_string()],
            "按编码的删除不动"
        );

        // 手改出的单字屏蔽行不生效
        s.delete_shadow("pinyin", BLOCK_ALL_CODES, "是").unwrap();
        assert!(s.blocked_words("pinyin").unwrap().is_empty());
        s.block_word("pinyin", "时间").unwrap();
        assert!(
            s.remove_shadow_rule("pinyin", BLOCK_ALL_CODES, "时间", None)
                .is_ok()
        );
        assert!(
            s.blocked_words("pinyin").unwrap().is_empty(),
            "规则列表能删屏蔽行"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 字数按用户看到的字算：带变体选择符 / 肤色 / 组合附加符 / 零宽连接的单个符号、国旗都是一个字。
    #[test]
    fn blockable_word_counts_visible_chars() {
        for one in [
            "好",
            "❤\u{FE0F}",
            "👍\u{1F3FD}",
            "👨\u{200D}👩\u{200D}👧",
            "🇨🇳",
            "e\u{301}",
            "1\u{FE0F}\u{20E3}",
            "",
        ] {
            assert!(!is_blockable_word(one), "{one:?} 是一个字");
        }
        for two in ["你好", "❤\u{FE0F}❤\u{FE0F}", "🇨🇳🇯🇵", "e\u{301}好"] {
            assert!(is_blockable_word(two), "{two:?} 至少两个字");
        }
        assert!(!is_blockable_word("好\u{200D}"), "尾随连接符不成字");
    }

    /// 英文桶同样认屏蔽行：键是词库原文、不归一大小写（`ok` 与 `OK` 是两个词），单字母不收。
    #[test]
    fn english_block_rows_are_case_sensitive() {
        let path = tmp("wind_sh_block_en.redb");
        let s = Store::open(&path).unwrap();
        s.block_word("english", "OK").unwrap();
        assert!(
            s.block_word("english", "x").is_err(),
            "单字母不能按文字屏蔽"
        );
        let set = s.blocked_words("english").unwrap();
        assert!(set.contains("OK") && !set.contains("ok"));
        assert!(
            s.get_shadow_rules("english", BLOCK_ALL_CODES)
                .unwrap()
                .is_none()
        );
        let same = |k: &str| vec![k.to_string()];
        assert!(
            s.words_hidden_by_shadow("english", &[("ok", "ok")], &same)
                .unwrap()
                .is_empty(),
            "小写的 ok 不算被屏蔽"
        );
        assert_eq!(
            s.clear_deleted_for_words("english", &[("ok", "OK")], &same)
                .unwrap(),
            1
        );
        assert!(s.blocked_words("english").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// `*` 的「屏蔽这个词」语义只在拼音 / 英文数据方案：码表（通配键 `*` 浏览态）里它就是普通编码。
    #[test]
    fn star_is_plain_code_outside_pinyin() {
        let path = tmp("wind_sh_star_ct.redb");
        let s = Store::open(&path).unwrap();
        s.delete_shadow("wb", BLOCK_ALL_CODES, "工作").unwrap();
        assert_eq!(
            s.get_shadow_rules("wb", BLOCK_ALL_CODES)
                .unwrap()
                .map(|r| r.deleted),
            Some(vec!["工作".to_string()]),
            "码表的 `*` 是输入码，规则照常读出"
        );
        assert!(s.blocked_words("wb").unwrap().is_empty());
        assert!(
            s.block_word("wb", "工作").is_err(),
            "只有拼音与英文能按文字屏蔽"
        );
        let same = |k: &str| vec![k.to_string()];
        assert!(
            s.words_hidden_by_shadow("wb", &[("aaaa", "工作")], &same)
                .unwrap()
                .is_empty(),
            "别的编码不按文字命中"
        );
        assert_eq!(
            s.clear_deleted_for_words("wb", &[("aaaa", "工作")], &same)
                .unwrap(),
            0
        );
        assert_eq!(
            s.clear_deleted_for_words("wb", &[(BLOCK_ALL_CODES, "工作")], &same)
                .unwrap(),
            1,
            "同码（`*`）照常按编码撤"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clear_deleted_for_words_sees_hand_edited_file() {
        let path = tmp("wind_sh_clear_sync.redb");
        let s = Store::open(&path).unwrap();
        assert!(s.get_shadow_rules("py", "nihao").unwrap().is_none()); // 表载入内存
        let path2 = tmp("wind_sh_clear_sync2.redb");
        let s2 = Store::open(&path2).unwrap();
        s2.delete_shadow("py", "nihao", "你好").unwrap();
        let dst = s.shadow_path("py");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(s2.shadow_path("py"), &dst).unwrap();

        let n = s
            .clear_deleted_for_words("py", &[("nihao", "你好")], &|k: &str| vec![k.to_string()])
            .unwrap();
        assert_eq!(n, 1);
        assert!(s.get_shadow_rules("py", "nihao").unwrap().is_none());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
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

    /// 规则落文本：pin / delete 按行写进 shadow.dict.yaml，重开读回；newest-first 保持。
    #[test]
    fn shadow_rules_live_in_text() {
        let p = tmp("wind_shadow_text.redb");
        {
            let s = Store::open(&p).unwrap();
            s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
            s.pin_shadow("wb", "aa", "工", None, 1).unwrap();
            s.delete_shadow("wb", "aa", "式").unwrap();
            let f = std::fs::read_to_string(s.shadow_path("wb")).unwrap();
            assert!(
                f.contains("aa\t工\tpin\t1\t\naa\t恭\tpin\t0\t\naa\t式\tdelete\t\t\n"),
                "{f}"
            );
        }
        let s = Store::open(&p).unwrap();
        let r = s.get_shadow_rules("wb", "aa").unwrap().unwrap();
        assert_eq!(
            r.pinned.iter().map(|x| x.word.as_str()).collect::<Vec<_>>(),
            vec!["工", "恭"]
        );
        assert_eq!(r.deleted, vec!["式".to_string()]);
    }

    /// 空编码桶（特殊模式）往返不丢。
    #[test]
    fn empty_code_rules_roundtrip() {
        let p = tmp("wind_shadow_empty_code.redb");
        {
            let s = Store::open(&p).unwrap();
            s.pin_shadow("wb", "", "日期", Some("phrase:rq:{date}"), 2)
                .unwrap();
        }
        let s = Store::open(&p).unwrap();
        let r = s.get_shadow_rules("wb", "").unwrap().unwrap();
        assert_eq!(r.pinned[0].cand_id.as_deref(), Some("phrase:rq:{date}"));
        assert_eq!(r.pinned[0].position, 2);
    }

    /// 按键路径查 Shadow 不碰 redb、不读文件（加载后删掉文件、暂停 redb 照样查得到）；手改文件
    /// sync 后生效。
    #[test]
    fn shadow_lookup_does_not_touch_redb_or_file() {
        let s = Store::open(tmp("wind_shadow_hot.redb")).unwrap();
        s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
        let f = s.shadow_path("wb");
        let saved = std::fs::read(&f).unwrap();
        std::fs::remove_file(&f).unwrap();
        s.pause().unwrap();
        assert!(s.get_shadow_rules("wb", "aa").unwrap().is_some());
        s.resume().unwrap();
        std::fs::write(
            &f,
            [saved.as_slice(), "bb\t八\tdelete\t\t\n".as_bytes()].concat(),
        )
        .unwrap();
        assert!(
            s.get_shadow_rules("wb", "bb").unwrap().is_none(),
            "未 sync 不生效"
        );
        assert_eq!(s.sync_shadows(), vec!["wb".to_string()]);
        assert_eq!(
            s.get_shadow_rules("wb", "bb").unwrap().unwrap().deleted,
            vec!["八".to_string()]
        );
    }

    /// 导入一批规则只写一次文件（逐条提交会把备份还原拖成每条一次 fsync）。
    #[test]
    fn import_writes_once() {
        let s = Store::open(tmp("wind_shadow_import_once.redb")).unwrap();
        let actions: Vec<crate::wdict::ShadowActionIo> = (0..200)
            .map(|i| crate::wdict::ShadowActionIo {
                action: "del".into(),
                code: format!("c{i}"),
                word: format!("词{i}"),
                position: 0,
                cand_id: None,
            })
            .collect();
        let t = std::time::Instant::now();
        assert_eq!(s.import_shadow_actions("wb", &actions).unwrap(), 200);
        assert!(
            t.elapsed() < std::time::Duration::from_millis(500),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(s.list_shadow_rules("wb").unwrap().len(), 200);
    }
}
