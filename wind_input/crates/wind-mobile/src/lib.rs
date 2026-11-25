//! wind-mobile：移动端形态的核心门面（headless Coordinator 的收口）。
//!
//! 平台绑定层（Android 的 UniFFI 包装、将来 iOS 的 Swift 包装）只依赖本 crate 这一条边，
//! 核心内部重构由这里吸收，不打断另一个仓、另一条 CI 上的平台构建。
//!
//! # 边界划在哪
//!
//! 判据是「换成 iOS 还成不成立」：
//!
//! - 留在这里（与绑定框架无关）：四层配置加载、已装方案扫描与激活校正、`preedit_display`
//!   覆盖、`SessionEvent` 泵线程、预热闸门、吃键判定转发、按键翻译与英文自动空格（`touch.rs`）。
//! - 归平台仓：UniFFI/Swift 的类型投影、日志接到 logcat/oslog、`cdylib` 产物形态、绑定生成器。
//!
//! 所以本 crate 的依赖表里不该出现任何绑定框架，否则第二个平台只能复制粘贴。
//!
//! - 同样留在这里：数据同步门面 `sync.rs`（`MobileSync`，JSON 进出、进程内跑同步 Runner，
//!   重载直接调核心）。绑定层只做字符串透传与线程调度，不碰同步逻辑。
//!
//! # 通道形状（对应 WindInputAndroid docs/architecture.md §3.1）
//!
//! - 正向：软键与物理键经 [`MobileCore::press`]（[`PressKey`] + [`KeySource`]）→ 翻译 VK、
//!   自动空格 → [`MobileCore::key_down`] → `Coordinator::handle_key_event` → 同步 [`KeyOutcome`]
//!   （上屏文本/组合区走按键返回值，与 TSF 宿主同一条主输入路；`ops` 已含补的空格与标点换位，
//!   宿主只按序执行）。直出场景（密码框）宿主不得调用 `press`
//! - 反向：`Receiver<SessionEvent>`（`View`/`Hide`/`Status`/`Notice`）泵线程 →
//!   [`MobileEventSink`] 回调（候选/状态/提示）
//! - 候选：推送帧 [`CandidateFrame`] 只给当页（够画编码栏与页码），列表由宿主按窗口
//!   [`MobileCore::candidates`] 拉、按绝对下标 [`MobileCore::select_candidate`] 选，
//!   不受页内数字键只能选前 9 个的限制。桌面鼠标点选走 push 通道上屏，headless 没有消费端，
//!   所以这里返回编辑指令流，与 `key_down` 同一种形状。
//!
//! # 工具栏与功能菜单的呈现不属于这一层
//!
//! 桌面功能菜单树大半在手机上无意义，此处只提供：
//!
//! - [`MobileCommand`]：能调到引擎的哪些开关（展示什么、怎么排、叫什么由宿主定）；
//! - [`InputStatus`]：引擎当前的模式真值（中英/标点/全半角/简繁/方案短称）。
//!
//! 状态必须由引擎推送而不是宿主自己记：方案切换、密码框强制英文都发生在引擎内部，
//! 宿主自记一份必然漂移。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod config_api;
mod schema_settings;
mod scheme_form;
pub mod sync;
mod touch;

pub use schema_settings::{SchemaSetting, SchemaSettingGroup, SchemaSettingKind, SchemaSettings};
pub use scheme_form::SchemeForm;
pub use touch::{KeySource, PressKey};

use wind_bridge::handler::KeyAction as BridgeKeyAction;
use wind_bridge::handler::{FocusData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::{CompositionView, Coordinator, InputProfile, SessionEvent, SessionStatus};
use wind_host::{KeyProbe, Modifiers};
use wind_ipc::protocol::{EVENT_KEY_DOWN, FocusLostReason, caret_source};
use wind_ui_types::{CandidateOp, MenuCmd, MenuItemSpec, MenuKind, UiEvent};

/// 宿主直接消费的接缝类型，从 [`wind_host`] 原样透出。
///
/// 不另立投影：多包一层就多一张要跟核心同步的映射表，漏接一个变体时那个动作在移动端
/// 静默消失。
pub use wind_host::{EditOp, KeyOutcome, TimingHint};

/// 候选窗口与它的核心侧硬上限，从协调器原样透出。
///
/// [`MAX_WINDOW`] 跟着透出：`limit` 超过它会被静默截断，宿主按自记的数字判断「还有没有更多」
/// 会在边界上反复拉空窗口。
pub use wind_coordinator::candidate_pull::{CandidateWindow, MAX_WINDOW, TextSegment};
/// 展开页「首单元读法」的数据类型，见 [`MobileCore::leading_readings`]。
pub use wind_coordinator::leading_reading::LeadingReading;
pub use wind_store::stats_report::StatsSummary;
/// 方案包导入 / 预览 / 删除的实现（与桌面同一份）。绑定层经此引用，不另开一条通往核心的依赖。
pub use wind_transfer;

/// 候选帧：编码栏与候选栏一次渲染所需的全部数据。
///
/// 用结构体而非长参数列表：加字段时不必改 trait 方法签名、连带宿主实现返工。
#[derive(Debug, Clone)]
pub struct CandidateFrame {
    /// 编码区串（五笔码 / 拼音串），编码栏正文
    pub preedit: String,
    /// 编码区插入符位置：`preedit` 内的字节偏移（恒在字符边界）
    pub preedit_caret: usize,
    /// 模式指示文本（五/拼/英/符…），编码栏左侧角标；空 = 不显示
    pub mode_label: String,
    /// 是否在「打字中」：有组合串且不是联想态。宿主据此决定提交/退格等的语义
    pub composing: bool,
    /// 当前页是否有候选。候选文本不在帧里，宿主按需经 `candidates` 拉取
    pub has_candidates: bool,
    /// 键盘选中项（页内下标），空格上屏目标
    pub selected: usize,
    /// 当前页（1 起）
    pub page: usize,
    /// 总页数
    pub total_pages: usize,
}

/// 引擎当前模式真值。是数据不是界面：工具栏怎么画由宿主决定。
#[derive(Debug, Clone)]
pub struct InputStatus {
    pub chinese_mode: bool,
    /// 键是否按拉丁字母直出：英文方案、中文模式关闭、或不可输入（密码框等）
    pub latin_input: bool,
    /// 模式主字，最多 2 个字符。中文态取方案的 `[schema] icon_label`（"五"/"拼"），
    /// 非中文态取 `[ui.labels]`（出厂 "英"/"A"，用户可配）。
    pub icon_label: String,
    pub full_width: bool,
    pub chinese_punct: bool,
    pub s2t_enabled: bool,
    /// 是否该展示简繁开关（用户未启用简繁功能时为 false）
    pub s2t_shown: bool,
}

/// 输入方案条目（方案选择器的数据源）
#[derive(Debug, Clone)]
pub struct SchemaEntry {
    /// 方案 id（wubi86 / pinyin …）
    pub id: String,
    /// 显示名（方案文件 `[schema] name`）
    pub name: String,
    /// 短称（"五"/"拼"）
    pub icon_label: String,
    pub active: bool,
}

/// 一个可选主题。
#[derive(Debug, Clone)]
pub struct ThemeEntry {
    /// 主题 id（目录名），即写回 [`MobileCore::set_theme`] 的值
    pub id: String,
    /// 显示名（`[meta] name`），如「清风·蓝」
    pub name: String,
    pub active: bool,
}

/// 一个求值后的语义色。
///
/// `${var}` 递归与 `{light, dark}` 变体已在核心侧展开，这里是终值；宿主切明暗时重新取一次即可。
#[derive(Debug, Clone)]
pub struct PaletteColor {
    /// 语义名：`bg` / `surface` / `text` / `accent_soft` / `toolbar_background` …
    pub name: String,
    /// `0xAARRGGBB`（Android `Color` 与 iOS `UIColor(rgb:)` 都吃这个布局）
    pub argb: u32,
}

/// 一个双拼布局（`schemas` 目录下 `shuangpin` 子目录里的 toml）。
#[derive(Debug, Clone)]
pub struct LayoutEntry {
    /// 文件名 stem，即写回 [`MobileCommand::SetShuangpinLayout`] 的值
    pub id: String,
    /// 显示名（布局文件 `[meta].name`），如「小鹤双拼」
    pub name: String,
    pub active: bool,
}

/// 某一天的输入字数（设置页「统计」的每日图）。
#[derive(Debug, Clone, PartialEq)]
pub struct DailyTotal {
    /// `YYYY-MM-DD`
    pub date: String,
    pub total: u32,
    pub chinese: u32,
    pub english: u32,
}

/// 一条用户词（设置页「词库」列表项）。
#[derive(Debug, Clone, PartialEq)]
pub struct UserWord {
    /// 带音节空格的显示码（`ni hao`）；码表方案、无边界信息的旧词没有空格。
    /// 原样回传给 [`MobileCore::user_word_remove`] 即可删掉——拆回扁平码由核心做。
    pub code: String,
    /// 真实文本（存储域，不做桌面设置页那层 `\n` 转义——移动端直接显示原文）
    pub text: String,
    pub weight: i32,
}

/// 一条命令短语（设置页「命令短语」列表项）：系统与用户短语的叠加视图。
#[derive(Debug, Clone, PartialEq)]
pub struct Phrase {
    pub code: String,
    /// 真实文本（存储域，模板原文如 `$Y年$M月$D日`；移动端直接显示原文）
    pub text: String,
    pub enabled: bool,
    /// 出厂的系统短语（只能开关，删不掉）；否则是用户自己加的
    pub is_system: bool,
    /// 打这个编码时候选上显示的样子（日期按此刻展开、符号组显示组名）；求值落空为空串。
    /// 列表里只给模板原文（`$Y年$M月$D日`），普通用户看不懂这条是干什么的
    pub preview: String,
}

/// 一条短语打出来在候选上的样子：只放这一条建层、查它的编码，前几个候选用「 · 」连起来
/// （`$SS` 组打全码时展开成组里的各项）——与输入时同一条求值路径，不另写一份展开规则。
/// 无宿主能力（剪贴板、反查）的求值落空，返回空串，列表回落显示原文。
fn phrase_preview(code: &str, text: &str, now: chrono::DateTime<chrono::Local>) -> String {
    let layer = wind_phrase::PhraseLayer::from_records([wind_phrase::PhraseSeed {
        code: code.to_string(),
        text: text.to_string(),
        weight: 0,
        position: 0,
        is_system: false,
        category: String::new(),
    }]);
    let hits = layer
        .lookup_at(
            code,
            now,
            &[],
            &wind_phrase::PhraseHost::empty(),
            &wind_phrase::PhraseScope::ALL,
        )
        .into_iter()
        .map(|h| h.text)
        .collect::<Vec<_>>();
    const SHOWN: usize = 4;
    let mut out = hits
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
    if hits.len() > SHOWN {
        out.push_str(" · …");
    }
    out
}

/// 用户词的一页与命中总数。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserWordPage {
    pub items: Vec<UserWord>,
    /// 命中总数（不是本页条数），分页 / 「共 N 条」用。
    pub total: u64,
}

/// 词库导入结果的一段（[`MobileCore::dict_import`]）。
///
/// 与桌面 `dict.import` 回的 JSON 逐段同义：用户词段给 `added` / `updated` / `unchanged`，
/// 其余段给 `imported`；`skipped` 是未导入的总数，`rejected` / `boundary_skipped` 是其中
/// 「为什么少」的分类（只有词条段过准入契约，其余段恒 0）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DictImportSection {
    /// `userWords` / `tempWords` / `freq` / `shadow`
    pub key: String,
    pub added: u64,
    pub updated: u64,
    pub unchanged: u64,
    /// 实际导入条数（用户词段 = added + updated + unchanged）
    pub imported: u64,
    /// 未导入的总数：解析期跳过 + 准入拒收 + 不补充而跳过
    pub skipped: u64,
    /// 拼音准入契约判为不合法、未入库的条数
    pub rejected: u64,
    /// 被拒行的样例（至多 5 条，「编码 词」）——让用户看得出是不是拿错了文件
    pub rejected_samples: Vec<String>,
    /// 程序补上了音节边界的条数
    pub boundary_filled: u64,
    /// 用户选「不补充」而跳过的条数（手机端固定补充，恒 0；留着与桌面同形）
    pub boundary_skipped: u64,
}

/// 词库导入结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DictImportResult {
    pub sections: Vec<DictImportSection>,
}

/// 方案的一个附加词库（设置页「方案 → 拼音」等二级页的开关项）。
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaDictInfo {
    /// 原样回传给 [`MobileCore::set_schema_dict_enabled`]
    pub id: String,
    /// 方案文件的 `label`（如「成语」）；发现目录自动登记、没写名字的库可能为空
    pub label: String,
    pub description: String,
    /// 当前生效的开关：用户覆盖 ?? 方案 `default_enabled` ?? 关
    pub enabled: bool,
}

/// 候选长按菜单的一项（桌面候选右键菜单在手机上的对应物）。
///
/// 只出可点的叶子：桌面菜单里的分隔线 / 标题行是绘制用的，命令项不会出现在候选菜单里；
/// 子菜单（「固定到第 N 位」）会被摊平成同级项。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateMenuItem {
    /// 文案，与桌面同一候选的右键菜单逐字一致（如「屏蔽该词」「设为生僻字」）
    pub label: String,
    /// `"move_top"` | `"move_up"` | `"move_down"` | `"pin_at_N"`（N 为 1 起的名次）| `"delete"` |
    /// `"hide_here"` | `"reset"` | `"toggle_common"` | `"copy"`。原样回传给
    /// [`MobileCore::candidate_menu_run`]。
    pub action: String,
    /// 灰显项照样列出：用户要看得出「这条不能调」，而不是以为菜单少了一项
    pub enabled: bool,
}

/// 一个已装方案（设置页「启用的方案」一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledSchema {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// 关不掉（英文：中英键切换的目标）
    pub locked: bool,
}

/// 候选提示的一段（段名 + 原始行），见 [`MobileCore::candidate_tooltip`]。
pub use wind_coordinator::handle_tooltip::CandidateTooltipSection;

/// [`CandidateMenuItem::action`] 与词条操作的对照。`copy` 不在此表——复制由宿主自己写剪贴板。
/// 「固定到第 N 位」不在表里，动作名是 `pin_at_N`（见 [`candidate_menu_action`]）。
const CANDIDATE_MENU_ACTIONS: [(&str, CandidateOp); 7] = [
    ("move_top", CandidateOp::MoveTop),
    ("move_up", CandidateOp::MoveUp),
    ("move_down", CandidateOp::MoveDown),
    ("delete", CandidateOp::Delete),
    ("hide_here", CandidateOp::HideHere),
    ("reset", CandidateOp::Reset),
    ("toggle_common", CandidateOp::ToggleCommon),
];

/// 移动端「固定到第 N 位」的 N 上限（扁平化时截掉其余子项）。
const MOBILE_PIN_AT_MAX: u8 = 9;

/// 词条操作 → 动作名。
fn candidate_menu_action(op: CandidateOp) -> Option<String> {
    if let CandidateOp::PinAt(n) = op {
        return Some(format!("pin_at_{n}"));
    }
    CANDIDATE_MENU_ACTIONS
        .iter()
        .find(|(_, o)| *o == op)
        .map(|(a, _)| (*a).to_string())
}

/// 动作名 → 词条操作（`copy` 与未知名 → None）。
fn candidate_menu_op(action: &str) -> Option<CandidateOp> {
    if let Some(n) = action.strip_prefix("pin_at_") {
        return n.parse().ok().map(CandidateOp::PinAt);
    }
    CANDIDATE_MENU_ACTIONS
        .iter()
        .find(|(a, _)| *a == action)
        .map(|&(_, op)| op)
}

/// 把桌面菜单树压成移动端的扁平列表：只留 Op 与 Copy 叶子，子菜单摊平。
fn flatten_candidate_menu(items: &[MenuItemSpec], out: &mut Vec<CandidateMenuItem>) {
    for item in items {
        let action = match item.kind {
            // 长按菜单是扁平列表，「第 N 位」只列到 9，免得一屏全是它（桌面子菜单仍到 20）。
            MenuKind::Op(CandidateOp::PinAt(n)) if n > MOBILE_PIN_AT_MAX => None,
            MenuKind::Op(op) => candidate_menu_action(op),
            MenuKind::Copy => Some("copy".to_string()),
            MenuKind::Submenu => {
                flatten_candidate_menu(&item.children, out);
                None
            }
            MenuKind::Command(_) | MenuKind::Separator | MenuKind::Label => None,
        };
        if let Some(action) = action {
            // 桌面子项「第 3 位」挂在「固定到第 N 位」下；摊平后没有父项，文案补全。
            let label = match item.kind {
                MenuKind::Op(CandidateOp::PinAt(n)) => format!("固定到第 {n} 位"),
                _ => item.label.clone(),
            };
            out.push(CandidateMenuItem {
                label,
                action,
                enabled: item.enabled,
            });
        }
    }
}

/// 宿主可下达的引擎命令。
///
/// 刻意是一张手工挑选的小表而非桌面 `MenuCmd` 的镜像：后者含截图、打开目录、
/// 诊断 HUD、语言栏图标等一批桌面专属项，暴露给移动端只会制造调不通的入口。
/// 新增移动端要用的开关时在此显式加一条。
#[derive(Debug, Clone)]
pub enum MobileCommand {
    /// 切到英文模式（中→英）
    English,
    /// 选中第 N 个方案（下标对齐 [`MobileCore::schemas`]），并回到中文模式（英→中）
    SelectSchema(usize),
    /// 中文标点 ↔ 英文标点
    TogglePunct,
    /// 全角 ↔ 半角
    ToggleWidth,
    /// 简入繁出开关
    ToggleS2t,
    /// 重载用户配置（设置页改完即时生效）
    ReloadConfig,
    /// 设置双拼布局（取值来自 [`MobileCore::shuangpin_layouts`]）。
    ///
    /// 不走 `MenuCmd`：桌面那套菜单里没有这一项（桌面在设置页里改），
    /// 而移动端没有设置页 RPC 通道，只能经本命令直达协调器。
    SetShuangpinLayout(String),
}

/// 反向通道接收端（由 SessionEvent 泵线程调用，非宿主主线程，实现方自行切回）。
pub trait MobileEventSink: Send + Sync {
    /// 候选/编码区更新
    fn on_candidates(&self, frame: CandidateFrame);
    /// 候选清空/隐藏
    fn on_hide_candidates(&self);
    /// 模式真值变化（中英/标点/全半角/简繁/方案短称）
    fn on_status(&self, status: InputStatus);
    /// 一次性提示（方案切换/模式切换/词库就绪/错误）
    fn on_toast(&self, text: String);
}

/// 输入法核心会话：一个实例对应宿主一个输入法服务的生命周期。
pub struct MobileCore {
    coord: Arc<Coordinator>,
    /// 预热进行中：泵线程据此丢弃预热产生的候选帧，不让它们闪到用户界面上
    warming: Arc<AtomicBool>,
    /// 出厂数据目录：配置快照要走与构造期同一条四层加载路径（见 [`MobileCore::config_snapshot`]）。
    data_dir: std::path::PathBuf,
    /// 刚由 [`Self::press`] / [`Self::select_candidate`] 在英文词后补过一个空格，
    /// 且此后没有别的输入动作（光标没动、没换框、没按别的键）。句末标点据此把空格挪到标点后面。
    auto_spaced: AtomicBool,
    /// 英文词后是否补空格（宿主设置项，默认开），见 [`Self::set_auto_space`]
    auto_space: AtomicBool,
    /// 触摸键盘的 `;` 是否进快捷输入（宿主设置项，默认关），见 [`Self::set_touch_quick_trigger`]
    touch_quick_trigger: AtomicBool,
}

impl MobileCore {
    /// 构造核心。
    ///
    /// - `data_dir`: 出厂数据目录（`schemas/` 所在，Android 上即 filesDir/data）
    /// - `user_root`: 用户目录（核心的「用户配置目录」：config.toml、schemas/、用户词库…，
    ///   布局同桌面；Android 上放 `Android/data/<包名>/files`，用户可经文件管理器直接编辑）
    /// - `local_root`: 本地目录（词库编译缓存与日志，可再生，不进用户视野；Android 上在私有 filesDir 下）
    /// - `fallback_schema`: 首装兜底方案 id；用户上次选过的优先
    pub fn new(
        data_dir: &Path,
        user_root: &Path,
        local_root: &Path,
        fallback_schema: &str,
        sink: Arc<dyn MobileEventSink>,
    ) -> Arc<Self> {
        mount_dirs(data_dir, user_root, local_root);

        // 四层配置加载，与桌面同一条路径（没有定制版包时 L2.5 自然缺席）。配置格式三端统一
        // 是多端同步的前提；退回 `Config::default()` 会让出厂 config.toml 的设置全部失效。
        let mut cfg = Config::load(Some(data_dir)).unwrap_or_else(|e| {
            tracing::warn!("配置加载失败，回落代码默认值: {e}");
            Config::default()
        });

        // 可选方案以 assets 里实际装了的为准，不用 config.toml 列的那份：出厂清单是桌面全量，
        // 装不出来的会成死项；而没列但装了的（如英文方案，桌面不放进 available）也要能用。
        let mut installed = scan_installed_schemas(data_dir);
        if !installed.iter().any(|s| s == fallback_schema) {
            installed.push(fallback_schema.to_string());
        }
        config_api::set_fallback_schema(fallback_schema);
        config_api::set_installed_schemas(installed.clone());
        // 启用的 = 装了的 ∩ 用户在设置里选的（没选过 = 全部），见 `config_api::apply_enabled_schemas`；
        // 重载时覆盖钩子按同一规则重算
        let available = config_api::enabled_schemas();

        // 激活方案：用户上次选的（用户层 `schema.active`）优先，`fallback_schema` 只是首装兜底，
        // 别无条件用参数覆盖，否则重启后回到兜底方案。仍要校验装了没有：用户层可能留着
        // 这一版已删掉的方案 id，照用会让引擎起不来。
        let active = if cfg.schema.active.is_empty() || !available.contains(&cfg.schema.active) {
            fallback_schema.to_string()
        } else {
            cfg.schema.active.clone()
        };
        tracing::info!("已装方案: {:?}，激活: {}", available, active);
        cfg.schema.available = available;
        cfg.schema.active = active;

        // 移动端强制改写项，与设置页快照共用同一个函数，两处才能一致。
        config_api::apply_mobile_overrides(&mut cfg);

        // 用户数据根不能传 None：那样系统短语层整段为空（见 `new_session`），词频与自造词也不落盘。
        //
        // 显式声明移动 Profile：平台差异由运行期 Profile 决定，不再靠 `cfg!(target_os)`，
        // 这样主机上的测试与 Android 产物走同一条移动分支。
        let (coord, rx) =
            Coordinator::new_session(cfg, Some(data_dir), Some(user_root), InputProfile::mobile());

        let warming = Arc::new(AtomicBool::new(true));
        spawn_session_pump(rx, sink, Arc::clone(&warming));

        // 设置页每改一项就重载一次：成功不弹「设置已更新」（失败照常提示）
        coord.set_quiet_reload_toast(true);
        // 强制改写项在每次重载配置后也要再套上（设置页每改一项就重载一次）
        coord.set_config_overlay(config_api::apply_mobile_overrides);
        // 保持启动预热开启（`set_eager_prewarm(true)` 是默认值）：`CachedDict` 写不出 wdat 缓存时
        // 未预热的方案每次查询都重解析整份 yaml，关掉就把一次性开销摊成每键开销。要改回按需加载，
        // 先确认词库能落成 wdat 缓存。惰性构建经核心的就绪契约搬到后台，避免冷启动首键阻塞。
        coord.prepare();
        // 拼音类方案：各声母第一次查询的一次性开销提前在后台付掉（见该函数说明）
        coord.prewarm_single_initials();

        Arc::new(Self {
            coord,
            warming,
            data_dir: data_dir.to_path_buf(),
            auto_spaced: AtomicBool::new(false),
            auto_space: AtomicBool::new(true),
            touch_quick_trigger: AtomicBool::new(false),
        })
    }

    /// 按下键（`vk` = wind-keys VK 码，`modifiers` 见 [`wind_host::Modifiers`]）。
    ///
    /// 抬起事件骨架期不喂（协调器按键链以 down 为主）。
    pub fn key_down(&self, vk: u32, modifiers: u32) -> KeyOutcome {
        // 用户真按键了就关闭预热窗口，否则预热完成前打的字，其候选帧会被泵线程当预热帧丢掉。
        // 预热线程末尾的 store(false) 只是幂等补充。
        self.warming.store(false, Ordering::Relaxed);

        // 吃键判定走核心的唯一真相源（`wind_coordinator::key_gate`），别在这里另写谓词，
        // 手写的一份必然与核心判据漂移。
        let probe = KeyProbe::new(vk).with_modifiers(Modifiers(modifiers));
        if !self.coord.should_handle_key(&probe) {
            // 交还宿主的键（空缓冲退格、方向键、回车…）动的是宿主里的正文，
            // 前后两次上屏就不再相邻：下一词学习的断点。光标前文也不知道变成什么了，
            // 清空等宿主报（`set_preceding_text`）
            self.coord.note_word_boundary();
            self.coord.set_preceding_text("");
            return KeyOutcome::passthrough();
        }
        let outcome =
            wind_coordinator::edit_ops::to_outcome(dispatch_key(&self.coord, vk, modifiers));
        self.coord.note_host_edits(&outcome.ops);
        outcome
    }

    /// 报告光标前的正文（不含组合区；末尾几十个字就够，多给的只留末尾）。
    ///
    /// 联想据此把逐字上屏拆开的词凑回来（五笔打「今」「天」，接「今天」的下一词）。
    /// 宿主在光标被挪动或正文被别人改过之后报（Android `onUpdateSelection` 里
    /// `getTextBeforeCursor`）、换输入框时报一次；本核心自己的上屏不必等报告，
    /// [`Self::key_down`] / [`Self::select_candidate`] 已按下发的编辑指令跟上。
    /// 读不到就报空串：联想退回只看刚上屏的那段。
    pub fn set_preceding_text(&self, text: String) {
        // 光标前不是空格了（光标被挪走、正文被改、读不到报空串）：自动补的空格不在原位，
        // 句末标点不能再换位
        if !text.ends_with(' ') {
            self.auto_spaced.store(false, Ordering::Relaxed);
        }
        self.coord.set_preceding_text(&text);
    }

    /// 焦点获得（对应 Android `onStartInput`）。
    pub fn focus_gained(&self) {
        self.auto_spaced.store(false, Ordering::Relaxed);
        // 新输入框的光标前文由宿主随后报（`set_preceding_text`），在那之前不沿用上一个框的
        self.coord.set_preceding_text("");
        let _ = self.coord.handle_focus_gained(&FocusData {
            x: 0,
            y: 0,
            height: 0,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: 1,
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: caret_source::TSF_SELECTION,
            bundle_id: String::new(),
            // 移动端没有窗口类概念，恒空；空串的语义是「不知道焦点在哪」，
            // 消费端据此保持现状（见 AppCompat::initial_mode_applies_to_window）。
            window_class: String::new(),
            window_title: String::new(),
        });
    }

    /// 焦点丢失（对应 Android `onFinishInput`）。Thread 语义 = 真正离开，清输入态。
    pub fn focus_lost(&self) {
        self.auto_spaced.store(false, Ordering::Relaxed);
        self.coord.set_preceding_text("");
        self.coord.handle_focus_lost(1, FocusLostReason::Thread);
    }

    /// 暂停 / 恢复学习（隐私 / 无痕输入）。暂停期间照常出候选、照常上屏，但这次输入
    /// 不写进任何用户数据：词频、自动造词、联想与下一词、英文造词、输入统计、各类历史。
    /// 用户在设置页 / 候选菜单里的显式操作（加词、删词、置顶）不受影响。
    ///
    /// 宿主在每次 `onStartInput`（[`Self::focus_gained`] 前后均可）按当前输入框重新声明：
    /// `EditorInfo.imeOptions` 带 `IME_FLAG_NO_PERSONALIZED_LEARNING`（无痕浏览器、私密
    /// 聊天框），或 `inputType` 是密码类（`TYPE_TEXT_VARIATION_PASSWORD` /
    /// `VISIBLE_PASSWORD` / `WEB_PASSWORD` / `TYPE_NUMBER_VARIATION_PASSWORD`）时传 `true`，
    /// 其余传 `false`。开关纯运行时、不落盘，所以「其余传 `false`」不能省——
    /// 否则上一个无痕框的暂停会一直延续到之后的普通输入框。
    pub fn set_learning_suspended(&self, on: bool) {
        self.coord.set_learning_suspended(on);
    }

    /// 下达引擎命令。状态变化经 [`MobileEventSink::on_status`] 异步回报，不在此处同步返回
    /// ——同一个变化也可能由按键或焦点事件引发，只留推送一条路，宿主才不会有两份真值。
    pub fn run_command(&self, cmd: MobileCommand) {
        let menu_cmd = match cmd {
            // 布局设置不是菜单动作，直接落协调器后返回
            MobileCommand::SetShuangpinLayout(id) => {
                self.coord.set_shuangpin_layout(&id);
                return;
            }
            MobileCommand::English => MenuCmd::SchemaEnglish,
            MobileCommand::SelectSchema(index) => MenuCmd::SchemaSelect(index),
            MobileCommand::TogglePunct => MenuCmd::TogglePunct,
            MobileCommand::ToggleWidth => MenuCmd::ToggleWidth,
            MobileCommand::ToggleS2t => MenuCmd::ToggleS2t,
            MobileCommand::ReloadConfig => MenuCmd::ReloadConfig,
        };
        let switches = matches!(
            menu_cmd,
            MenuCmd::SchemaSelect(_) | MenuCmd::SchemaEnglish | MenuCmd::ReloadConfig
        );
        self.coord
            .inject_ui_event(UiEvent::MenuAction(MenuKind::Command(menu_cmd)));
        // 换到拼音类方案（或重载重建了引擎）后同样预热；不是拼音类时函数自己直接返回
        if switches {
            self.coord.prewarm_single_initials();
        }
    }

    /// 构造时选定的输入档位。只供测试断言「门面用的是移动 Profile」。
    #[doc(hidden)]
    pub fn profile(&self) -> InputProfile {
        self.coord.profile()
    }

    /// 底层协调器。只供测试读 `debug_*` 诊断口，宿主不要碰。
    #[doc(hidden)]
    pub fn debug_coordinator(&self) -> &Coordinator {
        &self.coord
    }

    /// 可选方案列表。下标即 [`MobileCommand::SelectSchema`] 的取值
    /// ——顺序取自引擎管理器，与协调器内部的选择下标同源，宿主不要自行排序。
    pub fn schemas(&self) -> Vec<SchemaEntry> {
        let active = self.coord.active_schema_id();
        self.coord
            .schema_entries()
            .into_iter()
            .map(|(id, name, icon_label)| SchemaEntry {
                active: id == active,
                id,
                name,
                icon_label,
            })
            .collect()
    }

    /// 活跃方案此刻的手动音节分隔符字符（`'` 或 `` ` ``）；`None` = 没有，宿主不该摆「分词」键。
    ///
    /// 宿主界面的「分词」键发这个字符，引擎侧按同一条规则认它（单一真相源，
    /// 见 `Coordinator::manual_separator_char`）。取值随方案、`separator` 设置、
    /// 码表方案的整句开关变化，每次要用时再问，别缓存。纯读。
    pub fn separator_key(&self) -> Option<char> {
        self.coord.manual_separator_char()
    }

    /// 拉取候选全量的一个窗口。纯读，不改状态，可随时调。
    ///
    /// 别在每次按键都拉一大段：序列化与宿主侧的文字测量都在宿主主线程上。
    /// 起手拉够铺满一两屏即可，用户滚到尾部再续取。
    ///
    /// 要的范围超出已加载、且引擎还有更多时先扩容（[`CandidateWindow::rebuilt`] 置真，宿主须
    /// 整份重取已显示的部分）：首批只取够首屏，用户真往后滑才付钱。
    pub fn candidates(&self, offset: usize, limit: usize) -> CandidateWindow {
        self.coord.candidate_window_growing(offset, limit, false)
    }

    /// 同 [`Self::candidates`]，另带每条候选的注释灰字（码表剩余编码、拼音候选反查码…），
    /// 与电脑版候选窗同一份模板。宿主开着「候选显示编码提示」才调这个——注释逐条求值，不要就别付。
    pub fn candidates_with_comments(&self, offset: usize, limit: usize) -> CandidateWindow {
        self.coord.candidate_window_growing(offset, limit, true)
    }

    /// 按绝对下标选词，返回编辑指令流（与 [`Self::key_down`] 同一种形状）。
    ///
    /// 英文方案组码中选词，结果后追加补的空格（见 [`Self::press`]）。
    pub fn select_candidate(&self, index: usize) -> KeyOutcome {
        // 先算再选：选词之后组合已经没了，判不出「是在英文组码中选的」
        let english = self.coord.latin_input() && self.coord.is_composing();
        let mut outcome = self.select_candidate_plain(index);
        if english {
            self.append_auto_space(&mut outcome);
        }
        outcome
    }

    /// 同 [`Self::select_candidate`]，但不补空格、不动 `auto_spaced`。
    ///
    /// 宿主自己要紧接着上屏别的东西（表情、符号、数字键盘的字符）时用：先把首选顶上屏，
    /// 再接宿主的字符，中间不该夹一个空格。
    pub fn select_candidate_plain(&self, index: usize) -> KeyOutcome {
        let outcome = self.coord.select_candidate(index);
        self.coord.note_host_edits(&outcome.ops);
        outcome
    }

    /// 首单元的几种读法（展开页左栏画它）：同一串字母的不同切分、模糊音兄弟、只取声母。
    /// 没得选 / 非全拼 / 无编码 → 空。不改编码，只影响候选。
    ///
    /// 九宫格下是左栏逐段选拼音：列的是已锁定部分之后那一段的读法，点一条锁定一段
    /// （[`Self::set_leading_reading`]），退格先撤最后锁的那段。
    pub fn leading_readings(&self) -> Vec<LeadingReading> {
        self.coord.leading_readings()
    }

    /// 切换九宫格形态（宿主换到 / 换离九宫格布局时调用）。正在组的编码先清掉，
    /// 返回清空产生的编辑指令（与 [`Self::key_down`] 同形，无编码时为空）。
    pub fn set_keypad(&self, on: bool) -> KeyOutcome {
        let outcome = if self.coord.is_composing() {
            self.key_down(wind_keys::keymap::VK_ESCAPE, 0)
        } else {
            KeyOutcome::default()
        };
        self.coord.set_keypad(on);
        outcome
    }

    /// 指定方案能否用九宫格输入（全拼；双拼限纯字母布局）。宿主摆键盘列表时问，
    /// 决定这个方案除了全键盘要不要再给一格九宫格。读方案文件，不加载引擎。
    pub fn keypad_supported(&self, schema_id: &str) -> bool {
        self.coord.keypad_supported_of(schema_id)
    }

    /// 九宫格解码此刻是否生效（开了且活跃方案是拼音类）。
    pub fn keypad_active(&self) -> bool {
        self.coord.keypad_active()
    }

    /// 选定首单元的一种读法，候选按它重算（选回默认读法 = 清除）。返回值与
    /// [`Self::key_down`] 同形；读法已过期（编码变了）→ `None`，什么都没改。
    pub fn set_leading_reading(&self, head: &str, orig_len: usize) -> Option<KeyOutcome> {
        let outcome = self.coord.set_leading_reading(head, orig_len)?;
        self.coord.note_host_edits(&outcome.ops);
        Some(outcome)
    }

    /// 第 `index`（绝对下标，与 [`Self::candidates`] 同一口径）个候选的长按菜单；
    /// 无候选 / 越界 → 空。
    ///
    /// 菜单项与桌面右键同源（`Coordinator::candidate_menu_items`），文案与启用态逐字一致。
    pub fn candidate_menu(&self, index: usize) -> Vec<CandidateMenuItem> {
        let mut out = Vec::new();
        if let Some(items) = self.coord.candidate_menu_items(index) {
            flatten_candidate_menu(&items, &mut out);
        }
        out
    }

    /// 第 `index`（全量绝对下标）个候选的提示：与桌面候选窗悬停气泡同一份 `ui.tooltip.sections`
    /// 模板与求值（编码 / 读音 / 释义 / 拆字…）。手机没有悬停，宿主在长按时取来显示。
    ///
    /// 越界、或各段都为空时返回空列表。编码段依赖的反查索引第一次用时在后台建，建好前
    /// 这一段暂缺（核心绝不为它等待）。
    pub fn candidate_tooltip(&self, index: usize) -> Vec<CandidateTooltipSection> {
        self.coord.candidate_tooltip_at(index).unwrap_or_default()
    }

    /// 执行第 `index` 个候选的菜单动作；执行了返回 `true`。执行后核心重排候选，新列表经
    /// [`MobileEventSink::on_candidates`] 推来（与桌面右键同一条刷新）。
    ///
    /// `"copy"` 返回 `false`：复制由宿主自己写剪贴板，核心不碰。未知 action、越界、
    /// 或这一项在当前菜单里不存在 / 是灰的，同样返回 `false`——准入与 [`Self::candidate_menu`]
    /// 同源，宿主绕过灰显直接调也不会写出菜单本来拒绝的规则。
    ///
    /// 暂停学习（[`Self::set_learning_suspended`]）不拦这里：那是「别从输入里偷偷学」，
    /// 而这些是用户点名的显式编辑。
    pub fn candidate_menu_run(&self, index: usize, action: &str) -> bool {
        let Some(op) = candidate_menu_op(action) else {
            return false;
        };
        let offered = self
            .candidate_menu(index)
            .iter()
            .any(|i| i.action == action && i.enabled);
        if !offered {
            return false;
        }
        self.coord.candidate_or_quick_format_op_at(op, index);
        true
    }

    /// 可选主题。`_` 前缀的基底主题不在其中（它们只供继承）。
    pub fn themes(&self) -> Vec<ThemeEntry> {
        let active = self.coord.active_theme_id();
        self.coord
            .theme_entries()
            .into_iter()
            .map(|(id, name)| ThemeEntry {
                active: id == active,
                id,
                name,
            })
            .collect()
    }

    /// 按 id 切主题并持久化。返回是否命中。
    ///
    /// 按 id 而不是按下标：下标随主题目录增删漂移，宿主存下来下次会指向另一个主题。
    pub fn set_theme(&self, id: &str) -> bool {
        self.coord.select_theme_by_id(id)
    }

    /// 明暗设置：`"system"`（跟随系统）/ `"light"` / `"dark"`。
    pub fn theme_style(&self) -> String {
        self.coord.theme_style_name().to_string()
    }

    /// 设置明暗并持久化；未知值按跟随系统。
    pub fn set_theme_style(&self, style: &str) {
        self.coord.set_theme_style_name(style);
    }

    /// 求值后的语义色表。
    ///
    /// `system_dark` 必须由宿主给：核心的系统明暗探测只实现了 Windows 与 macOS，
    /// 其余平台恒 false，移动端若指望核心自己探测，「跟随系统」会静默变成恒亮色。
    /// Android 传 `Configuration.uiMode` 的 `UI_MODE_NIGHT_YES`，iOS 传
    /// `traitCollection.userInterfaceStyle == .dark`。
    ///
    /// 明暗或主题变化后重新调本方法取新表即可，不要在宿主侧缓存两套。
    pub fn palette(&self, system_dark: bool) -> Vec<PaletteColor> {
        self.coord
            .theme_palette(system_dark)
            .into_iter()
            .map(|(name, argb)| PaletteColor { name, argb })
            .collect()
    }

    /// 本次该用暗色吗（宿主要据此决定状态栏图标明暗等自身事务时用）。
    pub fn is_dark(&self, system_dark: bool) -> bool {
        self.coord.theme_dark_with(system_dark)
    }

    /// 键面字符 → 按法 `(字符, VK, 是否按 Shift)`。宿主启动时取一次。
    ///
    /// 含可打印 ASCII 与中文标点两部分，后者登记的是其 ASCII 源键的按法（详见
    /// `key_convert::char_strokes`）。宿主别自己写这张表：漏掉的字符会绕过引擎直接上屏，
    /// 智能符号、标点配对、自定义标点映射、中英标点切换对它统统失效。
    pub fn char_strokes(&self) -> Vec<(char, u32, bool)> {
        wind_coordinator::key_convert::char_strokes()
    }

    /// 可作引导键的符号键：`(规范名, 该键的字符)`。
    ///
    /// 触发键类配置（`input.temp_pinyin.trigger_keys` 等）存的是键名（如 `"backtick"`），界面要显示
    /// 键的样子（`` ` ``）；对照表由核心给，设置端自拼的会跟不上核心增删引导键。
    ///
    /// 宿主还可把名字还原成按键（名字 → 字符 → [`Self::char_strokes`]），让「点工具栏图标进入
    /// 某模式」与「上滑打出符号进入」走同一条路，模式的进入语义只有引擎一份实现。
    pub fn symbol_keys(&self) -> Vec<(String, char)> {
        wind_keys::keymap::symbol_keys()
            .map(|(name, ch)| (name.to_string(), ch))
            .collect()
    }

    // ── 配置读写（设置页）────────────────────────────────────────────
    //
    // 注册表说「有哪些键、什么类型」，快照说「现在各是什么值」，写入把用户的选择落到用户层。
    // 落盘逻辑全在 `Config::set_user_value`，理由见 `config_api` 模块文档。

    /// 当前生效配置的全量快照（TOML 文本），含移动端强制改写项。
    pub fn config_snapshot(&self) -> String {
        // 旧类型段（`schema.codetable` 等）是当前方案设置的别名，值取方案那边。
        config_api::overlay_scheme_sections(
            config_api::snapshot(&self.data_dir),
            wind_coordinator::web_host::WebDataHost::engine_mgr(&*self.coord)
                .scheme_alias_sections(),
        )
    }

    /// 出厂默认配置的全量快照（TOML 文本）。设置页据此标注默认值、实现「恢复默认」。
    pub fn config_defaults(&self) -> String {
        config_api::defaults(&self.data_dir)
    }

    /// core 的配置字段注册表：键 → 类型（+ 枚举值域）。
    ///
    /// 宿主用它在加载设置清单时比对出写错的键。
    pub fn config_registry(&self) -> Vec<config_api::ConfigFieldInfo> {
        config_api::registry()
    }

    /// 写一个配置键到用户层。只写盘，生效要另发
    /// [`MobileCommand::ReloadConfig`]——分开是为了让宿主把连改若干项合成一次重载。
    ///
    /// 旧类型级键（`schema.codetable.*` 等）是当前方案设置的别名，写到那个方案的用户层。
    pub fn config_set(&self, key: &str, value: config_api::ConfigValue) -> Result<(), String> {
        let canonical = wind_config::config_schema::canonical_key(key);
        if let Some(r) = wind_coordinator::web_host::WebDataHost::engine_mgr(&*self.coord)
            .scheme_alias_set(&canonical, value.clone().into_toml())
        {
            return r.map(|_| ()).map_err(|e| e.to_string());
        }
        config_api::set(key, value)
    }

    /// emoji 附加模块及当前开关（见 [`config_api::emoji_modules`]）。
    pub fn emoji_modules(&self) -> Vec<config_api::EmojiModuleState> {
        config_api::emoji_modules(&self.data_dir)
    }

    /// 打开 / 关闭一个 emoji 附加模块。只写盘，生效要另发 [`MobileCommand::ReloadConfig`]。
    pub fn set_emoji_module(&self, id: &str, on: bool) -> Result<(), String> {
        config_api::set_emoji_module(&self.data_dir, id, on)
    }

    /// 候选提示各段及当前开关（长按候选那张卡显示哪些段，见 [`config_api::tooltip_sections`]）。
    pub fn tooltip_sections(&self) -> Vec<config_api::TooltipSectionState> {
        config_api::tooltip_sections(&self.data_dir)
    }

    /// 全部已装方案（含没启用的）及其启用状态，顺序同已装列表（设置页「启用的方案」）。
    pub fn installed_schemas(&self) -> Vec<InstalledSchema> {
        use wind_coordinator::web_host::WebDataHost;
        let enabled = config_api::enabled_schemas();
        config_api::installed_schemas()
            .iter()
            .map(|id| InstalledSchema {
                name: self.coord.engine_mgr().schema_name(id),
                enabled: enabled.contains(id),
                locked: id == config_api::ENGLISH_SCHEMA,
                id: id.clone(),
            })
            .collect()
    }

    /// 方案包导入 / 删除 / 恢复落盘后调（与桌面 `web_scheme_import_package` 同一套收尾）：
    /// 1. 对 `changed_ids` 逐个使引擎与解析缓存失效——覆盖同 id 方案、恢复自带版本都不改
    ///    `config.schema`，随后的重载不会重建引擎，不失效就一直用旧定义；失效会扇出到把它当成员的混输；
    /// 2. 重扫各层 `schemas/` 刷新已装方案列表（规则同构造期：扫描结果 + 首装兜底方案）；
    /// 3. 重建跨方案派生配置（吃键集等）。
    ///
    /// 之后另发 [`MobileCommand::ReloadConfig`] 让可选方案跟上。导入 / 删除中途失败也要调
    /// （`changed_ids` 可空）：落盘是逐文件的，失败时磁盘可能已经变了一部分。
    pub fn on_schemes_changed(&self, changed_ids: &[String]) {
        use wind_coordinator::web_host::WebDataHost;
        for id in changed_ids {
            self.coord.engine_mgr().invalidate_schema(id);
        }
        self.refresh_installed_schemas();
        self.coord.refresh_schema_derived_config();
    }

    /// 重扫已装方案列表，规则同构造期：各层扫描结果，再补上首装兜底方案。
    pub fn refresh_installed_schemas(&self) {
        let mut ids = scan_installed_schemas(&self.data_dir);
        let fallback = config_api::fallback_schema();
        if !fallback.is_empty() && !ids.contains(&fallback) {
            ids.push(fallback);
        }
        config_api::set_installed_schemas(ids);
    }

    /// 方案包的目标目录：`(用户层 schemas, 只读层 schemas 列表)`。
    /// 只读层 = 出厂数据层（Android 即随 APK 解出的 `data/schemas`）与定制层，永不写、永不删。
    pub fn scheme_dirs(&self) -> (Option<std::path::PathBuf>, Vec<std::path::PathBuf>) {
        let mut user = None;
        let mut system = Vec::new();
        for layer in Config::resource_layers_named_with(Some(&self.data_dir)) {
            let dir = layer.path.join("schemas");
            if layer.name == "user" {
                user = Some(dir);
            } else {
                system.push(dir);
            }
        }
        (user, system)
    }

    /// 启用 / 停用一个已装方案。英文与最后一个中文方案关不掉（Err 里写原因）。
    /// 只写盘，生效要另发 [`MobileCommand::ReloadConfig`]。
    pub fn set_schema_enabled(&self, id: &str, on: bool) -> Result<(), String> {
        config_api::set_schema_enabled(id, on)
    }

    /// 打开 / 关闭一段候选提示（如「拆字」）。只写盘，生效要另发 [`MobileCommand::ReloadConfig`]。
    pub fn set_tooltip_section_enabled(&self, label: &str, on: bool) -> Result<(), String> {
        config_api::set_tooltip_section_enabled(&self.data_dir, label, on)
    }

    /// 快捷输入的候选来源：当前与出厂成员序（见 [`config_api::quick_mix_members`]）。
    pub fn quick_mix_members(&self) -> config_api::QuickMixMembers {
        config_api::quick_mix_members(&self.data_dir)
    }

    /// 整列写回快捷输入的候选来源。只写盘，生效要另发 [`MobileCommand::ReloadConfig`]。
    pub fn set_quick_mix_members(&self, members: Vec<String>) -> Result<(), String> {
        config_api::set_quick_mix_members(&self.data_dir, members)
    }

    // ── 输入统计（设置页「统计」）────────────────────────────────────
    //
    // 算法与桌面设置页同一份（`wind_store::stats_report`），本层只做取句柄与形状转换。

    /// 概览汇总。没有用户库（只读降级、无库的测试壳）时为 `None`。
    pub fn stats_summary(&self) -> Option<StatsSummary> {
        use wind_coordinator::web_host::WebDataHost;
        let store = self.coord.user_store()?;
        let collector = self.coord.stat_collector()?;
        Some(wind_store::stats_report::summary(store, collector))
    }

    /// `[from, to]`（`YYYY-MM-DD`，含首尾）每一天的字数，缺的日子补零，今天取内存最新值。
    pub fn stats_daily(&self, from: &str, to: &str) -> Vec<DailyTotal> {
        use wind_coordinator::web_host::WebDataHost;
        let Some(store) = self.coord.user_store() else {
            return Vec::new();
        };
        match wind_store::stats_report::daily_range(store, self.coord.stat_collector(), from, to) {
            Ok(days) => days
                .into_iter()
                .map(|(date, r)| DailyTotal {
                    total: r.total(),
                    chinese: r.chinese,
                    english: r.english,
                    date,
                })
                .collect(),
            Err(e) => {
                tracing::warn!("读每日统计失败: {e}");
                Vec::new()
            }
        }
    }

    /// 清空全部输入统计（历史 + 今日内存）。
    pub fn stats_clear(&self) -> Result<(), String> {
        use wind_coordinator::web_host::WebDataHost;
        wind_store::stats_report::clear(
            self.coord.user_store().map(|s| &**s),
            self.coord.stat_collector(),
        )
        .map_err(|e| e.to_string())
    }

    // ── 用户词库（设置页「词库」）────────────────────────────────────
    //
    // 本体与桌面设置页的 `dict.*` RPC 同一份（`wind_coordinator::user_words_api`）：
    // ü 归一、音节边界求解、搜索的三条分流都不在本层重写。`schema_id` 传方案 id 即可，
    // 拼音族折叠到 `pinyin` 数据方案在核心里做；混输方案与桌面现状一致，落它自己的桶
    // （`EngineManager::data_schema_id` 只折叠拼音族）。
    //
    // 加词 / 删词 / 清空落库时推进该方案的结构代次（`Store::bump_words_gen`），引擎据此
    // 失效候选缓存——改完立即打得出，与桌面同一条路，不需要另发重载。

    /// 分页列出用户词。`query` 为空 = 浏览（按编码序），非空 = 搜索（编码前缀命中在前，
    /// 词条内容 / 编码中段命中在后；可带音节空格）。出错记 warn 返回空页。
    pub fn user_words(
        &self,
        schema_id: &str,
        query: &str,
        offset: u32,
        limit: u32,
    ) -> UserWordPage {
        use wind_coordinator::web_host::WebDataHost;
        let r = wind_coordinator::user_words_api::list_paged(
            &*self.coord,
            schema_id,
            query,
            None,
            offset as usize,
            limit as usize,
        );
        // 三条分流都扫满全表（`total` 要精确）：标记一次，空闲后回收 redb 读缓存。
        // 与桌面 RPC 出口（wind-webdata 的 `scans_whole_table`）同一策略，只是那张表在
        // webdata 里、本层够不着，故在这里显式做。
        if let Some(store) = self.coord.user_store() {
            store.mark_scan_pending();
        }
        match r {
            Ok(page) => UserWordPage {
                total: page.total as u64,
                items: page
                    .items
                    .iter()
                    .map(|r| UserWord {
                        code: wind_coordinator::user_words_api::display_code(r),
                        text: r.text.clone(),
                        weight: r.weight,
                    })
                    .collect(),
            },
            Err(e) => {
                tracing::warn!("读用户词失败: {e}");
                UserWordPage::default()
            }
        }
    }

    /// 手动加词。`code` 可带音节空格（显式切分，优先于自动求解）。
    ///
    /// 权重固定取 [`USER_WORD_ADD_WEIGHT`]，与桌面设置页手动加词的默认值一致。
    pub fn user_word_add(&self, schema_id: &str, code: &str, text: &str) -> Result<(), String> {
        self.ensure_writable()?;
        wind_coordinator::user_words_api::add(
            &*self.coord,
            schema_id,
            code,
            text,
            USER_WORD_ADD_WEIGHT,
        )
        .map_err(|e| e.to_string())
    }

    /// 删词。`code` 接受列表给的显示码（带音节空格）。不存在静默成功。
    pub fn user_word_remove(&self, schema_id: &str, code: &str, text: &str) -> Result<(), String> {
        self.ensure_writable()?;
        wind_coordinator::user_words_api::remove(&*self.coord, schema_id, code, text)
            .map_err(|e| e.to_string())
    }

    /// 全部命令短语（系统 + 用户的叠加视图，按编码升序）。无存储时为空。
    ///
    /// 与桌面设置页 `phrase.list` 同一份数据（`Store::list_phrases`），只是不做那层 `\n` 转义。
    pub fn phrases(&self) -> Vec<Phrase> {
        use wind_coordinator::web_host::WebDataHost;
        let Some(store) = self.coord.user_store() else {
            return Vec::new();
        };
        match store.list_phrases() {
            Ok(v) => {
                let now = chrono::Local::now();
                v.into_iter()
                    .map(|p| Phrase {
                        preview: phrase_preview(&p.code, &p.text, now),
                        code: p.code,
                        text: p.text,
                        enabled: p.enabled,
                        is_system: p.is_system,
                    })
                    .collect()
            }
            Err(e) => {
                tracing::warn!("列短语失败: {e}");
                Vec::new()
            }
        }
    }

    /// 开 / 关一条短语（系统短语出厂关着的，如时间戳 tmts，在这里打开）。立即重建输入层。
    pub fn phrase_set_enabled(&self, code: &str, text: &str, enabled: bool) -> Result<(), String> {
        use wind_coordinator::web_host::WebDataHost;
        self.ensure_writable()?;
        let store = self.coord.user_store().ok_or("无持久化存储")?;
        store
            .set_phrase_enabled(code, text, enabled)
            .map_err(|e| e.to_string())?;
        self.coord.rebuild_phrases();
        Ok(())
    }

    /// 加一条用户短语。权重取 [`wind_store::phrases::DEFAULT_USER_PHRASE_WEIGHT`]，与桌面设置页
    /// 手动添加同一个缺省值（用户自己加的排在系统短语前）。
    pub fn phrase_add(&self, code: &str, text: &str) -> Result<(), String> {
        use wind_coordinator::web_host::WebDataHost;
        self.ensure_writable()?;
        let (code, text) = (code.trim(), text.trim());
        if code.is_empty() || text.is_empty() {
            return Err("编码和内容都不能为空".into());
        }
        let store = self.coord.user_store().ok_or("无持久化存储")?;
        store
            .add_phrase(
                code,
                text,
                0,
                wind_store::phrases::DEFAULT_USER_PHRASE_WEIGHT,
            )
            .map_err(|e| e.to_string())?;
        self.coord.rebuild_phrases();
        Ok(())
    }

    /// 删一条用户短语。删的若是遮蔽了系统条目的那条，系统默认那条随之露出来（同桌面
    /// `phrase.remove`：补回缺失的系统条目再重建）。
    pub fn phrase_remove(&self, code: &str, text: &str) -> Result<(), String> {
        use wind_coordinator::web_host::WebDataHost;
        self.ensure_writable()?;
        let store = self.coord.user_store().ok_or("无持久化存储")?;
        store.remove_phrase(code, text).map_err(|e| e.to_string())?;
        self.coord.restore_missing_system_phrases("删除短语");
        self.coord.rebuild_phrases();
        Ok(())
    }

    /// 系统短语恢复默认：开关回到出厂值（出厂关着的仍关着），返回改动条数。用户短语不动。
    pub fn phrases_restore_defaults(&self) -> Result<u64, String> {
        use wind_coordinator::web_host::WebDataHost;
        self.ensure_writable()?;
        Ok(self.coord.restore_system_phrases() as u64)
    }

    /// 清空该方案（折叠后的数据方案）的全部用户词，返回删除条数。
    pub fn user_words_clear(&self, schema_id: &str) -> Result<u64, String> {
        use wind_coordinator::web_host::WebDataHost;
        self.ensure_writable()?;
        let n = wind_coordinator::user_words_api::clear(&*self.coord, schema_id)
            .map_err(|e| e.to_string())?;
        // 整表规模的写：redb 刷盘把脏页晋升进读缓存且长期不还，做完立即回收。同桌面
        // RPC 出口的 `BULK_WRITE_METHODS`；失败只记日志——词已经清掉了，不能报成失败。
        if let Some(store) = self.coord.user_store()
            && let Err(e) = store.drop_page_cache()
        {
            tracing::error!(
                "清空用户词之后丢弃页缓存失败：{e}。数据已写入，但存储层现处于暂停态，\
                 后续读写都会失败，请重启输入法。"
            );
        }
        Ok(n as u64)
    }

    /// 用户正在操作联想候选（手指按在候选条上、展开面板开着、长按菜单弹着）：
    /// `true` 暂停联想的自动隐藏，`false` 结束操作、重新计满 `hide_after_ms`。
    /// 不在联想态时无操作。见 `Coordinator::assoc_hold`。
    pub fn assoc_hold(&self, hold: bool) {
        self.coord.assoc_hold(hold);
    }

    /// 联想学习数据的条数（联想里选过的词 + 学到的「下一词」），所有方案合计。
    pub fn assoc_history_count(&self) -> u64 {
        use wind_coordinator::web_host::WebDataHost;
        let Some(store) = self.coord.user_store() else {
            return 0;
        };
        // 枚举存储里出现过的全部数据方案：停用了的方案下学到的也算
        let n: usize = store
            .list_data_schemas()
            .unwrap_or_default()
            .iter()
            .map(|s| store.assoc_history_len(s))
            .sum();
        store.mark_scan_pending();
        n as u64
    }

    /// 清空全部方案的联想学习数据，返回删除条数。词频、用户词不动（桌面「清空词频」
    /// 会一并清它，手机上单独给入口：用户怀疑联想被自己的输入带偏时，只想重置这一项）。
    pub fn assoc_history_clear(&self) -> Result<u64, String> {
        use wind_coordinator::web_host::WebDataHost;
        self.ensure_writable()?;
        let store = self.coord.user_store().ok_or("无持久化存储")?;
        let mut n = 0usize;
        for s in store.list_data_schemas().map_err(|e| e.to_string())? {
            n += store.clear_assoc_history(&s).map_err(|e| e.to_string())?;
        }
        // 上一次上屏的词还记着，清完立刻再上屏一个词会接着学成一对；断开它
        self.coord.note_word_boundary();
        Ok(n as u64)
    }

    // ── 词库导出 / 导入（个人词库备份）────────────────────────────────
    //
    // 本体与桌面 `dict.export` / `dict.import` 同一份（`wind_coordinator::dict_transfer_api`），
    // 文件因此与电脑版互通：头部写 engine_type、跨引擎类型拒收、拼音准入契约都不在本层重写。
    // 导入经 Store 推进结构代次（`bump_words_gen`），与加词同一条路，导完立即打得出。

    /// 导出该方案的个人词库为多段 .wdict 文本（段按引擎类型取默认：码表 / 拼音四段，
    /// 混输仅候选调整）。宿主负责把文本写到用户选的文件里。
    pub fn dict_export(&self, schema_id: &str) -> Result<String, String> {
        use wind_coordinator::web_host::WebDataHost;
        let r = wind_coordinator::dict_transfer_api::export(&*self.coord, schema_id, None)
            .map_err(|e| e.to_string());
        // 整表扫描：标记一次，空闲后回收 redb 读缓存（同桌面 `FULL_SCAN_METHODS` 里的 dict.export）。
        if let Some(store) = self.coord.user_store() {
            store.mark_scan_pending();
        }
        r
    }

    /// 导入词库文本：.wdict（电脑版 / 本机导出，应用文件所含全部段）或 Rime / TSV（仅用户词）。
    ///
    /// `replace` = 先清空对应段再导入，否则合并。拼音音节边界固定由程序补充（桌面导入对话框
    /// 的默认选项）。文件的引擎类型与方案不一致（如五笔词库导入拼音）→ Err(原因)。
    pub fn dict_import(
        &self,
        schema_id: &str,
        content: &str,
        replace: bool,
    ) -> Result<DictImportResult, String> {
        use wind_coordinator::web_host::WebDataHost;
        self.ensure_writable()?;
        let out = wind_coordinator::dict_transfer_api::import(
            &*self.coord,
            schema_id,
            content,
            replace,
            true,
            None,
        )
        .map_err(|e| e.to_string())?;
        // 整表规模的写，理由与失败处置同 [`Self::user_words_clear`]（桌面 `BULK_WRITE_METHODS`
        // 里的 dict.import）。
        if let Some(store) = self.coord.user_store()
            && let Err(e) = store.drop_page_cache()
        {
            tracing::error!(
                "导入词库之后丢弃页缓存失败：{e}。数据已写入，但存储层现处于暂停态，\
                 后续读写都会失败，请重启输入法。"
            );
        }
        Ok(DictImportResult {
            sections: out
                .sections
                .into_iter()
                .map(|s| {
                    let w = s.words.unwrap_or_default();
                    let c = s.contract.unwrap_or_default();
                    DictImportSection {
                        key: s.key.to_string(),
                        added: w.added as u64,
                        updated: w.updated as u64,
                        unchanged: w.unchanged as u64,
                        imported: s.imported as u64,
                        skipped: s.skipped as u64,
                        rejected: c.rejected as u64,
                        rejected_samples: c.samples,
                        boundary_filled: c.filled as u64,
                        boundary_skipped: c.unfilled_skipped as u64,
                    }
                })
                .collect(),
        })
    }

    // ── 系统词库统计（设置页「词库 → 词库统计」）────────────────────────
    //
    // 本体与桌面 `dict.systemStats` 同一份（`wind_coordinator::dict_stats_api`）：选库、混输按
    // 成员并排、结果缓存、用户词 / 临时词计数都不在本层重写。

    /// 统计方案的系统词库，回包为 JSON 文本（形态同桌面 `dict.systemStats`：码表 / 拼音 /
    /// 英文方案一份，混输方案 `members` 并排各成员）。`top_n` 为重码 Top 列表条数（上限
    /// [`wind_coordinator::dict_stats_api::TOP_N_MAX`]），`refresh` 绕过缓存重算。
    ///
    /// O(全表)：拼音大词库一遍几十 MB 峰值、百毫秒级以上，宿主须在后台线程调用。
    /// 未知方案 / 混输未配成员 → Err(原因)。
    pub fn dict_stats(&self, schema_id: &str, top_n: u32, refresh: bool) -> Result<String, String> {
        wind_coordinator::dict_stats_api::system_stats(
            &*self.coord,
            schema_id,
            top_n as usize,
            refresh,
        )
        .map(|v| v.to_string())
        .map_err(|e| e.to_string())
    }

    // ── 方案附加词库（设置页「方案 → 拼音」等）────────────────────────
    //
    // 本体与桌面 `schema.setDictEnabled` 同一份（`wind_coordinator::schema_dicts_api`）：
    // override 只落稀疏项、先持久化再 live 翻、混输扇出都不在本层重写。改完即时生效，
    // 不需要另发重载。

    /// 列出方案的附加词库（不含主库），按方案文件顺序。未知方案记 warn 返回空。
    pub fn schema_dicts(&self, schema_id: &str) -> Vec<SchemaDictInfo> {
        match wind_coordinator::schema_dicts_api::list(&*self.coord, schema_id) {
            Ok(v) => v
                .into_iter()
                .map(|d| SchemaDictInfo {
                    id: d.id,
                    label: d.label,
                    description: d.description,
                    enabled: d.enabled,
                })
                .collect(),
            Err(e) => {
                tracing::warn!("列方案附加词库失败: {e}");
                Vec::new()
            }
        }
    }

    /// 开关方案的附加词库并持久化。未知方案 / 方案里没有这个库 / 只读模式 → Err(原因)。
    ///
    /// 词库文件缺失时打开不算错：开关照常落盘，重建时那一层只记 warning、主库照常出词
    /// ——出厂包可以先不带某批扩展库文件，开关项照样能列、能点。
    pub fn set_schema_dict_enabled(
        &self,
        schema_id: &str,
        dict_id: &str,
        on: bool,
    ) -> Result<(), String> {
        // 桌面扩展库开关不查只读（override 不在用户数据库里），手机上统一挡：只读模式本就
        // 意味着「这份用户目录不归本版本写」，原因原样交给用户。
        self.ensure_writable()?;
        wind_coordinator::schema_dicts_api::set_enabled(&*self.coord, schema_id, dict_id, on)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// 写口的前置闸：只读模式（用户数据由更新版本写入 / meta.toml 读不出）拒绝。
    ///
    /// 桌面那边只读模式下 store 根本不打开，写 RPC 靠「无持久化存储」间接挡住；这里显式
    /// 先查，是为了把原因（要升级 / 要从备份还原）原样交给用户，而不是一句看不懂的
    /// 「无持久化存储」。
    fn ensure_writable(&self) -> Result<(), String> {
        match self.coord.storage_readonly_reason() {
            Some(r) => Err(r.to_string()),
            None => Ok(()),
        }
    }

    /// 可选双拼布局。清单由核心扫描目录得出，宿主不要硬编码：
    /// 用户可以往 `schemas` 的 `shuangpin` 子目录里丢自己的布局文件，硬编码清单看不见它。
    pub fn shuangpin_layouts(&self) -> Vec<LayoutEntry> {
        let active = self.coord.active_shuangpin_layout();
        self.coord
            .shuangpin_layouts()
            .into_iter()
            .map(|(id, name)| LayoutEntry {
                active: id == active,
                id,
                name,
            })
            .collect()
    }
}

/// 移动端手动加词的权重。
///
/// 取桌面设置页手动加词的默认值，两个入口加出来的词排序口径一致：wind-setting 仓
/// `src/add_word_window.rs` 的 `DEFAULT_WEIGHT`（快捷加词小窗）与
/// `src/pages/dict/state.rs` 打开新增对话框时预填的 `edit_weight`，都是 1200。
/// 移动端词库页不提供权重输入（手机上没人会去调它），故固定取这个值。
pub const USER_WORD_ADD_WEIGHT: i32 = 1200;

/// 把用户目录、本地目录与安装根钉到宿主给的路径。
///
/// 用户目录（`user_root`，布局同桌面）与本地目录（`local_root`，缓存与日志）经
/// `Config::set_dir_override` 显式注入，不靠改 `XDG_*` 环境变量。
///
/// 安装根也必须钉：没有 `WIND_INSTALL_ROOT` 时它取 `current_exe().parent()`，安卓上是
/// `/system/bin`，核心找不到出厂 config.toml。后果是静默的：`set_user_value` 的「等于出厂默认
/// 不落盘」（`preset_for_pruning()`）永久降级为照常写入，每次保存都把当前默认钉进用户层，
/// 「恢复默认」也删不掉键。
///
/// `data_dir` 必须就是 `<安装根>/data`（核心的固定关系）；形状不对时只警告不猜，猜错会让核心
/// 去另一个目录读出厂配置，更难查。`WIND_INSTALL_ROOT`「生产严禁设置」的告诫只针对桌面；移动端
/// `current_exe()` 是 `app_process64`，不设就没有正确答案，且进程内只有本函数一个写入方。
/// `is_portable()` 也读这个根（看 `portable_mode` 标记文件），App 私有目录下不会有它。
fn mount_dirs(data_dir: &Path, user_root: &Path, local_root: &Path) {
    Config::set_dir_override(user_root.to_path_buf(), local_root.to_path_buf());
    // SAFETY: 这是构造核心的第一步，首次调用时核心还没起任何线程；键盘服务重建时再次构造，
    // 写入的是同一组路径。宿主侧若有线程同时读环境变量仍属未定义行为——移动端没有别的办法
    // 把这两个值交给核心（见上方文档），这是接受的代价。
    unsafe {
        std::env::set_var("HOME", local_root);
    }
    match (data_dir.file_name(), data_dir.parent()) {
        // SAFETY: 同上。
        (Some(name), Some(install_root)) if name == "data" => unsafe {
            std::env::set_var("WIND_INSTALL_ROOT", install_root);
        },
        _ => tracing::warn!(
            "数据目录 {} 不是 `<安装根>/data` 的形状，出厂默认取不到：配置剪枝与「恢复默认」会退化为照常写入",
            data_dir.display()
        ),
    }
}

/// 反向通道泵：`Receiver<SessionEvent>` → [`MobileEventSink`] 回调。
///
/// 对 `SessionEvent` 穷尽匹配（不写 `_`）：核心加了新事件，这里要编译失败而不是静默吞掉。
fn spawn_session_pump(
    rx: std::sync::mpsc::Receiver<SessionEvent>,
    sink: Arc<dyn MobileEventSink>,
    warming: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("wind-session-pump".into())
        .spawn(move || {
            for ev in rx {
                // 预热喂的假按键会产生候选帧，丢掉——否则键盘刚起来会闪一下候选。
                // 只挡 View/Hide，状态/提示类照常（预热不改模式，本来也不会产生）。
                if warming.load(Ordering::Relaxed)
                    && matches!(ev, SessionEvent::View(_) | SessionEvent::Hide)
                {
                    continue;
                }
                match ev {
                    SessionEvent::View(CompositionView {
                        preedit,
                        preedit_caret,
                        composing,
                        mode_label,
                        has_candidates,
                        selected,
                        page,
                        total_pages,
                    }) => sink.on_candidates(CandidateFrame {
                        preedit,
                        preedit_caret,
                        mode_label,
                        composing,
                        has_candidates,
                        selected,
                        page,
                        total_pages,
                    }),
                    SessionEvent::Hide => sink.on_hide_candidates(),
                    SessionEvent::Status(SessionStatus {
                        chinese_mode,
                        latin_input,
                        icon_label,
                        full_width,
                        chinese_punct,
                        s2t_enabled,
                        s2t_shown,
                    }) => sink.on_status(InputStatus {
                        chinese_mode,
                        latin_input,
                        icon_label,
                        full_width,
                        chinese_punct,
                        s2t_enabled,
                        s2t_shown,
                    }),
                    // 桌面的 toast 与状态气泡在核心里已合为一条 Notice
                    SessionEvent::Notice(text) => sink.on_toast(text),
                }
            }
        })
        .expect("spawn wind-session-pump");
}

/// 扫描各资源层（user / custom / data）的 `schemas/*.schema.toml`，返回已安装的方案 id。
///
/// 与桌面端 `EngineManager::installed_schemas` 同语义：各层合并去重（不是双拼布局那种靠前层胜出）。
/// 层序见 [`wind_config::Config::resource_layers_with`]。按文件名排序保证稳定，否则回送给
/// `SelectSchema` 的下标会随目录遍历顺序漂移。
fn scan_installed_schemas(data_dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for base in Config::resource_layers_with(Some(data_dir)) {
        let Ok(entries) = std::fs::read_dir(base.join("schemas")) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".schema.toml") {
                // 定制版 `[schemas] hide` 删掉的方案视为不存在（同 `EngineManager::scan_layer_schema_ids`），
                // 否则列表里会留一个切不过去的空条目。
                if Config::custom_hides_schema(id) {
                    continue;
                }
                ids.push(id.to_owned());
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// 喂键。不喂合成 caret：`new_session` 通道没有桌面的首显闸门，候选帧不依赖光标坐标。
fn dispatch_key(coord: &Coordinator, vk: u32, modifiers: u32) -> BridgeKeyAction {
    coord.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        // 必须原样透传，不能写死 0：吃键判定（`should_handle_key`）收到的是真实 modifiers，
        // 两边要对同一次按键认知一致；Shift 的语义归核心按键链（见 key_convert.rs）。
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    })
}

/// 词库门面：形状转换（显示码、`u64` 总数）与只读闸。存储语义本身在 wind-coordinator 的
/// `tests/user_words_api.rs` 验，这里不重复。
#[cfg(test)]
#[path = "user_words_tests.rs"]
mod user_words_tests;

/// 词库导出 / 导入门面：往返、跨引擎类型拒收、Rime/TSV、只读闸、导完立即打得出。
/// 段交集与准入契约的本体在 wind-coordinator 的 `dict_transfer_api`，桌面 JSON 形状在
/// wind-webdata 的测试里验，这里只验门面这一层。要 `build_dev/data` 的用例缺失时打印跳过。
#[cfg(test)]
#[path = "dict_transfer_tests.rs"]
mod dict_transfer_tests;

/// 方案附加词库门面：字段映射、错误转成原因串、只读闸、重建后仍是新值。列表口径与
/// live 生效在 wind-coordinator 的 `tests/schema_dicts_api.rs` 验，这里只验门面这一层。
/// 要 `build_dev/data`，缺失时打印跳过。
#[cfg(test)]
#[path = "schema_dicts_tests.rs"]
mod schema_dicts_tests;

/// 候选长按菜单门面：扁平化、action 映射、准入。菜单内容本身与绝对下标执行在
/// wind-coordinator 的 `tests/candidate_menu_abs_index.rs` 验，这里只验门面这一层。
#[cfg(test)]
#[path = "candidate_menu_tests.rs"]
mod candidate_menu_tests;

/// 候选提示门面：与桌面气泡同一份段模板。段内容本身在 wind-coordinator 的 tooltip 测试里验，
/// 这里只验「按绝对下标取得到、越界为空、编码段在索引建好后出现」。
#[cfg(test)]
#[path = "candidate_tooltip_tests.rs"]
mod candidate_tooltip_tests;

/// 正确拼音提示的分段透传：核心按 `hint.enabled` / 来源 / `hint.scope` 给候选生成带提示正文，这里只验
/// 「拉取窗口带出分段、提示段角色 `pinyin_hint`、关掉即空」。哪些候选该标在协调器测试里验。
#[cfg(test)]
#[path = "pinyin_hint_tests.rs"]
mod pinyin_hint_tests;

/// 泵线程的预热闸门：只拦 `View` / `Hide`，用户真按键后帧必须到达宿主。
#[cfg(test)]
#[path = "lib_session_pump_tests.rs"]
mod session_pump_tests;
