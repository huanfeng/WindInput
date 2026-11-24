//! wind-punct: 标点转换纯逻辑（从 wind-coordinator 抽出，可原生测试）。
//!
//! 与 Go `wind_input/internal/coordinator/handle_punctuation.go` 对齐。所有函数无副作用
//! （除经 `&mut PunctuationConverter` 推进引号状态机外），输入为配置 + 当前模式布尔
//! （chinese_punct / full_width），便于单测。
//!
//! 转换优先级（对齐 Go `convertPunct`）：自定义映射 → 数字后智能 → 中文标点 → 全半角。
//!
//! # 参数是 `&PunctConfig` 而不是 `&InputConfig`
//!
//! 自定义映射表可由方案整表替换（[`wind_config::PunctSpec::custom_mappings`]），临英、快符、
//! 主输入路各查各的表。凡读 `punct.*` 的函数一律收窄到 `&PunctConfig`（由
//! `Coordinator::effective_punct` 供给），漏接即编译失败，而不是方案表静默不生效。
//!
//! 缺口：`PunctuationConverter::peek_custom`（wind-transform）可绕过本 crate 直调，目前只有
//! `Coordinator::english_pairs_via_pipeline` 这么做，它有守门测试。再出现直调时，要么收编进
//! 本 crate，要么补一条守门测试。
//!
//! 读 `symbol.*` 的 [`participates`] / [`english_participates`] / [`english_smart_source_chars`]
//! 仍吃 `&InputConfig`：智能符号不下放方案级。

pub mod spacing;

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

/// 英文后智能标点：中文标点模式下，若 ch 在英文后智能列表、且插入点前一字符是拉丁字母，
/// 则该标点按英文（半角）输出（`Node.js` 不转 `Node。js`）。`latin_before` 由调用方给出
/// ——它来自输入序列记账或被顶出的文本，不是文档现读（见 `spacing` 模块）。
pub fn is_smart_punct_after_latin(punct: &PunctConfig, ch: char, latin_before: bool) -> bool {
    punct.smart_after_latin && latin_before && punct.latin_smart_list.contains(ch)
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

/// 引号键在当前模式下的 (左形, 右形)：自定义映射的 `"1`/`"2`（`'1`/`'2`）两行即左形与
/// 右形，任一行缺值/空串则该侧回落内置中文引号。非引号键返回 None。
///
/// 这是左右形的唯一真相源，自动配对的判定与插入都必须问它，不能直接查 `quote_pair`：
/// 引号自定义成 `「」` 后两边口径不一，交替态与配对栈会错位。无状态，左右按行取，不看交替态。
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
/// 英文半角下 DLL 默认透传标点键，引擎收不到，英半列会成死格。core 把这个集合推给 DLL，
/// DLL 只吃集合内的键并转发，没配的键行为不变。
///
/// 铁律：C++ 吃键集必须 ⊆ Rust 出字集。本函数同时是 DLL 吃键判据和
/// `Coordinator::handle_english_custom_punct` 的接手判据，同源即不漂移。以下不算覆盖：
/// 空串列（回落默认转换）；英半列等于原字符本身（设置端整行落盘，`-` `=` 的默认值就是原字符，
/// 吃下虽出同样的字，宿主却收不到原始按键，单键快捷键会失效）。引号两行折回同一源字符。
pub fn custom_english_punct_chars(punct: &PunctConfig) -> Vec<char> {
    if !punct.custom_enabled {
        return Vec::new();
    }
    let mut out: Vec<char> = Vec::new();
    for (key, vals) in &punct.custom_mappings {
        let Some(src) = wind_transform::punctuation::custom_key_source_char(key) else {
            continue;
        };
        let Some(en_half) = vals.get(3) else {
            continue;
        };
        if en_half.is_empty() || en_half.chars().eq([src]) {
            continue; // 英半列无值或等于原字符 → 与透传等价，不必吃键
        }
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
/// 键一律经 [`PunctuationConverter::custom_key`] 生成，别自己按 `ch.to_string()` 拼：
/// 引号的存储键是 `"1`/`"2`，拼出来的键查不到。
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
    convert_punct_ctx(conv, punct, chinese_punct, full_width, ch, prev_char, false)
}

/// [`convert_punct`]，另带「插入点前一字符是不是拉丁字母」（英文后智能标点）。
/// 两种智能直出英文同一优先级（自定义映射之后、中文标点之前），任一命中即按英文列。
pub fn convert_punct_ctx(
    conv: &mut PunctuationConverter,
    punct: &PunctConfig,
    chinese_punct: bool,
    full_width: bool,
    ch: char,
    prev_char: u16,
    latin_before: bool,
) -> String {
    let smart_en = chinese_punct
        && (is_smart_punct_after_digit(punct, ch, prev_char)
            || is_smart_punct_after_latin(punct, ch, latin_before));
    let is_chinese_punct = chinese_punct && !smart_en;

    // 1. 自定义映射优先（四状态均可配置）。开关与映射表同取自传入的生效 `PunctConfig`
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

/// 无副作用地计算 `ch` 在当前模式下的标点产物，镜像 `convert_punct` 优先级。
/// `chinese=true` 算中文标点产物（引号经 peek 预测不改状态）；`chinese=false` 算英文产物
/// （替换用）。
///
/// 引号同样查自定义映射（键取当前左右态，不推进状态），否则智能符号的武装判定会拿内置 `“`
/// 去比对，与实际上屏的自定义值不符。唯一不查自定义的是英文半角列：这一路专供智能符号 press2
/// 的替换文本，语义是「原样产物」，随用户改写的话连按两次就换不回英文。
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

/// 英文智能符号：源字符 `ch`（键本身的 ASCII 标点）是否在 `symbol.english_chars` 里。
///
/// 与中文侧 [`participates`] 按产物判定刻意不同：推给 DLL 的吃键集必须是源字符（见
/// [`english_smart_source_chars`]），按源字符判定则参与判据与吃键判据天然同源。
pub fn english_participates(cfg: &InputConfig, ch: char) -> bool {
    // 空白不参与，须与 [`english_smart_source_chars`] 的过滤一致：否则英文全角下空格会被
    // 武装，而解除武装的判据按 `punct_char` / `numpad_char` 问键答不上，press1 当场自解武装。
    !ch.is_whitespace() && cfg.symbol.english_chars.contains(ch)
}

/// 英文输入模式的智能符号需要 DLL 吃下并转发的源字符集合（去重、升序）。
///
/// 英文半角下 DLL 默认透传标点键，智能符号无从触发。core 把这个集合并入
/// `CONFIG_KEY_CUSTOM_EN_PUNCT` 推送（与 [`custom_english_punct_chars`] 合并），DLL 据此
/// 精确吃键。开关关闭时返回空集。
///
/// 铁律同 [`custom_english_punct_chars`]：C++ 吃键集必须 ⊆ Rust 出字集。没有英半自定义的键
/// 经 `Coordinator::handle_english_custom_punct` 原样出 ASCII，与透传等价，故并入安全。
pub fn english_smart_source_chars(cfg: &InputConfig) -> Vec<char> {
    if !cfg.symbol.english_mode {
        return Vec::new();
    }
    let mut out: Vec<char> = Vec::new();
    for c in cfg.symbol.english_chars.chars() {
        // 空白不是标点键产物，混进集合会让 DLL 吃下空格键。
        if c.is_whitespace() || out.contains(&c) {
            continue;
        }
        out.push(c);
    }
    out.sort_unstable();
    out
}

/// 主键盘能打出的全部 ASCII 标点，与 `key_convert::punct_char` 的两列逐字对应。
/// 首行 = Shift+数字的上挡符号，后两行 = OEM 键的无 Shift / 有 Shift 两态（手工排版即文档）。
#[rustfmt::skip]
const PUNCT_SOURCES: [char; 32] = [
    ')', '!', '@', '#', '$', '%', '^', '&', '*', '(',
    '-', '_', '=', '+', '[', '{', ']', '}', '\\', '|',
    ';', ':', '\'', '"', ',', '<', '.', '>', '/', '?', '`', '~',
];

/// 某份自定义标点表覆盖了哪些标点源字符（任一列非空即算覆盖，去重升序）。
/// 覆盖的键会被改写，必须经引擎出字、不能透传；空串列是回落默认转换，不算覆盖。
///
/// 两个消费者共用以保证同源：[`chinese_passthrough_punct_chars`] 判全局表，`ConfigBundle`
/// 判每个方案的表（方案声明 `[punct] custom_mappings` 时整表替换，全局表不参与）。
///
/// 查表走 [`custom_lookup`]，别自己遍历 `custom_mappings` 的键：引号的存储键是 `"1`/`"2`，
/// 折不回源字符，方案自定义的引号会被漏减。
pub fn custom_covered_punct_chars(conv: &PunctuationConverter, punct: &PunctConfig) -> Vec<char> {
    PUNCT_SOURCES
        .iter()
        .copied()
        .filter(|&ch| {
            (0..4).any(|col| custom_lookup(conv, punct, ch, col).is_some_and(|v| !v.is_empty()))
        })
        .collect()
}

/// 空格行哪几列配了自定义映射：第 i 位 = 列 i（[`punct_col_idx`] 的列号）非空。
///
/// 中文模式空缓冲时 DLL 默认透传空格（保留网页滚动等宿主语义），只在全角态吃；半角列配了
/// 映射就得据此多吃，否则按 `OnTestKeyDown` 结论走的宿主（Notepad++、WPS）直接收到半角
/// 空格。查表同走 [`custom_lookup`]。
pub fn custom_space_cols(conv: &PunctuationConverter, punct: &PunctConfig) -> u8 {
    (0..4)
        .filter(|&col| custom_lookup(conv, punct, ' ', col).is_some())
        .fold(0, |mask, col| mask | (1 << col))
}

/// 该字符是否正在参与自动配对。配对栈由引擎维护，透传掉栈就断了，故命中即不能透传。
///
/// 必须连开关一起判：配对表出厂有值而两个开关出厂关闭，只看表会把撞码最狠的 `(` `)` 等
/// 挡在透传之外（见 docs/architecture/smart-symbol-compat-notes.md「产物不变的标点要透传」）。
/// per-app 开关是 AND 语义，据全局判在全局开、某 app 关时会多排除几个键，落在保守侧。
fn is_pair_char(cfg: &InputConfig, ch: char) -> bool {
    let cn = cfg.auto_pair.chinese
        && cfg
            .auto_pair
            .chinese_pairs
            .iter()
            .any(|p| p.chars().any(|c| c == ch));
    let en = cfg.auto_pair.english
        && cfg
            .auto_pair
            .english_pairs
            .iter()
            .any(|p| p.chars().any(|c| c == ch));
    cn || en
}

/// 中文标点态下产物就是原样半角 ASCII、因而该让 DLL 直接透传（不吃键）的标点集合，去重升序。
/// 吃下再原样吐回会被非 TSF-aware 宿主当虚拟键码解释，见
/// docs/architecture/smart-symbol-compat-notes.md「产物不变的标点要透传」。
///
/// 吃键集必须 ⊆ 出字集：[`convert_punct`] 的自定义映射、中文标点表任一步会改写的字符不能透传，
/// 智能符号与配对符也须经引擎；全半角一步由 DLL 判 `IsFullWidth()`。英文标点态另有一份，见
/// [`english_passthrough_punct_chars`]，两份都推给宿主按当下标点态选用。
///
/// 只管标点转换层面。引导键、方案码元首码等空缓冲也生效的按键占用，本 crate 看不见，调用方
/// 必须再减去，见 `ConfigBundle` 里 `cn_passthrough_punct_chars` 的组装；只在有会话时生效的
/// 绑定（翻页、次选等）不必排除，透传闸门本就带 `!hasInputSession`。
pub fn chinese_passthrough_punct_chars(
    conv: &PunctuationConverter,
    cfg: &InputConfig,
) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for ch in PUNCT_SOURCES {
        // 1. 中文标点表有映射（`!`→！、`$`→￥、`^`→……、`(`→（、`)`→））⇒ 要转换，必须吃。
        if conv.peek_chinese_str(ch).is_some() {
            continue;
        }
        // 2. 自定义映射任一列有值 ⇒ 要改写，必须吃。只看中半列不够：中文输入模式下还能切
        //    英文标点态（走英半列），键若已被透传，那一列就成了死格。
        if custom_covered_punct_chars(conv, &cfg.punct).contains(&ch) {
            continue;
        }
        // 3. 参与中文智能符号 ⇒ 连按替换要引擎接手，必须吃。走到这里的字符没有中文产物，
        //    出厂 `smart_chars` 下恒不命中，留着防用户把 ASCII 配进去。必须连 `smart_mode`
        //    一起判（出厂关）；也不能改用 `symbol.english_chars`，那归英文标点态管。
        if cfg.symbol.smart_mode && participates(cfg, &ch.to_string()) {
            continue;
        }
        // 4. 配对符 ⇒ 配对栈由引擎维护，必须吃。
        //    `(` `)` 已被第 1 条拦下（有中文映射），这条是防用户改配对表把别的标点配进去。
        if is_pair_char(cfg, ch) {
            continue;
        }
        out.push(ch);
    }
    out.sort_unstable(); // 推送字节可复现（同 custom_english_punct_chars）
    out
}

/// 英文标点态（中文输入模式 + 标点切英文）下产物就是原样半角 ASCII、因而该透传的标点
/// 集合，去重升序。[`chinese_passthrough_punct_chars`] 的姊妹，差别只在「什么算会被改写」。
///
/// 这个态不走中文标点表，`,` `.` `;` 等中文态必须吃的键在这里产物不变，所以集合通常更大，
/// 不能给中文那份叠条件得到。「超集」只是出厂配置下的观察，不是不变量：两态的智能符号判据
/// 来源不同。
///
/// 英半列是这个态唯一会改写字符的来源，故复用 [`custom_english_punct_chars`]，与 DLL 英文模式
/// 吃键同源。全角由调用方的动态闸门挡。按键占用层同样由调用方再滤，见 `ConfigBundle`。
pub fn english_passthrough_punct_chars(cfg: &InputConfig) -> Vec<char> {
    let en_custom = custom_english_punct_chars(&cfg.punct);
    let mut out: Vec<char> = Vec::new();
    for ch in PUNCT_SOURCES {
        // 1. 英半列配了自定义 ⇒ 要出用户配的值，必须吃。
        if en_custom.contains(&ch) {
            continue;
        }
        // 2. 参与英文智能符号 ⇒ 连按替换要引擎接手，必须吃。必须连 `english_punct_mode`
        //    一起判（出厂关），否则出厂 `english_chars`（`.,?!:;`）整排被挡。`english_mode`
        //    管整个输入法切英文，那时 DLL 走另一条分支，不查本集合。
        if cfg.symbol.english_punct_mode && cfg.symbol.english_chars.contains(ch) {
            continue;
        }
        // 3. 配对符 ⇒ 配对栈由引擎维护，必须吃。
        //    这个态下 `(` `)` `[` `]` 不再被中文映射拦下，本条是它们唯一的闸门。
        if is_pair_char(cfg, ch) {
            continue;
        }
        out.push(ch);
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
