//! 按脚本把文本切成字体段：判定域的脚本表 + 两遍扫描的归属解析。
//!
//! 产出一组 [`FontRun`]，调用方按段调 `SetFontFamilyName`（DirectWrite）或等价接口。
//! 背景与判据见 `docs/design/mongolian-vertical-candidates.md` 的 C 节。要点：
//!
//! - 与 `wind-candidate` 的 `charblock`（显示域）是两张表，不要合并：本表漏一块会让字用错
//!   字体或在词中间切断成形，故按大段/平面兜底，不逐块列举。
//! - 切段边界就是 shaping 边界，必须落在脚本边界上：分类轴是「脚本」不是「字符功能」，
//!   [`ScriptClass::Digits`] 只含 ASCII 数字（蒙古文数字留在蒙文词内）。
//! - 空格/数字/西文标点默认中性（[`Raw::Neutral`]），归属继承相邻强脚本；用户显式声明了
//!   `digits` / `punct` 才提升为强归属。混用字体会改行高，占位行与宽度预算必须用同一份指派。
//! - 与拆字字根（PUA）的编排次序：`create_layout` 后写覆盖前写，脚本段夹在 base family 与
//!   `pua_runs` 之间，且 `class == None` 的段不得再调 `SetFontFamilyName`，否则字根变方框。
//!   私用区在本表全部落表外（[`Raw::StrongOther`]），见 `private_use_area_falls_to_default`。

/// 用户可在配置里指派字体的具名脚本类。
///
/// 刻意只有这几类（字体常常缺、或缺得难看的）。其余脚本走 [`FontRun::class`] 为 `None`
/// 的默认链，但仍被识别为强脚本，不会被当成中性字符传染出去。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScriptClass {
    /// 拉丁字母（含扩展 A/B、IPA、拉丁扩展附加）。不含 ASCII 数字与标点。
    Latin,
    /// 希腊字母（含希腊扩展）。
    Greek,
    /// 西里尔字母。
    Cyrillic,
    /// 汉字、假名、谚文、注音、CJK 标点与全角形式，以及补充平面的汉字扩展。
    Cjk,
    /// 表情符号。
    Emoji,
    /// ASCII 数字 `0-9`。各脚本自己的数字不在此列，理由见模块文档。
    Digits,
    /// ASCII 与通用标点。全角标点归 [`Self::Cjk`]，不在此列。
    ///
    /// 已知代价：`U+00B7 ·`（人名间隔号）与 `U+00B0 °` 也在本类里，声明 punct 会把它们从
    /// 中文串中切走。它们的码位在拉丁补充区，按脚本无从区分。
    Punct,
}

impl ScriptClass {
    /// 配置里的键名。
    pub fn key(self) -> &'static str {
        match self {
            Self::Latin => "latin",
            Self::Greek => "greek",
            Self::Cyrillic => "cyrillic",
            Self::Cjk => "cjk",
            Self::Emoji => "emoji",
            Self::Digits => "digits",
            Self::Punct => "punct",
        }
    }

    /// 从配置键名解析；未知键返回 `None`（调用方按「忽略并记一条日志」处理，
    /// 不要 panic——配置文件是用户手写的）。
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key.trim().to_ascii_lowercase().as_str() {
            "latin" => Self::Latin,
            "greek" => Self::Greek,
            "cyrillic" => Self::Cyrillic,
            "cjk" => Self::Cjk,
            "emoji" => Self::Emoji,
            "digits" => Self::Digits,
            "punct" => Self::Punct,
            _ => return None,
        })
    }

    /// 全部取值，供设置页/文档枚举与测试穷举。
    pub const ALL: &'static [ScriptClass] = &[
        Self::Latin,
        Self::Greek,
        Self::Cyrillic,
        Self::Cjk,
        Self::Emoji,
        Self::Digits,
        Self::Punct,
    ];
}

/// 直立竖排（对联式）的「一格」切分：一个基字 + 粘在它后面的所有记号。
///
/// 不能直接按 `chars()`：每格单独排版，组合音调符号、日文浊点、汉字异体字选择符会掉到
/// 下一格、渲染成孤立的点或不显示。
///
/// 已知边界：不是完整的字素簇算法，只覆盖 CJK + 拉丁 + 数字 + 标点 + 常见 emoji；按格切分
/// 必然切断连写脚本（阿拉伯文、蒙古文）的字形连接，那些脚本应走整项旋转（`rotated`）。
pub fn upright_cells(s: &str) -> Vec<&str> {
    const ZWJ: u32 = 0x200D;
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    // 上一个字符是 ZWJ ⇒ 下一个必须并入本格（emoji 组合序列）。
    let mut join_next = false;
    // 本格是「只有一个区域指示符」的状态 ⇒ 下一个区域指示符与它配成国旗。
    let mut lone_ri = false;
    for (i, c) in s.char_indices() {
        let sticky = join_next || is_trailing_mark(c) || (lone_ri && is_regional_indicator(c));
        match start {
            Some(st) if !sticky => {
                out.push(&s[st..i]);
                start = Some(i);
            }
            None => start = Some(i),
            _ => {}
        }
        join_next = c as u32 == ZWJ;
        lone_ri = is_regional_indicator(c) && !sticky;
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

fn is_regional_indicator(c: char) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&(c as u32))
}

/// 该码位是否必须跟着前一个字（不能独立成格）。
///
/// 与 [`Raw::Sticky`] 是两张表，不要合并：那张只列跨脚本的（同脚本组合符按脚本分段天然在
/// 一起），这里每格独立排版，同脚本的组合符也得逐类列全。
fn is_trailing_mark(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F   // 组合音调符号
        | 0x0483..=0x0489 // 西里尔组合符
        | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7 // 希伯来
        | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC // 阿拉伯
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E // 泰文
        | 0x180B..=0x180F // 蒙古文自由变体选择符
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF // 组合符扩展
        | 0x200B..=0x200D | 0xFEFF // 零宽（ZWSP/ZWNJ/ZWJ/BOM）
        | 0x20D0..=0x20F0 // 组合用记号（含 U+20E3 键帽）
        | 0x302A..=0x302F // CJK 声调标记
        | 0x3099..=0x309A // 日文浊点/半浊点
        | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F // 变体选择符 / 半标记
        | 0xE0020..=0xE007F // 标签字符（旗帜子序列）
        | 0xE0100..=0xE01EF // 变体选择符补充（汉字异体）
    )
}

/// 一个码位的原始分类（尚未考虑用户声明了哪些类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Raw {
    /// 明确属于某个具名类。
    Class(ScriptClass),
    /// 明确属于某个脚本，但不是具名类（蒙古文、阿拉伯文、天城文、私用区……）。
    /// 是强归属：不继承邻居，也不被邻居继承。
    StrongOther,
    /// 中性：归属继承上下文。`Some(c)` 表示用户若显式声明了类 `c`，本码位提升为 `Class(c)`。
    Neutral(Option<ScriptClass>),
    /// 粘连：跨脚本的组合符 / 变体选择符 / 连接符，无条件跟随前一个码位。只需列跨脚本的
    /// 有限集合：绝大多数组合符与基字符同脚本块，按脚本分类天然在一起。
    Sticky,
}

/// 码位区间 → 原始分类。必须按 `start` 升序且互不相交（`table_is_sorted_and_disjoint`
/// 钉着；二分查找依赖这条）。
///
/// 表外一律 [`Raw::StrongOther`]：没列到的脚本要当强脚本，否则会被邻居传染。但 Common
/// 符号块恰好相反，必须显式列成中性，否则 `abc→def`、`第①条` 会被切成三段（只在声明
/// `cjk` 或 `emoji` 时暴露）。
#[rustfmt::skip]
const TABLE: &[(u32, u32, Raw)] = &[
    // ── ASCII ─────────────────────────────────────────────────────────────
    // 控制字符含 \n：必须中性，否则硬换行会把一行候选切成两段。
    (0x0000, 0x0020, Raw::Neutral(None)),
    (0x0021, 0x002F, Raw::Neutral(Some(ScriptClass::Punct))),
    (0x0030, 0x0039, Raw::Neutral(Some(ScriptClass::Digits))),
    (0x003A, 0x0040, Raw::Neutral(Some(ScriptClass::Punct))),
    (0x0041, 0x005A, Raw::Class(ScriptClass::Latin)),
    (0x005B, 0x0060, Raw::Neutral(Some(ScriptClass::Punct))),
    (0x0061, 0x007A, Raw::Class(ScriptClass::Latin)),
    (0x007B, 0x007E, Raw::Neutral(Some(ScriptClass::Punct))),
    (0x007F, 0x00A0, Raw::Neutral(None)),                       // DEL..NBSP
    (0x00A1, 0x00A9, Raw::Neutral(Some(ScriptClass::Punct))),   // ¡¢£¤¥¦§¨©
    (0x00AA, 0x00AA, Raw::Class(ScriptClass::Latin)),           // ª（UCD 里 Script=Latin）
    (0x00AB, 0x00B9, Raw::Neutral(Some(ScriptClass::Punct))),   // «¬­®¯°±²³´µ¶·¸¹
    (0x00BA, 0x00BA, Raw::Class(ScriptClass::Latin)),           // º（同 ª）
    (0x00BB, 0x00BF, Raw::Neutral(Some(ScriptClass::Punct))),   // »¼½¾¿
    // ── 拉丁 ──────────────────────────────────────────────────────────────
    (0x00C0, 0x00D6, Raw::Class(ScriptClass::Latin)),
    (0x00D7, 0x00D7, Raw::Neutral(Some(ScriptClass::Punct))),   // ×
    (0x00D8, 0x00F6, Raw::Class(ScriptClass::Latin)),
    (0x00F7, 0x00F7, Raw::Neutral(Some(ScriptClass::Punct))),   // ÷
    (0x00F8, 0x02AF, Raw::Class(ScriptClass::Latin)),           // 扩展 A/B + IPA
    // 修饰字母是 Script=Common 不是 Latin：其中有注音声调符（ˇ ˊ ˋ ˙），划进 Latin 会把
    // 注音串里的声调符切到拉丁字体。
    (0x02B0, 0x02FF, Raw::Neutral(None)),
    (0x0300, 0x036F, Raw::Sticky),                              // 组合附加符号
    (0x0370, 0x03FF, Raw::Class(ScriptClass::Greek)),
    (0x0400, 0x052F, Raw::Class(ScriptClass::Cyrillic)),
    (0x1100, 0x11FF, Raw::Class(ScriptClass::Cjk)),             // 谚文字母（组合式，与音节同族）
    (0x1AB0, 0x1AFF, Raw::Sticky),                              // 组合符扩展
    (0x1D00, 0x1DBF, Raw::Class(ScriptClass::Latin)),           // 音标扩展 + 补充
    (0x1DC0, 0x1DFF, Raw::Sticky),                              // 组合符补充
    (0x1E00, 0x1EFF, Raw::Class(ScriptClass::Latin)),           // 拉丁扩展附加
    (0x1F00, 0x1FFF, Raw::Class(ScriptClass::Greek)),           // 希腊扩展
    // ── 通用标点与符号（全是 Script=Common，必须中性，理由见上文表头）────────
    (0x2000, 0x200A, Raw::Neutral(None)),                       // 各种宽度的空格
    (0x200B, 0x200F, Raw::Sticky),                              // 零宽 / 方向标记（含 ZWJ）
    (0x2010, 0x2027, Raw::Neutral(Some(ScriptClass::Punct))),
    (0x2028, 0x202F, Raw::Neutral(None)),                       // 行/段分隔、方向控制
    (0x2030, 0x205E, Raw::Neutral(Some(ScriptClass::Punct))),
    (0x205F, 0x206F, Raw::Neutral(None)),
    (0x2070, 0x20CF, Raw::Neutral(None)),                       // 上下标 + 货币符号（€ ₹ ₽）
    (0x20D0, 0x20F0, Raw::Sticky),                              // 组合用记号（含 keycap U+20E3）
    // 字母式符号 ℃™№ / 数字形式 ⅠⅡ / 箭头 → / 数学 ∑ / 技术 ⌚ / 带圈 ① / 制表 / 几何 ■。
    // 归 Neutral(None) 而非 Emoji：这段只有零星几个 emoji（⌚⏰ℹ），整段归 Emoji 会让
    // ① ■ → 跟着 emoji 字体跑。代价是声明 emoji 也搬不走 ⌚⏰。
    (0x20F1, 0x25FF, Raw::Neutral(None)),
    // 杂项符号/装饰符号：只有一部分是 emoji，故默认中性、声明了 emoji 才切走（声明后
    // ★ ✓ 等非 emoji 也会跟着走）。
    (0x2600, 0x27BF, Raw::Neutral(Some(ScriptClass::Emoji))),
    (0x27C0, 0x2AFF, Raw::Neutral(None)),                       // 补充箭头 / 数学运算符
    (0x2B00, 0x2BFF, Raw::Neutral(Some(ScriptClass::Emoji))),
    (0x2C60, 0x2C7F, Raw::Class(ScriptClass::Latin)),           // 拉丁扩展 C
    (0x2DE0, 0x2DFF, Raw::Sticky),                              // 西里尔扩展 A：整块都是组合符
    (0x2E00, 0x2E7F, Raw::Neutral(Some(ScriptClass::Punct))),   // 补充标点
    // ── CJK ───────────────────────────────────────────────────────────────
    (0x2E80, 0x2FDF, Raw::Class(ScriptClass::Cjk)),             // CJK 部首补充 + 康熙部首
    (0x2FF0, 0x2FFF, Raw::Class(ScriptClass::Cjk)),             // 表意文字描述符
    // 全角标点（U+3001 等）刻意归 CJK 而不是 Punct：它们的字形与度量必须跟中文字体走。
    (0x3000, 0x9FFF, Raw::Class(ScriptClass::Cjk)),             // CJK 标点/假名/注音/谚文兼容/扩展A/基本汉字
    (0xA640, 0xA69F, Raw::Class(ScriptClass::Cyrillic)),        // 西里尔扩展 B（含其组合符）
    (0xA720, 0xA7FF, Raw::Class(ScriptClass::Latin)),           // 拉丁扩展 D
    (0xA960, 0xA97F, Raw::Class(ScriptClass::Cjk)),             // 谚文字母扩展 A
    (0xAB30, 0xAB6F, Raw::Class(ScriptClass::Latin)),           // 拉丁扩展 E
    (0xAC00, 0xD7FF, Raw::Class(ScriptClass::Cjk)),             // 谚文音节 + 字母扩展 B
    (0xF900, 0xFAFF, Raw::Class(ScriptClass::Cjk)),             // 兼容汉字
    (0xFB00, 0xFB06, Raw::Class(ScriptClass::Latin)),           // 拉丁连字 ﬁ ﬂ
    (0xFE00, 0xFE0F, Raw::Sticky),                              // 变体选择符
    (0xFE10, 0xFE1F, Raw::Class(ScriptClass::Cjk)),             // 竖排形式
    (0xFE20, 0xFE2F, Raw::Sticky),                              // 组合用半符号
    (0xFE30, 0xFE6F, Raw::Class(ScriptClass::Cjk)),             // CJK 兼容形式 + 小写变体
    (0xFEFF, 0xFEFF, Raw::Sticky),                              // ZWNBSP/BOM：夹在词中间不得切开
    (0xFF00, 0xFFEF, Raw::Class(ScriptClass::Cjk)),             // 半角及全角形式
    (0xFFF0, 0xFFFF, Raw::Neutral(None)),                       // 特殊区（含替换字符 U+FFFD）
    // ── 补充平面 ──────────────────────────────────────────────────────────
    (0x1AFF0, 0x1B16F, Raw::Class(ScriptClass::Cjk)),           // 假名扩展 B / 补充 / 小写假名
    // 起点下探到 0x1F000 以纳入区域指示符 U+1F1E6..1F1FF（国旗）。
    (0x1F000, 0x1FAFF, Raw::Class(ScriptClass::Emoji)),         // 麻将/纸牌/区域指示符/表情
    // 按平面兜底而非逐块列举：扩展 B..J 及以后的新扩展全部落进来。
    (0x20000, 0x3FFFF, Raw::Class(ScriptClass::Cjk)),
    // emoji tag 序列（U+1F3F4 + tag + U+E007F），同 ZWJ 序列须粘连，否则旗帜退化成纯黑旗。
    (0xE0020, 0xE007F, Raw::Sticky),
    (0xE0100, 0xE01EF, Raw::Sticky),                            // 变体选择符补充
];

/// 查表：这个码位的原始分类。
fn raw_of(cp: u32) -> Raw {
    // 二分：表约 40 项、每次绘制逐字符调用，比线性扫描稳妥。有序性由测试钉住。
    let mut lo = 0usize;
    let mut hi = TABLE.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (s, e, r) = TABLE[mid];
        if cp < s {
            hi = mid;
        } else if cp > e {
            lo = mid + 1;
        } else {
            return r;
        }
    }
    Raw::StrongOther
}

/// 一个字体段：`[start, start + len)` 半开区间，下标与长度均以 UTF-16 码元计
/// （`DWRITE_TEXT_RANGE` 要的就是码元）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontRun {
    pub start: usize,
    pub len: usize,
    /// 该段的字体归属：`Some(c)` = 用具名类 `c` 指派的字体链；`None` = 用默认链。
    pub class: Option<ScriptClass>,
}

/// 取下标 `i` 处的码位与它占的码元数。落单的代理返回其原值、步长 1
/// （畸形文本不该让切段逻辑 panic 或错位）。
fn next_cp(wide: &[u16], i: usize) -> (u32, usize) {
    let u = wide[i];
    let lead = (0xD800..=0xDBFF).contains(&u);
    let trail = wide
        .get(i + 1)
        .copied()
        .filter(|t| (0xDC00..=0xDFFF).contains(t));
    match (lead, trail) {
        (true, Some(t)) => (
            0x1_0000 + (((u as u32 - 0xD800) << 10) | (t as u32 - 0xDC00)),
            2,
        ),
        _ => (u as u32, 1),
    }
}

/// 把 UTF-16 序列按脚本切成字体段。
///
/// `declared` 是用户显式声明了字体指派的类。只有被声明的类才会切出独立段：未声明的
/// 类并入默认链，避免为「反正都用同一个字体」的文本产生大量无意义的段（每段一次
/// `SetFontFamilyName` COM 调用）。
///
/// # 返回值的性质（`runs_tile_the_input` 钉着）
///
/// 返回的段无缝、不重叠地覆盖整个输入，且边界永不落在代理对中间。
/// `declared` 为空时恒返回单段（零配置快路径）。空输入返回空表。
pub fn font_runs(wide: &[u16], declared: &[ScriptClass]) -> Vec<FontRun> {
    if wide.is_empty() {
        return Vec::new();
    }
    let one = |class| {
        vec![FontRun {
            start: 0,
            len: wide.len(),
            class,
        }]
    };
    if declared.is_empty() {
        return one(None);
    }

    // 第一遍：逐码位分类，结果按码元展开成 `(码元起点, 码元长度, 归属)`。
    // 归属此时是三态：`Some(Some(c))` 强归属具名类、`Some(None)` 强归属默认链、
    // `None` 待继承（中性 / 粘连）。
    #[allow(clippy::type_complexity)]
    let mut cells: Vec<(usize, usize, Option<Option<ScriptClass>>)> =
        Vec::with_capacity(wide.len());
    let mut cps: Vec<u32> = Vec::with_capacity(wide.len());
    let mut i = 0usize;
    while i < wide.len() {
        let (cp, step) = next_cp(wide, i);
        let owner = match raw_of(cp) {
            // 具名类被声明才切出去；没声明就并入默认链（而不是变成中性——它仍是强脚本，
            // 不该被邻居传染，也不该去传染邻居）。
            Raw::Class(c) => Some(declared.contains(&c).then_some(c)),
            Raw::StrongOther => Some(None),
            // 中性字符：对应的类被显式声明时提升为强归属，否则待继承。
            Raw::Neutral(Some(c)) if declared.contains(&c) => Some(Some(c)),
            Raw::Neutral(_) | Raw::Sticky => None,
        };
        cells.push((i, step, owner));
        cps.push(cp);
        i += step;
    }

    // 第一遍半：emoji 呈现序列的回溯改写。键帽（基字 + 可选 U+FE0F + U+20E3）与「码位 +
    // U+FE0F」只有整串落在同一字体段里，emoji 字体的连字才会触发；否则显示成「1」加空框。
    // 判据：只在 Emoji 已声明时改写（依赖 `candidate_window::build_font_plan` 的出厂指派）；
    // 只认 U+FE0F（U+FE0E 是文本呈现，语义相反）；改写赢过基字的 Digits 等中性提升，但基字
    // 是强脚本字符时不动。只改归属、不动起止，切段的无缝覆盖自动保持。
    if declared.contains(&ScriptClass::Emoji) {
        // 数字在 `TABLE` 里是 `Neutral(Some(Digits))`，已被第一支覆盖。
        let promotable =
            |cp: u32| matches!(raw_of(cp), Raw::Neutral(_) | Raw::Class(ScriptClass::Emoji));
        for k in 1..cells.len() {
            let base = match cps[k] {
                0xFE0F => k - 1,
                0x20E3 if cps[k - 1] == 0xFE0F && k >= 2 => k - 2,
                0x20E3 => k - 1,
                _ => continue,
            };
            if !promotable(cps[base]) {
                continue;
            }
            for cell in &mut cells[base..=k] {
                cell.2 = Some(Some(ScriptClass::Emoji));
            }
        }
    }

    // 第二遍：待继承的格子并入相邻强归属——前向优先（读序上「跟着前面走」更符合直觉，
    // 也让 `"abc 中文"` 里的空格跟拉丁走），段首没有前驱时回退到后向；整串都待继承
    // （纯标点/纯空白）时用默认链。
    let mut owners: Vec<Option<ScriptClass>> = vec![None; cells.len()];
    let mut last: Option<Option<ScriptClass>> = None;
    for (k, cell) in cells.iter().enumerate() {
        if let Some(o) = cell.2 {
            last = Some(o);
        }
        owners[k] = last.unwrap_or(None);
    }
    // 回填开头那一段没有前驱的格子：取第一个强归属。
    if let Some(first_strong) = cells.iter().position(|c| c.2.is_some()) {
        let o = cells[first_strong].2.unwrap_or(None);
        for slot in owners.iter_mut().take(first_strong) {
            *slot = o;
        }
    }

    // 第三遍：合并相邻同归属。
    let mut runs: Vec<FontRun> = Vec::new();
    for (k, &(start, step, _)) in cells.iter().enumerate() {
        match runs.last_mut() {
            Some(r) if r.class == owners[k] => r.len += step,
            _ => runs.push(FontRun {
                start,
                len: step,
                class: owners[k],
            }),
        }
    }
    runs
}

/// 一套字体方案：默认链 + 具名脚本类的指派链。
///
/// 回退链（缺字才试下一个）与脚本指派（落在该类即无条件生效）是两层机制，缺一不可：
/// 字体多半自带 ASCII 字形，回退链换不掉英文；指派的字体又总有覆盖不到的字。合成顺序：
/// 指派决定这一段的 base family，该段再走自己的回退链。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FontPlan {
    /// 默认字体链。第 1 项是 base family，其余是回退顺序。空 = 用渲染器的全局字族。
    default: Vec<String>,
    /// 具名脚本类 → 字体链。按 [`ScriptClass`] 升序存放且每类至多一条，
    /// 使 `Hash`/`Eq` 与配置里的书写顺序无关——否则同一份配置换个写法就让测量缓存全 miss。
    scripts: Vec<(ScriptClass, Vec<String>)>,
    /// [`Self::scripts`] 的键，与之同序。派生字段，构造时算一次：
    /// [`font_runs`] 在每次排版的热路径上要它，现算就是每次一次分配。
    declared: Vec<ScriptClass>,
}

impl FontPlan {
    /// 归一化构造：去掉空白字体名与空链，按类升序去重（同类多次声明后写胜出）。
    pub fn new(default: Vec<String>, scripts: Vec<(ScriptClass, Vec<String>)>) -> Self {
        fn clean(chain: Vec<String>) -> Vec<String> {
            chain
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
        let mut kept: Vec<(ScriptClass, Vec<String>)> = Vec::new();
        for (c, chain) in scripts {
            let chain = clean(chain);
            if chain.is_empty() {
                continue;
            }
            match kept.iter_mut().find(|(k, _)| *k == c) {
                Some(slot) => slot.1 = chain,
                None => kept.push((c, chain)),
            }
        }
        kept.sort_by_key(|(c, _)| *c);
        let declared = kept.iter().map(|(c, _)| *c).collect();
        Self {
            default: clean(default),
            scripts: kept,
            declared,
        }
    }

    /// 只有一个默认字体、没有任何指派 —— 与升级前的单字族行为完全等价，
    /// 调用方可据此走原路径，一行 COM 调用都不多做。
    pub fn is_trivial(&self) -> bool {
        self.default.len() <= 1 && self.scripts.is_empty()
    }

    /// 默认链的 base family（`None` = 用渲染器全局字族）。
    pub fn base_family(&self) -> Option<&str> {
        self.default.first().map(|s| s.as_str())
    }

    /// 被显式指派了字体的类，升序。直接喂给 [`font_runs`]。
    pub fn declared(&self) -> &[ScriptClass] {
        &self.declared
    }

    /// 某个归属的字体链。`None` 取默认链；未指派的类不会出现在 [`font_runs`] 的结果里，
    /// 真传进来也回落默认链而不是 panic。
    pub fn chain_for(&self, class: Option<ScriptClass>) -> &[String] {
        match class {
            Some(c) => self
                .scripts
                .iter()
                .find(|(k, _)| *k == c)
                .map(|(_, v)| v.as_slice())
                .unwrap_or(&self.default),
            None => &self.default,
        }
    }

    /// 是否有任何一条链需要回退（长度 ≥ 2）。没有就不必构造自定义 fallback 对象——
    /// 不构造 = 走系统默认回退 = 与升级前逐位等价，零回归。
    pub fn needs_fallback(&self) -> bool {
        self.default.len() > 1 || self.scripts.iter().any(|(_, v)| v.len() > 1)
    }

    /// 全部链（默认链在前），供调用方按 `chain[0]` 建回退映射。
    pub fn chains(&self) -> impl Iterator<Item = &[String]> {
        std::iter::once(self.default.as_slice())
            .chain(self.scripts.iter().map(|(_, v)| v.as_slice()))
            .filter(|c| !c.is_empty())
    }
}

#[cfg(test)]
#[path = "script_tests.rs"]
mod tests;
