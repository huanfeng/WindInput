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
    // 空白一律不参与，与 [`english_smart_source_chars`] 的过滤**同源**（那边早就滤了，这边
    // 当时漏了）。两处不同源的后果：用户在 `english_chars` 里写了空格 ⇒ 英文全角下按空格会
    // 被 `full_width_source_char` 取到 `' '` 并据此武装，而解除武装那道判据按 `punct_char` /
    // `numpad_char` 问键 ⇒ 空格键两边都答 `None` ⇒ press1 当场自解武装，press2 永远不来，
    // 全程零日志。空格本来也不是「标点的中英两形」这件事的成员。
    !ch.is_whitespace() && cfg.symbol.english_chars.contains(ch)
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

/// 中文输入模式下**产物就是原样半角 ASCII**、因而该让 DLL 直接透传（不吃键）的标点集合，
/// 去重升序。覆盖 Shift+数字的上挡符号与 OEM 标点键的两态。
///
/// # 为什么不能「吃下来再原样吐回去」
///
/// 吃了再吐要绕一圈经 CUAS 回到宿主，而非 TSF-aware 宿主普遍把这条路上送达的**字符码当
/// 虚拟键码**解释。Tkinter 实测（B-9，2026-09-18）：`#`(0x23)→`VK_END`、`%`(0x25)→`VK_LEFT`、
/// `&`(0x26)→`VK_UP`，字不上屏、反倒执行了一次光标移动；同批的 `@`(0x40)、`*`(0x2A) 只因撞上
/// 的 VK 在宿主里没有默认绑定才侥幸正常。微软拼音对这批符号根本不吃键，宿主拿到的 keycode
/// 仍是真实 VK（`VK_3`/`VK_5`/`VK_7`），因而不受影响 —— 本函数就是对齐那个行为。
///
/// # 判据与出字侧同源
///
/// 铁律同 [`custom_english_punct_chars`]：**吃键集必须 ⊆ 出字集**。这里反过来用：凡
/// [`convert_punct`] 的三步（自定义映射 → 中文标点表 → 全半角）里**任何一步会改写该字符**，
/// 就不能透传。第三步（全半角）由 DLL 侧判 `IsFullWidth()`，故本函数只管前两步，外加
/// 智能符号 / 配对符这两类「产物虽同、语义上仍须经引擎」的例外。
///
/// # ⚠️ 本函数只管「标点转换层面」，按键占用由调用方再滤一道
///
/// 覆盖范围含 OEM 标点键，而那批键可能被配成**引导键**（`special:*` / `mix` / 临拼 / 临英）
/// 或**方案码元首码**——这两类在**空缓冲时也生效**，透传掉就是「那个模式再也进不去」
/// / 「那个方案再也打不出字」，且**不报错**。但它们属于按键绑定语义、数据在
/// `keys.key_actions` 与各方案码元集里，本 crate（只吃 `InputConfig`）看不见。
///
/// ⇒ 调用方**必须**在本函数结果上再减去那两类，见 `ConfigBundle` 里
/// `cn_passthrough_punct_chars` 的组装。翻页键 / 次选键 / 以词定字这些**只在有会话时**
/// 生效的绑定不必在此排除——两侧的透传闸门本就带 `!hasInputSession`。
/// 主键盘能打出的全部 ASCII 标点，与 `key_convert::punct_char` 的两列**逐字对应**
/// （那边答 VK+shift→字符，这边只要字符本身）。
/// 首行 = Shift+数字的上挡符号，后两行 = OEM 键的无 Shift / 有 Shift 两态。
///
/// 手工排版即文档：压成两行就再也对不上「首行数字、后两行 OEM」这句话了。
#[rustfmt::skip]
const PUNCT_SOURCES: [char; 32] = [
    ')', '!', '@', '#', '$', '%', '^', '&', '*', '(',
    '-', '_', '=', '+', '[', '{', ']', '}', '\\', '|',
    ';', ':', '\'', '"', ',', '<', '.', '>', '/', '?', '`', '~',
];

/// 该字符是否**正在**参与自动配对。配对栈由引擎维护，透传掉栈就断了，故命中即不能透传。
///
/// ⚠️ **必须连开关一起判**。两张配对表出厂就有值（`()[]{}｛｝` 等），而
/// `auto_pair.english` / `auto_pair.chinese` 两个开关的默认值都是 `false`——只看表不看
/// 开关，等于凭一个没开的功能把 `(` `)` `[` `]` `{` `}` 挡在透传之外，而 `(`(0x28) 撞
/// `VK_DOWN`、`)`(0x29) 撞 `VK_SELECT`，正是最该修的那批（B-9 实测：英文标点态打 `(`
/// 光标下移、字不上屏）。
///
/// per-app 开关（`auto_pair_allowed_for_pid`）是 AND 语义、只能把已开的关掉，所以全局
/// 关着就一定不配对，这里据全局判是安全的；反向（全局开、某 app 关）会多排除几个键，
/// 落在保守侧。
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

/// # 只管**中文标点态**
///
/// 判据第 1 步问的是中文标点表，故结论只在中文标点态下成立。英文标点态（中文输入模式下
/// 也能切）另有一份更大的集合，见 [`english_passthrough_punct_chars`]——那个态不走中文
/// 标点表，`,` `.` `;` 这些的产物同样是原样 ASCII，撞码还更凶（`.`→`VK_DELETE` 吞字符、
/// `[`→`VK_LWIN` 弹开始菜单）。两个集合都要推给宿主，由宿主按当下标点态选用。
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
        // 2. 自定义映射**任一列**有值 ⇒ 要改写，必须吃。
        //    只看中半列不够：中文输入模式下还能切到英文标点态（走英半列），那时这个键若已被
        //    透传，用户配的那一列就成了打不到的死格。空串列 = 回落默认转换，不算覆盖。
        if (0..4).any(|col| custom_lookup(conv, &cfg.punct, ch, col).is_some_and(|v| !v.is_empty()))
        {
            continue;
        }
        // 3. 参与**中文**智能符号 ⇒ 连按替换要引擎接手，必须吃。
        //
        //    这个态的智能符号按**中文产物**判（[`participates`]），而第 1 条已经保证走到
        //    这里的字符没有中文产物 ⇒ 产物就是它自己。出厂 `smart_chars` 全是中文符号，
        //    故本条实际恒不命中；留着是为了用户把 ASCII 配进 `smart_chars` 时仍然正确。
        //
        //    ⛔ 这里**不能**改用 `symbol.english_chars`：那是英文智能符号的源字符集，
        //    归 `english_punct_mode` / `english_mode` 两个开关管，与中文标点态无关。
        //    早先在此按它排除，把 `.` `,` `?` `!` `:` `;` 整排挡在了透传之外——而那两个
        //    开关出厂都是关的，等于凭一个没生效的功能否掉了一批键（`.` 撞 VK_DELETE，
        //    恰恰是最该修的那个）。
        if participates(cfg, &ch.to_string()) {
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

/// **英文标点态**（中文输入模式 + 标点切英文）下产物就是原样半角 ASCII、因而该透传的标点
/// 集合，去重升序。[`chinese_passthrough_punct_chars`] 的姊妹，成因与失效方向完全相同，
/// 差别只在「什么算会被改写」。
///
/// # 为什么必须单独一份，不能给中文那份加个条件
///
/// 英文标点态下 [`convert_punct`] 根本不走中文标点表（`is_chinese_punct == false`），
/// 于是 `,` `.` `;` `'` `[` `]` `\` 这些**在中文态必须吃**的键，在这个态下产物就是原样
/// ASCII。所以这份集合比中文那份**更大**，是超集而非子集 —— 给现有集合叠 `&&` 只会让它
/// 更小，方向正好反了。
///
/// 撞码在这个态下同样成立，而且比中文态那批更凶：`.`(0x2E)→`VK_DELETE`（**吞掉光标后
/// 一个字符**）、`[`(0x5B)→`VK_LWIN`（弹开始菜单）、`'`(0x27)→`VK_RIGHT`、`;`(0x3B)→`VK_F1`。
///
/// # 判据
///
/// 英文半角列（col 3）是这个态唯一会改写字符的来源，故复用
/// [`custom_english_punct_chars`]——那正是「英半列有覆盖」的判据，与 DLL 英文模式吃键
/// 共用同一份，天然同源。全角由调用方的动态闸门挡（全角走 col 1，不在本函数职责内）。
///
/// ⚠️ 与姊妹函数同样的职责边界：**按键占用层（引导键 / 方案码元首码）不在此排除**，
/// 由调用方再滤一道，见 `ConfigBundle` 里的组装。
pub fn english_passthrough_punct_chars(cfg: &InputConfig) -> Vec<char> {
    let en_custom = custom_english_punct_chars(&cfg.punct);
    let mut out: Vec<char> = Vec::new();
    for ch in PUNCT_SOURCES {
        // 1. 英半列配了自定义 ⇒ 要出用户配的值，必须吃。
        if en_custom.contains(&ch) {
            continue;
        }
        // 2. 参与英文智能符号 ⇒ 连按替换要引擎接手，必须吃。
        //
        //    ⚠️ **必须连开关一起判**。`english_chars` 只是源字符集，它归
        //    `english_punct_mode` 管（本函数的场景是「中文输入 + 英文标点态」，对应的正是
        //    这一个开关；`english_mode` 管的是整个输入法切英文，那时 DLL 走另一条分支、
        //    根本不查本集合）。出厂 `english_punct_mode = false`，不判开关就等于凭一个
        //    没生效的功能把出厂 `english_chars`（`.,?!:;`）整排挡在透传之外——`.` 撞
        //    `VK_DELETE`，是这批里最该修的一个。
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

    /// 空白不参与，与 `english_smart_source_chars` 的过滤同源：配了空格的用户在英文全角下
    /// 会用空格键武装，而解除武装那道判据按键问字符（空格键答 None）⇒ press1 自解武装、
    /// press2 永不到来且零日志。
    #[test]
    fn english_participates_never_matches_whitespace() {
        let mut c = cfg();
        c.symbol.english_chars = ". ,".to_string();
        assert!(!english_participates(&c, ' '));
        assert!(english_participates(&c, '.'), "非空白成员不受影响");
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

    // ── chinese_passthrough_punct_chars ────────────────────────────────────────
    //
    // 判据的真值表锁在这里。漏掉任一条排除项的现场表现都是「某个键忽然不经引擎了」，
    // 且不报错 —— 靠真机复现的成本远高于这几条断言。

    #[test]
    fn passthrough_default_is_the_unmapped_punct_set() {
        let conv = PunctuationConverter::new();
        let c = cfg();
        // 默认配置下「标点层面产物不变」的全集，恰是中文标点表没有映射的那十个。
        // 这张表同时是撞码风险的全集（ASCII 码点 ≤0xFF 才可能撞 VK）：
        //   `#`→VK_END  `%`→VK_LEFT  `&`→VK_UP  `-`→VK_INSERT  `/`→VK_HELP  `|`→VK_F13
        //   `@` `=` 落在未分配 VK 上，`*`→VK_PRINT、`+`→VK_EXECUTE 无默认绑定，侥幸无害。
        //
        // ⚠️ 这不是最终放行集：`-` `=` `/` 这些还可能被配成引导键或方案码元首码，
        // 由 `ConfigBundle` 再滤一道（见本函数文档的职责边界说明）。
        assert_eq!(
            chinese_passthrough_punct_chars(&conv, &c),
            vec!['#', '%', '&', '*', '+', '-', '/', '=', '@', '|']
        );
    }

    #[test]
    fn english_passthrough_is_superset_of_chinese() {
        // 这条断言表达的是设计意图：英文标点态不走中文标点表，凡中文态能透传的，英文态
        // 必然也能。反过来不成立——`,` `.` `;` 这些在中文态要转，在英文态却是原样。
        // 若哪天两者出现「中文能透、英文不能」的字符，多半是判据写反了。
        let conv = PunctuationConverter::new();
        let c = cfg();
        let cn = chinese_passthrough_punct_chars(&conv, &c);
        let en = english_passthrough_punct_chars(&c);
        for ch in &cn {
            assert!(en.contains(ch), "`{ch}` 中文态能透传，英文态没有理由不能");
        }
        assert!(en.len() > cn.len(), "英文态该是真超集，不该只是相等");
    }

    #[test]
    fn english_passthrough_default_set() {
        // 默认配置下被挡住的只有配对符一类：出厂 `english_punct_mode = false`，
        // 智能符号那条判据不成立。
        let en = english_passthrough_punct_chars(&cfg());
        // ★ `.` 是这批里最危险的一个（撞 `VK_DELETE`，会吞掉光标后一个字符）。
        //   它一度因为「只看 english_chars 不看开关」被挡在修复之外——凭一个出厂关闭的
        //   功能否掉了一批键。这条锁住那个回归。
        for ch in ['.', ',', '?', '!', ':', ';'] {
            assert!(
                en.contains(&ch),
                "`{ch}` 在出厂配置（english_punct_mode=false）下产物即原样，应透传"
            );
        }
        // 开关打开后才轮到智能符号接手。
        let mut c_sw = cfg();
        c_sw.symbol.english_punct_mode = true;
        let en_sw = english_passthrough_punct_chars(&c_sw);
        for ch in ['.', ',', '?', '!', ':', ';'] {
            assert!(
                !en_sw.contains(&ch),
                "开了 english_punct_mode，`{ch}` 要留给引擎"
            );
        }
        // ★ 出厂 `auto_pair.english = false`，配对没开 ⇒ 配对符也该透传。
        //   `(`(0x28) 撞 `VK_DOWN`、`)`(0x29) 撞 `VK_SELECT`，实测就是打 `(` 光标下移、
        //   字不上屏。早先只看配对表不看开关，把它们挡在了修复之外。
        for ch in ['(', ')', '[', ']', '{', '}'] {
            assert!(
                en.contains(&ch),
                "`{ch}` 在出厂配置（auto_pair.english=false）下不参与配对，应透传"
            );
        }
        // 开关打开后才轮到配对栈接手。
        let mut c2 = cfg();
        c2.auto_pair.english = true;
        let en2 = english_passthrough_punct_chars(&c2);
        for ch in ['(', ')', '[', ']', '{', '}'] {
            assert!(!en2.contains(&ch), "开了配对，`{ch}` 要留给引擎维护配对栈");
        }
        // 中文态里被中文标点表拦下、英文态却该放行的那批，逐个点名。
        for ch in ['\'', '"', '\\', '`', '~', '$', '^', '_'] {
            assert!(en.contains(&ch), "`{ch}` 在英文半角态产物即原样，应透传");
        }
    }

    #[test]
    fn english_passthrough_excludes_en_half_custom() {
        // 英半列配了自定义 ⇒ 要出用户配的值，必须吃。判据复用
        // `custom_english_punct_chars`，与 DLL 英文模式吃键同源。
        let mut c = cfg();
        c.punct.custom_enabled = true;
        c.punct.custom_mappings.insert(
            "/".into(),
            vec![String::new(), String::new(), String::new(), "÷".into()],
        );
        assert!(!english_passthrough_punct_chars(&c).contains(&'/'));
    }

    #[test]
    fn passthrough_excludes_quotes_via_state_machine() {
        // 引号不在 `static_chinese` 表里、走交替状态机，但 `peek_chinese_str` 覆盖了它们
        // （返回 `‘` / `“`），故判据天然把它们排除。漏掉这条的后果是引号被透传、
        // 中文引号再也打不出来。
        let conv = PunctuationConverter::new();
        let got = chinese_passthrough_punct_chars(&conv, &cfg());
        assert!(!got.contains(&'\''), "单引号要转中文引号，不得透传");
        assert!(!got.contains(&'"'), "双引号要转中文引号，不得透传");
    }

    #[test]
    fn passthrough_excludes_chinese_mapped_symbols() {
        let conv = PunctuationConverter::new();
        let c = cfg();
        let got = chinese_passthrough_punct_chars(&conv, &c);
        for ch in ['!', '$', '^', '(', ')'] {
            assert!(!got.contains(&ch), "{ch} 有中文标点映射，不得透传");
        }
    }

    #[test]
    fn passthrough_excludes_any_custom_mapping_column() {
        // 中半列配了 → 必须吃（要出用户配的值）。
        let conv = PunctuationConverter::new();
        let mut c = cfg();
        c.punct.custom_enabled = true;
        c.punct.custom_mappings.insert(
            "#".into(),
            vec!["井".into(), String::new(), String::new(), String::new()],
        );
        assert!(!chinese_passthrough_punct_chars(&conv, &c).contains(&'#'));

        // ★ 只配了**英半列**也必须吃：中文输入模式下还能切到英文标点态走那一列，
        //   透传掉的话用户配的那格就永远打不出来。这条是「只看中半列」会漏掉的。
        let mut c2 = cfg();
        c2.punct.custom_enabled = true;
        c2.punct.custom_mappings.insert(
            "%".into(),
            vec![String::new(), String::new(), String::new(), "pct".into()],
        );
        assert!(!chinese_passthrough_punct_chars(&conv, &c2).contains(&'%'));
    }

    #[test]
    fn passthrough_empty_custom_column_is_not_coverage() {
        // 四列全是空串 = 回落默认转换，不算覆盖，仍可透传。
        let conv = PunctuationConverter::new();
        let mut c = cfg();
        c.punct.custom_enabled = true;
        c.punct.custom_mappings.insert(
            "#".into(),
            vec![String::new(), String::new(), String::new(), String::new()],
        );
        assert!(chinese_passthrough_punct_chars(&conv, &c).contains(&'#'));
    }

    #[test]
    fn passthrough_excludes_smart_symbol_and_pair_chars() {
        let conv = PunctuationConverter::new();
        // 参与**中文**智能符号 → 连按替换要引擎接手。中文态按**产物**判，而走到这一步的
        // 字符产物就是它自己，故把 ASCII 配进 `smart_chars` 即命中。
        let mut c = cfg();
        c.symbol.smart_chars = "#".into();
        assert!(!chinese_passthrough_punct_chars(&conv, &c).contains(&'#'));

        // ⛔ 反过来，`english_chars` **不该**影响中文态：它归 english_punct_mode /
        // english_mode 管，与中文标点态无关。早先在此按它排除，把出厂 `.,?!:;` 整排挡在
        // 透传外——而那两个开关出厂都是关的。这条锁住那个回归。
        let mut c2 = cfg();
        c2.symbol.english_chars = "#".into();
        assert!(
            chinese_passthrough_punct_chars(&conv, &c2).contains(&'#'),
            "english_chars 不该影响中文标点态的判据"
        );

        // 被配进配对表**且开关开着** → 配对栈由引擎维护，必须吃。
        let mut c2 = cfg();
        c2.auto_pair.english = true;
        c2.auto_pair.english_pairs = vec!["@&".into()];
        let got = chinese_passthrough_punct_chars(&conv, &c2);
        assert!(!got.contains(&'@'));
        assert!(!got.contains(&'&'));

        // ⛔ 开关关着时配对不生效，光有表不该挡住透传。两个开关出厂都是 false，
        // 只看表不看开关等于凭一个没开的功能否掉一批键——`(`(0x28) 撞 VK_DOWN 就是这么
        // 被漏掉的。这条锁住那个回归。
        let mut c3 = cfg();
        c3.auto_pair.english = false;
        c3.auto_pair.english_pairs = vec!["@&".into()];
        let got3 = chinese_passthrough_punct_chars(&conv, &c3);
        assert!(got3.contains(&'@'), "配对开关关着，光有表不该挡住透传");
    }
}
