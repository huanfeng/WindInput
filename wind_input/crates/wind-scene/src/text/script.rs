//! 按脚本把文本切成字体段——**判定域**的脚本表 + 两遍扫描的归属解析。
//!
//! 服务于「一个字体不含英文（或英文字形难看）时，为拉丁字符单独指派字体」这类需求。
//! 产出是一组 [`FontRun`]，调用方按段调 `SetFontFamilyName`（DirectWrite）或等价接口。
//!
//! # ⛔ 与 `wind-candidate` 的 `charblock` 是两张表，不要合并
//!
//! 那张表的模块文档已经写过同型的禁令（它与 `is_han` 也是两张）。判据一致：
//!
//! | | 漏一块的后果 |
//! |---|---|
//! | `charblock`（显示域） | 类型列显示「其它」——一个不好看的标签 |
//! | 本表（判定域） | 那批字**用错字体渲染**，或更糟：在词中间切断成形 |
//!
//! 具体到本仓：`charblock` 里**根本没有蒙古文块**，且它自己写明「新版 Unicode 的新块会
//! 落进『其它』，而那是可以接受的退化」。对标签可以接受，对字体指派不行。故本表
//! **按大段/平面兜底**（如整个平面 2~3 归 CJK），不逐块列举。
//!
//! # ★★ 切段边界必须落在脚本边界上
//!
//! `SetFontFamilyName(range)` 切出来的段是**独立的 shaping 单元**。蒙古文、阿拉伯文都是
//! 上下文相关成形（同一字母在词首/词中/词尾形状不同），在词中间切一刀成形就断了。
//!
//! 最阴的陷阱是**按「字符功能」而非「脚本」分类**：把「数字」当成一类指派给英文字体，
//! 蒙古文数字 `U+1810..=U+1819` 就会被从蒙文词里切走，把一个词劈成三段。故本表里
//! [`ScriptClass::Digits`] **只含 ASCII 数字**，各脚本自己的数字一律留在本脚本内。
//!
//! # 中性字符继承上下文（UAX #24 的 Common script）
//!
//! `"1. ᠮᠣᠩᠭᠣᠯ"` 里的 `1` 和 `.` 归谁？朴素地写成「ASCII → 英文字体」的话，中文候选里的
//! 半角数字和标点也会跳字体——而混用字体会改行高（见下），表现是「带数字的那几项行高
//! 和别人不一样」。
//!
//! 故空格/数字/西文标点默认是 [`Raw::Neutral`]，**归属继承相邻的强脚本**；只有用户
//! **显式声明**了 `digits` / `punct` 指派时才提升为强归属——声明了就是显式意图。
//!
//! # ⚠️ 混用字体会改行高
//!
//! DirectWrite 的行高取该行**所有字体中最大**的 line metrics。给拉丁指派一个 line gap
//! 更大的字体，整行候选会变高。`candidate_window.rs` 的占位行注释已经为同型问题写过一次
//! （「真实后端的行高来自该字族的 line metrics」）——占位行与宽度预算路径必须与真实渲染
//! 用同一份指派，否则预算按一种字体算、排版按另一种走。
//!
//! # ⚠️ 与拆字字根（PUA）的编排次序是一条硬约束
//!
//! `dwrite.rs` 的 `create_layout` 是四层叠加、**后写覆盖前写**：
//! TextFormat 全局字族 → 全文 base family → 本模块的脚本段 → `pua_runs` 的私用区段。
//! 脚本段必须夹在中间，且 `class == None` 的段**不得**再调一次 `SetFontFamilyName`——
//! 顺序搞反、或对 `None` 段也下发，都会把字根段的字族盖回主字体，字根变方框。
//! 那正是 `pua_runs` 文档里记着的那个历史 bug。
//!
//! 判据上两者不冲突：三段私用区（BMP `U+E000..F8FF` 与补充 A/B）在本表里**全部落表外**
//! ⇒ [`Raw::StrongOther`] ⇒ 永远自成一段、不会被并进任何已声明的类。
//! `private_use_area_falls_to_default` 钉着这条。

/// 用户可在配置里指派字体的具名脚本类。
///
/// 刻意只有这几类：它们是「一个字体常常缺、或缺得难看」的那几个。其余脚本（蒙古文、
/// 阿拉伯文、天城文……）走 [`FontRun::class`] 为 `None` 的默认链——**它们仍然被识别为
/// 强脚本**，不会被当成中性字符传染出去。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScriptClass {
    /// 拉丁字母（含扩展 A/B、IPA、拉丁扩展附加）。**不含** ASCII 数字与标点。
    Latin,
    /// 希腊字母（含希腊扩展）。
    Greek,
    /// 西里尔字母。
    Cyrillic,
    /// 汉字、假名、谚文、注音、CJK 标点与全角形式，以及补充平面的汉字扩展。
    Cjk,
    /// 表情符号。
    Emoji,
    /// **ASCII** 数字 `0-9`。各脚本自己的数字不在此列，理由见模块文档。
    Digits,
    /// ASCII 与通用标点。全角标点归 [`Self::Cjk`]，不在此列。
    ///
    /// ⚠️ 已知代价：`U+00B7 ·`（人名间隔号「米开朗基罗·博纳罗蒂」）与 `U+00B0 °`（25°C）
    /// 也在本类里。声明 punct 会把它们从中文串中切走，间隔号跟着西文标点字体跑。
    /// 这与「全角标点刻意归 Cjk」是同一条判据的边缘情形——半角形式在中文里同样常用，
    /// 但它们的**码位**在拉丁补充区，按脚本无从区分。
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
/// # 为什么不能直接 `chars()`
///
/// 每格会被单独排版一次，切错的表现是**记号掉到下一格去**——组合音调符号、日文浊点
/// （U+3099/309A）、汉字异体字选择符（U+FE00–FE0F / U+E0100–E01EF）会各自单独成格，
/// 渲染成一个孤立的点或直接不显示。异体选择符这条对汉字尤其要命：它区分同一码位的两种写法。
///
/// Rust 的 `char` 是完整标量，代理对天然不会被切开——这是选 `char_indices` 而不是按 UTF-16
/// 推进的理由。
///
/// # 已知边界
///
/// ⚠️ 这不是完整的字素簇算法（那需要一张 Unicode 属性表）。覆盖的是**这个模式实际会遇到**
/// 的：CJK ＋拉丁＋数字＋标点＋常见 emoji。天城文那类需要重排序的元音符号不在其列。
///
/// ⚠️ 更根本的一条：直立竖排按格切分，**必然切断连写脚本的字形连接**（阿拉伯文、蒙古文）。
/// 那些脚本要用的是整项旋转（`rotated`），不是本模式。判据不在这个函数里，而在
/// 「谁会选这个布局」——故此处只记不拦。
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

/// 该码位是否**必须跟着前一个字**（不能独立成格）。
///
/// # ⛔ 与 [`Raw::Sticky`] 是两张表，不要合并
///
/// `Raw::Sticky` 的文档写明它**只列跨脚本的那几个**，因为同脚本的组合符与基字符落在同一
/// 区间、按脚本分段天然就在一起。本函数没有那层保护：每一格是独立的排版单元，
/// 同脚本的组合符照样会被切走。故这里必须逐类列全，两张表的取舍方向是相反的。
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

/// 一个码位的**原始**分类（尚未考虑用户声明了哪些类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Raw {
    /// 明确属于某个具名类。
    Class(ScriptClass),
    /// 明确属于某个脚本，但不是具名类（蒙古文、阿拉伯文、天城文、私用区……）。
    /// **是强归属**：不继承邻居，也不被邻居继承。
    StrongOther,
    /// 中性：归属继承上下文。`Some(c)` 表示用户若显式声明了类 `c`，本码位提升为 `Class(c)`。
    Neutral(Option<ScriptClass>),
    /// 粘连：**跨脚本**的组合符 / 变体选择符 / 连接符，无条件跟随前一个码位。
    ///
    /// ★ 这里只需列跨脚本的那几个有限集合：绝大多数组合符与其基字符**同脚本块**
    /// （蒙古文的 FVS `U+180B..=U+180D` 就在蒙古文块内），按脚本分类天然把它们分到一起。
    /// 穷举 Unicode 全部 Mn/Mc 既做不到也没必要。
    Sticky,
}

/// 码位区间 → 原始分类。**必须按 `start` 升序且互不相交**
/// （`table_is_sorted_and_disjoint` 钉着；二分查找依赖这条）。
///
/// 表外一律 [`Raw::StrongOther`]——这个默认方向对**脚本**是对的：没列到的脚本要被当成
/// 强脚本，当成中性会让它被邻居传染（蒙古文串里夹一个未列的字符就会跟着拉丁走）。
///
/// ⚠️ **但对 Common（通用）符号块恰好相反，它们必须显式列成中性。**
/// 「表外 = 强归属」曾把 `U+2070..20CF`（上下标、货币 €）、`U+20F1..25FF`
/// （℃ ™ № Ⅰ → ∑ ① ■）、`U+27C0..2AFF`、`U+2E00..2E7F` 一并划成强归属，
/// 于是 `abc→def` 被切成三段、`第①条` 被切成三段——中间那段掉回默认链，
/// 宽度预算与实际排版对不上。
/// ★ 这个 bug 在「只声明 latin」的最小用例下**看不见**（汉字与 ① 都是默认链、会并成一段），
/// 必须声明 `cjk` 或 `emoji` 才暴露，人工试用极易溜过。故这些块逐段列在下表里。
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
    // 修饰字母（Spacing Modifier Letters）是 Script=Common **不是 Latin**：
    // ˇ ˊ ˋ ˙（U+02C7/02CA/02CB/02D9）是注音声调符，U+02EA/02EB 在 UCD 里就是 Bopomofo。
    // 划进 Latin 会让声明 latin 的用户把注音串里的声调符切到拉丁字体。
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
    // ⚠️ 归 Neutral(None) 而非 Emoji：这一段里只有零星几个是 emoji（⌚⏰ℹ），
    // 整段归 Emoji 会让 ① ■ → 跟着 emoji 字体跑，代价远大于收益。
    // 代价是声明 emoji 也搬不走 ⌚⏰ —— 已知取舍。
    (0x20F1, 0x25FF, Raw::Neutral(None)),
    // 杂项符号/装饰符号：只有一部分是 emoji（★ ☆ ✓ 等不是），故默认中性、
    // **声明了 emoji 才切走**——注意保护只在「未声明 emoji」时成立，声明后 ★ 同样跟着走。
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
    // 起点下探到 0x1F000（而非 0x1F300）以纳入**区域指示符** U+1F1E6..1F1FF——
    // 国旗由两个区域指示符组成，漏掉它们会让 🇨🇳 与 🙂 在声明 emoji 后用两种字体。
    (0x1F000, 0x1FAFF, Raw::Class(ScriptClass::Emoji)),         // 麻将/纸牌/区域指示符/表情
    // ★ 按平面兜底而非逐块列举：扩展 B..J 及以后的新扩展全部落进来。
    //   逐块列举正是 issue #83 的成因（`is_han` 差一个码位就把新扩展漏到域外）。
    (0x20000, 0x3FFFF, Raw::Class(ScriptClass::Cjk)),
    // emoji tag 序列（🏴󠁧󠁢󠁥󠁮󠁧󠁿 = U+1F3F4 + tag + U+E007F）。与 ZWJ 序列完全同型：
    // 不粘连就会在这里切一刀，旗帜退化成一面纯黑旗，且 tag 段连回退渲染都没有。
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

/// 一个字体段：`[start, start + len)` 半开区间，下标与长度均以 **UTF-16 码元** 计
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
/// `declared` 是用户**显式声明了字体指派**的类。只有被声明的类才会切出独立段：未声明的
/// 类并入默认链，避免为「反正都用同一个字体」的文本产生大量无意义的段（每段一次
/// `SetFontFamilyName` COM 调用）。
///
/// # 返回值的性质（`runs_tile_the_input` 钉着）
///
/// 返回的段**无缝、不重叠地覆盖整个输入**，且边界永不落在代理对中间。
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

    // 第一遍：逐**码位**分类，结果按码元展开成 `(码元起点, 码元长度, 归属)`。
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

    // 第一遍半：emoji 呈现序列的**回溯改写**。
    //
    // 键帽（基字 + 可选 U+FE0F + U+20E3，如 1️⃣ #️⃣）与「文本码位 + U+FE0F 表情呈现选择符」
    // （©️ ™️ ☑️ ▶️）只有整串落在**同一个字体段**里，emoji 字体的连字 / 呈现替换才会触发。
    // 而基字多半是 ASCII 数字或中性符号：第一遍把它判成待继承（声明了 digits 则是 Digits），
    // 键帽符与 FE0F 又是粘连——三格各归各家，回退从不触发（基准字体自带「1」），连字永远
    // 不成，画面上是一个普通的「1」加一个空框。这是纯代码缺陷，换任何系统版本都修不好。
    //
    // 三条判据缺一不可：
    // - 只在 Emoji 已声明时改写。未声明时一切照旧——这意味着本段单独上线什么都不会变，
    //   它依赖出厂对 emoji 类的指派（`candidate_window::build_font_plan`）才生效；
    // - 只认 U+FE0F。U+FE0E 是**文本**呈现选择符，语义正好相反，提升到 emoji 字体等于把
    //   用户「我要文字形态」的显式意图反着执行；
    // - 改写赢过基字已有的强归属（同时声明了 digits 时基字是 Digits）：连字是比「这是个
    //   数字」更具体的事实。但基字若是强脚本字符（汉字、拉丁字母后面跟了个孤零零的 FE0F）
    //   则不动——那不是任何 emoji 序列，搬过去只会让它换一款字体。
    //
    // 只改归属字段、不动起点与长度，切段的无缝覆盖与代理对完整性自动保持。竖排的
    // `upright_cells` 早已靠 `is_trailing_mark` 把这些序列并成一格，这里是横排的同一件事。
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

    // 第二遍：待继承的格子并入相邻强归属——**前向优先**（读序上「跟着前面走」更符合直觉，
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
/// # 两层机制刻意分开，缺一不可
///
/// | | 语义 | 触发条件 |
/// |---|---|---|
/// | **回退链**（链里的第 2 项起） | 「本字体**没有**这个字 → 依次试下一个」 | 仅当当前字体缺字 |
/// | **脚本指派**（[`Self::scripts`]） | 「拉丁字符**一律**用 X」 | 无条件，字符落在该类即生效 |
///
/// 只做回退链解决不了问题的一半：**绝大多数字体都带 ASCII 字形**（Mongolian Baiti 也有），
/// 一旦字体自己「有」英文，回退链永远不触发，想换也换不掉。反过来指派也替代不了回退链——
/// 用户指派的英文字体总有覆盖不到的字符。两者的合成顺序是：
/// **指派决定这一段的 base family → 该段再各自走自己的回退链**。
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
    /// 归一化构造：去掉空白字体名与空链，按类升序去重（同类多次声明**后写胜出**）。
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
