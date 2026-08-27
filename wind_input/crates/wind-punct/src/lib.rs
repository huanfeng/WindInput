//! wind-punct: 标点转换纯逻辑（从 wind-coordinator 抽出，可原生测试）。
//!
//! 与 Go `wind_input/internal/coordinator/handle_punctuation.go` 对齐。所有函数无副作用
//! （除经 `&mut PunctuationConverter` 推进引号状态机外），输入为配置 + 当前模式布尔
//! （chinese_punct / full_width），便于单测。
//!
//! 转换优先级（对齐 Go `convertPunct`）：自定义映射 → 数字后智能 → 中文标点 → 全半角。
//!
//! # ★★ 参数是 `&PunctConfig` 而不是 `&InputConfig`（自定义标点方案级化的关键）
//!
//! 自定义映射表可以由方案整表替换（[`wind_config::PunctSpec::custom_mappings`]），于是
//! 「该用哪张表」不再是全局唯一答案——临英查 `english` 方案的表、快符查快符方案的表、
//! 主输入路查活跃方案的表。调用方必须显式说明用的是**哪一份**。
//!
//! ⇒ 凡读 `punct.*` 的函数一律收窄到 `&PunctConfig`，由 `Coordinator::effective_punct`
//! 供给；**经本 crate 的消费点漏接一个就是编译失败**，而不是「这条路上方案表静默不生效」。
//! 同型见 `[phrases]` 六闸门把 `scope` 做成必填参数（漏接表现是另一个功能停止工作，零日志）。
//!
//! ⚠️ **这道防线有一个缺口**：`PunctuationConverter::peek_custom`（wind-transform）可以被
//! 直接调用，绕过本 crate。当前唯一这么做的是 `Coordinator::english_pairs_via_pipeline`
//! （英文自动配对要算左右符号的实际形态），它有专门的守门测试。**再出现这种直调时，要么
//! 收编进本 crate，要么给它也补一条守门测试**——签名收窄管不着它。
//!
//! 仍吃 `&InputConfig` 的是读 `symbol.*` 的那几个（[`participates`] /
//! [`english_participates`] / [`english_smart_source_chars`]）——智能符号**不下放**方案级。

use wind_config::config::{InputConfig, PunctConfig};
use wind_transform::fullwidth::to_full_width;
use wind_transform::punctuation::PunctuationConverter;

/// 数字后智能标点：中文标点模式下，若 ch 在智能标点列表且光标前一字符为数字，
/// 则该标点按英文（半角）输出（如 "3." 不转 "3。"）。`prev_char` 为 UTF-16 单元（0=不可用）。
pub fn is_smart_punct_after_digit(punct: &PunctConfig, ch: char, prev_char: u16) -> bool {
    if !punct.smart_after_digit {
        return false;
    }
    let list = &punct.smart_list;
    let in_list = if list.is_empty() {
        ch == '.' || ch == ','
    } else {
        list.contains(ch)
    };
    if !in_list {
        return false;
    }
    // 数字 '0'..='9' = 0x30..=0x39
    (0x30..=0x39).contains(&prev_char)
}

/// 自定义标点映射的列号：中半 0 / 英全 1 / 中全 2 / 英半 3。
/// `chinese_punct` 须是**已扣除数字后智能**的有效值（见 `convert_punct`）。
pub fn punct_col_idx(chinese_punct: bool, full_width: bool) -> usize {
    match (chinese_punct, full_width) {
        (true, true) => 2,
        (true, false) => 0,
        (false, true) => 1,
        (false, false) => 3,
    }
}

/// 引号键在当前模式下的 **(左形, 右形)**：自定义映射的 `"1`/`"2`（`'1`/`'2`）两行即左形与
/// 右形，任一行缺值/空串则该侧回落内置中文引号。非引号键返回 None。
///
/// 这是「左右形」的**唯一真相源**——自动配对的判定与插入都必须问它，不能自己去查
/// `quote_pair`（内置形）：用户把引号自定义成 `「」` 后，判定按 `“”` 不命中、插入却按 `「」`
/// 配对，交替态与配对栈立刻错位（就是「一次出对、一次出单」那个老 bug 的自定义映射版本）。
/// 无状态：不看也不动引号交替态，因为左右是**按行**取的，不靠「第几次」推导。
pub fn quote_forms(
    punct: &PunctConfig,
    chinese_punct: bool,
    full_width: bool,
    c: char,
) -> Option<(String, String)> {
    let (def_left, def_right) = wind_transform::punctuation::quote_pair(c)?;
    let (left_key, right_key) = wind_transform::punctuation::quote_custom_keys(c)?;
    let col = punct_col_idx(chinese_punct, full_width);
    let pick = |key: &str, def: char| -> String {
        if !punct.custom_enabled {
            return def.to_string();
        }
        punct
            .custom_mappings
            .get(key)
            .and_then(|vals| vals.get(col))
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| def.to_string())
    };
    Some((pick(left_key, def_left), pick(right_key, def_right)))
}

/// 「英文半角列有自定义覆盖」的源字符集合（去重、升序）。
///
/// 存在理由：英文输入模式（非全角）下 TSF 默认**直接透传**标点键，引擎根本收不到，于是四列
/// 里的「英半」成了打不到的死格（英全列有 `english_fullwidth` 分支吃键才得以生效）。core 把
/// 这个集合推给 DLL，DLL 只吃集合内的键并转发——**门控精确到字符**：用户没配的标点键行为
/// 完全不变（仍走 DLL 本地英文配对 + 透传）。
///
/// 与之配对的铁律：**C++ 吃键集必须 ⊆ Rust 出字集**。本函数同时是 DLL 吃键判据和
/// `Coordinator::handle_english_custom_punct` 的接手判据，同源即不会漂移；空串列（= 回落
/// 默认转换）不算覆盖，否则会吃下一个自己不出字的键。
///
/// 引号两行（`"1`/`"2`）折回同一个源字符 `"`，任一行有值即视为该键有覆盖。
pub fn custom_english_punct_chars(punct: &PunctConfig) -> Vec<char> {
    if !punct.custom_enabled {
        return Vec::new();
    }
    let mut out: Vec<char> = Vec::new();
    for (key, vals) in &punct.custom_mappings {
        if vals.get(3).is_none_or(|v| v.is_empty()) {
            continue; // 英半列无值 → 回落默认转换，不必吃键
        }
        let Some(src) = wind_transform::punctuation::custom_key_source_char(key) else {
            continue;
        };
        if !out.contains(&src) {
            out.push(src);
        }
    }
    out.sort_unstable(); // HashMap 迭代序不稳定，排序保证推送字节可复现
    out
}

/// 纯查表读自定义标点映射的指定列（不碰转换器引号状态），供无副作用计算用。
/// 四状态列：中半 0 / 英全 1 / 中全 2 / 英半 3。
///
/// 键的生成一律走 [`PunctuationConverter::custom_key`]——此处曾自己按 `ch.to_string()` 拼键，
/// 于是引号（存储键是 `"1`/`"2`）在这条路上永远查不到自定义。
pub fn custom_lookup(
    conv: &PunctuationConverter,
    punct: &PunctConfig,
    ch: char,
    col_idx: usize,
) -> Option<String> {
    conv.peek_custom(punct, ch, col_idx)
}

/// 标点转换单点流水线（对齐 Go `convertPunct`）。`conv` 推进引号状态机故取 `&mut`。
pub fn convert_punct(
    conv: &mut PunctuationConverter,
    punct: &PunctConfig,
    chinese_punct: bool,
    full_width: bool,
    ch: char,
    prev_char: u16,
) -> String {
    let smart_en = chinese_punct && is_smart_punct_after_digit(punct, ch, prev_char);
    let is_chinese_punct = chinese_punct && !smart_en;

    // 1. 自定义映射优先（四状态均可配置）。开关与映射表同取自传入的**生效** `PunctConfig`
    //    （`lookup_custom` 内部判 `custom_enabled`，故此处不再重复一道开关）。
    let col_idx = punct_col_idx(is_chinese_punct, full_width);
    if let Some(text) = conv.lookup_custom(punct, ch, col_idx) {
        return text;
    }

    // 2~4. 默认转换：中文标点（含引号状态机）→ 全半角。
    let mut piece = ch.to_string();
    if is_chinese_punct && let Some(c) = conv.to_chinese(ch) {
        piece = c;
    }
    if full_width {
        piece = to_full_width(&piece);
    }
    piece
}

/// 无副作用地计算 `ch` 在当前模式下的标点产物，**镜像** `convert_punct` 优先级。
/// `chinese=true` 算中文标点产物（引号经 peek 预测不改状态）；`chinese=false` 算英文产物
/// （替换用）。
///
/// **引号同样参与自定义映射**：键经 `custom_key` 取当前左右态（`"1`/`"2`），`peek` 不推进
/// 状态。此前引号被整体跳过，导致智能符号的武装判定拿标准 `“` 去比对参与集合，而实际上屏的
/// 是用户自定义值——该武装的不武装、参与集合形同虚设。
///
/// 唯一仍不查自定义的是**英文半角列**（`chinese=false && !full_width`）：pure 的这一路语义是
/// 「该键在英文模式下的原样产物」，专供智能符号 press2 的替换文本；若随中文列一起被用户改写，
/// 连按两次就换不回英文了。
pub fn compute_punct_str_pure(
    conv: &PunctuationConverter,
    punct: &PunctConfig,
    full_width: bool,
    ch: char,
    chinese: bool,
) -> Option<String> {
    let col_idx = if chinese && full_width {
        Some(2) // 中文全角
    } else if chinese {
        Some(0) // 中文半角
    } else if full_width {
        Some(1) // 英文全角
    } else {
        None // 英文半角：pure 计算走原样（见上方文档）
    };
    if let Some(ci) = col_idx
        && let Some(v) = custom_lookup(conv, punct, ch, ci)
    {
        return Some(v);
    }

    let mut s = ch.to_string();
    if chinese {
        s = conv.peek_chinese_str(ch)?;
    }
    if full_width {
        s = to_full_width(&s);
    }
    Some(s)
}

/// 中文标点串 `cn` 是否在用户配置的智能符号参与集合内（子串包含匹配）。
pub fn participates(cfg: &InputConfig, cn: &str) -> bool {
    !cn.is_empty() && cfg.symbol.smart_chars.contains(cn)
}

/// 英文智能符号：**源字符** `ch`（键本身的 ASCII 标点）是否在 `symbol.english_chars` 里。
///
/// 与中文侧 [`participates`] 按「实际产物」判定刻意不同——英文侧的产物通常就等于源字符，
/// 而推给 DLL 的吃键集必须是源字符（见 [`english_smart_source_chars`]）。按源字符判定，
/// 「参与判据」与「吃键判据」天然同源，不必从自定义英半列的产物反推回按键。
pub fn english_participates(cfg: &InputConfig, ch: char) -> bool {
    cfg.symbol.english_chars.contains(ch)
}

/// 英文输入模式的智能符号需要 DLL 吃下并转发的源字符集合（去重、升序）。
///
/// 英文半角下 DLL 默认**直接透传**标点键，引擎收不到 → 智能符号无从触发。core 把这个集合
/// 并入 `CONFIG_KEY_CUSTOM_EN_PUNCT` 推送（与 [`custom_english_punct_chars`] 合并），DLL 据此
/// 精确吃键。开关关闭时返回空集，英文模式行为与历史完全一致。
///
/// 与之配对的铁律同 [`custom_english_punct_chars`]：**C++ 吃键集必须 ⊆ Rust 出字集**。合并后的
/// 集合同时是 DLL 吃键判据和 `Coordinator::handle_english_custom_punct` 的接手判据，同源即不漂移
/// ——后者对没有英半自定义的键会原样出 ASCII，与透传等价，故并入是安全的。
pub fn english_smart_source_chars(cfg: &InputConfig) -> Vec<char> {
    if !cfg.symbol.english_mode {
        return Vec::new();
    }
    let mut out: Vec<char> = Vec::new();
    for c in cfg.symbol.english_chars.chars() {
        // 空白不是按键产物，混进集合会让 DLL 吃下空格键（`IsPunctuationKey` 挡得住，但判据
        // 应当自己干净）。
        if c.is_whitespace() || out.contains(&c) {
            continue;
        }
        out.push(c);
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> InputConfig {
        InputConfig::default()
    }

    #[test]
    fn smart_punct_after_digit_default_list() {
        let c = cfg(); // 默认 smart_punct_after_digit=true, list=".,:"
        // '.' 在列表 + 前字符是数字 '5'(0x35) → true
        assert!(is_smart_punct_after_digit(&c.punct, '.', 0x35));
        // 前字符非数字 → false
        assert!(!is_smart_punct_after_digit(&c.punct, '.', b'a' as u16));
        // 不在列表的标点 → false
        assert!(!is_smart_punct_after_digit(&c.punct, '!', 0x35));
    }

    #[test]
    fn convert_punct_chinese_and_fullwidth() {
        let mut conv = PunctuationConverter::new();
        let c = cfg();
        // 中文标点模式：'.' → '。'
        assert_eq!(
            convert_punct(&mut conv, &c.punct, true, false, '.', 0),
            "。"
        );
        // 英文标点模式 + 全角：'.' 走全半角 → '．'
        let out = convert_punct(&mut conv, &c.punct, false, true, '.', 0);
        assert_ne!(out, "."); // 全角化
    }

    #[test]
    fn convert_punct_smart_digit_forces_english() {
        let mut conv = PunctuationConverter::new();
        let c = cfg();
        // 中文模式但前字符是数字 → '.' 按英文输出（不转 '。'）。
        assert_eq!(
            convert_punct(&mut conv, &c.punct, true, false, '.', 0x33),
            "."
        );
    }

    #[test]
    fn compute_pure_mirrors_chinese() {
        let conv = PunctuationConverter::new();
        let c = cfg();
        // 中文产物：'.' → '。'（peek 不改状态）。
        assert_eq!(
            compute_punct_str_pure(&conv, &c.punct, false, '.', true).as_deref(),
            Some("。")
        );
    }

    #[test]
    fn participates_substring_match() {
        let mut c = cfg();
        c.symbol.smart_chars = "。，".to_string();
        assert!(participates(&c, "。"));
        assert!(!participates(&c, "！"));
        assert!(!participates(&c, ""));
    }

    /// 英文侧参与判定按**源字符**（键本身的 ASCII），与中文侧按产物刻意不同。
    #[test]
    fn english_participates_by_source_char() {
        let mut c = cfg();
        c.symbol.english_chars = ".,".to_string();
        assert!(english_participates(&c, '.'));
        assert!(english_participates(&c, ','));
        assert!(!english_participates(&c, '?'));
    }

    /// 推给 DLL 的吃键集受 `english_mode` 门控：关闭时必须是空集——英文模式的标点键就该
    /// 保持透传，多吃一个键就是一次潜在丢键（吃了再吐，严格 TSF 宿主不回退合成 WM_CHAR）。
    #[test]
    fn english_smart_source_chars_gated_by_switch() {
        let mut c = cfg();
        c.symbol.english_chars = ".,;".to_string();
        assert!(
            english_smart_source_chars(&c).is_empty(),
            "开关关闭时不得吃任何键"
        );
        c.symbol.english_mode = true;
        // 升序去重（推送字节须可复现）。
        assert_eq!(english_smart_source_chars(&c), vec![',', '.', ';']);
        // 空白不是按键产物，不进吃键集。
        c.symbol.english_chars = ". ,".to_string();
        assert_eq!(english_smart_source_chars(&c), vec![',', '.']);
    }

    #[test]
    fn custom_lookup_empty_is_none() {
        let conv = PunctuationConverter::new();
        let c = cfg(); // 默认无自定义映射
        assert_eq!(custom_lookup(&conv, &c.punct, '.', 0), None);
    }

    /// 回归锁（根因）：自定义映射来自**实时配置**，不是转换器里的启动快照。
    /// 同一个 conv 实例，配置里加上映射后下一次转换即生效——这正是「设置页改自定义标点
    /// 必须重启服务才生效」的病灶：曾把表存进转换器且只在 `Coordinator::new` 注入一次。
    #[test]
    fn convert_punct_follows_live_custom_mappings() {
        let mut conv = PunctuationConverter::new();
        let mut c = cfg();
        // 出厂：中文标点模式下 '"' 走内置引号交替 → 左引号。
        assert_eq!(
            convert_punct(&mut conv, &c.punct, true, false, '"', 0),
            "\u{201C}"
        );
        conv.reset();

        // 用户在设置页配了双引号第一次/第二次（中文半角列）。
        c.punct.custom_enabled = true;
        c.punct
            .custom_mappings
            .insert("\"1".into(), vec!["「".into()]);
        c.punct
            .custom_mappings
            .insert("\"2".into(), vec!["」".into()]);
        assert_eq!(
            convert_punct(&mut conv, &c.punct, true, false, '"', 0),
            "「",
            "热重载后第一次应立即出自定义值"
        );
        assert_eq!(
            convert_punct(&mut conv, &c.punct, true, false, '"', 0),
            "」",
            "第二次应出「第二次」那一行"
        );
    }

    /// 推给 DLL 的吃键集合：只含「英半列非空」的行的源字符，且引号两行折回同一字符。
    /// 这个集合同时是 DLL 的吃键判据和 core 的出字判据，多一个字符就是一次丢键。
    #[test]
    fn custom_english_punct_chars_only_covered_keys() {
        let mut c = cfg();
        c.punct.custom_enabled = true;
        // 引号两行都配了英半列 → 折回一个 '"'
        c.punct.custom_mappings.insert(
            "\"1".into(),
            vec!["E".into(), "".into(), "".into(), "#".into()],
        );
        c.punct.custom_mappings.insert(
            "\"2".into(),
            vec!["￥".into(), "".into(), "".into(), "$".into()],
        );
        // 只配了中半列 → 英文模式无需吃键
        c.punct
            .custom_mappings
            .insert("/".into(), vec!["、".into()]);
        // 英半列是空串（回落默认）→ 同样不该吃
        c.punct.custom_mappings.insert(
            ";".into(),
            vec!["；".into(), "".into(), "".into(), "".into()],
        );
        // 单引号只配英半列
        c.punct.custom_mappings.insert(
            "'1".into(),
            vec!["".into(), "".into(), "".into(), "@".into()],
        );
        assert_eq!(custom_english_punct_chars(&c.punct), vec!['"', '\'']);

        // 总开关关掉 → 空集合（DLL 恢复历史行为）
        c.punct.custom_enabled = false;
        assert!(custom_english_punct_chars(&c.punct).is_empty());
    }

    /// `"1`/`"2` 两行 = 左形/右形：配对判定与插入都从这里取，两行都用得上
    /// （曾只按「第几次」取用，配对钉左后第二行永远取不到）。
    #[test]
    fn quote_forms_maps_two_rows_to_left_and_right() {
        let mut c = cfg();
        // 未自定义：回落内置中文引号。
        assert_eq!(
            quote_forms(&c.punct, true, false, '"'),
            Some(("\u{201C}".into(), "\u{201D}".into()))
        );
        // 两行齐：左形取 "1、右形取 "2。
        c.punct.custom_enabled = true;
        c.punct
            .custom_mappings
            .insert("\"1".into(), vec!["「".into()]);
        c.punct
            .custom_mappings
            .insert("\"2".into(), vec!["」".into()]);
        assert_eq!(
            quote_forms(&c.punct, true, false, '"'),
            Some(("「".into(), "」".into()))
        );
        // 只配左形：右侧回落内置（不会跟着变）。
        c.punct.custom_mappings.remove("\"2");
        assert_eq!(
            quote_forms(&c.punct, true, false, '"'),
            Some(("「".into(), "\u{201D}".into()))
        );
        // 非引号键无左右形。
        assert_eq!(quote_forms(&c.punct, true, false, ','), None);
        // 列随模式走：中文全角取第 2 列。
        c.punct
            .custom_mappings
            .insert("\"1".into(), vec!["「".into(), "x".into(), "『".into()]);
        assert_eq!(
            quote_forms(&c.punct, true, true, '"').map(|(l, _)| l),
            Some("『".into())
        );
    }

    /// 引号在 pure 路径也能查到自定义（键取 `"1`/`"2`），且 peek 不推进交替态。
    /// 这决定智能符号的武装判定拿的是「用户实际会上屏的符号」而非标准引号。
    #[test]
    fn compute_pure_quote_uses_custom_mapping() {
        let conv = PunctuationConverter::new();
        let mut c = cfg();
        c.punct.custom_enabled = true;
        c.punct
            .custom_mappings
            .insert("\"1".into(), vec!["￥".into()]);
        assert_eq!(
            compute_punct_str_pure(&conv, &c.punct, false, '"', true).as_deref(),
            Some("￥")
        );
        // 英文半角列刻意不查自定义（press2 的替换目标须保持原样英文）。
        assert_eq!(
            compute_punct_str_pure(&conv, &c.punct, false, '"', false).as_deref(),
            Some("\"")
        );
    }
}
