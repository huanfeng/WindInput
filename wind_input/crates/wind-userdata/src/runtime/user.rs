//! 用户词（对照 `wind-store/src/user_words.rs`）。P2 起真源是 `user/<schema>/user.dict.yaml`：
//! 已绑定文本的方案写入经 delta、空闲 / 显式折回进文本；读取叠加「由文本建的段 + delta」。
//! 清空走 `rewrite_text`；导入逐条 Upsert 后立即折回（已有词不挪行位置，新词按行序追加）。

use super::Runtime;
use crate::engine::Op;
use crate::record::{OpKind, Payload, Target};
use crate::snapshot::SnapshotEntry;

/// 手动加词文本长度上限（字符数）。`add_user_word` 是所有手动加词路径（快捷加词、命令栏
/// `dict.add` 显式给码、设置页词库管理）的共同写入闸口；10000 字是兜底防线，防病态输入。
/// 导入也逐行校验。
pub const USER_TEXT_MAX_CHARS: usize = 10_000;
/// 手动加词编码长度上限（字符数）。真正收紧的是拼音音节边界 `boundary: u64`——码超过 64 字节
/// 边界位掩码就装不下。真实方案远小于它（五笔 4 位、英文 32、10 字词组全拼约 60），128 留出
/// 两倍余量，又能挡住把 text 错填进 code 这类输入。
pub const USER_CODE_MAX_CHARS: usize = 128;

const U: Target = Target::User;

#[derive(Clone, Debug, PartialEq)]
pub struct UserWord {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub boundary: u64,
    /// 入库顺序（文本行号；尚未折回进文本的新词为 0）。
    pub order: u32,
}

impl From<SnapshotEntry> for UserWord {
    fn from(e: SnapshotEntry) -> UserWord {
        UserWord {
            code: e.code,
            text: e.text,
            weight: e.weight,
            boundary: e.boundary,
            order: e.order,
        }
    }
}

/// 一行导入会落成什么（与 `user_upsert_op` 同一判据：权重更大或补上 0 边界才写）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowEffect {
    Added,
    Updated,
    Unchanged,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserImportCounts {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
}

impl UserImportCounts {
    fn add(&mut self, e: RowEffect) {
        match e {
            RowEffect::Added => self.added += 1,
            RowEffect::Updated => self.updated += 1,
            RowEffect::Unchanged => self.unchanged += 1,
        }
    }
}

/// 已有 (权重, 边界) 遇到一行导入：效果与合并后的值。
fn merge_row(existing: Option<(i32, u64)>, row: &UserWord) -> (RowEffect, (i32, u64)) {
    match existing {
        None => (RowEffect::Added, (row.weight, row.boundary)),
        Some((w, b)) => {
            let merged = (w.max(row.weight), if b != 0 { b } else { row.boundary });
            let effect = if merged != (w, b) {
                RowEffect::Updated
            } else {
                RowEffect::Unchanged
            };
            (effect, merged)
        }
    }
}

/// 同批重复的键按逐条加词的结果合并：权重取 max、边界取第一个非 0 的；保留首次出现的次序。
pub(crate) fn merge_same_keys(rows: &[UserWord]) -> Vec<UserWord> {
    let mut at: std::collections::HashMap<(&str, &str), usize> = std::collections::HashMap::new();
    let mut out: Vec<UserWord> = Vec::with_capacity(rows.len());
    for r in rows {
        match at.get(&(r.code.as_str(), r.text.as_str())) {
            Some(&i) => {
                let m = &mut out[i];
                m.weight = m.weight.max(r.weight);
                if m.boundary == 0 {
                    m.boundary = r.boundary;
                }
            }
            None => {
                at.insert((r.code.as_str(), r.text.as_str()), out.len());
                out.push(r.clone());
            }
        }
    }
    out
}

pub(crate) fn check_len(code: &str, text: &str) -> anyhow::Result<()> {
    let n = text.chars().count();
    if n > USER_TEXT_MAX_CHARS {
        anyhow::bail!("词条文本过长：{n} 字（上限 {USER_TEXT_MAX_CHARS}）");
    }
    let n = code.chars().count();
    if n > USER_CODE_MAX_CHARS {
        anyhow::bail!("编码过长：{n} 字符（上限 {USER_CODE_MAX_CHARS}）");
    }
    Ok(())
}

impl Runtime {
    /// 新增或合并一个用户词的操作：已存在 ⇒ 权重取 max、旧边界为 0 时补上（无变化返回 None）。
    pub(crate) fn user_upsert_op(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        weight: i32,
        boundary: u64,
    ) -> Option<Op> {
        let (w, b) = match self.ud.get(U, schema, code, text) {
            Some(e) => {
                let w = e.weight.max(weight);
                let b = if e.boundary != 0 {
                    e.boundary
                } else {
                    boundary
                };
                if w == e.weight && b == e.boundary {
                    return None;
                }
                (w, b)
            }
            None => (weight, boundary),
        };
        Some(Op::new(
            U,
            schema,
            OpKind::Upsert,
            code,
            text,
            Payload::Weight(w),
            b,
        ))
    }

    pub fn add_user_word(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        weight: i32,
        boundary: u64,
    ) -> anyhow::Result<()> {
        check_len(code, text)?;
        if let Some(op) = self.user_upsert_op(schema, code, text, weight, boundary) {
            self.ud.apply_batch(&[op])?;
        }
        self.bump_words(schema);
        Ok(())
    }

    pub fn remove_user_word(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        if self.ud.get(U, schema, code, text).is_some() {
            self.ud
                .apply(U, schema, OpKind::Delete, code, text, Payload::None, 0)?;
        }
        self.bump_words(schema);
        Ok(())
    }

    /// 一批 [`Self::add_user_word`]：先全部校验长度（任一条过长整批不写），再一次 `apply_batch`。
    /// 同批重复的键先按逐条调用的结果合并（权重取 max、边界取第一个非 0 的）。
    pub fn add_user_words(&self, schema: &str, rows: &[UserWord]) -> anyhow::Result<()> {
        for r in rows {
            check_len(&r.code, &r.text)?;
        }
        let ops: Vec<Op> = merge_same_keys(rows)
            .iter()
            .filter_map(|r| self.user_upsert_op(schema, &r.code, &r.text, r.weight, r.boundary))
            .collect();
        if !ops.is_empty() {
            self.ud.apply_batch(&ops)?;
        }
        self.bump_words(schema);
        Ok(())
    }

    /// 一批 [`Self::remove_user_word`]：不存在的跳过，其余一次 `apply_batch`。
    pub fn remove_user_words(&self, schema: &str, keys: &[(String, String)]) -> anyhow::Result<()> {
        let ops = super::delete_ops(&self.ud, U, schema, keys);
        if !ops.is_empty() {
            self.ud.apply_batch(&ops)?;
        }
        self.bump_words(schema);
        Ok(())
    }

    /// 只改权重；不存在返回 false、不新建。
    pub fn update_user_word_weight(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        weight: i32,
    ) -> anyhow::Result<bool> {
        let Some(e) = self.ud.get(U, schema, code, text) else {
            return Ok(false);
        };
        self.ud.apply(
            U,
            schema,
            OpKind::SetWeight,
            code,
            text,
            Payload::Weight(weight),
            e.boundary,
        )?;
        self.bump_words(schema);
        Ok(true)
    }

    pub fn user_word(&self, schema: &str, code: &str, text: &str) -> Option<UserWord> {
        self.ud.get(U, schema, code, text).map(UserWord::from)
    }

    pub fn user_words(&self, schema: &str, code: &str) -> Vec<UserWord> {
        self.ud
            .exact(U, schema, code)
            .into_iter()
            .map(UserWord::from)
            .collect()
    }

    pub fn for_each_user_word(
        &self,
        schema: &str,
        prefix: &str,
        f: &mut dyn FnMut(&UserWord) -> bool,
    ) {
        self.ud
            .scan(U, schema, prefix, &mut |e| f(&UserWord::from(e)));
    }

    pub fn count_user_words(&self, schema: &str) -> usize {
        let mut n = 0;
        self.ud.scan(U, schema, "", &mut |_| {
            n += 1;
            true
        });
        n
    }

    /// 清空：已绑定文本 ⇒ 文件重写为空表（§3.8 整文件替换）；否则整段替换为空。
    pub fn clear_user_words(&self, schema: &str) -> anyhow::Result<usize> {
        let n = self.count_user_words(schema);
        if self.ud.text_binding(U, schema).is_some() {
            self.ud.rewrite_text(U, schema, &[])?;
        } else {
            self.ud.fold_with(U, schema, &mut |_| Vec::new())?;
        }
        self.bump_words(schema);
        Ok(n)
    }

    pub fn user_row_effect(&self, schema: &str, row: &UserWord) -> RowEffect {
        let existing = self
            .ud
            .get(U, schema, &row.code, &row.text)
            .map(|e| (e.weight, e.boundary));
        merge_row(existing, row).0
    }

    /// 逐行分类（导入预览与导入共用）：同批重复的键，第二次出现按「本批已合并」的结果判定。
    pub fn classify_user_rows(&self, schema: &str, rows: &[UserWord]) -> Vec<RowEffect> {
        let mut seen: std::collections::HashMap<(&str, &str), (i32, u64)> =
            std::collections::HashMap::new();
        rows.iter()
            .map(|r| {
                let key = (r.code.as_str(), r.text.as_str());
                let existing = seen.get(&key).copied().or_else(|| {
                    self.ud
                        .get(U, schema, &r.code, &r.text)
                        .map(|e| (e.weight, e.boundary))
                });
                let (effect, merged) = merge_row(existing, r);
                seen.insert(key, merged);
                effect
            })
            .collect()
    }

    /// 导入（合并）：已存在取 max 权重、补 0 边界；新词按行序追加。已绑定文本时立即折回进文本——
    /// 导入是用户的显式动作，不等空闲。同批重复的键，第二次出现按「本批已合并」的结果分类。
    pub fn import_user_rows(
        &self,
        schema: &str,
        rows: &[UserWord],
    ) -> anyhow::Result<UserImportCounts> {
        for r in rows {
            check_len(&r.code, &r.text)?;
        }
        // 文本隔离时整批拒绝：否则词已进 delta、列表里看得见，导入却报失败
        self.ud.check_text_writable(U, schema)?;
        let mut counts = UserImportCounts::default();
        for e in self.classify_user_rows(schema, rows) {
            counts.add(e);
        }
        let ops: Vec<Op> = rows
            .iter()
            .filter_map(|r| self.user_upsert_op(schema, &r.code, &r.text, r.weight, r.boundary))
            .collect();
        if !ops.is_empty() {
            self.ud.apply_batch(&ops)?;
        }
        // 外部编辑中不写回：留在 delta，结束编辑时折回
        if self.ud.text_binding(U, schema).is_some() {
            self.ud.fold_text_unless_held(U, schema)?;
        }
        self.bump_words(schema);
        Ok(counts)
    }
}
