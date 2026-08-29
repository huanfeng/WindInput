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

use wind_bridge::handler::KeyAction as BridgeKeyAction;
use wind_bridge::handler::{FocusData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_host::{KeyProbe, Modifiers};
use wind_ipc::protocol::{EVENT_KEY_DOWN, FocusLostReason, caret_source};
use wind_ui_types::{MenuCmd, MenuKind, UiCommand, UiEvent};

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
        mount_user_dirs(user_root);

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
        let mut available = scan_installed_schemas(data_dir);
        if !available.iter().any(|s| s == fallback_schema) {
            available.push(fallback_schema.to_string());
        }

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

        // 编码区归候选区自绘（移动端必须如此，非偏好）：默认的 `app_inline` 是把编码
        // 塞进宿主组合区、协调器**不下发 preedit** 给候选窗。移动端没有桌面那种浮在光标
        // 旁的候选窗，编码要显示在键盘上方的编码栏里，就必须让协调器把 preedit 发出来。
        // 「配置格式三端统一、取值分端」的典型一例——项本身是共用的。
        cfg.ui.candidate.preedit_display = "candidate_top".to_string();

        // 用户数据根：redb 落在这里。**不能传 None**——那样系统短语层会整段为空
        // （详见 `new_headless_with_ui_at` 的说明），词频与自造词也不落盘。
        let (coord, rx) =
            Coordinator::new_headless_with_ui_at(cfg, Some(data_dir), Some(user_root));

        let warming = Arc::new(AtomicBool::new(true));
        spawn_ui_pump(rx, sink, Arc::clone(&warming));

        // 自绘候选条，不提供光标坐标——关掉桌面用的首显闸门等待
        coord.set_caret_independent(true);
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

        Arc::new(Self { coord, warming })
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
        });
    }

    /// 焦点丢失（对应 Android `onFinishInput`）。Thread 语义 = 真正离开，清输入态。
    pub fn focus_lost(&self) {
        self.coord.handle_focus_lost(1, FocusLostReason::Thread);
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
        self.coord
            .inject_ui_event(UiEvent::MenuAction(MenuKind::Command(menu_cmd)));
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
    pub fn candidates(&self, offset: usize, limit: usize) -> CandidateWindow {
        self.coord.candidate_window(offset, limit)
    }

    /// 按**绝对下标**选词，返回编辑指令流（与 [`Self::key_down`] 同一种形状）。
    ///
    /// 取代此前「合成数字键」的做法——那条路把选词限死在页内 1-9，第 10 个及以后
    /// 永远点不到。
    pub fn select_candidate(&self, index: usize) -> KeyOutcome {
        self.coord.select_candidate(index)
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

/// 把三类用户目录钉进 App 私有路径。
///
/// TODO(上游): wind-config 支持显式目录注入后移除环境变量挂载。
/// `dirs` crate 在 unix 系按 XDG 变量解析，移动端进程私有目录不在默认位置上。
fn mount_user_dirs(user_root: &Path) {
    let root = user_root.display();
    unsafe {
        std::env::set_var("HOME", user_root);
        std::env::set_var("XDG_CONFIG_HOME", format!("{root}/config"));
        std::env::set_var("XDG_DATA_HOME", format!("{root}/data"));
        std::env::set_var("XDG_CACHE_HOME", format!("{root}/cache"));
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
