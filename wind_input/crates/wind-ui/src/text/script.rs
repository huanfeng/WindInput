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
        i += step;
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
mod tests {
    use super::*;

    /// 直立竖排切格：一个基字 + 粘着它的记号。
    ///
    /// ★ 第二条断言是重点：异体选择符若单独成格，那一格排出来是空的，而**前一格的字
    /// 还会退回默认写法**——两处同时错，从画面上看只像「字长得不对」。
    #[test]
    fn upright_cells_keep_marks_with_their_base() {
        assert_eq!(upright_cells("你好世界"), ["你", "好", "世", "界"]);
        // 汉字异体选择符（U+E0101）必须并进前一格。
        assert_eq!(
            upright_cells("\u{8FB6}\u{E0101}好"),
            ["\u{8FB6}\u{E0101}", "好"]
        );
        // 日文浊点。
        assert_eq!(upright_cells("か\u{3099}な"), ["か\u{3099}", "な"]);
        // 拉丁组合音调。
        assert_eq!(upright_cells("e\u{0301}f"), ["e\u{0301}", "f"]);
        // 代理对（补充平面汉字）本就是一个 char，不会被切开。
        assert_eq!(upright_cells("\u{20000}字"), ["\u{20000}", "字"]);
        assert_eq!(upright_cells(""), Vec::<&str>::new());
        // 逐格拼回去必须还是原文——切分不得吞字。
        for s in [
            "你好",
            "a\u{0301}b\u{0302}",
            "\u{1F1E8}\u{1F1F3}x",
            "1. ᠮᠣᠩᠭᠣᠯ",
        ] {
            assert_eq!(upright_cells(s).concat(), s, "{s:?} 切分后拼不回原文");
        }
    }

    /// emoji 组合序列（ZWJ 连接、区域指示符成对）不得被拆散。
    #[test]
    fn upright_cells_keep_emoji_sequences_whole() {
        // 国旗 = 两个区域指示符。
        assert_eq!(upright_cells("\u{1F1E8}\u{1F1F3}"), ["\u{1F1E8}\u{1F1F3}"]);
        // 两面国旗相邻：必须切成两格，不能四个挤成一格。
        assert_eq!(
            upright_cells("\u{1F1E8}\u{1F1F3}\u{1F1EF}\u{1F1F5}"),
            ["\u{1F1E8}\u{1F1F3}", "\u{1F1EF}\u{1F1F5}"]
        );
        // ZWJ 序列（家庭 emoji 的一段）。
        assert_eq!(
            upright_cells("\u{1F468}\u{200D}\u{1F469}"),
            ["\u{1F468}\u{200D}\u{1F469}"]
        );
    }

    /// 类别名有**两份**：这里的 [`ScriptClass::key`]，和 wind-config 注册表里的
    /// `FONT_SCRIPT_KEYS`（`ui.font.scripts` 的键名值域，经 capability 传给设置端做预填）。
    ///
    /// ★ 断言**顺序也一致**而不只是集合相等：设置端的预填是按这个顺序逐行铺的，
    /// 顺序漂了用户每次打开对话框都看到行序变化，会被当成「配置被改了」。
    ///
    /// ⚠️ 只能在 wind-ui 侧测——wind-config 依赖不到 wind-ui。加类别时两处一起改，
    /// 漏了 wind-config 那份的表现是「core 认这个类，但设置页永远不列出来」。
    #[test]
    fn script_class_keys_match_config_registry() {
        let mine: Vec<&str> = ScriptClass::ALL.iter().map(|c| c.key()).collect();
        assert_eq!(
            mine,
            wind_config::config_schema::FONT_SCRIPT_KEYS,
            "ScriptClass::ALL 与 wind-config 的 FONT_SCRIPT_KEYS 漂移了"
        );
        // 反向：注册表里的每个名字都解析得回来（防「两边各写了同样多但不同的名字」）。
        for k in wind_config::config_schema::FONT_SCRIPT_KEYS {
            assert!(
                ScriptClass::from_key(k).is_some(),
                "注册表键 {k} 解析不出类别"
            );
        }
    }

    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// 取每段对应的原文与归属，方便断言。
    fn seg(s: &str, declared: &[ScriptClass]) -> Vec<(String, Option<ScriptClass>)> {
        let w = u16s(s);
        font_runs(&w, declared)
            .into_iter()
            .map(|r| {
                (
                    String::from_utf16_lossy(&w[r.start..r.start + r.len]),
                    r.class,
                )
            })
            .collect()
    }

    /// 二分查找依赖有序且不相交；表是手写的，这条必须钉住。
    #[test]
    fn table_is_sorted_and_disjoint() {
        for w in TABLE.windows(2) {
            assert!(w[0].0 <= w[0].1, "区间自身颠倒: {:#X?}", w[0]);
            assert!(
                w[0].1 < w[1].0,
                "未按 start 升序或区间重叠: {:#X?} 与 {:#X?}",
                w[0],
                w[1]
            );
        }
        let last = TABLE[TABLE.len() - 1];
        assert!(last.0 <= last.1);
    }

    /// ★ 段必须无缝、不重叠地覆盖整个输入——切段错位的后果是「一部分文字根本没被渲染」
    /// 或「某段被渲染两次」，而这两种都不会报错。
    #[test]
    fn runs_tile_the_input() {
        let samples = [
            "",
            "a",
            "中",
            "1. ᠮᠣᠩᠭᠣᠯ",
            "abc中文123 ᠮᠣᠩ🙂é",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "e\u{0301}cole",
            "。、！？",
        ];
        for s in samples {
            let w = u16s(s);
            for declared in [
                &[][..],
                &[ScriptClass::Latin][..],
                &[ScriptClass::Latin, ScriptClass::Digits, ScriptClass::Punct][..],
                ScriptClass::ALL,
            ] {
                let runs = font_runs(&w, declared);
                let mut pos = 0usize;
                for r in &runs {
                    assert_eq!(r.start, pos, "段不连续: {s:?} / {declared:?} / {runs:?}");
                    assert!(r.len > 0, "空段: {s:?} / {runs:?}");
                    pos += r.len;
                }
                assert_eq!(pos, w.len(), "未覆盖到末尾: {s:?} / {runs:?}");
            }
        }
    }

    /// 零配置快路径：没声明任何指派就不该切段（每段一次 COM 调用，白切是纯开销）。
    #[test]
    fn no_declaration_yields_single_run() {
        assert_eq!(
            seg("abc中文123", &[]),
            vec![("abc中文123".to_string(), None)]
        );
    }

    /// 声明了 latin：拉丁字母切出去，汉字留默认。
    #[test]
    fn latin_is_split_out_when_declared() {
        assert_eq!(
            seg("中abc文", &[ScriptClass::Latin]),
            vec![
                ("中".to_string(), None),
                ("abc".to_string(), Some(ScriptClass::Latin)),
                ("文".to_string(), None),
            ]
        );
    }

    /// ★★ 未声明的具名类必须并入默认链，**不能**变成中性——变成中性就会被邻居传染。
    /// 只声明 latin 时，汉字（Cjk 类）要老老实实待在默认链里。
    #[test]
    fn undeclared_class_joins_default_not_neutral() {
        // 若 Cjk 未声明时被当成中性，"中" 会继承前面的 "a" 而跟着拉丁字体跑。
        assert_eq!(
            seg("a中", &[ScriptClass::Latin]),
            vec![
                ("a".to_string(), Some(ScriptClass::Latin)),
                ("中".to_string(), None),
            ]
        );
    }

    /// ★★ 中性字符默认继承上下文：中文里的半角数字/标点**不得**跳到英文字体。
    /// 这是「朴素地写成 ASCII → 英文字体」最容易翻车的一条。
    #[test]
    fn neutrals_inherit_context_by_default() {
        assert_eq!(
            seg("中文123。", &[ScriptClass::Latin]),
            vec![("中文123。".to_string(), None)]
        );
        // 前向优先：拉丁后面的数字跟拉丁走。
        assert_eq!(
            seg("abc123", &[ScriptClass::Latin]),
            vec![("abc123".to_string(), Some(ScriptClass::Latin))]
        );
    }

    /// 显式声明 digits 才打破继承。
    #[test]
    fn declaring_digits_breaks_inheritance() {
        assert_eq!(
            seg("中文123", &[ScriptClass::Digits]),
            vec![
                ("中文".to_string(), None),
                ("123".to_string(), Some(ScriptClass::Digits)),
            ]
        );
    }

    /// ★★★ 蒙古文数字 `U+1810..=U+1819` 绝不能被 `digits` 类切走——切走就是在蒙文词
    /// 中间断开成形。这正是「分类轴必须是脚本、不能是字符功能」那条判据的落点。
    #[test]
    fn mongolian_digits_are_never_split_by_the_digits_class() {
        // ᠐᠑ 是蒙古文数字 0/1，夹在蒙古文字母之间。
        let s = "\u{1820}\u{1810}\u{1811}\u{1821}";
        assert_eq!(
            seg(s, &[ScriptClass::Digits, ScriptClass::Latin]),
            vec![(s.to_string(), None)],
            "蒙古文数字被切出去了——成形会断"
        );
    }

    /// 蒙古文整串（含自由变体选择符 FVS）是一个强归属段，不被拉丁指派打断。
    #[test]
    fn mongolian_run_stays_intact() {
        // ᠮᠣᠩᠭᠣᠯ + FVS1
        let s = "\u{182E}\u{1823}\u{1829}\u{182D}\u{1823}\u{182F}\u{180B}";
        assert_eq!(seg(s, &[ScriptClass::Latin]), vec![(s.to_string(), None)]);
    }

    /// 跨脚本的组合符必须跟随基字符：`e` + U+0301 不能被切成两段，否则重音会分裂。
    #[test]
    fn combining_mark_sticks_to_its_base() {
        let s = "\u{0065}\u{0301}中";
        assert_eq!(
            seg(s, &[ScriptClass::Latin]),
            vec![
                ("\u{0065}\u{0301}".to_string(), Some(ScriptClass::Latin)),
                ("中".to_string(), None),
            ]
        );
    }

    /// ZWJ 表情序列不得被切开（切开会退化成三个独立人像）。
    #[test]
    fn zwj_emoji_sequence_is_one_run() {
        let s = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(
            seg(s, &[ScriptClass::Emoji, ScriptClass::Latin]),
            vec![(s.to_string(), Some(ScriptClass::Emoji))]
        );
    }

    /// 变体选择符跟随前一个码位（keycap `1️⃣` = 数字 + VS16 + U+20E3）。
    #[test]
    fn variation_selector_and_keycap_stick() {
        let s = "\u{0031}\u{FE0F}\u{20E3}";
        assert_eq!(
            seg(s, &[ScriptClass::Digits]),
            vec![(s.to_string(), Some(ScriptClass::Digits))]
        );
    }

    /// 代理对不得被切开：段边界只落在码位边界上。
    #[test]
    fn surrogate_pairs_are_never_split() {
        // U+20000 是扩展 B 汉字（代理对），两侧是拉丁。
        let s = "a\u{20000}b";
        let w = u16s(s);
        let runs = font_runs(&w, &[ScriptClass::Latin, ScriptClass::Cjk]);
        for r in &runs {
            let head = w[r.start];
            assert!(
                !(0xDC00..=0xDFFF).contains(&head),
                "段起点落在低位代理上: {runs:?}"
            );
        }
        assert_eq!(
            seg(s, &[ScriptClass::Latin, ScriptClass::Cjk]),
            vec![
                ("a".to_string(), Some(ScriptClass::Latin)),
                ("\u{20000}".to_string(), Some(ScriptClass::Cjk)),
                ("b".to_string(), Some(ScriptClass::Latin)),
            ]
        );
    }

    /// 整串都是中性时用默认链（没有可继承的强归属）。
    ///
    /// ★ 单独这一条分不出「空格是中性」还是「空格是强归属」——两种实现结果都是单段 None。
    /// 故配一条对照：空格夹在已声明的类中间时必须被吸收进同一段。
    #[test]
    fn all_neutral_falls_back_to_default() {
        assert_eq!(
            seg("   ", &[ScriptClass::Latin]),
            vec![("   ".to_string(), None)]
        );
        assert_eq!(
            seg("a b", &[ScriptClass::Latin]),
            vec![("a b".to_string(), Some(ScriptClass::Latin))],
            "空格成了强归属：会把一段拉丁切成三段"
        );
    }

    /// 段首的中性字符没有前驱，回退到后向继承。
    #[test]
    fn leading_neutral_inherits_backward() {
        assert_eq!(
            seg("  abc", &[ScriptClass::Latin]),
            vec![("  abc".to_string(), Some(ScriptClass::Latin))]
        );
    }

    /// 硬换行 `\n` 必须中性——它若成为强归属就会把一行多行候选切成互不相干的段。
    ///
    /// ★ 样本两侧必须是**已声明的类**：写成 `seg("中\n文", &[Latin])` 的话，两侧的汉字
    /// 本身也是默认链，`\n` 无论中性还是强归属都并成单段 —— 测试名声称钉住的东西钉不住。
    #[test]
    fn newline_is_neutral() {
        assert_eq!(
            seg("a\nb", &[ScriptClass::Latin]),
            vec![("a\nb".to_string(), Some(ScriptClass::Latin))],
            "\\n 成了强归属：多行候选会被切成互不相干的段"
        );
    }

    /// ★★ 表外的**强归属**必须真的是强归属：未列脚本（蒙古文）与已声明类（拉丁）相邻时
    /// 必须各自成段。
    ///
    /// 这条守的是 `font_runs` 里 `Raw::StrongOther => Some(None)` 那一臂，与
    /// `undeclared_class_joins_default_not_neutral` 守的 `Raw::Class(c)` 臂是**两条独立
    /// 代码路径**。此前三条「纯蒙文/纯 PUA」的用例全是整串同类，把 `StrongOther` 改成
    /// `Neutral(None)` 它们照样全绿（全中性时没有强归属可继承，输出同样是单段）——
    /// 也就是说本模块的整个卖点「蒙文串不被拉丁指派打断」曾经没有任何断言在守。
    #[test]
    fn undeclared_script_stays_strong_next_to_a_declared_class() {
        let mongolian = "\u{1820}\u{1821}";
        assert_eq!(
            seg(&format!("abc{mongolian}"), &[ScriptClass::Latin]),
            vec![
                ("abc".to_string(), Some(ScriptClass::Latin)),
                (mongolian.to_string(), None),
            ],
            "蒙古文被当成中性、跟着拉丁指派跑了"
        );
        // 反向（蒙文在前）同样成立——继承是前向优先，别让方向掩盖问题。
        assert_eq!(
            seg(&format!("{mongolian}abc"), &[ScriptClass::Latin]),
            vec![
                (mongolian.to_string(), None),
                ("abc".to_string(), Some(ScriptClass::Latin)),
            ]
        );
    }

    /// ★ Common 符号块必须中性。它们曾整段落进「表外 = 强归属」，于是 `abc→def`
    /// 被切成三段、中间那段掉回默认链，宽度预算与实际排版对不上。
    ///
    /// 这个失效在「只声明 latin」下也看得见（→ 自成一段），但 `第①条` 那种必须声明
    /// `cjk` 才暴露，故两种都测。
    #[test]
    fn common_symbols_are_neutral_and_do_not_split_runs() {
        // 箭头夹在拉丁中间：整串一段。
        assert_eq!(
            seg("abc→def", &[ScriptClass::Latin]),
            vec![("abc→def".to_string(), Some(ScriptClass::Latin))]
        );
        // 带圈数字夹在汉字中间：声明 cjk 时整串仍是一段。
        assert_eq!(
            seg("第①条", &[ScriptClass::Cjk, ScriptClass::Latin]),
            vec![("第①条".to_string(), Some(ScriptClass::Cjk))]
        );
        // 货币符号、℃、补充标点同理。
        assert_eq!(
            seg("€100", &[ScriptClass::Latin, ScriptClass::Digits]),
            vec![("€100".to_string(), Some(ScriptClass::Digits))]
        );
        assert_eq!(
            seg("25℃", &[ScriptClass::Cjk]),
            vec![("25℃".to_string(), None)]
        );
    }

    /// emoji tag 序列（英格兰旗）不得被切开——与 ZWJ 序列完全同型的失效。
    #[test]
    fn emoji_tag_sequence_is_one_run() {
        let flag = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
        assert_eq!(
            seg(flag, &[ScriptClass::Emoji, ScriptClass::Latin]),
            vec![(flag.to_string(), Some(ScriptClass::Emoji))],
            "tag 序列被切开：旗帜会退化成一面纯黑旗"
        );
    }

    /// 国旗由两个区域指示符组成（U+1F1E6..1F1FF），必须与其它 emoji 同属 Emoji 类，
    /// 否则同一行里两种 emoji 用两种字体、度量不一致。
    #[test]
    fn regional_indicator_flags_belong_to_the_emoji_class() {
        let s = "\u{1F1E8}\u{1F1F3}\u{1F642}"; // 🇨🇳🙂
        assert_eq!(
            seg(s, &[ScriptClass::Emoji]),
            vec![(s.to_string(), Some(ScriptClass::Emoji))]
        );
    }

    /// U+FEFF（ZWNBSP/BOM）夹在词中间不得切一刀。
    #[test]
    fn zwnbsp_does_not_split_a_word() {
        assert_eq!(
            seg("ab\u{FEFF}cd", &[ScriptClass::Latin]),
            vec![("ab\u{FEFF}cd".to_string(), Some(ScriptClass::Latin))]
        );
    }

    /// 西里尔扩展 A 整块都是组合符，必须粘连——否则声明 cyrillic 时重音与基字符分裂。
    /// 它证伪了「组合符总与基字符同脚本块」这个直觉（该块不在 U+0400..052F 内）。
    #[test]
    fn cyrillic_combining_extension_sticks() {
        let s = "\u{0430}\u{2DE0}"; // а + 西里尔组合字母
        assert_eq!(
            seg(s, &[ScriptClass::Cyrillic]),
            vec![(s.to_string(), Some(ScriptClass::Cyrillic))]
        );
    }

    /// 注音声调符（U+02C7 ˇ 等）是 Common 不是 Latin：声明 latin 时不得把它从注音串切走。
    #[test]
    fn bopomofo_tone_marks_are_not_latin() {
        assert_eq!(
            seg("\u{3123}\u{02C7}", &[ScriptClass::Latin, ScriptClass::Cjk]),
            vec![("\u{3123}\u{02C7}".to_string(), Some(ScriptClass::Cjk))]
        );
    }

    /// 落单代理走 `next_cp` 的兜底臂——`&str` 造不出这种输入，必须直接喂 `&[u16]`，
    /// 否则那条兜底分支「文档承诺了、测试碰不到」。
    #[test]
    fn lone_surrogates_do_not_panic_or_misalign() {
        for w in [
            vec![0xD800u16, 0x0061],         // 落单高位代理 + 'a'
            vec![0x0061u16, 0xDC00],         // 'a' + 落单低位代理
            vec![0xDC00u16],                 // 只有一个低位代理
            vec![0xD800u16, 0xD800, 0x4E2D], // 连续两个高位代理 + 汉字
        ] {
            let runs = font_runs(&w, &[ScriptClass::Latin, ScriptClass::Cjk]);
            let total: usize = runs.iter().map(|r| r.len).sum();
            assert_eq!(total, w.len(), "落单代理下段长总和对不上: {runs:?}");
            assert_eq!(runs.first().map(|r| r.start), Some(0));
        }
    }

    /// 全角标点归 CJK 而不是 Punct：声明 punct 指派英文字体时，「。」不得跟着跑。
    #[test]
    fn fullwidth_punct_belongs_to_cjk() {
        assert_eq!(
            seg("中文。", &[ScriptClass::Punct, ScriptClass::Latin]),
            vec![("中文。".to_string(), None)]
        );
    }

    /// 私用区（拆字字根）落默认链——它随后会被 chaizi 的字体集覆盖，不该被脚本指派抢走。
    #[test]
    fn private_use_area_falls_to_default() {
        assert_eq!(
            seg("\u{E0E1}\u{E0E2}", &[ScriptClass::Latin, ScriptClass::Cjk]),
            vec![("\u{E0E1}\u{E0E2}".to_string(), None)]
        );
    }

    // ── FontPlan ──────────────────────────────────────────────────────────

    fn chain(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 归一化：空白字体名剔除、空链整条丢弃、同类后写胜出。
    #[test]
    fn plan_normalizes_input() {
        let p = FontPlan::new(
            chain(&["  Mongolian Baiti  ", "", "   "]),
            vec![
                (ScriptClass::Latin, chain(&["Consolas"])),
                (ScriptClass::Cjk, chain(&["", "  "])),
                (ScriptClass::Latin, chain(&["Segoe UI", "Arial"])),
            ],
        );
        assert_eq!(p.base_family(), Some("Mongolian Baiti"));
        assert_eq!(p.declared(), &[ScriptClass::Latin], "空链的 cjk 不该被声明");
        assert_eq!(
            p.chain_for(Some(ScriptClass::Latin)),
            ["Segoe UI", "Arial"],
            "同类重复声明应后写胜出"
        );
    }

    /// ★ 书写顺序不同、内容相同的两份方案必须相等且同 hash——它要进测量缓存的键，
    /// 顺序敏感会让同一份配置换个写法就全 miss（缓存失效是静默的，只表现为掉帧）。
    #[test]
    fn plan_equality_is_write_order_independent() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let a = FontPlan::new(
            chain(&["A"]),
            vec![
                (ScriptClass::Punct, chain(&["P"])),
                (ScriptClass::Latin, chain(&["L"])),
            ],
        );
        let b = FontPlan::new(
            chain(&["A"]),
            vec![
                (ScriptClass::Latin, chain(&["L"])),
                (ScriptClass::Punct, chain(&["P"])),
            ],
        );
        assert_eq!(a, b);
        let h = |p: &FontPlan| {
            let mut s = DefaultHasher::new();
            p.hash(&mut s);
            s.finish()
        };
        assert_eq!(h(&a), h(&b));
    }

    /// 平凡方案（≤1 个默认字体、无指派）要能被识别出来——调用方据此完全走旧路径。
    #[test]
    fn trivial_plan_is_detected() {
        assert!(FontPlan::default().is_trivial());
        assert!(FontPlan::new(chain(&["A"]), vec![]).is_trivial());
        assert!(!FontPlan::new(chain(&["A", "B"]), vec![]).is_trivial());
        assert!(
            !FontPlan::new(chain(&["A"]), vec![(ScriptClass::Latin, chain(&["L"]))]).is_trivial()
        );
    }

    /// 未指派的类回落默认链（不 panic），`None` 也取默认链。
    #[test]
    fn chain_for_falls_back_to_default() {
        let p = FontPlan::new(
            chain(&["A", "B"]),
            vec![(ScriptClass::Latin, chain(&["L"]))],
        );
        assert_eq!(p.chain_for(None), ["A", "B"]);
        assert_eq!(p.chain_for(Some(ScriptClass::Cjk)), ["A", "B"]);
        assert_eq!(p.chain_for(Some(ScriptClass::Latin)), ["L"]);
    }

    /// 只有真有回退项时才需要构造自定义 fallback：没有就走系统默认，零回归。
    #[test]
    fn needs_fallback_only_when_a_chain_has_more_than_one() {
        assert!(
            !FontPlan::new(chain(&["A"]), vec![(ScriptClass::Latin, chain(&["L"]))])
                .needs_fallback()
        );
        assert!(FontPlan::new(chain(&["A", "B"]), vec![]).needs_fallback());
        assert!(
            FontPlan::new(
                chain(&["A"]),
                vec![(ScriptClass::Latin, chain(&["L", "M"]))]
            )
            .needs_fallback()
        );
    }

    /// `chains()` 覆盖默认链与全部指派链，且不吐空链。
    #[test]
    fn chains_enumerates_default_first_then_assignments() {
        let p = FontPlan::new(
            chain(&["A"]),
            vec![(ScriptClass::Latin, chain(&["L", "M"]))],
        );
        let got: Vec<Vec<String>> = p.chains().map(|c| c.to_vec()).collect();
        assert_eq!(got, vec![chain(&["A"]), chain(&["L", "M"])]);
        // 默认链为空时不该吐出一条空链（它会被当成「base family 是空串」建映射）。
        let q = FontPlan::new(vec![], vec![(ScriptClass::Latin, chain(&["L"]))]);
        assert_eq!(q.chains().count(), 1);
    }

    /// 键名 round-trip：设置页与配置文件靠它对齐。
    ///
    /// ⚠️ 遍历 `ALL` 自己是**测不出 `ALL` 漏项**的：将来加一个变体却忘了写进 `ALL`，
    /// `key()`/`from_key()` 的 match 有穷尽性检查会拦，`ALL` 不会——设置页少一项、测试照绿。
    /// 故显式钉住条数，加变体时必须一并改这个数字。
    #[test]
    fn class_keys_round_trip() {
        assert_eq!(ScriptClass::ALL.len(), 7, "加了变体却没写进 ALL");
        for &c in ScriptClass::ALL {
            assert_eq!(ScriptClass::from_key(c.key()), Some(c));
        }
        assert_eq!(ScriptClass::from_key("LATIN"), Some(ScriptClass::Latin));
        assert_eq!(ScriptClass::from_key(" latin "), Some(ScriptClass::Latin));
        assert_eq!(ScriptClass::from_key("mongolian"), None);
    }
}
