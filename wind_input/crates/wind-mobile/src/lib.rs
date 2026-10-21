//! wind-mobile：移动端形态的核心门面（headless Coordinator 的收口）。
//!
//! # 这一层为什么存在
//!
//! 平台绑定层（Android 的 UniFFI 包装、将来 iOS 的 Swift 包装）此前直接 path 依赖
//! **七个**核心 crate（coordinator/host/bridge/config/ipc/keys/ui-types），等于把绑定层
//! 焊在核心的内部结构上：核心一次内部重构就打断平台仓的构建，而平台仓在另一个仓库、
//! 另一条 CI 上，断了往往过几天才发现。
//!
//! 本 crate 把移动端要用的能力聚合成**一条依赖边**。平台仓只依赖 `wind-mobile`，
//! 核心怎么重构由这里吸收。
//!
//! # 边界划在哪
//!
//! 判据不是「Android 用不用得上」，而是「**换成 iOS 还成不成立**」：
//!
//! - **留在这里**（移动端形态，与绑定框架无关）：四层配置加载、已装方案扫描与
//!   激活校正、`preedit_display` 覆盖、`UiCommand` 泵线程、预热闸门、吃键判定转发。
//! - **归平台仓**（平台专属）：UniFFI/Swift 的类型投影、日志接到 logcat/oslog、
//!   `cdylib` 产物形态、绑定生成器。
//!
//! 所以本 crate 的依赖表里不该出现任何绑定框架——一旦沾上，「接口层」就退化成
//! 「那个平台的实现层」，第二个平台再来只能复制粘贴。
//!
//! # 通道形状（对应 WindInputAndroid docs/architecture.md §3.1）
//!
//! - 正向：[`MobileCore::key_down`] → `Coordinator::handle_key_event` → 同步 [`KeyOutcome`]
//!   （上屏文本/组合区走按键返回值，与 TSF 宿主同一条主输入路）
//! - 反向：`Receiver<UiCommand>` 泵线程 → [`MobileEventSink`] 回调（候选/状态/提示）
//! - 候选：推送帧 [`CandidateFrame`] 只给**当页**（够画编码栏与页码），列表本身由宿主按窗口
//!   [`MobileCore::candidates`] 拉、按绝对下标 [`MobileCore::select_candidate`] 选。
//!   这两个口取代了骨架期「合成页内数字键」的做法——那把点选限死在前 9 个，
//!   于是候选栏的滚动也没有意义（一帧才 6~9 条）。桌面鼠标点选走 push 通道上屏，
//!   headless 没有消费端，所以这里改走「返回编辑指令流」，与 `key_down` 同一种形状。
//!
//! 骨架期已知妥协（均有 TODO 标注）：目录经 XDG 环境变量挂载。
//!
//! # 工具栏与功能菜单的呈现不属于这一层
//!
//! 桌面那棵功能菜单树（截图、打开日志目录、诊断 HUD、语言栏图标调试…）大半在手机上
//! 无意义，照搬过来只会把桌面形态的包袱带进移动端。此处只提供两样东西：
//!
//! - [`MobileCommand`] —— **能调到**引擎的哪些开关（展示什么、怎么排、叫什么由宿主定）
//! - [`InputStatus`] —— 引擎当前的模式真值（中英/标点/全半角/简繁/方案短称）
//!
//! 状态**必须**由引擎推送而不是宿主自己记：方案切换会改写方案短称，密码框会强制
//! 英文，这些都发生在引擎内部；宿主侧自记一份开关态必然与引擎漂移。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub mod config_api;

use wind_bridge::handler::KeyAction as BridgeKeyAction;
use wind_bridge::handler::{FocusData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_host::{KeyProbe, Modifiers};
use wind_ipc::protocol::{EVENT_KEY_DOWN, FocusLostReason, caret_source};
use wind_ui_types::{CandidateOp, MenuCmd, MenuItemSpec, MenuKind, UiCommand, UiEvent};

/// 宿主直接消费的接缝类型，从 [`wind_host`] 原样透出。
///
/// 不在这里另立一套投影：`EditOp`/`KeyOutcome` 本就是给宿主用的接缝，多包一层就多一张
/// 要跟着核心同步的映射表，而这张表的漏项是静默的（少接一个变体 = 那个动作在移动端
/// 悄悄消失，不报错、不掉键，单纯功能没有）。
pub use wind_host::{EditOp, KeyOutcome, TimingHint};

/// 候选窗口与它的核心侧硬上限，从协调器原样透出。
///
/// [`MAX_WINDOW`] 要跟着透出而不是让宿主自己记一份：宿主传的 `limit` 超过它会被
/// 核心静默截断，宿主若按自记的数字判断「还有没有更多」，就会在边界上反复拉空窗口。
pub use wind_coordinator::candidate_pull::{CandidateWindow, MAX_WINDOW};
pub use wind_store::stats_report::StatsSummary;

/// 候选帧：编码栏与候选栏一次渲染所需的全部数据。
///
/// 拆成结构体而非长参数列表——它还会继续加字段（翻页栏、注释行），每加一个都改
/// trait 方法签名会连带宿主侧实现全部返工。
#[derive(Debug, Clone)]
pub struct CandidateFrame {
    /// 编码区串（五笔码 / 拼音串），编码栏正文
    pub preedit: String,
    /// 编码区插入符位置：`preedit` 内的**字节**偏移（恒在字符边界）
    pub preedit_caret: usize,
    /// 模式指示文本（五/拼/英/符…），编码栏左侧角标；空 = 不显示
    pub mode_label: String,
    /// 当前页候选文本
    pub items: Vec<String>,
    /// 键盘选中项（**页内**下标），空格上屏目标
    pub selected: usize,
    /// 当前页（1 起）
    pub page: usize,
    /// 总页数
    pub total_pages: usize,
}

/// 引擎当前模式真值。**数据，不是界面**——工具栏画几个格、放哪、什么图标由宿主决定。
#[derive(Debug, Clone)]
pub struct InputStatus {
    pub chinese_mode: bool,
    /// 模式主字，最多 2 个字符。中文态取方案的 `[schema] icon_label`（"五"/"拼"），
    /// 非中文态取 `[ui.labels]`（出厂 "英"/"A"，**用户可配**）。
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
/// `${var}` 递归与 `{light, dark}` 变体都已在核心侧展开完毕，这里是**终值**——
/// 宿主不该也不需要自己存两套色表，切明暗时重新取一次即可。
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
    /// **带音节空格的显示码**（`ni hao`）；码表方案、无边界信息的旧词没有空格。
    /// 原样回传给 [`MobileCore::user_word_remove`] 即可删掉——拆回扁平码由核心做。
    pub code: String,
    /// 真实文本（存储域，不做桌面设置页那层 `\n` 转义——移动端直接显示原文）
    pub text: String,
    pub weight: i32,
}

/// 一条快捷短语（设置页「快捷短语」列表项）：系统与用户短语的叠加视图。
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
/// 子菜单（目前候选菜单没有）会被摊平成同级项。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateMenuItem {
    /// 文案，与桌面同一候选的右键菜单逐字一致（如「隐藏候选」「设为生僻字」）
    pub label: String,
    /// `"move_top"` | `"move_up"` | `"move_down"` | `"delete"` | `"reset"` |
    /// `"toggle_common"` | `"copy"`。原样回传给 [`MobileCore::candidate_menu_run`]。
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
const CANDIDATE_MENU_ACTIONS: [(&str, CandidateOp); 6] = [
    ("move_top", CandidateOp::MoveTop),
    ("move_up", CandidateOp::MoveUp),
    ("move_down", CandidateOp::MoveDown),
    ("delete", CandidateOp::Delete),
    ("reset", CandidateOp::Reset),
    ("toggle_common", CandidateOp::ToggleCommon),
];

/// 把桌面菜单树压成移动端的扁平列表：只留 Op 与 Copy 叶子，子菜单摊平。
fn flatten_candidate_menu(items: &[MenuItemSpec], out: &mut Vec<CandidateMenuItem>) {
    for item in items {
        let action = match item.kind {
            MenuKind::Op(op) => CANDIDATE_MENU_ACTIONS
                .iter()
                .find(|(_, o)| *o == op)
                .map(|(a, _)| *a),
            MenuKind::Copy => Some("copy"),
            MenuKind::Submenu => {
                flatten_candidate_menu(&item.children, out);
                None
            }
            MenuKind::Command(_) | MenuKind::Separator | MenuKind::Label => None,
        };
        if let Some(action) = action {
            out.push(CandidateMenuItem {
                label: item.label.clone(),
                action: action.to_string(),
                enabled: item.enabled,
            });
        }
    }
}

/// 宿主可下达的引擎命令。
///
/// 刻意是一张**手工挑选的小表**而非桌面 `MenuCmd` 的镜像：后者含截图、打开目录、
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

/// 反向通道接收端（由 UiCommand 泵线程调用，**非宿主主线程**，实现方自行切回）。
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
    /// 出厂数据目录。留着是为了配置快照能走**与构造期同一条**四层加载路径
    /// （见 [`MobileCore::config_snapshot`]）——少了它只能读代码默认值，
    /// 设置页会把出厂 config.toml 里的取值显示成默认值。
    data_dir: std::path::PathBuf,
}

impl MobileCore {
    /// 构造核心。
    ///
    /// - `data_dir`: 出厂数据目录（`schemas/` 所在，Android 上即 filesDir/data）
    /// - `user_root`: 用户/缓存数据根（Android 上即 filesDir/rust，内部分 config/data/cache）
    /// - `fallback_schema`: **首装兜底**方案 id；用户上次选过的优先，见下方说明
    pub fn new(
        data_dir: &Path,
        user_root: &Path,
        fallback_schema: &str,
        sink: Arc<dyn MobileEventSink>,
    ) -> Arc<Self> {
        mount_dirs(data_dir, user_root);

        // 四层配置加载（代码默认 ⊕ data/config.toml ⊕ data_custom/config.toml ⊕ 用户层），
        // 与桌面同一条路径——移动端没有定制版包时 L2.5 自然缺席，不需要分支——
        // 「配置格式三端统一」是移植的硬约束（多端同步的前提），这里若退回
        // `Config::default()`，出厂 config.toml 里的短语/命令栏/候选行为全部失效。
        let mut cfg = Config::load(Some(data_dir)).unwrap_or_else(|e| {
            tracing::warn!("配置加载失败，回落代码默认值: {e}");
            Config::default()
        });

        // 可选方案 = **assets 里实际装了的方案**，而不是 config.toml 里列的那份。
        //
        // 两个方向都要修正：
        // - 删：出厂清单列的是桌面全量，装不出来的条目会在方案选择器里变成死项；
        // - 加：清单**没列但装了**的照样要能用（英文方案就是这样——桌面把它当作
        //   临时英文的目标引擎、不放进 available，移动端却要把它当一等方案切换）。
        // 移动端的语义就是「装了什么就能用什么」，以文件系统为准最省心。
        let mut installed = scan_installed_schemas(data_dir);
        if !installed.iter().any(|s| s == fallback_schema) {
            installed.push(fallback_schema.to_string());
        }
        config_api::set_installed_schemas(installed.clone());
        // 启用的 = 装了的 ∩ 用户在设置里选的（没选过 = 全部），见 `config_api::apply_enabled_schemas`；
        // 重载时覆盖钩子按同一规则重算
        let available = config_api::enabled_schemas();

        // 激活方案：**用户上次选的优先**，`fallback_schema` 只是首装兜底。
        //
        // ⚠ 此前这里无条件用参数覆盖，把三层加载刚读出来的用户层选择直接冲掉——
        // `select_schema` 明明把 `schema.active` 写进了用户层，表现却是「切到拼音，
        // 杀掉进程再进来又回五笔」，而且切换当场是好的，只有重启才复现，极难归因。
        //
        // 仍要校验「装了没有」：用户层可能留着上一版装过、这一版删掉的方案 id，
        // 照用会让引擎起不来（可选方案里没有它，`select_schema` 也选不回去）。
        let active = if cfg.schema.active.is_empty() || !available.contains(&cfg.schema.active) {
            fallback_schema.to_string()
        } else {
            cfg.schema.active.clone()
        };
        tracing::info!("已装方案: {:?}，激活: {}", available, active);
        cfg.schema.available = available;
        cfg.schema.active = active;

        // 移动端强制改写项（编码显示位置等）。**与设置页快照共用同一个函数**：
        // 两处若各写一份，构造时改了、快照没改，设置页就会显示一个引擎没在用的值，
        // 用户还能去「改」它、改完毫无反应。
        config_api::apply_mobile_overrides(&mut cfg);

        // 用户数据根：redb 落在这里。**不能传 None**——那样系统短语层会整段为空
        // （详见 `new_headless_with_ui_at` 的说明），词频与自造词也不落盘。
        let (coord, rx) =
            Coordinator::new_headless_with_ui_at(cfg, Some(data_dir), Some(user_root));

        let warming = Arc::new(AtomicBool::new(true));
        spawn_ui_pump(rx, sink, Arc::clone(&warming));

        // 自绘候选条，不提供光标坐标——关掉桌面用的首显闸门等待
        coord.set_caret_independent(true);
        // 设置页每改一项就重载一次：成功不弹「设置已更新」（失败照常提示）
        coord.set_quiet_reload_toast(true);
        // 强制改写项在每次重载配置后也要再套上（设置页每改一项就重载一次）
        coord.set_config_overlay(config_api::apply_mobile_overrides);
        // ⚠ 这里**保持开启**启动预热（`set_eager_prewarm(true)` 是默认值）。
        //
        // 曾经关掉过，理由是「手机上编译所有方案要几秒 CPU、会撞上用户打字」——那个
        // 判断错了两次：ANR 的真根因是**核心构造在宿主主线程**（已由宿主侧挪到后台），
        // 与预热无关；而关掉预热的代价远超收益——未预热的方案**不会变成「按需加载一次」，
        // 而是每次查询都重解析整份 yaml**（`CachedDict` 写不出 wdat 缓存时退化为内存模式），
        // 实测每次按键多花 200ms，触摸事件因此堆积、连点被误判成上滑。
        //
        // 要改回按需加载，先确认词库能落成 wdat 缓存，否则就是把一次性开销摊成永久开销。
        // 惰性构建搬到后台（真机冷启动首键实测阻塞 2.8 秒）。走核心的就绪契约，
        // 宿主不再自己猜「喂哪几个键能触发惰性构建」。
        coord.prepare();
        // 拼音类方案：各声母第一次查询的一次性开销提前在后台付掉（见该函数说明）
        coord.prewarm_single_initials();

        Arc::new(Self {
            coord,
            warming,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// 按下键（`vk` = wind-keys VK 码，`modifiers` 见 [`wind_host::Modifiers`]）。
    ///
    /// 抬起事件骨架期不喂（协调器按键链以 down 为主）。
    pub fn key_down(&self, vk: u32, modifiers: u32) -> KeyOutcome {
        // 用户真按键了 → 预热窗口立即关闭。没有这一行，用户在预热完成前抢先打字时，
        // 他的**真实候选帧**会被泵线程当成预热帧丢掉（表现为「打了字没候选」）。
        // 预热线程末尾那次 store(false) 因此是幂等的补充，不是唯一出口。
        self.warming.store(false, Ordering::Relaxed);

        // ★ 吃键判定走**核心的唯一真相源**（`wind_coordinator::key_gate`）。
        // 此处曾有一份手写谓词，一个会话里被同形 bug 打脸三次（空缓冲功能键失效、
        // 英文模式字母失效…），根因就是它与核心判据漂移。宿主不再自己判。
        let probe = KeyProbe::new(vk).with_modifiers(Modifiers(modifiers));
        if !self.coord.should_handle_key(&probe) {
            // 交还宿主的键（空缓冲退格、方向键、回车…）动的是宿主里的正文，
            // 前后两次上屏就不再相邻：下一词学习的断点
            self.coord.note_word_boundary();
            return KeyOutcome::passthrough();
        }
        wind_coordinator::edit_ops::to_outcome(dispatch_key(&self.coord, vk, modifiers))
    }

    /// 焦点获得（对应 Android `onStartInput`）。
    pub fn focus_gained(&self) {
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
        self.coord.handle_focus_lost(1, FocusLostReason::Thread);
    }

    /// 暂停 / 恢复学习（隐私 / 无痕输入）。暂停期间照常出候选、照常上屏，但这次输入
    /// **不写进任何用户数据**：词频、自动造词、联想与下一词、英文造词、输入统计、各类历史。
    /// 用户在设置页 / 候选菜单里的显式操作（加词、删词、置顶）不受影响。
    ///
    /// 宿主在**每次** `onStartInput`（[`Self::focus_gained`] 前后均可）按当前输入框重新声明：
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

    /// 拉取候选全量的一个窗口。**纯读**，不改状态，可随时调。
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

    /// 按**绝对下标**选词，返回编辑指令流（与 [`Self::key_down`] 同一种形状）。
    ///
    /// 取代此前「合成数字键」的做法——那条路把选词限死在页内 1-9，第 10 个及以后
    /// 永远点不到。
    pub fn select_candidate(&self, index: usize) -> KeyOutcome {
        self.coord.select_candidate(index)
    }

    /// 第 `index`（**绝对下标**，与 [`Self::candidates`] 同一口径）个候选的长按菜单；
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
        let Some(&(_, op)) = CANDIDATE_MENU_ACTIONS.iter().find(|(a, _)| *a == action) else {
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
    /// ⚠ `system_dark` **必须由宿主给**：核心的系统明暗探测只实现了 Windows 与 macOS，
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

    /// **键面字符 → 按法** `(字符, VK, 是否按 Shift)`。宿主启动时取一次。
    ///
    /// 含可打印 ASCII 与中文标点两部分，后者登记的是其 ASCII 源键的按法——引擎那边
    /// 不存在直接产出中文标点的键。详见 `key_convert::char_strokes`。
    ///
    /// 软键盘手里只有「用户点了键面上的哪个字符」，引擎的入口却是键事件。宿主自己写
    /// 这张表就是第二份真相源，漏掉的字符会**绕过引擎直接上屏**——智能符号、标点配对、
    /// 自定义标点映射、中英标点切换对它统统失效，而设置页里那些开关照样摆着、
    /// 点了没反应也不报错。
    pub fn char_strokes(&self) -> Vec<(char, u32, bool)> {
        wind_coordinator::key_convert::char_strokes()
    }

    /// 可作**引导键**的符号键：`(规范名, 该键的字符)`。
    ///
    /// 触发键类配置（`input.temp_pinyin.trigger_keys` 等）存的是**键名**（如 `"backtick"`），
    /// 而界面要显示的是那个键长什么样（`` ` ``）。两者的对照表由核心给：设置端自己拼一份的话，
    /// 核心增删引导键时它不会跟着改，新键的选项永远不出现（`symbol_keys` 的文档里记着这条）。
    ///
    /// 宿主的第二个用途是**把名字还原成按键**：名字 → 字符 → [`Self::char_strokes`] 里的按法。
    /// 于是「点工具栏图标进入某模式」与「上滑打出那个符号进入」走的是同一条路——
    /// 模式的进入语义（写引导符进编码区等）只有引擎那一份实现。
    pub fn symbol_keys(&self) -> Vec<(String, char)> {
        wind_keys::keymap::symbol_keys()
            .map(|(name, ch)| (name.to_string(), ch))
            .collect()
    }

    // ── 配置读写（设置页）────────────────────────────────────────────
    //
    // 这三个口一起构成设置页的数据面：注册表说「有哪些键、什么类型」，快照说
    // 「现在各是什么值」，写入把用户的选择落到用户层。落盘的所有讲究都在
    // `Config::set_user_value` 里，本层不自己写一行——理由见 `config_api` 模块文档。

    /// 当前生效配置的全量快照（TOML 文本），含移动端强制改写项。
    pub fn config_snapshot(&self) -> String {
        config_api::snapshot(&self.data_dir)
    }

    /// 出厂默认配置的全量快照（TOML 文本）。设置页据此标注默认值、实现「恢复默认」。
    pub fn config_defaults(&self) -> String {
        config_api::defaults(&self.data_dir)
    }

    /// core 的配置字段注册表：键 → 类型（+ 枚举值域）。
    ///
    /// 宿主用它校验自己的设置清单。清单里写错一个键的后果是「这个开关点了没反应」
    /// 且不报错，把注册表交出去就能在加载清单时当场比对出来。
    pub fn config_registry(&self) -> Vec<config_api::ConfigFieldInfo> {
        config_api::registry()
    }

    /// 写一个配置键到用户层。**只写盘**，生效要另发
    /// [`MobileCommand::ReloadConfig`]——分开是为了让宿主把连改若干项合成一次重载。
    pub fn config_set(&self, key: &str, value: config_api::ConfigValue) -> Result<(), String> {
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

    /// 启用 / 停用一个已装方案。英文与最后一个中文方案关不掉（Err 里写原因）。
    /// 只写盘，生效要另发 [`MobileCommand::ReloadConfig`]。
    pub fn set_schema_enabled(&self, id: &str, on: bool) -> Result<(), String> {
        config_api::set_schema_enabled(id, on)
    }

    /// 打开 / 关闭一段候选提示（如「拆字」）。只写盘，生效要另发 [`MobileCommand::ReloadConfig`]。
    pub fn set_tooltip_section_enabled(&self, label: &str, on: bool) -> Result<(), String> {
        config_api::set_tooltip_section_enabled(&self.data_dir, label, on)
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

    /// 全部快捷短语（系统 + 用户的叠加视图，按编码升序）。无存储时为空。
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
    /// 先查，是为了把**原因**（要升级 / 要从备份还原）原样交给用户，而不是一句看不懂的
    /// 「无持久化存储」。
    fn ensure_writable(&self) -> Result<(), String> {
        match self.coord.storage_readonly_reason() {
            Some(r) => Err(r.to_string()),
            None => Ok(()),
        }
    }

    /// 可选双拼布局。清单由核心扫描目录得出，**宿主不要硬编码**——
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

/// 把用户目录与**安装根**钉进 App 私有路径。
///
/// TODO(上游): wind-config 支持显式目录注入后移除环境变量挂载。
/// `dirs` crate 在 unix 系按 XDG 变量解析，移动端进程私有目录不在默认位置上。
///
/// # 为什么安装根也必须钉
///
/// `Config::data_dir()` = `install_root()/data`，而 `install_root()` 在没有
/// `WIND_INSTALL_ROOT` 时取 `current_exe().parent()`——安卓上那是
/// `/system/bin`，于是核心**找不到出厂 config.toml**。
///
/// 后果不是报错，是一个功能整体静默失效：`set_user_value` 的「等于出厂默认就不落盘」
/// 那道收口靠 `preset_for_pruning()` 拿出厂默认做比对，取不到时安全降级为「照常写入」。
/// 桌面上那是偶发降级，安卓上是**永久**的——每一次保存都会把当前默认钉死进用户层，
/// 将来核心改默认值，这些用户一个也收不到（`schema.mix.auto_commit_block_on_pinyin`
/// 就是这么引爆的，真机一份配置 105 键里 62 键是这样来的）。
///
/// 它同时让「恢复默认」看起来没生效：写回默认值本该把该键从用户层删掉，降级后变成
/// 老老实实写一遍，用户层里那个键一直在。
///
/// ⚠ 传入的 `data_dir` 必须就是 `<安装根>/data`——这是核心那边的固定关系，
/// 不是本函数能选择的。目录名不对时只警告不猜：猜错会让核心去另一个目录读出厂配置，
/// 比找不到更难查。
///
/// # 关于 `WIND_INSTALL_ROOT` 标着"生产严禁设置"
///
/// 那条告诫针对的是桌面：那里 `current_exe().parent()` 本就是正确答案，设它只会把
/// 安装根指到别处。移动端没有这个前提——`current_exe()` 是 `/system/bin/app_process64`，
/// 与 App 私有目录毫无关系，不设它就没有任何正确答案。
///
/// 这与上面那三个 XDG 变量属于同一件事，共用同一个上游 TODO：**wind-config 支持显式
/// 目录注入后，这四行一起删掉**。在那之前，移动端进程只有本函数一个写入方，
/// 也只有一个核心实例，不存在两处争抢的问题。
///
/// 顺带确认过的一个连带风险：`is_portable()` 也读这个根，判据是根目录下有
/// `portable_mode` 标记文件。App 私有的 `filesDir` 下只有 `data`/`user`/`rust`
/// 三个目录，不会有这个文件；真有的话用户配置会改落 `filesDir/userdata`。
fn mount_dirs(data_dir: &Path, user_root: &Path) {
    let root = user_root.display();
    unsafe {
        std::env::set_var("HOME", user_root);
        std::env::set_var("XDG_CONFIG_HOME", format!("{root}/config"));
        std::env::set_var("XDG_DATA_HOME", format!("{root}/data"));
        std::env::set_var("XDG_CACHE_HOME", format!("{root}/cache"));
    }
    match (data_dir.file_name(), data_dir.parent()) {
        (Some(name), Some(install_root)) if name == "data" => unsafe {
            std::env::set_var("WIND_INSTALL_ROOT", install_root);
        },
        _ => tracing::warn!(
            "数据目录 {} 不是 `<安装根>/data` 的形状，出厂默认取不到：             配置剪枝与「恢复默认」会退化为照常写入",
            data_dir.display()
        ),
    }
}

/// 反向通道泵：`Receiver<UiCommand>` → [`MobileEventSink`] 回调。
fn spawn_ui_pump(
    rx: std::sync::mpsc::Receiver<UiCommand>,
    sink: Arc<dyn MobileEventSink>,
    warming: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("wind-ui-pump".into())
        .spawn(move || {
            for cmd in rx {
                // 预热喂的假按键会产生候选帧，丢掉——否则键盘刚起来会闪一下候选。
                // 只挡候选类，状态/提示类照常（预热不改模式，本来也不会产生）。
                if warming.load(Ordering::Relaxed)
                    && matches!(
                        cmd,
                        UiCommand::UpdateCandidates { .. } | UiCommand::HideCandidates
                    )
                {
                    continue;
                }
                match cmd {
                    UiCommand::UpdateCandidates {
                        preedit,
                        preedit_caret,
                        mode_label,
                        candidates,
                        selected,
                        page,
                        total_pages,
                        ..
                    } => {
                        sink.on_candidates(CandidateFrame {
                            preedit,
                            preedit_caret,
                            mode_label,
                            items: candidates.into_iter().map(|c| c.text).collect(),
                            selected,
                            page,
                            total_pages,
                        });
                    }
                    UiCommand::HideCandidates => sink.on_hide_candidates(),
                    UiCommand::UpdateToolbar(st) => {
                        sink.on_status(InputStatus {
                            chinese_mode: st.chinese_mode,
                            icon_label: st.icon_label,
                            full_width: st.full_width,
                            chinese_punct: st.chinese_punct,
                            s2t_enabled: st.s2t_enabled,
                            s2t_shown: st.s2t_shown,
                        });
                    }
                    // 状态提示气泡在桌面是独立浮窗，移动端并入 toast 一条通路
                    UiCommand::ShowToast { text, .. } | UiCommand::ShowStatusTip { text, .. } => {
                        sink.on_toast(text)
                    }
                    _ => {}
                }
            }
        })
        .expect("spawn wind-ui-pump");
}

/// 扫描**各资源层**（user / custom / data）的 `schemas/*.schema.toml`，返回已安装的方案 id。
///
/// 与桌面端 `EngineManager::installed_schemas` 同语义：**合并去重**，各层都贡献 id
/// （不是「靠前层胜出」——那是双拼布局的语义）。层序见
/// [`wind_config::Config::resource_layers_with`]。
///
/// 顺序按文件名排序以保证**稳定**：方案下标要回送给引擎选择（`SelectSchema`），
/// 顺序随目录遍历漂移会让「上次选的第 2 个」这次指向另一个方案。
fn scan_installed_schemas(data_dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for base in Config::resource_layers_with(Some(data_dir)) {
        let Ok(entries) = std::fs::read_dir(base.join("schemas")) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".schema.toml") {
                // 定制版 `[schemas] hide` 删掉的方案在任何层都不存在（与桌面端
                // `EngineManager::scan_layer_schema_ids` 同判据）。不滤的话移动端方案列表里
                // 会留一个选了也切不过去的空条目——引擎侧 `read_schema` 早已对它返回 None。
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

/// 喂键。**不再喂合成 caret**：核心侧 `set_caret_independent(true)` 已让首显闸门
/// 对自绘候选的宿主直接放行。此前这里必须编造一组非零坐标去骗过闸门
/// （`height` 写 0 还会被判为「宿主尚未 reflow」整帧丢弃，候选一次都不下发）。
fn dispatch_key(coord: &Coordinator, vk: u32, modifiers: u32) -> BridgeKeyAction {
    coord.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        // ⚠ 必须原样透传，**不能写死 0**。此前这里是 0，而吃键判定
        // （`should_handle_key`）却收到了真实的 modifiers——判定看得见 Shift、
        // 派发看不见，两边对同一次按键的认知不一致。
        //
        // 后果不是崩溃而是功能缺失：宿主为了让 Shift+字母出大写，只能在 K 侧
        // 绕过引擎直接上屏一个大写字母，于是中文输入中途按 Shift 会**打断组合**
        // ——用户看到的是「按了 Shift 就只出一个英文字母」。修好这里之后，
        // Shift 的语义交还给核心按键链（它本就有完整的修饰键转换，见 key_convert.rs）。
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    })
}

/// 词库门面：形状转换（显示码、`u64` 总数）与只读闸。存储语义本身在 wind-coordinator 的
/// `tests/user_words_api.rs` 验，这里不重复。
#[cfg(test)]
mod user_words_tests {
    use super::*;

    /// 直接拼 [`MobileCore`]：`MobileCore::new` 要挂环境变量与出厂目录，测这几个口用不着。
    /// ⚠️ 目录名带 pid：多会话并行跑测试时固定名会互删夹具。
    fn core(tag: &str, meta: Option<&str>) -> (MobileCore, std::path::PathBuf) {
        let user =
            std::env::temp_dir().join(format!("wind_mobile_uw_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        if let Some(m) = meta {
            std::fs::write(user.join("meta.toml"), m).unwrap();
        }
        let (coord, _rx) =
            Coordinator::new_headless_with_ui_at(Config::default(), None, Some(&user));
        let core = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        (core, user)
    }

    #[test]
    fn add_list_search_remove_clear() {
        let (m, dir) = core("crud", None);
        m.user_word_add("pinyin", "ni hao", "你好").unwrap();
        m.user_word_add("pinyin", "wo men", "我们").unwrap();

        let page = m.user_words("pinyin", "", 0, 50);
        assert_eq!(page.total, 2);
        assert_eq!(
            page.items[0],
            UserWord {
                code: "ni hao".into(),
                text: "你好".into(),
                weight: USER_WORD_ADD_WEIGHT,
            },
            "显示码带音节空格、权重取桌面加词默认值"
        );

        let hits = m.user_words("pinyin", "们", 0, 50);
        assert_eq!((hits.total, hits.items[0].text.as_str()), (1, "我们"));

        // 列表给的显示码原样回传即可删掉
        m.user_word_remove("pinyin", &page.items[0].code, "你好")
            .unwrap();
        assert_eq!(m.user_words("pinyin", "", 0, 50).total, 1);

        assert_eq!(m.user_words_clear("pinyin"), Ok(1));
        assert_eq!(m.user_words("pinyin", "", 0, 50), UserWordPage::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 预览走候选同一条求值：日期模板按此刻展开、组展开成各项、纯文本原样。
    #[test]
    fn phrase_preview_expands_like_a_candidate() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 10, 21, 9, 5, 0)
            .unwrap();
        assert_eq!(
            phrase_preview("date", "$Y年$M月$D日", now),
            "2026年10月21日"
        );
        assert_eq!(phrase_preview("dz", "北京市朝阳区", now), "北京市朝阳区");
        assert_eq!(
            phrase_preview("zzhf", r#"$SS("常用回复", "好的", "收到")"#, now),
            "好的 · 收到",
            "组打全码时展开成各项"
        );
        assert_eq!(
            phrase_preview("x", r#"$SS("g", "1", "2", "3", "4", "5")"#, now),
            "1 · 2 · 3 · 4 · …",
            "多了只列前几个"
        );
    }

    /// 快捷短语：加 / 列 / 开关 / 删用户短语；只读时拒写。系统短语的叠加与出厂开关语义在
    /// wind-store 的测试里验，这里只验门面这一层。
    #[test]
    fn phrase_add_list_toggle_remove() {
        let (m, dir) = core("phrases", None);
        m.phrase_add(" dz ", " 北京市朝阳区 ").unwrap();
        let mine = |m: &MobileCore| {
            m.phrases()
                .into_iter()
                .filter(|p| p.code == "dz")
                .collect::<Vec<_>>()
        };
        assert_eq!(
            mine(&m),
            [Phrase {
                code: "dz".into(),
                text: "北京市朝阳区".into(),
                enabled: true,
                is_system: false,
                preview: "北京市朝阳区".into(),
            }],
            "编码与内容去掉首尾空白、默认启用、归属用户"
        );
        m.phrase_set_enabled("dz", "北京市朝阳区", false).unwrap();
        assert!(!mine(&m)[0].enabled);
        m.phrase_remove("dz", "北京市朝阳区").unwrap();
        assert!(mine(&m).is_empty());
        assert!(m.phrase_add("dz", "  ").is_err(), "空内容拒收");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 暂停学习只是转发：开关落到协调器上（学习路径的覆盖在 wind-coordinator 的
    /// `tests/learning_suspended.rs` 验）。
    #[test]
    fn learning_suspended_forwards_to_coordinator() {
        let (m, dir) = core("suspend", None);
        assert!(!m.coord.learning_suspended(), "默认不暂停");
        m.set_learning_suspended(true);
        assert!(m.coord.learning_suspended());
        m.set_learning_suspended(false);
        assert!(!m.coord.learning_suspended());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 只读模式：写口带着原因拒绝；读口照约定返回空页（不 panic）。
    #[test]
    fn readonly_rejects_writes_with_reason() {
        let (m, dir) = core("ro", Some("format_version = 99\ndir_id = \"x\"\n"));
        let err = m.user_word_add("pinyin", "ni hao", "你好").unwrap_err();
        assert!(err.contains("99"), "要把只读原因交给用户: {err}");
        assert!(m.user_word_remove("pinyin", "ni hao", "你好").is_err());
        assert!(m.user_words_clear("pinyin").is_err());
        assert_eq!(m.user_words("pinyin", "", 0, 50), UserWordPage::default());
        assert!(m.phrase_add("dz", "地址").is_err(), "短语写入同样受只读闸");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 词库导出 / 导入门面：往返、跨引擎类型拒收、Rime/TSV、只读闸、导完立即打得出。
/// 段交集与准入契约的本体在 wind-coordinator 的 `dict_transfer_api`，桌面 JSON 形状在
/// wind-webdata 的测试里验，这里只验门面这一层。要 `build_dev/data` 的用例缺失时打印跳过。
#[cfg(test)]
mod dict_transfer_tests {
    use super::*;

    fn data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    fn dict_ready() -> bool {
        data_dir()
            .join("schemas/wubi86/wubi86_jidian.dict.yaml")
            .exists()
            && data_dir().join("schemas/pinyin.schema.toml").exists()
    }

    /// 带真词库的 [`MobileCore`]，当前方案五笔。⚠️ 目录名带 pid：多会话并行跑测试时固定名会互删夹具。
    fn core(tag: &str) -> (MobileCore, std::path::PathBuf) {
        let user =
            std::env::temp_dir().join(format!("wind_mobile_dt_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.auto_commit_at_full = false;
        let (coord, _rx) =
            Coordinator::new_headless_with_ui_at(cfg, Some(&data_dir()), Some(&user));
        let core = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        (core, user)
    }

    /// 打一串码，返回全部候选文本，再 Esc 清掉组合。
    fn type_code(m: &MobileCore, code: &str) -> Vec<String> {
        for ch in code.chars() {
            dispatch_key(&m.coord, ch.to_ascii_uppercase() as u32, 0);
        }
        let items = m.candidates(0, MAX_WINDOW).items;
        dispatch_key(&m.coord, 0x1B, 0);
        items
    }

    /// 加词 → 导出（头部带 engine_type、含该词）→ 清空 → 导入导出的文本 → 词回来且立即打得出。
    #[test]
    fn export_clear_import_round_trip_is_typable() {
        if !dict_ready() {
            eprintln!("!!! 跳过：build_dev 词库不存在");
            return;
        }
        let (m, dir) = core("rt");
        // 先打一遍把这个码的候选算进缓存——验的正是导入后缓存会不会挡住新词。
        assert!(
            !type_code(&m, "aaaa").iter().any(|t| t == "风起测词"),
            "前提：词库里没有"
        );
        m.user_word_add("wubi86", "aaaa", "风起测词").unwrap();

        let text = m.dict_export("wubi86").unwrap();
        assert!(
            text.contains("engine_type: codetable"),
            "头部要写引擎类型，导入端据此拒收跨类型文件:\n{text}"
        );
        assert!(text.contains("风起测词"), "导出要含刚加的词:\n{text}");

        assert_eq!(m.user_words_clear("wubi86"), Ok(1));
        assert!(!type_code(&m, "aaaa").iter().any(|t| t == "风起测词"));

        let r = m.dict_import("wubi86", &text, false).unwrap();
        let uw = r
            .sections
            .iter()
            .find(|s| s.key == "userWords")
            .expect("要有用户词段");
        assert_eq!((uw.added, uw.skipped, uw.rejected), (1, 0, 0), "{r:?}");
        assert_eq!(m.user_words("wubi86", "", 0, 50).total, 1);
        let after = type_code(&m, "aaaa");
        assert!(
            after.iter().any(|t| t == "风起测词"),
            "导完立即可打，实得 {after:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 五笔导出的文件导入拼音方案：按引擎类型拒收，且一条都不落库。
    #[test]
    fn codetable_file_rejected_by_pinyin_schema() {
        if !dict_ready() {
            eprintln!("!!! 跳过：build_dev 词库不存在");
            return;
        }
        let (m, dir) = core("cross");
        m.user_word_add("wubi86", "aaaa", "风起测词").unwrap();
        let text = m.dict_export("wubi86").unwrap();
        let err = m.dict_import("pinyin", &text, false).unwrap_err();
        assert!(err.contains("码表") && err.contains("拼音"), "{err}");
        assert_eq!(m.user_words("pinyin", "", 0, 50).total, 0, "拒收不落库");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 第三方纯文本：Rime（`---`/`...` 头，`词\t编码`）与 TSV（`编码\t词`）都只进用户词。
    #[test]
    fn rime_and_tsv_text_import_into_user_words() {
        if !dict_ready() {
            eprintln!("!!! 跳过：build_dev 词库不存在");
            return;
        }
        let (m, dir) = core("rime");
        let rime = "---\nname: t\nversion: \"1\"\n...\n风起甲词\taaaa\t100\n风起乙词\taaab\n";
        let r = m.dict_import("wubi86", rime, false).unwrap();
        assert_eq!(r.sections.len(), 1, "{r:?}");
        assert_eq!(
            (r.sections[0].key.as_str(), r.sections[0].added),
            ("userWords", 2)
        );
        assert!(type_code(&m, "aaab").iter().any(|t| t == "风起乙词"));

        let r = m.dict_import("wubi86", "aaac\t风起丙词\n", false).unwrap();
        assert_eq!(r.sections[0].added, 1, "{r:?}");

        // replace：先清空再导入
        let r = m.dict_import("wubi86", "aaad\t风起丁词\n", true).unwrap();
        assert_eq!(r.sections[0].added, 1, "{r:?}");
        let page = m.user_words("wubi86", "", 0, 50);
        assert_eq!(
            page.items
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>(),
            ["风起丁词"],
            "replace 要先清空"
        );

        // 拼音方案：无空格的码过准入契约，由程序补上音节边界（显示码带空格即证）。
        let r = m
            .dict_import("pinyin", "fengqihaha\t风起哈哈\n", false)
            .unwrap();
        assert_eq!(
            (r.sections[0].added, r.sections[0].boundary_filled),
            (1, 1),
            "{r:?}"
        );
        let page = m.user_words("pinyin", "", 0, 50);
        assert_eq!(page.items[0].code, "feng qi ha ha");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 只读模式：导入带着原因拒绝（不碰数据）；导出读不到存储也报错而不是给空文件。
    #[test]
    fn readonly_rejects_import_with_reason() {
        let user = std::env::temp_dir().join(format!("wind_mobile_dt_ro_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("meta.toml"),
            "format_version = 99\ndir_id = \"x\"\n",
        )
        .unwrap();
        let (coord, _rx) =
            Coordinator::new_headless_with_ui_at(Config::default(), None, Some(&user));
        let m = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        let err = m.dict_import("pinyin", "aa\t工\n", false).unwrap_err();
        assert!(err.contains("99"), "要把只读原因交给用户: {err}");
        assert!(m.dict_export("pinyin").is_err());
        let _ = std::fs::remove_dir_all(&user);
    }
}

/// 方案附加词库门面：字段映射、错误转成原因串、只读闸、重建后仍是新值。列表口径与
/// live 生效在 wind-coordinator 的 `tests/schema_dicts_api.rs` 验，这里只验门面这一层。
/// 要 `build_dev/data`，缺失时打印跳过。
#[cfg(test)]
mod schema_dicts_tests {
    use super::*;

    fn data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    /// 以 `root` 下的 redb 与 override 目录拼 [`MobileCore`]；同一个 `root` 再调 = 重启。
    /// 不走 `MobileCore::new`：它改进程环境变量，与并行测试互相踩。
    fn core_at(root: &Path) -> MobileCore {
        let store = Arc::new(wind_store::Store::open(root.join("s.redb")).unwrap());
        let coord = Coordinator::new_headless_with_store_override(
            Config::default(),
            Some(&data_dir()),
            store,
            Some(root.join("overrides")),
        );
        MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: data_dir(),
        }
    }

    #[test]
    fn list_toggle_persist_and_errors() {
        if !data_dir().join("schemas/pinyin.schema.toml").exists() {
            eprintln!("!!! 跳过：build_dev 词库不存在");
            return;
        }
        let root = std::env::temp_dir().join(format!("wind_mobile_sd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let idiom = |m: &MobileCore| {
            m.schema_dicts("pinyin")
                .into_iter()
                .find(|d| d.id == "cell_idiom")
        };

        let m = core_at(&root);
        assert_eq!(
            idiom(&m),
            Some(SchemaDictInfo {
                id: "cell_idiom".into(),
                label: "成语".into(),
                description: "成语、俗语与古诗文名句".into(),
                enabled: false,
            })
        );
        assert!(
            m.schema_dicts("pinyin")
                .iter()
                .all(|d| d.id != "pinyin_main"),
            "不列主库"
        );
        m.set_schema_dict_enabled("pinyin", "cell_idiom", true)
            .unwrap();
        assert!(idiom(&m).unwrap().enabled);
        drop(m);
        let m = core_at(&root);
        assert!(idiom(&m).unwrap().enabled, "重建 MobileCore 后仍开");

        let err = m
            .set_schema_dict_enabled("pinyin", "no_such_dict", true)
            .unwrap_err();
        assert!(err.contains("no_such_dict"), "原因要点名: {err}");
        assert!(
            m.set_schema_dict_enabled("nope", "cell_idiom", true)
                .is_err()
        );
        assert!(m.schema_dicts("nope").is_empty(), "未知方案列空、不 panic");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 只读模式：带着原因拒绝，且先于方案校验（不论方案在不在）。
    #[test]
    fn readonly_rejects_with_reason() {
        let user = std::env::temp_dir().join(format!("wind_mobile_sd_ro_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("meta.toml"),
            "format_version = 99\ndir_id = \"x\"\n",
        )
        .unwrap();
        let (coord, _rx) =
            Coordinator::new_headless_with_ui_at(Config::default(), None, Some(&user));
        let m = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        let err = m
            .set_schema_dict_enabled("pinyin", "cell_idiom", true)
            .unwrap_err();
        assert!(err.contains("99"), "要把只读原因交给用户: {err}");
        let _ = std::fs::remove_dir_all(&user);
    }
}

/// 候选长按菜单门面：扁平化、action 映射、准入。菜单内容本身与绝对下标执行在
/// wind-coordinator 的 `tests/candidate_menu_abs_index.rs` 验，这里只验门面这一层。
#[cfg(test)]
mod candidate_menu_tests {
    use super::*;

    #[test]
    fn flatten_keeps_op_and_copy_leaves_and_inlines_submenus() {
        let items = vec![
            MenuItemSpec::leaf("置顶", MenuKind::Op(CandidateOp::MoveTop), false, false),
            MenuItemSpec::separator(),
            MenuItemSpec::label("标题"),
            MenuItemSpec::leaf(
                "命令",
                MenuKind::Command(MenuCmd::OpenSettings),
                true,
                false,
            ),
            MenuItemSpec {
                label: "更多".into(),
                kind: MenuKind::Submenu,
                enabled: true,
                checked: false,
                children: vec![MenuItemSpec::leaf(
                    "设为生僻字",
                    MenuKind::Op(CandidateOp::ToggleCommon),
                    true,
                    false,
                )],
            },
            MenuItemSpec::leaf("复制", MenuKind::Copy, true, false),
        ];
        let mut out = Vec::new();
        flatten_candidate_menu(&items, &mut out);
        let got: Vec<(&str, &str, bool)> = out
            .iter()
            .map(|i| (i.label.as_str(), i.action.as_str(), i.enabled))
            .collect();
        assert_eq!(
            got,
            [
                ("置顶", "move_top", false),
                ("设为生僻字", "toggle_common", true),
                ("复制", "copy", true),
            ]
        );
    }

    /// 真词库下走一遍门面：第二页候选取得到菜单、置顶生效；copy / 未知 action / 灰显项 /
    /// 越界都返回 false 或空。词典缺失时跳过（计数照绿）。
    #[test]
    fn menu_and_run_by_absolute_index() {
        let data =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
            eprintln!("跳过：词库不存在");
            return;
        }
        let user = std::env::temp_dir().join(format!("wind_mobile_cm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.auto_commit_at_full = false;
        let (coord, _rx) = Coordinator::new_headless_with_ui_at(cfg, Some(&data), Some(&user));
        let m = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        assert!(m.candidate_menu(0).is_empty(), "无候选时菜单为空");
        for ch in ['A', 'A'] {
            dispatch_key(&m.coord, ch as u32, 0);
        }
        let pp = m.coord.debug_page_texts().len();
        let all = m.candidates(0, MAX_WINDOW);
        assert!(all.total > pp + 1, "前提：aa 有第二页");
        let n = pp + 1;
        let target = all.items[n].clone();

        let menu = m.candidate_menu(n);
        let actions: Vec<&str> = menu.iter().map(|i| i.action.as_str()).collect();
        assert_eq!(actions.first(), Some(&"move_top"), "{menu:?}");
        assert_eq!(actions.last(), Some(&"copy"), "{menu:?}");
        assert!(!actions.contains(&""), "分隔线不应出现: {menu:?}");

        assert!(!m.candidate_menu_run(n, "copy"), "复制归宿主");
        assert!(!m.candidate_menu_run(n, "bogus"), "未知 action");
        assert!(
            !m.candidate_menu_run(0, "move_top"),
            "首位置顶是灰的，不执行"
        );
        assert!(!m.candidate_menu_run(all.total, "move_top"), "越界");
        assert!(m.candidate_menu(all.total).is_empty(), "越界菜单为空");

        assert!(m.candidate_menu_run(n, "move_top"));
        assert_eq!(m.candidates(0, 1).items, [target], "置顶后成首选");
        let _ = std::fs::remove_dir_all(&user);
    }
}

/// 候选提示门面：与桌面气泡同一份段模板。段内容本身在 wind-coordinator 的 tooltip 测试里验，
/// 这里只验「按绝对下标取得到、越界为空、编码段在索引建好后出现」。
#[cfg(test)]
mod candidate_tooltip_tests {
    use super::*;

    #[test]
    fn tooltip_by_absolute_index() {
        let data =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
            eprintln!("跳过：词库不存在");
            return;
        }
        let user = std::env::temp_dir().join(format!("wind_mobile_tip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.auto_commit_at_full = false;
        let (coord, _rx) = Coordinator::new_headless_with_ui_at(cfg, Some(&data), Some(&user));
        let m = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        assert!(m.candidate_tooltip(0).is_empty(), "无候选时为空");
        for ch in ['W', 'Q', 'V', 'B'] {
            dispatch_key(&m.coord, ch as u32, 0);
        }
        let all = m.candidates(0, MAX_WINDOW);
        assert!(all.total > 0, "前提：wqvb 有候选");
        assert!(m.candidate_tooltip(all.total).is_empty(), "越界为空");
        // 编码段靠反查索引，第一次取时才在后台建：轮询到它出现（模拟器外通常一两百毫秒）
        let mut code_line = None;
        for _ in 0..100 {
            let tip = m.candidate_tooltip(0);
            assert!(
                tip.iter().all(|s| !s.lines.is_empty()),
                "段不应为空: {tip:?}"
            );
            code_line = tip
                .iter()
                .find(|s| s.title.as_deref().is_some_and(|t| t.starts_with("编码")))
                .map(|s| s.lines.join(" "));
            if code_line.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let code_line = code_line.expect("编码段应在索引建好后出现");
        assert!(
            code_line.contains("wqvb"),
            "首选的编码里应有 wqvb: {code_line}"
        );
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 候选窗带注释：五笔打 a，有剩余编码的候选注释非空；不带注释的接口恒空、文本两边一致。
    #[test]
    fn window_with_comments_matches_items() {
        let data =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
            eprintln!("跳过：词库不存在");
            return;
        }
        let user = std::env::temp_dir().join(format!("wind_mobile_cmt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.auto_commit_at_full = false;
        let (coord, _rx) = Coordinator::new_headless_with_ui_at(cfg, Some(&data), Some(&user));
        let m = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        dispatch_key(&m.coord, 'A' as u32, 0);
        let plain = m.candidates(0, 20);
        let rich = m.candidates_with_comments(0, 20);
        assert!(plain.comments.is_empty(), "不带注释的接口不该算注释");
        assert_eq!(plain.items, rich.items, "两个接口的候选文本应一致");
        assert_eq!(rich.comments.len(), rich.items.len(), "注释与候选一一对应");
        assert!(
            rich.comments.iter().any(|c| !c.is_empty()),
            "五笔单码 a 的候选里应有带剩余编码的: {:?}",
            rich.items.iter().zip(&rich.comments).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 滑到尾部再取：五笔单码首批 100 条（has_more），要第 90～150 条时核心扩容重建、
    /// 标 rebuilt，总数变多；要的范围在已加载之内时不重建。
    #[test]
    fn window_grows_past_loaded_candidates() {
        let data =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
            eprintln!("跳过：词库不存在");
            return;
        }
        let user = std::env::temp_dir().join(format!("wind_mobile_grow_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        cfg.schema.codetable.auto_commit_at_full = false;
        let (coord, _rx) = Coordinator::new_headless_with_ui_at(cfg, Some(&data), Some(&user));
        let m = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        dispatch_key(&m.coord, 'A' as u32, 0);
        let first = m.candidates(0, 24);
        assert!(!first.rebuilt, "首屏在已加载范围内，不该重建");
        let loaded = first.total;
        assert!(loaded >= 90, "前提：五笔单码首批至少 90 条，实得 {loaded}");
        let more = m.candidates(loaded - 10, 60);
        assert!(more.rebuilt, "要的范围超出已加载且还有更多，应扩容重建");
        assert!(
            more.total > loaded,
            "扩容后总数应变多：{loaded} → {}",
            more.total
        );
        assert_eq!(more.items.len(), 60, "扩容后这一段应取满");
        let again = m.candidates(0, 24);
        assert!(!again.rebuilt, "已加载范围内再取不重建");
        let _ = std::fs::remove_dir_all(&user);
    }

    /// 单字母预热只查引擎、不碰输入状态：预热完没有残留编码 / 候选，接着照常打字。
    /// 码表方案直接跳过（函数自己判引擎类型）。
    #[test]
    fn prewarm_leaves_no_composition() {
        let data =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !data.join("schemas/pinyin.schema.toml").exists() {
            eprintln!("跳过：拼音方案不存在");
            return;
        }
        let user = std::env::temp_dir().join(format!("wind_mobile_warm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        let mut cfg = Config::default();
        cfg.schema.available = vec!["pinyin".into()];
        cfg.schema.active = "pinyin".into();
        cfg.input.default.chinese_mode = true;
        let (coord, _rx) = Coordinator::new_headless_with_ui_at(cfg, Some(&data), Some(&user));
        let m = MobileCore {
            coord,
            warming: Arc::new(AtomicBool::new(false)),
            data_dir: user.clone(),
        };
        m.coord.prewarm_single_initials();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert_eq!(
            m.candidates(0, 5).total,
            0,
            "预热不该留下候选（它不走按键路径）"
        );
        dispatch_key(&m.coord, 'N' as u32, 0);
        dispatch_key(&m.coord, 'I' as u32, 0);
        assert!(m.candidates(0, 5).total > 0, "预热后照常打字出候选");
        let _ = std::fs::remove_dir_all(&user);
    }
}
