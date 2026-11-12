//! 英文自动造词（`[schema.english.auto_learn]`，GH#136 / C1-11 子项 1）。
//!
//! # 信号：上屏了词库里没有的原文
//!
//! 英文没有中文那种「连续单字成词」的造词判据——它的单位本来就是一个词。用户把一串
//! 词库里没有的字母上了屏（`Immortalwrt`），这本身就是「这是个我要用的词」的信号。
//! 出口（维护者 2026-10-07 定）：选中原文候选、回车 / 空格上屏原码、标点顶屏原文；
//! 切中英文时上屏的原文不算（那是放弃组合，不是「我要这个词」）。
//!
//! 造词与词频互不相干：造完它就是词库原文，此后按词库词记词频（`99ee7b01` 那条「非词库
//! 原文不记词频」管的是还没造成词的那一刻，并不冲突）。
//!
//! # 范围：英文方案 + 临时英文，一个开关
//!
//! 两者共用 schema id `english` 与同一套用户 / 临时词库，造出来的词两边都查得到。快捷输入
//! 里的英文不在此列——那里的原文进快捷输入历史。
//!
//! # 落点：临时词库 → 晋升用户词库
//!
//! 与码表 / 拼音的自动造词同构：先进临时词库（有上限、会淘汰，拼错一次不会永久留下），
//! 累计使用 `promote_count` 次（含造词那一次）后晋升。存原形（`Immortalwrt`）、码取小写，
//! 与手工加词同一个取码函数。

use crate::coordinator::{Coordinator, LEARN_ADD_WEIGHT};
use tracing::debug;

/// 可造词的长度区间（字符）。下界挡掉单字母；上界与英文方案的最大码长同量级，
/// 再长多半是误粘进来的整段。
const MIN_LEN: usize = 2;
const MAX_LEN: usize = 32;

/// 查「已有没有这个词」时取多少条。精确码排在前（`cmp_exact_first`），同码同文的变体
/// 只有寥寥几条，取 16 足够覆盖。
const EXISTS_LOOKUP_LIMIT: usize = 16;

/// 这串能不能造词：纯 ASCII 字母数字（至少一个字母），长度在区间内。
///
/// 比手工加词的取码规则（`english_add_word_code`）更窄：临英开了 `allow_symbols` 能打出
/// `C++`、`e-mail`，但英文方案的码元只有字母，带符号的串在英文方案里打不全，自动收进来
/// 多半是噪声（手工加词另说）；含 `'` 的是分词查询语法（`p'o`），不是词。数字放行：
/// `x64`、`mp3` 是常见技术词，打前缀 `x` / `mp` 就能补出来。
fn learnable(text: &str) -> bool {
    let n = text.chars().count();
    (MIN_LEN..=MAX_LEN).contains(&n)
        && text.chars().all(|c| c.is_ascii_alphanumeric())
        && text.chars().any(|c| c.is_ascii_alphabetic())
}

/// 英文库里已有的同码条目对这次造词意味着什么。
enum Existing {
    /// 词库（系统 / 用户）里已有，大小写不论——不造。
    InDict,
    /// 临时词库里已有逐字相同的一条——推进它的次数（可能因此晋升）。
    SameTemp,
    /// 临时词库里只有大小写不同的一条（`Immortalwrt` vs 打的 `immortalwrt`）——不另造一条，
    /// 但给**那一条**（库里存的原形）推一次：临英靠 Shift 进入、首字母恒大写，造出来的是
    /// `Immortalwrt`；之后在英文方案里打小写回车，若不计数就永远攒不够晋升。
    TempOtherCase(String),
    /// 没有——造一条新的。
    None,
}

impl Coordinator {
    /// 上屏了一段英文原文：开关开着且它不在词库里，就记进英文临时词库（必要时晋升）。
    ///
    /// 调用方只管在「上屏原文」的出口调它，判据（开关、能否造词、是否已在库里）全在这里。
    ///
    /// `schema`：英文数据归属——英文方案主路传活跃方案 id（第三方英文类方案落自己的桶），
    /// 临英传它的英文方案 id（出厂 `english`）。
    pub(crate) fn learn_english_raw(&self, schema: &str, text: &str) {
        let (enabled, promote_count) = {
            let rt = self.rt();
            let al = &rt.config.schema.english.auto_learn;
            (al.enabled, al.promote_count)
        };
        if !enabled || !learnable(text) || self.learning_suspended() {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let code = text.to_ascii_lowercase();
        let target = match self.english_existing(schema, &code, text) {
            Existing::InDict => return,
            Existing::SameTemp | Existing::None => text.to_string(),
            Existing::TempOtherCase(stored) => stored,
        };
        // 「屏蔽这个词」优先于自动造词（§8 第 11 条）：用户删过的词再上屏原文不学回来。
        // 按字面比，不归一大小写（屏蔽 `OK` 不挡 `ok`）。
        if self
            .blocked_words_in(schema)
            .is_some_and(|b| b.contains(text) || b.contains(&target))
        {
            debug!("english auto-learn: 已被屏蔽，跳过");
            return;
        }
        match store.learn_temp_word(schema, &code, &target, LEARN_ADD_WEIGHT, 0) {
            Ok(count) => {
                // 日志规范：不带词文本。
                debug!("english auto-learn: temp word count={count}");
                self.maybe_evict_temp(store, schema);
                self.maybe_promote_temp(store, schema, &code, &target, count, promote_count);
            }
            Err(e) => tracing::warn!("英文自动造词写入失败: {e}"),
        }
    }

    /// 临时英文的造词入口：数据归属取临英当下的英文词库方案；候选显示关着（`None`）时英文
    /// 引擎没加载、判不了「是不是词库词」，不造。
    pub(crate) fn learn_temp_english_raw(&self, state: &crate::coordinator::State, text: &str) {
        if let Some(schema) = self.overlay_engine_schema(state) {
            self.learn_english_raw(&schema, text);
        }
    }

    /// `schema` 是不是英文类方案的数据归属（自动造词的晋升阈值 / 淘汰上限据此取英文那份）。
    /// 只认**已加载**的引擎：英文数据只会经已加载的英文引擎写进来。
    pub(crate) fn is_english_data_schema(&self, schema: &str) -> bool {
        self.engine_mgr.loaded_engine_type(schema) == Some(wind_engine::EngineType::English)
    }

    /// 查英文库里同码（`code` 小写）的条目，判这次造词该怎么处理。
    ///
    /// ⚠️ 不能用 `english_head_dict_word`：它在英文调频关着时恒返回 `None`（出厂即关），
    /// 那样「是不是词库词」就退化成了「调频开没开」。这里直接问引擎，与调频无关。
    ///
    /// **只问已加载的引擎，不为此去加载**：调用点都持着 state 锁，临英关着候选显示时英文
    /// 引擎本就没加载，在这里冷加载整套英文词库会卡住这次按键（同 `record_temp_english_selection`
    /// 「不为记一次词频去加载它」的原则）。没加载就判不了「是不是词库词」，宁可不造。
    fn english_existing(&self, schema: &str, code: &str, text: &str) -> Existing {
        if !self.is_english_data_schema(schema) {
            return Existing::InDict;
        }
        let cands = self
            .engine_mgr
            .convert_with(schema, code, EXISTS_LOOKUP_LIMIT)
            .candidates;
        let mut out = Existing::None;
        for c in cands
            .iter()
            .filter(|c| c.code.eq_ignore_ascii_case(code))
            .filter(|c| c.text.eq_ignore_ascii_case(text))
        {
            if !c.meta.is_temp_dict || c.meta.is_user_dict {
                return Existing::InDict;
            }
            out = if c.text == text {
                Existing::SameTemp
            } else {
                Existing::TempOtherCase(c.text.clone())
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::learnable;

    #[test]
    fn learnable_words() {
        assert!(learnable("Immortalwrt"));
        assert!(learnable("x64"));
        assert!(!learnable("a"), "单字母不造");
        assert!(!learnable("123"), "纯数字不造");
        assert!(!learnable("C++"), "带符号的码打不回来");
        assert!(!learnable("don't"), "撇号是分词语法");
        assert!(!learnable("thank you"), "含空白");
        assert!(!learnable(&"a".repeat(33)), "超长");
    }
}
