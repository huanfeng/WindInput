//! 应用兼容性规则
//!
//! 按进程名为特定应用提供候选窗定位 / 光标获取等兼容修正。文件格式为 TOML 的 `[[apps]]`
//! 数组表，加载顺序：
//! 系统预置（`{data_dir}/compat.toml`）→ 定制版（`data_custom/compat.toml`）→
//! 用户覆盖（`{user_config_dir}/compat.toml`）。
//!
//! 每一层都是相对下一层的差异，逐字段叠加（写了的覆盖、没写的继承、`unset` 取消、`disabled`
//! 禁用），没有哪个字段有特殊待遇。叠加引擎见 [`crate::compat_overlay`]，本模块把叠加结果变成
//! 运行时结构体。`[[apps]]` 可带窗口条件，多条命中时按具体程度分级合成（[`AppCompat::resolve`]，
//! 见 `docs/design/compat-window-match.md`）。宿主行为背景见
//! `docs/architecture/app-compat-host-notes.md`。

pub use crate::compat_overlay::{ANY_PROCESS, RuleId, RuleKey, wildcard_match};
use crate::compat_overlay::{
    FieldEdit, Obj, Raw, apply_edits, compose, is_disabled, materialize, overlay_raw, parse_raw,
    render_raw, rule_id, sanitize, unset_of, wildcard_match_folded,
};
use crate::config::SmartMethod;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

/// 默认兼容规则文件名。
pub const COMPAT_FILE_NAME: &str = "compat.toml";

/// 写回用户层 compat.toml 时的固定文件头。
///
/// 用户层每次改动都整份重写（不保留注释），须在文件里讲明。字段文档在系统层 `data/compat.toml`。
pub(crate) const USER_COMPAT_HEADER: &str = "\
# 用户层应用兼容规则（相对系统层 data/compat.toml 的差异）
#
# ⚠ 本文件由输入法（右键菜单 / 设置页）自动管理，每次改动都会整份重写，
#   手写的注释与排版不会保留。需要长期留存的说明请写在系统层 compat.toml。
#
# 叠加语义：每条规则只记与系统层不同的字段，逐字段叠加在同一条系统规则之上：
#   - 写了的字段覆盖系统值，没写的字段继承系统值；
#   - unset = [\"字段名\", …] 取消系统设定的字段（回到「跟随全局」）；
#   - 显式写 false 可以关掉系统打开的开关；
#   - disabled = true 禁用整条系统规则。
# 「同一条」= process（不写即 \"*\"，所有进程）+ class（顶层窗口类名）+ title（顶层窗口标题）
#   三者都相同；class / title 支持 * ? 通配、不区分大小写。
# 一个窗口命中多条时按具体程度叠加，越具体越后叠（后叠的赢）：
#   \"*\" < 仅窗口条件 < 仅进程名 < 进程名 + 窗口条件。
#   更具体那条里的 unset 也取消 \"*\" 等更低级规则设下的同名字段（回到「跟随全局」）。
# 设置页「应用兼容性」可新增、修改带 class / title 的规则。title 在焦点切换时判定，
#   标题变化不重算初始中英态；只对可能命中 title 规则的进程采集窗口标题，日志不记标题原文。
# 字段说明见系统层 data/compat.toml 顶部注释。

";

/// `skip_serializing_if` 用：省略默认为 false 的开关，避免写回时铺满一堆 `= false`。
fn is_false(b: &bool) -> bool {
    !*b
}

/// 用户层 `compat.toml` 的进程内写锁。
///
/// 写用户层是「读整份 → 改 → 整份写回」，两个写入方（右键菜单在协调器线程、设置端 RPC 在
/// 各自的连接线程）交错时，后写的会把先写的改动整份覆盖掉。所有写入路径
/// （[`update_user_raw`]、`compat_admin::rpc` 的写方法）必须在读到写完的**全程**持有它。
static USER_COMPAT_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn lock_user_compat() -> std::sync::MutexGuard<'static, ()> {
    // 持锁线程 panic 只会让锁中毒，数据本身在磁盘上，不该让之后所有写入都失败。
    USER_COMPAT_WRITE.lock().unwrap_or_else(|e| e.into_inner())
}

/// 落盘：唯一临时名 → 写满 → `fsync` → rename。
///
/// 不 `fsync` 的话断电后可能留下半截文件，而语法错的 compat.toml 会整份跳过、下次菜单操作
/// 按空集重写，等于用户层全丢。临时名带 pid 与序号，没经过 [`lock_user_compat`] 也不撞名。
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| COMPAT_FILE_NAME.to_string());
    let tmp = path.with_file_name(format!(
        "{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// 候选窗首显策略：新组合的候选窗**何时**显示。
///
/// 宿主插入组合内容后要 reflow 才给得出正确坐标，三档是「快」与「准」之间的取舍。
/// 依据见 `docs/redesign/candidate-window-positioning.md` 第 6 层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstShowMode {
    /// 等宿主 reflow 后的权威坐标才显示。最准，代价是可感知的首显延迟。
    Wait,
    /// 默认档。等到坐标「可信」即放行：采用首条与上一轮权威坐标不同的试探坐标，连打时直接采信
    /// 首条。坐标不可信时（首帧信任门 `caret_cache_verified`）自动退回等真值。
    #[default]
    Fast,
    /// 完全不等，首帧直接沿用上一次的坐标。最快，但只要光标位置变动过
    /// （手动移动、换行、文本重排）那个位置就是错的，会先错位显示再跳回。
    Instant,
}

impl FirstShowMode {
    /// 配置串 → 枚举。无法识别返回 `None`。
    ///
    /// 回落默认档只发生在全局层（`Self::from_config(s).unwrap_or_default()`）；per-app 层认不出的
    /// 值退化为「没配」＝跟随全局，不把拼错的值固化成显式覆盖。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "wait" => Some(Self::Wait),
            "fast" => Some(Self::Fast),
            "instant" => Some(Self::Instant),
            _ => None,
        }
    }
    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Fast => "fast",
            Self::Instant => "instant",
        }
    }
}

/// 上屏文本里的换行用什么字符表达。
///
/// 富文本宿主（Word 等）里只有 CR 构成换行，字节即存储的宿主里任何转换都是改写用户数据，
/// 没有全局正确值，故按应用配置（见 [`CommitNewlineRule`]）。出厂 [`Keep`](Self::Keep)：
/// 漏配的代价是看得见的显示异常，好过静默改写行尾。见
/// `docs/architecture/app-compat-host-notes.md` 第一节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NewlineStyle {
    /// 原样透传，一个字符都不改。出厂默认。
    #[default]
    Keep,
    /// 一律折成单个 CR（`\r`）——Windows 富文本模型的段落分隔符。
    Cr,
    /// 一律折成单个 LF（`\n`）。
    Lf,
    /// 一律折成 CRLF（`\r\n`）。
    Crlf,
}

impl NewlineStyle {
    /// 按本档位改写 `text` 里的换行。
    ///
    /// 三种行尾都认（`\r\n` / 孤立 `\n` / 孤立 `\r`），`\r\n` 算一个换行。
    ///
    /// [`Keep`](Self::Keep) 与「文本里压根没有换行」两种情况零拷贝返回。
    pub fn apply<'a>(self, text: &'a str) -> Cow<'a, str> {
        let eol = match self {
            Self::Keep => return Cow::Borrowed(text),
            Self::Cr => "\r",
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        };
        if !text.contains(['\r', '\n']) {
            return Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len() + 8);
        let mut it = text.chars().peekable();
        while let Some(ch) = it.next() {
            match ch {
                '\r' => {
                    // CRLF：吃掉紧随的 LF，整体只产出一个换行。
                    if it.peek() == Some(&'\n') {
                        it.next();
                    }
                    out.push_str(eol);
                }
                '\n' => out.push_str(eol),
                c => out.push(c),
            }
        }
        Cow::Owned(out)
    }

    /// 配置串 → 枚举。无法识别返回 `None`——同 [`FirstShowMode::from_config`] 的理由：
    /// per-app 层认不出的值退化为「没配」＝跟随全局，不把拼错的值固化成显式覆盖。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "keep" => Some(Self::Keep),
            "cr" => Some(Self::Cr),
            "lf" => Some(Self::Lf),
            "crlf" => Some(Self::Crlf),
            _ => None,
        }
    }

    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Cr => "cr",
            Self::Lf => "lf",
            Self::Crlf => "crlf",
        }
    }
}

/// 应用独立的初始中英状态取值。
///
/// 语义是**初始值而非锁定**：进入该应用时套用，用户随后可自由手动切换，
/// 停留在该应用期间不再被改写（详见 `Coordinator::initial_chinese_mode_for`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitialMode {
    English,
    Chinese,
}

impl InitialMode {
    /// 配置串 → 枚举。无法识别返回 `None`（＝不干预），不 panic 也不回落到某一档：
    /// 「用户拼错了」与「用户想要英文」是两回事，后者必须是显式写对才成立。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "english" | "en" => Some(Self::English),
            "chinese" | "zh" => Some(Self::Chinese),
            _ => None,
        }
    }
    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::English => "english",
            Self::Chinese => "chinese",
        }
    }
    /// 落到 `chinese_mode` / `chinese_punct` 这类布尔状态。
    pub fn is_chinese(self) -> bool {
        matches!(self, Self::Chinese)
    }
    /// 布尔 → 枚举（菜单写盘时把当前状态反写成规则用）。
    pub fn from_chinese(chinese: bool) -> Self {
        if chinese {
            Self::Chinese
        } else {
            Self::English
        }
    }
}

/// 宿主组合区里的占位字符。
///
/// 有会话却没有可见编码时组合区要挂一个占位（TSF 不接受空组合）。空格会被浏览器受控输入框
/// `trim()` 掉而终止组合，零宽字符又让靠「出现了内容」进编辑态的宿主失灵，故按应用配置，
/// 各档适用宿主见 `docs/architecture/app-compat-host-notes.md` 第二节。
///
/// 消费：协调器按焦点应用解析后直接发出（[`Self::as_str`]）；DLL 的兜底占位经
/// `CONFIG_KEY_COMPOSITION_PLACEHOLDER` 按客户端 pid 下发。DLL 三种字符都认作占位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceholderChar {
    /// U+0020。不写 = 这一档。
    #[default]
    Space,
    /// U+200B ZERO WIDTH SPACE。
    Zwsp,
    /// U+2800 BRAILLE PATTERN BLANK：非空白（`trim()` / `\s` 不删）、有宽度。
    Blank,
}

impl PlaceholderChar {
    /// 配置串 → 枚举。无法识别返回 `None`（＝未配置，回落空格）。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "space" => Some(Self::Space),
            "zwsp" => Some(Self::Zwsp),
            "blank" => Some(Self::Blank),
            _ => None,
        }
    }
    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Space => "space",
            Self::Zwsp => "zwsp",
            Self::Blank => "blank",
        }
    }
    /// 实际写进宿主组合区的字符串（恒为一个字符）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Space => " ",
            Self::Zwsp => "\u{200B}",
            Self::Blank => "\u{2800}",
        }
    }
}

/// 联想态的占位：在 [`PlaceholderChar`] 三档之外多一档 `none`（不挂占位组合）。
///
/// 联想态没有编码，占位组合只是为了让宿主继续把按键转发给输入法。`none` 时宿主里什么都不放，
/// 转发资格改由上屏应答里的 `COMMIT_FLAG_ASSOC_SESSION` 同步告知 DLL（见 `KeyEventSink`）。
/// 代价：没有组合就没有 range，候选窗位置沿用上屏前最后一次的光标坐标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssocPlaceholder {
    Space,
    Zwsp,
    Blank,
    /// 不挂占位组合。
    None,
}

impl AssocPlaceholder {
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            other => PlaceholderChar::from_config(other).map(Self::from),
        }
    }
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Space => "space",
            Self::Zwsp => "zwsp",
            Self::Blank => "blank",
            Self::None => "none",
        }
    }
    /// 写进宿主组合区的字符串；`none` 为空串（调用方据此不开组合）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Space => PlaceholderChar::Space.as_str(),
            Self::Zwsp => PlaceholderChar::Zwsp.as_str(),
            Self::Blank => PlaceholderChar::Blank.as_str(),
            Self::None => "",
        }
    }
}

impl From<PlaceholderChar> for AssocPlaceholder {
    fn from(p: PlaceholderChar) -> Self {
        match p {
            PlaceholderChar::Space => Self::Space,
            PlaceholderChar::Zwsp => Self::Zwsp,
            PlaceholderChar::Blank => Self::Blank,
        }
    }
}

impl Default for AssocPlaceholder {
    fn default() -> Self {
        PlaceholderChar::default().into()
    }
}

fn de_assoc_placeholder<'de, D>(d: D) -> Result<Option<AssocPlaceholder>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "assoc_placeholder", AssocPlaceholder::from_config)
}

fn de_composition_placeholder<'de, D>(d: D) -> Result<Option<PlaceholderChar>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "composition_placeholder", PlaceholderChar::from_config)
}

/// 拼音音节分隔符的应用级覆盖（`ui.candidate.syllable_separator` 的 per-app 版）。
///
/// 只改显示；没写 = 跟随全局设置（所以没有 `Default`）。常用档位而非任意字符：
/// 要任意字符走全局项。典型用途：Unreal Engine 等对资产名校验严格的程序不接受 `'`，选 `none`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyllableSeparator {
    /// `'`（引擎原样，即全局默认）。
    Apostrophe,
    /// 不显示分隔符。
    None,
    /// 空格。
    Space,
    /// `·`。
    Dot,
}

impl SyllableSeparator {
    /// 配置串 → 枚举。无法识别返回 `None`（＝未配置，跟随全局）。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "apostrophe" => Some(Self::Apostrophe),
            "none" => Some(Self::None),
            "space" => Some(Self::Space),
            "dot" => Some(Self::Dot),
            _ => None,
        }
    }
    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Apostrophe => "apostrophe",
            Self::None => "none",
            Self::Space => "space",
            Self::Dot => "dot",
        }
    }
    /// 实际显示的分隔符字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apostrophe => "'",
            Self::None => "",
            Self::Space => " ",
            Self::Dot => "\u{00B7}",
        }
    }
}

fn de_syllable_separator<'de, D>(d: D) -> Result<Option<SyllableSeparator>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "syllable_separator", SyllableSeparator::from_config)
}

/// 含换行的上屏文本怎么交给宿主。
///
/// 有的宿主（新版微信）对 TSF 多行上屏会残留编码显示，改走剪贴板 + Ctrl+V 即无残留。粘贴会
/// 覆盖剪贴板且 Ctrl+V 未必是粘贴，故按应用配置。
///
/// 消费：DLL 的上屏出口，经 `CONFIG_KEY_MULTILINE_COMMIT` 按客户端 pid 下发。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MultilineCommit {
    /// TSF 上屏。不写 = 这一档。
    #[default]
    Commit,
    /// 结束组合后经剪贴板 + Ctrl+V 粘贴。只影响含换行的上屏。
    Paste,
}

impl MultilineCommit {
    /// 配置串 → 枚举。无法识别返回 `None`（＝未配置，回落 commit）。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "commit" => Some(Self::Commit),
            "paste" => Some(Self::Paste),
            _ => None,
        }
    }
    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Paste => "paste",
        }
    }
}

fn de_multiline_commit<'de, D>(d: D) -> Result<Option<MultilineCommit>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "multiline_commit", MultilineCommit::from_config)
}

/// 应用独立的候选窗定位方式。
///
/// 与全局 `ui.candidate.position_mode` 同语义，按应用覆盖：少数宿主报的 caret 坐标就是不准，
/// 全局改成固定又会连累其余应用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidatePositionMode {
    /// 跟随光标（与全局默认同义）。显式写它 = 「这个应用不要跟随全局的 fixed」。
    FollowCaret,
    /// 固定在 `candidate_x/candidate_y`（该应用自己的一份坐标）。
    Fixed,
}

impl CandidatePositionMode {
    /// 配置串 → 枚举。无法识别返回 `None`（＝跟随全局），理由同
    /// [`FirstShowMode::from_config`]：拼错不该固化成显式覆盖。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "follow_caret" => Some(Self::FollowCaret),
            "fixed" => Some(Self::Fixed),
            _ => None,
        }
    }
    /// 枚举 → 配置串（写回 compat.toml 用）。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::FollowCaret => "follow_caret",
            Self::Fixed => "fixed",
        }
    }
    /// 是否固定位置。
    pub fn is_fixed(self) -> bool {
        matches!(self, Self::Fixed)
    }
}

/// 容错反序列化 `Option<CandidatePositionMode>`：无法识别的值退化为 `None`（＝跟随全局）。
/// 不可改用 derive，理由见 [`de_initial_mode`]。
fn de_candidate_position_mode<'de, D>(d: D) -> Result<Option<CandidatePositionMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(
        d,
        "candidate_position_mode",
        CandidatePositionMode::from_config,
    )
}

/// 状态气泡的锚点：`screen_*` = 前台窗口所在显示器的工作区，
/// `window_*` = 前台窗口的可见边框。几何换算在 UI 层（wind-ui 的 `anchor_origin`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusAnchor {
    ScreenCenter,
    ScreenTopLeft,
    ScreenTopRight,
    ScreenBottomLeft,
    ScreenBottomRight,
    WindowCenter,
    WindowBottomLeft,
}

impl StatusAnchor {
    /// 全部锚点，顺序即菜单顺序。
    pub const ALL: [StatusAnchor; 7] = [
        Self::ScreenCenter,
        Self::ScreenTopLeft,
        Self::ScreenTopRight,
        Self::ScreenBottomLeft,
        Self::ScreenBottomRight,
        Self::WindowCenter,
        Self::WindowBottomLeft,
    ];

    /// 枚举 → 配置串。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::ScreenCenter => "screen_center",
            Self::ScreenTopLeft => "screen_top_left",
            Self::ScreenTopRight => "screen_top_right",
            Self::ScreenBottomLeft => "screen_bottom_left",
            Self::ScreenBottomRight => "screen_bottom_right",
            Self::WindowCenter => "window_center",
            Self::WindowBottomLeft => "window_bottom_left",
        }
    }

    /// 配置串 → 枚举；认不出返回 `None`。查的是 [`Self::ALL`] + [`Self::as_config`]，值域只有一份。
    pub fn from_config(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|a| a.as_config().eq_ignore_ascii_case(s))
    }
}

/// 状态气泡定位方式：全局 `ui.status.position_mode` 与按应用 `status_position_mode` 共用取值
/// （见 [`STATUS_POSITION_MODES`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPositionMode {
    /// 跟随光标（出厂默认）。光标坐标不可信时按 [`StatusFallback`] 兜底。
    FollowCaret,
    /// 固定屏幕坐标（全局 `custom_x/y`；按应用 `status_x/y`）。
    Fixed,
    /// 固定在某个锚点，不读光标。
    Anchor(StatusAnchor),
}

/// [`StatusPositionMode`] 的全部配置串，同时是配置注册表 `ui.status.position_mode` 的值域。
pub const STATUS_POSITION_MODES: [&str; 9] = [
    "follow_caret",
    "fixed",
    "screen_center",
    "screen_top_left",
    "screen_top_right",
    "screen_bottom_left",
    "screen_bottom_right",
    "window_center",
    "window_bottom_left",
];

impl StatusPositionMode {
    /// 配置串 → 枚举；认不出返回 `None`（调用方回落：全局取出厂默认，按应用取跟随全局）。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "follow_caret" => Some(Self::FollowCaret),
            "fixed" => Some(Self::Fixed),
            other => StatusAnchor::from_config(other).map(Self::Anchor),
        }
    }
    /// 枚举 → 配置串。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::FollowCaret => "follow_caret",
            Self::Fixed => "fixed",
            Self::Anchor(a) => a.as_config(),
        }
    }
}

/// 状态气泡的兜底位置：只在 `follow_caret` 且光标坐标不可信时生效。全局
/// `ui.status.fallback_position` 与按应用 `status_fallback_position` 共用取值
/// （见 [`STATUS_FALLBACK_POSITIONS`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFallback {
    /// 最近一次有效坐标（出厂默认 = 本功能引入前的行为）。
    Last,
    /// 不显示。
    Hide,
    /// 显示在锚点。
    Anchor(StatusAnchor),
}

/// [`StatusFallback`] 的全部配置串，同时是配置注册表 `ui.status.fallback_position` 的值域。
pub const STATUS_FALLBACK_POSITIONS: [&str; 9] = [
    "last",
    "hide",
    "screen_center",
    "screen_top_left",
    "screen_top_right",
    "screen_bottom_left",
    "screen_bottom_right",
    "window_center",
    "window_bottom_left",
];

impl StatusFallback {
    /// 配置串 → 枚举；认不出返回 `None`。
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "last" => Some(Self::Last),
            "hide" => Some(Self::Hide),
            other => StatusAnchor::from_config(other).map(Self::Anchor),
        }
    }
    /// 枚举 → 配置串。
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Last => "last",
            Self::Hide => "hide",
            Self::Anchor(a) => a.as_config(),
        }
    }
}

// 两个带载荷的枚举在 compat.toml 里是扁平字符串，derive 表达不了：序列化走 `as_config`，
// 反序列化走下面的容错函数（不实现 `Deserialize`，免得被绕过容错直接 derive 进别的结构）。
impl Serialize for StatusPositionMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_config())
    }
}

impl Serialize for StatusFallback {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_config())
    }
}

/// compat.toml 字符串枚举字段的通用容错：类型写错（非字符串）或值域外的字符串一律回落
/// `None`（= 跟随全局）并 WARN、记入回落清单。理由同 [`de_initial_mode`]；与它不同的是
/// 类型错也吞（同 [`de_app_schema`]）：compat.toml 没有段级降级。
fn de_opt_str_enum<'de, D, T>(
    d: D,
    field: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = match Option::<toml::Value>::deserialize(d)? {
        None => return Ok(None),
        Some(toml::Value::String(s)) => s,
        Some(other) => {
            let raw = other.to_string();
            tracing::warn!("compat.toml: {field} = {raw} 不是字符串，本项按「跟随全局」处理");
            crate::tolerant_de::record_fallback(&raw);
            return Ok(None);
        }
    };
    match parse(&raw) {
        Some(v) => Ok(Some(v)),
        None => {
            tracing::warn!(
                "compat.toml: {field} = \"{raw}\" 不在取值范围内，本项按「跟随全局」处理"
            );
            crate::tolerant_de::record_fallback(&raw);
            Ok(None)
        }
    }
}

fn de_status_position_mode<'de, D>(d: D) -> Result<Option<StatusPositionMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "status_position_mode", StatusPositionMode::from_config)
}

fn de_status_fallback<'de, D>(d: D) -> Result<Option<StatusFallback>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "status_fallback_position", StatusFallback::from_config)
}

/// 容错反序列化 `Option<InitialMode>`：无法识别的值退化为 `None`（＝不干预）。
///
/// 不能直接 `#[derive(Deserialize)]`：一个字段认不出会让整条规则反序列化失败而被跳过。
/// 单字段容错把影响限制在这一个字段内。
fn de_initial_mode<'de, D>(d: D) -> Result<Option<InitialMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "initial_mode / initial_punct", InitialMode::from_config)
}

/// 容错反序列化 `Option<FirstShowMode>`：无法识别的值退化为 `None`（＝跟随全局）。
///
/// 理由同 [`de_initial_mode`]：一个字段拼错不该让整条规则失效。
fn de_first_show_mode<'de, D>(d: D) -> Result<Option<FirstShowMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "first_show_mode", FirstShowMode::from_config)
}

/// 容错反序列化 `Option<NewlineStyle>`：无法识别的值退化为 `None`（＝跟随全局）。
///
/// 理由同 [`de_initial_mode`]：用户照注释手加一条、`style` 拼错一个字母，不该让整条规则失效。
fn de_newline_style<'de, D>(d: D) -> Result<Option<NewlineStyle>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_opt_str_enum(d, "commit_newline.style", NewlineStyle::from_config)
}

/// [`AppCompatRule::schema`] 里「记住本应用上次用的方案」的保留值。
///
/// `@` 前缀是保留标记空间：方案 id 来自文件名，不以 `@` 开头。以 `@` 开头的值只认这一个，
/// 其余在解析时按未配置处理（见 [`de_app_schema`]）。
pub const APP_SCHEMA_REMEMBER: &str = "@remember";

/// [`AppCompatRule::schema`] 解析后的语义视图，见 [`AppCompatRule::app_schema`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppSchema<'a> {
    /// 固定为该方案 id（是否在 `schema.available` 内由协调器按当时的 available 判）。
    Fixed(&'a str),
    /// 记住本应用上次用的方案（记忆表在 `state.toml` 的 `app_schemas`）。
    Remember,
}

/// 容错反序列化 [`AppCompatRule::schema`]：类型写错（`1` / 数组）、空串、`@` 开头但不是
/// [`APP_SCHEMA_REMEMBER`] 的值一律回落 `None`（＝跟随全局）并 WARN。
///
/// 理由同 [`de_initial_mode`]。「id 是否在 `schema.available` 内」不在这里判：available 可热重载，
/// 由协调器按当时的 available 校验。
fn de_app_schema<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = match Option::<toml::Value>::deserialize(d)? {
        None => return Ok(None),
        Some(toml::Value::String(s)) => s,
        Some(other) => {
            let raw = other.to_string();
            tracing::warn!("compat.toml: schema = {raw} 不是字符串，本项按「跟随全局」处理");
            crate::tolerant_de::record_fallback(&raw);
            return Ok(None);
        }
    };
    let v = raw.trim();
    if v.is_empty() {
        return Ok(None);
    }
    if v.starts_with('@') && v != APP_SCHEMA_REMEMBER {
        tracing::warn!(
            "compat.toml: schema = \"{v}\" 不是已知标记（只认 {APP_SCHEMA_REMEMBER}），\
             本项按「跟随全局」处理"
        );
        crate::tolerant_de::record_fallback(v);
        return Ok(None);
    }
    Ok(Some(v.to_string()))
}

/// 容错反序列化规则的 `process`：类型写错（`process = 1`）时**本条规则作废**（回落空串，
/// 查找表与各段构建都跳过空进程名），同文件其它规则照常生效。
///
/// 进程名是规则的主键，认不出就不知道该套给谁，唯一安全的答案是不套给任何人。
fn de_process<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<toml::Value>::deserialize(d)? {
        None => Ok(String::new()),
        // 读入时就 trim：叠加与管理层匹配进程名都 trim，运行时查表若不 trim，
        // 用户手写的 ` Foo.exe ` 就会被菜单写进一条运行时永远匹配不上的行。
        Some(toml::Value::String(s)) => Ok(s.trim().to_string()),
        Some(other) => {
            let raw = other.to_string();
            tracing::warn!("compat.toml: process = {raw} 不是字符串，本条规则作废");
            crate::tolerant_de::record_fallback(&raw);
            Ok(String::new())
        }
    }
}

/// 容错反序列化规则的 `comment`（仅文档用途）：类型写错回落空串并 WARN，规则本身照常生效。
fn de_comment<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<toml::Value>::deserialize(d)? {
        None => Ok(String::new()),
        Some(toml::Value::String(s)) => Ok(s),
        Some(other) => {
            let raw = other.to_string();
            tracing::warn!("compat.toml: comment = {raw} 不是字符串，本项忽略");
            crate::tolerant_de::record_fallback(&raw);
            Ok(String::new())
        }
    }
}

/// 单个应用的兼容性规则。
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AppCompatRule {
    /// 进程名（不区分大小写），如 "Weixin.exe"。类型写错 ⇒ 本条作废，见 [`de_process`]。
    #[serde(default, deserialize_with = "de_process")]
    pub process: String,
    /// 说明（仅文档用途）。
    #[serde(
        default,
        deserialize_with = "de_comment",
        skip_serializing_if = "String::is_empty"
    )]
    pub comment: String,
    /// 用户层专用：禁用同名的系统规则（合并后该进程视为没有这条规则）。
    /// 条目里的其它字段照常保留，去掉本标记即恢复到禁用前的状态。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_bool",
        skip_serializing_if = "is_false"
    )]
    pub disabled: bool,
    /// 使用 caret rect 的 top 而非 bottom 定位候选窗。
    /// 适用于 GetTextExt 返回的 height 不稳定的 WebView 应用（如微信 Qt 输入框，
    /// height 在 1↔20px 间跳变 → bottom 漂移 ~20px，但 top 始终稳定）。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_bool",
        skip_serializing_if = "is_false"
    )]
    pub caret_use_top: bool,
    /// 拦截「组合期间上报的 caret rect 仍停在上一次组合位置」的宿主。
    ///
    /// 开启后，probe 若与组合前宿主主动上报的空闲坐标矛盾即判为陈旧、不予采信（见
    /// `Coordinator::handle_caret_probe`）。
    ///
    /// 必须逐宿主开启：微信该信组合前缓存、WindTerm 该信 probe，同一份位置关系推不出该信谁。
    /// 见 `docs/architecture/caret-position-host-compat-notes.md` 的 B 类与 D 类。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_bool",
        skip_serializing_if = "is_false"
    )]
    pub stale_probe_guard: bool,
    /// 把连续的 `TSF_COMPOSITION`（selection 无效、以组合起点降级）→ `TSF_SELECTION`
    /// 识别为同一次布局采样的两阶段结果，禁止后半帧把当前 caret 误重锁成组合起点。
    ///
    /// 必须逐宿主开启，不能全局信任非零 compStart：别的宿主有 compStart 陈旧、坐标系混用的情形，
    /// 全局信任会关掉大偏移自愈。协调器还会核对来源顺序、前帧降级点、reported compStart 与当前锁
    /// 四者一致。宿主行为见 `docs/architecture/caret-position-host-compat-notes.md` 的 E 类。
    ///
    /// 三态：未写 = 继承下层，`Some(true)` = 开，`Some(false)` = 显式关闭。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub composition_start_pair_guard: Option<bool>,
    /// 宿主报的组合起点跟着插入点漂移时，把候选窗锚点钉在首帧起点，而不是让大偏移逃生阀
    /// 跟着走；`None` / `Some(false)` = 不干预。判据在协调器侧：组合矩形退化成插入点形态。
    ///
    /// 必须逐宿主开启：WPS 文字需要钉住，Excel / WPS 表格数据一模一样却要靠逃生阀跟着单元格走。
    /// 见 `docs/architecture/caret-position-host-compat-notes.md` 的「组合矩形的三种形态」。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub pin_anchor_when_start_drifts: Option<bool>,
    /// 候选窗首显策略；`None` = 不干预，跟随全局 `ui.candidate.first_show_mode`。
    ///
    /// 三档互斥，做成枚举而不是几个 bool，由类型保证互斥。必须是 `Option`：「没配过」与「显式配了
    /// fast」要能区分，否则改全局默认对从未配过的应用不生效。
    #[serde(
        default,
        deserialize_with = "de_first_show_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub first_show_mode: Option<FirstShowMode>,
    /// 进入本应用时的初始中英状态；`None` = 不干预，沿用全局逻辑。
    ///
    /// 必须是 `Option`：`#[serde(default)]` 下的 bool 会让所有未配置的应用拿到 `false`。
    #[serde(
        default,
        deserialize_with = "de_initial_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub initial_mode: Option<InitialMode>,
    /// 进入本应用时的初始中英标点；`None` = 不干预。
    ///
    /// 显式值压过 `input.punct.follow_mode` 的推导，否则开着 follow_mode 时它会静默无效。
    #[serde(
        default,
        deserialize_with = "de_initial_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub initial_punct: Option<InitialMode>,
    /// 该进程加入 HostRender 白名单（受限宿主如 Win11 开始菜单 SearchHost.exe，候选窗由
    /// 服务进程渲染后经共享内存转交宿主进程内的 DLL 上屏，绕开普通窗口盖不过的 Band 层级）。
    ///
    /// 消费点须按事件源 PID 直查 `AppCompat::host_render_processes()` 现算的白名单
    /// （`HostRenderManager::is_process_whitelisted`），不得经 `ActiveCompat` 焦点槽缓存：开始菜单
    /// 弹出会连带激活兄弟进程而污染焦点槽（见 `docs/redesign/host-render-windows-port.md` §11.2）。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_bool",
        skip_serializing_if = "is_false"
    )]
    pub host_render: bool,
    /// 该应用是否启用符号自动配对；`None` = 不干预，沿用全局 `input.auto_pair.*`。
    ///
    /// 必须是 `Option`，理由同 `initial_mode`。典型用途是表格类宿主（Excel / WPS 表格）：
    /// 配对后无法把光标退回两符号之间（见 `docs/architecture/app-compat-host-notes.md` 第五节）。
    ///
    /// 消费点有三条，缺一即半截修复：`active_pairs()`（中文标点态）、`english_pairs_via_pipeline()`
    /// （英文标点流水线）、`push_english_pair_config()`（纯英文模式由 C++ 独立处理）。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_pair: Option<bool>,
    /// 该应用的配对插入是否改走 Unicode 按键注入提交；`None` = 跟随全局
    /// `input.auto_pair.commit_via_composition`（默认直写）。
    ///
    /// 必须是 `Option`，理由同 `auto_pair`。消费在推送路径 `push_pair_via_comp_config`：直查目标
    /// PID/窗口的规则表，不走 `active_compat` 焦点槽（后台进程会收到错误值）。
    ///
    /// 典型用途：AutoCAD 表格在位编辑器、Chromium 系宿主丢弃直写却谎报 S_OK，见
    /// `docs/architecture/pair-commit-host-compat-notes.md`。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub pair_via_comp: Option<bool>,
    /// 该应用是否启用中英 / 中数间自动补空格；`None` = 跟随全局 `input.spacing.*`。
    ///
    /// 只能关不能「开」出全局没开的那一档：`Some(true)` 等于跟随全局（取哪几种交界仍由全局
    /// 两个开关定），存在的意义是在用户层压掉系统规则里的 `false`。必须是 `Option`，理由同
    /// `auto_pair`。
    ///
    /// 典型用途是自带空格处理的 Markdown 编辑器（pangu 类插件）、终端与代码编辑器。
    /// 拉丁字母之间的词间空格（`commit_space`）属英文输入本身，不受本项影响。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_spacing: Option<bool>,
    /// 该应用的密码框是否强制英文；`None` = 跟随全局 `input.password_force_english`。
    /// 典型用途是宿主把普通输入框误报成密码框。必须是 `Option`，理由同 `initial_mode`。
    ///
    /// 消费点只能有一个判定函数（协调器 `password_force_english_for_pid`）：服务端抑制态与推给
    /// DLL 的吃键门控须出自同一处，否则密码框丢键。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub password_force_english: Option<bool>,
    /// 该应用使用的输入方案；`None` = 跟随全局（最近一次全局手切的结果 / `schema.active`）。
    ///
    /// - 具体方案 id ⇒ 焦点跨进程切入时固定切到它；
    /// - [`APP_SCHEMA_REMEMBER`] ⇒ 切入时恢复本应用上次用的方案（无记录则用全局）。
    ///
    /// 规则应用内的手切只改本应用的方案，不写 `schema.active`。消费点在协调器
    /// `app_schema_rule`（按当时的 available 校验）。
    /// 读取请走 [`Self::app_schema`]，不要直接比较字符串。
    #[serde(
        default,
        deserialize_with = "de_app_schema",
        skip_serializing_if = "Option::is_none"
    )]
    pub schema: Option<String>,
    /// 该应用的智能符号替换方案；`None` = 沿用全局 `input.symbol.smart_method`。
    ///
    /// `DeleteReplace`（全局默认）依赖对宿主做删改，在 Tabby 一类终端上出错；`HoldComposition`
    /// 全程不做删改。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub smart_method: Option<SmartMethod>,
    /// 光标坐标水平校正（dp，96dpi 基准逻辑像素，正=右）。
    ///
    /// 修宿主上报 caret 坐标的系统性偏移（如 Windows Terminal），与主题里候选窗相对光标的布局
    /// 偏移不是一回事；候选窗、状态气泡等所有消费者一并受益。单位 dp，按目标点所在显示器 DPI
    /// 换算在协调器侧完成（`apply_caret_compat`）。0 = 不偏移，故不用 `Option`。
    ///
    /// 消费点有两处（`apply_focus_caret` / `handle_caret_update`），与 `caret_use_top` 同层同处。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_i32",
        skip_serializing_if = "is_zero_i32"
    )]
    pub caret_offset_x: i32,
    /// 光标坐标垂直校正（dp，96dpi 基准逻辑像素，正=下）。语义见 [`Self::caret_offset_x`]。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_i32",
        skip_serializing_if = "is_zero_i32"
    )]
    pub caret_offset_y: i32,
    /// 该应用的候选窗定位方式；`None` = 不干预，沿用全局 `ui.candidate.position_mode`。
    ///
    /// 必须是 `Option`，理由同 `first_show_mode`。
    #[serde(
        default,
        deserialize_with = "de_candidate_position_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub candidate_position_mode: Option<CandidatePositionMode>,
    /// 固定模式下该应用自己的候选窗落点（内容左上屏幕坐标，物理像素）。合适的落点取决于该应用
    /// 窗口的位置，故各存一份。
    ///
    /// `(0,0)` = 已开固定但还没拖过，与全局 `ui.candidate.custom_x/custom_y` 同一套哨兵约定（含
    /// `avoid_unset_sentinel` 的 1px 规避），两处必须同款。不分显示器，换屏由 `clamp_to_work_area` 兜住。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_i32",
        skip_serializing_if = "is_zero_i32"
    )]
    pub candidate_x: i32,
    /// 固定模式下该应用自己的候选窗落点 Y。语义见 [`Self::candidate_x`]。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_i32",
        skip_serializing_if = "is_zero_i32"
    )]
    pub candidate_y: i32,
    /// 该应用的状态气泡定位方式；`None` = 跟随全局 `ui.status.position_mode`。
    ///
    /// 与 [`Self::candidate_position_mode`] 同构；坐标 [`Self::status_x`]/[`Self::status_y`] 与它同层取，
    /// 没配才整套回落全局。消费点在协调器 `status_position`。
    #[serde(
        default,
        deserialize_with = "de_status_position_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub status_position_mode: Option<StatusPositionMode>,
    /// `fixed` 下该应用自己的状态气泡落点 X（内容左上屏幕坐标，物理像素）。`(0,0)` = 已开固定
    /// 但还没摆过，由 UI 落到光标所在屏——与全局 `ui.status.custom_x/y` 同一套哨兵约定。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_i32",
        skip_serializing_if = "is_zero_i32"
    )]
    pub status_x: i32,
    /// `fixed` 下该应用自己的状态气泡落点 Y。语义见 [`Self::status_x`]。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_i32",
        skip_serializing_if = "is_zero_i32"
    )]
    pub status_y: i32,
    /// 该应用光标坐标不可信时状态气泡的兜底位置；`None` = 跟随全局 `ui.status.fallback_position`。
    /// 只在（规则或全局解析出的）定位方式为 `follow_caret` 时生效。
    #[serde(
        default,
        deserialize_with = "de_status_fallback",
        skip_serializing_if = "Option::is_none"
    )]
    pub status_fallback_position: Option<StatusFallback>,
    /// 忽略该宿主自己写 compartment（OPENCLOSE / CONVERSION）引起的中英切换，开和关两个方向。
    /// `None` = 跟随全局 `input.ignore_host_ime_switch`，`Some(v)` 覆盖全局。旧键
    /// `ignore_host_ime_close` 作读取别名（`compat_overlay::LEGACY_APP_KEYS`）。
    ///
    /// 开启有代价：gvim 一类宿主正确依赖 OPENCLOSE 值语义，故全局出厂关。Ctrl 按住时放行（区分系统
    /// 热键 Ctrl+Space，判据由 DLL 经 `MODE_SWITCH_CTRL_HELD` 交代）。宿主背景见
    /// `docs/architecture/tsf-openclose-compartment-semantics.md` 第 7 节。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub ignore_host_ime_switch: Option<bool>,
    /// 这个宿主读走候选串时，是否当它在自绘、从而收起我们的候选窗。
    /// `None` = 否（默认），`Some(true)` = 是，`Some(false)` = **显式关闭**。
    ///
    /// `Some(false)` 与 `None` 不同义：显式 `false` 才挡得住出厂同名规则（如 `MapleStory.exe = true`）
    /// 与 `process = "*"` 通配。
    ///
    /// 只管「宿主没声明、却把候选串读走了」（`UIELEMENT_FLAG_HOST_READS`）这条推断；宿主声明接管
    /// （`BeginUIElement` 回 `pbShow=FALSE`）是事实，本开关管不着。默认「否」：普通桌面宿主读走候选串
    /// 是常态，误判成「宿主在画」会让候选框一个都没有，代价远大于多一个框。依据见
    /// `docs/design/game-compat-tsf-uielement.md` §1.4。查表见 `Coordinator::uielement_host_draws_by_inference`。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub host_drawn_candidates: Option<bool>,
    /// 「打开设置 / 加词编辑」这类要拉起外部程序的操作，是否由**服务进程**直接执行。
    /// `None` / `Some(false)` = 默认：经 IPC 让当前宿主进程执行（宿主有前台权限，窗口能到最前）；
    /// `Some(true)` = 服务进程自己 `ShellExecuteW`。
    ///
    /// Win11 开始菜单、便笺等宿主收到拉起请求后并不执行。代价：服务进程没有前台权限，拉起的
    /// 窗口可能停在后台，故只给确有需要的宿主开。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub launch_by_service: Option<bool>,
    /// 组合区占位字符；`None` = 空格。见 [`PlaceholderChar`]。
    ///
    /// 查表请走 [`AppCompat::composition_placeholder_for`]：它负责「进程名未知 ⇒ 空格」这条
    /// 例外（`process = "*"` 通配已由统一合成处理）。
    #[serde(
        default,
        deserialize_with = "de_composition_placeholder",
        skip_serializing_if = "Option::is_none"
    )]
    pub composition_placeholder: Option<PlaceholderChar>,
    /// 联想态挂在宿主里的占位字符；`None` = 跟随 `composition_placeholder`。
    ///
    /// 联想态没有编码，占位组合只为让宿主继续转发按键；有的软件会拿这个字符做特殊处理，
    /// 可以只给联想态换一个，不动打字时的占位。
    #[serde(
        default,
        deserialize_with = "de_assoc_placeholder",
        skip_serializing_if = "Option::is_none"
    )]
    pub assoc_placeholder: Option<AssocPlaceholder>,
    /// 拼音音节分隔符的显示；`None` = 跟随全局 `ui.candidate.syllable_separator`。见 [`SyllableSeparator`]。
    #[serde(
        default,
        deserialize_with = "de_syllable_separator",
        skip_serializing_if = "Option::is_none"
    )]
    pub syllable_separator: Option<SyllableSeparator>,
    /// 含换行的上屏走 TSF 还是剪贴板粘贴；`None` = commit（历史行为）。见 [`MultilineCommit`]。
    #[serde(
        default,
        deserialize_with = "de_multiline_commit",
        skip_serializing_if = "Option::is_none"
    )]
    pub multiline_commit: Option<MultilineCommit>,
}

impl AppCompatRule {
    /// [`Self::schema`] 的语义视图；`None` = 未配置（跟随全局）。
    pub fn app_schema(&self) -> Option<AppSchema<'_>> {
        match self.schema.as_deref()? {
            APP_SCHEMA_REMEMBER => Some(AppSchema::Remember),
            id => Some(AppSchema::Fixed(id)),
        }
    }
}

fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

/// 用户层（右键菜单）写入的入口：读用户层 → 交给 `f` 改 → 整份写回。
///
/// 只读写**用户层**：系统层 `data/compat.toml` 不受影响。文件或目录不存在时自动创建。
/// 用户层里记的是**相对系统层的差异**，`f` 拿到已叠加好的系统层（用来判断哪些值冗余、
/// 哪些字段清除时需要 `unset`）以及「系统层是否真的读到了」。
///
/// 解析失败仍然重建（不让菜单卡死，用户手改坏了 TOML 时仍能通过菜单恢复到可用状态），
/// 但先把损坏的原文件留一份 `.bad`：手改坏了的内容往往还能救，静默抹掉就是白丢。
/// 读到写完的全程持有 [`lock_user_compat`]：右键菜单与设置端 RPC 会交错写同一个文件。
fn update_user_raw(
    user_dir: &Path,
    f: impl FnOnce(&mut Raw, &Raw, bool),
) -> Result<(), std::io::Error> {
    let _guard = lock_user_compat();
    // 用户目录退化成数据目录时（拿不到用户配置目录的兜底），「用户层」与「系统层」是同一个文件：
    // 此时写入会把系统预置里被判为「冗余」的字段当场删掉。拒绝比改坏出厂文件好。
    #[cfg(not(test))]
    if crate::config::Config::data_dir().is_some_and(|d| d == user_dir) {
        return Err(std::io::Error::other(
            "用户配置目录与数据目录相同，拒绝改写系统预置的 compat.toml",
        ));
    }
    let path = user_dir.join(COMPAT_FILE_NAME);
    let mut user = match std::fs::read_to_string(&path) {
        Ok(text) => match parse_raw(&text) {
            // 「读得进来」还不够：有内容被跳过（`[apps]` 写成单括号之类）时，重写会把它们丢掉，
            // 同样按损坏处理——先留一份 `.bad`。
            Ok(r) if r.skipped.is_empty() => r,
            Ok(r) => {
                tracing::warn!(
                    "用户层 compat.toml 有内容无法读入（{}），将重建（原文件留作 {COMPAT_FILE_NAME}.bad）",
                    r.skipped.join("；")
                );
                let _ = std::fs::copy(&path, user_dir.join(format!("{COMPAT_FILE_NAME}.bad")));
                Raw {
                    skipped: Vec::new(),
                    ..r
                }
            }
            Err(e) => {
                tracing::warn!(
                    "用户层 compat.toml 解析失败，将重建（原文件留作 {COMPAT_FILE_NAME}.bad）: {e}"
                );
                let _ = std::fs::copy(&path, user_dir.join(format!("{COMPAT_FILE_NAME}.bad")));
                Raw::default()
            }
        },
        Err(_) => Raw::default(),
    };
    let (system, known) = system_layer_for_menu();
    f(&mut user, &system, known);
    std::fs::create_dir_all(user_dir)?;
    let text = render_raw(USER_COMPAT_HEADER, &user).map_err(std::io::Error::other)?;
    write_atomic(&path, &text)?;
    // 菜单写入不经 RPC，设置端此前无从得知；写成功后通知（经 RPC 的写入由 dispatch 广播）。
    crate::change_hook::notify_compat_changed("menu", user_dir);
    Ok(())
}

/// 菜单路径看到的系统层（`data` ⊕ `data_custom`）。第二个返回值 = 系统层是否**确定**：
/// 只有数据目录本身解析不出来时才算未知——目录在、文件缺失或解析失败，运行时看到的就是
/// 「系统层为空」，这是确定的答案。未知时清除字段不能凭「系统里没有」就省掉 `unset`。
#[cfg(not(test))]
fn system_layer_for_menu() -> (Raw, bool) {
    let data = crate::config::Config::data_dir();
    let known = data.is_some();
    let mut raw = Raw::default();
    for dir in [data, crate::config::Config::custom_data_dir()]
        .into_iter()
        .flatten()
    {
        if let Some(layer) = load_raw(&dir.join(COMPAT_FILE_NAME)) {
            raw = overlay_raw(raw, &layer);
        }
    }
    (raw, known)
}

/// 测试构建里菜单路径的系统层由测试显式注入（读真实数据目录会让测试依赖本机环境）；
/// 没注入 = 系统层未知。
#[cfg(test)]
fn system_layer_for_menu() -> (Raw, bool) {
    TEST_MENU_SYSTEM
        .with(|c| c.borrow().clone())
        .unwrap_or((Raw::default(), false))
}

#[cfg(test)]
thread_local! {
    static TEST_MENU_SYSTEM: std::cell::RefCell<Option<(Raw, bool)>> =
        const { std::cell::RefCell::new(None) };
}

/// 测试：在 `f` 期间让右键菜单的写入路径看到指定的系统层。
#[cfg(test)]
pub(crate) fn with_menu_system<R>(system: Raw, known: bool, f: impl FnOnce() -> R) -> R {
    TEST_MENU_SYSTEM.with(|c| *c.borrow_mut() = Some((system, known)));
    let r = f();
    TEST_MENU_SYSTEM.with(|c| *c.borrow_mut() = None);
    r
}

/// 对 `[[apps]]` 里某一条（按身份定位）的差异行做一批字段编辑。传进程名即**纯进程键**那一条，
/// 同进程带 class / title 的规则不受影响；右键菜单在当前值来自更具体的窗口规则时传那条的身份
/// （协调器 `menu_writeback_target`），出厂规则则在用户层写同身份的覆盖。
fn edit_user_apps(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    edits: &[FieldEdit],
) -> Result<(), std::io::Error> {
    let target = target.into();
    let Some(id) = target.id("apps") else {
        return Err(std::io::Error::other(
            "规则身份为空（进程名 / 类名 / 标题都没有）",
        ));
    };
    let id = Some(id);
    update_user_raw(user_dir, |user, system, known| {
        // 下层（系统 / data_custom）把这个进程的规则禁用了，而用户在菜单里给它选了一个选项：
        // 这是明确的「我要配置它」，不带重新启用的话规则仍是禁用，选项存了却永远不生效。
        // 用户自己在设置页禁用的（用户层有 `disabled`）则不动，那是他的明确决定。
        let lower_disabled = system
            .apps
            .iter()
            .any(|r| rule_id("apps", r) == id && is_disabled(r));
        let user_decided = user
            .apps
            .iter()
            .any(|r| rule_id("apps", r) == id && r.contains_key("disabled"));
        let mut all = edits.to_vec();
        if lower_disabled && !user_decided {
            all.push(FieldEdit::Set(
                "disabled".into(),
                serde_json::Value::Bool(false),
            ));
        }
        let had = user.apps.iter().any(|r| rule_id("apps", r) == id);
        apply_edits::<AppCompatRule>("apps", &system.apps, &mut user.apps, &target, &all, known);
        // 菜单「跟随全局」作用在用户新增的「进程 + 窗口」规则上：清掉的只是这一条的字段，更低级的
        // 规则（纯进程 / `*`）会透上来；清空了整条就消失。留个痕迹便于排查「点了跟随全局却不是全局值」。
        let in_system = system.apps.iter().any(|r| rule_id("apps", r) == id);
        if had && !in_system && !user.apps.iter().any(|r| rule_id("apps", r) == id) {
            tracing::debug!(
                "菜单写回后用户规则已无任何字段，整条移除（process={} class={:?} title_len={}）",
                target.process,
                target.class,
                target.title.chars().count()
            );
        }
    })
}

/// `Some(v)` ⇒ 设置；`None` ⇒ 清除（回到没设置 / 跟随全局，系统层设了它时写 `unset`）。
fn set_or_clear<T: Serialize>(key: &str, v: Option<T>) -> FieldEdit {
    match v.map(|v| serde_json::to_value(v)) {
        Some(Ok(v)) if !v.is_null() => FieldEdit::Set(key.to_string(), v),
        _ => FieldEdit::Clear(key.to_string()),
    }
}

fn set_value(key: &str, v: impl Into<serde_json::Value>) -> FieldEdit {
    FieldEdit::Set(key.to_string(), v.into())
}

fn clear(key: &str) -> FieldEdit {
    FieldEdit::Clear(key.to_string())
}

/// 某进程在 `[[apps]]` 里的整条规则开关状态（右键菜单的「启用兼容规则」用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSwitch {
    /// 系统层与用户层都没有这个进程的规则：没有可开关的东西。
    NoRule,
    Enabled,
    Disabled,
}

/// 读某进程整条规则（纯进程键那一条）的开关状态（系统层 ⊕ 用户层叠加后判定）。
pub fn menu_rule_switch(user_dir: &Path, process: &str) -> RuleSwitch {
    let (system, _) = system_layer_for_menu();
    let user = load_raw(&user_dir.join(COMPAT_FILE_NAME)).unwrap_or_default();
    let merged = overlay_raw(system, &user);
    let id = Some(RuleId::process_only(process));
    match merged.apps.iter().find(|r| rule_id("apps", r) == id) {
        None => RuleSwitch::NoRule,
        Some(r) if is_disabled(r) => RuleSwitch::Disabled,
        Some(_) => RuleSwitch::Enabled,
    }
}

/// 启用 / 禁用某进程的整条规则（写用户层 `disabled`；启用 = 去掉这条差异，
/// 系统层自己就禁用的规则则写显式 `disabled = false`，均由 `apply_edits` 的规范化决定）。
///
/// 不走 [`edit_user_apps`]：它会在下层禁用时自动追加 `disabled = false`，排在后面会把这里写的
/// 「禁用」冲掉。与设置端 `CompatAdmin::set_disabled` 同一套写法。
pub fn set_user_rule_disabled(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    disabled: bool,
) -> Result<(), std::io::Error> {
    let target = target.into();
    update_user_raw(user_dir, |user, system, known| {
        apply_edits::<AppCompatRule>(
            "apps",
            &system.apps,
            &mut user.apps,
            &target,
            &[set_value("disabled", disabled)],
            known,
        )
    })
}

/// 设置用户层 compat.toml 中指定进程的首显策略（`None` = 清除，回到跟随全局）。
pub fn set_user_first_show_mode(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    mode: Option<FirstShowMode>,
) -> Result<(), std::io::Error> {
    edit_user_apps(user_dir, target, &[set_or_clear("first_show_mode", mode)])
}

/// 设置用户层 compat.toml 中指定进程的初始中英状态（`None` = 清除）。
pub fn set_user_initial_mode(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    mode: Option<InitialMode>,
) -> Result<(), std::io::Error> {
    edit_user_apps(user_dir, target, &[set_or_clear("initial_mode", mode)])
}

/// 设置用户层 compat.toml 中指定进程的初始中英标点（`None` = 清除）。
pub fn set_user_initial_punct(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    mode: Option<InitialMode>,
) -> Result<(), std::io::Error> {
    edit_user_apps(user_dir, target, &[set_or_clear("initial_punct", mode)])
}

/// 设置用户层 compat.toml 中指定进程是否加入 HostRender 白名单。
/// `false` 写成显式覆盖（系统层打开时它才是「从白名单里去掉」；系统层没有时会被当作冗余丢掉）。
pub fn set_user_host_render(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    enabled: bool,
) -> Result<(), std::io::Error> {
    edit_user_apps(user_dir, target, &[set_value("host_render", enabled)])
}

/// 设置用户层 compat.toml 中指定进程的符号自动配对开关（`None` = 清除）。
pub fn set_user_auto_pair(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    enabled: Option<bool>,
) -> Result<(), std::io::Error> {
    edit_user_apps(user_dir, target, &[set_or_clear("auto_pair", enabled)])
}

/// 设置用户层 compat.toml 中指定进程的密码框强制英文（`None` = 清除）。
pub fn set_user_password_force_english(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    enabled: Option<bool>,
) -> Result<(), std::io::Error> {
    edit_user_apps(
        user_dir,
        target,
        &[set_or_clear("password_force_english", enabled)],
    )
}

/// 设置用户层 compat.toml 中指定进程的输入方案（`None` = 清除）。
pub fn set_user_schema(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    schema: Option<String>,
) -> Result<(), std::io::Error> {
    edit_user_apps(user_dir, target, &[set_or_clear("schema", schema)])
}

/// 设置用户层 compat.toml 中指定进程的候选窗定位方式（`None` = 清除，含坐标）。
///
/// ⚠ 清除定位方式时**一并清坐标**：留着一份孤儿坐标，下次用户重新开固定就会跳到
/// 上一次的老位置，而他刚刚才关掉它——「关了又开，位置从哪来的」无从解释。
pub fn set_user_candidate_position_mode(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    mode: Option<CandidatePositionMode>,
) -> Result<(), std::io::Error> {
    let mut edits = vec![set_or_clear("candidate_position_mode", mode)];
    if mode.is_none() {
        edits.push(clear("candidate_x"));
        edits.push(clear("candidate_y"));
    }
    edit_user_apps(user_dir, target, &edits)
}

/// 记住指定进程的候选窗固定落点：**定位方式与坐标一起写**（内容左上，物理像素）。
///
/// 拖动本身就是「固定在这儿」的确认，方式与坐标一起写，用户层这条差异自成一体。
/// 调用方须先做 `(0,0)` 哨兵规避（规避函数在协调器侧，这里不代劳）。
pub fn set_user_candidate_fixed_pos(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    x: i32,
    y: i32,
) -> Result<(), std::io::Error> {
    edit_user_apps(
        user_dir,
        target,
        &[
            set_or_clear(
                "candidate_position_mode",
                Some(CandidatePositionMode::Fixed),
            ),
            set_value("candidate_x", x),
            set_value("candidate_y", y),
        ],
    )
}

/// 设置用户层 compat.toml 中指定进程的「忽略软件切换中英」（`None` = 清除，即跟随全局
/// `input.ignore_host_ime_switch`：下层设了它就写 `unset`）。
///
/// 写新键；该条里的旧键 `ignore_host_ime_close` 在读入时已规范化成新键，写回自然只剩新键。
pub fn set_user_ignore_host_ime_switch(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    enabled: Option<bool>,
) -> Result<(), std::io::Error> {
    edit_user_apps(
        user_dir,
        target,
        &[set_or_clear("ignore_host_ime_switch", enabled)],
    )
}

/// 设置用户层 compat.toml 中指定进程的智能符号替换方案（`None` = 清除）。
pub fn set_user_smart_method(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    method: Option<SmartMethod>,
) -> Result<(), std::io::Error> {
    edit_user_apps(user_dir, target, &[set_or_clear("smart_method", method)])
}

/// 设置用户层 compat.toml 中指定进程的光标坐标校正偏移（像素，正=右/下）。
pub fn set_user_caret_offset(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    dx: i32,
    dy: i32,
) -> Result<(), std::io::Error> {
    edit_user_apps(
        user_dir,
        target,
        &[
            set_value("caret_offset_x", dx),
            set_value("caret_offset_y", dy),
        ],
    )
}

/// 设置用户层 compat.toml 中指定进程的状态气泡定位：**方式与坐标一起写**。`None` = 清除
/// （坐标一并清掉）；非 `fixed` 的方式不用坐标，也清掉，免得留一对孤儿坐标、下次开固定跳到老位置。
///
/// ⚠ `fixed` 的坐标由调用方先做 `(0,0)` 哨兵规避（协调器的 `avoid_unset_sentinel`）。
pub fn set_user_status_position(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    mode: Option<StatusPositionMode>,
    x: i32,
    y: i32,
) -> Result<(), std::io::Error> {
    let mut edits = vec![set_or_clear("status_position_mode", mode)];
    if mode == Some(StatusPositionMode::Fixed) {
        edits.push(set_value("status_x", x));
        edits.push(set_value("status_y", y));
    } else {
        edits.push(clear("status_x"));
        edits.push(clear("status_y"));
    }
    edit_user_apps(user_dir, target, &edits)
}

/// 设置用户层 compat.toml 中指定进程的状态气泡兜底位置（`None` = 清除）。
pub fn set_user_status_fallback(
    user_dir: &Path,
    target: impl Into<RuleKey>,
    fallback: Option<StatusFallback>,
) -> Result<(), std::io::Error> {
    edit_user_apps(
        user_dir,
        target,
        &[set_or_clear("status_fallback_position", fallback)],
    )
}

/// 「初始模式作用域」规则：某进程的 per-app **初始模式**只在哪些**窗口类**上重算。
///
/// `explorer.exe` 一个进程名同时承载桌面与任务栏 / Alt+Tab 等路过型焦点，给桌面配的初始模式
/// 不该套到后者上。判据必须是白名单「规则在哪生效」，不能是黑名单「哪些是过渡窗口」：拿不到
/// 窗口类与新出现的类都该落在作用域外、保持现状。见 `docs/architecture/app-compat-host-notes.md`
/// 第四节。
///
/// 独立成段而不是 `[[apps]]` 的字段：它是进程规则的适用范围，不是又一个行为开关。
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct InitialModeScopeRule {
    /// 进程映像名（不区分大小写），如 `explorer.exe`。缺失或类型写错 ⇒ 本条作废（见 [`de_process`]）。
    #[serde(default, deserialize_with = "de_process")]
    pub process: String,
    /// 说明（仅文档用途）。与 `AppCompatRule::comment` 同理**必须存在于结构体里**：
    /// serde 默认静默忽略未知字段，只声明在 TOML 注释里的话，用户层写回时会被丢掉。
    #[serde(
        default,
        deserialize_with = "de_comment",
        skip_serializing_if = "String::is_empty"
    )]
    pub comment: String,
    /// 用户层专用：禁用同名的系统规则（合并后该进程视为没有这条规则）。
    /// 条目里的其它字段照常保留，去掉本标记即恢复到禁用前的状态。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_bool",
        skip_serializing_if = "is_false"
    )]
    pub disabled: bool,
    /// 该进程下**允许重算初始模式**的顶层窗口类名（不区分大小写）。
    /// 空清单 = 该进程的初始模式规则在任何窗口上都不重算。
    #[serde(default)]
    pub classes: Vec<String>,
}

/// 按应用指定上屏文本的换行形式（值域与理由见 [`NewlineStyle`]）。
///
/// 独立成段而不是 `[[apps]]` 的字段：它与候选窗定位无关，也不参与右键菜单写回。代价是同一
/// 进程的配置分散在两个段。
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct CommitNewlineRule {
    /// 进程映像名（不区分大小写），如 `WINWORD.EXE`。类型写错 ⇒ 本条作废（见 [`de_process`]）。
    #[serde(default, deserialize_with = "de_process")]
    pub process: String,
    /// 说明（仅文档用途）。与 [`AppCompatRule::comment`] 同理**必须存在于结构体里**：
    /// serde 默认静默忽略未知字段，只声明在 TOML 注释里的话，用户层写回时会被丢掉。
    #[serde(
        default,
        deserialize_with = "de_comment",
        skip_serializing_if = "String::is_empty"
    )]
    pub comment: String,
    /// 用户层专用：禁用同名的系统规则（合并后该进程视为没有这条规则）。
    /// 条目里的其它字段照常保留，去掉本标记即恢复到禁用前的状态。
    #[serde(
        default,
        deserialize_with = "crate::tolerant_de::tolerant_bool",
        skip_serializing_if = "is_false"
    )]
    pub disabled: bool,
    /// 该应用上屏时换行用什么字符表达。**认不出的值退化为 `None`＝跟随全局**，
    /// 而不是让整份文件解析失败——理由见 [`de_newline_style`]。
    #[serde(default, deserialize_with = "de_newline_style")]
    pub style: Option<NewlineStyle>,
}

/// 焦点窗口上下文：按它解析 `[[apps]]` 规则（[`AppCompat::resolve`]）。
///
/// 类名 / 标题拿不到时传空串：带对应条件的规则一律不命中（「不知道是哪个窗口」不能套窗口规则）。
/// 进程名为空同理：只命中不限进程（`"*"` / 只写窗口条件）的规则。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowCtx<'a> {
    /// 进程映像名，如 `AutoHotkey.exe`（不区分大小写）。
    pub process: &'a str,
    /// 顶层窗口类名。
    pub class: &'a str,
    /// 顶层窗口标题。
    pub title: &'a str,
}

/// 一次解析的结果。
#[derive(Debug, Clone, Default)]
pub struct ResolvedRule {
    /// 命中的各条规则按级合成后的结果；一条都没命中为 `None`。
    ///
    /// `process` 取命中的最具体那条具体进程规则的写法；只命中了不限进程的规则时为 `"*"`。
    pub rule: Option<AppCompatRule>,
    /// 命中的**带窗口条件**的规则身份，按合成顺序。只命中纯进程 / `"*"` 规则时为空。
    ///
    /// 留给协调器判断「命中的窗口规则变了没有」：进入类字段（initial_mode 等）只在 pid 变了、
    /// 或这个集合变了时才重算，仅标题变化不重算（设计稿「服务端判定」）。
    pub window_matches: Vec<RuleId>,
    /// `window_matches` 里**自己写了（或 `unset` 了）进入类字段**（[`ENTRY_FIELDS`]）的那些，同序。
    ///
    /// 进入类字段的跨越判据看的是它而不是 `window_matches`：出厂的 `Chrome_WidgetWin_*` 只管
    /// 占位字符，VS Code 里弹出个原生对话框（类名不同）就让它不再命中——若按 `window_matches`
    /// 判，这一下就会被当成「切进了另一个规则窗口」，把用户在 VS Code 里手切的中英态冲掉。
    pub entry_window_matches: Vec<RuleId>,
}

/// 进入类字段：只在焦点「切进来」那一刻生效（初始中英、初始标点、按应用方案），停留期间尊重
/// 用户手切。其余字段都是持续类，随窗口上下文即时刷新。
pub const ENTRY_FIELDS: [&str; 3] = ["initial_mode", "initial_punct", "schema"];

/// 参与合成的一条 `[[apps]]` 规则（已跨层叠好、启用、单独反序列化过）。
#[derive(Debug, Clone)]
struct Candidate {
    id: RuleId,
    /// 原始键值，已去掉身份键（`process` / `class` / `title`），保留 `unset`。
    fields: Obj,
    /// 进程名原始写法（trim 后），合成结果的 `process` 取它。
    process: String,
    /// `id.class` / `id.title` 拆成字符（[`RuleId`] 里已是折叠后的）：建表时拆一次，
    /// 匹配时不分配、不重折。
    class_pat: Box<[char]>,
    title_pat: Box<[char]>,
}

/// 一次解析用的焦点窗口上下文，已规范化：进程名 ASCII 小写，类名 / 标题逐字符折叠
/// （与 [`RuleId`] 同一口径）。每次解析只折叠这一次，逐条规则比较时直接用。
struct FoldedCtx {
    /// 进程名 ASCII 小写（未 trim，与 [`WindowCtx::process`] 原样比较的口径一致）；空 = 不知道是谁。
    process: String,
    class: Vec<char>,
    title: Vec<char>,
}

impl FoldedCtx {
    fn new(ctx: &WindowCtx) -> Self {
        use crate::compat_overlay::fold_char;
        FoldedCtx {
            process: ctx.process.to_ascii_lowercase(),
            class: ctx.class.chars().map(fold_char).collect(),
            title: ctx.title.chars().map(fold_char).collect(),
        }
    }
}

/// 窗口条件：模式为空 = 不限；拿不到值（空串）⇒ 带该条件的规则不命中（哪怕模式是 `*`）。
fn cond_matches(pattern: &[char], value: &[char]) -> bool {
    pattern.is_empty() || (!value.is_empty() && wildcard_match_folded(pattern, value))
}

impl Candidate {
    fn matches(&self, ctx: &FoldedCtx) -> bool {
        self.matches_process_class(&ctx.process, &ctx.class)
            && cond_matches(&self.title_pat, &ctx.title)
    }

    /// 进程 + 类名两项（不看标题）：解析缓存的一级按它把候选拆成「基底」与「待比标题」两组。
    fn matches_process_class(&self, process: &str, class: &[char]) -> bool {
        let process_ok =
            self.id.is_any_process() || (!process.is_empty() && self.id.process == process);
        process_ok && cond_matches(&self.class_pat, class)
    }
}

/// 按合成顺序把命中的规则逐行叠起来（`cands` 必须已按合成顺序排好）。没有命中 = 空结果。
fn compose_candidates<'a>(cands: impl Iterator<Item = &'a Candidate>) -> ResolvedRule {
    let mut acc = Obj::new();
    let mut hit = false;
    let mut name: Option<&str> = None;
    let mut window_matches = Vec::new();
    let mut entry_window_matches = Vec::new();
    for c in cands {
        hit = true;
        acc = compose(&acc, &c.fields);
        if !c.id.is_any_process() {
            name = Some(&c.process);
        }
        if c.id.has_window_condition() {
            window_matches.push(c.id.clone());
            let unset = unset_of(&c.fields);
            if ENTRY_FIELDS
                .iter()
                .any(|k| c.fields.contains_key(*k) || unset.iter().any(|u| u == k))
            {
                entry_window_matches.push(c.id.clone());
            }
        }
    }
    if !hit {
        return ResolvedRule::default();
    }
    acc.remove("unset");
    acc.insert(
        "process".into(),
        serde_json::Value::String(name.unwrap_or(ANY_PROCESS).to_string()),
    );
    let rule = match serde_json::from_value::<AppCompatRule>(serde_json::Value::Object(acc)) {
        Ok(r) => Some(r),
        Err(e) => {
            // 每行单独反序列化过（见 `build_apps`），正常走不到这里。
            tracing::warn!("compat.toml: 合成结果不合法，按无规则处理: {e}");
            None
        }
    };
    ResolvedRule {
        rule,
        window_matches,
        entry_window_matches,
    }
}

/// 两个升序下标表按序归并（不去重：两表不相交）。
fn merge_sorted<'a>(a: &'a [usize], b: &'a [usize]) -> impl Iterator<Item = usize> + 'a {
    let (mut a, mut b) = (a.iter().copied().peekable(), b.iter().copied().peekable());
    std::iter::from_fn(move || match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if x < y => a.next(),
        (Some(_), Some(_)) => b.next(),
        (Some(_), None) => a.next(),
        (None, _) => b.next(),
    })
}

/// [`AppCompat::resolve`] 一级缓存（按 (进程, 类名)）的容量，满了按 LRU 淘汰最久没用的一条。
pub(crate) const RESOLVE_CACHE_CAP: usize = 64;

/// 每个一级条目下「命中的标题规则集合 → 结果」的容量，满了清空这一条目的二级表。
/// 同一 (进程, 类名) 下标题规则通常寥寥几条，能组合出的命中集合远少于此。
const TITLE_HITS_CAP: usize = 16;

/// 一级缓存键：规范化后的 (进程名 ASCII 小写, 类名逐字符折叠)。
type ClassKey = (String, String);

/// 一级缓存条目：某个 (进程, 类名) 下与标题无关的那部分解析。
///
/// 标题随浏览器标签页、编辑器文件频繁变化，而类名不变：把「进程 + 类名都命中」的候选在这里
/// 一次拆好，标题变化时只在 `title_idx` 那几条里比标题、再与基底合成。
#[derive(Debug)]
struct ClassEntry {
    /// 不带标题条件、进程 + 类名都命中的候选下标（升序 = 合成顺序）。
    base_idx: Vec<usize>,
    /// 带标题条件、进程 + 类名都命中的候选下标（升序）。为空 ⇒ 标题与结果无关（零额外开销）。
    title_idx: Vec<usize>,
    /// 一条标题规则都不命中时的结果（= 只叠 `base_idx`）。
    base: std::sync::Arc<ResolvedRule>,
    /// 二级：命中的标题规则下标（升序）→ 合成结果。标题千变万化，命中集合只有寥寥几种。
    by_hits: HashMap<Vec<usize>, std::sync::Arc<ResolvedRule>>,
    /// LRU 时间戳（越大越近）。
    used: u64,
}

/// 解析结果缓存（两级，见 [`ClassEntry`]）。克隆出来的是空缓存：重载规则表（新建 / 克隆
/// [`AppCompat`]）即清空。
#[derive(Default)]
struct ResolveCache(std::sync::Mutex<ResolveCacheInner>);

#[derive(Default)]
struct ResolveCacheInner {
    entries: HashMap<ClassKey, ClassEntry>,
    tick: u64,
}

impl Clone for ResolveCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for ResolveCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolveCache")
    }
}

/// 所有应用兼容性规则 + 运行时查找表。
#[derive(Debug, Clone, Default)]
pub struct AppCompat {
    /// 参与合成的 `[[apps]]` 规则，**已按合成顺序稳定排好**（见 [`RuleId::compose_order`]）。
    candidates: Vec<Candidate>,
    /// `candidates` 按进程分桶的下标（各桶内升序 = 合成顺序）：不限进程的一桶，外加
    /// 小写进程名 → 该进程的各条。一个焦点窗口只可能命中这两桶，解析时只扫它们。
    any_process_idx: Vec<usize>,
    process_idx: HashMap<String, Vec<usize>>,
    /// 小写进程名 → 无窗口上下文时的合成结果（`"*"` ⊕ 本进程），供 [`Self::get_rule`] 借出引用。
    by_process: HashMap<String, AppCompatRule>,
    /// 无窗口上下文、没有本进程规则时的合成结果（只有 `"*"`）。
    wildcard: Option<AppCompatRule>,
    /// HostRender 白名单（原始大小写）。
    host_render: Vec<String>,
    /// 有启用的、不限进程的带标题条件规则。见 [`Self::process_may_match_title`]。
    title_any_process: bool,
    /// 有启用的带标题条件规则的进程（小写）。
    title_processes: std::collections::HashSet<String>,
    cache: ResolveCache,
    /// 小写进程名 → 该进程允许重算初始模式的窗口类名集合（小写）。
    /// **进程不在表内 = 不受限制**（绝大多数应用走这条路，零行为变化）。
    mode_scope: HashMap<String, std::collections::HashSet<String>>,
    /// 小写进程名 → 该进程的上屏换行形式。
    /// **进程不在表内 = 跟随全局**（绝大多数应用走这条路）。
    commit_newline: HashMap<String, NewlineStyle>,
}

/// 序列化中间体：承载 TOML 的两个顶层数组表，避免把 `lookup` 暴露给 TOML。
///
/// `pub(crate)` 只为让 `value_domain_guard` 守门元测试够得着，不要当成对外类型。
#[cfg(test)]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub(crate) struct AppCompatFile {
    #[serde(default)]
    pub(crate) apps: Vec<AppCompatRule>,
    /// 对应 `[[initial_mode_scope]]` 段。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) initial_mode_scope: Vec<InitialModeScopeRule>,
    /// 对应 `[[commit_newline]]` 段。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) commit_newline: Vec<CommitNewlineRule>,
}

impl AppCompat {
    /// 从一组规则构建（含查找表）。作用域清单为空 ⇒ 所有进程都不受限。
    pub fn from_rules(apps: Vec<AppCompatRule>) -> Self {
        Self::from_parts(apps, Vec::new())
    }

    /// 从两段规则构建（含查找表）。换行清单为空 ⇒ 所有进程都跟随全局。
    ///
    /// 结构体经序列化回到键值再参与合成：省略的 `None` / `false` 都按「没写」处理。
    pub fn from_parts(apps: Vec<AppCompatRule>, scope: Vec<InitialModeScopeRule>) -> Self {
        let rows: Vec<Obj> = apps
            .iter()
            .filter_map(|r| match serde_json::to_value(r) {
                Ok(serde_json::Value::Object(o)) => Some(o),
                _ => None,
            })
            .collect();
        Self::from_app_rows(&rows, scope)
    }

    /// 由已跨层叠好的 `[[apps]]` 原始行构建。
    fn from_app_rows(rows: &[Obj], scope: Vec<InitialModeScopeRule>) -> Self {
        let mut c = AppCompat::default();
        c.build_apps(rows);
        c.build_mode_scope(scope);
        c
    }

    /// 补上 `[[commit_newline]]` 那一段。
    ///
    /// 做成链式而不是给 [`Self::from_parts`] 加第三个参数：那个签名已有跨 crate 的
    /// 调用方，加参数要改一圈与本特性无关的代码。
    pub fn with_commit_newline(mut self, rules: Vec<CommitNewlineRule>) -> Self {
        self.commit_newline = rules
            .into_iter()
            .filter(|r| !r.process.is_empty())
            // `style` 没写或认不出 ⇒ 这条规则什么都没说，等价于不存在（跟随全局）。
            .filter_map(|r| Some((r.process.to_ascii_lowercase(), r.style?)))
            .collect();
        self
    }

    /// 该进程上屏时换行用什么表达；**没配返回 `None` = 跟随全局**。
    ///
    /// 返回 `Option` 而不是回落默认档，理由同 [`FirstShowMode::from_config`]。
    pub fn commit_newline_for(&self, process_name: &str) -> Option<NewlineStyle> {
        if process_name.is_empty() {
            return None;
        }
        self.commit_newline
            .get(&process_name.to_ascii_lowercase())
            .copied()
    }

    /// 该焦点窗口是否落在「允许重算 per-app 初始模式」的作用域内。
    ///
    /// 返回 false 表示调用方应**保持现状**（不重算初始模式、不推进模式归属）。
    ///
    /// - 该进程没配作用域 ⇒ true；
    /// - 配了且窗口类命中 ⇒ true；
    /// - 配了但不命中，含窗口类为空 ⇒ false（拿不到窗口标识时也该别重算，见 `InitialModeScopeRule`）。
    pub fn initial_mode_applies_to_window(&self, process_name: &str, window_class: &str) -> bool {
        match self.mode_scope.get(&process_name.to_ascii_lowercase()) {
            None => true,
            Some(set) => {
                !window_class.is_empty() && set.contains(&window_class.to_ascii_lowercase())
            }
        }
    }

    /// 按进程名（不区分大小写）查规则，没有窗口上下文：等于 `resolve` 以空类名、空标题调用，
    /// 即 `"*"` 规则 ⊕ 本进程的纯进程规则（带窗口条件的规则一律不命中）。都没有返回 `None`。
    ///
    /// **进程名为空返回 `None`**（空名不吃通配）：不知道是谁时保持历史行为，与
    /// [`Self::composition_placeholder_for`] 同一口径。要显式取通配那条传 [`ANY_PROCESS`]。
    pub fn get_rule(&self, process_name: &str) -> Option<&AppCompatRule> {
        if process_name.is_empty() {
            return None;
        }
        self.by_process
            .get(&process_name.to_ascii_lowercase())
            .or(self.wildcard.as_ref())
    }

    /// 按焦点窗口上下文解析 `[[apps]]` 规则：命中的各条按具体程度分级合成，越具体越后叠
    /// （后叠的赢）。顺序见 [`RuleId::compose_order`]；合成在原始键值上逐行 `compose`，最后才
    /// 反序列化（结构体上分不开裸 bool 的「没写」与 `false`）。被禁用的规则不参与；行内 `unset`
    /// 跨级生效（取消更低级规则设下的字段 = 本窗口回到全局默认）。
    ///
    /// 缓存分两级（重载规则表即清空）：一级按规范化的 (进程名, 类名)，存「进程 + 类名都命中」的
    /// 候选拆成的基底与待比标题的子集（LRU，容量 [`RESOLVE_CACHE_CAP`]）；二级按命中的标题规则
    /// 集合存合成结果。标题频繁变化（切标签页 / 文件）只在子集里比标题，不重扫全表、不重合成；
    /// 没有标题规则时不折叠标题，与只按类名缓存等价。
    ///
    /// **进程名为空返回空结果**（空名不吃通配，窗口条件规则也不命中），口径同 [`Self::get_rule`]。
    pub fn resolve(&self, ctx: &WindowCtx) -> std::sync::Arc<ResolvedRule> {
        if ctx.process.is_empty() {
            return std::sync::Arc::default();
        }
        self.with_class_entry(ctx, |entry, hits| {
            if hits.is_empty() {
                return entry.base.clone();
            }
            if let Some(r) = entry.by_hits.get(&hits) {
                return r.clone();
            }
            let resolved = std::sync::Arc::new(compose_candidates(
                merge_sorted(&entry.base_idx, &hits).map(|i| &self.candidates[i]),
            ));
            if entry.by_hits.len() >= TITLE_HITS_CAP {
                entry.by_hits.clear();
            }
            entry.by_hits.insert(hits, resolved.clone());
            resolved
        })
    }

    /// 该窗口命中的标题规则（候选下标，升序）——「标题签名」。两个窗口类名相同、签名相同，
    /// [`Self::resolve`] 的结果必然相同：协调器据此判断「仅标题变化」是否真的换了生效规则，
    /// 命中集合没变就不算换窗口（不重算、不重推）。下标只在同一张表内有意义，重载即失效。
    ///
    /// 进程名为空 / 没有可能命中的标题规则时为空，不折叠标题。
    pub fn title_signature(&self, ctx: &WindowCtx) -> Vec<usize> {
        if ctx.process.is_empty() {
            return Vec::new();
        }
        self.with_class_entry(ctx, |_, hits| hits)
    }

    /// 取（必要时建）`ctx` 的一级缓存条目，连同标题命中的标题规则下标交给 `f`。
    /// 条目没有标题规则时不折叠标题，命中集合恒空。
    fn with_class_entry<R>(
        &self,
        ctx: &WindowCtx,
        f: impl FnOnce(&mut ClassEntry, Vec<usize>) -> R,
    ) -> R {
        use crate::compat_overlay::fold_char;
        let key: ClassKey = (
            ctx.process.to_ascii_lowercase(),
            ctx.class.chars().map(fold_char).collect(),
        );
        let mut cache = self.cache.0.lock().unwrap_or_else(|e| e.into_inner());
        cache.tick += 1;
        let tick = cache.tick;
        if !cache.entries.contains_key(&key) {
            if cache.entries.len() >= RESOLVE_CACHE_CAP
                && let Some(oldest) = cache
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.used)
                    .map(|(k, _)| k.clone())
            {
                cache.entries.remove(&oldest);
            }
            let entry = self.class_entry(&key);
            cache.entries.insert(key.clone(), entry);
        }
        let entry = cache.entries.get_mut(&key).expect("上面刚确保条目存在");
        entry.used = tick;
        let hits = if entry.title_idx.is_empty() {
            Vec::new()
        } else {
            let title: Vec<char> = ctx.title.chars().map(fold_char).collect();
            entry
                .title_idx
                .iter()
                .copied()
                .filter(|&i| cond_matches(&self.candidates[i].title_pat, &title))
                .collect()
        };
        f(entry, hits)
    }

    /// 建一级缓存条目：把可能命中该 (进程, 类名) 的候选按「进程 + 类名都命中」筛出，再按有无
    /// 标题条件拆成基底与待比标题两组。
    fn class_entry(&self, key: &ClassKey) -> ClassEntry {
        let class: Vec<char> = key.1.chars().collect();
        let own: &[usize] = self
            .process_idx
            .get(&key.0)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (mut base_idx, mut title_idx) = (Vec::new(), Vec::new());
        for i in merge_sorted(&self.any_process_idx, own) {
            let c = &self.candidates[i];
            if !c.matches_process_class(&key.0, &class) {
                continue;
            }
            if c.title_pat.is_empty() {
                base_idx.push(i);
            } else {
                title_idx.push(i);
            }
        }
        let base = std::sync::Arc::new(compose_candidates(
            base_idx.iter().map(|&i| &self.candidates[i]),
        ));
        ClassEntry {
            base_idx,
            title_idx,
            base,
            by_hits: HashMap::new(),
            used: 0,
        }
    }

    /// 是否存在启用的带 `title` 条件的 `[[apps]]` 规则。
    ///
    /// 按进程的判断见 [`Self::process_may_match_title`]（推给 DLL 的开关用那个）。
    pub fn has_title_rules(&self) -> bool {
        self.title_any_process || !self.title_processes.is_empty()
    }

    /// 该进程的窗口**可能**命中标题规则：有不限进程的标题规则，或本进程有标题规则。
    ///
    /// 推给 DLL 的「采不采窗口标题」开关按进程取它（`CONFIG_KEY_COMPAT_TITLE_MATCH`），协调器
    /// 也据此决定留不留焦点事件带来的标题：用不上的进程一次标题都不取、不留。进程名为空 = false。
    pub fn process_may_match_title(&self, process: &str) -> bool {
        !process.is_empty()
            && (self.title_any_process
                || self.title_processes.contains(&process.to_ascii_lowercase()))
    }

    /// 一级缓存（按 (进程, 类名)）当前的条目数。
    #[cfg(test)]
    pub(crate) fn resolve_cache_len(&self) -> usize {
        self.cache.0.lock().unwrap().entries.len()
    }

    /// 右键菜单为焦点窗口写回 `fields` 时该写到哪条规则。
    ///
    /// 菜单显示的是焦点窗口的生效值；它若来自「进程名 + 窗口条件」的规则，写纯进程键会被那条压住
    /// （点了没反应）。所以取命中的规则里**比纯进程键更具体**（T3：具体进程 + 窗口条件）、且自己
    /// 写了或 `unset` 了其中任一字段的最后叠的那条——它就是这些字段生效值的出处，写它必然生效。
    /// 返回 `None` = 写纯进程键（照旧）：生效值来自纯进程 / `"*"` / 仅窗口条件的规则时，纯进程键
    /// 叠在它们之后，写它就够了；而改仅窗口条件那条（如 `Chrome_WidgetWin_*`）会波及别的应用。
    pub fn menu_writeback_target(&self, ctx: &WindowCtx, fields: &[&str]) -> Option<RuleId> {
        if ctx.process.is_empty() {
            return None;
        }
        let folded = FoldedCtx::new(ctx);
        self.candidates
            .iter()
            .rev()
            .find(|c| {
                let unset = unset_of(&c.fields);
                !c.id.is_any_process()
                    && c.id.has_window_condition()
                    && c.matches(&folded)
                    && fields
                        .iter()
                        .any(|k| c.fields.contains_key(*k) || unset.iter().any(|u| u == k))
            })
            .map(|c| c.id.clone())
    }

    /// 该进程组合区用哪个占位字符。唯一的查表入口，协调器发占位与给 DLL 下发兜底占位都走这里。
    ///
    /// 走统一合成（[`Self::get_rule`]）：`process = "*"` 通配对它生效，本进程显式写 `space`
    /// 压过通配，都没配 = 空格。进程名未知（空串）⇒ 空格，不吃通配：不知道是谁时保持历史行为。
    pub fn composition_placeholder_for(&self, process_name: &str) -> PlaceholderChar {
        if process_name.is_empty() {
            return PlaceholderChar::default();
        }
        self.get_rule(process_name)
            .and_then(|r| r.composition_placeholder)
            .unwrap_or_default()
    }

    /// 该进程联想态的占位字符：`assoc_placeholder` 优先，没配跟随 [`Self::composition_placeholder_for`]。
    pub fn assoc_placeholder_for(&self, process_name: &str) -> AssocPlaceholder {
        if process_name.is_empty() {
            return AssocPlaceholder::default();
        }
        let rule = self.get_rule(process_name);
        rule.as_ref()
            .and_then(|r| r.assoc_placeholder)
            .or_else(|| {
                rule.as_ref()
                    .and_then(|r| r.composition_placeholder)
                    .map(AssocPlaceholder::from)
            })
            .unwrap_or_default()
    }

    /// 现算 HostRender 白名单：`host_render = true` 的进程名（原始大小写）。
    ///
    /// 只收**具体进程、无窗口条件**的规则，看的是那条规则自己的值（不与 `"*"` 合成）：白名单是
    /// 按进程下发给 bridge 的，`"*"` 与窗口条件在这里都表达不出来。
    ///
    /// 供 `HostRenderManager::set_whitelist` 消费；调用方须按事件源 PID 直查，
    /// 不得经 `ActiveCompat` 全局焦点槽缓存，理由见 [`AppCompatRule::host_render`]。
    pub fn host_render_processes(&self) -> Vec<String> {
        self.host_render.clone()
    }

    /// 由已跨层叠好的 `[[apps]]` 原始行建合成用的候选表与各查找表。
    fn build_apps(&mut self, rows: &[Obj]) {
        let mut candidates = Vec::new();
        self.host_render.clear();
        for r in rows.iter().filter(|r| !is_disabled(r)) {
            // 作废的行（身份认不出，见 `rule_id`）不套给任何人。
            let Some(id) = rule_id("apps", r) else {
                continue;
            };
            // 单行结构性失败只丢这一行并留 WARN，不牵连其它规则（与 `materialize` 一致）。
            let mut solo = r.clone();
            solo.remove("unset");
            let rule =
                match serde_json::from_value::<AppCompatRule>(serde_json::Value::Object(solo)) {
                    Ok(rule) => rule,
                    Err(e) => {
                        tracing::warn!("compat.toml: 规则 {id:?} 结构不合法，已跳过: {e}");
                        continue;
                    }
                };
            if !id.is_any_process() && !id.has_window_condition() && rule.host_render {
                self.host_render.push(rule.process.clone());
            }
            let mut fields = r.clone();
            for k in ["process", "class", "title"] {
                fields.remove(k);
            }
            candidates.push(Candidate {
                class_pat: id.class.chars().collect(),
                title_pat: id.title.chars().collect(),
                id,
                fields,
                process: rule.process,
            });
        }
        // 稳定排序：同一合成级里平手的，保持 `rows` 的顺序，即该身份首次出现的层
        // （出厂 < data_custom < 用户）与行序（见 `compat_overlay::overlay`）。
        candidates.sort_by_key(|c| c.id.compose_order());

        let mut any_process_idx = Vec::new();
        let mut process_idx: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, c) in candidates.iter().enumerate() {
            if c.id.is_any_process() {
                any_process_idx.push(i);
            } else {
                process_idx.entry(c.id.process.clone()).or_default().push(i);
            }
        }

        // 无窗口上下文时命中的只有「仅 `*`」（T0）与本进程的纯进程规则（T2）；排序后 T0 全在
        // T2 之前，故「T0 各条 ⊕ 本进程 T2 各条」就是合成顺序。按进程分桶各合成一次，
        // 而不是对每个进程再全表扫一遍（那是 O(N²)）。
        let plain = |c: &&Candidate| !c.id.has_window_condition();
        let t0: Vec<&Candidate> = any_process_idx
            .iter()
            .map(|&i| &candidates[i])
            .filter(plain)
            .collect();
        self.wildcard = compose_candidates(t0.iter().copied()).rule;
        self.by_process = process_idx
            .iter()
            .filter_map(|(name, idx)| {
                let own: Vec<&Candidate> =
                    idx.iter().map(|&i| &candidates[i]).filter(plain).collect();
                if own.is_empty() {
                    return None;
                }
                let rule = compose_candidates(t0.iter().copied().chain(own)).rule?;
                Some((name.clone(), rule))
            })
            .collect();
        let titled = candidates.iter().filter(|c| !c.title_pat.is_empty());
        self.title_any_process = titled.clone().any(|c| c.id.is_any_process());
        self.title_processes = titled
            .filter(|c| !c.id.is_any_process())
            .map(|c| c.id.process.clone())
            .collect();
        self.candidates = candidates;
        self.any_process_idx = any_process_idx;
        self.process_idx = process_idx;
    }

    fn build_mode_scope(&mut self, rules: Vec<InitialModeScopeRule>) {
        self.mode_scope = rules
            .into_iter()
            .filter(|r| !r.process.is_empty())
            .map(|r| {
                (
                    r.process.to_ascii_lowercase(),
                    r.classes
                        .iter()
                        .map(|c| c.to_ascii_lowercase())
                        .collect::<std::collections::HashSet<_>>(),
                )
            })
            .collect();
    }

    /// 加载兼容规则：系统层（`{data_dir}/compat.toml`）+ 定制层
    /// （`data_custom/compat.toml`）+ 用户层覆盖（`{user_dir}/compat.toml`）。
    /// 任一文件缺失/解析失败均静默跳过。
    ///
    /// 定制层由 [`crate::config::Config::custom_data_dir`] 决定（清单在场才有），不经参数传入：本函数
    /// 有五个跨 crate 调用点，而定制层的位置是进程级事实、不随调用方的 `data_dir` 变。
    /// 测试需要指定定制层时用 [`Self::load_layered`]。
    pub fn load(data_dir: Option<&Path>, user_dir: Option<&Path>) -> Self {
        Self::load_layered(
            data_dir,
            crate::config::Config::custom_data_dir().as_deref(),
            user_dir,
        )
    }

    /// 三层显式加载，层序 `data < data_custom < user`：每一层都是相对下一层的差异，
    /// 在**原始键值**上逐字段叠加（见 [`crate::compat_overlay`]），最后一次性变成运行时结构体。
    ///
    /// 三段（`[[apps]]` / `[[initial_mode_scope]]` / `[[commit_newline]]`）各自独立叠加，
    /// 互不牵连：为某进程写 `[[apps]]` 规则不会让更低层给它配的作用域或换行规则消失。
    pub fn load_layered(
        data_dir: Option<&Path>,
        custom_dir: Option<&Path>,
        user_dir: Option<&Path>,
    ) -> Self {
        let mut raw = Raw::default();
        for dir in [data_dir, custom_dir, user_dir].into_iter().flatten() {
            if let Some(layer) = load_raw(&dir.join(COMPAT_FILE_NAME)) {
                raw = overlay_raw(raw, &sanitize_raw(&layer).0);
            }
        }
        Self::from_raw(&raw)
    }

    /// 测试用：把一份 compat.toml 文本当作**唯一一层**，走与生产 `load_layered` 完全相同的路径
    /// （解析 → 清理 → 叠加 → 反序列化）。容错类测试必须走它而不是直接反序列化结构体，
    /// 否则测的不是生产路径。
    #[cfg(test)]
    pub(crate) fn from_single_layer_text(text: &str) -> Self {
        let raw = parse_raw(text).expect("测试夹具必须是合法 TOML");
        Self::from_raw(&overlay_raw(Raw::default(), &sanitize_raw(&raw).0))
    }

    /// 叠加好的原始键值 → 运行时表（跳过禁用行、逐行容错反序列化；`[[apps]]` 保留原始键值供合成）。
    pub(crate) fn from_raw(raw: &Raw) -> Self {
        // 没有 `classes` 的作用域规则没有意义，直接作废：反序列化会把它变成空清单，而空清单的含义
        // 是「该进程的初始模式在任何窗口上都不重算」——写错 / 漏写一个字段不能静默变成「全挡」。
        // （想全挡要显式写 `classes = []`。）
        let scopes: Vec<_> = raw
            .initial_mode_scope
            .iter()
            .filter(|r| r.contains_key("classes"))
            .cloned()
            .collect();
        Self::from_app_rows(&raw.apps, materialize::<InitialModeScopeRule>(&scopes))
            .with_commit_newline(materialize::<CommitNewlineRule>(&raw.commit_newline))
    }
}

/// 一层原始键值参与叠加之前的清理：无效的值当成「没写」剔掉（返回清理后的副本与每处剔除的说明）。
///
/// 必须在叠加**之前**做：错值若先参与叠加、事后才被容错回落成 `None`，会把下层的好值一起盖没。
pub(crate) fn sanitize_raw(raw: &Raw) -> (Raw, Vec<crate::compat_overlay::Problem>) {
    let (apps, mut report) = sanitize::<AppCompatRule>("apps", &raw.apps);
    let (initial_mode_scope, r2) =
        sanitize::<InitialModeScopeRule>("initial_mode_scope", &raw.initial_mode_scope);
    let (commit_newline, r3) = sanitize::<CommitNewlineRule>("commit_newline", &raw.commit_newline);
    report.extend(r2);
    report.extend(r3);
    (
        Raw {
            apps,
            initial_mode_scope,
            commit_newline,
            ..Default::default()
        },
        report,
    )
}

/// 读一层 compat.toml 的原始键值；文件不存在返回 `None`（不告警），读取 / 解析失败也返回
/// `None` 但**留 WARN**：整份跳过意味着这一层所有规则一起失效，必须在日志里留下痕迹。
pub(crate) fn load_raw(path: &Path) -> Option<Raw> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("compat.toml 读取失败，整份跳过: {}: {e}", path.display());
            }
            return None;
        }
    };
    match parse_raw(&text) {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::warn!("compat.toml 解析失败，整份跳过: {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
#[path = "app_compat_commit_newline_rule_tests.rs"]
mod commit_newline_rule_tests;

#[cfg(test)]
#[path = "app_compat_newline_style_tests.rs"]
mod newline_style_tests;

#[cfg(test)]
#[path = "app_compat_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_compat_disabled_rule_tests.rs"]
mod disabled_rule_tests;

/// 分层叠加语义（整体原则：每一层都是相对下一层的差异，逐字段叠加）与菜单写入路径。
#[cfg(test)]
#[path = "app_compat_layering_tests.rs"]
mod layering_tests;

/// 按窗口上下文分级合成（`docs/design/compat-window-match.md`「叠加顺序」）。
#[cfg(test)]
#[path = "app_compat_resolve_tests.rs"]
mod resolve_tests;

/// 旧文件（没有 class / title）在新实现下的合成结果必须与按进程名认「同一条」的旧实现**逐字节相同**。
///
/// 这里留一份旧实现的精简副本（只保留按进程名 coalesce / overlay 的那几行）作对照：
/// 新实现改的正是「认同一条」的键，拿新实现自己的函数当基准就成了自证。
#[cfg(test)]
#[path = "app_compat_legacy_equivalence_tests.rs"]
mod legacy_equivalence_tests;
