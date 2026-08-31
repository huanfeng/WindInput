//! 候选**注释段**（候选右侧灰字）的模板渲染——`ui.candidate.comment_template_*` 的唯一消费点。
//!
//! # 模板语法
//!
//! | 写法 | 含义 |
//! |---|---|
//! | `${name}` | 变量替换 |
//! | `${name:arg}` | 带参数的变量（如 `${chaizi_all:／}` 指定逐字分隔符） |
//! | `${a\|b\|c}` | 取**首个非空**的变量 |
//! | `{ … }` | 可选段：段内变量**全为空**则整段（含字面文本）消失 |
//!
//! 另有两条隐含规则：
//! - **空变量吞掉紧邻的一个空白**：`{(拼: ${pinyin} ${chaizi})}` 在拆字为空时得
//!   `(拼: nǐ hǎo)` 而非 `(拼: nǐ hǎo )`。
//! - **整个模板视为一个隐式可选段**：所有变量都为空时输出空串，故 `拼:${pinyin}` 在查不到
//!   读音时不会剩下一个孤零零的 `拼:`。
//!
//! 可用变量见 [`Coordinator::eval_var`]。不做转义：注释模板里出现字面 `$`/`{`/`}` 的概率极低，
//! 真需要时再加，现在加只是徒增用户要记的规则。
//!
//! 面向用户的完整说明在文档站 `customize/candidate-comment`（设置页对话框里有跳转按钮）——
//! 语法还会继续长（将来的注释库变量、模式级覆盖），塞进设置页的说明框只会越来越挤。
//!
//! # 为什么是模板而不是「来源列表 + 分隔符」
//!
//! 前一版是有序来源列表 `comment_sources` 加一个 `comment_separator`。它表达不了装饰字符
//! （括号、标签文字）、表达不了「A 为空时用 B」，而且**顺序、内容、分隔三件事散在三个键里**。
//! 模板把它们收进一个字符串，顺带消掉了三处复杂度：分隔符配置、按长度量级硬编码的横排过滤、
//! 以及为保出厂零回归而引入的「编码类来源互斥」。
//!
//! 更要紧的是**可扩展性**：新增一个注释来源（如将来的独立注释库）只是多一个变量名，
//! 配置结构完全不动。
//!
//! # 横竖各持一份模板
//!
//! 两种排布的可用横向空间差一个数量级（竖排每行独占，横排全部候选共享一行宽度），能放什么
//! 本就不是同一个答案。共用一份的结果必是「为竖排配的拼音把横排候选窗撑爆」或「为横排收着
//! 配的注释让竖排一片空白」。
//!
//! **由此，上一版的「溢出转悬停提示」机制已删除**：横排显示什么由横排模板自己决定，
//! 「放不下所以推去气泡」这个前提不存在了。那套机制本身还有个缺陷——它按同名标签追加
//! （`拼音:` / `拆字:`），而 `ui.tooltip.pinyin_enabled` 出厂即 `true`，于是气泡里会同时
//! 出现 tooltip 自己的逐字 `[拼音]` 段和追加的那份，等于把重复做实了。悬停提示现回归由
//! `ui.tooltip.*` 独家负责。
//!
//! # 三层：模式级 → 方案级 → 全局
//!
//! | 层 | 落点 |
//! |---|---|
//! | 模式级 | `input.{temp_english,temp_pinyin,url}` / `schema.mix_modes[]` / 方案文件 `[overlay]` |
//! | 方案级 | 方案文件 `[candidate].comment_template_*` |
//! | 全局 | `ui.candidate.comment_template_*` |
//!
//! 上两层是三态（键缺失=跟随下一层／非空=覆盖／空串=本层不显示注释），最底层必有值。
//! 决策点 [`resolve_template`]，与 `layout::vertical_for` 同构。
//!
//! ★ **「没意见」的语义是「跟随下一层」而不是「跟随全局」**——加了方案层之后这两者
//! 不再等价。设计与判据见 `docs/design/candidate-comment-layering.md`。
//!
//! # 为什么装配收在协调器
//!
//! 变量的数据源分属多个 crate —— `Candidate::comment`（wind-engine 产）、候选自身的
//! `code`/`boundary`、`ReverseLookup`（wind-reverse）、`codetable_reverse_hint`
//! （wind-engine）。只有协调器同时够得着。
//!
//! 解析/渲染（纯函数 [`parse`] / [`render`]）与变量求值（[`Coordinator::comment_for`]）
//! 刻意分开，与 `layout.rs` 同构：前者可用任意求值闭包测出完整语法矩阵，不必构造协调器。

use crate::coordinator::State;
use crate::pipeline::ModeKind;
use wind_candidate::{Candidate, CandidateSource};
use wind_config::config::CommentTemplateOverride;
use wind_config::{Config, OverlaySpec};

/// 一次变量引用：名字 + 可选参数（`${chaizi_all:／}` 的 `／`）。
///
/// 参数**不 trim**，名字才 trim：`${chaizi_all: · }` 里那两个空格正是用户要的分隔符，
/// 削掉它就没法配出「亻尔 · 女子」。而 `${ pinyin }` 这种手滑仍要认。
#[derive(Debug, Clone, PartialEq, Eq)]
struct VarRef {
    name: String,
    arg: Option<String>,
}

impl VarRef {
    /// 解析 `name` 或 `name:arg`。只切**第一个**冒号——分隔符本身可以含冒号。
    fn parse(s: &str) -> Self {
        match s.split_once(':') {
            Some((n, a)) => Self {
                name: n.trim().to_string(),
                arg: Some(a.to_string()),
            },
            None => Self {
                name: s.trim().to_string(),
                arg: None,
            },
        }
    }
}

/// 模板节点。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    /// 字面文本。
    Text(String),
    /// `${a|b}`：按序取首个非空变量。
    Var(Vec<VarRef>),
    /// `{ … }`：段内变量全空则整段消失。不嵌套（段内的 `{` 按字面处理）。
    Group(Vec<Node>),
}

/// 解析模板。**不会失败**——未闭合的 `${` / `{` 一律退化为字面文本。
///
/// 宽容而非报错，是因为这个字符串由用户在设置页手打，且它的产物直接显示在候选栏里：
/// 语法写错时让他看到自己打的原文（`${pinyn}` 原样出现），比弹一个错误对话框或者静默
/// 变空更容易自己改对。
fn parse(tpl: &str) -> Vec<Node> {
    let b = tpl.as_bytes();
    let mut nodes = Vec::new();
    let mut text = String::new();
    let mut i = 0usize;
    while i < b.len() {
        // `${a|b}` —— 先于裸 `{` 判定，否则变量的 `{` 会被当成段起点。
        if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
            if let Some(end) = find_byte(b, i + 2, b'}') {
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                nodes.push(Node::Var(
                    tpl[i + 2..end]
                        .split('|')
                        .map(VarRef::parse)
                        .filter(|v| !v.name.is_empty())
                        .collect(),
                ));
                i = end + 1;
                continue;
            }
            // 未闭合 → 后面全是字面文本。
        } else if b[i] == b'{' {
            // 段内容的扫描必须跳过 `${…}`，否则变量的 `}` 会被误当作段结束。
            if let Some(end) = find_group_end(b, i + 1) {
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                // 段不嵌套：内部再出现的 `{` 由递归解析按字面文本处理（它找不到配对的 `}`）。
                nodes.push(Node::Group(parse(&tpl[i + 1..end])));
                i = end + 1;
                continue;
            }
        }
        // 按字符推进，保证切片落在 UTF-8 边界上（模板含中文标签文字）。
        let ch_len = utf8_len(b[i]);
        text.push_str(&tpl[i..(i + ch_len).min(tpl.len())]);
        i += ch_len;
    }
    if !text.is_empty() {
        nodes.push(Node::Text(text));
    }
    nodes
}

/// UTF-8 首字节 → 该字符的字节数（非法首字节按 1 处理，与解析的宽容取向一致）。
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

fn find_byte(b: &[u8], from: usize, target: u8) -> Option<usize> {
    (from..b.len()).find(|&i| b[i] == target)
}

/// 找可选段的结束 `}`，**跳过内部的 `${…}`**。未闭合返回 `None`。
fn find_group_end(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
            // 变量未闭合 ⇒ 段也无从闭合（`?` 即 return None）。
            i = find_byte(b, i + 2, b'}')? + 1;
            continue;
        }
        if b[i] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 渲染结果：文本 + 「本段里出现过非空变量吗」。
///
/// 后者是可选段与顶层的存废依据，**必须与文本分开返回**：一个段可能渲染出非空文本
/// （字面装饰字符）却一个变量都没填上，那正是要整段丢弃的情形（`(拼: )`）。
struct Rendered {
    text: String,
    any_var_filled: bool,
}

/// 渲染节点序列。`eval` 按变量名求值：`None` = **未知变量名**，`Some("")` = 已知但为空。
///
/// 两者刻意区分：未知变量名原样输出 `${name}` 并**计作已填充**，于是拼错的变量名一定会
/// 显示在候选栏里让用户看见。若把未知当空处理，用户得到的是「配了没反应」——本仓记忆里
/// 反复出现的那类静默失效。
fn render_nodes(nodes: &[Node], eval: &impl Fn(&str, Option<&str>) -> Option<String>) -> Rendered {
    let mut out = String::new();
    let mut any = false;
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Var(refs) => {
                // 未知名恒排在「首个非空」判定之外单独处理：它不是值，是错误提示。
                let mut value: Option<String> = None;
                for r in refs {
                    match eval(&r.name, r.arg.as_deref()) {
                        None => {
                            value = Some(format!("${{{}}}", r.name));
                            break;
                        }
                        Some(v) if !v.is_empty() => {
                            value = Some(v);
                            break;
                        }
                        Some(_) => {} // 已知但空 → 试下一个回退
                    }
                }
                match value {
                    Some(v) => {
                        out.push_str(&v);
                        any = true;
                    }
                    // 空变量吞掉紧邻的一个空白：`(拼: ${pinyin} ${chaizi})` 在拆字为空时
                    // 不留下 `)` 前那个多余空格。只吞一个——吞到底会把用户有意排的版式抹平。
                    None => {
                        if out.ends_with(' ') || out.ends_with('\t') {
                            out.pop();
                        }
                    }
                }
            }
            Node::Group(inner) => {
                let r = render_nodes(inner, eval);
                if r.any_var_filled {
                    out.push_str(&r.text);
                    any = true;
                } else if out.ends_with(' ') || out.ends_with('\t') {
                    // 整段消失时同样吞掉紧邻空白（`${code}{ (${pinyin})}` → `wq`，非 `wq `）。
                    out.pop();
                }
            }
        }
    }
    Rendered {
        text: out,
        any_var_filled: any,
    }
}

/// 渲染模板。**纯函数**。
///
/// 整个模板按一个隐式可选段处理：所有变量都为空 ⇒ 返回空串。否则返回渲染结果（已 trim
/// 首尾空白——模板里为分隔而写的空格，在相邻内容缺席时不该留在两端）。
///
/// `max_chars` = 0 表示不限；超出则截断并加 `…`。
pub(crate) fn render(
    tpl: &str,
    max_chars: usize,
    eval: impl Fn(&str, Option<&str>) -> Option<String>,
) -> String {
    let r = render_nodes(&parse(tpl), &eval);
    if !r.any_var_filled {
        return String::new();
    }
    let s = r.text.trim();
    if max_chars == 0 {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    let head: String = chars[..max_chars].iter().collect();
    format!("{head}…")
}

/// 音节间分隔符。**注音用空格**（rime 注音惯例 `nǐ hǎo`）而非隔音符 `'`：
/// 隔音符是**编码**域的写法（`ni'hao` 是「怎么打」），带声调的注音是**读音**域（「怎么读」），
/// 两者混排会让人以为那串可以照着打。
const SYLLABLE_SEP: &str = " ";

/// 按**音节边界真值**把 `code` 切成音节序列：`nihao` + `0b101` → `["ni", "hao"]`。
///
/// `boundary` 的 bit i 置位 = 第 i **字节**是音节起点（见
/// `wind_dict::binformat::DictEntry::boundary`），真值来自词库源数据 `你好\tni hao` 里的
/// 那个空格。bit 0 是整串起点。
///
/// 超过 64 字节的部分不再有边界信息（bitmask 装不下），并入最后一个音节 —— 拼音词长上限
/// 远小于此，实际不触发，但不加这个界会读到 `>> 64` 的未定义移位。
fn syllables_of(code: &str, boundary: u64) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 1..code.len().min(64) {
        if (boundary >> i) & 1 == 1 {
            out.push(&code[start..i]);
            start = i;
        }
    }
    if start < code.len() {
        out.push(&code[start..]);
    }
    out
}

/// 候选的**带声调注音**。
///
/// 声调只存在于读音表（`pinyin_map.txt`）里，编码域一律无调（拼音输入法不打声调）；而多音字
/// 的正确读音只有词条编码知道。故两边都要用：**拿编码的音节去筛读音表**，见
/// `ReverseLookup::toned_pinyin_of`。
///
/// 拼音来源候选带 `code` + `boundary` 时给得出音节序列（消歧准确）；其余候选（码表/短语/
/// 英文…）没有拼音码可依，传 `None` 退回逐字最常用读音，多音字可能不准 —— 这是数据下界。
///
/// ⚠️ `boundary == 0` 的语义是「**无边界信息**」而非「单音节」（单音节是 `0b1`），必须当作
/// 拿不到音节序列。否则五笔码 `wqvb` 会被当成一个拼音音节送去筛读音表。
fn pinyin_text(
    c: &Candidate,
    infer: impl FnOnce(&str) -> String,
    lookup: impl FnOnce(&str, Option<&[&str]>) -> String,
) -> String {
    // 路径 A —— 拼音来源候选自带词条真值，**优先于任何推断**：`code` 就是用户实际打出这个
    // 词的音节串、`boundary` 是词库标注的切分，比枚举笛卡尔积回查词典更可靠也更省。
    // ⚠️ `boundary == 0` 是「无边界信息」不是「单音节」（单音节是 `0b1`）。
    if c.source == CandidateSource::Pinyin && !c.code.is_empty() && c.boundary != 0 {
        let syls = syllables_of(&c.code, c.boundary);
        return lookup(&c.text, Some(&syls));
    }
    // 路径 B —— 码表/短语等非拼音来源：没有拼音码可依，交给引擎**按词推断**。
    //
    // ⚠️ 这条路径此前直接落到「逐字最常用读音」，于是五笔方案下词组注音系统性出错
    // （「行长」→ `xíng cháng`，两个字都错），而五笔用户恰恰是注音功能最主要的受众
    // ——打得出但不会读。推断走 `EngineManager::word_pinyin_syllables`，它枚举每字读音的
    // 笛卡尔积、取第一个**能在拼音词典里查回该词**的组合，查不回的组合直接排除。
    let inferred = infer(&c.text);
    if inferred.is_empty() {
        // 推断失败（含非汉字、生僻多音字超组合数护栏）→ 交由查表层逐字取最常用读音。
        return lookup(&c.text, None);
    }
    let syls: Vec<&str> = inferred.split(' ').filter(|s| !s.is_empty()).collect();
    lookup(&c.text, Some(&syls))
}

/// 「模式 → 注释模板覆盖」映射。**唯一一处**把这层对应关系写死的地方——新增模式只加一行。
///
/// 返回 `None` = 该模式没有覆盖，跟随全局；`Some(s)` = 用 `s`（`s` 可能是空串，
/// 语义是「本模式不显示注释」——这正是三态里「缺失」与「空」必须分开的原因）。
///
/// 与 [`crate::layout::intent_for`] 同构（声明式重算，不做「进入时保存、退出时回放」），
/// 但**刻意不含 `add_word`**：加词面板走 `show_add_word_preview` 独立绘制路径，
/// 根本不经过 `comment_for`，给它加键只会是永远不生效的死配置。
/// 这两个函数的模式集合不同不是遗漏——布局要管所有会显示候选窗的路径，注释只管渲染注释的那条。
///
/// `overlay` = 当前特殊模式的 `[overlay]` 段快照（`State::overlay_spec`）。它与 `cfg`
/// 共用生命周期 `'a`，因为返回值可能借用其中任一方——快照存在 `State` 里正是为了让
/// 这个借用有处可依：特殊模式的配置已下沉到方案文件，每次现查注册表拿到的是临时值。
pub(crate) fn template_for<'a>(
    cfg: &'a Config,
    overlay: Option<&'a OverlaySpec>,
    active: Option<ModeKind>,
    vertical: bool,
) -> Option<&'a str> {
    let pick = |v: &'a CommentTemplateOverride,
                h: &'a CommentTemplateOverride|
     -> Option<&'a str> { if vertical { v.as_deref() } else { h.as_deref() } };
    match active {
        Some(ModeKind::Mix(i)) => cfg
            .schema
            .mix_modes
            .get(i as usize)
            .and_then(|m| pick(&m.comment_template_vertical, &m.comment_template_horizontal)),
        // 生僻字模式并入本支：它的 `overlay` 恒为 None（没有宿主方案、没有 [overlay] 段），
        // 于是 `and_then` 直接给出 None ＝ 跟随全局模板。这正是想要的默认档。
        Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) => {
            overlay.and_then(|o| pick(&o.comment_template_vertical, &o.comment_template_horizontal))
        }
        Some(ModeKind::TempPinyin) => pick(
            &cfg.input.temp_pinyin.comment_template_vertical,
            &cfg.input.temp_pinyin.comment_template_horizontal,
        ),
        Some(ModeKind::TempEnglish) => pick(
            &cfg.input.temp_english.comment_template_vertical,
            &cfg.input.temp_english.comment_template_horizontal,
        ),
        Some(ModeKind::Url) => pick(
            &cfg.input.url.comment_template_vertical,
            &cfg.input.url.comment_template_horizontal,
        ),
        // 辅助码沿用主路径注释模板（辅助码只是筛选，注释来源仍是拼音主流程）。
        Some(ModeKind::AuxCode) => None,
        None => None,
    }
}

/// 方案级注释模板（`[candidate].comment_template_*`）。`None` = 本方案没意见。
pub(crate) fn schema_template_of(
    behavior: &wind_config::SchemaBehavior,
    vertical: bool,
) -> Option<&str> {
    if vertical {
        behavior.comment_template_vertical.as_deref()
    } else {
        behavior.comment_template_horizontal.as_deref()
    }
}

/// 三层裁决：**模式级 → 方案级 → 全局**。
///
/// # ★ 「没意见」的语义是「跟随**下一层**」，不是「跟随全局」
///
/// 加了方案层之后这两者不再等价。唯一能区分本实现与旧的两层实现的格子是
/// **「模式无意见 + 方案有意图 + 全局有值」**——旧实现给全局值，本实现给方案值。
/// 测试 `schema_layer_wins_when_mode_absent` 钉住它。
///
/// 三态里的第三态（空串 = 不显示）**在每一层都成立**：`Some("")` 是一个有意见的层，
/// 它压过下面所有层并渲染成空注释。故这里只能用 `Option::or`，不能顺手写成
/// 「非空才算数」——那会让「本模式/本方案不要注释」变得无法表达。
pub(crate) fn resolve_template<'a>(
    mode: Option<&'a str>,
    schema: Option<&'a str>,
    global: &'a str,
) -> &'a str {
    mode.or(schema).unwrap_or(global)
}

impl crate::coordinator::Coordinator {
    /// 当前生效的注释模板：模式级 → 方案级 → 全局，见 [`resolve_template`]。
    ///
    /// 借用而非返回 String——它每次按键、每页候选前调一次，没必要为此分配。
    ///
    /// # ★ 方案层由调用方传入，而不是在这里取
    ///
    /// 方案级模板住在方案文件里，只能经 `EngineManager::active_behavior()` 拿到一个临时
    /// `Arc<SchemaBehavior>`——借用它的 `&str` 活不过本函数。调用方（`notify_ui_update`）
    /// 在候选循环外取一次存局部变量，模板借用它。
    ///
    /// ⛔ **不要改成把方案段快照进 `State`**（`[overlay]` 那份是那么做的）：快照要有失效点，
    /// 而 `schema_generation` **不随 `invalidate_schema` 递增**（设置页改 `schema_overrides`
    /// 不 bump 代际）⇒ 用户在设置页改完方案级模板，代际没变、快照不刷新，表现正是本仓
    /// 反复栽的「设置了不生效、重启后生效」。`behavior_cache` 则已在 `invalidate_schema`
    /// 里被清，每次现取才是对的。
    ///
    /// # ★ 方案层归属取 active，不取 `effective_data_schema`
    ///
    /// 注释模板是**呈现类**配置，与 `[candidate].layout` 同源取 active。临英/临拼/mix 的
    /// 注释需求**已经由模式层表达**，方案层再按 effective 解析一次就是两层说同一件事，
    /// 且两者可以互相矛盾。（注释**库**的过滤是数据类，另见 `ReverseLookup::comment_of`。）
    pub(crate) fn comment_template_for<'a>(
        &self,
        cfg: &'a Config,
        state: &'a State,
        behavior: &'a wind_config::SchemaBehavior,
        vertical: bool,
    ) -> &'a str {
        resolve_template(
            template_for(cfg, state.overlay_spec.as_ref(), state.active, vertical),
            schema_template_of(behavior, vertical),
            cfg.ui.candidate.comment_template(vertical),
        )
    }

    /// 渲染该候选的注释段。`vertical` 决定用哪份模板。
    ///
    /// `reverse` 由调用方在候选循环**外**取一次读锁传入——每条候选各取一次锁在满页 9 条
    /// × 每次按键的频率下是不必要的争用。
    /// `dict_schema` = 注释库白名单（`[[ui.comment_dicts]].schemas`）的求值作用域，
    /// 由调用方按 `effective_data_schema` 解析一次传入。
    pub(crate) fn comment_for(
        &self,
        c: &Candidate,
        tpl: &str,
        max_chars: usize,
        reverse: &wind_reverse::ReverseLookup,
        pinyin_hint: bool,
        dict_schema: &str,
    ) -> String {
        render(tpl, max_chars, |name, arg| {
            self.eval_var(name, arg, c, reverse, pinyin_hint, dict_schema)
        })
    }

    /// **任意文本**的反查渲染 —— cmdbar `dict.rev(text, n, format=…)` 的宿主侧实现。
    ///
    /// 与 [`Self::comment_for`] 共用模板渲染器与**变量词汇表**：用户在候选注释里学会的
    /// `${code}` / `${pinyin}` / `${chaizi}` 在这里同名同义，学一次。两处的变量求值
    /// （[`Self::eval_var`] / [`Self::eval_text_var`]）刻意挨着放 —— 它们是同一套词汇的
    /// 两个入口，分开写必然漂移。
    ///
    /// 与注释段的差别只在**取值对象**：那边是「当前候选」（带 source / code / boundary
    /// 等身份），这边是「一段裸文本」（剪贴板来的，没有候选身份）。故候选专属的
    /// `${code_hint}` 在这里不存在，写了会被渲染层原样回显让人看见。
    ///
    /// `max_chars` 传 0（不截断）：这里的产物**就是上屏文本**，不是显示投影。
    /// 候选窗的显示截断另有 `ui.candidate.max_chars` 在下游负责，两者不是一回事。
    ///
    /// ⚠️ **调用方不得已持有 `self.reverse` 的读锁**：本方法自取一次读锁，而 std 的
    /// `RwLock` 在有写者排队时同线程重入读会死锁（读锁不可重入）。当前唯一长期持有该
    /// 读锁的是 `notify_ui_update` 的候选渲染循环，它不经过短语求值，故无嵌套；
    /// 新增持锁路径时须回到这里核对。
    pub(crate) fn reverse_render(&self, text: &str, tpl: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
        // ★ `${char}` 恒有值（它只是把待查文本原样回显），故不能让它算进「查到了」。
        // 否则 `render` 的「变量全空则整体消失」就永远不会触发：剪贴板里是个查不到的
        // 字符（英文、标点、生僻字）时，`${char}: ${code} ${pinyin}` 会渲染成孤零零的
        // `A:` 并当作一条正常候选出现 —— 而正确行为是这条候选压根不该存在。
        let found = std::cell::Cell::new(false);
        let out = render(tpl, 0, |name, arg| {
            let v = self.eval_text_var(name, arg, text, &reverse);
            if name != "char" && v.as_deref().is_some_and(|s| !s.is_empty()) {
                found.set(true);
            }
            v
        });
        if found.get() { out } else { String::new() }
    }

    /// 纯文本（无候选身份）的模板变量求值。`None` = 未知变量名。
    ///
    /// 变量语义与 [`Self::eval_var`] 逐项对齐，仅两点差异：
    /// - `char` —— 本入口独有：注释段不需要它（候选文本已在注释左边），而反查产物要自带被查的字；
    /// - `code` —— 恒取主码表反查，**不带**注释段那道 `pinyin_hint && source==Pinyin` 门控。
    ///   那道门控的理由是「码表方案下候选的码就是用户自己打的，反查是冗余」，而剪贴板文本
    ///   不是用户打出来的，反查正是这里的全部目的。
    fn eval_text_var(
        &self,
        name: &str,
        arg: Option<&str>,
        text: &str,
        reverse: &wind_reverse::ReverseLookup,
    ) -> Option<String> {
        // 判据是 `chars().count()` 而非 `len()`：扩展区汉字走代理对，按字节数会被当成词组。
        let single = text.chars().count() == 1;
        Some(match name {
            "char" => text.to_string(),
            // `?`：索引未就绪时**当作「本变量解析不出」**，而不是当作空码。
            // 于是 `reverse_render` 的 found 判据照常生效——整条反查候选这一次不出现，
            // 而不是出现一条编码栏空着的半成品。索引建好后下一次按键即恢复。
            "code" => self.engine_mgr.codetable_reverse_hint(text)?,
            // `code_all` —— 该字在码表里的**全部**码位，默认 `/` 连接（`我` → `q/trn/trnt`）。
            //
            // 与 `code` 的分工照搬同文件 `chaizi` / `chaizi_all` 的既有惯例：不带后缀取单个，
            // `_all` 取全部、且可用 `${code_all:分隔符}` 换连接符。
            //
            // ★ 反查默认用它而不是 `code`：`codetable_reverse_hint` 取的是 `codes.last()`，
            // 而 `codes_of` 按**码长升序**，故它给的恒是最长的全码。那对候选注释是对的
            // （注释是候选右侧的窄条，塞下三个码会把候选行撑爆），但对「这个字怎么打」
            // 恰恰是最没用的答案 —— 简码才是用户要的。
            "code_all" => {
                let sid = self.engine_mgr.code_source_schema();
                // `?` 的理由同上面的 `code`：区分「索引没就绪」与「查不到」。
                let codes = self.engine_mgr.word_codes_in(&sid, text)?;
                match arg {
                    // `word_codes_in` 固定用 `/` 连接，换分隔符只能在这里替。
                    Some(sep) if !codes.is_empty() => codes.replace('/', sep),
                    _ => codes,
                }
            }
            "pinyin" => {
                // 与 `pinyin_text` 的路径 B 同构：先让引擎按词推断音节（多音字消歧），
                // 推断不出再退回逐字最常用读音。这里没有路径 A —— 裸文本没有词条 code。
                let inferred = self.engine_mgr.word_pinyin_syllables(text);
                if inferred.is_empty() {
                    reverse.toned_pinyin_of(text, None, SYLLABLE_SEP)
                } else {
                    let syls: Vec<&str> = inferred.split(' ').filter(|s| !s.is_empty()).collect();
                    reverse.toned_pinyin_of(text, Some(&syls), SYLLABLE_SEP)
                }
            }
            "chaizi" if single => reverse.radicals_of(text, ""),
            "chaizi" => String::new(),
            "chaizi_code" if single => reverse.chaizi_code_of(text),
            "chaizi_code" => String::new(),
            "chaizi_all" => reverse.radicals_of(text, arg.unwrap_or(" ")),
            // 作用域取活跃方案：本入口（cmdbar `dict.rev`）是**低频**路径，就地取一次
            // 比给整条求值链加一个参数划算；且它没有候选身份，也就没有临英那种
            // 「数据归 english 桶」的语境可言。
            "dict" => reverse.comment_of(text, None, &self.engine_mgr.active_schema_id()),
            _ => return None,
        })
    }

    /// 变量求值。`None` = 未知变量名（渲染层据此原样回显 `${name}` 让用户看见拼写错误）。
    ///
    /// 可用变量：
    /// - `code_hint` —— 引擎产的编码提示：码表前缀候选的**剩余编码**（输入 `si` 时 `sikao`
    ///   标 `kao`），以及混输的来源标记 `拼`。取自 `Candidate::comment`，这是该字段的
    ///   **唯一**消费点（其语义已收窄为「引擎产的编码提示」，不再是最终显示结果）。
    /// - `code` —— 主码表**整词反查编码**。仅对拼音来源候选生效且受方案 `show_code_hint`
    ///   门控（临时拼音 / 快捷输入等反查模式强制开启）：码表方案下候选的码就是用户自己打的，
    ///   反查是冗余信息，故那里恒空。
    /// - `pinyin` —— 带声调注音，见 [`pinyin_text`]。
    /// - `chaizi` —— 拆字字根串，**仅单字候选**。拆字回答的是「这个**字**由哪些字根构成」，
    ///   本就是单字概念；词组的字根串是各字字根的机械拼接，用户不会按字根记词，却足以把
    ///   候选行推得极宽（View 引擎**不支持文本折行**，超宽从窗口右缘硬裁，而注释恰在最右）。
    /// - `chaizi_code` —— 该字在拆字库里记录的**编码**，仅单字候选。与 `chaizi` 正交，
    ///   拼在一起即悬停提示拆字段的同款信息（`亻尔 [wq]`），但格式由模板决定而非写死。
    /// - `chaizi_all[:分隔符]` —— 不限字数的逐字字根，默认空格连接；带参数可改，
    ///   如 `${chaizi_all:／}` → `亻尔／女子`。长度自负（配 `comment_max_chars` 或只用于竖排）。
    /// - `dict` —— 用户挂载的注释词库（`[[ui.comment_dicts]]`）里该词的注释。键是**词**，
    ///   一份「英汉释义」「emoji 名称」可跨全部方案复用；候选 `code` 作可选消歧。
    ///
    /// `arg` = `${name:arg}` 的冒号后部分（未 trim）。只有声明支持参数的变量会读它，
    /// 其余变量收到参数时**静默忽略**而非报未知——参数写错不该让整个变量退化成错误回显。
    fn eval_var(
        &self,
        name: &str,
        arg: Option<&str>,
        c: &Candidate,
        reverse: &wind_reverse::ReverseLookup,
        pinyin_hint: bool,
        dict_schema: &str,
    ) -> Option<String> {
        // 判据是 `chars().count()` 而非 `len()`：扩展区汉字走代理对，按字节数会被当成词组。
        let single = c.text.chars().count() == 1;
        Some(match name {
            "code_hint" => c.comment.clone(),
            "code" => {
                if pinyin_hint && c.source == CandidateSource::Pinyin {
                    // `?`：索引未就绪 ⇒ 本变量未解析（与 eval_text_var 同一语义）。
                    // 注释是装饰，这一次不显示即可，绝不为它卡住按键线程。
                    self.engine_mgr.codetable_reverse_hint(&c.text)?
                } else {
                    String::new()
                }
            }
            // `code_all` —— 全部码位（`我` → `q/trn/trnt`），`code` 只给最长的那个全码。
            //
            // 门控与 `code` **完全一致**（同为拼音来源候选才出）：理由也一样 ——
            // 码表方案下候选的码就是用户自己打的，反查是冗余信息。
            //
            // 与 [`Self::eval_text_var`] 的同名变量同义，两处必须一起改：用户在注释模板里
            // 学会的写法要能原样用在 `dict.rev(format=…)` 里，反之亦然。
            //
            // 出厂注释模板不含它 —— 三个码位会把候选行推得很宽，横排尤甚。它是给愿意
            // 用竖排、想一眼看全简码的用户的选项，不是默认。
            "code_all" => {
                if pinyin_hint && c.source == CandidateSource::Pinyin {
                    let sid = self.engine_mgr.code_source_schema();
                    let codes = self.engine_mgr.word_codes_in(&sid, &c.text)?;
                    match arg {
                        Some(sep) if !codes.is_empty() => codes.replace('/', sep),
                        _ => codes,
                    }
                } else {
                    String::new()
                }
            }
            "pinyin" => pinyin_text(
                c,
                |t| self.engine_mgr.word_pinyin_syllables(t),
                |t, syls| reverse.toned_pinyin_of(t, syls, SYLLABLE_SEP),
            ),
            "chaizi" if single => reverse.radicals_of(&c.text, ""),
            "chaizi" => String::new(),
            "chaizi_code" if single => reverse.chaizi_code_of(&c.text),
            "chaizi_code" => String::new(),
            "chaizi_all" => reverse.radicals_of(&c.text, arg.unwrap_or(" ")),
            // 用户挂载的注释词库（`[[ui.comment_dicts]]`）。键是**词**，故一份库可跨方案复用；
            // 候选自身的 `code` 作可选消歧（注释库声明了 code 列时才生效，跨方案对不上则
            // 回落该词首条，见 `ReverseLookup::comment_of`）。
            // 注释库的 `schemas` 白名单在**查询时**求值，作用域取 `dict_schema`
            // （由调用方按 `effective_data_schema` 解析）：注释库是**数据类**资源，
            // 归属与词频/短语同源——临英下要按 `english` 查，而不是主方案。
            // 这与注释**模板**取 active（呈现类）刻意不同，见 `comment_template_for`。
            "dict" => reverse.comment_of(&c.text, Some(&c.code), dict_schema),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod mode_template_tests {
    use super::*;
    use wind_config::config::MixModeConfig;

    /// 全局两份模板设成可辨认的值，模式级一律留空（跟随）。
    fn base_cfg() -> Config {
        let mut c = Config::default();
        c.ui.candidate.comment_template_vertical = "全局竖".into();
        c.ui.candidate.comment_template_horizontal = "全局横".into();
        c
    }

    /// 未配模式级覆盖时，各模式都取全局同方向那份——含「无模式」。
    #[test]
    fn absent_override_follows_global() {
        let c = base_cfg();
        for active in [
            None,
            Some(ModeKind::TempEnglish),
            Some(ModeKind::TempPinyin),
            Some(ModeKind::Url),
        ] {
            assert_eq!(template_for(&c, None, active, true), None, "{active:?} 竖");
            assert_eq!(template_for(&c, None, active, false), None, "{active:?} 横");
        }
    }

    /// ★★ 三态的第三态：空串 = 本模式不显示注释，**不等于**跟随全局。
    ///
    /// 这是 `Option<String>` 相对 `String` 的全部增量——用空串表达「跟随」的话，
    /// 「本模式不要注释」就没法表达了，而那正是本功能最主要的用途。
    #[test]
    fn empty_override_means_no_comment_not_follow() {
        let mut c = base_cfg();
        c.input.temp_pinyin.comment_template_vertical = Some(String::new());
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempPinyin), true),
            Some(""),
            "空串必须原样返回（= 本模式不显示），不能退化成 None（= 跟随全局）"
        );
        // 同模式的另一方向未配 → 仍跟随
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempPinyin), false),
            None
        );
    }

    /// ★ 横竖两个方向各自独立三态：只覆盖竖排时，横排仍跟随全局。
    #[test]
    fn directions_are_independent() {
        let mut c = base_cfg();
        c.input.temp_english.comment_template_vertical = Some("${dict}".into());
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempEnglish), true),
            Some("${dict}")
        );
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempEnglish), false),
            None,
            "只配了竖排，横排必须仍跟随全局"
        );
    }

    /// 覆盖只作用于本模式，别的模式与「无模式」不受影响。
    #[test]
    fn override_is_scoped_to_its_mode() {
        let mut c = base_cfg();
        c.input.temp_english.comment_template_vertical = Some("${dict}".into());
        assert_eq!(template_for(&c, None, None, true), None, "无模式不受影响");
        assert_eq!(
            template_for(&c, None, Some(ModeKind::TempPinyin), true),
            None,
            "别的模式不受影响"
        );
    }

    /// mix 实例按下标各自独立（其配置仍在 `Config` 里）。
    #[test]
    fn instance_modes_are_per_index() {
        let mut c = base_cfg();
        c.schema.mix_modes = vec![
            MixModeConfig {
                comment_template_vertical: Some("快捷用".into()),
                ..Default::default()
            },
            MixModeConfig::default(),
        ];
        assert_eq!(
            template_for(&c, None, Some(ModeKind::Mix(0)), true),
            Some("快捷用")
        );
        assert_eq!(template_for(&c, None, Some(ModeKind::Mix(1)), true), None);
    }

    /// 特殊模式的模板来自**方案文件的 `[overlay]` 段**（经 `State::overlay_spec` 快照传入），
    /// 不再来自 `Config`。下标只用于判「是不是 Special」，取值一律走 overlay 参数。
    #[test]
    fn special_mode_template_comes_from_overlay_spec() {
        let c = base_cfg();
        let ov = OverlaySpec {
            comment_template_horizontal: Some("快符用".into()),
            ..Default::default()
        };
        assert_eq!(
            template_for(&c, Some(&ov), Some(ModeKind::Special(0)), false),
            Some("快符用")
        );
        assert_eq!(
            template_for(&c, Some(&ov), Some(ModeKind::Special(0)), true),
            None,
            "只配了横排，竖排仍跟随全局"
        );
        // 下标不参与取值：换个下标、同一份快照，结果不变。
        assert_eq!(
            template_for(&c, Some(&ov), Some(ModeKind::Special(9)), false),
            Some("快符用")
        );
    }

    /// 没有快照（未进入特殊模式 / 该方案无 `[overlay]` 段）回落跟随全局，不 panic。
    /// mix 侧的下标越界（热重载删掉了该实例）同样回落。
    #[test]
    fn out_of_range_index_falls_back_to_follow() {
        let c = base_cfg();
        assert_eq!(template_for(&c, None, Some(ModeKind::Mix(7)), true), None);
        assert_eq!(
            template_for(&c, None, Some(ModeKind::Special(7)), true),
            None,
            "快照为 None 时不该 panic，跟随全局"
        );
    }

    /// ★ 方案层**不得**影响 `template_for` 本身——它只回答「模式怎么想」。
    ///
    /// 与 `layout::intent_for_answers_mode_layer_only` 同一条：分层的意义在于每层各答各的。
    /// 若图省事把方案意图折进 `template_for`，「模式没意见」与「模式没意见但方案有意见」
    /// 就再也分不开，将来想在两者之间插一层（如运行时手动值）便无处可插。
    #[test]
    fn template_for_answers_mode_layer_only() {
        let c = base_cfg();
        // 参数表里压根没有方案层，签名本身就是这条约束的载体；这里钉住「无模式 = None」，
        // 使「顺手在 None 分支里读方案」的改动当场变红。
        assert_eq!(template_for(&c, None, None, true), None);
        assert_eq!(template_for(&c, None, None, false), None);
    }
}

#[cfg(test)]
mod schema_layer_tests {
    //! 三层裁决（模式 → 方案 → 全局）的纯函数矩阵。端到端接线另见
    //! `coordinator::mode_comment_e2e_tests`。
    use super::*;

    fn behavior(v: Option<&str>, h: Option<&str>) -> wind_config::SchemaBehavior {
        wind_config::SchemaBehavior {
            comment_template_vertical: v.map(str::to_string),
            comment_template_horizontal: h.map(str::to_string),
            ..Default::default()
        }
    }

    /// ★★★ 唯一能区分三层与两层实现的格子。
    #[test]
    fn schema_layer_wins_when_mode_absent() {
        assert_eq!(resolve_template(None, Some("方案"), "全局"), "方案");
    }

    #[test]
    fn mode_layer_wins_over_schema() {
        assert_eq!(resolve_template(Some("模式"), Some("方案"), "全局"), "模式");
    }

    #[test]
    fn global_applies_only_when_no_layer_has_opinion() {
        assert_eq!(resolve_template(None, None, "全局"), "全局");
    }

    /// ★★ 空串在**每一层**都是「有意见」，压过下面所有层。
    ///
    /// 写成「非空才算数」的话，「本方案/本模式不要注释」就再也无法表达——
    /// 而那正是这套三态最主要的用途。
    #[test]
    fn empty_string_is_an_opinion_at_every_layer() {
        assert_eq!(resolve_template(None, Some(""), "全局"), "");
        assert_eq!(resolve_template(Some(""), Some("方案"), "全局"), "");
    }

    /// 横竖各自独立三态：只覆盖竖排、横排跟随，是合法且常见的配置。
    #[test]
    fn schema_directions_are_independent() {
        let b = behavior(Some("方案竖"), None);
        assert_eq!(schema_template_of(&b, true), Some("方案竖"));
        assert_eq!(schema_template_of(&b, false), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 求值闭包：按名取值；名字不在表里即「未知变量」。参数一律忽略。
    fn ev<'a>(
        pairs: &'a [(&'a str, &'a str)],
    ) -> impl Fn(&str, Option<&str>) -> Option<String> + 'a {
        move |n, _arg| {
            pairs
                .iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.to_string())
        }
    }

    /// 求值闭包：把收到的「名字 + 参数」原样回显，供参数语法的用例断言解析结果。
    fn echo_arg() -> impl Fn(&str, Option<&str>) -> Option<String> {
        |n, arg| Some(format!("{n}<{}>", arg.unwrap_or("∅")))
    }

    // ---------------- 出厂零回归 ----------------

    /// ★★ 出厂模板 `${code_hint|code}` 必须与本功能引入前的硬编码行为逐字节一致：
    /// 引擎产的剩余编码优先，为空则回退到主码表反查码，两者皆空则不显示。
    ///
    /// 这是整个改动的零回归闸门 —— 存量用户升级后不该看到任何变化。
    #[test]
    fn default_template_reproduces_legacy_behavior() {
        const T: &str = "${code_hint|code}";
        // ① 引擎给了剩余码 → 用它（此时反查码即便存在也不参与，旧逻辑正是 if/else if）
        assert_eq!(
            render(T, 0, ev(&[("code_hint", "kao"), ("code", "wq")])),
            "kao"
        );
        // ② 引擎没给 → 回退反查码
        assert_eq!(render(T, 0, ev(&[("code_hint", ""), ("code", "wq")])), "wq");
        // ③ 都没有 → 不显示
        assert_eq!(render(T, 0, ev(&[("code_hint", ""), ("code", "")])), "");
    }

    // ---------------- 语法 ----------------

    #[test]
    fn plain_variable_and_literal_text() {
        assert_eq!(
            render("${pinyin}", 0, ev(&[("pinyin", "nǐ hǎo")])),
            "nǐ hǎo"
        );
        assert_eq!(
            render("拼:${pinyin}", 0, ev(&[("pinyin", "nǐ hǎo")])),
            "拼:nǐ hǎo"
        );
    }

    /// ★ 整个模板是隐式可选段：变量全空时**连字面文本一起**不显示，
    /// 否则会剩下一个孤零零的 `拼:`。
    #[test]
    fn all_vars_empty_hides_literal_text_too() {
        assert_eq!(render("拼:${pinyin}", 0, ev(&[("pinyin", "")])), "");
        assert_eq!(render("(${a} ${b})", 0, ev(&[("a", ""), ("b", "")])), "");
    }

    /// 无变量的纯文本模板同样不显示 —— 没有任何变量被填上，按隐式段规则整体消失。
    /// （想固定显示一段文字不是注释段的用途，那属于主题。）
    #[test]
    fn literal_only_template_shows_nothing() {
        assert_eq!(render("拼音", 0, ev(&[])), "");
    }

    #[test]
    fn fallback_takes_first_non_empty() {
        let e = ev(&[("a", ""), ("b", ""), ("c", "C")]);
        assert_eq!(render("${a|b|c}", 0, &e), "C");
        assert_eq!(render("${a|b}", 0, &e), "");
    }

    /// ★★ 可选段：段内变量全空则**整段消失**，含段内的装饰字符。
    ///
    /// 这是 `{}` 存在的全部理由 —— 没有它，`${code}{ (${pinyin})}` 在拼音为空时会显示
    /// `wq ()`，而空括号要配对解析才删得掉，靠 trim / 折叠空白救不了。
    #[test]
    fn optional_group_vanishes_when_all_its_vars_empty() {
        const T: &str = "${code}{ (${pinyin})}";
        assert_eq!(
            render(T, 0, ev(&[("code", "wq"), ("pinyin", "nǐ hǎo")])),
            "wq (nǐ hǎo)"
        );
        assert_eq!(
            render(T, 0, ev(&[("code", "wq"), ("pinyin", "")])),
            "wq",
            "拼音为空时整段消失，不得留下 `()`"
        );
    }

    /// 段内只要有**一个**变量非空，整段保留（空的那个按空串替换）。
    #[test]
    fn group_survives_if_any_var_filled() {
        const T: &str = "{(拼: ${pinyin} ${chaizi})}";
        assert_eq!(
            render(T, 0, ev(&[("pinyin", "nǐ hǎo"), ("chaizi", "女子")])),
            "(拼: nǐ hǎo 女子)"
        );
        assert_eq!(
            render(T, 0, ev(&[("pinyin", "nǐ hǎo"), ("chaizi", "")])),
            "(拼: nǐ hǎo)",
            "空变量须吞掉紧邻空白，否则是 `(拼: nǐ hǎo )`"
        );
        assert_eq!(render(T, 0, ev(&[("pinyin", ""), ("chaizi", "")])), "");
    }

    /// 空变量只吞**一个**紧邻空白——吞到底会把用户有意排的版式抹平。
    #[test]
    fn empty_var_eats_exactly_one_space() {
        assert_eq!(
            render("${a}   ${b}", 0, ev(&[("a", "A"), ("b", "")])),
            "A",
            "尾部空白由 trim 收拾"
        );
        assert_eq!(
            render("${a}   ${b}!", 0, ev(&[("a", "A"), ("b", "")])),
            "A  !",
            "只吞一个，其余留给用户的版式"
        );
    }

    /// ★ 段内变量的 `}` 不得被误当作段结束符——扫描段边界时必须跳过 `${…}`。
    /// 写错会让 `{(${pinyin})}` 解析成段 `(${pinyin`，后面 `)}` 变字面文本。
    #[test]
    fn group_scan_skips_variable_braces() {
        assert_eq!(
            render("{[${a}]}", 0, ev(&[("a", "X")])),
            "[X]",
            "段内变量的右花括号不是段结束"
        );
        assert_eq!(render("{[${a}]}", 0, ev(&[("a", "")])), "");
    }

    // ---------------- 变量参数 `${name:arg}` ----------------

    /// ★★ 参数**不 trim**：`${chaizi_all: · }` 里那两个空格正是用户要的分隔符。
    ///
    /// 名字仍 trim（`${ pinyin }` 这种手滑要认）。两者规则不同是有意的 ——
    /// 名字的空白一定是手滑，参数的空白一定是内容。
    #[test]
    fn variable_arg_preserves_whitespace_but_name_is_trimmed() {
        assert_eq!(
            render("${chaizi_all: · }", 0, echo_arg()),
            "chaizi_all< · >"
        );
        assert_eq!(render("${ pinyin }", 0, echo_arg()), "pinyin<∅>");
    }

    /// 只切**第一个**冒号——分隔符本身可以含冒号。
    #[test]
    fn variable_arg_splits_on_first_colon_only() {
        assert_eq!(render("${x:a:b}", 0, echo_arg()), "x<a:b>");
    }

    /// 参数可为空串（`${x:}`）：与「无参数」区分开，前者是「显式要求不加分隔」。
    #[test]
    fn empty_arg_differs_from_absent_arg() {
        assert_eq!(render("${x:}", 0, echo_arg()), "x<>");
        assert_eq!(render("${x}", 0, echo_arg()), "x<∅>");
    }

    /// 参数与回退链共存：每一段各自解析自己的参数。
    #[test]
    fn arg_works_inside_fallback_chain() {
        let e = |n: &str, arg: Option<&str>| match n {
            "a" => Some(String::new()), // 已知但空 → 回退
            "b" => Some(format!("B<{}>", arg.unwrap_or("∅"))),
            _ => None,
        };
        assert_eq!(render("${a:x|b:y}", 0, e), "B<y>");
    }

    /// ★ 未知变量名**原样回显**并计作已填充 —— 拼错一定看得见。
    ///
    /// 若把未知当空处理，用户得到的是「配了没反应」，得去翻文档才知道是拼写错误。
    #[test]
    fn unknown_variable_is_echoed_verbatim() {
        assert_eq!(
            render("${pinyn}", 0, ev(&[("pinyin", "nǐ hǎo")])),
            "${pinyn}"
        );
        // 回退链里遇到未知名即停（它是错误提示，不是"空值"，不该被跳过）。
        assert_eq!(
            render("${nope|pinyin}", 0, ev(&[("pinyin", "X")])),
            "${nope}"
        );
    }

    /// 语法写坏不 panic、不吞内容：未闭合的 `${` / `{` 退化成字面文本。
    /// 但字面文本里没有变量被填上 ⇒ 按隐式段规则整体不显示。
    #[test]
    fn malformed_template_degrades_to_literal_text() {
        assert_eq!(render("${unclosed", 0, ev(&[])), "");
        assert_eq!(render("{unclosed", 0, ev(&[])), "");
        // 与真变量混排时，字面部分原样保留。
        assert_eq!(render("${a} ${bad", 0, ev(&[("a", "A")])), "A ${bad");
    }

    /// 中文字面文本按字符推进，不得在 UTF-8 中间切开（切开会 panic）。
    #[test]
    fn multibyte_literal_text_is_safe() {
        assert_eq!(
            render("【读音】${pinyin}（完）", 0, ev(&[("pinyin", "nǐ")])),
            "【读音】nǐ（完）"
        );
    }

    // ---------------- 截断 ----------------

    #[test]
    fn max_chars_truncates_with_ellipsis() {
        let e = ev(&[("a", "zhōng guó rén")]);
        assert_eq!(render("${a}", 0, &e), "zhōng guó rén", "0 = 不限");
        assert_eq!(render("${a}", 5, &e), "zhōng…");
        // 按字符而非字节计——带声调字母是多字节，按字节会截在半个字符上。
        assert_eq!(render("${a}", 100, &e), "zhōng guó rén");
    }

    // ---------------- 变量求值：拼音 / 拆字 ----------------

    /// 边界真值切分：`你好` 的 `nihao` + `0b101`（音节起于字节 0 和 2）→ `["ni","hao"]`。
    #[test]
    fn boundary_splits_syllables() {
        assert_eq!(syllables_of("nihao", 0b101), vec!["ni", "hao"]);
        // 单音节的 boundary 是 0b1（「整串是一个音节」是真信息）。
        assert_eq!(syllables_of("hao", 0b1), vec!["hao"]);
        assert_eq!(
            syllables_of("zhongguoren", 1 | 1 << 5 | 1 << 8),
            vec!["zhong", "guo", "ren"]
        );
    }

    /// ★ `boundary == 0` 的语义是「**无边界信息**」而非「单音节」：不得把 `code` 整串当一个
    /// 音节，而要**降级到推断路径**（此前是降级到逐字首音，本轮改为推断）。
    ///
    /// 若把 0 当成「整串一个音节」，五笔码 `wqvb` 会被当作一个拼音音节送去筛读音表。
    #[test]
    fn zero_boundary_falls_through_to_inference() {
        let c = Candidate {
            text: "你好".into(),
            code: "nihao".into(),
            boundary: 0, // ← 要害
            source: CandidateSource::Pinyin,
            ..Default::default()
        };
        assert_eq!(
            pinyin_text(
                &c,
                |t| {
                    assert_eq!(t, "你好");
                    "ni hao".to_string()
                },
                |_, syls| {
                    assert_eq!(
                        syls,
                        Some(&["ni", "hao"][..]),
                        "boundary=0 应走推断，而非把 code 整串当一个音节"
                    );
                    "nǐ hǎo".to_string()
                }
            ),
            "nǐ hǎo"
        );
    }

    /// ★★ 非拼音来源（五笔候选）走**引擎按词推断**，而不是直接退到逐字首音。
    ///
    /// 这是本轮修的核心 bug：`boundary` 在码表方案下恒为 0、`code` 是形码，此前该路径直接
    /// 落到「逐字最常用读音」，于是「行长」显示成 `xíng cháng`（两个字都错）——而五笔用户
    /// 正是注音功能最主要的受众。断言落在**有没有把推断结果当音节传下去**上。
    ///
    /// 顺带钉住：即便 `boundary` 恰好有值也不得当拼音边界用（那是别的编码域的字段值）。
    #[test]
    fn non_pinyin_candidate_infers_syllables_instead_of_first_reading() {
        let c = Candidate {
            text: "行长".into(),
            code: "tfta".into(), // 五笔码
            boundary: 0b101,     // ← 有值，但不属于拼音域
            source: CandidateSource::CodeTable,
            ..Default::default()
        };
        assert_eq!(
            pinyin_text(
                &c,
                |t| {
                    assert_eq!(t, "行长", "推断应按候选文本而非编码");
                    "hang zhang".to_string()
                },
                |t, syls| {
                    assert_eq!(t, "行长");
                    assert_eq!(
                        syls,
                        Some(&["hang", "zhang"][..]),
                        "推断出的音节必须传下去消歧，否则「行长」会显示成 xíng cháng"
                    );
                    "háng zhǎng".to_string()
                }
            ),
            "háng zhǎng"
        );
    }

    /// 推断失败（含非汉字、生僻多音字超组合数护栏）→ 交由查表层逐字取最常用读音。
    /// 这是最后的兜底，不是常态路径。
    #[test]
    fn failed_inference_falls_back_to_per_char_readings() {
        let c = Candidate {
            text: "你好".into(),
            source: CandidateSource::CodeTable,
            ..Default::default()
        };
        assert_eq!(
            pinyin_text(
                &c,
                |_| String::new(), // 推断失败
                |_, syls| {
                    assert!(syls.is_none(), "推断失败时不得传入空音节序列");
                    "nǐ hǎo".to_string()
                }
            ),
            "nǐ hǎo"
        );
    }

    /// ★ 拼音来源候选**优先用词条真值，不走推断**。
    ///
    /// 词条自带的 `code`+`boundary` 比枚举笛卡尔积回查词典更可靠也更省。
    /// 闭包里 panic 是断言手段：一旦实现改成无条件先推断，会以「不该被调用」失败。
    #[test]
    fn pinyin_candidate_prefers_entry_truth_over_inference() {
        let c = Candidate {
            text: "行长".into(),
            code: "hangzhang".into(),
            boundary: 1 | 1 << 4, // hang|zhang
            source: CandidateSource::Pinyin,
            ..Default::default()
        };
        assert_eq!(
            pinyin_text(
                &c,
                |_| unreachable!("拼音来源候选不得走推断"),
                |_, syls| {
                    assert_eq!(
                        syls,
                        Some(&["hang", "zhang"][..]),
                        "词条音节须原样传下去消歧"
                    );
                    "háng zhǎng".to_string()
                }
            ),
            "háng zhǎng"
        );
    }
}
