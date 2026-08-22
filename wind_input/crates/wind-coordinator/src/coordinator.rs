//! 中央协调器
//!
//! 与 Go 版本 `wind_input/internal/coordinator/coordinator.go` 对齐。
//!
//! 职责（按键优先级链的精简核心版）：
//! - key_up：Shift 释放触发模式切换
//! - key_down 热键匹配（切换引擎 / 全半角 / 标点 / 中英）
//! - Shift 待切换、Ctrl/Alt 透传
//! - 中文模式下的编辑键（Esc/Backspace/Space/Enter/数字选词/字母累积）
//!
//! 候选生成委托给 [`EngineManager`]，运行时词频 boost + 最终排序在本层应用。

use crate::handle_mode::MixLens;
use crate::pipeline::{ModeKind, Rewind};
// 子模块（src/coordinator/ 目录）：这批切片重度访问本模块**私有**字段/函数，
// 子模块对父私有项可见，平级模块则须放开可见性——归属判据即「是否需要碰私有态」。
mod first_show;
mod langbar_icon;
mod message_handler;
mod push_config;

// 平移到子模块的项以原路径保真（handle_* 均经 `crate::coordinator::` 引用，勿改回直连）。
pub(crate) use crate::config_bundle::{ConfigBundle, schema_key_union};
pub(crate) use crate::key_convert::{
    char_to_main_vk, en_case_variants, full_width_source_char, numpad_char, numpad_to_main,
    printable_char, punct_char, wind_mods_to_win32,
};
use crate::preedit_cursor;
use crate::theme_style::ThemeStyle;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, trace, warn};
use wind_keys::keymap;

use wind_bridge::handler::*;
use wind_bridge::push::PushServer;
use wind_candidate::{Candidate, CandidateSource};
use wind_config::Config;
use wind_config::PreeditDisplay;
use wind_config::hotkey;
use wind_engine::EngineManager;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_SHIFT, MOD_SHORTCUT, calc_key_hash};
use wind_store::Store;
use wind_store::stat_collector::{StatCollector, StatEvent};
use wind_store::stats::CommitSource;
use wind_transform::fullwidth::to_full_width;
use wind_transform::punctuation::PunctuationConverter;
use wind_ui_types::CandidateItem;
use wind_ui_types::ToolbarState;
use wind_ui_types::{GlobalHotkeyEntry, UiCommand, UiEvent};
use wind_ui_types::{ToastKind, ToastPosition};

/// caret_use_top 兼容下保留给「上方显示」避让正文的最小行高（物理像素——宿主上报的
/// caret rect 本就是物理像素，此处刻意不做 dp 换算，与 `caret_offset_*` 不是同一件事：
/// 那是用户配置的校正量，这是拿宿主自己上报的物理量兜底，两者单位巧合都叫「像素」但
/// 出处不同）。微信 reflow 后的权威帧通常上报真实行高（~20px，随 DPI 缩放），直接取用；
/// 仅退化帧（height=1）落到此下限，保证上方候选窗底边抬到正文之上而不遮挡。偏大只是
/// 多留空隙，故取一个稳妥的正文行高量级。
const CARET_USE_TOP_MIN_LINE_H: i32 = 18;

/// direct_commit 顶码余码新组合的 keyup 兜底定时器时长（ms）。见 top-commit-mode 设计文档 §5。
pub(crate) const DEFERRED_COMPOSITION_FALLBACK_MS: u32 = 150;

/// 把 `caret_offset_*` 的 dp 值按显示器缩放换算成物理像素偏移。纯函数，与 DPI 查询解耦，
/// 可脱离真实系统单测——`dpi_scale_for_point` 那部分才是不可控的平台调用，两者故意分开。
fn dp_offset_to_pixels(dx_dp: i32, dy_dp: i32, scale: f32) -> (i32, i32) {
    (
        (dx_dp as f32 * scale).round() as i32,
        (dy_dp as f32 * scale).round() as i32,
    )
}

/// `apply_caret_compat` 里 dx/dy≠0 分支的完整落地逻辑（含 composition_start 同步平移），
/// 抽成接受显式 `scale` 的自由函数，好在不依赖 `dpi_scale_for_point`（`cfg(test)` 下恒
/// 1.0）的前提下，直接用非 1.0 的 scale 单测「dp 换算确实接进了这条变换」——`dp_offset_to_pixels`
/// 只验证换算数学本身，不证明它真被这里调用；两者故意分成两条覆盖面（2026-08-17 code
/// review 指出的 test-wiring gap）。
fn apply_dp_offset(data: &mut CaretData, dx_dp: i32, dy_dp: i32, scale: f32) {
    let (px_dx, px_dy) = dp_offset_to_pixels(dx_dp, dy_dp, scale);
    data.x += px_dx;
    data.y += px_dy;
    if data.composition_start_x != 0 {
        data.composition_start_x += px_dx;
    }
    if data.composition_start_y != 0 {
        data.composition_start_y += px_dy;
    }
}

/// 取屏幕点 (x, y) 所在显示器的有效 DPI 缩放（96dpi = 1.0）。
///
/// 非 Windows 平台回退 1.0 是**语义正确**而非「还没实现」：本仓 macOS 端的屏幕坐标口径
/// 本就是点（point），dp 与点在 1x/Retina 下始终 1:1（Retina 的物理像素放大在別处的
/// backing scale 里处理，不影响这层点坐标），故 1.0 无需再查。⚠️ 不要照抄 `wind-ui/src/dpi.rs`
/// 给这里补一条 `CGDisplay` Retina 分支——那是候选窗渲染用物理像素定尺寸，跟这里「点/dp
/// 之间无缩放」不是同一个问题，抄错了 macOS 会双重缩放。Windows 上失败（`GetDpiForMonitor`
/// 出错）同样回退 1.0，此时才是真的「查不到就当没有」。
///
/// `cfg(test)` 下强制回退 1.0：`cargo test` 可能在开发者本机的真实高 DPI 屏幕上跑，
/// 若测试也走真实 `GetDpiForMonitor`，断言的期望坐标会随本机屏幕缩放而漂移——同一条测试
/// 换台机器就变红。真实换算逻辑收在 [`dp_offset_to_pixels`] 里单独用显式 scale 值测。
#[cfg(all(windows, not(test)))]
fn dpi_scale_for_point(x: i32, y: i32) -> f32 {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut dpi_x: u32 = 0;
        let mut dpi_y: u32 = 0;
        if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() && dpi_y > 0 {
            return dpi_y as f32 / 96.0;
        }
        1.0
    }
}

#[cfg(any(not(windows), test))]
fn dpi_scale_for_point(_x: i32, _y: i32) -> f32 {
    1.0
}

/// 取进程 ID 对应的可执行文件名（如 "Weixin.exe"）。对齐 Go `bridge.GetProcessName`：
/// OpenProcess(QUERY_LIMITED_INFORMATION) + QueryFullProcessImageNameW，取末段文件名。
/// 失败（进程已退出/权限不足）返回空串。
#[cfg(windows)]
pub(crate) fn process_name(pid: u32) -> String {
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            // ★ 提权进程（任务管理器、注册表编辑器、以管理员身份运行的任何程序）在这里
            //   必定 ACCESS_DENIED：本服务是中完整性，目标是高完整性，`PROCESS_QUERY_
            //   LIMITED_INFORMATION` 也过不去。走快照兜底，见 process_name_via_snapshot。
            Err(_) => return process_name_via_snapshot(pid),
        };
        let mut buf = [0u16; MAX_PATH as usize];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        if ok.is_err() {
            return process_name_via_snapshot(pid);
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string()
    }
}

/// 进程名兜底：系统进程快照。**只在 `OpenProcess` 失败时调用**。
///
/// 为什么需要它：`OpenProcess` 需要对目标进程持句柄权限，跨完整性级别（本服务中完整性 →
/// 提权进程高完整性）一律拒绝。而快照 API 只是读系统进程表，不需要对任何进程有权限，
/// 提权进程的映像名照样读得到。
///
/// 实测症状（用户报告，2026-08-18）：任务管理器聚焦时进程名取空 ⇒ 匹配不到任何
/// per-app 规则、`mode_scope` 也无从推进 ⇒ 沿用上一个应用（常常是桌面）的英文策略，
/// 表现为「任务管理器套上了桌面的配置」。取空与「这个进程确实没配规则」在日志里同形，
/// 是这个缺陷难以归因的原因，故失败时补一条 WARN。
///
/// 代价：一次全系统进程枚举（实测数百微秒到数毫秒）。正常路径永不触发；且调用方
/// (`cached_proc_name`) 缓存优先，同一 pid 至多走一次。
#[cfg(windows)]
fn process_name_via_snapshot(pid: u32) -> String {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("进程名取空：pid={pid} 快照创建失败 {e:?}（per-app 规则将不匹配）");
                return String::new();
            }
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = String::new();
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
                    let n = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    found = String::from_utf16_lossy(&entry.szExeFile[..n]);
                    break;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        if found.is_empty() {
            // 进程刚退出（pid 已不在表里）也会走到这，与权限失败无法区分——两者对调用方
            // 的后果相同（规则不匹配），一条 WARN 足够定位，不必细分。
            tracing::warn!("进程名取空：pid={pid} 不在进程快照中（per-app 规则将不匹配）");
        } else {
            tracing::debug!(
                "进程名经快照兜底取得：pid={pid} name={found}（OpenProcess 被拒，通常是提权进程）"
            );
        }
        found
    }
}

/// 非 Windows（测试/交叉编译）下无进程名概念，返回空串 → 不命中任何兼容规则。
#[cfg(not(windows))]
pub(crate) fn process_name(_pid: u32) -> String {
    String::new()
}

/// 引擎一次转换请求的候选上限（boost 重排后截断到 9）
pub(crate) const ENGINE_MAX_CANDIDATES: usize = 50;

/// 临时拼音（overlay 模式）向拼音引擎取数的上限。
///
/// **为什么这里可以直接取全量、而主路径要分批**：拼音引擎的 `max_candidates` 只用于最后
/// 一步 `truncate`，召回/整句/排序全是全量做的（见 `pinyin/mod.rs`）。实测 `yi` 取 50 与取
/// 5000 的耗时（6.2ms vs 6.4ms）与峰值内存（778KB）**完全相同**——小 limit 省不到任何成本，
/// 只是把已构造好的候选丢掉。而临拼**没有翻页扩容通路**（`expand_candidates` 的守卫比对的是
/// `input_buffer`，临拼的码在 `temp_pinyin_buffer` 里），一次取不全就永远取不到：
/// 这正是「临拼下 `ying` 打不出「瑩」（该字在第 158 位）」的成因。
///
/// 取全量后翻页天然可穷尽——翻页只是对 `state.candidates` 切片，无需重新查询。
/// 实测拼音候选上界为 916（`yi`），5000 留足余量。
///
/// ⚠️ **该值只对拼音类引擎安全**。码表单字母候选可达 5472 条（`r`），取全量峰值 34.9MB、
/// 耗时 39.6ms，绝不可用；故取数前须按目标方案的引擎类型分流（见 `temp_pinyin_limit`）。
pub(crate) const TEMP_PINYIN_MAX_CANDIDATES: usize = 5000;

/// 自动造词（L）写入临时层的初始权重（保守默认，低于手动加词；后续可接 schema.learning 配置）。
/// 复选次数只用于晋升判定（见 `Store::learn_temp_word`），不再驱动权重增长——
/// 晋升入用户词库时统一取 `wind_store::temp_words::PROMOTED_WEIGHT`。
pub(crate) const LEARN_ADD_WEIGHT: i32 = 800;

/// 自提交宽限期：本输入法吐字后这段时间内收到的 `SelectionChanged` 视为宿主回声，
/// 不当作用户移动光标（见 `handle_selection_changed`）。
///
/// **已由真机日志校准**（2026-07-20，记事本/Chrome/EverEdit 混合样本 n≈280）：
/// - 自提交回声：3.6 ~ 10.7ms，离群值 62.9ms / 78.9ms
/// - 用户真实光标移动：最小 322.8ms，其余 453ms / 828ms / 1.4s / 70s
///
/// 两类之间 79ms→323ms 是一段空白，200ms 落在正中，上下均有 2.5 倍以上余量。
/// 取值过小 → 回声被误判为用户操作，序列被切碎、造词失效；取值过大 → 用户上屏后
/// 短时间内的真实光标移动漏掉一次终止（由 idle 超时兜底）。
/// 重新校准方法：把 `handle_selection_changed` 的 TRACE 打开，重跑分布。
pub(crate) const SELF_COMMIT_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// 首显长兜底：坐标不可信时等待权威坐标的上限。
///
/// 两个用处同一语义——「这一帧的坐标值得等，因为手里那份不能用」：
/// - `handle_caret_pending`：宿主明说「组合刚起、坐标待定」（`wait` 档）；
/// - `fire_pending_first_show`：`fast` 档短兜底到期，但坐标缓存未经当前插入点验证。
///
/// 取值来自 `wait` 档既有行为（长期作默认档，用户未反馈过「候选窗要等半秒」）。实测
/// Excel 首次输入建单元格编辑上下文需 454ms、真坐标 558ms 到达，是已知最慢的一档。
pub(crate) const FIRST_SHOW_LONG_FALLBACK_MS: u64 = 600;

/// 当前 unix 秒（拼音衰减分以此对 last_used 计龄；与 store record_freq 同口径）。
pub(crate) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 协调器输入状态
/// 检索范围过滤模式（与 Go config.FilterMode 对齐）：(模式, 菜单显示名)
pub(crate) const FILTER_MODES: [(wind_candidate::FilterMode, &str); 3] = [
    (wind_candidate::FilterMode::Smart, "智能模式"),
    (wind_candidate::FilterMode::General, "常用字"),
    (wind_candidate::FilterMode::Gb18030, "全部字符"),
];

/// 重启信号通道（对齐 Go restartRequestCh）：菜单"重启服务"→ main 重拉进程。
static RESTART_TX: std::sync::OnceLock<std::sync::mpsc::Sender<()>> = std::sync::OnceLock::new();

/// 创建重启信号通道，返回接收端（main 在创建协调器前调用并阻塞等待）。
pub fn restart_signal() -> std::sync::mpsc::Receiver<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = RESTART_TX.set(tx);
    rx
}

/// 请求重启服务（菜单触发；向 main 发信号，由 main 释放单例并重拉自身）。
pub fn request_restart() {
    if let Some(tx) = RESTART_TX.get() {
        let _ = tx.send(());
    }
}

/// 「设置」菜单的网页配置 URL 提供者：由 main 注入（捕获 web_state 的 Weak 句柄，
/// 调用时签发 token 构造 URL）。本 crate 仅持有闭包、不依赖 wind-webapi，保持解耦；
/// 返回 None 表示未注入或 web 服务尚未就绪。
#[allow(clippy::type_complexity)]
static SETTINGS_URL_PROVIDER: std::sync::OnceLock<Box<dyn Fn() -> Option<String> + Send + Sync>> =
    std::sync::OnceLock::new();

/// 注入「设置」网页配置 URL 提供者（main 在启动 web 服务后调用一次）。
pub fn set_settings_url_provider(f: Box<dyn Fn() -> Option<String> + Send + Sync>) {
    let _ = SETTINGS_URL_PROVIDER.set(f);
}

/// 取「设置」网页配置 URL（None=未注入或服务未就绪）。
/// macOS 经 CmdOpenSettings(0x0507) 让 .app 直接启动设置应用，不走 URL/exe 路径，故仅非 macOS。
#[cfg(not(target_os = "macos"))]
pub(crate) fn settings_url() -> Option<String> {
    SETTINGS_URL_PROVIDER.get().and_then(|f| f())
}

/// 取同目录下 wind_setting 设置应用的可执行路径（None=不存在）。
/// 由当前 exe 名推导变体：wind_input[_dev].exe → wind_setting[_dev].exe，
/// 故无需感知编译期变体，正式/dev 版自动对应。
/// macOS 经 CmdOpenSettings(0x0507) 由 .app 按 bundleID 启动设置应用，不需可执行路径，故仅非 macOS。
#[cfg(not(target_os = "macos"))]
pub(crate) fn settings_app_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let stem = exe.file_stem()?.to_str()?; // wind_input 或 wind_input_dev
    let setting = stem.replacen("wind_input", "wind_setting", 1);
    let path = dir.join(format!("{setting}.exe"));
    path.exists().then(|| path.display().to_string())
}

/// ⚠️ `Default` **只在测试构建下存在**（`cfg_attr(test, ...)`）。
///
/// 生产侧一律走 `Coordinator::new` 里的显式构造：那里每个字段的初值都有来历
/// （`chinese_mode` 取配置、`toolbar_visible` 取配置、`ime_active` 必须为 false 等），
/// 而 `Default` 会把它们全给成零值。放开给生产用，早晚有人用它造出一个
/// 「中文模式关着、工具栏不显示」的状态，且完全不报错。
#[cfg_attr(test, derive(Default))]
pub(crate) struct State {
    pub(crate) chinese_mode: bool,
    pub(crate) full_width: bool,
    pub(crate) chinese_punct: bool,
    /// 简繁转换开关（运行时切换；commit 时把简体输出转繁体）
    pub(crate) s2t_enabled: bool,
    /// 检索范围过滤模式（smart/general/gb18030；运行时切换）
    pub(crate) filter_mode: wind_candidate::FilterMode,
    /// 检索范围的**临时**放宽（手动触发：末页再按翻页键 / 专用热键）。
    /// 设计见 `docs/design/smart-filter-scope-relax.md` §5。
    ///
    /// **只在内存、绝不写配置**——这是与 `set_filter_mode` 的关键区别，后者会持久化到
    /// `input.filter_mode`。本次组合结束（缓冲清空）即失效，失效收口在
    /// `handle_key_event_policed`（清空路径十几处，散点接线必漏）。
    ///
    /// 放宽时把**全部**被滤候选带 `is_scope_filtered` 标记**追加到末尾**，与自动补充同一
    /// 呈现方式（区别只在补多少：自动补到一页、手动放全部）。
    ///
    /// ⚠️ 曾设计成「按真实顺序插入，与菜单切『全部字符』所见一致」，**已否决**：翻页是线性
    /// 前进的动作，翻到末尾再翻却让新字插到第 1 页（实测 `dwi` 的字权重 8999 占三简位，正好
    /// 排到第 1 页第 2 位），视口要么跳回页首、要么原地不动，两种都突兀。菜单切换是全局持久
    /// 的换档，末页翻页是临时的渐进探索——语义不同，不必对齐呈现。
    pub(crate) scope_relaxed: bool,
    /// 用户是否开启常驻工具栏（菜单开关；与“当前是否激活”正交）。
    pub(crate) toolbar_visible: bool,
    /// 本输入法当前是否处于激活态：IME_ACTIVATED/FocusGained 置真；
    /// IME_DEACTIVATED（切换输入法）与 FocusLost 的 `Thread` reason（整个应用失去前台，
    /// 含“每应用独立输入法”下切到别的输入法的应用）置假。
    ///
    /// ⚠ 本字段只表达「本输入法是否在为某个宿主服务」，**不表达「焦点在不在可编辑控件
    /// 里」**——后者是 [`Self::has_edit_context`]。两者变化时机不同（前者随应用切换，
    /// 后者随控件切换），曾经挤在这一个布尔量里，导致应用内点到非文本框时无法表达，
    /// 工具栏永不隐藏（实测 LogExpert / 文件管理器，2026-07-26）。
    pub(crate) ime_active: bool,
    /// 焦点当前是否落在可编辑控件里。focus_gained 置真；FocusLost 的 `CtxLost` /
    /// `NoEditCtx` / `Thread` reason 置假（`DocChanged` 不动——换文档后由随后的
    /// focus_gained 或 no-edit-ctx 分支重新定夺）。
    ///
    /// 与 [`Self::ime_active`] 正交：应用还在前台、输入法仍激活，但焦点可能落在
    /// 不可输入的地方（文件列表、日志面板），此时工具栏应当隐藏。
    pub(crate) has_edit_context: bool,
    /// 「焦点确实落在一个**没有可编辑上下文**的文档上」——比 `has_edit_context` 权威。
    ///
    /// ⚠ 两者的差别是**信号权威度**，不是又多了一个负责者：
    /// · `has_edit_context` 被 `CtxLost`（DocMgr 级失焦）置假。那是**噪声层**——它回答的是
    ///   「DocMgr 走了」，不是「用户进了不可输入的地方」。用于工具栏可见性没问题：翻错了
    ///   UI 层 50ms 防抖能吸收，漏隐藏的代价也只是碍眼。
    /// · 本字段只由两个**权威**信号改写：`focus_lost(NoEditCtx)`（DLL 判定新文档确实没有
    ///   可编辑上下文）与 `focus_gained`（有可编辑上下文才会发）。
    ///
    /// 为什么必须分开：语言栏图标是持续可见的全局指示，误显「英」很刺眼，代价与工具栏
    /// 完全不对称。实测（2026-08-18）用 `has_edit_context` 驱动图标的后果——
    ///   `handle_focus_lost reason=CtxLost` → 200ms 后 `input_block → NoEditContext` → 图标变「英」，
    /// 而那次焦点根本没离开可编辑控件。这正是 C++ 那版最终学会的事（只在 gaining 分支
    /// 推进状态），我把判定收归 Rust 时**没有把这条一起带过来**。
    ///
    /// ⚠ 后续（同日）：本字段仍然维护，但 `InputBlock::NoEditContext` 已**不再让图标显英**
    /// （见 `shows_english`）——即使只由权威信号驱动，它在 Electron 类宿主上也是每分钟
    /// 数次的日常事件。字段保留是因为 tooltip 与未来的权威状态上报仍要用它。
    pub(crate) focus_no_edit_ctx: bool,
    pub(crate) caps_lock: bool,
    pub(crate) input_buffer: String,
    /// `input_buffer` 的「原始大小写」影子串：用户按 Shift+字母打出的大写只存在这里。
    /// 空 = 没有大写；与缓冲失配同样视为没有大写（见 `preedit_cursor::cased_is_valid`）。
    ///
    /// **缓冲本身恒为全小写**——引擎查询、顶码判定、词频记账、加词取码一律按它，大小写对
    /// 匹配零影响。本字段只出现在两个出口：组合区显示，以及「上屏原码」（回车/空格空码/
    /// 标点顶屏）。读写走 `preedit_cursor::BufEdit::new_cased`，勿裸改。
    pub(crate) input_buffer_cased: String,
    /// 编码区光标：`input_buffer` 内的字节偏移，定义域 `[0, input_buffer.len()]`。
    /// 恒指向剩余编码内部——已转换前缀（`committed_text`）是只读前缀，光标进不去（Home 只到
    /// 剩余编码开头）。光标**不参与引擎查询**：`update_candidates` 恒查整串，移动光标不重算
    /// 候选（对齐 Go `inputCursorPos`）。所有读写走 `preedit_cursor::BufEdit`，勿裸改。
    pub(crate) input_cursor_pos: usize,
    /// 组合区显示文本（拼音含音节分隔 "ni'hao"；码表为原始编码）。
    /// 仅显示输入码/拼音，绝不包含候选列表。
    pub(crate) preedit: String,
    /// 拼音音节拆分形态（不含已转换前缀）。供「混输高亮跟随」：高亮拼音候选 → preedit 用此
    /// 拆分串；高亮码表/五笔候选 → 用原始码（input_buffer）。空串 = 无拆分形态（码表/无拼音，
    /// 恒原始码）。每次 build_candidates 重置；非普通模式（active!=None）不读取。
    pub(crate) preedit_split_body: String,
    /// **全拼降级**的音节拆分形态（双拼方案下把击键按全拼切分，`zaijian` → `zai'jian`）。
    /// 高亮到 `is_fullpinyin_fallback` 的候选时 preedit 用它；其余情形不读。
    /// 空串 = 无此形态（非双拼 / 开关关 / 支路无产出）。每次 build_candidates 重置。
    pub(crate) preedit_fp_body: String,
    /// **简拼分段**形态（把击键按简拼候选的音节序列切开，`wbwn` → `w'b'w'n`）。
    /// 高亮到 `is_abbrev` 的候选时 preedit 用它；其余情形不读。
    /// 空串 = 无此形态（非双拼 / 无简拼候选）。每次 build_candidates 重置。
    ///
    /// 只有双拼会有值：全拼下简拼分段已经是 `preedit_split_body` 本身
    /// （见 `ConvertResult::preedit_abbrev`）。
    pub(crate) preedit_abbrev_body: String,
    /// 候选调整（shadow）规则的**归一编码**；空串 = 落回 `input_buffer`（击键原样）。
    ///
    /// 取自 `ConvertResult::shadow_code`，与 `preedit_split_body` 同生命周期（每次
    /// `build_candidates` 重置）。存在的唯一理由是双拼：`data_schema_id` 已把全拼与双拼
    /// 折叠成同一个 schema，若 key 继续取击键，双拼的 `hc` 与全拼的 `hao` 会落成两个互不
    /// 相认的键。归一后两者共享同一条规则。全拼恒空串（恒等，存量规则零迁移）。
    ///
    /// ⚠️ **读写两端必须同取此值**（`shadow_code_of`）：读端 `apply_shadow`、写端
    /// `candidate_op_scope`、菜单灰显 `shadow_has_rule` 若有一处漏改，失配是**完全静默**的
    /// ——规则写得进去、读不出来，界面毫无异常。守门测试见 `handle_candidate.rs` 的
    /// `every_shadow_read_goes_through_normalized_code`。
    pub(crate) shadow_code: String,
    /// 出简让全用：本次输入过程中**各级简码位的首选**，下标 0/1/2 = 码长 1/2/3。
    /// 值为 `(该级的码, 首选文本)`——记的是用户**实际看到的**那一条（已过 `apply_filter` /
    /// `apply_freq_rerank` / `apply_shadow`），故天然含调频与候选调整的效果。
    ///
    /// **存码而不是只按下标索引**，是因为退格不是唯一的改码方式：`input_cursor_pos` 允许在
    /// 编码区中间插入/删除，此时缓冲长度不变而码已经变了（`kht` 改成 `kxt`）。用时校验
    /// `input_buffer.starts_with(code)`，不匹配即视为无记录。
    ///
    /// 失效**不靠推送**：`input_buffer.clear()` 在协调器里有十余个散落调用点，逐个接线必漏。
    /// 改由 `build_candidates` 开头按前缀关系统一淘汰——缓冲清空、光标编辑、方案切换
    /// 全被同一条规则覆盖。
    pub(crate) shortcode_tops: [Option<(String, String)>; 3],
    pub(crate) candidates: Vec<Candidate>,
    /// 当前页内高亮候选下标（0-based，相对当前页）——键盘选中项，空格上屏的目标
    pub(crate) selected_index: usize,
    /// 当前页码（0-based）
    pub(crate) current_page: usize,
    /// 动态分级加载：当前候选对应的输入码
    pub(crate) candidate_input: String,
    /// 动态分级加载：当前加载上限
    pub(crate) candidate_limit: usize,
    /// 动态分级加载：是否可能还有更多前缀候选未加载
    pub(crate) has_more: bool,
    /// 拼音类组合区「已转换前缀」（逐步转换：选中的汉字累积于此、留在组合区不上屏，
    /// 全部转换完才整体上屏）。内部存简体原文，输出时再 s2t。仅拼音/临拼/混输文本透镜使用，
    /// 码表（五笔）选词消费整串、绝不进入此态。见 docs/redesign/pinyin-composition-enhance.md。
    pub(crate) committed_text: String,
    /// 已分步上屏的段：`(raw_code, code, text, source, boundary)`。
    /// 供退格逐段回退与完整上屏时自动造词；来源用于混输自动造词的"全段同源"归属路由（P2d）。
    ///
    /// # 为什么记两份码
    ///
    /// 两个消费者要的量纲天生不同，**不可合并**：
    /// - `raw_code` = **原始输入空间**的消费码（双拼下是击键 `hc`）。退格回退（`pop_*_seg`）
    ///   把它并回输入缓冲，故必须与缓冲同域。
    /// - `code` = **全拼语义**码（`hao`）。词频记账与自动造词（`learn_phrase_on_commit`）
    ///   用它，且 `boundary` 的位移量按 `code.len()` 算——换成双拼击键会写坏用户词库
    ///   并让音节边界位全错。
    ///
    /// 引擎侧只把 `consumed_length` 回映射到原始输入空间，`code` 刻意保持全拼语义
    /// （见 `wind_engine::pinyin` 中 `map_consumed_length` 与 Fix A 的注释）。曾因这里
    /// 只记全拼码，双拼下退格把 `hao` 并回击键缓冲 `ma` → 重解析成 `ha|o|ma` 而错乱。
    /// 非双拼场景两者恒相等。
    ///
    /// boundary = 该段 code 的音节边界（见 `wind_dict::binformat::DictEntry::boundary`）；
    /// 段自身可能是多音节整词（选「你好」→ 段码 nihao、段内边界 ni|hao），故自动造词拼接
    /// 各段时须把段内边界平移到全局位置，不能只按「一段一音节」记。
    pub(crate) committed_segs: Vec<(String, String, String, CandidateSource, u64)>,
    /// 当前激活的独占输入模式（临时拼音/快捷输入/临时英文）。`None` = 普通输入。
    /// 单点决策的唯一真相源：结构上保证同一时刻至多一个独占模式（见 `pipeline.rs`）。
    pub(crate) active: Option<ModeKind>,
    /// 各 overlay 模式组合区显示主体（= preedit 去掉只读前缀的部分），供光标位置换算。
    /// 仅临拼 / mix 需要维护——它们的主体是引擎 `preedit_display`（含插入的音节分隔符），
    /// 与缓冲不同形；临英 / 特殊 / URL 的主体恒等于自身缓冲，直接用缓冲即可（见
    /// `overlay_caret_parts`）。缓冲空时可能为 stale，但此时光标必为 0、换算不读它，无害。
    pub(crate) overlay_body: String,
    /// 临时拼音输入缓冲（拼音串）
    pub(crate) temp_pinyin_buffer: String,
    /// 临时拼音编码区光标（`temp_pinyin_buffer` 内字节偏移）。下同，各 overlay 缓冲各带一个。
    pub(crate) temp_pinyin_cursor: usize,
    /// 临时拼音目标方案 id（如 "pinyin"）
    pub(crate) temp_pinyin_schema: String,
    /// 临时拼音组合区前缀字符（触发键，如 "`"）
    pub(crate) temp_pinyin_prefix: String,
    /// 临时英文输入缓冲
    pub(crate) temp_english_buffer: String,
    /// 临时英文编码区光标（`temp_english_buffer` 内字节偏移）
    pub(crate) temp_english_cursor: usize,
    /// 临时英文前缀字符（触发键符号，如 "/"；触发键进入时非空，Shift+字母进入时为空）
    pub(crate) temp_english_prefix: String,
    /// 网址模式输入缓冲（原样累积的 URL 文本）
    pub(crate) url_buffer: String,
    /// 网址模式编码区光标（`url_buffer` 内字节偏移）
    pub(crate) url_cursor: usize,
    /// 统一夺取回退登记（仅在夺取式模式激活时为 Some，见 pipeline::Rewind）
    pub(crate) rewind: Option<Rewind>,
    /// 特殊模式编码缓冲（自带码表的查询码）
    pub(crate) special_buffer: String,
    /// 特殊模式编码区光标（`special_buffer` 内字节偏移）。
    /// 注：Go 版特殊模式**不支持**光标（尾加尾删），此处随共享层一并补齐，不再留缺口。
    pub(crate) special_cursor: usize,
    /// 当前特殊模式下标（= `EngineManager::overlay_modes()` 注册表下标；仅 active==Special 时有效）
    pub(crate) special_id: u8,
    /// 当前特殊模式的 `[overlay]` 段**快照**（进入时填、退出时清）。
    ///
    /// 快照而非每次查注册表，三个理由：
    /// 1. `comment::template_for` 返回借用 `cfg` 的 `&str`（刻意不分配），临时 Vec 借不出来；
    /// 2. 布局/注释取值在候选更新路径上，省掉每次的整表 clone；
    /// 3. ★ 注册表按 id 排序，装一个新 overlay 方案会让其后方案的下标平移——快照让
    ///    「模式进行中装了方案」不至于把当前模式的行为换成隔壁那个的。
    ///
    /// 这不是 `layout.rs` 反对的那种「进入时保存、退出时回放」：快照的是**只读配置**，
    /// 随 `active = None` 自然失效，没有需要被回放的动作，声明式重算的性质不变。
    pub(crate) overlay_spec: Option<wind_config::OverlaySpec>,
    /// 特殊模式显示态前缀（进入键符号，如 "\"；只显示不消费，组合区前缀，对齐临时拼音）
    pub(crate) special_prefix: String,
    /// 临时 mix 编码缓冲
    pub(crate) mix_buffer: String,
    /// mix 编码区光标（`mix_buffer` 内字节偏移）
    pub(crate) mix_cursor: usize,
    /// mix 模式显示态前缀（进入键符号，如 ";"；只显示不消费，组合区前缀）
    pub(crate) mix_prefix: String,
    /// 当前 mix 模式下标（= features.mix_modes 索引；仅 active==Mix 时有效）
    pub(crate) mix_id: u8,
    /// 当前候选区是「重复上屏」候选（成员 `quick_input.repeat`，空缓冲时注入上次上屏内容）。
    ///
    /// 该候选没有对应编码，只能整体上屏：选词记录、造词、标点顶屏三条路径据此绕开它。
    /// 由 `update_mix_candidates` 每次装配时重置，故任何一次输入都会自动清掉。
    pub(crate) mix_repeat: bool,
    /// 辅助码 overlay（筛选会话 + 显示基线 + 显示前缀三件套）。仅 active==AuxCode 时
    /// 有效，退出/上屏/复位一律整体 `take`/`None`（同生共死，见 `AuxCodeOverlay`）。
    /// 筛选会话状态在 `wind_aux_code::AuxCodeSession`，按键路由在 `handle_aux_code` 模块。
    pub(crate) aux_code: Option<crate::handle_aux_code::AuxCodeOverlay>,
    pub(crate) caret_x: i32,
    pub(crate) caret_y: i32,
    pub(crate) caret_height: i32,
    /// 上面这组坐标的来源（`wind_ipc::protocol::caret_source::*`）。
    ///
    /// **与坐标成对写入**——凡是写 `caret_x/y` 的地方都必须同时写它，否则来源会指向上一次的
    /// 坐标，比没有这个字段更危险。焦点气泡靠它判断「这组坐标够不够格拿来定位」：
    /// TSF 域出自当前 context，GUI 域是跨窗口的 Win32 光标，两者不是同一件东西。
    pub(crate) caret_source: i32,
    /// 菜单是否打开（打开时键盘事件转发给菜单窗口；UI 自管导航）
    pub(crate) menu_open: bool,
    /// 菜单打开时刻，供焦点路径的关闭守卫用（见 `menu_close_on_focus_change`）。
    /// **必须与 `menu_open = true` 成对写入**：漏写会让守卫读到上一次打开的时间戳，
    /// 于是刚弹出的菜单被一条迟到的焦点事件当场关掉。
    pub(crate) menu_opened_at: Option<std::time::Instant>,
    /// 菜单目标候选（页内下标 + 文本），供候选词条操作/复制
    pub(crate) menu_target_page_local: usize,
    pub(crate) menu_target_text: String,
    /// 快捷加词模式（对齐 Go addWordState）：候选窗内从最近上屏字符选字组词加入用户词库。
    /// 与 `active`（独占输入模式）正交：加词模式不处理编码输入，仅 ↑↓ 调词长 / Enter 确认。
    pub(crate) add_word_active: bool,
    /// 加词候选字符池（最近上屏字符，时间序：旧→新，末尾为最近一字）。
    pub(crate) add_word_chars: Vec<char>,
    /// 当前选取的词长（取 `add_word_chars` 末尾 N 字；0 = 无可用字符）。
    pub(crate) add_word_len: usize,
    /// 当前词自动计算的编码（拼音生成 / 码表反查；空 = 无法计算，确认时中止）。
    pub(crate) add_word_code: String,
    /// `add_word_code` 的音节边界（见 `wind_dict::binformat::DictEntry::boundary`）；
    /// 0 = 无信息（码表反查/逐字兜底）。与 code 同生同灭，入库时一并写入用户词。
    pub(crate) add_word_boundary: u64,
}

/// 智能符号模式待命态：press1 提交一个参与集合内的标点后武装，等待时限内同键 press2
/// 触发替换。对齐 Go `smartSymbol*` 字段。
#[derive(Default)]
pub(crate) struct SmartSymbolArm {
    pub(crate) armed: bool,
    /// 武装的触发键（原始英文标点字符）
    pub(crate) key: char,
    /// press1 产出的标点串（…… 为多 rune），删除数 = 其 rune 数。
    /// 正向存中文串、反向存英文串——恒等于**实际上屏的那个串**，press2 的删除数按它算。
    pub(crate) str: String,
    /// 替换方向：false=正向（press1 中文 → press2 英文，原有语义）；
    /// true=反向（press1 英文 → press2 中文）。反向来源：数字后智能标点、英文标点状态、
    /// 英文输入模式。
    pub(crate) reverse: bool,
    /// press1 当时的 `(chinese_mode, chinese_punct)` 快照。press2 要求两者都没变——三种上下文
    /// （中文标点 / 英文标点 / 英文输入模式）各有独立开关与独立产物，press1 后用户切了模式，
    /// 再按同键就该当成全新 press1，而不是在新上下文里按旧方向删字。
    pub(crate) mode_snapshot: (bool, bool),
    /// 武装时刻（None=未武装）；用于时限判定
    pub(crate) at: Option<std::time::Instant>,
    /// HoldComposition 模式下 press1 进入组合态的中文文本（用于 disarm 时清理）。
    /// DeleteReplace 模式下始终为 None。
    pub(crate) held_text: Option<String>,
    /// HoldComposition + has_input 时 press1 设为 true：已武装但调用方须先顶屏上屏候选，
    /// 再开 HoldComposition；coordinator 标点分支检测此标志并生成 CommitAndHoldComposition。
    pub(crate) hold_pending_commit: bool,
}

/// 当前焦点进程派生的 caret 兼容态，字段取自 `compat.toml` 的 `[[apps]]` 规则。
///
/// focus_gained / ime_activated 时按 `client_token` 高 32 位的 PID 解析进程名并缓存
/// （见 `update_active_compat`），避免每次 caret 更新重复 OpenProcess。
///
/// 用命名结构体而非元组：两个 bool 语义完全不同，`(u32, bool, bool)` 的 `.1`/`.2`
/// 在调用点无从分辨——本仓已有多次「下标/名字与实际语义脱节」的返工。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ActiveCompat {
    /// 已解析的焦点进程 PID（0 = 尚未解析，此时其余字段无意义）。
    pub(crate) pid: u32,
    /// 用 caret rect 的 top 而非 bottom 定位候选窗。微信等 WebView 宿主的 GetTextExt
    /// height 在 1↔20px 间跳变致 bottom 漂移，top 稳定。
    pub(crate) caret_use_top: bool,
    /// 候选窗首显策略（见 `AppCompatRule::first_show_mode`）。三档互斥。
    pub(crate) first_show_mode: wind_config::app_compat::FirstShowMode,
    /// 本进程是否配了初始状态规则（`initial_mode` / `initial_punct` 任一非空）。
    ///
    /// 用途是判定「本次焦点切换是否**进出**了规则应用」：规则的副作用必须严格限制在
    /// 规则应用的进出，不能外溢。若判据退化成「规则表非空」，那么只要用户配过任意
    /// 一条规则，**任意两个应用之间**的切换都会触发重算——`global + remember=false`
    /// （出厂默认）下这会把模式重置成配置默认，用户在 Word 手切的英文切到 Chrome
    /// 就没了，与 Everything 毫无关系。
    pub(crate) has_initial_rule: bool,
    /// 本进程的符号自动配对开关；`None` = 跟随全局 `input.auto_pair.*`。
    ///
    /// ⚠ 消费点三条，缺一即半截修复（见 `AppCompatRule::auto_pair` 的说明）：中文标点态、
    /// 英文标点流水线、以及推给 DLL 的英文配对配置——纯英文模式的配对完全在 C++ 侧独立
    /// 处理，协调器收不到那些键，只关前两条的话切到英文模式配对照旧。
    pub(crate) auto_pair: Option<bool>,
    /// 本进程的智能符号替换方案；`None` = 跟随全局 `input.symbol.smart_method`。
    pub(crate) smart_method: Option<wind_config::config::SmartMethod>,
    /// 光标坐标校正偏移（dp，96dpi 基准逻辑像素，正=右/下）。宿主报告的 caret 系统性偏移时用，
    /// 与 `caret_use_top` 在同两处消费（`apply_focus_caret` / `handle_caret_update`）。
    /// 应用时按目标点所在显示器的 DPI 换算成物理像素，见 [`Coordinator::apply_caret_compat`]。
    pub(crate) caret_offset_x: i32,
    pub(crate) caret_offset_y: i32,
}

/// 「当前焦点为什么打不出中文」——全局**唯一**的判定结果。
///
/// 2026-08-18 起由协调器独占。此前判据分散在两侧：C++ 侧算 `_bNoEditContext` /
/// `IsPasswordSuppressActive()` 驱动语言栏图标（还自带一份 200ms 迟滞），Rust 侧算
/// `password_suppress` / `has_edit_context` 驱动工具栏与输入闸——**两个负责者、两份迟滞，
/// 必然漂移**，实测出现过「图标说英文、工具栏说中文」的错位。
///
/// ⚠ **只管「显示什么」，不管「吃不吃键」**：吃键闸门必须留在 DLL 本地
/// （`CTextService::IsPasswordSuppressActive`），因为它要在 IPC **之前**给出答案。
/// 把它也搬过来就会重现「吃了再吐」丢键——DLL 吃下键、core 回 PassThrough，而
/// Chrome/Electron 一类严格 TSF 宿主不回退合成 `WM_CHAR`，那个键直接消失。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum InputBlock {
    /// 能正常输入。
    #[default]
    None,
    /// 线程级 `GUID_COMPARTMENT_KEYBOARD_DISABLED`：系统把输入法整个禁用了。
    /// 罕见且严重，是唯一配得上「图标变淡」这种呈现的一档。
    KeyboardDisabled,
    /// 密码框：context 级 KEYBOARD_DISABLED 或 `IS_PASSWORD` InputScope。
    Password,
    /// 焦点不在可编辑控件里（CAD 绘图区、浏览器非输入区、QQ 的 READONLY DocMgr）。
    NoEditContext,
}

impl InputBlock {
    /// 是否该把模式格 / 图标主字覆盖成「英」。
    ///
    /// ★★★ **`NoEditContext` 刻意不在内**。它与另外两档看着都是「敲键盘不出中文」，
    /// 但成因的**发生频率差三个数量级**：密码框与线程级禁用是罕见事件，而「焦点不在
    /// 可编辑控件里」是日常——实测 VS Code 里 8 分钟发了 35 次 `NoEditCtx`（每换一次
    /// docMgr 就一次：点标签页、点侧边栏、点终端面板）。每次都翻一下图标，用户看到的
    /// 是图标自己在抖，而不是任何有用的信息。
    ///
    /// 而且此刻图标显示什么都不影响功能：焦点不在输入控件上时，敲键盘本来就没有落点。
    /// 与 2026-08-04（ce167f37）否掉「用变淡表示无可编辑上下文」是同一条理由，那次也是
    /// 「日常状态不配强呈现」。桌面显示「英」不受本决定影响——那走的是 initial_mode 规则
    /// （`chinese_mode` 真的变了），不是本闸门。
    ///
    /// ⚠ 另一半原因：`NoEditCtx` 这个 `FocusLostReason` 回答的是「**这个 docMgr** 有没有
    /// 可编辑 context」，而不是「输入法现在可不可用」。用事件推断状态本身就不严谨——
    /// C++ 侧表达后者的是 `_hasTextInputContext`，且它在 `OnSetThreadFocus` 里会**重新
    /// 权威查询**。真要恢复这一档，得让 DLL 把那个状态如实上报，而不是从失焦事件反推。
    pub(crate) fn shows_english(self) -> bool {
        matches!(self, Self::KeyboardDisabled | Self::Password)
    }
    /// 是否该变淡。**只留给线程级禁用**，理由同上。
    ///
    /// 唯一的生产调用点在 `langbar_icon.rs`，而那整个模块是
    /// `cfg(all(feature = "desktop-ui", windows))`——语言栏图标是 Windows 独有形态，
    /// macOS 的 IMKit 与 headless/Android 形态都没有对应物。故在别的平台上 lib 单独
    /// 编译时它确实无人调用：本文件的断言在 `#[cfg(test)]` 里，`--all-targets` 的 lib
    /// 那一趟看不见它们。
    ///
    /// 判据取「调用点的 cfg」而非笼统的 `not(windows)`：关掉 `desktop-ui` 的 Windows
    /// 构建同样没有这个调用点，写成后者会在那个组合下重新变成硬错误。
    /// 与 `wind-ui/sys.rs` 的 `clamp_content_in_bounds` 同一既定写法。
    #[cfg_attr(not(all(feature = "desktop-ui", windows)), allow(dead_code))]
    pub(crate) fn dims_icon(self) -> bool {
        matches!(self, Self::KeyboardDisabled)
    }
}

/// 进入「不可输入」呈现前要求状态稳定的时长。恢复方向不受此限（立即生效）。
pub(crate) const INPUT_BLOCK_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// [`Coordinator::input_block_gate`] 的内部状态。
#[derive(Default)]
pub(crate) struct InputBlockGate {
    /// 当前**已呈现**的档位。不变量：图标与工具栏显示的恒等于它。
    shown: InputBlock,
    /// 「已经想切到某个非 None 档，但还没稳够 `INPUT_BLOCK_DELAY`」的起始时刻。
    pending_since: Option<std::time::Instant>,
    /// 复查线程在途（单飞）。churn 期间每次焦点事件都 spawn 一个线程是没有意义的。
    probing: bool,
}

/// 焦点切换时是否需要重算初始状态（即是否调用 `apply_initial_mode`）。
///
/// 抽成模块级纯函数是为了能直接单测这个判据本身。内联在 `handle_focus_gained` 里时，
/// 唯一的覆盖方式是构造完整 `FocusData` 并走那条带 UI/IPC 副作用的路径，于是「门控条件
/// 写错」这类缺陷极易漏网——本仓已有多次「门控退化后测试仍全绿」的先例。
///
/// - `crossed`：焦点是否**跨进程**切入。同应用内的焦点跳转为 false，否则用户手切的
///   模式会在换输入框时被拉回初始值（「初始值」与「锁定」的分界线）。
/// - `per_app`：`state_scope="app"` 的既有按应用记忆语义。
/// - `old_has_rule` / `new_has_rule`：切换前后的进程是否配了 compat.toml 初始状态规则。
///   两者**取或**，使规则同时覆盖「进入规则应用」和「离开规则应用」两个方向：只看 new
///   会让从 Everything 切出去后英文状态残留给下一个应用；而放宽成「规则表非空」又会让
///   任意两个无规则应用之间的切换也重算，把用户手切的状态冲掉。
/// - `out_of_scope`：切入的窗口是否落在该进程的**初始模式作用域**之外
///   （见 `InitialModeScopeRule`；未配作用域的进程恒 false）。
///   **一票否决，压过上面全部条件**。
///
///   为什么必须一票否决而不是并进那个「或」：`explorer.exe` 一个进程名同时承载桌面
///   （用户就是冲它配的 `initial_mode = "english"`）与任务栏 / Alt+Tab / 溢出区，
///   `new_has_rule` 对两者恒同真，光靠它分不开。判据只能来自窗口类。
///   实测样本（2026-08-18）：非桌面焦点 169 次、桌面 12 次——**14:1** 的误切代价。
///
///   ★★★ 该参数曾是 `new_is_transient`（黑名单：这个类是不是过渡窗口），当天被实测
///   推翻。窗口类取不到时黑名单恒 false ⇒ 放行 ⇒ 照样套规则，而「拿不到窗口类」恰恰
///   是 explorer 新起 TSF 连接时的常态（17:24:08 现场：焦点刚建连，caret 都还是
///   last_known，图标当场闪英）。反转成作用域白名单后，「不知道在哪」自动落在作用域外
///   = 保持现状。**信息缺失时的正确答案是"别动"，不是"按默认动"。**
pub(crate) fn should_reapply_initial(
    crossed: bool,
    per_app: bool,
    old_has_rule: bool,
    new_has_rule: bool,
    out_of_scope: bool,
) -> bool {
    !out_of_scope && crossed && (per_app || old_has_rule || new_has_rule)
}

/// 中央协调器
/// 上一次推给 UI 的工具栏指令（供 `notify_toolbar` 去重）。
///
/// 区分「隐藏」与「显示某状态」两种，而不是只存 `Option<ToolbarState>`：后者无法表达
/// 「上次推的是 Hide」，于是 Hide→Show→Hide 中的第二个 Hide 会被误判成「和上次一样」
/// （上次其实是 Show）而跳过——工具栏就再也藏不掉了。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ToolbarPush {
    Hidden,
    /// `Box` 是为了压小枚举体积：`ToolbarState` 带 String，比 `Hidden` 大得多，
    /// clippy 的 large_enum_variant 会因此告警。
    Shown(Box<ToolbarState>),
}

pub struct Coordinator {
    pub(crate) state: Mutex<State>,
    pub(crate) push_server: Arc<PushServer>,
    /// 配置 + 轻量派生缓存快照（RwLock<Arc<>> 原子替换支持热重载）。
    /// 访问统一经 `self.rt()`。
    rt: std::sync::RwLock<std::sync::Arc<ConfigBundle>>,
    /// UI 命令发送端。
    ///
    /// 不是裸的 `mpsc::Sender`：UI 线程是事件驱动的，投递之后还得把它叫醒，而这里有 50 余处
    /// 发送点——[`crate::UiSender`] 把这两步绑成一次 `send`，漏不掉。详见其模块文档。
    pub(crate) ui_tx: crate::UiSender,
    pub(crate) engine_mgr: EngineManager,
    /// redb 持久化存储（用户词/临时词/词频/影子规则）；None=无持久化（headless 测试）。
    pub(crate) store: Option<Arc<Store>>,
    /// 标点转换器（引号左右状态）
    pub(crate) punct: Mutex<PunctuationConverter>,
    /// 智能符号模式待命态（同键连按删中文标点改英文）
    pub(crate) smart_symbol: Mutex<SmartSymbolArm>,
    /// 码表自动造词的连续单字缓冲。**独立于 `State`**：终止信号多来自 IPC 回调
    /// （焦点丢失 / IME 停用 / 光标移动），那些路径不持 `state` 锁，塞进 `State` 会
    /// 逼出跨锁调用。见 `auto_phrase` 模块头注释。
    pub(crate) auto_phrase: Mutex<crate::auto_phrase::AutoPhraseBuf>,
    /// 最近一次**本输入法自己**向宿主吐字的时刻（由 `commit_action` 统一打点）。
    ///
    /// 用途只有一个：宿主插入我们提交的文字后会回送 `SelectionChanged`，它和「用户真的
    /// 移动了光标」在协议层**长得一模一样**，只能靠时间区分。若不区分，每上屏一个字就会
    /// 被自己的回声判成「用户移动光标」→ flush → 缓冲永远只有 1 个字 → 造词恒不触发。
    ///
    /// **打点必须收口在 `commit_action` 一处**：漏掉任一吐字路径，该路径的回声就会切碎序列。
    pub(crate) last_self_commit: Mutex<Option<std::time::Instant>>,
    /// 自动造词写入计数，供临时词库淘汰按次节流（见 `maybe_evict_temp`）。
    pub(crate) auto_phrase_writes: std::sync::atomic::AtomicUsize,
    /// CapsLock 全局低级键盘钩子。
    ///
    /// ★ **只有用户在 `keys.session_actions` 里真的配了 `capslock` 时才是 `Some`**。没配的
    /// 用户进程里根本不存在全局键盘钩子——这是本功能唯一的风险控制手段（用户明确要求）。
    ///
    /// 为什么非钩子不可：CapsLock 的锁定态由系统在 TSF **之前**维护，`pfEaten` 压不住；
    /// 而「让它翻转再回敲复原」在快速连按下有竞态（大写会卡住），还会被厂商 OSD 工具
    /// 观测到并弹窗。详见 `wind_keys::capslock_hook` 模块文档。
    pub(crate) capslock_hook: Mutex<Option<wind_keys::capslock_hook::CapsLockHook>>,
    /// 钩子线程 → 动作消费线程的投递口。
    ///
    /// 在 `new` 里就建好并起好消费线程（那里才有 `Arc<Self>`），钩子装卸只是复用它。
    /// 消费线程空闲时阻塞在 channel 上，未装钩子时零开销。
    capslock_press_tx: std::sync::mpsc::Sender<()>,
    /// 短语层（系统+用户，来自 store，仅 enabled）。变更后可 rebuild_phrases 重建。
    pub(crate) phrases: std::sync::RwLock<wind_phrase::PhraseLayer>,
    /// 最近一次解析的系统短语条目（启动时填充；"恢复默认"重读文件成功后刷新）。
    /// 作为重读失败（文件缺失/TOML 语法错误）时的回退，避免把库里系统短语清空。
    pub(crate) system_phrase_entries: std::sync::RwLock<Vec<wind_phrase::SystemPhraseEntry>>,
    /// system.phrases.toml 路径（None=无 data_dir，如 headless 测试）。
    /// "恢复默认"据此重读文件，使手工编辑无需重启服务即可生效。
    pub(crate) system_phrase_path: Option<std::path::PathBuf>,
    /// 简繁转换器（OpenCC；None=数据缺失不可用）。变体由配置 features.s2t.variant 决定，
    /// 启动时加载；菜单仅提供开/关。置于 Mutex 兼容 reload 时整体替换。
    pub(crate) s2t: Mutex<Option<wind_transform::s2t::Converter>>,
    /// 通用规范汉字表（检索范围"常用字"判定；空集时退化为不过滤）
    pub(crate) common_chars: wind_candidate::CommonChars,
    // Shadow 规则已迁至 redb（self.store 的 SHADOW 表）。
    /// 工具栏位置，按显示器 key（"workRight,workBottom"）独立记录。
    pub(crate) toolbar_positions: Mutex<std::collections::HashMap<String, (i32, i32)>>,
    /// 工具栏当前所在显示器的 key（None=尚未定位）。`sync_toolbar_monitor` 的去重依据：
    /// notify_toolbar 在每次模式切换/焦点事件上都跑，无此缓存就会把用户拖动过的位置
    /// 反复重置回记忆值。拖动落盘时（`save_toolbar_pos`）同步更新，否则拖到别的屏之后
    /// 这里仍记着旧 key，下一次校正会被误判为「屏没变」而跳过。
    pub(crate) current_toolbar_monitor: Mutex<Option<String>>,
    /// 候选反查（编码/拆字/拼音）供悬停提示与加词出码；拆字段随主码表方案
    /// 热重载（见 `sync_chaizi_assets`），拼音段启动加载后不变。
    pub(crate) reverse: std::sync::RwLock<wind_reverse::ReverseLookup>,
    /// 辅助码表（懒加载，首次辅助码输入时经 `ensure_aux_code_table` 读取并 merge；路径由
    /// 调用方经覆盖解析函数定位，本处不做 `data_dir.join`）。`None` = 尚未加载。
    pub(crate) aux_code_table: std::sync::RwLock<Option<wind_aux_code::AuxCodeTable>>,
    /// 快捷输入格式表（`system.quick.toml`，支持用户目录整份覆盖）。
    ///
    /// 启动加载后不变，故无锁：与 `system.phrases.toml` 同语义——**改完必须重启服务**，
    /// 全仓的覆盖点都没有文件监视器。加载失败已在 `FormatTable::load` 内回落内置默认表，
    /// 此处恒是一张可用的表。
    pub(crate) quick_formats: wind_quick_input::FormatTable,
    /// 快捷输入格式表的**用户调整**（右键调序 / 停用）运行时镜像，键为格式类别。
    ///
    /// 真相在 `userdata.redb` 的 `quick_format` 表，这里是读缓存：候选生成在热路径上
    /// （每次按键都跑），每次去查库不划算。
    ///
    /// ⚠️ 右键操作必须**写库 + 更新本镜像**两件都做。只写库不回灌，症状就是
    /// 「调了没反应、重启才生效」——本仓这个坑踩过不止一次。
    ///
    /// 与 `quick_formats`（文件，启动后不变）分开存放是刻意的：GUI 调整绝不回写
    /// `system.quick.toml`，那会抢走高级用户手写文件的所有权，也会让普通用户
    /// 点两下右键就永久脱离出厂更新。
    pub(crate) quick_adjust:
        std::sync::RwLock<std::collections::HashMap<String, wind_quick_input::FormatAdjust>>,
    /// 拆字资产当前生效状态（库解析路径 / 已下发字根字体），reload 变更检测用。
    pub(crate) chaizi_assets: Mutex<ChaiziAssets>,
    /// 注释词库当前生效的解析路径列表（顺序即优先级），reload 变更检测用。
    /// 见 `sync_comment_dicts`。
    pub(crate) comment_dict_paths: Mutex<Vec<std::path::PathBuf>>,
    /// 标点配对跟踪栈（用于智能跳过）；中/英配对表在 rt bundle 内。
    pub(crate) pair_tracker: Mutex<wind_transform::pair_tracker::PairTracker>,
    /// 最近一次有效光标坐标 (x,y,height)；用于无效坐标时回退，避免候选窗跑到左上角
    last_valid_caret: Mutex<(i32, i32, i32)>,
    /// 延迟首次显示：新组合首帧不立即显示候选窗，待 handle_caret_update 收到 reflow 后的权威坐标、
    /// 或兜底 timer 超时再首显，避免在 reflow 前的陈旧坐标处先显示再跳（对齐 Go pendingFirstShow）。
    /// 宿主不依赖光标坐标（自绘候选条，如 Android）：跳过首显闸门的等待。
    ///
    /// 闸门存在的理由是**桌面候选窗要等宿主 reflow 后的权威坐标**才好定位。自绘宿主
    /// 把候选画在自己的固定位置上，坐标毫无意义——不关掉的话，宿主只能编造一组非零
    /// 合成坐标去骗过闸门（Android 一度就是这么做的，`height` 写 0 还会被判为「宿主
    /// 尚未 reflow」整帧丢弃，候选一次都不下发）。
    caret_independent: std::sync::atomic::AtomicBool,
    /// 启动时预热全部已装方案（桌面默认开；移动端关，见构造里的说明）
    pub(crate) eager_prewarm: std::sync::atomic::AtomicBool,
    /// 0=Idle 1=Preparing 2=Ready 3=Failed（见 [`Coordinator::readiness`]）
    readiness_state: std::sync::atomic::AtomicU8,
    pending_first_show: Mutex<bool>,
    /// 上述兜底 timer 的代际令牌：每次 arm 自增，超时回调比对以作废被新按键取代的旧 timer。
    pending_first_show_token: Mutex<u64>,
    /// 联想窗自动隐藏 timer 的代际令牌（同上，见 `handle_assoc::arm_assoc_hide`）。
    /// **进入与退出联想态都要自增**——退出时不加，旧计时会在下一轮联想里提前把窗收掉。
    pub(crate) assoc_hide_token: Mutex<u64>,
    /// 本次组合候选窗是否已首次显示过（true=后续刷新可立即下发；false=首帧需延迟）。
    candidate_shown: Mutex<bool>,
    /// 显示授权：handle_caret_update / 兜底 timer 在调 notify_ui_update 前置位以放行首帧显示；
    /// 按键路径不置位，首帧改为 arm 延迟。notify_ui_update 内 swap 消费。
    show_authorized: std::sync::atomic::AtomicBool,
    /// 候选窗当前是否正在**反转排列**候选项（`ui.candidate.flip_when_above` 真正生效）。
    ///
    /// 由 UI 侧 `UiEvent::CandidateFlipped` 单向写入：判据要窗口尺寸 + 屏幕工作区才算得出
    /// （还叠加模式级强制横/竖排），协调器读配置推不出来，故只镜像不推导。
    /// 消费点唯一：[`Coordinator::apply_session_action`] 用它把 `highlight_up`/`highlight_down`
    /// 的走向翻过来，见那里的说明。
    candidate_flipped: std::sync::atomic::AtomicBool,
    /// 鼠标悬停目标（原始 tag）：-1 无，0..N 候选页内下标，或翻页器 tag。
    /// 与 `State::selected_index` 相互独立：悬停只是视觉提示，不改变空格上屏的目标。
    ///
    /// # ★★ 为什么不放在 `State` 里
    ///
    /// 它的生命周期是**候选窗会话**（窗口一隐藏就该归零），不是输入状态。放在 `State` 里时，
    /// 清空只能由每个候选装填点手工执行——主路径 `update_candidates` 做了，
    /// 特殊模式 / 临拼 / 混输 / 快捷输入的 8 个装填点全部漏了，于是悬停高亮与 tooltip 跨组合、
    /// 跨模式存活（用户 2026-08-12 反馈「再次弹出时悬停被记忆」）。普通输入下每敲一键都重走
    /// 主路径，残留被持续覆盖掉，**故该缺陷在主路径上物理不可观测**。
    ///
    /// 移出为原子量后，[`Coordinator::clear_hover`] **不需要 state 锁**，才能安放进
    /// [`Coordinator::notify_ui_hide`]——那里有 40+ 个调用点，无法逐一确认是否已持锁，
    /// 加锁即埋死锁。「窗口隐藏即清空悬停」这句话至此才在真相源上成立，而不只在 UI 侧的
    /// 防抖状态（`CandidateMouse::reset_hover`）上成立。
    pub(crate) hover_index: std::sync::atomic::AtomicI32,
    /// 本轮组合的首显是否用了**非权威**坐标（fast 的试探采样 / instant 沿用的旧坐标）。
    /// 置位后，该轮第一次权威坐标到达时改用放宽的容差判断要不要校正——校正动作本身
    /// 才是抖动的观感来源，小偏差不动比「跳一下修正」更稳。组合结束时复位。
    pub(crate) first_show_was_provisional: std::sync::atomic::AtomicBool,
    /// 坐标缓存是否已被**当前插入点**验证过（= `state.caret_*` 还算不算数）。
    ///
    /// `fast` 档短兜底的隐含前提是「手里的旧坐标 ≈ 当前插入点」——同一行连打时它只差一个
    /// 字宽，所以拿它首显毫无问题。本标志就是那个前提的显式化：
    ///
    /// - **置位**：[`Coordinator::handle_caret_update`] 采纳一帧权威坐标（与
    ///   `last_authoritative_caret` 同一处，同一条「够格当基准」的判据）。
    /// - **清位**：焦点到达（换 DocMgr，坐标属于上一个文档/单元格/应用）、
    ///   用户移动光标（[`Coordinator::handle_selection_changed`] 的非回声分支，
    ///   同一 DocMgr 内点到别处）。
    ///
    /// 清位后 `fast` 的 25ms 短兜底会退让为 [`FIRST_SHOW_LONG_FALLBACK_MS`] 长兜底（判据在
    /// [`Coordinator::arm_pending_first_show`]）：此时「快」没有意义，只会把候选窗快速显示
    /// 到一个错误位置、再当着用户的面跳回来。
    ///
    /// ⚠ **不复用 `last_authoritative_caret.2`**：那个字段回答的是「有没有可比的基准值」
    /// （probe 判据用），本字段回答「手里的值可不可信」。当前取值恰好一致，但两者对边缘
    /// 输入的期望会分化，合用一个必有一方错。
    caret_cache_verified: std::sync::atomic::AtomicBool,
    /// 本轮组合的首显是否已进入「长兜底等待」（首帧信任门命中）。
    ///
    /// 唯一用途是让后续按键**不重置**那段等待的计时——见
    /// [`Coordinator::arm_pending_first_show`] 里对该死结的说明。`reset_first_show` 复位。
    first_show_extended: std::sync::atomic::AtomicBool,
    /// `ui.status.show_on_focus` 的焦点气泡正等一个 TSF 权威坐标。
    ///
    /// 焦点事件到达时坐标常常还只是 GUI 回退值（`OnSetFocus` 拿不到同步 edit session 锁），
    /// 直接拿它定位就是用户反馈的「还没输入时定位非常不准」。故置位挂起，由
    /// [`Coordinator::handle_caret_update`] 在权威坐标到来时消费并补显示。
    ///
    /// **刻意不配兜底 timer**：超时后能做的只有「拿不可信坐标显示」，正是本机制要挡的事。
    /// 等不到就不显示，失焦/下一次焦点事件清位。
    pending_focus_tip: std::sync::atomic::AtomicBool,
    /// 上一次弹过焦点气泡的宿主（`client_token`，DLL 实例级 = 每进程一个）。
    ///
    /// **气泡的语义是「切到了新的输入宿主」，不是「换了 docMgr」**。一个宿主内部可以有多个
    /// docMgr 并频繁互切：Excel 在单元格里起输入时切一次、输入完焦点落到公式编辑栏又切一次，
    /// 若按 docMgr 计就成了「输入一次闪两下」（同一单元格内连续输入反而不闪，因为中途不换
    /// docMgr）——这个「闪的时机与用户的操作节奏对不上」正是它扰人的原因。
    /// 故以 token 去重：同 token 只在首次进入时弹，离开该宿主（`FocusLostReason::Thread`）时清零。
    last_focus_tip_token: Mutex<u64>,
    /// 上一次按键时刻，仅用于算出下面那个「相邻按键间隔」。
    pub(crate) last_key_at: Mutex<Option<std::time::Instant>>,
    /// **相邻两次按键**的间隔（毫秒），fast 档据此判断是否处于连续快速输入。
    ///
    /// ⚠ 必须是「按键与按键之间」，不能用 `last_key_at.elapsed()`——后者是「距上次按键多久」，
    /// 而试探坐标恒在按键后 10ms 内到达，那个条件永远成立、判据会被完全绕过。本功能就这么
    /// 空跑过一轮：日志里 163 次全报「连续输入 7~13ms」，而实际脚本节奏是 60ms。
    pub(crate) last_key_interval_ms: Mutex<Option<u64>>,
    /// 上一轮组合最终采纳的**权威** caret 坐标 (x, y, valid)，供首显试探采样做判据。
    ///
    /// 为什么这个能当判据：首帧 reflow 未完成时，宿主的 GetTextExt 返回的正是上一轮那个
    /// 位置（实测 WPS 连续两次返回上一轮终值，第三次才更新）；而真正 reflow 之后，光标
    /// 必然因新插入的组合内容而移动。所以「与上一轮权威坐标不同」≈「宿主已经 reflow」。
    /// 误判方向是安全的：判成「未 reflow」只是退回等 debounce（慢而不错）。
    pub(crate) last_authoritative_caret: Mutex<(i32, i32, bool)>,
    /// 组合起点屏幕坐标 (x, y, valid)：嵌入预编辑模式（编码插入宿主、光标随输入右移）下候选窗锚此处
    /// （缓冲头部），不随输入移动。同一组合只锁定首个有效值（handle_caret_update），组合结束复位。
    composition_start: Mutex<(i32, i32, bool)>,
    /// 应用兼容规则表（compat.toml，系统层 + 用户层覆盖）。按焦点进程名查规则。
    ///
    /// 用 Mutex 而非不可变字段：右键菜单切换 per-app 开关后要写用户层并**立即重载**。
    /// 只更新 `active_compat` 缓存是不够的——切到别的应用再切回来时 pid 变化两次，
    /// `update_active_compat` 会拿这张表重新解析，用旧表就会把刚才的切换悄悄回滚。
    pub(crate) app_compat: Mutex<wind_config::app_compat::AppCompat>,
    /// 启动时的 (系统数据目录, 用户配置目录)，供 compat.toml 热重载复用同一口径。
    /// 不用 `Config::data_dir()` 等静态函数：便携版/测试会传入自定义路径，静态函数
    /// 拿到的是默认安装位置，重载后规则会与初次加载不一致。
    pub(crate) compat_dirs: (Option<std::path::PathBuf>, Option<std::path::PathBuf>),
    /// 当前焦点进程派生的 caret 兼容态，见 [`ActiveCompat`]。
    pub(crate) active_compat: Mutex<ActiveCompat>,
    /// pid → 进程名（小写）缓存，`update_active_compat` 填充，会话级只增不清。
    /// 供 FOCUS_GAINED 同步路径（`get_current_mode`）免 OpenProcess 查询进程名。
    pub(crate) pid_names: Mutex<HashMap<u32, String>>,
    /// 「上一次**真正参与**初始模式决策的宿主」：`(pid, 该宿主是否配了初始状态规则)`。
    ///
    /// ⚠ 必须与 `active_compat.pid` **分开**，尽管两者绝大多数时候相同。
    /// `active_compat` 记的是「当前焦点在哪个进程」——过渡窗口（任务栏）也要更新它，
    /// 因为任务栏搜索框同样需要 caret 兼容项。而本字段记的是「初始模式该按谁算」，
    /// 过渡窗口**不更新**它。
    ///
    /// 合用一个变量的后果实测过（2026-08-18）：点任务栏时虽然正确跳过了模式重算，
    /// 但那次焦点仍把 `active_compat.pid` 变成了 explorer，于是紧接着**真正回到桌面**时
    /// `crossed` 恒为假（同一个 explorer pid），桌面配的 `initial_mode = "english"`
    /// 再也不会生效——过渡窗口把「跨进程切入」这个一次性事件提前消费掉了。
    ///
    /// 同源教训见 `_hasFocus` / `_hasThreadFocus`（TSF 侧）与
    /// `ime_active` / `has_edit_context` 的拆分：一个变量同时回答两个问题，
    /// 迟早会遇到两个答案相反的场景。
    pub(crate) mode_scope: Mutex<(u32, bool)>,
    /// 按应用独立中英状态表（`input.default.state_scope = "app"` 时启用）：
    /// 进程名（小写）→ chinese_mode，会话级记忆（服务重启即清，见计划决策）。
    mode_states: Mutex<HashMap<String, bool>>,
    /// 用户最后一次主动切换后的 (中英, 全半角, 中英标点) 内存镜像；
    /// remember_last_state=true 时随切换同步落盘 state.toml（`record_last_state`）。
    runtime_last: Mutex<(bool, bool, bool)>,
    /// 最近一次 CapsLock 取消注入的时刻（`cancel_caps_on_switch` 冷却，防振荡回路放大）。
    last_caps_inject: Mutex<Option<std::time::Instant>>,
    /// 前台上下文快照 `(app, title, sel)`，供命令直通车 app()/title()/sel() 取值。
    /// darwin `.app` 经 CMD_FRONT_CONTEXT 于聚焦时上报；其它平台暂空。
    front_ctx: Mutex<(String, String, String)>,
    /// 主题目录（data/themes）
    pub(crate) themes_dir: Option<std::path::PathBuf>,
    /// 当前主题名
    pub(crate) theme_name: Mutex<String>,
    /// 主题颜色风格：0=跟随系统 1=亮色 2=暗色
    pub(crate) theme_style: Mutex<ThemeStyle>,
    /// 状态气泡上一次显示的文本，用于抑制"内容没变却重复弹窗"。
    /// 关掉某个内容段后（如全半角），切换该状态不再改变气泡文本，此时应当整个不弹窗。
    /// 在 `show_status` 做文本比对而非判断"这次变的是哪个字段"，是因为后者要给全部
    /// 十余个调用点传参，而文本比对一处生效、且将来新增状态项零成本。
    pub(crate) last_status_text: Mutex<String>,
    /// 上一次推给 UI 的工具栏指令，用于**去重**。`None` = 本次会话还没推过。
    ///
    /// 宿主焦点抖动会把同一份状态连推数次（真机：飞书 200ms 内 5 轮
    /// focus_lost/gained，每轮一组 HideToolbar + UpdateToolbar + HideCandidates），
    /// 全部挤在 UI 线程上，表现就是「切换时占用高、语言栏图标迟钝」——图标更新排在
    /// 这些重复消息后面。内容没变就不必再推。
    pub(crate) last_toolbar_push: Mutex<Option<ToolbarPush>>,
    /// `toggle_schema:<id>` 的**来源**：`(从哪个方案按进来, 写入时的方案变更代际)`。
    ///
    /// 刻意只存运行时、不落配置：它描述的是「用户此刻的往返意图」，不是偏好。持久化会让
    /// 重启后第一次按跳到一个用户早忘了的方案——那正是「回到来源」这个语义最容易失信的
    /// 时刻。无有效来源时按 `toggle_schema` 到已在的方案是 no-op（不切走）。
    ///
    /// # 为什么带代际，而不是在切方案时清空
    ///
    /// 切 active 方案在协调器侧有**五条路径**（循环键 / 直达热键 / 命令栏 / 菜单
    /// `select_schema` / 设置页 RPC），其中只有两条走 `finish_user_schema_switch`——
    /// 那个"统一收尾"从来就没统一到全部。散点补清空必漏，且漏掉的表现是「往返键把人送回
    /// 几步之前的方案」，低频且难复现。
    ///
    /// 改为记下写入时 `EngineManager::schema_generation()` 的值，读取时比对是否仍相等：
    /// 期间**任何**路径切过方案，代际就对不上，来源自动失效。零散点接线。
    ///
    /// 只比对方案 id 是不够的——「切走又切回来」与「从未变过」在 id 上完全同形。
    ///
    /// 第三项是**触发键 VK**（0 = 非方案级绑定触发，如全局热键）。有它，回程才真正
    /// 「不依赖目标方案的配置」：去程后该键在目标方案里临时获得回程语义，哪怕目标方案
    /// 的 `[key_actions]` 是空的。
    ///
    /// ★ 没有这一项时，「五笔按 RShift 去英文方案」要求英文方案**自己也配一遍** RShift
    /// 才回得来——设计文档 §5 原本断言 `toggle_schema` 对锁死「从结构上免疫」，那只覆盖了
    /// 「回到哪」，没覆盖「怎么按得动」。测试里复现过。
    pub(crate) schema_toggle_origin: Mutex<Option<(String, u64, u32)>>,
    /// 当前主题定义的序号槽位字符（views.index.labels）；push_theme 载入时刷新。
    /// 序号优先级：用户配置 index_labels > 本字段 > 默认数字。
    pub(crate) theme_index_labels: Mutex<Vec<String>>,
    /// 命令栏（cmdbar）服务束（ime/config/dict 等动作后端），构造后由 init_cmdbar 装配。
    pub(crate) cmdbar_services: std::sync::OnceLock<wind_cmdbar::Services>,
    /// 宿主服务（剪贴板等平台能力）。`OnceLock` 构造后注入惯例（同 `self_weak`）；
    /// 未注入时首次取用即固化默认实现（桌面 DesktopHostServices），故 Android FFI
    /// 必须在**首个可能触碰剪贴板的调用之前** `set_host_services`。
    pub(crate) host_services: std::sync::OnceLock<Arc<dyn crate::host_services::HostServices>>,
    /// 自身 Weak 引用：$CC 命令在独立线程异步执行（避免持 state 锁回调自锁方法致死锁）。
    pub(crate) self_weak: std::sync::OnceLock<std::sync::Weak<Coordinator>>,
    /// 上屏历史环形缓冲（index 0 = 最近）：供命令栏 `last(n)` 取最近上屏文本。
    pub(crate) recent_commits: Mutex<std::collections::VecDeque<String>>,
    /// 撤销上屏（`ime.undo_commit`）删除量：最近一次「同步落到光标前」的字符数（UTF-16 单元，
    /// 与 TSF ShiftStart / macOS NSRange 同量纲）。**刻意与 `recent_commits` 分离**——历史队列
    /// 记「上过什么」（供 last/加词，深度 16），本值记「光标前紧邻的还是不是它、有几个字」这一
    /// 时效态。默认 1 → undo 永远有动作；每次上屏经 `note_commit_action` 覆盖 → 只有「刚输入完
    /// 那次」精准删多个；撤销一次即复位 1、焦点变化亦复位 → 之后回落删 1（宁可少删多按几次，
    /// 也不按陈旧计数误删多个）。
    pub(crate) last_commit_len: std::sync::atomic::AtomicUsize,
    /// 编码显示方式运行时态（命令栏 ime.toggle("preedit") 循环切换；初值随配置）。
    /// 统一权威：决定候选窗是否显示 preedit（in_app→不显示）及是否内联首单元（embedded）。
    pub(crate) preedit_display: Mutex<PreeditDisplay>,
    /// 候选窗隐藏开关（命令栏 ime.toggle("candwin") 切换；隐藏时 notify_ui_update 不显示候选）。
    hide_candidate_window: Mutex<bool>,
    /// 候选布局方向运行时态（命令栏 ime.toggle("layout") 切换；true=竖排，初值随配置，持久化）。
    ///
    /// 这是布局方向的**基线真相源**——模式级覆盖（`layout.rs`）在它之上叠加，不改写它。
    pub(crate) candidate_vertical: Mutex<bool>,
    /// 上次真正下发给 UI 的候选方向（`layout.rs` 的去重缓存，避免每次按键重发致重排抖动）。
    /// 与 `candidate_vertical` 的区别：后者是基线，本字段是**叠加模式意图后实际生效**的值。
    pub(crate) candidate_layout_sent: Mutex<bool>,
    /// 输入统计采集器（内存聚合 + 后台 flush，与 store 共享 Arc）；None=无持久化/headless。
    pub(crate) stat_collector: Option<StatCollector>,
    /// 本次按键是否已被具体上屏路径记录统计（AtomicBool，避免与 state 锁冲突致死锁）。
    pub(crate) stat_recorded: std::sync::atomic::AtomicBool,
    /// 全屏状态缓存：由 notify_toolbar_async 在后台线程异步刷新，notify_toolbar 直接读取，
    /// 消除 bridge handler 线程上的 SHQueryUserNotificationState 阻塞。
    pub(crate) fullscreen_cached: std::sync::atomic::AtomicBool,
    /// 全屏探测的单飞闸：已有探测在途时跳过新的。焦点变化是成串来的，而探的是同一个
    /// 全局前台状态，此前每次都 spawn 一个线程。见 `notify_toolbar_async`。
    pub(crate) fullscreen_probing: std::sync::atomic::AtomicBool,
    /// host-render 管理器（Windows）：与 `BridgeServer` 共享同一 `Arc` 实例。
    /// 服务入口经 `set_host_render` 注入一次；Task 6/7 据此写候选/工具提示/状态帧并隐藏。
    /// 采用 `OnceLock`（与 `self_weak`/`cmdbar_services` 同一构造后注入惯例），
    /// 避免为其贯穿 `new`/`new_headless` 等构造器签名。
    #[cfg(windows)]
    #[allow(dead_code)] // Task 6/7 接线写帧/隐藏后即被读取
    host_render: std::sync::OnceLock<Arc<wind_bridge::host_render_windows::HostRenderManager>>,
    /// 最近一次输入诊断快照（compartment 禁用态 / InputScope 密码位），供 Task 6 HUD 展示。
    pub(crate) last_input_diag: Mutex<crate::input_diag::InputDiagState>,
    /// 最近一次窗口 / TSF 上下文诊断快照（`CMD_DIAG_SNAPSHOT`）。
    /// 与 `last_input_diag` 分开存：两者上报时机不同，合成一个就得回答「只到了一半算什么」。
    pub(crate) last_window_diag: Mutex<crate::input_diag::WindowDiagView>,
    /// 密码框强制英文抑制态：命中密码 InputScope 时置 true，输入闸据此强制英文透传
    /// （**不改 `chinese_mode` 持久值**）。
    ///
    /// 呈现：2026-08-04 起工具栏模式格显 "英" 且不高亮（`ToolbarState::password_suppress`），
    /// TSF 语言栏图标同样显 "英"（C++ 侧本地判 `IsPasswordSuppressActive`，不经 IPC）。
    /// 此前的「图标保持不变」是对齐 Go 旧版的决策，已按用户反馈推翻——图标显方案标签
    /// 而键已被全放行，用户无从知道自己打不出中文。
    /// ⚠ 呈现与输入闸是两条独立的路：改这里的展示**不会**改变是否抑制，反之亦然。
    pub(crate) password_suppress: std::sync::atomic::AtomicBool,
    /// 「不可输入」**呈现**的迟滞闸门（判定本身见 [`Coordinator::input_block`]）。
    ///
    /// 两个方向不对称，取值与理由继承自已删除的 C++ 版：
    /// · 恢复（→ 可输入）**立即**生效——误显「英」很刺眼；
    /// · 进入（→ 不可输入）延迟 [`INPUT_BLOCK_DELAY`]——晚一点显「英」用户察觉不到。
    /// 实测 QQ 密码框场景这两个量每约 180ms 翻转两次，不做迟滞就是图标闪烁源。
    pub(crate) input_block_gate: Mutex<InputBlockGate>,
    /// 上次广播出去的语言栏悬停提示文本，用于去重。
    ///
    /// tooltip 只有几种取值，而状态推送远比它频繁（全半角、标点、方案切换都会推状态却
    /// 不改 tooltip）。不去重的话每次状态变化都白发一条 IPC 给所有宿主。
    pub(crate) last_langbar_tooltip: Mutex<String>,
    /// 密码框抑制策略开关（默认 true）；关闭时 `apply_input_diag` 不再置位 `password_suppress`。
    pub(crate) password_suppress_enabled: std::sync::atomic::AtomicBool,
    /// 输入诊断 HUD 是否可见（Task 6/7 接线；本任务先占位默认 false）。
    pub(crate) input_diag_hud_visible: std::sync::atomic::AtomicBool,
    /// HUD 分区显示开关（右键菜单「显示分类」）。会话级，不持久化。
    pub(crate) input_diag_sections: Mutex<wind_ui_types::DiagSections>,
    /// HUD 冻结中（右键菜单「停止刷新」）：新快照不再推给 UI。
    ///
    /// 冻结落在**推送**这一层而不是 UI 渲染层：数据照常进 `last_*_diag`（解冻后立即有
    /// 最新值），只是不往屏幕上送。若改在 UI 侧丢弃，解冻后得等下一次焦点事件才恢复。
    pub(crate) input_diag_frozen: std::sync::atomic::AtomicBool,
    /// HUD 窗口置顶（右键菜单）。默认开——诊断浮窗被盖住就失去意义。
    pub(crate) input_diag_topmost: std::sync::atomic::AtomicBool,
}

/// 拆字资产当前生效状态：库的解析后绝对路径 + 已下发的字根字体（路径, DWrite 家族名）。
/// 变更检测用——库变了才重载反查表，字体变了才重发（渲染端每次 set 都重建字体集）。
#[derive(Default)]
pub(crate) struct ChaiziAssets {
    pub(crate) db: Option<std::path::PathBuf>,
    pub(crate) font: Option<(String, String)>,
}

/// 一次候选刷新后的输入结局（码表全码/空码策略，仅正向输入字母时消费）。
pub(crate) enum InputOutcome {
    /// 正常更新候选，继续组合。
    Normal,
    /// 全码自动上屏该文本。
    AutoCommit(String),
    /// 全码唯一命中含副作用 `$CC` 命令：清组合并异步执行（无同步上屏文本，
    /// 语义与空格选中命令一致，见 `commit_command`）。
    AutoCommand(Box<Candidate>),
    /// 满码空码：清空缓冲。
    Clear,
}

impl Coordinator {
    /// 注入宿主服务（剪贴板等平台能力）。重复注入静默忽略（`OnceLock` 语义）。
    ///
    /// Android FFI 必须在首个可能触碰剪贴板的调用之前注入——未注入时首次取用
    /// 即固化默认实现（见 [`Self::host_services`]），此后本方法不再生效。
    /// 桌面构造路径不调用：默认实现就是桌面剪贴板。
    pub fn set_host_services(&self, svc: Arc<dyn crate::host_services::HostServices>) {
        let _ = self.host_services.set(svc);
    }

    /// 宿主服务访问点；未注入时落默认实现（桌面剪贴板直通 / headless no-op）。
    pub(crate) fn host_services(&self) -> &Arc<dyn crate::host_services::HostServices> {
        self.host_services.get_or_init(|| {
            #[cfg(feature = "desktop-ui")]
            {
                Arc::new(crate::host_services::DesktopHostServices)
            }
            #[cfg(not(feature = "desktop-ui"))]
            {
                Arc::new(crate::host_services::NullHostServices)
            }
        })
    }

    /// 注入 host-render 管理器（Windows）。服务入口在构造 `BridgeServer` 后调用一次，
    /// 与其共享同一 `Arc` 实例。重复注入静默忽略（`OnceLock` 语义）。
    #[cfg(windows)]
    pub fn set_host_render(&self, mgr: Arc<wind_bridge::host_render_windows::HostRenderManager>) {
        let _ = self.host_render.set(mgr.clone());
        // 把同一 Arc 传给 UI 线程，使其在消息循环中激活 SHM 分流路径（Task 7）。
        let _ = self.ui_tx.send(wind_ui_types::UiCommand::SetHostRender(
            wind_ui_types::HostRenderArc(mgr),
        ));
    }

    /// 取已注入的 host-render 管理器（Windows）；未注入返回 None。供 Task 6/7 写帧/隐藏。
    #[cfg(windows)]
    pub(crate) fn host_render(
        &self,
    ) -> Option<&Arc<wind_bridge::host_render_windows::HostRenderManager>> {
        self.host_render.get()
    }

    /// 把 `app_compat` 现算的 HostRender 白名单同步给 manager。
    ///
    /// 白名单来自 compat.toml 的 `host_render = true` 规则（`AppCompatRule::host_render`），
    /// 不是 config.toml 字段——调用点是每次 `app_compat` 被重新加载之后（menu 写规则、
    /// 未来若加设置页开关同理），而非常规配置热重载（compat.toml 与 config.toml 是两个
    /// 独立文件，后者变了不代表前者变了）。
    #[cfg(windows)]
    pub(crate) fn sync_host_render_whitelist(&self) {
        if let Some(mgr) = self.host_render() {
            let processes = self
                .app_compat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .host_render_processes();
            mgr.set_whitelist(processes);
        }
    }

    /// 当前是否处于 host-render 受限宿主模式（SearchHost.exe / 开始菜单搜索框等）。
    /// `active_target()` 每次现查（无缓存），避免跨帧持有失效目标；它仅在 active 连接
    /// **已完成 setup** 时返回 Some，而 setup 会拒绝白名单外进程——故此判定天然经过
    /// 白名单过滤，语义为「确实在 host 渲染」（比按事件源 pid 查白名单更严格）。
    /// 非 Windows 编译始终返回 false，零开销。
    pub(crate) fn host_render_active(&self) -> bool {
        #[cfg(windows)]
        return self
            .host_render()
            .map(|m| m.active_target().is_some())
            .unwrap_or(false);
        #[cfg(not(windows))]
        return false;
    }

    /// 当前是否有活跃组合（编码缓冲非空）。
    ///
    /// 供**无 TSF 前置过滤的宿主**（Android IME）做吃键判定：Windows 侧
    /// `OnTestKeyDown` 在 IPC 往返前就决定吃不吃键，空缓冲的空格/回车/退格/数字
    /// 根本不会送进协调器；Android 的 `onKeyDown` 没有这一层，宿主必须自己按
    /// 「有组合才吃功能键」过滤，否则协调器对这些键返回的 `Consumed`（意为
    /// 「已在输入法内处理」）会被当成消费，宿主既不上屏也不执行默认行为。
    pub fn is_composing(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .input_buffer
            .is_empty()
    }

    /// 就绪状态。见 [`wind_host::Readiness`]。
    pub fn readiness(&self) -> wind_host::Readiness {
        use std::sync::atomic::Ordering;
        match self.readiness_state.load(Ordering::Relaxed) {
            1 => wind_host::Readiness::Preparing,
            2 => wind_host::Readiness::Ready,
            3 => wind_host::Readiness::Failed,
            _ => wind_host::Readiness::Idle,
        }
    }

    /// 触发后台准备（幂等、非阻塞）。返回 `false` = 已在进行或已就绪。
    ///
    /// 准备的内容是**首次真实查询才会触发的惰性构建**（反查/合并索引，实测真机冷启动
    /// 同步阻塞 2.8 秒）。故这里走**与 `handle_key_event` 完全相同的按键路径**——
    /// 另写一条「预加载」必然与真实路径漂移：Android 侧手写预热的第一版就漏掉了释放
    /// 首显闸门那步，「预热」只花 3ms 返回，惰性构建原样留给了用户的第一次按键。
    ///
    /// 喂 'a' 再退格，一进一删回到空缓冲，不留状态。
    pub fn prepare(self: &Arc<Self>) -> bool {
        use std::sync::atomic::Ordering;
        // Idle → Preparing 的 CAS 保证只跑一次
        if self
            .readiness_state
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("wind-prepare".into())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let key = |vk: u32| wind_bridge::handler::KeyEventData {
                    key_code: vk,
                    scan_code: 0,
                    modifiers: 0,
                    event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
                    toggles: 0,
                    event_seq: 0,
                    prev_char: 0,
                };
                use wind_bridge::handler::MessageHandler;
                this.handle_key_event(&key(0x41));
                // 收尾用**退格**而非 ESC：ESC 未必清空编码缓冲，实测预热完缓冲里还留着
                // 那个 'a'，用户接着打 "aa" 会得到 "aaa"。退格逐码删，一进一删必回空。
                this.handle_key_event(&key(wind_keys::keymap::VK_BACK));
                this.readiness_state.store(2, Ordering::Relaxed);
                info!("prepare 完成，耗时 {:?}", t0.elapsed());
            })
            .is_ok();
        if !spawned {
            self.readiness_state.store(3, Ordering::Relaxed);
        }
        spawned
    }

    /// 是否在启动时预热全部已装方案。移动端应设 `false`（理由见构造里的说明）。
    ///
    /// 构造返回后**立即**调用即可生效：预热线程有 1.5s 的启动延迟专为此留（见构造处）。
    pub fn set_eager_prewarm(&self, value: bool) {
        self.eager_prewarm
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }

    /// 声明本宿主**不提供光标坐标**（自绘候选条）。
    ///
    /// 置位后首显闸门直接放行，宿主不必再喂合成 caret。这不是能力协商——没有分支矩阵，
    /// 只是关掉一段桌面专属的等待逻辑。
    pub fn set_caret_independent(&self, value: bool) {
        self.caret_independent
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn build(
        config: Config,
        data_dir: Option<&Path>,
        push_server: Arc<PushServer>,
        ui_tx: crate::UiSender,
        user_dir: Option<std::path::PathBuf>,
        store: Option<Arc<Store>>,
        override_dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        // 注入 redb Store：码表引擎注册用户词/临时词层，用户词进候选合并。
        // override_dir 为 None 时由 EngineManager 取默认（用户配置目录下的 schema_overrides）。
        let engine_mgr = match override_dir {
            Some(od) => {
                EngineManager::with_store_override(&config, data_dir, store.clone(), Some(od))
            }
            None => EngineManager::with_store(&config, data_dir, store.clone()),
        };
        // 应用兼容规则：系统层(data/compat.toml) + 用户层覆盖。供焦点进程按名查规则
        // （如微信 caret_use_top）。
        let app_compat = wind_config::app_compat::AppCompat::load(data_dir, user_dir.as_deref());
        // 配置的轻量派生缓存集中到 ConfigBundle（支持运行时热替换）。
        let schema_keys = schema_key_union(&engine_mgr);
        let bundle = ConfigBundle::build(config.clone(), &schema_keys);
        info!(
            "Compiled hotkeys: {} key_down, {} key_up",
            bundle.compiled_hotkeys.key_down.len(),
            bundle.compiled_hotkeys.key_up.len()
        );

        // 短语层（方案 B）：TOML 变更时同步进 store，再从 store（仅 enabled）建层。
        // 启动解析的条目缓存进结构体，作为"恢复默认"重读文件失败时的回退。
        let mut system_phrase_entries: Vec<wind_phrase::SystemPhraseEntry> = Vec::new();
        // 用户目录同名文件整体替代安装目录那份（覆盖替换，非合并）。
        // ⚠️ 解析在此**一次定死**：后续 `current_system_phrase_entries` 的重读走同一路径，
        // 故运行时新放的覆盖文件要下次启动才生效（与全仓其它覆盖点一致，无文件监视）。
        let system_phrase_path = Config::resolve_data_file(data_dir, "system.phrases.toml");
        if system_phrase_path.is_none() && data_dir.is_some() {
            warn!("system.phrases.toml 缺失（用户/安装目录均未找到），系统短语将为空");
        }
        let phrases = {
            if let Some(store) = store.as_ref() {
                if let Some(p) = system_phrase_path.as_ref() {
                    let entries = wind_phrase::PhraseLayer::parse_system_entries(p);
                    // 内容哈希：条目稳定序列化后哈希
                    let hash = phrase_entries_hash(&entries);
                    // 自愈：哈希不一致（TOML 改动）或表内系统短语为空（被删/未初始化）时才同步。
                    // 仅凭哈希会漏掉"系统短语从表中丢失但 TOML 未变"的场景。
                    let sys_empty = store
                        .list_system_phrases()
                        .map(|v| v.is_empty())
                        .unwrap_or(false);
                    if store.phrase_sys_hash().ok().flatten().as_deref() != Some(hash.as_str())
                        || sys_empty
                    {
                        let sys: Vec<wind_store::phrases::SystemPhrase> = entries
                            .iter()
                            .map(|e| wind_store::phrases::SystemPhrase {
                                code: e.code.clone(),
                                text: e.text.clone(),
                                weight: e.weight,
                                position: e.position,
                            })
                            .collect();
                        if let Ok(st) = store.sync_system_phrases(&sys) {
                            info!(
                                "Synced system phrases: +{} ~{} -{}",
                                st.added, st.updated, st.removed
                            );
                            let _ = store.set_phrase_sys_hash(&hash);
                        }
                    }
                    system_phrase_entries = entries;
                }
                let recs = store
                    .enabled_phrases_for_input()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| (p.code, p.text, p.weight, p.position, p.is_system));
                std::sync::RwLock::new(wind_phrase::PhraseLayer::from_records(recs))
            } else {
                std::sync::RwLock::new(wind_phrase::PhraseLayer::default())
            }
        };

        // 简繁转换器：从 data/opencc 加载（变体来自配置，默认 s2t）
        let opencc_dir = data_dir.map(|d| d.join("opencc"));
        let s2t_variant = if config.input.s2t.variant.is_empty() {
            "s2t".to_string()
        } else {
            config.input.s2t.variant.clone()
        };
        let s2t = opencc_dir.as_ref().and_then(|dir| {
            let conv = wind_transform::s2t::Converter::load_variant(dir, &s2t_variant);
            if conv.is_some() {
                info!("Loaded S2T converter (variant={})", s2t_variant);
            }
            conv
        });

        // 词频已迁 redb（self.store 的 FREQ 表，选词时 record_freq）。

        // 标点配对表（中/英）已在 ConfigBundle 内构建。

        // 通用规范汉字表（检索范围"常用字"判定）。用户目录同名文件整体替代（见
        // docs/architecture/user-override.md）——自定义"常用字"范围是这张表的主要用途。
        let common_chars = wind_candidate::CommonChars::load(
            &Config::resolve_schema_resource(data_dir, "common_chars.txt").unwrap_or_default(),
        );
        if common_chars.is_empty() {
            warn!("common_chars.txt 缺失，检索范围过滤将退化为不过滤");
        } else {
            info!("Loaded common chars table");
        }

        // 候选反查表（拆字/拼音）：拆字库路径取自主码表方案 [engine.chaizi].db_path（相对 schemas/，
        // 用户方案目录优先——第三方方案的拆字库只在用户目录下）。
        let chaizi_db = engine_mgr
            .chaizi_spec()
            .filter(|c| !c.db_path.is_empty())
            .and_then(|c| {
                let p = Config::resolve_schema_resource(data_dir, &c.db_path);
                if p.is_none() {
                    warn!(
                        "拆字库不存在（用户/系统 schemas 目录均未找到）: {}",
                        c.db_path
                    );
                }
                p
            });
        // 快捷输入格式表：日期/数字/金额/计算候选的文本与组内顺序。同样支持用户整份覆盖，
        // 是给高频输入者的高级特性，普通用户不会碰到（缺文件时回落内置默认表，行为与出厂一致）。
        let quick_formats = wind_quick_input::FormatTable::load(
            Config::resolve_data_file(data_dir, "system.quick.toml").as_deref(),
        );
        // 表达式条目在这里预检一次：写错的表达式在运行期只表现为「那条候选不出现」，
        // 没有预检就没有任何线索（热路径不能每次按键都告警）。
        crate::quick_eval::precheck(&quick_formats);

        // 拼音读音表同样支持用户覆盖（整体替代）：改多音字取音、补生僻字读音都靠换这张表。
        let pinyin_map = Config::resolve_data_file(data_dir, "pinyin_map.txt");
        if pinyin_map.is_none() && data_dir.is_some() {
            warn!("pinyin_map.txt 缺失（用户/安装目录均未找到），逐字拼音反查将不可用");
        }
        let reverse =
            wind_reverse::ReverseLookup::load(pinyin_map.as_deref(), chaizi_db.as_deref());
        if !reverse.is_empty() {
            info!("Loaded reverse-lookup (chaizi/pinyin)");
        }

        // Shadow 规则已迁至 redb（self.store 的 SHADOW 表，事务持久），不再用 shadow.json。
        // 从 state.toml 加载工具栏位置（按显示器 key 独立存储）。
        let runtime_state = Config::state_dir()
            .map(|d| wind_config::RuntimeState::load(&d))
            .unwrap_or_default();
        let toolbar_positions_init = runtime_state.toolbar_positions.clone();
        let themes_dir = data_dir.map(|d| d.join("themes"));
        // 初始主题名：config.ui.theme.name 为单一源，未设置则回退 FALLBACK_THEME。
        let cfg_theme = config.ui.theme.name.trim();
        let initial_theme = if !cfg_theme.is_empty() {
            cfg_theme.to_string()
        } else {
            crate::handle_mode::FALLBACK_THEME.to_string()
        };
        // 初始明暗：config.ui.theme.style（system 跟随系统实时探测，见 ThemeStyle::resolve_dark）。
        let theme_style_init = ThemeStyle::from_config(&config.ui.theme.style);

        // 标点转换器：只持引号交替态，自定义映射每次从实时配置读（故此处无需注入——
        // 曾在此注入一份副本且仅此一次，设置页改自定义标点必须重启服务才生效）。
        let punct_conv = PunctuationConverter::new();

        // 编码显示方式运行时初值（config 移入结构体前先算）。
        let preedit_display_init = config.ui.candidate.preedit();

        // 候选布局方向运行时初值（与下方 SetCandidateLayout 下发一致；config 移入前先算）。
        let candidate_vertical_init = config.ui.candidate.layout.eq_ignore_ascii_case("vertical");

        // 候选窗显隐运行时初值（ui.candidate.hide_window；此前恒为 false，配置不生效）。
        let hide_candidate_window_init = config.ui.candidate.hide_window;

        // 统计采集器：与 store 共享 Arc，内存聚合 + 后台定时 flush。
        let stat_collector = store.clone().map(StatCollector::new);
        // 启动初始状态：remember_last_state=true 时从 state.toml 恢复上次三态，否则用配置默认。
        let d = &config.input.default;
        let (init_chinese, init_full, init_punct) = if d.remember_last_state {
            (
                runtime_state.last_chinese_mode,
                runtime_state.last_full_width,
                runtime_state.last_chinese_punct,
            )
        } else {
            (d.chinese_mode, d.full_width, d.chinese_punct)
        };
        let (capslock_press_tx, capslock_press_rx) = std::sync::mpsc::channel::<()>();
        let coordinator = Arc::new(Self {
            state: Mutex::new(State {
                chinese_mode: init_chinese,
                full_width: init_full,
                chinese_punct: init_punct,
                s2t_enabled: config.input.s2t.enabled,
                filter_mode: wind_candidate::FilterMode::from_config(&config.input.filter_mode),
                scope_relaxed: false,
                toolbar_visible: config.ui.toolbar.visible, // 启动初值来自配置(运行时可菜单切换)
                ime_active: false, // 启动未激活：工具栏待 IME_ACTIVATED/FocusGained 才显示
                has_edit_context: false, // 同上：焦点尚未落到任何可编辑控件
                focus_no_edit_ctx: false, // 尚无权威判定，不表态
                caps_lock: false,
                input_buffer: String::new(),
                input_buffer_cased: String::new(),
                input_cursor_pos: 0,
                preedit: String::new(),
                preedit_split_body: String::new(),
                preedit_fp_body: String::new(),
                preedit_abbrev_body: String::new(),
                shadow_code: String::new(),
                shortcode_tops: [const { None }; 3],
                candidates: Vec::new(),
                selected_index: 0,
                current_page: 0,
                candidate_input: String::new(),
                candidate_limit: 0,
                has_more: false,
                committed_text: String::new(),
                committed_segs: Vec::new(),
                active: None,
                overlay_body: String::new(),
                temp_pinyin_buffer: String::new(),
                temp_pinyin_cursor: 0,
                temp_pinyin_schema: String::new(),
                temp_pinyin_prefix: String::new(),
                temp_english_buffer: String::new(),
                temp_english_cursor: 0,
                temp_english_prefix: String::new(),
                url_buffer: String::new(),
                url_cursor: 0,
                rewind: None,
                special_buffer: String::new(),
                special_cursor: 0,
                special_id: 0,
                overlay_spec: None,
                special_prefix: String::new(),
                mix_buffer: String::new(),
                mix_cursor: 0,
                mix_id: 0,
                mix_prefix: String::new(),
                mix_repeat: false,
                aux_code: None,
                caret_x: 0,
                caret_y: 0,
                caret_height: 0,
                caret_source: wind_ipc::protocol::caret_source::UNKNOWN,
                menu_open: false,
                menu_opened_at: None,
                menu_target_page_local: 0,
                menu_target_text: String::new(),
                add_word_active: false,
                add_word_chars: Vec::new(),
                add_word_len: 0,
                add_word_code: String::new(),
                add_word_boundary: 0,
            }),
            push_server,
            rt: std::sync::RwLock::new(std::sync::Arc::new(bundle)),
            ui_tx,
            engine_mgr,
            store,
            punct: Mutex::new(punct_conv),
            capslock_hook: Mutex::new(None),
            capslock_press_tx,
            smart_symbol: Mutex::new(SmartSymbolArm::default()),
            auto_phrase: Mutex::new(crate::auto_phrase::AutoPhraseBuf::new()),
            last_self_commit: Mutex::new(None),
            auto_phrase_writes: std::sync::atomic::AtomicUsize::new(0),
            phrases,
            system_phrase_entries: std::sync::RwLock::new(system_phrase_entries),
            system_phrase_path,
            s2t: Mutex::new(s2t),
            common_chars,
            toolbar_positions: Mutex::new(toolbar_positions_init),
            current_toolbar_monitor: Mutex::new(None),
            reverse: std::sync::RwLock::new(reverse),
            aux_code_table: std::sync::RwLock::new(None),
            quick_formats,
            // 空初值：真正的装载在 new() 里经 `reload_quick_adjust` 完成（需要 store，
            // 而 store 在本结构体构造之后才可用）。headless 无 store 时保持空 = 出厂顺序。
            quick_adjust: std::sync::RwLock::new(std::collections::HashMap::new()),
            chaizi_assets: Mutex::new(ChaiziAssets {
                db: chaizi_db,
                font: None, // 字体在 new() 经 sync_chaizi_assets 下发（headless 无 UI 不发）
            }),
            // 空初值 + new() 里的 sync_comment_dicts 首次加载：与拆字字体同一套「声明式变更
            // 检测」，构造期不做 IO，加载与热重载走同一条路径（不会出现只在启动生效的分叉）。
            comment_dict_paths: Mutex::new(Vec::new()),
            pair_tracker: Mutex::new(wind_transform::pair_tracker::PairTracker::new()),
            last_valid_caret: Mutex::new((0, 0, 0)),
            caret_independent: std::sync::atomic::AtomicBool::new(false),
            eager_prewarm: std::sync::atomic::AtomicBool::new(true),
            readiness_state: std::sync::atomic::AtomicU8::new(0),
            pending_first_show: Mutex::new(false),
            pending_first_show_token: Mutex::new(0),
            assoc_hide_token: Mutex::new(0),
            candidate_shown: Mutex::new(false),
            show_authorized: std::sync::atomic::AtomicBool::new(false),
            candidate_flipped: std::sync::atomic::AtomicBool::new(false),
            hover_index: std::sync::atomic::AtomicI32::new(-1),
            composition_start: Mutex::new((0, 0, false)),
            last_authoritative_caret: Mutex::new((0, 0, false)),
            last_key_at: Mutex::new(None),
            last_key_interval_ms: Mutex::new(None),
            first_show_was_provisional: std::sync::atomic::AtomicBool::new(false),
            caret_cache_verified: std::sync::atomic::AtomicBool::new(false),
            first_show_extended: std::sync::atomic::AtomicBool::new(false),
            pending_focus_tip: std::sync::atomic::AtomicBool::new(false),
            last_focus_tip_token: Mutex::new(0),
            app_compat: Mutex::new(app_compat),
            compat_dirs: (
                data_dir.map(|d| d.to_path_buf()),
                user_dir.as_ref().map(|d| d.to_path_buf()),
            ),
            active_compat: Mutex::new(ActiveCompat::default()),
            pid_names: Mutex::new(HashMap::new()),
            mode_scope: Mutex::new((0, false)),
            mode_states: Mutex::new(HashMap::new()),
            runtime_last: Mutex::new((init_chinese, init_full, init_punct)),
            last_caps_inject: Mutex::new(None),
            front_ctx: Mutex::new((String::new(), String::new(), String::new())),
            themes_dir,
            theme_name: Mutex::new(initial_theme),
            last_status_text: Mutex::new(String::new()),
            last_toolbar_push: Mutex::new(None),
            schema_toggle_origin: Mutex::new(None),
            theme_style: Mutex::new(theme_style_init),
            theme_index_labels: Mutex::new(Vec::new()),
            cmdbar_services: std::sync::OnceLock::new(),
            host_services: std::sync::OnceLock::new(),
            self_weak: std::sync::OnceLock::new(),
            recent_commits: Mutex::new(std::collections::VecDeque::new()),
            last_commit_len: std::sync::atomic::AtomicUsize::new(1),
            preedit_display: Mutex::new(preedit_display_init),
            hide_candidate_window: Mutex::new(hide_candidate_window_init),
            candidate_vertical: Mutex::new(candidate_vertical_init),
            candidate_layout_sent: Mutex::new(candidate_vertical_init),
            stat_collector,
            stat_recorded: std::sync::atomic::AtomicBool::new(false),
            fullscreen_cached: std::sync::atomic::AtomicBool::new(false),
            fullscreen_probing: std::sync::atomic::AtomicBool::new(false),
            #[cfg(windows)]
            host_render: std::sync::OnceLock::new(),
            last_input_diag: Mutex::new(Default::default()),
            input_block_gate: Mutex::new(InputBlockGate::default()),
            last_langbar_tooltip: Mutex::new(String::new()),
            last_window_diag: Mutex::new(Default::default()),
            password_suppress: std::sync::atomic::AtomicBool::new(false),
            password_suppress_enabled: std::sync::atomic::AtomicBool::new(true),
            input_diag_hud_visible: std::sync::atomic::AtomicBool::new(false),
            input_diag_sections: Mutex::new(Default::default()),
            input_diag_frozen: std::sync::atomic::AtomicBool::new(false),
            input_diag_topmost: std::sync::atomic::AtomicBool::new(true),
        });
        // CapsLock 钩子的动作消费线程。钩子回调只做非阻塞投递（它超时会被系统静默移除且
        // 无从察觉），真正的动作在这里执行，可安全加锁。未装钩子时它一直阻塞在 channel 上。
        // 起在这里而非 `new`：只有此处能同时拿到 `Arc<Self>` 与 receiver。
        {
            let c = Arc::clone(&coordinator);
            std::thread::Builder::new()
                .name("capslock-action".into())
                .spawn(move || {
                    for _ in capslock_press_rx {
                        c.handle_capslock_hook_press();
                    }
                    debug!("CapsLock 钩子事件通道已关闭");
                })
                .ok();
        }
        // 命令栏：装配 Services（ime/config/dict 后端）+ 自身 Weak 引用。
        coordinator.init_cmdbar();
        // 启动即显示常驻工具栏（反映初始 中英/方案/标点/全半角）
        coordinator.notify_toolbar();
        // 码元集与按键功能的冲突体检（只告警）。默认字符集下直接返回，无开销。
        coordinator.warn_code_char_conflicts();
        coordinator
    }

    /// 上屏历史快照（index 0 = 最近）。供命令栏 `last(n)` 读取。
    pub(crate) fn recent_commits_snapshot(&self) -> Vec<String> {
        self.recent_commits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// 前台上下文快照 `(app, title, sel)`，供命令栏 `app()/title()/sel()` 读取。
    pub(crate) fn front_ctx_snapshot(&self) -> (String, String, String) {
        self.front_ctx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 取当前「配置 + 派生缓存」快照（Arc 克隆，开销低）。所有配置读取经此。
    pub(crate) fn rt(&self) -> std::sync::Arc<ConfigBundle> {
        self.rt.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 回车键是否配置为「清空编码」（`input.enter_behavior = "clear"`）。
    ///
    /// 回车有五条彼此独立的处理路径（主输入 / 临时拼音 / 临时英文 / 混合输入 / 特殊模式），
    /// 此判据由它们共用。此前各路径内联比较字符串，且**只判在「空缓冲」分支上**，
    /// 于是「打了码再按回车」时配置静默失效、照旧上屏原码；收口成单一具名判据，
    /// 使「某条路径没接」退化为「没有调用点」这种更容易发现的缺失。
    pub(crate) fn enter_clears_composition(&self) -> bool {
        self.rt().config.input.enter_behavior == "clear"
    }

    /// 空码时按标点/符号键是否丢弃这串废码（`input.punct_on_empty_behavior = "clear"`）。
    ///
    /// 「空码」= 缓冲非空但一个候选都没有（多为码表打错字根）。此时既有行为是把废码连同
    /// 标点一起送上屏，用户要的往往是「这串码作废、句号照打」。
    ///
    /// ⚠️ 只管**无候选**那一支。有候选时按标点仍顶屏首选——那是「就选高亮那条吧」的既有
    /// 语义，与本开关无关。
    ///
    /// ⚠️ 与 `schema.codetable.punct_commit` 正交：那一项关掉是吞键、连标点都不输出。
    ///
    /// 判据收口在此而非内联比较字符串：标点有两个彼此独立的上屏出口（普通标点、智能符号
    /// `CommitAndHoldComposition`），内联必漏其一，而漏掉的那个是「只在开了智能符号的宿主
    /// 上复现」的间歇性不一致。参见 [`Self::enter_clears_composition`] 的同款教训。
    pub(crate) fn punct_clears_on_empty(&self) -> bool {
        self.rt().config.input.punct_on_empty_behavior == "clear"
    }

    /// 焦点/IME 激活时按 client_token 高 32 位的 PID 解析焦点进程名，缓存其 caret 兼容态
    /// （对齐 Go `HandleFocusGained` 设置 activeCompatRule）。按 pid 缓存：同进程命中直接返回，
    /// 避免每次焦点事件重复 OpenProcess。仅在重型/异步段调用，不在 DLL 同步阻塞路径上。
    fn update_active_compat(&self, client_token: u64) {
        let pid = (client_token >> 32) as u32;
        if pid == 0 {
            return;
        }
        // 缓存优先于反查：macOS 的 `.app` 随焦点事件把宿主 bundle id 送进 `pid_names`
        // （服务进程那边 `process_name` 恒返回空串），此处必须先读缓存才能拿到宿主名。
        // Windows 上首次见到该 pid 时缓存为空 → 照常 OpenProcess 反查，行为不变。
        //
        // ⚠ 在取 `active_compat` 锁**之前**读缓存：本函数末尾是「先 drop(ac) 再锁
        // pid_names」，两把锁在此嵌套会引入一个方向相反的持有序。
        let cached_name = self.cached_proc_name(client_token);
        let mut ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
        if ac.pid == pid {
            return; // 同进程，规则已缓存
        }
        let name = if cached_name.is_empty() {
            process_name(pid)
        } else {
            cached_name
        };
        let (next, rule_matched, rule_initial_mode, rule_initial_punct) = {
            let table = self.app_compat.lock().unwrap_or_else(|e| e.into_inner());
            let rule = table.get_rule(&name);
            let initial_mode = rule.and_then(|r| r.initial_mode);
            let initial_punct = rule.and_then(|r| r.initial_punct);
            (
                ActiveCompat {
                    pid,
                    caret_use_top: rule.map(|r| r.caret_use_top).unwrap_or(false),
                    first_show_mode: rule.map(|r| r.first_show_mode).unwrap_or_default(),
                    has_initial_rule: initial_mode.is_some() || initial_punct.is_some(),
                    auto_pair: rule.and_then(|r| r.auto_pair),
                    smart_method: rule.and_then(|r| r.smart_method),
                    caret_offset_x: rule.map(|r| r.caret_offset_x).unwrap_or(0),
                    caret_offset_y: rule.map(|r| r.caret_offset_y).unwrap_or(0),
                },
                rule.is_some(),
                initial_mode,
                initial_punct,
            )
        };
        // 无条件记录（对齐 Go handle_lifecycle.go:698）。原实现仅在 caret_use_top=true 时打，
        // 规则未命中与「命中但全 false」在日志里无从区分，查「某应用兼容项没生效」时看不到
        // 究竟是没匹配上进程名还是字段没读到。
        debug!(
            "Compat rule for process={name}: matched={} caret_use_top={} first_show_mode={} initial_mode={} initial_punct={} auto_pair={} smart_method={} caret_offset=({},{})",
            rule_matched,
            next.caret_use_top,
            next.first_show_mode.as_config(),
            rule_initial_mode
                .map(|m| m.as_config())
                .unwrap_or("(follow-global)"),
            rule_initial_punct
                .map(|m| m.as_config())
                .unwrap_or("(follow-global)"),
            match next.auto_pair {
                Some(true) => "on",
                Some(false) => "off",
                None => "(follow-global)",
            },
            match next.smart_method {
                Some(wind_config::config::SmartMethod::DeleteReplace) => "delete_replace",
                Some(wind_config::config::SmartMethod::HoldComposition) => "hold_composition",
                None => "(follow-global)",
            },
            next.caret_offset_x,
            next.caret_offset_y
        );
        *ac = next;
        drop(ac);
        // 顺带填 pid→进程名缓存，供 FOCUS_GAINED 同步路径免 OpenProcess 查询（per-app 状态）。
        if !name.is_empty() {
            self.pid_names
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(pid, name.to_lowercase());
        }
    }

    /// 客户端连接时校正 `pid_names` 缓存：现查一次真名，与缓存不符即覆盖并记 WARN。
    ///
    /// 为什么需要：`pid_names` 是**一次写入、永不失效**的 pid→名字缓存，而
    /// `cached_proc_name` / `update_active_compat` 都是缓存优先。Windows 会复用已退出
    /// 进程的 PID，于是「A 退出 → B 拿到同一个 pid」之后，B 会被永久当成 A——整条
    /// per-app 链一起错：compat 规则（`initial_mode` / `caret_*` / `auto_pair` /
    /// `smart_method` / `first_show_mode`）与中英记忆表。而且**没有任何自愈路径**：
    /// `update_active_compat` 对同 pid 直接早退，连重查的机会都没有。
    ///
    /// 选连接时机校正，是因为新进程必然连一次，而同进程多次连接（多 TSF 实例、管道抖动
    /// 后重连）重查一次也只是几微秒的 `OpenProcess`，宁可多查不可不查。
    ///
    /// ⚠ 现查为空时**保留缓存**：macOS 的服务进程 `process_name` 恒返回空串，那边的名字
    /// 由 `.app` 随焦点事件送进缓存。清掉会让 compat 规则在下一次 focus_gained 之前全部失配。
    #[cfg(any(windows, test))]
    pub(crate) fn revalidate_pid_name(&self, pid: u32, live_name: &str) {
        if pid == 0 || live_name.is_empty() {
            return;
        }
        let live = live_name.to_lowercase();
        let mut names = self.pid_names.lock().unwrap_or_else(|e| e.into_inner());
        match names.get(&pid) {
            Some(cached) if *cached == live => {}
            Some(cached) => {
                // 这条 WARN 就是 PID 复用的现场证据。缓存过一个名字、现查却是另一个，
                // 只可能是那个 pid 换了进程——在此之前它一直是静默错配。
                tracing::warn!(
                    "pid_names 校正：pid={pid} 缓存={cached} 实际={live}（PID 已被复用，此前按缓存匹配的 per-app 规则是错的）"
                );
                names.insert(pid, live);
            }
            None => {
                names.insert(pid, live);
            }
        }
    }

    /// `MessageHandler::handle_client_connected` 的纯逻辑部分：只有 `pid` 确实等于当前
    /// 前台窗口的 pid 才刷新规则字段，避免后台宿主的无关重连（管道抖动等）覆盖掉
    /// 真正聚焦应用的 per-app 兼容态。`foreground_pid` 作为参数传入而非内部现查，是为了
    /// 脱离真实 `GetForegroundWindow` 单测——`dpi_scale_for_point` 已经吃过一次「测试跑在
    /// 真实系统 API 上导致断言随运行环境漂移」的教训。
    ///
    /// 生产调用点仅有 `handle_client_connected` 的 `#[cfg(windows)]` 分支；非 Windows 的
    /// 非测试构建没有调用方也没有本函数（连同 `refresh_active_compat_rule_fields` 一起
    /// 用同一个 cfg 门控，避免出现「函数存在但调不到」的死代码）。
    #[cfg(any(windows, test))]
    fn apply_connected_pid_compat(&self, pid: u32, foreground_pid: u32) {
        if foreground_pid != pid {
            return;
        }
        self.refresh_active_compat_rule_fields(pid);
    }

    /// 只刷新 `active_compat` 里「当前生效设置」那一半字段（`caret_use_top` /
    /// `first_show_mode` / `auto_pair` / `smart_method` / `caret_offset_*`），**刻意不碰
    /// `.pid` 与 `.has_initial_rule`**。
    ///
    /// 这两个字段的另一重身份是「上一次真实 `FOCUS_GAINED` 落在哪个进程」——
    /// `get_current_mode`（DLL 同步路径）与 `handle_focus_gained` 的 `crossed` 判据都靠
    /// 它俩识别「这次是不是跨进程切入」。连接建立**不是**真实的焦点事件：对一个全新启动、
    /// TSF DLL 第一次在其中加载、且此刻恰好已在前台的进程，管道连接必然先于它有史以来
    /// 第一条 `FOCUS_GAINED`（发不出消息就说明还没连上）。若这里跟 `update_active_compat`
    /// 一样整体覆写 `ActiveCompat`，会让 `.pid` 提前变成新进程，随后真正到达的那条
    /// `FOCUS_GAINED` 就会被 `crossed` 误判成「同进程」，吞掉 `should_reapply_initial`
    /// （该应用的 `initial_mode`/`initial_punct` 规则）与 `get_current_mode` 的首键竞态
    /// 消除逻辑（2026-08-17 code review 发现，未真机复现）。
    ///
    /// 字段提取逻辑刻意与 `update_active_compat` 分开写而非提取共用：那是已跑通真机验证的
    /// 现有函数，为省几行重复去动它的取值顺序/锁持有范围不划算——两处字段列表如有出入，
    /// 应同步核对。
    ///
    /// 未做「同 pid 重复调用去重」：不像 `update_active_compat` 有 `.pid` 可比对，本函数
    /// 没有身份缓存可用；接受每次连接都重新 `OpenProcess` 一次（<1ms，且连接本就是低频
    /// 事件，不是按键路径）。
    #[cfg(any(windows, test))]
    fn refresh_active_compat_rule_fields(&self, pid: u32) {
        if pid == 0 {
            return;
        }
        let cached_name = self.cached_proc_name((pid as u64) << 32);
        let name = if cached_name.is_empty() {
            process_name(pid)
        } else {
            cached_name
        };
        if name.is_empty() {
            return;
        }
        let (
            caret_use_top,
            first_show_mode,
            auto_pair,
            smart_method,
            caret_offset_x,
            caret_offset_y,
        ) = {
            let table = self.app_compat.lock().unwrap_or_else(|e| e.into_inner());
            let rule = table.get_rule(&name);
            (
                rule.map(|r| r.caret_use_top).unwrap_or(false),
                rule.map(|r| r.first_show_mode).unwrap_or_default(),
                rule.and_then(|r| r.auto_pair),
                rule.and_then(|r| r.smart_method),
                rule.map(|r| r.caret_offset_x).unwrap_or(0),
                rule.map(|r| r.caret_offset_y).unwrap_or(0),
            )
        };
        debug!(
            "Connected-pid compat refresh for process={name} (pid={pid}): caret_use_top={} first_show_mode={} auto_pair={} smart_method={} caret_offset=({},{})",
            caret_use_top,
            first_show_mode.as_config(),
            match auto_pair {
                Some(true) => "on",
                Some(false) => "off",
                None => "(follow-global)",
            },
            match smart_method {
                Some(wind_config::config::SmartMethod::DeleteReplace) => "delete_replace",
                Some(wind_config::config::SmartMethod::HoldComposition) => "hold_composition",
                None => "(follow-global)",
            },
            caret_offset_x,
            caret_offset_y
        );
        {
            let mut ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
            ac.caret_use_top = caret_use_top;
            ac.first_show_mode = first_show_mode;
            ac.auto_pair = auto_pair;
            ac.smart_method = smart_method;
            ac.caret_offset_x = caret_offset_x;
            ac.caret_offset_y = caret_offset_y;
        }
        self.pid_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pid, name.to_lowercase());
    }

    /// 按 client_token 高 32 位的 PID 查已缓存的进程名（小写）。未缓存返回空串。
    /// 仅 HashMap 查询，可用于 DLL 同步阻塞路径。
    fn cached_proc_name(&self, client_token: u64) -> String {
        let pid = (client_token >> 32) as u32;
        if pid == 0 {
            return String::new();
        }
        self.pid_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
            .cloned()
            .unwrap_or_default()
    }

    /// 消费一次输入诊断上报（compartment 禁用态 + InputScope 掩码）：更新 `last_input_diag`
    /// 快照，并按 `password_suppress_enabled` 开关决定是否强制英文抑制（密码框场景）。
    pub(crate) fn apply_input_diag(&self, pid: u32, disabled: bool, reason_byte: u8, mask: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        let reason = crate::input_diag::reason_from(disabled, mask);
        // 本地一律以 mask/disabled 经 reason_from 推导 reason 作准；上报的 reason_byte
        // 仅供展示/日志参考，不参与本地决策（避免"双重来源"歧义）。
        let _ = reason_byte; // 上游已按 mask/disabled 推导 reason；保留形参对齐上报字段序。
        let name = if pid != 0 {
            self.cached_proc_name((pid as u64) << 32)
        } else {
            String::new()
        };
        // 抑制：命中密码 InputScope 位 且 策略开关开 → 强制英文。
        //
        // ⚠ 曾经这里还有一条 `&& !disabled`，理由是「disabled 时 DLL 已放行所有键、引擎收不到
        // 键，抑制 moot」。那条推理错在 `disabled` 的层级：DLL 放行看的是**线程级**
        // KEYBOARD_DISABLED，而 Windows 侧当时往这个字段传的是**context 级**的密码框判定。
        // 于是 Chromium 网页密码框（只置 context 级）被这条判据整个否掉——键没被放行、抑制也
        // 不生效，密码框里照打中文，高级菜单的开关看着像坏了。2026-07-27 两侧一并修正：
        // `disabled` 统一为线程级语义，密码信号只走 mask。
        //
        // 现在 disabled 只参与 `reason_from` 的展示推导，不再进决策——单一来源，避免再次歧义。
        // 线程级 disabled 为真时本判据仍可能算出 suppress=true，这是**安全的**：那时 DLL 在
        // OnTestKeyDown 开头就全放行了，一个键都不会送到引擎，suppress 取值无从被观测。
        // 危险的只有反方向（core 抑制而 DLL 吃键 → 「吃了再吐」丢键），故不变量是
        // **core.suppress ⊆ C++.suppress**，见 C++ `IsPasswordSuppressActive`。
        let suppress = crate::input_diag::is_password_scope(mask)
            && self.password_suppress_enabled.load(Relaxed);
        self.password_suppress.store(suppress, Relaxed);
        {
            let mut d = self
                .last_input_diag
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *d = crate::input_diag::InputDiagState {
                pid,
                process_name: name,
                disabled,
                reason,
                mask,
            };
        }
        self.push_input_diag_hud_if_visible();
    }

    /// 消费一次诊断快照：存 DLL 上报的窗口链 / TSF 实例。
    ///
    /// ⚠ host-render 运行态（白名单 / 活跃）**不在这里算**——它们是服务端随时可查的实时值，
    /// 存进快照就等于被冻结在「快照到达那一刻」。而 `active_target` 恰恰要到**首次按键**
    /// 才置位（searchapp/SearchHost 这类 transient DocMgr 宿主不发 focus_gained，note_focus
    /// 只能走 CMD_KEY_EVENT），快照却在 OnSetFocus 就发出了 ⇒ 存下来的必然是 `活跃: 否`，
    /// 让人误判成 host render 没生效。现算在 [`Self::push_input_diag_hud`]。
    pub(crate) fn apply_diag_snapshot(&self, snap: &wind_ipc::protocol::DiagSnapshotPayload) {
        // 进程名：服务端按 pid 现查（DLL 不上报——它未必有权限打开别的进程）。
        // 快照来源进程与前台进程分别查：多进程宿主下它们本就可能不同，而「本快照来自谁」
        // 是判读整份数据的前提（见 `WindowDiagView::pid`）。
        let proc_name = |pid: u32| {
            if pid != 0 {
                self.cached_proc_name((pid as u64) << 32)
            } else {
                String::new()
            }
        };
        let process_name = proc_name(snap.pid);
        let fg_process_name = proc_name(snap.fg_pid);

        {
            let mut w = self
                .last_window_diag
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *w = crate::input_diag::WindowDiagView {
                pid: snap.pid,
                process_name,
                focus_hwnd: snap.focus_hwnd,
                focus_class: snap.focus_class.clone(),
                focus_source_label: wind_ipc::protocol::window_source::label(
                    snap.focus_hwnd_source,
                )
                .to_string(),
                root_hwnd: snap.root_hwnd,
                root_class: snap.root_class.clone(),
                root_band: snap.root_band,
                fg_hwnd: snap.fg_hwnd,
                fg_class: snap.fg_class.clone(),
                fg_pid: snap.fg_pid,
                fg_process_name,
                docmgr_id: snap.docmgr_id,
                context_id: snap.context_id,
                focus_session_id: snap.focus_session_id,
                docmgr_changed: snap.docmgr_changed(),
                host_band: snap.host_band,
                // 这两项由 push 时现算填入（见本函数文档），此处留默认值。
                host_whitelisted: false,
                host_active: false,
                received: true,
            };
        }
        self.push_input_diag_hud_if_visible();
    }

    /// 下发诊断快照采集开关给 DLL（随 HUD 显隐 + 握手时）。
    ///
    /// 采集要查三次窗口类名 + band，故默认关；**握手时必须也推一次**——DLL 每次重连都从
    /// 默认值（关）起步，只在切换时推会让重连后的宿主永远不采集，而 SearchHost 这类
    /// transient 宿主恰恰最常重连，也恰恰最需要 HUD（它是 AppContainer，写不了日志）。
    pub fn push_diag_snapshot_config(&self, client_token: u64) {
        let enabled = self
            .input_diag_hud_visible
            .load(std::sync::atomic::Ordering::Relaxed);
        let value = wind_ipc::codec::encode_diag_snapshot_value(enabled);
        let msg = wind_ipc::codec::encode_sync_config(
            wind_ipc::protocol::CONFIG_KEY_DIAG_SNAPSHOT,
            &value,
        );
        if client_token != 0 {
            self.push_server.push_to_token(client_token, &msg);
        } else {
            self.push_server.push_to_active(&msg);
        }
    }

    /// HUD 推送（数据到达路径）：HUD 可见且**未冻结**时下发一帧。
    pub(crate) fn push_input_diag_hud_if_visible(&self) {
        self.push_input_diag_hud(false);
    }

    /// HUD 推送。`force=true` 时无视冻结照常下发。
    ///
    /// ⚠ 冻结只该挡住**数据变化**引起的刷新，不该挡住用户自己的操作（切分区/切置顶/
    /// 切冻结本身）。两者混为一谈的后果是"点了菜单屏幕毫无反应"——而那与"菜单坏了"
    /// 在用户眼里完全一样。故所有菜单动作一律走 `force=true`。
    pub(crate) fn push_input_diag_hud(&self, force: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        if !self.input_diag_hud_visible.load(Relaxed) {
            return;
        }
        if !force && self.input_diag_frozen.load(Relaxed) {
            return;
        }
        let d = self
            .last_input_diag
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // 取 state 快照：HUD 要显示决定工具栏可见性的两个正交状态位。
        // 先 drop 掉 last_input_diag 的锁再取 state 锁，避免与其它路径形成反序嵌套。
        let (process_name, pid, disabled, reason, mask) =
            (d.process_name.clone(), d.pid, d.disabled, d.reason, d.mask);
        drop(d);
        let (ime_active, has_edit_context) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.ime_active, s.has_edit_context)
        };
        // 窗口快照独立取（锁序：last_input_diag → state → last_window_diag，全程不嵌套）。
        #[cfg_attr(not(windows), allow(unused_mut))] // host-render 现算段仅 Windows 有
        let mut window = self
            .last_window_diag
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // host-render 运行态**在此现算**，不沿用快照里的值：它们随时可查，存进快照就会被
        // 冻结在快照到达那一刻（详见 `apply_diag_snapshot` 文档）。
        //
        // ⚠ 必须按**快照来源进程**的 pid 直查，不得走 `ActiveCompat` 全局焦点槽——开始菜单
        // 弹出会连带激活兄弟进程污染该槽，那正是当初 avail 位被污染、DLL 陷入销毁重建循环的
        // 成因（`docs/redesign/host-render-windows-port.md` §11.2）。
        #[cfg(windows)]
        if window.pid != 0
            && let Some(mgr) = self.host_render()
        {
            window.host_whitelisted = mgr.is_process_whitelisted(window.pid);
            window.host_active = mgr.active_target().is_some_and(|t| t.pid == window.pid);
        }
        let sections = *self
            .input_diag_sections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let view = wind_ui_types::InputDiagView {
            process_name,
            pid,
            disabled,
            reason_text: crate::input_diag::reason_label(reason).to_string(),
            mask,
            ime_active,
            has_edit_context,
            window,
            sections,
            topmost: self.input_diag_topmost.load(Relaxed),
            frozen: self.input_diag_frozen.load(Relaxed),
        };
        let _ = self
            .ui_tx
            .send(wind_ui_types::UiCommand::ShowInputDiag(view));
    }

    /// 查 `compat.toml` 中该进程的初始中英规则；`None` = 未配置（不干预）。
    ///
    /// 仅 HashMap 查询，无 OpenProcess，故可用于 DLL 同步阻塞路径（`get_current_mode`）。
    pub(crate) fn rule_initial_mode(
        &self,
        proc_name: &str,
    ) -> Option<wind_config::app_compat::InitialMode> {
        if proc_name.is_empty() {
            return None;
        }
        self.app_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_rule(proc_name)
            .and_then(|r| r.initial_mode)
    }

    /// 查 `compat.toml` 中该进程的初始中英标点规则；`None` = 未配置（不干预）。
    pub(crate) fn rule_initial_punct(
        &self,
        proc_name: &str,
    ) -> Option<wind_config::app_compat::InitialMode> {
        if proc_name.is_empty() {
            return None;
        }
        self.app_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_rule(proc_name)
            .and_then(|r| r.initial_punct)
    }

    /// 决策进程 `proc_name` 的中英初始状态（初始状态语义的单一内聚点）。
    ///
    /// 顺序：**按应用规则表（compat.toml）** → per-app 记忆表 → 全局记忆 / 配置默认。
    ///
    /// ⚠ 规则表排在记忆表**之前**是刻意的，与此处原 `TODO(app_rules)` 注释设想的位置相反。
    /// 原设想是「首次进入时生效，之后跟随用户手切」，那个语义对 Everything / Listary 这类
    /// **常驻隐藏式**窗口不成立：进程始终不退出，会话级记忆表里「首次」只有一次，用户从第二次
    /// 唤出起规则就再也不生效。放到记忆表之前，配合 `apply_initial_mode` 的跨进程守卫，语义
    /// 才是「每次从别的应用切进来都套用，停留在该应用期间尊重手切」。
    ///
    /// 规则是**初始值不是锁定**：它只在焦点跨进程切入的那一刻参与决策，此后用户手切自由，
    /// 且同应用内的焦点跳转不会重新套用（守卫见 `apply_initial_mode` 调用点）。
    fn initial_chinese_mode_for(&self, proc_name: &str) -> bool {
        let bundle = self.rt();
        let d = &bundle.config.input.default;
        if let Some(m) = self.rule_initial_mode(proc_name) {
            return m.is_chinese();
        }
        if d.per_app_scope()
            && !proc_name.is_empty()
            && let Some(&m) = self
                .mode_states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(proc_name)
        {
            return m;
        }
        if d.remember_last_state {
            self.runtime_last
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .0
        } else {
            d.chinese_mode
        }
    }

    /// 用户主动切换中英/全半角/标点后记录"最后状态"镜像；
    /// remember_last_state=true 时同步落盘 state.toml（复用 toolbar_positions 的 load-modify-save 模式）。
    /// 必须在释放 state 锁后调用。
    pub(crate) fn record_last_state(&self) {
        let (c, f, p) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.chinese_mode, s.full_width, s.chinese_punct)
        };
        *self.runtime_last.lock().unwrap_or_else(|e| e.into_inner()) = (c, f, p);
        if self.rt().config.input.default.remember_last_state
            && let Some(dir) = Config::state_dir()
        {
            let mut rs = wind_config::RuntimeState::load(&dir);
            rs.last_chinese_mode = c;
            rs.last_full_width = f;
            rs.last_chinese_punct = p;
            let _ = rs.save(&dir);
        }
    }

    /// state_scope="app" 时把中英状态写回当前前台进程的记忆表（进程名取自 pid 缓存）。
    pub(crate) fn record_app_mode(&self, chinese: bool) {
        if !self.rt().config.input.default.per_app_scope() {
            return;
        }
        let pid = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pid;
        let name = self
            .pid_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
            .cloned()
            .unwrap_or_default();
        if !name.is_empty() {
            self.mode_states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(name, chinese);
        }
    }

    /// 「切换**中英模式**时取消大小写锁定」（input.capslock.cancel_on_mode_switch）：
    /// CapsLock 开着时 `effective_chinese = chinese_mode && !caps_lock` 恒为英文大写，
    /// 切中英"看似无效"。开启该配置后，切换动作先物理敲击 CapsLock 取消系统
    /// 锁定并同步镜像，让切换真正生效。返回是否执行了取消（供调用方决定归位语义）。
    /// 需在未持有 state 锁时调用。
    ///
    /// ⚠ **切方案不走这里**，走无条件的 [`Self::force_cancel_caps_lock`]。判据是「这个动作
    /// 的语义前提是不是『我要用中文打字』」：切中英模式时用户可能正是要打大写英文，取消
    /// 大写会与他的意图相反，故留给配置；切方案则不然——没有任何解释能让「切到五笔之后
    /// 继续打大写英文」成立。把两者共用一个开关，就是本开关（出厂 false）关着时
    /// 「英文态/大写态下方案切换看着毫无反应」的成因。
    /// 工具栏推送去重：与上次推的相同则返回 `false`（调用方跳过下发）。
    ///
    /// 只做「内容比对」，**不判断该不该显示**——那是 `notify_toolbar` 四项合取的事。
    /// 相同即跳过是安全的：UI 侧的工具栏是纯粹的状态镜像，没有需要靠重复消息驱动的
    /// 动画或计时。
    ///
    /// ⚠️ 配置热重载后要 `reset_toolbar_push_dedup`：那条路径可能改变工具栏的呈现
    /// （显隐开关、全屏策略），而 `ToolbarState` 里并不带这些量，光比内容会把该重推的
    /// 那一次判成「没变」——同 `last_status_text` 在 reload 里被清空的理由。
    pub(crate) fn take_toolbar_push_if_changed(&self, want: ToolbarPush) -> bool {
        let mut last = self
            .last_toolbar_push
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&want) {
            return false;
        }
        *last = Some(want);
        true
    }

    /// 清空工具栏推送去重缓存，使下一次 `notify_toolbar` 必定下发。
    pub(crate) fn reset_toolbar_push_dedup(&self) {
        *self
            .last_toolbar_push
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn cancel_caps_on_switch(&self) -> bool {
        if !self.rt().config.input.capslock.cancel_on_mode_switch {
            return false;
        }
        self.force_cancel_caps_lock()
    }

    /// 取消大小写锁定，**不看配置开关**。仅供语义前提为「我要用中文打字」的动作调用
    /// （目前只有切方案，见 `finish_user_schema_switch`）。
    ///
    /// CapsLock 未开时返回 false 且不注入——「没开着」与「开关关着」在调用方看来都是
    /// 「本次没取消」，两者都不该触发归位记账。
    pub(crate) fn force_cancel_caps_lock(&self) -> bool {
        {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !s.caps_lock {
                return false;
            }
        }
        // 防抖：同一轮切换动作内不重复注入（一次注入的系统回环在几十 ms 内完成）。
        // 振荡回路的主熔断在 C++ 侧（OPENCLOSE 的 CapsLock 联动抑制 + Ctrl 判据），
        // 此处窗口必须远小于用户连续两轮「开大写→切换」的最短间隔——曾设 1500ms，
        // 实测会吞掉快节奏的第二轮合法请求（表现为"有时要按两次"），勿再调大。
        {
            let mut last = self
                .last_caps_inject
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(t) = *last
                && t.elapsed() < std::time::Duration::from_millis(300)
            {
                debug!("cancel_caps_lock: 注入防抖期内，跳过");
                return false;
            }
            *last = Some(std::time::Instant::now());
        }
        // SendInput 敲击 VK_CAPITAL；失败（非 Windows/注入受限）不动镜像，行为退回「没取消」。
        if let Err(e) = wind_keys::key_inject::tap_caps_lock() {
            warn!("cancel_caps_lock: 注入 CapsLock 失败: {e}");
            return false;
        }
        // 乐观同步镜像（后续按键立即按新状态处理）；注入回环的 CapsLock key_up
        // 状态通知（toggles bit=0）随后到达时与此幂等。
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .caps_lock = false;
        true
    }

    /// IME 激活 / 焦点切换（重型段）时按配置矩阵落地初始状态。
    /// `reset_aux`＝激活场景：remember=false 时同时重置全半角/标点为配置默认
    /// （焦点切换场景不重置——同一激活期内切窗口不动全半角/标点）。
    /// 需在未持有 state 锁时调用。
    fn apply_initial_mode(&self, client_token: u64, reset_aux: bool) {
        let bundle = self.rt();
        let d = &bundle.config.input.default;
        let proc = self.cached_proc_name(client_token);
        let chinese = self.initial_chinese_mode_for(&proc);
        let rule_punct = self.rule_initial_punct(&proc);
        let follow = bundle.config.input.punct.follow_mode;
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if reset_aux && !d.remember_last_state {
            s.full_width = d.full_width;
            s.chinese_punct = d.chinese_punct;
        }
        if s.chinese_mode != chinese {
            s.chinese_mode = chinese;
            // 标点随中英文切换（对齐 handle_toggle_mode/handle_system_mode_switch）。
            if follow {
                s.chinese_punct = chinese;
            }
        }
        // per-app 标点规则**最后**落地，压过 follow_mode 的推导与 reset_aux 的重置。
        // 顺序反了的话，用户配了 initial_punct 却恰好开着 follow_mode 时，规则会被
        // 上面那行静默覆盖——「配了没反应、日志里也没有痕迹」正是本仓反复出现的形态。
        if let Some(p) = rule_punct {
            s.chinese_punct = p.is_chinese();
        }
    }

    /// 「当前焦点为什么打不出中文」的**真值**（未过迟滞）。呈现请用
    /// [`Self::effective_input_block`]。
    ///
    /// 三个信号源都已由 DLL 上报到位，无需新增 IPC：
    /// · 线程级禁用 → `focus_gained` 的 `disabled` 字段（DLL 注释写明该字段**统一是线程级**，
    ///   密码框那一层折在 mask 的 IS_PASSWORD 位里，两者不可混为一谈）；
    /// · 密码框     → `apply_input_diag` 据 mask 置位的 `password_suppress`，与输入闸同源；
    /// · 无编辑上下文 → `focus_lost(NoEditCtx/CtxLost)` / `focus_gained` 维护的 `has_edit_context`。
    ///
    /// ⚠ `ime_active` 为假时一律返回 `None`：那说明本输入法根本没在服务任何宿主
    /// （用户切到了别的输入法），此时 `has_edit_context` 恒假，不早退就会把图标永久钉成「英」。
    pub(crate) fn input_block(&self) -> InputBlock {
        let (ime_active, no_edit_ctx) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            // ⚠ 读 `focus_no_edit_ctx` 而**不是** `has_edit_context`：后者被噪声层的
            // CtxLost 置假，拿它驱动图标会在焦点根本没离开输入框时显「英」（实测见字段注释）。
            (s.ime_active, s.focus_no_edit_ctx)
        };
        if !ime_active {
            return InputBlock::None;
        }
        if self
            .last_input_diag
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disabled
        {
            return InputBlock::KeyboardDisabled;
        }
        if self
            .password_suppress
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return InputBlock::Password;
        }
        if no_edit_ctx {
            return InputBlock::NoEditContext;
        }
        InputBlock::None
    }

    /// 过了迟滞闸门之后**该呈现**的档位。图标与工具栏都只许读这一个。
    ///
    /// 未到期时返回旧值并安排一次复查——否则 churn 停下后没有任何事件会再驱动它，
    /// 状态会永久停在「差一点就该变」。
    pub(crate) fn effective_input_block(&self) -> InputBlock {
        let now = self.input_block();
        let mut g = self
            .input_block_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if now == g.shown {
            // 回到已呈现的状态 ⇒ 撤销待定。churn 期间来回翻转会次次撤销，图标因此稳定不动。
            g.pending_since = None;
            return g.shown;
        }
        if now == InputBlock::None {
            // 恢复方向：立即。
            g.shown = now;
            g.pending_since = None;
            return now;
        }
        // 进入方向：要求稳定 INPUT_BLOCK_DELAY。
        // ⚠ 用 `get_or_insert` 而非直接赋值：每次事件都重置起点的话，churn 下永远等不到到期。
        let t0 = *g.pending_since.get_or_insert(std::time::Instant::now());
        let waited = t0.elapsed();
        if waited >= INPUT_BLOCK_DELAY {
            g.shown = now;
            g.pending_since = None;
            tracing::debug!("input_block → {:?}（已稳定 {:?}）", now, waited);
            return now;
        }
        if !g.probing {
            g.probing = true;
            let remaining = INPUT_BLOCK_DELAY - waited;
            let weak = self.self_weak.get().cloned();
            let spawned = std::thread::Builder::new()
                .name("input-block-gate".into())
                .spawn(move || {
                    std::thread::sleep(remaining);
                    if let Some(c) = weak.and_then(|w| w.upgrade()) {
                        c.input_block_gate
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .probing = false;
                        // 走收口点重新评估：它会再调 effective_input_block，
                        // 那时若状态仍不变就到期落地，变回去了则被上面的撤销分支吃掉。
                        c.notify_toolbar();
                    }
                });
            if spawned.is_err() {
                // 线程没起来就把闸放回去，否则此后永远不再复查。
                g.probing = false;
            }
        }
        g.shown
    }

    /// 热重载用户配置：从磁盘重读 Config 并原子替换 bundle（轻量设置即时生效），
    /// 再 best-effort 刷新主题/工具栏。返回是否仍需重启才能完全生效。
    /// 轻量项（标点/智能符号/候选数/热键/配对/导航键等）即时生效；重型项（引擎/方案/
    /// 词典/字体）当前不在 bundle 内，需重启——为不打断使用，这里统一返回 false，
    /// 由调用方/用户按需重启。
    /// 同步拆字资产到当前来源方案（`chaizi_spec`：码表=自身、混输=其主码表成员、拼音=全局
    /// 主码表，与编码段同源）：库路径变了才重载反查表拆字段（含变为无配置时清空释放内存），
    /// 字根字体变了才重发（渲染端每次 set 都重建字体集，勿重复下发）。调用点=启动、方案切换
    /// （菜单/循环/设置页）、reload_user_config(schema_dirty)。资源相对路径按「用户方案目录
    /// 优先、回落系统数据目录」解析（与方案文件同规则）。
    pub(crate) fn sync_chaizi_assets(&self) {
        let data_dir = Config::data_dir();
        let spec = self.engine_mgr.chaizi_spec();
        let new_db = spec
            .as_ref()
            .filter(|c| !c.db_path.is_empty())
            .and_then(|c| {
                let p = Config::resolve_schema_resource(data_dir.as_deref(), &c.db_path);
                if p.is_none() {
                    warn!(
                        "拆字库不存在（用户/系统 schemas 目录均未找到）: {}",
                        c.db_path
                    );
                }
                p
            });
        let new_font = spec
            .as_ref()
            .filter(|c| !c.font_path.is_empty())
            .and_then(|c| {
                Config::resolve_schema_resource(data_dir.as_deref(), &c.font_path)
                    .map(|p| (p.to_string_lossy().into_owned(), c.font_family.clone()))
            });
        let mut assets = self.chaizi_assets.lock().unwrap_or_else(|e| e.into_inner());
        if assets.db != new_db {
            self.reverse
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .reload_chaizi(new_db.as_deref());
            assets.db = new_db;
        }
        if new_font != assets.font {
            // 变为 None 时仅不再重发（字体集无撤销接口；旧字体仅影响 PUA 段渲染，无害）。
            if let Some((path, family)) = &new_font {
                let _ = self.ui_tx.send(UiCommand::SetTooltipChaiziFont {
                    path: path.clone(),
                    family: family.clone(),
                });
            }
            assets.font = new_font;
        }
    }

    /// 同步注释词库（`[[ui.comment_dicts]]`）到反查表：解析路径列表，与上次生效的比对，
    /// **变了才重载**。调用点=启动、reload_user_config、切方案（switch/cycle）。
    ///
    /// 变更检测比的是**解析后的路径序列**（含顺序）而非配置结构：顺序即优先级，调换两个库
    /// 的位置必须触发重载；而只改 `label` 这类不影响加载的字段则不该重载。有了 `.wcmt`
    /// 缓存，重载本身只是重开 mmap，但切方案是高频操作，能不动就不动。
    ///
    /// **按活跃方案过滤**（`schemas` 字段，留空=全部）：一份大英汉词典挂在五笔方案上，
    /// 每次输入都要多走一次注定查不到的二分。方案专属的库因此只在其方案下加载 ——
    /// 这也是切方案要调本函数的原因。
    ///
    /// 路径**以 `schemas/` 为基准**解析（`resolve_schema_resource`，用户目录优先、回落安装
    /// 目录），与拆字库、字根字体这些方案附属资源同一规则 —— 注释库本就是同类东西：
    /// 放在 `schemas/` 下、随整机备份走（`user_schemas_dir` 递归打包）、不参与召回。
    /// 配置里因此写 `comments/xxx.dict.yaml` 而非 `schemas/comments/xxx.dict.yaml`。
    pub(crate) fn sync_comment_dicts(&self) {
        let data_dir = Config::data_dir();
        let specs = {
            let rt = self.rt();
            rt.config.ui.comment_dicts.clone()
        };
        let active = self.engine_mgr.active_schema_id();
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for s in specs
            .iter()
            .filter(|s| s.enabled && !s.path.is_empty() && s.applies_to(&active))
        {
            match Config::resolve_schema_resource(data_dir.as_deref(), &s.path) {
                // 按**解析后路径**去重：两条 spec 写不同的相对路径却指向同一个文件时
                // （`a.dict.yaml` 与 `./a.dict.yaml`，或用户目录与安装目录同名文件都被
                // 解析到同一处），只加载一次。重复加载除了浪费解析时间，还会让优先级
                // 判定变得依赖「第几次出现」——去重后靠前那条恒胜出。
                Some(p) if paths.contains(&p) => {
                    info!("注释词库重复挂载，已跳过: {} (id={})", p.display(), s.id)
                }
                Some(p) => paths.push(p),
                // 只 warn 不中断：一个库路径写错不该让其余库一起不加载。
                None => warn!(
                    "注释词库不存在（用户/安装目录均未找到）: {} (id={})",
                    s.path, s.id
                ),
            }
        }
        let mut cur = self
            .comment_dict_paths
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *cur == paths {
            return;
        }
        // 注释库缓存与词库 .wdat **同根**：`comment_cache_path` 自己按源文件父目录名分
        // 命名空间（`schemas/comments/x.dict.yaml` → `<cache>/comments/x.wcmt`），与
        // `EngineManager::cache_path` 同构，不再另立一层专用目录。
        // 无缓存目录（便携/测试）时传 None，注释库退化为内存加载，功能不受影响。
        let cache_dir = Config::cache_dir();
        self.reverse
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .reload_comments(&paths, cache_dir.as_deref());
        *cur = paths;
    }

    /// 辅助码表缓存失效：方案切换后置 `None`，下次进入辅助码时按新方案的
    /// `[engine.aux_code]` 重新懒加载（`ensure_aux_code_table` 只在 `None` 时加载）。
    ///
    /// 缓存是**全局共享一份**、不区分方案，而各方案码表不同（拼音用笔画表、双拼用
    /// 小鹤全码表）——切方案不清缓存会让双拼仍在用拼音那份表。与 `sync_chaizi_assets`
    /// / `sync_comment_dicts` 同源：方案附属资源随活跃方案切换重挂载。
    pub(crate) fn invalidate_aux_code_table(&self) {
        *self
            .aux_code_table
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 就地改写内存配置并重建 ConfigBundle，**不触发** reload_user_config 的那一整套副作用
    /// （toast、引擎热重建、热键重注册、主题下发、向 TSF 推 IPC 配置）。
    ///
    /// 用于「改动只影响少数几个 UI 字段、且发生频率高」的场景——典型是拖动窗口后落盘位置：
    /// 走 reload_user_config 会每拖一次弹一个「设置已更新」toast，明显不合适。
    /// 调用方仍需自行用 `Config::set_user_*` 把值写盘，本函数只负责让内存态立刻跟上。
    /// 候选窗定位参数 `(fixed, fixed_x, fixed_y)`，随每次 `UpdateCandidates` 下发。
    ///
    /// fixed 时 UI 侧忽略光标坐标，改用 `custom_x/custom_y`；`(0,0)` 表示"已开启固定
    /// 但用户还没拖过"，由 UI 落到屏幕默认锚点。快捷加词面板复用同一个候选窗实例，
    /// 因此也走这里——否则同一个窗口会在"加词时跟随、打字时固定"之间来回跳。
    pub(crate) fn candidate_fixed_pos(&self) -> (bool, i32, i32) {
        let rt = self.rt();
        let c = &rt.config.ui.candidate;
        (c.is_fixed_position(), c.custom_x, c.custom_y)
    }

    pub(crate) fn refresh_config_in_memory(&self, mutate: impl FnOnce(&mut Config)) {
        let mut cfg = self.rt().config.clone();
        mutate(&mut cfg);
        let keys = schema_key_union(&self.engine_mgr);
        let bundle = std::sync::Arc::new(ConfigBundle::build(cfg, &keys));
        *self.rt.write().unwrap_or_else(|e| e.into_inner()) = bundle;
        // 状态气泡去重缓存只在"内容配置不变"的前提下有效：改了 ui.status.items 之类后，
        // 同一状态该合成出不同文本，留着旧缓存会把改动后的第一次显示误判成"内容没变"而吞掉。
        self.last_status_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn reload_user_config(&self) -> bool {
        match Config::load(Config::data_dir().as_deref()) {
            Ok(cfg) => {
                // 方案相关项（活跃/可用方案、全局上屏策略）是否变化：变了才热重建引擎，
                // 避免每次保存都丢词典缓存（拼音合并/unigram 重建开销大）。
                let old = self.rt();
                // schema 段已含全局 codetable/pinyin/mix（上屏策略/调频等）；temp_pinyin 在 input 段，
                // 引擎按需缓存，故一并纳入脏判定。
                let schema_dirty = old.config.schema != cfg.schema
                    || old.config.input.temp_pinyin != cfg.input.temp_pinyin;
                // 候选窗定位方式切换的边沿检测（见下方 ReportCandidatePos）。
                let cand_was_fixed = old.config.ui.candidate.is_fixed_position();
                drop(old);

                let keys = schema_key_union(&self.engine_mgr);
                let bundle = std::sync::Arc::new(ConfigBundle::build(cfg, &keys));
                let new_cfg = bundle.config.clone();
                *self.rt.write().unwrap_or_else(|e| e.into_inner()) = bundle;
                info!("User config hot-reloaded (schema_dirty={})", schema_dirty);
                // 同 refresh_config_in_memory：设置页改了 ui.status.items 后，旧的去重缓存
                // 会把改动后的第一次状态显示误判成"内容没变"而吞掉。
                self.last_status_text
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                // 工具栏去重缓存同理：热重载可能改变工具栏的显隐策略，而 ToolbarState
                // 不带那些量，不清就会把该重推的那一次判成「内容没变」。
                self.reset_toolbar_push_dedup();
                // 注释词库跟随全局配置，**不在 schema_dirty 分支内**：`[[ui.comment_dicts]]`
                // 改动本身不会把 schema 标脏，放进那个分支等于「改了挂载列表没反应，
                // 直到下次切方案才生效」。自身按路径序列做变更检测，未变即空操作。
                self.sync_comment_dicts();
                // 语言栏图标的呈现参数同理跟随全局配置（`[ui.langbar]`），也不属 schema。
                // 少了这一步，改角标形状/配色要重启才生效——「改了没反应、重启就好」正是
                // 本仓反复出现的那类缺陷（运行时镜像态没回灌）。自带变更检测，未变即空操作。
                #[cfg(all(feature = "desktop-ui", windows))]
                self.apply_langbar_config();

                if schema_dirty {
                    // 热重建方案集：清输入缓冲、刷新工具栏/状态，免重启切换方案。
                    self.engine_mgr.reload_from_config(&new_cfg);
                    // 主码表可能变更：拆字库/字根字体随之切换（变更检测，未变不动）。
                    self.sync_chaizi_assets();
                    // 再同步一次注释库：上面那次用的是**重建前**的活跃方案，而重建可能换掉
                    // 它（方案被删除、默认方案变更）。方案专属库（`schemas`）因此要在这里
                    // 复核一遍——两次调用都有变更检测，未变的那次是空操作。
                    self.sync_comment_dicts();
                    {
                        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        s.input_buffer.clear();
                        s.candidates.clear();
                        s.preedit.clear();
                    }
                    self.notify_ui_hide();
                    self.push_state_update();
                    self.notify_toolbar(); // 方案名变化 → 刷新工具栏标签
                }
                // 同步主题选择:设置页改 config.ui.theme.* 后内存态须跟随,reload_config 才会下发新主题
                // (此前 reload_config 只重推旧内存主题 → 设置页切主题不生效)。
                {
                    let name = new_cfg.ui.theme.name.trim();
                    if !name.is_empty() {
                        *self.theme_name.lock().unwrap_or_else(|e| e.into_inner()) =
                            name.to_string();
                    }
                    *self.theme_style.lock().unwrap_or_else(|e| e.into_inner()) =
                        ThemeStyle::from_config(&new_cfg.ui.theme.style);
                }
                // 同步工具栏显隐:设置页改 ui.toolbar.visible 后运行时态跟随,再刷新工具栏。
                // 运行时镜像态回灌：这些开关运行时读 state（菜单/热键直改），config 是持久化
                // 真相源，两者只在启动时拷贝一次是不够的——设置页改了必须在此跟随，否则要重启
                // 服务才生效（症状：设置页改「检索范围」无效、而右键菜单正常）。
                let filter_changed = {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.toolbar_visible = new_cfg.ui.toolbar.visible;
                    s.s2t_enabled = new_cfg.input.s2t.enabled;
                    let new_mode =
                        wind_candidate::FilterMode::from_config(&new_cfg.input.filter_mode);
                    let changed = s.filter_mode != new_mode;
                    s.filter_mode = new_mode;
                    changed
                };
                // 检索范围变了且正在组合：以新范围重过滤刷新（与 set_filter_mode 一致，
                // 否则当前这屏候选要等下一次按键才更新）。
                if filter_changed {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !s.input_buffer.is_empty() {
                        self.update_candidates(&mut s);
                        self.notify_ui_update(&s);
                    }
                }
                self.apply_ui_config(); // 外观项（候选排列/编码显示/候选窗显隐）即时生效
                // 「定位方式」刚从跟随切到固定：若候选窗此刻正显示着，就地固定在它当前的位置，
                // 而不是跳到陈旧的 custom_x/custom_y（用户从没拖过时是 0,0，会窜到屏幕左上角）。
                // 窗口没显示则不上报，首显时由 UI 侧落到屏幕默认锚点。与 status_toggle_pinned 同构。
                if !cand_was_fixed && new_cfg.ui.candidate.is_fixed_position() {
                    let _ = self.ui_tx.send(UiCommand::ReportCandidatePos);
                }
                self.reload_config(); // 刷新主题/工具栏（候选窗下次输入按新配置）
                self.notify_toolbar(); // 工具栏显隐(visible/全屏)按新配置即时刷新
                self.sync_global_hotkeys(); // keys.global_hotkeys 增删/改键即时生效
                self.sync_direct_switch_hotkey(); // keys.activate_ime 改键/清空即时生效
                // capslock 绑定的增删即时生效：配上才装全局钩子，删掉立刻卸载。
                self.sync_capslock_hook();
                // 推送英文自动配对配置到 TSF 客户端（client_token=0 = 广播到所有活跃客户端）
                self.push_english_pair_config(0);
                self.push_jump_out_keys_config(0); // 配对跳出键同步（英文模式跳出 + 中文转发放行）
                self.push_password_suppress_config(0); // 密码框抑制策略（DLL 本地吃键门控）
                self.push_custom_en_punct_config(0); // 英半列自定义标点：DLL 据此吃键转发
                self.push_pair_state_ttl_config(0); // 配对状态时效（DLL 侧闸门据此判陈旧）
                // 诊断采集开关本身与配置文件无关（会话级），这里重推纯属幂等保险——
                // 与 password_suppress 同样处理，让"重载一次"能修好任何 DLL 侧状态漂移。
                self.push_diag_snapshot_config(0);
                self.show_toast(
                    "设置已更新",
                    ToastPosition::BottomCenter,
                    ToastKind::Success,
                );
                false
            }
            Err(e) => {
                tracing::error!("热重载配置失败: {}", e);
                self.show_toast(
                    "配置加载失败",
                    ToastPosition::BottomCenter,
                    ToastKind::Error,
                );
                true
            }
        }
    }

    /// 显示一次性通知 toast（约 2.5 秒后自动隐藏）。供配置热重载、词库就绪、错误等一次性事件。
    pub(crate) fn show_toast(&self, text: &str, position: ToastPosition, kind: ToastKind) {
        let _ = self.ui_tx.send(UiCommand::ShowToast {
            text: text.to_string(),
            position,
            kind,
            duration_ms: 2500,
        });
    }

    /// 服务重启后由新进程在就绪时弹一次「服务已重启」提示。
    ///
    /// 「重启服务」把旧进程连同其 UI 窗口线程一起销毁，退出前发 toast 用户看不到，
    /// 故反馈须由重启拉起的新进程接力（main 解析 `--restarted` 标志，service-ready 后调本方法）。
    /// Toast 由本进程 wind-ui 窗口渲染，不经 push 下发、不依赖 TSF 客户端重连，故就绪即可见。
    pub fn show_restart_toast(&self) {
        self.show_toast(
            "服务已重启",
            ToastPosition::BottomCenter,
            ToastKind::Success,
        );
    }

    /// 触发截图所有可见 UI 窗口，保存到用户配置目录下的 screenshots/ 子目录。
    pub(crate) fn trigger_screenshot(&self) {
        if let Some(dir) = wind_config::Config::user_config_dir() {
            let dir = dir.join("screenshots").display().to_string();
            let _ = self.ui_tx.send(UiCommand::TakeScreenshot { dir });
        }
    }

    /// 按当前配置（bundle）重新下发外观相关 UI 指令并同步运行时态。
    /// 热重载用：候选排列方向 / 编码显示方式 / 候选窗显隐 改动即时生效（无需重启）。
    /// 与命令栏 ime.toggle 共写同一组运行时 Mutex；以 config 为准重置（config 为持久化真相源）。
    pub(crate) fn apply_ui_config(&self) {
        let bundle = self.rt();
        let cand = &bundle.config.ui.candidate;
        // 候选排列方向（ui.candidate.layout == "vertical"）：config 是**基线**的持久化真相源，
        // 但实际下发要叠加当前模式的布局意图（见 layout.rs）——热重载不能把模式级覆盖清掉。
        // 此前这里无条件下发 config 值：模式进行中改任意一项设置都会静默取消强制竖排，
        // 且因为不留痕迹而极难复现。
        let vertical = cand.layout.eq_ignore_ascii_case("vertical");
        *self
            .candidate_vertical
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = vertical;
        {
            // 调用点（启动 / 配置重载）均不持 state 锁；加锁顺序 state → candidate_layout_sent
            // 与 notify_ui_update 一致，不构成环。
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.sync_candidate_layout(&state);
        }
        // 编码显示方式（ui.candidate.preedit_display）
        let mode = cand.preedit();
        *self
            .preedit_display
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = mode;
        let _ = self
            .ui_tx
            .send(UiCommand::SetPreeditEmbedded(mode.embedded()));
        // 候选窗显隐（ui.candidate.hide_window）
        let hidden = cand.hide_window;
        *self
            .hide_candidate_window
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = hidden;
        if hidden {
            self.clear_hover();
            let _ = self.ui_tx.send(UiCommand::HideCandidates);
        }
        // 候选字号覆盖（ui.candidate.font_size，0=跟随主题）；font_size_follow_theme=true 时强制跟随。
        let font_size = if cand.font_size_follow_theme {
            0.0
        } else {
            cand.font_size
        };
        let _ = self.ui_tx.send(UiCommand::SetCandidateFontSize(font_size));
        // 候选字体族（ui.font.family；空=默认）。
        let _ = self.ui_tx.send(UiCommand::SetCandidateFontFamily(
            bundle.config.ui.font.family.clone(),
        ));
        // 翻页栏 / 页码显示覆盖（ui.candidate.pager_bar_display / page_number_display）
        let _ = self
            .ui_tx
            .send(UiCommand::SetPagerDisplay(cand.pager_bar_display.clone()));
        let _ = self.ui_tx.send(UiCommand::SetPageNumberDisplay(
            cand.page_number_display.clone(),
        ));
        // 上方时反转候选顺序 / 交换编码候选栏 / 翻页栏并入编码栏
        let _ = self
            .ui_tx
            .send(UiCommand::SetCandidateFlipWhenAbove(cand.flip_when_above));
        let _ = self.ui_tx.send(UiCommand::SetCandidateSwapWhenAbove(
            cand.swap_preedit_when_above,
        ));
        let _ = self
            .ui_tx
            .send(UiCommand::SetPagerInPreedit(cand.pager_in_preedit));
        // 候选窗尺寸下限（ui.candidate.min_window_width_* / min_window_height_* / min_rows，抗抖动）
        let _ = self.ui_tx.send(UiCommand::SetCandidateMinSize {
            width_horizontal: cand.min_window_width_horizontal,
            width_vertical: cand.min_window_width_vertical,
            height_horizontal: cand.min_window_height_horizontal,
            height_vertical: cand.min_window_height_vertical,
            rows: cand.effective_min_rows(),
        });
        // 悬停提示延迟（ui.tooltip.delay）
        let _ = self
            .ui_tx
            .send(UiCommand::SetTooltipDelay(bundle.config.ui.tooltip.delay));
        // 工具栏自动隐藏（ui.toolbar.auto_hide / auto_hide_delay 秒→毫秒；下限 1 秒防误设 0 即隐）。
        // apply_ui_config 为启动(:717)与配置重载(:1270)共用单点，设置页改动即时生效。
        let tb = &bundle.config.ui.toolbar;
        let _ = self.ui_tx.send(UiCommand::SetToolbarAutoHide {
            enabled: tb.auto_hide,
            delay_ms: u64::from(tb.auto_hide_delay.max(1)) * 1000,
        });
        let _ = self.ui_tx.send(UiCommand::SetToolbarVertical(tb.vertical));
    }

    /// 当前活跃方案 ID（测试/诊断用）
    pub fn active_schema_id(&self) -> String {
        self.engine_mgr.active_schema_id()
    }

    /// 可选方案的 `(id, 显示名, 短称)`，**顺序即 [`MenuCmd::SchemaSelect`] 的下标**。
    ///
    /// 给自绘方案选择器的宿主用（Android）：桌面在协调器里直接构建菜单树，移动端的
    /// 选择器长什么样由宿主决定，只要拿到条目与下标即可。顺序必须原样透传——
    /// 宿主自行排序会让回送的下标指向另一个方案。
    pub fn schema_entries(&self) -> Vec<(String, String, String)> {
        self.engine_mgr
            .available_schemas()
            .into_iter()
            .map(|id| {
                let name = self.engine_mgr.schema_name(&id);
                let short = self.engine_mgr.schema_icon_label(&id);
                (id, name, short)
            })
            .collect()
    }

    /// 当前是否中文标点（测试/诊断用）
    pub fn is_chinese_punct(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .chinese_punct
    }

    /// 当前是否中文模式（测试/诊断用）
    pub fn is_chinese_mode(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .chinese_mode
    }

    /// 会话态按键绑定的统一执行（配置驱动，见 `keys.session_actions`）：翻页 / 移高亮 /
    /// 取消。普通模式与所有 overlay 模式共用；`include_printable` 区分码表型（`-`/`=` 作
    /// 翻页）与文本/表达式型（临英/快捷输入，`-`/`=` 作输入字符，不夺为动作）。
    ///
    /// 命中并执行返回 `Some`，未命中或条件不足返回 `None`（键回落调用方的原有处理）。
    ///
    /// # ★ 守卫按动作分，不按调用点分
    ///
    /// 导航类只在有候选时有意义（`requires_candidates`），`cancel` 则在「打了码还没出
    /// 候选」时也必须生效。判据挂在 `SessionAction` 上而不是写在这里的 `if`——本函数有
    /// 三个调用点（主输入 / mix / 候选导航），条件写死在函数体内还好，写到调用点上就是
    /// 三份要保持一致的守卫，那正是本仓栽过四次的形状。
    pub(crate) fn apply_session_action(
        &self,
        state: &mut State,
        data: &KeyEventData,
        include_printable: bool,
    ) -> Option<KeyAction> {
        let shift = data.modifiers & MOD_SHIFT != 0;
        let action = self.session_action_for(data.key_code, shift, include_printable)?;
        if action.requires_candidates() && state.candidates.is_empty() {
            return None;
        }
        let nav = match action {
            wind_config::SessionAction::HighlightUp => keymap::NavAction::HighlightUp,
            wind_config::SessionAction::HighlightDown => keymap::NavAction::HighlightDown,
            wind_config::SessionAction::PagePrev => keymap::NavAction::PagePrev,
            wind_config::SessionAction::PageNext => keymap::NavAction::PageNext,
            wind_config::SessionAction::Cancel => {
                // 无会话时放行：空闲按 Tab 该是宿主的制表符，不是「取消一个不存在的输入」。
                // 判据与 `cancel_session` 的适用范围一致，见那里。
                if !Self::has_input_session(state) {
                    return None;
                }
                return Some(self.cancel_session(state));
            }
            // 选词 / 以词定字**刻意不在这里执行**，返回 None 让键落到各自的既有消费点
            // （`select_char_index` 在本函数之前、`select_key_offset` 在数字选词臂之后）。
            //
            // ★ 理由是它们带 **overflow 语义**：候选不足 / 词长不够时要按
            // `keys.overflow.{select_key,select_char_key}` 分档处置（吞键 / 上屏高亮候选 /
            // 上屏并追加字符），而本函数只有「命中就执行」一种结局。搬进来就得把三档策略
            // 和各模式的选中出口一起搬，那是把两件事挤进一个函数。
            //
            // 收编改变的是**配置从哪来**（session_actions 而非 select_key_groups），
            // 不是执行路径——后者一行未动，故 overflow 与各模式的选中语义零回归。
            wind_config::SessionAction::SelectCandidate(_)
            | wind_config::SessionAction::SelectChar(_) => return None,
            // 辅助码：**不顶字**，原地筛当前候选（见 `enter_aux_code` / `commit_and_enter_bound_action`
            // 的同名分支）。门卫在 `enter_aux_code` 里（未开启 / 无码表 / 无候选
            // 都返回 None），此处不重复判断——两处各写一份判据必然漂移。
            //
            // 无候选时走不到这里：上面的 `requires_candidates()` 已经放行了按键，
            // 于是空闲按 Tab 仍是宿主的制表符。
            wind_config::SessionAction::AuxCode => {
                return self.enter_aux_code(state, crate::handle_aux_code::AuxCodeTrigger::Direct);
            }
            // 翻页 + 进辅助码的组合动词（单一 `session_actions` 写法，无需跨 `key_actions`）。
            // 正常态先翻到下一页、尚未进入辅助码态则进入（FromPage 保留刚翻到的页码）；
            // 已在辅助码态内则只翻页（底下 `handle_candidate_nav_or_auto_exit` 把本动词按
            // `page_next` 处理）。
            // 辅助码未开启 / 无码表时 `enter_aux_code` 返回 None，退化为纯翻页。
            wind_config::SessionAction::PageNextAuxCode => {
                self.page_next(state);
                if state.active.is_none() {
                    if let Some(act) =
                        self.enter_aux_code(state, crate::handle_aux_code::AuxCodeTrigger::FromPage)
                    {
                        return Some(act);
                    }
                }
                self.notify_ui_update(state);
                return Some(KeyAction::Consumed);
            }
            // 表里只存启用项（`ConfigBundle::build` 过滤过），None 到不了这里。
            wind_config::SessionAction::None => return None,
        };
        // 候选被反转排列时，高亮移动按**屏幕上看到的方向**走：竖排 + 上翻 + flip_when_above
        // 三者同时成立时，屏幕从上到下是候选 n..1，此时 ↑ 对应的是候选序的「下一个」。
        // 不区分按键（↑/↓ 与 Shift+Tab/Tab 一并翻转）——这两组都绑在同一对
        // `highlight_up`/`highlight_down` 上，行为分叉会让「同一个动作两种走向」。
        //
        // **翻页键不在此列**：页与页之间没有空间关系（新页在原处整体替换），反转只发生在页内。
        //
        // 回卷语义无需另写：反转后视觉最下方是页内第 0 项，按 ↓ 越界 == `move_up` 的
        // 「页首回卷到上一页末项」，两者本就是同一件事。
        let flipped = self
            .candidate_flipped
            .load(std::sync::atomic::Ordering::Relaxed);
        let changed = match nav {
            keymap::NavAction::HighlightUp if flipped => self.move_down(state),
            keymap::NavAction::HighlightDown if flipped => self.move_up(state),
            keymap::NavAction::HighlightUp => self.move_up(state),
            keymap::NavAction::HighlightDown => self.move_down(state),
            keymap::NavAction::PagePrev => self.page_prev(state),
            keymap::NavAction::PageNext => self.page_next(state),
        };
        if changed {
            // 混输高亮跟随：普通模式下高亮在五笔↔拼音候选间移动可能切换 preedit 形态
            // （原始码 ↔ 音节拆分）。重算 preedit；若形态变化且嵌入编码（app_inline），须回传
            // 组合串使宿主内联编码同步；候选窗模式仅 notify_ui_update 刷新即可。
            // 门控：仅普通模式（active==None）且存在拆分形态——纯五笔(无拆分)/纯拼音(全拼音
            // 候选→形态恒定)均不触发，零回归。
            let mut composed: Option<KeyAction> = None;
            if state.active.is_none() && !state.preedit_split_body.is_empty() {
                let before = state.preedit.clone();
                self.sync_preedit_to_highlight(state);
                if state.preedit != before {
                    let in_app = self
                        .preedit_display
                        .lock()
                        .map(|m| m.in_app())
                        .unwrap_or(true);
                    if in_app {
                        let text = state.preedit.clone();
                        let caret_pos = text.chars().count() as u32;
                        composed = Some(KeyAction::UpdateComposition { text, caret_pos });
                    }
                }
            }
            self.notify_ui_update(state);
            if let Some(act) = composed {
                return Some(act);
            }
        }
        Some(KeyAction::Consumed)
    }

    /// 当前是否有输入会话：正在 overlay 模式里，或普通输入有编码 / 候选 / 已上屏段。
    ///
    /// 与 C++ 的 `_HasInputSession()`（`hasComposition || _hasCandidates`）**语义对齐**：
    /// overlay 模式一定持有 composition。两侧判据必须同构，否则会出现「C++ 吃了键、
    /// 服务端这边判定无会话不接管」的丢键，或反过来「C++ 放行了、这边却想处理」。
    ///
    /// ⚠️ 不能只判 buffer 非空：overlay 模式在**空缓冲**时按取消键同样要退出模式——
    /// 那时「退出」本身就是用户要的动作。
    pub(crate) fn has_input_session(state: &State) -> bool {
        state.active.is_some()
            || !state.input_buffer.is_empty()
            || !state.candidates.is_empty()
            || !state.committed_text.is_empty()
    }

    /// 放弃当前输入会话：清掉未上屏内容，并退出所在的 overlay 模式。**Esc 的语义单点**。
    ///
    /// # 收敛了六处逐字重复的实现
    ///
    /// 主输入路径与五个 overlay handler 此前各写一份 Esc 分支，形态完全一致
    /// （`exit_X` + `notify_ui_hide` + `ClearComposition`），**差异只在退出函数**，
    /// 而那按 `state.active` 分派即可。散着的代价不是重复本身，是「回车五条路径」
    /// 那次的形状：任何一条新逻辑都只惠及主路径，其余五处静默落后。
    ///
    /// ⚠️ 菜单（`menu_open`）与快捷加词（`add_word_active`）**刻意不收**：它们是模态窗口，
    /// 菜单的键直接转发给 UI 窗口自行解释（`UiCommand::MenuKey`），协调器这边根本不决定
    /// 语义；加词模式则消费全部按键。要让自定义取消键在那两处也生效，得改 `wind-ui` 的
    /// 键解释器，是另一层的事。
    pub(crate) fn cancel_session(&self, state: &mut State) -> KeyAction {
        match state.active {
            Some(ModeKind::TempPinyin) => self.exit_temp_pinyin(state),
            Some(ModeKind::TempEnglish) => self.exit_temp_english(state),
            Some(ModeKind::Url) => self.exit_url_mode(state),
            Some(ModeKind::Special(_)) => self.exit_special_mode(state),
            Some(ModeKind::Mix(_)) => self.exit_mix_mode(state),
            // ★ 辅助码要**两步**：`exit_aux_code` 是本仓唯一一个「退出后主组合仍存活」的
            // 退出函数——它按设计还原拼音候选与 preedit（辅助码只是筛选，Esc 的语义是
            // 「放弃筛选、继续拼音」，见该函数注释）。而本函数末尾无条件
            // `notify_ui_hide` + `ClearComposition`，两者拼在一起就自相矛盾：协调器认为
            // 组合区里还有 `li`、候选窗里还有三条，宿主那边却收到了「清掉组合」。下一次
            // 敲 `a` 会让屏幕上凭空冒出 `lia`。
            //
            // 所以取消键在辅助码态要连主组合一起放弃 —— 用户按的是「放弃」，不是「退出
            // 筛选」；后者由 Esc 那一臂与触发键复按承担（都走 `aux_code_exited`，返回
            // UpdateComposition 而非 ClearComposition），两个动作从此语义分明。
            Some(ModeKind::AuxCode) => {
                self.exit_aux_code(state);
                self.reset_pinyin_composition(state);
            }
            // 普通输入：取消整个组合，含已转换前缀（拼音分步上屏的那部分）一并丢弃。
            None => self.reset_pinyin_composition(state),
        }
        self.notify_ui_hide();
        KeyAction::ClearComposition
    }

    /// keyup-only 键（CapsLock / 纯修饰键）上的会话态绑定（`keys.session_actions`）。
    ///
    /// 这批键**只有 keyup 到得了服务端**：C++ 对纯修饰键的 keydown 一律放行不吃（吃掉会让
    /// AutoCAD 看不到修饰键、正交模式覆盖失效），CapsLock 的 keydown 则压根不转发给服务端。
    /// 所以它们的绑定只能在这里消费——挂到 keydown 链上是配得上、永不触发。
    ///
    /// 一期只有导航类动词，[`Self::apply_session_action`] 自带「无候选返回 `None`」的守卫，正好
    /// 实现「有会话归绑定、无会话归原语义」：空闲时按 CapsLock 仍然切大小写。
    ///
    /// ⚠️ 二期加 `clear` / `cancel` 时，判据要放宽到「有编码**或**有候选」——那时改**这一处**
    /// 的守卫，别在各调用点各判一次（Esc 散成七处就是那么来的）。
    fn handle_session_action_key_up(&self, data: &KeyEventData) -> Option<KeyAction> {
        if !keymap::is_key_up_only_vk(data.key_code) {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // include_printable 取值在这里无关紧要——keyup-only 键没有一个是可打印的。
        // 传 true 与主输入路径保持一致，免得日后有人照抄这行时带走一个错误的先例。
        self.apply_session_action(&mut state, data, true)
    }

    /// 该字符此刻能否进输入缓冲：缓冲为空时查**首码集**，否则查码元**全集**。
    ///
    /// 首码判据取 `input_buffer.is_empty()`，而不是「无候选且无已提交」——码是按
    /// `input_buffer` 查询的，缓冲空就是新一轮码的开头；分步上屏后续打的第一个字符
    /// 同样算首码，与引擎的查询语义保持一致。
    ///
    /// 默认码元集（`a-z`）下，字母恒为真、其余恒为假，与历史逐键等价。
    pub(crate) fn can_enter_buffer(&self, state: &State, ch: char) -> bool {
        if state.input_buffer.is_empty() {
            self.engine_mgr.active_is_leading_char(ch)
        } else {
            self.engine_mgr.active_is_code_char(ch)
        }
    }

    /// 非码元字符的处置：终结当前组合并输出该字符。
    ///
    /// ⚠️ **刻意不透传**。C++ 在中文模式下对字母键是**无条件吃**的
    /// （`KeyEventSink.cpp` 的 `chinese_letter` 分支，仅 CapsLock 透传例外），
    /// 此处返回 `PassThrough` 就构成「吃了再吐」：不补发 `WM_KEYDOWN` 的宿主
    /// （EverEdit 一类）直接丢字符，全角态下还会出半角。故一律由本侧出字——
    /// 铁律是「C++ 吃键集 ⊆ Rust 出字集」，见 project_fullwidth_eat_flip。
    ///
    /// 空组合时同样走这条路：`commit_highlight_then_char` 在无候选无已提交时
    /// 只输出该字符（并按全角态转换），正是需要的行为。
    pub(crate) fn reject_non_code_char(&self, state: &mut State, ch: char) -> KeyAction {
        let has_comp = !state.input_buffer.is_empty()
            || !state.committed_text.is_empty()
            || !state.candidates.is_empty();
        self.commit_highlight_then_char(state, ch, has_comp)
    }

    /// 码元字符进缓冲的公共通路：插入 → 顶码上屏 → 候选刷新 → 组合区更新。
    ///
    /// 字母臂与非字母码元闸门（[`Self::try_code_char_gate`]）共用本函数——两条路进来的
    /// 只是「哪个键产出了这个字符」不同，进缓冲之后的处置完全一致。**不要复制这段**：
    /// 顶码的显示首选一致性、自动上屏的记账码分流都在这里，复制出去必然漂移。
    ///
    /// `ch` 是进缓冲的小写码元，`raw` 是进影子串的原始形态（Shift 大写等）。
    pub(crate) fn accumulate_code_char(&self, state: &mut State, ch: char, raw: char) -> KeyAction {
        // 顶码前记住「即将成为前缀」的缓冲及其显示首选：顶码上屏文本须与用户实际所见的
        // 首候选一致——调频置顶 / shadow 在协调器层重排（apply_freq_rerank/apply_shadow），
        // 引擎 handle_top_code 内部 convert 看不到，会顶出权重首选而非显示首选（对齐 Go
        // 复用 ConvertEx 取 Candidates[0] 的一致性修复）。顶码绝大多数发生在「满码+1」，
        // 此时前缀恰为顶码前缓冲，state.candidates 正是其显示候选。
        let pre_buf = state.input_buffer.clone();
        // 顶码上屏候选 = 用户实际所见的**显示首选**：取顶码前缓冲（即将成为前缀）的显示
        // 首候选——它已过智能过滤 / 词频重排 / shadow，正是用户所见。保留整条候选（含
        // is_command / phrase_template / group_code），供顶码分流：码表候选 & 普通短语 →
        // 文本顶上屏；$CC 命令短语 → 求值执行。短语 source 为 `Phrase`（**不参与**本
        // filter，放行靠 is_phrase / is_command 显式判定，与 source 取值无关）；拼音/英文
        // 候选（拼音本就排首，或智能过滤掉生僻码表字后仅剩拼音，如「wang」只有生僻字
        // 「佢」被过滤、显示全是拼音）仍被排除 → 下方放弃顶码继续组合
        // （对齐「上屏须与显示一致 + 非码表/短语类不上屏」）。
        //
        // ⚠️ 短语候选**必须再问一次短语层**：它们的 `code` 恒为空串，前缀命中与精确命中
        // 在候选上长得一模一样（`is_prefix` 只标 marker 导航，普通字面短语的前缀命中不打
        // 标记）。5 码短语 `zzsfz` 敲到 `zzsf` 时就已排在候选首位——不加这道判据，打
        // `zzsfa`（短语里没有这条码）会顶出 `zzsfz` 的内容，而正确行为是落进空码。
        let pre_display_first = state.candidates.first().cloned().filter(|c| {
            c.source == CandidateSource::CodeTable
                || ((c.is_phrase || c.is_command) && self.phrase_has_exact_code(&pre_buf))
        });
        // 在光标处插入（光标在末尾时等价于旧的 push）。后续顶码/候选刷新一律按整串
        // 缓冲判定，与光标位置无关——光标只是编辑位置，不参与引擎查询。
        preedit_cursor::BufEdit::new_cased(
            &mut state.input_buffer,
            &mut state.input_cursor_pos,
            &mut state.input_buffer_cased,
        )
        .insert_cased(ch, raw);

        // 顶码上屏：缓冲超过满码长且整串无匹配 → 顶前 N 码首选，余码续打
        // （schema.top_code_commit；置于候选刷新前，对齐 Go handleAlphaKey）。
        // 短语侧否决：整串已是精确码短语 / 还能续打成更长短语 → 不是「溢出」，放弃顶码
        // 继续组合（见 `phrase_vetoes_top_code`：引擎的两道闸只问码表，够不着短语层）。
        let top_code = self
            .engine_mgr
            .handle_top_code(&state.input_buffer)
            .filter(|_| !self.phrase_vetoes_top_code(&state.input_buffer))
            // 切点修正：引擎把 prefix 固定切在 `max_code_length`，而**短语码长不受方案满码长
            // 约束**（5 码短语 `zzsfz` 落在 4 码五笔里）。顶码前的缓冲若恰是一条精确码短语，
            // 就以短语码为切点。不修则 `zzsfza` 被切成 `zzsf` + `za`，与 `pre_buf` 对不上而
            // 落进「多级溢出」分支，又因 `zzsf` 在码表无字放弃顶码——表现为「进空码不顶码」。
            // pre_buf 长度恰为满码长时两种切法本就重合（`zzbd` 一类），行为不变。
            .map(|(engine_top, remainder)| {
                if self.phrase_has_exact_code(&pre_buf) {
                    let rem: String = state
                        .input_buffer
                        .chars()
                        .skip(pre_buf.chars().count())
                        .collect();
                    (engine_top, rem)
                } else {
                    (engine_top, remainder)
                }
            });
        if let Some((engine_top, remainder)) = top_code {
            let buf = state.input_buffer.clone();
            let prefix: String = buf[..buf.len().saturating_sub(remainder.len())].to_string();
            // 顶码候选决策：
            // - prefix==顶码前缓冲（满码+1，最常见）：用显示首选候选（码表/普通短语/命令）；
            //   显示首选非码表且非短语 → None → 放弃顶码（继续组合让用户选拼音）。
            // - 否则（多级溢出，罕见 wubi 场景）：回退引擎码表顶码纯文本（无命令语义）。
            if prefix == pre_buf {
                match pre_display_first {
                    // $CC 命令短语顶码：纯文本命令（≈词条）同步求值文本走标准文本顶码；
                    // 含副作用命令（开应用/切设置等）异步执行 + 余码走标准流程。
                    Some(cand) if cand.is_command => {
                        let input = if cand.group_code.is_empty() {
                            prefix.clone()
                        } else {
                            cand.group_code.clone()
                        };
                        return match self.eval_command_text_only(&cand.phrase_template, &input) {
                            // 求值文本与 `cand.text`（display 标签）无关，变体覆盖对它没有语义
                            // → None，走 `commit_top_text` 内的默认转换（对齐 `AutoCommit` 的
                            // 命令文本同样只过 `maybe_s2t`）。
                            Some(text) => self.commit_top_text(
                                state,
                                &prefix,
                                text,
                                None,
                                &remainder,
                                cand.source,
                            ),
                            None => self.top_commit_command_with_remainder(
                                state, &cand, &prefix, &remainder,
                            ),
                        };
                    }
                    // 码表候选 / 普通短语：文本顶上屏 + 余码续打。
                    Some(cand) => {
                        let source = cand.source;
                        let s2t_override = cand.s2t_override.clone();
                        return self.commit_top_text(
                            state,
                            &prefix,
                            cand.text,
                            s2t_override.as_deref(),
                            &remainder,
                            source,
                        );
                    }
                    // 显示首选是拼音/英文 → 放弃顶码，落到下方正常候选刷新继续组合。
                    None => {}
                }
            } else if !engine_top.is_empty() {
                // 多级溢出：引擎码表纯文本顶码（码表无字则 engine_top 空 → 放弃顶码，
                // 落到下方正常候选刷新继续组合）。此路来自引擎码表查询，确为码表来源。
                return self.commit_top_text(
                    state,
                    &prefix,
                    engine_top,
                    None, // 引擎码表纯文本，无候选对象可承载变体覆盖
                    &remainder,
                    CandidateSource::CodeTable,
                );
            }
        }

        // 全码自动上屏 / 满码空码清空（schema.auto_commit_at_full / clear_on_empty_max）。
        match self.update_candidates(state) {
            InputOutcome::AutoCommit(text) => {
                // 自动上屏文本取自首候选（handle_candidate.rs 构造 AutoCommit 时同源）。
                // 记账码同取首候选（按来源分流，见 `freq_code`），无候选时退回输入缓冲。
                let (source, code) = state
                    .candidates
                    .first()
                    .map(|c| (c.source, self.freq_code(&state.input_buffer, c)))
                    .unwrap_or_else(|| (CandidateSource::default(), state.input_buffer.clone()));
                let out = self.commit_candidate(state, &text, None, source, &code);
                self.notify_ui_hide();
                return Self::commit_action(out, true);
            }
            // 含副作用命令自动命中：与空格选中命令同路（清组合 + 异步执行）。
            InputOutcome::AutoCommand(cand) => {
                return self.commit_command(state, &cand);
            }
            InputOutcome::Clear => {
                state.input_buffer.clear();
                state.candidates.clear();
                self.notify_ui_hide();
                return KeyAction::ClearComposition;
            }
            InputOutcome::Normal => {}
        }
        let display = state.preedit.clone();
        let caret_pos = self.composition_caret(state);
        self.notify_ui_update(state);
        KeyAction::UpdateComposition {
            caret_pos,
            text: display,
        }
    }

    /// 非字母码元闸门：本方案把某个数字/符号配成了码元，且此刻允许它进缓冲 → 接管。
    ///
    /// 置于优先级链的「模式激活/URL 夺取之后、以词定字/翻页/大 match 之前」，于是组码中
    /// 的码元抢在选词键、翻页键、标点流水线之前——这正是「组码中码元优先」契约。
    /// 空缓冲时 `can_enter_buffer` 查的是**首码集**，数字默认不在其中 ⇒ 不接管，
    /// 数字键照常作选词/透传，用户不会失去「选第 1 个候选」与原生数字输入。
    ///
    /// 字母**不走这里**：它们在大 match 的字母臂处理，那里还有 z-fallback 等字母专属判定。
    ///
    /// ⚠️ 默认码元集 `a-z` 不含任何非字母字符 ⇒ 本闸门恒返回 `None`，与历史逐键等价。
    pub(crate) fn try_code_char_gate(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        // Ctrl/Alt 组合不是码元输入。上游已拦截，此处为纵深防御。
        if data.modifiers & MOD_SHORTCUT != 0 {
            return None;
        }
        let shift = data.modifiers & MOD_SHIFT != 0;
        let ch = printable_char(data.key_code, shift)?;
        if ch.is_ascii_alphabetic() {
            return None;
        }
        // 缓冲恒存小写（与字母同域）；`ch` 作为原始形态进影子串。
        let lower = ch.to_ascii_lowercase();
        if !self.can_enter_buffer(state, lower) {
            return None;
        }
        Some(self.accumulate_code_char(state, lower, ch))
    }

    /// 码元字符集与既有按键功能的冲突清单：`(字符, 占用它的功能名)`，空 = 无冲突。
    ///
    /// 「组码中码元优先」意味着配成码元的符号会从翻页/次选/以词定字/引导键手里被夺走。
    /// 这是方案作者的选择，不该阻止；但必须让他知道——否则现场表现是「翻页键忽然不灵了」，
    /// 而两处配置分开看都合理，无从查起。
    ///
    /// 判定**反查现有函数**而非重新解析配置：`page_keys` 一类存的是键组名（`minus_equal`），
    /// 自己再解析一遍必然与 `NavKeys::from_config` 漂移。此处对码元集里的每个非字母字符
    /// 找回它的 VK，再逐个问那些判定函数「这个键归你吗」——判据因此永远与实际行为同源。
    ///
    /// 只查非字母：字母本就是默认码元，且字母触发键（z）有专门的裁决顺序，不构成冲突。
    pub fn code_char_conflicts(&self) -> Vec<(char, Vec<&'static str>)> {
        let charset = self.engine_mgr.active_input_chars();
        if charset.is_default_alpha() {
            // 默认集只有字母，不可能与符号类功能冲突——顺带免掉一整轮反查。
            return Vec::new();
        }
        let mut out = Vec::new();
        for ch in charset.chars() {
            if ch.is_ascii_alphabetic() {
                continue;
            }
            let Some(vk) = char_to_main_vk(ch) else {
                continue;
            };
            let mut owners: Vec<&'static str> = Vec::new();

            // ── 组码中类占用：码元在组码中恒优先，故恒冲突 ──
            //
            // 数字选词是硬编码的 VK_1..=VK_9 / VK_0 臂，不经任何配置，故单独判。
            // 数字配成码元即等于放弃组码期间的数字选词（一刀切让位，见设计文档 §3.3）。
            if ch.is_ascii_digit() {
                owners.push("数字选词键");
            }
            // 会话态绑定：翻页/移高亮/取消都在组码期间抢这个键，故都算占用。
            // 措辞按实际动作分——设置页把这行原样显示给用户，笼统写「会话态按键」
            // 等于让用户自己去查是哪个功能占了。
            // ★ 走**当前方案**的语义表（含方案级 `[session_actions]`），不是跨方案并集：
            // 本函数比较的另一方是 `active_input_chars()`——活跃方案的码元集。两边必须同
            // 方案才谈得上冲突，拿并集去比会报出「别的方案里占了」这种当前根本不存在的冲突。
            // 可达性并集另有其人（`schema_session_vks`），别把两者混用。
            if let Some(a) = self.session_action_for(vk, false, true) {
                owners.push(match a {
                    wind_config::SessionAction::Cancel => "取消键",
                    wind_config::SessionAction::AuxCode => "辅助码键",
                    _ => "翻页/高亮键",
                });
            }
            if self.select_key_offset(vk).is_some() {
                owners.push("次选键");
            }
            if self.select_char_index(vk).is_some() {
                owners.push("以词定字键");
            }

            // ── 空缓冲类占用：模式引导键 ──
            //
            // ★ 只在该字符**可作首码**时才是真冲突。首码仲裁
            // （`code_char_takes_lead`）此时让引导键让位给码表 ⇒ 该模式再也进不去。
            // 不能作首码时两者井水不犯河水——模式只在空缓冲用、码元只在组码中用，
            // 报出来只会变成噪音，把真冲突淹掉。
            if charset.contains_leading(ch) {
                if self.match_special_trigger(vk).is_some() {
                    owners.push("特殊模式引导键");
                }
                if self.match_mix_trigger(vk).is_some() {
                    owners.push("快捷输入/混输引导键");
                }
                if self.is_temp_pinyin_trigger(vk) {
                    owners.push("临时拼音触发键");
                }
                if self.is_temp_english_trigger(vk) {
                    owners.push("临时英文触发键");
                }
            }
            if !owners.is_empty() {
                out.push((ch, owners));
            }
        }
        out
    }

    /// 启动时把 [`Self::code_char_conflicts`] 的结果写进日志。只告警，不改行为。
    pub(crate) fn warn_code_char_conflicts(&self) {
        let charset = self.engine_mgr.active_input_chars();
        for (ch, owners) in self.code_char_conflicts() {
            // 后果按「能否作首码」分档：首码意味着连空缓冲都归码表，被占用的模式引导键
            // 会彻底进不去；仅后续码则只影响组码期间。文案里直接给出化解办法，
            // 否则用户看到告警也不知道下一步该改哪。
            if charset.contains_leading(ch) {
                warn!(
                    "码元集含 {:?} 且允许其作首码，但该键原配作 {}；空缓冲时它将归码表，这些功能再也进不去。\
                     要两者共存：把它排除出 leading_chars（它便只在组码中作码元）",
                    ch,
                    owners.join(" / ")
                );
            } else {
                warn!(
                    "码元集含 {:?}（仅作后续码），该键同时配作 {}；组码中它归码表，这些功能在组码期间失效，空缓冲时不受影响",
                    ch,
                    owners.join(" / ")
                );
            }
        }
    }

    /// 普通模式「顶屏高亮候选 + 输出字符」：把已转换前缀与当前高亮候选一并上屏，再接该字符。
    /// 小键盘 direct 语义共用此路（编码型缓冲里数字不是合法编码，故终结当前组合而非入缓冲；
    /// 但**不丢弃**用户已打的码——顶屏它，对齐主键盘标点键的既有行为）。
    ///
    /// `has_comp` 由调用方在改动 state 前算好：空组合时无需隐藏候选窗。
    pub(crate) fn commit_highlight_then_char(
        &self,
        state: &mut State,
        ch: char,
        has_comp: bool,
    ) -> KeyAction {
        let committed = self.take_committed(state);
        let mut out = self.maybe_s2t(state, &committed);
        // ★ 联想态**不顶屏**。
        //
        // 顶屏的语义前提是「用户打了码、还没选词，按这个字符意味着『就选高亮那条吧』」。
        // 联想态**没有码**——高亮那条是输入法猜的，不是用户在选。此刻按「。」的意图
        // 就是打个句号，顶屏等于替用户做了个他没做的选择。
        //
        // 不修的后果真机上很刺眼（2026-08-16 反馈）：打「我」上屏、联想首条「我们」、
        // 按「。」得到「我我们。」——既顶了不该顶的，又用了整词而非该补的那半截「们」。
        // 两个错叠在一起，看起来像凭空多出一个字。
        if !state.candidates.is_empty() && !state.assoc_active() {
            let idx = self
                .highlighted_global_index(state)
                .min(state.candidates.len() - 1);
            let cand = state.candidates[idx].clone();
            // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
            let freq_code = self.freq_code(&state.input_buffer, &cand);
            self.record_selection(&freq_code, &cand.text, cand.source);
            out.push_str(&self.cand_s2t_text(state, &cand));
        }
        state.input_buffer.clear();
        state.candidates.clear();
        if has_comp {
            self.notify_ui_hide();
        }
        // 英文补空格（`schema.english.commit_space`）**刻意不接这里**：本函数的用途是
        // 「顶掉高亮候选 + 紧接着上屏这个字符」，补了会得到 `hello ,` 这种断开的标点。
        // 不是漏接。
        out.push_str(&if state.full_width {
            to_full_width(&ch.to_string())
        } else {
            ch.to_string()
        });
        Self::commit_action(out, state.chinese_mode)
    }

    // ───────────────────────── 临时拼音 ─────────────────────────

    // ───────────────────────── 快捷输入 ─────────────────────────

    // ───────────────────────── 临时英文 ─────────────────────────

    // ───────────────────────── 特殊模式 ─────────────────────────

    // ───────────────────────── 临时 mix 模式 ─────────────────────────

    /// 取出并清空「已转换前缀」（简体），用于非选词的终结性上屏（回车/空格上屏原码/标点键）。
    /// 码表模式恒为空串，无副作用。
    pub(crate) fn take_committed(&self, state: &mut State) -> String {
        state.committed_segs.clear();
        std::mem::take(&mut state.committed_text)
    }

    /// 清空拼音逐步转换的组合态（已转换前缀 + 缓冲 + 候选）。
    /// 把首显闸门要等的坐标预置成「已就绪」（**仅 crate 内单元测试用**）。
    ///
    /// headless 下没有宿主上报 caret，于是每一帧都是「首帧且坐标未就绪」，
    /// `notify_ui_update` 会在闸门处 return，候选压根不下发——任何断言「UI 收到了什么」
    /// 的测试都会拿到空通道，看着像功能没接上，其实是被闸门拦了。
    ///
    /// 放在本文件是因为它要写的两个字段是 `coordinator` 模块私有的；
    /// 兄弟模块（如 `handle_assoc`）的测试够不着，只能经这个入口。
    #[cfg(test)]
    pub(crate) fn debug_mark_coords_ready(&self) {
        *self
            .last_valid_caret
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (100, 200, 20);
        *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (100, 200, true);
    }

    pub(crate) fn reset_pinyin_composition(&self, state: &mut State) {
        state.committed_text.clear();
        state.committed_segs.clear();
        state.input_buffer.clear();
        state.input_buffer_cased.clear();
        state.input_cursor_pos = 0;
        state.preedit.clear();
        state.preedit_split_body.clear();
        state.preedit_fp_body.clear();
        state.preedit_abbrev_body.clear();
        state.shadow_code.clear();
        state.candidates.clear();
        self.reset_candidate_view(state);
    }

    /// cmdbar 能力 wrapper（被 handle_cmdbar 控制器经 Weak 回调）。各方法自锁，**禁止**在持
    /// state 锁时调用（spawn_command 已确保在独立线程、未持锁时执行）。
    /// 撤销最近一次上屏（cmdbar `ime.undo_commit`）：删除光标前 `last_commit_len` 个字符
    /// （UTF-16 单元），推 ReplaceBackward(N, "") 给活跃客户端（复用智能标点删除替换通道及其
    /// 全部宿主兼容修复）。计数语义见 [`Self::last_commit_len`]：默认 1 → 永远有动作；被最近
    /// 一次上屏覆盖 → 只精准删「刚输入完那次」；`swap(1)` 读取即复位 → 连续触发第二次起逐字删
    /// （数量不再可信，宁可少删多按几次，也不按陈旧计数误删多个）。
    ///
    /// v1 不校验光标前内容（用户主动触发；焦点变化/其它输入均已把计数刷回 1，故误删至多 1 个）；
    /// v2 预留 prevChar 比对。已知限制：SendInput 退格兜底宿主按「一次退格删一整字」处理时，
    /// emoji 会多删（兜底宿主 × emoji 双重边缘），留待后续按宿主特判。
    pub(crate) fn cmd_undo_commit(&self) {
        // 正在打字（缓冲非空）时不动作：ReplaceBackward 作用于已上屏文本，
        // 与组合态并存会把删除落进组合窗前的位置，语义混乱。
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.input_buffer.is_empty() {
                debug!("undo_commit: 输入缓冲非空，忽略");
                return;
            }
        }
        // 读取并复位为 1：撤销一次后计数即失效，下次 undo 退化删 1（除非其间又有新上屏刷新）。
        let count = self
            .last_commit_len
            .swap(1, std::sync::atomic::Ordering::Relaxed) as u32;
        if count == 0 {
            return;
        }
        debug!("undo_commit: 删除 {} 个 UTF-16 单元", count);
        let encoded = wind_ipc::codec::encode_replace_backward(count, "");
        let _ = self.push_server.push_commit_to_active(&encoded);
    }

    pub(crate) fn cmd_ime_toggle(&self, target: &str) {
        match target {
            "cn-en" => {
                self.handle_menu_command("toggle_mode");
            }
            "fullshape" => {
                self.handle_menu_command("toggle_width");
                // handle_menu_command 只 push_state_update，不刷工具栏；菜单路径由调用方
                // 补 notify_toolbar，命令栏路径同样需要补，否则工具栏全/半角状态不更新。
                self.notify_toolbar();
            }
            "s2t" => {
                self.handle_menu_command("toggle_s2t");
                self.notify_toolbar();
            }
            "toolbar" => self.toggle_toolbar(),
            "preedit" => self.cmd_toggle_preedit(),
            "candwin" => self.cmd_toggle_candwin(),
            "layout" => self.cmd_toggle_layout(),
            other => {
                warn!(
                    "ime.toggle: 暂不支持 target {:?}（Rust 平台能力待补）",
                    other
                )
            }
        }
    }

    /// 循环切换编码显示方式（内嵌应用 → 候选顶部 → 候选内联 → ...），下发 UI 并持久化。
    fn cmd_toggle_preedit(&self) {
        let mode = {
            let mut m = self
                .preedit_display
                .lock()
                .unwrap_or_else(|x| x.into_inner());
            *m = m.next();
            *m
        };
        // 候选窗内联标志（仅 candidate_inline 为 true）；in_app 由 notify_ui_update 读运行时态门控。
        let _ = self
            .ui_tx
            .send(UiCommand::SetPreeditEmbedded(mode.embedded()));
        // 持久化到用户层 ui.candidate.preedit_display（重启后保留）。
        if let Err(e) =
            Config::set_user_string(&["ui", "candidate", "preedit_display"], mode.as_config())
        {
            warn!("ime.toggle preedit: 持久化失败: {}", e);
        }
        self.show_tip(mode.label());
    }

    /// 切换候选窗显隐（运行时态）。隐藏时下次刷新即不显示候选。
    fn cmd_toggle_candwin(&self) {
        let hidden = {
            let mut h = self
                .hide_candidate_window
                .lock()
                .unwrap_or_else(|x| x.into_inner());
            *h = !*h;
            *h
        };
        if hidden {
            self.clear_hover();
            let _ = self.ui_tx.send(UiCommand::HideCandidates);
        }
        self.show_tip(if hidden {
            "候选窗:隐藏"
        } else {
            "候选窗:显示"
        });
    }

    /// 切换候选布局方向（横排 ↔ 竖排），下发 UI 并持久化。命令栏 ime.toggle("layout")。
    /// 切换时 composition 已清（命令选中即 ClearComposition），下次输入按新方向渲染。
    fn cmd_toggle_layout(&self) {
        let vertical = {
            let mut v = self
                .candidate_vertical
                .lock()
                .unwrap_or_else(|x| x.into_inner());
            *v = !*v;
            *v
        };
        // 翻转的是**基线**；实际下发仍要叠加当前模式意图（见 layout.rs），否则在强制竖排的
        // 模式里切换会绕过覆盖直接改方向，且去重缓存与真实下发值脱节。
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.sync_candidate_layout(&state);
        }
        // 持久化 ui.candidate.layout（重启后保留）。
        if let Err(e) = Config::set_user_string(
            &["ui", "candidate", "layout"],
            if vertical { "vertical" } else { "horizontal" },
        ) {
            warn!("ime.toggle layout: 持久化失败: {}", e);
        }
        self.show_tip(if vertical {
            "候选:竖排"
        } else {
            "候选:横排"
        });
    }

    /// 第 `i` 个候选（0 基）的序号标签，按「用户配置 > 主题 > 默认数字」裁决：
    /// ① 用户 `ui.candidate.index_labels` 显式设了该槽位 → 用之；
    /// ② 否则当前主题 `views.index.labels` 有非空槽位 → 用之；
    /// ③ 否则回退默认 (i+1)。
    fn resolve_index_label(
        &self,
        cand_cfg: &wind_config::config::UiCandidateConfig,
        i: usize,
    ) -> String {
        if let Some(s) = cand_cfg.user_index_label(i) {
            return s;
        }
        if let Some(s) = self
            .theme_index_labels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(i)
            .filter(|s| !s.is_empty())
        {
            return s.clone();
        }
        (i + 1).to_string()
    }

    pub(crate) fn notify_ui_update(&self, state: &State) {
        // CapsLock 钩子闸门：本函数是候选/编码状态变化后的必经出口，挂在这里覆盖面最大。
        // 放在最前面，使下方的 early return（无候选无编码 → 隐藏）也走得到。
        self.sync_capslock_gate(state);
        // 模式指示标记（拼/双/快/英/符）：仅在候选为空时显示（进入模式/无候选阶段），
        // 一旦有候选即隐藏，减少干扰。必须纳入下方"空则隐藏"守卫——否则进入模式时
        // 缓冲为空会直接隐藏，标记发不出。
        // 联想候选就住在 `candidates` 里，故「有候选即隐藏模式标记」这条**原样适用**
        // ——不必也不该为联想加判据。曾经加过一条 `|| assoc_active()`，那会让下面的
        // 「空则隐藏」守卫在联想态成立，候选窗直接被收掉：本该只影响标记的改动，
        // 顺手把整个窗关了。
        let mode_label = if state.candidates.is_empty() {
            self.mode_indicator_text(state).unwrap_or_default()
        } else {
            String::new()
        };
        if state.candidates.is_empty() && state.input_buffer.is_empty() && mode_label.is_empty() {
            self.clear_hover(); // 组合结束的最常见隐藏出口（不经 notify_ui_hide），须自行归零
            let _ = self.ui_tx.send(UiCommand::HideCandidates);
            self.reset_first_show();
            return;
        }
        // candwin 切换：用户隐藏候选窗时不显示（仍可盲打/自动上屏）。
        if *self
            .hide_candidate_window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            self.clear_hover();
            let _ = self.ui_tx.send(UiCommand::HideCandidates);
            self.reset_first_show();
            return;
        }
        // 模式级候选布局：按当前模式意图叠加全局基线重算方向，与上次下发不同才下发。
        // 必须在下方 UpdateCandidates **之前**——同 channel 按序处理，UI 先改方向再填候选。
        // 这是「强制竖排/横排」的唯一执行点，模式进入/退出各处都不再自己动布局（见 layout.rs）。
        self.sync_candidate_layout(state);
        // 延迟首次显示：新组合首帧若非经授权（reflow 后权威坐标 / 兜底 timer）则不立即显示，
        // 改 arm 兜底 timer，待 handle_caret_update 的权威坐标或超时再首显。避免在 reflow 前的
        // 陈旧坐标处先显示、reflow 后再跳（根治"上屏后立即输入候选窗错位约一个上屏宽度"）。
        // 例外①：仅显示模式标记（无候选/无编码）时跳过延迟——进入模式时缓冲为空、无刚上屏文字，
        // 光标无 reflow 跳动风险，强制延迟只会让状态提示迟钝。
        // 注：host-render 受限宿主**不**跳过首帧延迟——曾以「服务端直绘 SHM 无需等 reflow」
        // 为由直显，结果首帧用的是陈旧 caret（SearchHost 的 caret 事件在首键后才到），
        // 显示后再跳位（真机踩坑）。本机制自带兜底 timer，受限宿主 caret 事件缺席时
        // 也会超时首显，不存在「永不显示」风险。
        let only_mode_label =
            !mode_label.is_empty() && state.candidates.is_empty() && state.input_buffer.is_empty();
        let authorized = self
            .show_authorized
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        // 例外②③：两个「不必等」的逃生口。对齐 Go handle_key_action.go:207-209——本仓移植时
        // 只搬了「等」的一侧，漏了 Go 用来跳过等待的这两项，故此前比 Go 原版更保守：无论坐标
        // 是否已就绪、宿主是否光标稳定，新组合首帧一律压到 reflow 权威坐标才显示。实测代价是
        // 按键→候选窗恒定 85~95ms（其中 C++ OnLayoutChange 的 50ms debounce 占大头），连打时
        // 候选窗只来得及显示 2~29ms，表现为「迟钝」。
        //   ② skip_caret_pending：compat.toml 把该宿主标记为「光标稳定、无 reflow 漂移」，
        //      直接首显。连打场景**只有这一项能生效**——③ 依赖的组合起点会被
        //      reset_first_show() 在每次上屏时复位（Go 的 clearState 同样如此）。
        //   ③ 坐标已就绪：已有过有效 caret 且本轮组合起点已锁定 ⇒ 没有漂移可等。
        //      对应 Go 的 `!caretValid || !compositionStartValid` 取反。
        let skip_caret_pending = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first_show_mode
            == wind_config::app_compat::FirstShowMode::Instant;
        let coords_ready = self
            .last_valid_caret
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .2
            > 0
            && self
                .composition_start
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .2;
        let shown = *self
            .candidate_shown
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let is_first_frame = !authorized && !shown && !only_mode_label;
        // 自绘候选的宿主不等坐标：闸门的全部意义是等宿主 reflow 后的权威坐标，
        // 而它根本不用坐标。
        let caret_free = self
            .caret_independent
            .load(std::sync::atomic::Ordering::Relaxed);
        if is_first_frame && !caret_free && !skip_caret_pending && !coords_ready {
            // 唯一的「等」出口。与下面的放行日志成对，两条合起来即可从服务端日志判定
            // 每一帧走了哪条路、以及是哪个逃生口生效——不必再对着 TSF 日志比时间戳。
            debug!(
                "first_show 闸门 → 等待权威坐标（arm {}ms 兜底）: skip_caret_pending=0 coords_ready=0",
                self.planned_first_show_timeout_ms()
            );
            self.arm_pending_first_show();
            return;
        }
        if is_first_frame {
            // instant 档用的是上一轮遗留的坐标，必然是「非权威」；coords_ready 那条是已锁定
            // 的本轮组合起点，属权威，不置位。
            if skip_caret_pending {
                self.first_show_was_provisional
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            debug!(
                "first_show 闸门 → 立即显示（逃生口）: instant={} coords_ready={}",
                skip_caret_pending as u8, coords_ready as u8
            );
        }
        let t_nu = std::time::Instant::now();
        // 仅推送当前页候选（窗口按 1..N 编号，翻页后重新编号）
        let (start, end) = self.page_range(state);
        // 候选序号标签有**三种**归属，旧实现是个 bool 只装得下前两种：
        //  - 数字透镜：数字键正在录表达式 → 选词改用字母标签 a/b/c
        //  - 自由输入：字母与数字**都是**字面输入，没有任何键能按序号选 → 干脆不画序号
        //    （画了就是骗人——用户会去按那个数字，结果把数字打进缓冲）
        //  - 其余：正常序号
        let mix_lens = matches!(state.active, Some(ModeKind::Mix(_))).then(|| self.mix_lens(state));
        let alpha = mix_lens == Some(MixLens::Numeric);
        let hide_index = mix_lens == Some(MixLens::Free);
        // 悬停提示/候选微调配置（热重载快照）
        let rt = self.rt();
        let cand_cfg = &rt.config.ui.candidate;
        let tip_cfg = &rt.config.ui.tooltip;
        // 命令直通车候选前缀标注（features.cmdbar.candidate_prefix）：仅命令候选(is_command)显示。
        let cmd_prefix = rt.config.input.cmdbar.candidate_prefix.as_str();
        // 检索范围放宽（自动补充）候选的前缀标注，见 docs/design/smart-filter-scope-relax.md
        let scope_prefix = rt.config.input.scope_relax.prefix.as_str();
        // 编码提示(反查):对拼音来源候选,用主码表真实反查索引填 comment(实际填充见下方候选构造,
        // 受 source==Pinyin 守卫)。门控两类:
        //  - 普通拼音/混输方案:跟随方案 show_code_hint(pinyin_show_code_hint 解析,混输取次方案);
        //  - overlay 反查模式(临时拼音 / 快捷输入(mix)内拼音):**无视开关强制显示**
        //    (对齐 Go AddCodeHintsForced)——这些模式本身就是"用拼音反查码表编码",必须出码。
        // 码表类方案/候选的剩余编码由码表引擎在 convert 内填,不在此处理。
        let force_hint = matches!(
            state.active,
            Some(ModeKind::TempPinyin) | Some(ModeKind::Mix(_))
        );
        let pinyin_hint = force_hint || self.engine_mgr.pinyin_show_code_hint();
        let tip_opts = wind_reverse::TooltipOptions {
            code: tip_cfg.code_enabled,
            pinyin: tip_cfg.pinyin_enabled,
            heteronyms: tip_cfg.pinyin_heteronyms,
            max_readings: tip_cfg.pinyin_max_readings,
            chaizi: tip_cfg.chaizi_enabled,
        };
        // 调试提示上下文：仅开启调试段时解析一次（mixed 归属 / 方案 id），循环内按候选来源选用。
        let dbg_ctx = if tip_cfg.debug_enabled {
            // 归属与读写两端同源（`effective_data_schema`）：特殊模式下若这里仍按 active 解析，
            // 调试段显示的计数与排序实际用的不是同一个 key——排查时会被它带偏，
            // 而这正是最难察觉的一种不一致。
            Some(self.build_debug_schema_ctx(self.effective_data_schema(state).as_deref()))
        } else {
            None
        };
        // 反查表读锁在候选循环外取一次（写方仅 sync_chaizi_assets 的热重载路径）。
        let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
        // 注释段（候选右侧灰字）模板，见 `crate::comment`。横竖各持一份、互不影响：
        // 两种排布的可用横向空间差一个数量级，能放什么本就不是同一个答案。
        // 模式级覆盖优先于全局（临英可只显示 ${dict}、临拼可整个关掉），见 `comment::template_for`。
        let comment_tpl =
            self.comment_template_for(&rt.config, state, self.desired_vertical(state));
        // [编码] 段来源方案（循环外解析一次）：码表方案=自身全部编码（码长升序 a/ab/abc）、
        // 混输=其主码表成员、拼音=全局主码表。编码按词查方案词库反查索引（word_codes_in），
        // 不按取码规则生成。候选并非用该编码方案直接输入时（来源方案≠活跃方案，或处于
        // 临时拼音/快捷输入反查模式）标题带来源方案名：[编码(五笔)]。
        let code_schema = tip_cfg
            .code_enabled
            .then(|| self.engine_mgr.code_source_schema())
            .filter(|s| !s.is_empty());
        let code_source_name = code_schema.as_deref().and_then(|sid| {
            let indirect = force_hint || sid != self.engine_mgr.active_schema_id();
            indirect.then(|| {
                let name = self.engine_mgr.schema_name(sid);
                if name.is_empty() {
                    sid.to_string()
                } else {
                    name
                }
            })
        });
        let items: Vec<CandidateItem> = state.candidates[start..end]
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let full = self.cand_s2t_text(state, c);
                // 显示截断（超长加 …）：短语与普通候选统一按用户可配的 ui.candidate.max_chars。
                // 短语 text 在生成层已存完整原文（仅一行化），此处仅裁显示——上屏仍用完整原文。
                let disp = cand_cfg.truncate_display(&full);
                // 反查提示按截断后文本生成：超长候选（如长短语）逐字反查会撑爆气泡且显示不全，
                // 只提示实际显示出的字（… 为非 CJK，tooltip_for 自动滤除，不影响反查内容）。
                // [编码] 段按候选**完整原文**查词库（截断/繁化文本词库里没有；查不到=None 不显示）。
                let word_code = code_schema
                    .as_deref()
                    .map(|sid| self.engine_mgr.word_codes_in(sid, &c.text))
                    .filter(|s| !s.is_empty());
                let mut tooltip = reverse.tooltip_for(
                    &disp,
                    &tip_opts,
                    word_code.as_deref(),
                    code_source_name.as_deref(),
                );
                // 注释段（候选右侧灰字）：渲染当前排布对应的模板。
                // 与悬停提示无耦合——注释放不下的内容不往气泡里塞，气泡有自己的
                // `ui.tooltip.*` 三段（编码/拼音/拆字），塞了会与之重复。
                let comment = self.comment_for(
                    c,
                    comment_tpl,
                    cand_cfg.comment_max_chars,
                    &reverse,
                    pinyin_hint,
                );
                // 调试段：独立一行 [调试] + 来源/方案/编码/权重/序/词频。全关时不再兜底回填编码
                // （tooltip 各 provider 全关即真正为空，不显示气泡）。
                if let Some(ctx) = &dbg_ctx {
                    let dbg = self.debug_tooltip_section(c, &state.input_buffer, ctx);
                    if !tooltip.is_empty() {
                        tooltip.push('\n');
                    }
                    tooltip.push_str(&dbg);
                }
                CandidateItem {
                    // 命令候选加前缀标注（截断后再加,保证前缀不被截掉）。
                    // 检索范围放宽补进来的候选同理加标注（`input.scope_relax.prefix`），让用户
                    // 一眼看出「这几条是超出当前检索范围补来的」，而非词库里本该有的常用字。
                    text: if c.is_command && !cmd_prefix.is_empty() {
                        format!("{cmd_prefix}{disp}")
                    } else if c.is_scope_filtered && !scope_prefix.is_empty() {
                        format!("{scope_prefix}{disp}")
                    } else {
                        disp
                    },
                    code: c.code.clone(),
                    label: if alpha {
                        ((b'a' + i as u8) as char).to_string()
                    } else {
                        self.resolve_index_label(cand_cfg, i)
                    },
                    tooltip,
                    comment,
                    no_index: hide_index,
                }
            })
            .collect();
        // 翻页信息改为结构化字段传给候选窗（窗口内渲染独立的页码指示）
        let total_pages = self.total_pages(state);
        let selected = state.selected_index.min(items.len().saturating_sub(1));
        // 悬停目标独立于选中项：候选越界视为无悬停，翻页器 tag 原样透传
        let hover =
            match self.hover_target() {
                h if (0..wind_ui_types::HOVER_PAGE_PREV).contains(&h) => {
                    if (h as usize) < items.len() { h } else { -1 }
                }
                h => h, // 翻页器 tag / -1
            };
        // preedit 是否嵌入宿主（app_inline）：嵌入时编码插入宿主、光标随输入右移，候选窗须锚在
        // 组合起点（缓冲头部）而非跟随光标末尾；非嵌入时 preedit 在候选窗、宿主光标不动，用当前光标。
        // 该标志同时门控下方 preedit 是否下发候选窗渲染（嵌入时候选窗不重复显示 preedit）。
        // 联想态**不再强制非嵌入**：宿主侧此刻挂着占位组合（见 `ASSOC_COMPOSITION`），
        // 归属如实按配置走即可。嵌入模式下 `maybe_enter_assoc` 干脆不给标识
        // （`state.preedit` 为空），候选窗因此没有编码栏、高度不跳。
        let in_app = self
            .preedit_display
            .lock()
            .map(|m| m.in_app())
            .unwrap_or(true);
        // 坐标基准：嵌入模式且组合起点已锁定 → 用组合起点（钉在缓冲头部，不随输入移动）；否则当前光标。
        // 组合起点由 handle_caret_update 在本组合首个有效坐标处锁定。候选窗首显已由"延迟首显"门控
        // 保证发生在 reflow 后的权威坐标处。无效坐标回退最近有效坐标，避免跑到屏幕左上角。
        let cs = *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (cx, cy, ch) = if in_app && cs.2 {
            (cs.0, cs.1, state.caret_height)
        } else {
            (state.caret_x, state.caret_y, state.caret_height)
        };
        let (caret_x, caret_y, caret_height, caret_valid) = self.resolve_caret_for_ui(cx, cy, ch);
        let n_items = items.len();
        // 编码区**恒下发**：谁来画由 `preedit_host_owned` 表达。
        // 曾经是「in_app 就发空串」——那等于把渲染策略焊进数据通道，自绘编码栏的宿主
        // 拿不到数据（Android 侧一度只能靠改显示模式配置绕开）。
        let preedit = state.preedit.clone();
        let preedit_caret = self.ui_caret_bytes(state).min(preedit.len());
        let (cand_fixed, cand_fixed_x, cand_fixed_y) = self.candidate_fixed_pos();
        // mode_label 已在顶部计算（纳入空则隐藏守卫）：作为候选窗内联标记随候选窗一并显示。
        let _ = self.ui_tx.send(UiCommand::UpdateCandidates {
            preedit,
            preedit_caret,
            preedit_host_owned: in_app,
            mode_label,
            candidates: items,
            selected,
            hover,
            page: state.current_page + 1,
            total_pages,
            caret_x,
            caret_y,
            caret_height,
            caret_valid,
            fixed: cand_fixed,
            fixed_x: cand_fixed_x,
            fixed_y: cand_fixed_y,
        });
        // 候选窗已下发显示：标记本组合已首显，后续刷新（翻页/选字/打字）即可立即下发不再延迟。
        *self
            .candidate_shown
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = true;
        // macOS：把当前页候选右键菜单的禁用位随候选更新一并推给 `.app`，供其右键即时灰显。
        // Windows 的右键菜单在进程内 `show_candidate_menu` 实时算 enabled，不走此推送。
        #[cfg(target_os = "macos")]
        self.push_candidate_menu_flags(state, start, end);
        tracing::debug!(
            "notify_ui_update: build+send {:?} (n={})",
            t_nu.elapsed(),
            n_items
        );
    }

    /// macOS：计算当前页每候选的右键菜单禁用位并经 push 通道下发（CmdCandidateMenuFlags 0x0505）。
    /// 位定义与 Swift CandidatePanel 对齐：0x01 上移 / 0x02 下移 / 0x04 置顶 / 0x08 删除 / 0x10 恢复默认。
    /// 语义对齐进程内 `show_candidate_menu`（共用 candidate_delete_menu / candidate_op_scope 判定）：
    /// 首项禁上移/置顶、末项禁下移；拼音普通候选禁全部调位；删除按候选来源判定；
    /// 无 shadow 规则禁恢复默认；无词库落点整页全禁。
    /// 注：macOS 端「删除」文案固定，来源动态文案（禁用短语/删除用户词…）待协议扩展后接入。
    #[cfg(target_os = "macos")]
    pub(crate) fn push_candidate_menu_flags(&self, state: &State, start: usize, end: usize) {
        if !self.push_server.has_clients() || start >= end {
            return;
        }
        let total = state.candidates.len();
        // 无词库落点（无独立归属的 overlay / 空码浏览态）：整页全禁，只留复制——与 Windows 侧
        // `show_candidate_menu` 的「仅复制」分支同一判据（见 `candidate_op_scope`）。
        let Some(scope) = self.candidate_op_scope(state) else {
            let flags = vec![0x1Fu8; end.min(total).saturating_sub(start)];
            self.push_server
                .push_to_active(&wind_ipc::codec::encode_candidate_menu_flags(&flags));
            return;
        };
        let schema = scope.schema;
        let code = scope.code;
        let is_pinyin = matches!(scope.engine_type, Some(wind_engine::EngineType::Pinyin));
        let mut flags = Vec::with_capacity(end - start);
        for idx in start..end.min(total) {
            let cand = &state.candidates[idx];
            let word = &cand.text;
            let mut f = 0u8;
            if idx == 0 {
                f |= 0x01 | 0x04; // 首项：禁上移 + 禁置顶（已在首位，置顶是冗余规则）
            }
            if idx + 1 >= total {
                f |= 0x02; // 末项：禁下移
            }
            // 拼音普通候选：禁全部调位（无稳定位置语义）；命令候选例外。
            if is_pinyin && !cand.is_command {
                f |= 0x01 | 0x02 | 0x04;
            }
            let (_, deletable) = crate::handle_menu::candidate_delete_menu(cand);
            if !deletable {
                f |= 0x08;
            }
            let cand_id = (!cand.id.is_empty()).then_some(cand.id.as_str());
            if !self.shadow_has_rule(&schema, &code, word, cand_id) {
                f |= 0x10; // 无 shadow 规则：禁恢复默认
            }
            flags.push(f);
        }
        self.push_server
            .push_to_active(&wind_ipc::codec::encode_candidate_menu_flags(&flags));
    }

    pub(crate) fn notify_ui_hide(&self) {
        // 候选窗隐藏即会话终结：无条件收回 CapsLock 拦截。
        //
        // ★ 这里刻意**不查 state** 而是直接归零。闸门的两个方向后果不对称：少吃只是
        // 「CapsLock 绑定这一次没生效」，多吃却是「用户在别的应用里 CapsLock 按不动」。
        // 凡拿不准就归零。
        wind_keys::capslock_hook::set_should_eat(false);
        // 悬停归零同理：窗口没了，悬停目标不可能还有意义。UI 侧 `CandidateMouse::reset_hover`
        // 清的只是防抖闸门（决定何时**发**事件），高亮与 tooltip 读的是本值——不清这一句，
        // 特殊模式下窗口再次弹出时会带着上次的悬停高亮，鼠标却从未移动过。
        self.clear_hover();
        let _ = self.ui_tx.send(UiCommand::HideCandidates);
        self.reset_first_show();
    }

    // ———————————————— 鼠标交互（来自 UI 线程的反向事件）————————————————

    /// 注入渲染端反向事件（[`UiEvent`]）——headless/Android FFI 的公开入口。
    ///
    /// 桌面路径由 `new` 里 spawn 的事件线程消费 `Receiver<UiEvent>` 后调用同一分发；
    /// Android 的候选点击/翻页/菜单动作语义上就是 UiEvent，Kotlin 侧经 FFI 直调本方法，
    /// 不再另设通道（入方向无排队语义，与 `MessageHandler` 的方法直调先例一致）。
    ///
    /// 线程契约：可从任意**非协调器回调**线程调用（内部按需自行加锁/推送；
    /// 与桌面事件线程同款纪律，勿在持 state 锁的回调里重入）。
    pub fn inject_ui_event(&self, ev: UiEvent) {
        self.handle_ui_event(ev);
    }

    /// 分发 UI 鼠标事件（在专用线程中执行，可安全加锁/推送）
    pub(crate) fn handle_ui_event(&self, ev: UiEvent) {
        match ev {
            UiEvent::CandidateSelect(i) => self.mouse_select(i),
            UiEvent::Page(dir) => self.mouse_page(dir),
            UiEvent::Hover(i) => self.mouse_hover(i),
            UiEvent::Toolbar(a) => self.mouse_toolbar(a),
            UiEvent::ToolbarMoved { x, y } => self.save_toolbar_pos(x, y),
            UiEvent::CandidateOp { op, page_local } => self.candidate_op(op, page_local),
            UiEvent::RequestCandidateMenu { page_local, x, y } => {
                self.show_candidate_menu(page_local, x, y)
            }
            UiEvent::RequestMainMenu(anchor) => self.show_main_menu(anchor),
            UiEvent::MenuAction(kind) => self.menu_action(kind),
            UiEvent::MenuClose => {
                // ESC / 点击别处关闭：无动作派发，可直接解除 tooltip 隐藏抑制。
                self.menu_close();
                self.clear_tooltip_menu_flag();
            }
            UiEvent::GlobalHotkey(action) => self.handle_global_hotkey(&action),
            UiEvent::StatusTipMoved { x, y } => self.save_status_tip_pos(x, y),
            UiEvent::CandidateWindowMoved { x, y } => self.save_candidate_pos(x, y),
            UiEvent::RequestStatusMenu { x, y } => self.show_status_menu(x, y),
            UiEvent::RequestTooltipMenu { x, y } => self.show_tooltip_menu(x, y),
            UiEvent::RequestInputDiagMenu { x, y } => self.show_input_diag_menu(x, y),
            UiEvent::SystemThemeChanged => self.on_system_theme_changed(),
            UiEvent::CandidateFlipped(v) => self
                .candidate_flipped
                .store(v, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// 切换检索范围（0 智能/1 常用字/2 全部字符），以新范围重过滤并刷新候选。
    /// 持久化到 `config.input.filter_mode`（单一源：与设置页统一，reload 不会覆盖菜单选择）。
    pub(crate) fn set_filter_mode(&self, index: usize) {
        let (mode, label) = match FILTER_MODES.get(index) {
            Some(&(m, l)) => (m, l),
            None => return,
        };
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.filter_mode == mode {
                return;
            }
            s.filter_mode = mode;
        }
        if let Err(e) = Config::set_user_string(&["input", "filter_mode"], mode.as_config()) {
            warn!("set_filter_mode: 持久化 input.filter_mode 失败: {}", e);
        }
        self.refresh_config_in_memory(|c| c.input.filter_mode = mode.as_config().to_string());
        // 组合中：以新范围重建候选并刷新
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s.input_buffer.is_empty() {
            self.update_candidates(&mut s);
            self.notify_ui_update(&s);
        }
        drop(s);
        self.show_tip(label);
    }

    /// 持久化简繁开关到 `config.input.s2t.enabled`（单一源：与设置页统一，reload 不会覆盖
    /// 菜单/热键选择）。菜单与热键两条切换路径共用，避免只改一处留下不对称。
    pub(crate) fn persist_s2t_enabled(&self, on: bool) {
        if let Err(e) = Config::set_user_bool(&["input", "s2t", "enabled"], on) {
            warn!("toggle_s2t: 持久化 input.s2t.enabled 失败: {}", e);
        }
        self.refresh_config_in_memory(|c| c.input.s2t.enabled = on);
    }

    /// 影子规则：当前 code 是否对该候选有规则（置顶/删除），决定菜单"恢复默认"可用性。
    ///
    /// `cand_id` 取候选的稳定 id（短语候选非空）：动态短语的规则 `word` 记的是写入当天的
    /// 求值文本，只按 word 查会在次日恒判「无规则」——菜单「恢复默认」永久灰显，用户既改
    /// 不动也清不掉。判据与 `apply_shadow` / `candidate_op` 的写入端保持同一把键。
    pub(crate) fn shadow_has_rule(
        &self,
        schema: &str,
        code: &str,
        word: &str,
        cand_id: Option<&str>,
    ) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        // 折叠到 data_schema_id（与 apply_shadow/candidate_op 一致），拼音族共享。
        let schema = self.engine_mgr.data_schema_id(schema);
        matches!(
            store.get_shadow_rules(&schema, code),
            Ok(Some(rec)) if rec.has_target(word, cand_id)
        )
    }

    /// 当前焦点应用是否启用符号自动配对。per-app 规则（`compat.toml` 的 `auto_pair`）
    /// 优先，未配则跟随全局——全局开关仍在各自的 `input.auto_pair.chinese/english` 里，
    /// 本函数只回答「这个宿主要不要一刀切关掉」。
    ///
    /// ⚠ 三个消费点必须都问它：`active_pairs()`、`english_pairs_via_pipeline()`、
    /// `push_english_pair_config()`。前两条走协调器，第三条是 C++ 侧英文配对引擎——
    /// 纯英文模式的标点键根本到不了协调器，漏接它等于「切到英文就又配对了」。
    pub(crate) fn auto_pair_allowed_here(&self) -> bool {
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_pair
            .unwrap_or(true)
    }

    /// 当前模式下生效的配对表（按中/英标点 + 各自开关）
    pub(crate) fn active_pairs(&self, chinese_punct: bool) -> Option<Vec<(char, char)>> {
        // per-app 关闭：返回 None 等价于「配对表为空」，插对与右符号跳出一并失效。
        // 在取表这一层收口，而不是在每个使用点各加一个 if——后者是本仓栽过四次的形态。
        if !self.auto_pair_allowed_here() {
            return None;
        }
        let rt = self.rt();
        if chinese_punct {
            if rt.config.input.auto_pair.chinese {
                return Some(rt.cn_pairs.clone());
            }
        } else if rt.config.input.auto_pair.english {
            return Some(rt.en_pairs.clone());
        }
        None
    }

    /// 判断标点字符 `ch` 是否参与当前生效的自动配对（作为左符号或右符号）。
    /// 智能符号与自动配对互斥的判定依据（见 `smart_symbol_arm_str`）。
    pub(crate) fn is_auto_pair_char(&self, state: &State, ch: char) -> bool {
        match self.active_pairs(state.chinese_punct) {
            Some(pairs) => pairs.iter().any(|(l, r)| *l == ch || *r == ch),
            None => false,
        }
    }

    /// 滚轮翻页：dir<0 上一页，dir>0 下一页；仅重绘候选窗，不上屏。
    fn mouse_page(&self, dir: i32) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.candidates.is_empty() {
            return;
        }
        let changed = if dir < 0 {
            self.page_prev(&mut state)
        } else {
            self.page_next(&mut state)
        };
        if changed {
            self.notify_ui_update(&state);
        }
    }

    /// 悬停高亮：设置独立的悬停目标（候选或翻页器），不改键盘选中项，重绘。
    /// target<0 表示离开。空格上屏仍以 selected_index 为准。
    fn mouse_hover(&self, target: i32) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.candidates.is_empty() {
            // 候选已清空 → 悬停不再对应屏幕上任何东西，归零后返回（无候选时窗口本就不显示，
            // 不必重绘）。★ 必须**归零而非早退**：早退会让「鼠标移出候选窗」发出的那条
            // `Hover(-1)` 在候选恰好清空时被整个吞掉，旧值一路残留到下一次候选窗显示。
            self.clear_hover();
            return;
        }
        let new_hover = if target == wind_ui_types::HOVER_PAGE_PREV
            || target == wind_ui_types::HOVER_PAGE_NEXT
        {
            target // 翻页器悬停
        } else if target >= 0 {
            let (start, end) = self.page_range(&state);
            if (target as usize) < end - start {
                target
            } else {
                -1
            }
        } else {
            -1
        };
        if self
            .hover_index
            .swap(new_hover, std::sync::atomic::Ordering::Relaxed)
            != new_hover
        {
            self.notify_ui_update(&state);
        }
    }

    pub(crate) fn build_status(&self) -> StatusUpdateData {
        let (chinese_mode, full_width, chinese_punct, toolbar_visible, caps_lock) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.chinese_mode,
                s.full_width,
                s.chinese_punct,
                s.toolbar_visible,
                s.caps_lock,
            )
        };
        // 有效中文：中文模式且大写锁定未开（对齐 Go effectiveChinese = chineseMode && !capsLockOn）。
        let effective_chinese = chinese_mode && !caps_lock;
        let icon_label = if effective_chinese {
            let id = self.engine_mgr.active_schema_id();
            let lbl = self.engine_mgr.schema_icon_label(&id);
            if lbl.is_empty() {
                "中".to_string()
            } else {
                lbl
            }
        } else if caps_lock {
            "A".to_string()
        } else {
            "英".to_string()
        };
        StatusUpdateData {
            chinese_mode,
            full_width,
            chinese_punct,
            toolbar_visible,
            caps_lock,
            icon_label,
            key_down_hotkeys: self.rt().compiled_hotkeys.key_down_tsf_hashes(),
            key_up_hotkeys: self.rt().compiled_hotkeys.key_up_tsf_hashes(),
        }
    }

    /// 焦点事件携带的 caret 落缓存的**唯一入口**。
    ///
    /// 焦点 caret 有两条到达路径——同步段的 [`Self::handle_focus_gained_caret`] 与重型段的
    /// [`Self::handle_focus_gained`]——而**重型段必然晚于同步段执行**（见 `server.rs::handle_client`：
    /// 同步段先回 `ModePush` 解除 DLL 阻塞，重型段延后到响应写出之后才跑）。
    ///
    /// 此前重型段自己直写 `state.caret_*`，既没有 `height == 0` 守卫也不做 `caret_use_top`
    /// 变换，于是把同步段刚做好的两道处理**整个抹掉**：退化矩形进了缓存，微信一类宿主的
    /// 坐标差一个行高。两处口径分裂既不编译报错也不 panic，只表现为「焦点后第一次定位偏一行」，
    /// 是典型的看不见的分裂。故合并到此，两条路径都必须经由它。
    /// 应用 per-app 的光标坐标兼容变换：`caret_use_top` 抬升 + `caret_offset_*` 校正。
    ///
    /// ★ **两个调用点必须都走它**（`apply_focus_caret` 与 `handle_caret_update`）。
    /// `caret_use_top` 原本就是分头写在这两处的，任何新增变换只要漏一处，症状就是
    /// 「有时生效有时不生效」——取决于本次坐标是走焦点路径还是常规更新路径，极难归因。
    ///
    /// 偏移校正针对的是**宿主报告的坐标本身系统性偏移**（如 Windows Terminal，别家输入法
    /// 同样偏）。与主题里的候选窗偏移不是一回事：那个是候选窗相对光标的布局（样式层），
    /// 这个修的是光标坐标（兼容层），故候选窗/状态气泡/HUD 等所有消费者一并受益。
    ///
    /// `caret_offset_*` 以 dp（96dpi 基准逻辑像素）配置，而宿主上报的 caret 坐标是物理像素
    /// （屏幕坐标，DPI-aware 进程下即物理像素）——同一份 dp 配置在 100%/200% 缩放的显示器上
    /// 观感应一致，故须按**目标点所在显示器**的当前 DPI 换算成物理像素后再叠加，而不能直接
    /// 相加。多屏且缩放不同时，用哪块屏的 DPI 只能在换算时按坐标现查，缓存不得。
    ///
    /// 组合起点坐标同步平移以保持锚点一致；为 0（未提供）时不动，避免把「没有值」
    /// 变成「一个偏移后的假值」。
    fn apply_caret_compat(&self, data: &mut CaretData) {
        let (use_top, dx, dy) = {
            let ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
            (ac.caret_use_top, ac.caret_offset_x, ac.caret_offset_y)
        };
        if use_top && data.height > 0 {
            let raw_h = data.height;
            data.y -= raw_h;
            data.height = raw_h.max(CARET_USE_TOP_MIN_LINE_H);
            if data.composition_start_y != 0 {
                data.composition_start_y -= raw_h;
            }
        }
        if dx != 0 || dy != 0 {
            let scale = dpi_scale_for_point(data.x, data.y);
            apply_dp_offset(data, dx, dy, scale);
        }
    }

    fn apply_focus_caret(&self, data: &CaretData, via: &str) {
        // 独立日志行：与 handle_caret_update 区分开，否则无法从日志判断焦点坐标走的是哪条路
        // （2026-08-01 那轮修复第一版就因为看不出这点，白跑了一轮真机验证）。
        tracing::debug!(
            "{via} (no-show): x={} y={} h={} src={}",
            data.x,
            data.y,
            data.height,
            wind_ipc::protocol::caret_source::name(data.source)
        );
        // height==0 = 宿主尚未 reflow，GetTextExt 返回退化矩形，坐标不可信。
        if data.height == 0 {
            return;
        }
        let mut data = *data;
        self.apply_caret_compat(&mut data);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.caret_x = data.x;
        state.caret_y = data.y;
        state.caret_height = data.height;
        state.caret_source = data.source;
    }

    /// [`Self::resolve_caret_for_ui`] 的判据内核：这一组坐标本身是否可信（不看历史值）。
    ///
    /// ★ **负坐标是合法的**：主显示器左上角才是虚拟桌面原点，摆在主屏左侧/上方的显示器，
    /// 其坐标整块为负。把负数一并判为"异常"会让副屏用户的光标永远取不到有效坐标，
    /// 从而永远走回退分支——症状与本次要修的「气泡永远在主屏」一模一样。
    /// 上界 32000 只用于挡住 i32 溢出级的脏数据（宿主偶发上报的未初始化值）。
    fn caret_is_valid(x: i32, y: i32, height: i32) -> bool {
        height > 0 && !(x == 0 && y == 0) && x.abs() < 32000 && y.abs() < 32000
    }

    /// 解析「用于 UI 定位」的光标坐标：无效坐标回退到最近一次有效坐标。
    /// 返回 `(x, y, height, valid)`，`valid=false` 表示本进程至今没收到过任何可信坐标。
    ///
    /// ★ **候选窗与状态气泡必须共用本函数**。`state.caret_*` 里可以躺着 (0,0)：
    /// [`Self::handle_caret_update`] 是**先写缓存、后判 `now_valid`** 的（无效坐标写进去了才
    /// return），所以「读 `state.caret_*` 得到的坐标」与「可信坐标」并不等价。候选窗一直有这道
    /// 闸门、状态气泡没有，于是同一份 (0,0) 只让气泡飞到主显示器左上角 —— 多显示器下表现为
    /// 「气泡永远在主屏」，而候选窗一切正常，两者症状分裂正是这道闸门只装了一半造成的。
    ///
    /// (0,0) 当哨兵而非合法坐标：主显示器左上角虽是合法位置，但宿主「没有坐标」时报的也是它，
    /// 两者不可区分；判为无效只损失「光标恰在主屏左上角」这一像素级罕见情形。
    fn resolve_caret_for_ui(&self, cx: i32, cy: i32, ch: i32) -> (i32, i32, i32, bool) {
        let valid = Self::caret_is_valid(cx, cy, ch);
        let mut lv = self
            .last_valid_caret
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if valid {
            *lv = (cx, cy, ch);
            (cx, cy, ch, true)
        } else if lv.2 > 0 {
            (lv.0, lv.1, lv.2, true) // 回退到最近有效坐标，避免跑到屏幕左上角
        } else {
            (cx, cy, ch, false) // 尚无任何有效坐标：临时显示，待有效坐标到达再重定位
        }
    }

    /// 在当前光标下方显示状态提示气泡（中英/标点/全半角/方案切换）
    pub(crate) fn show_tip(&self, text: &str) {
        let bundle = self.rt();
        let si = &bundle.config.ui.status;
        // 禁用则完全不显示状态提示气泡。
        if !si.enabled {
            return;
        }
        // 空文本不弹窗：ui.status.items 全部取消勾选时合成文本为空，此前会渲染出一个
        // 什么都没有的小气泡（本地窗口路径无空文本判断，只有 host-render 的 render_frame 有）。
        // 与设置页「全部取消则不显示气泡」的说明保持一致。
        if text.trim().is_empty() {
            return;
        }
        // 先放锁再解析：resolve_caret_for_ui 要另取 last_valid_caret 锁，与候选窗路径
        // （state → last_valid_caret）保持同向，不构成反序。
        let (raw_x, raw_y, raw_h) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.caret_x, s.caret_y, s.caret_height)
        };
        let (x, y, caret_height, _valid) = self.resolve_caret_for_ui(raw_x, raw_y, raw_h);
        // 常驻(always)→ duration_ms=0(UI 不自动隐藏);否则按 duration 自动隐藏。对齐 Go display_mode。
        let duration_ms = if si.display_mode.eq_ignore_ascii_case("always") {
            0
        } else {
            si.duration.max(1) as u64
        };
        // 位置模式 fixed:用固定屏幕坐标 custom_x/custom_y;否则跟随光标(caret + offset)。
        let fixed = si.position_mode.eq_ignore_ascii_case("fixed");
        let _ = self.ui_tx.send(UiCommand::ShowStatusTip {
            text: text.to_string(),
            x,
            y,
            caret_height,
            offset_x: si.offset_x,
            offset_y: si.offset_y,
            duration_ms,
            fixed,
            fixed_x: si.custom_x,
            fixed_y: si.custom_y,
        });
        // 记录实际显示出去的文本，供 show_status 去重。临时提示（模式标记/主题名等）
        // 也记在这里：它们会覆盖掉旧的状态文本，从而使随后的同名状态气泡照常显示，
        // 不会被误判成"内容没变"。
        *self
            .last_status_text
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = text.to_string();
    }

    /// 隐藏状态提示气泡（常驻模式失焦时调用）。
    pub(crate) fn hide_tip(&self) {
        // 挂起中的焦点气泡一并作废：焦点都走了，那次挂起等来的权威坐标也已经属于别的上下文，
        // 补显示出来就是「切走之后气泡才姗姗弹出」。
        self.pending_focus_tip
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = self.ui_tx.send(UiCommand::HideStatusTip);
        // 清空去重缓存：否则重新获焦时"常驻显示"会因文本与隐藏前相同而不弹。
        self.last_status_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// 常驻(always)模式且启用时,显示当前合成状态(激活/获焦时调用)。temp 模式不在此显示。
    pub(crate) fn show_persistent_status_if_always(&self) {
        let si = &self.rt().config.ui.status;
        if si.enabled && si.display_mode.eq_ignore_ascii_case("always") {
            self.show_tip(&self.status_indicator_text());
        }
    }

    /// `ui.status.show_on_focus`：焦点切到新输入框时强制显示一次状态气泡。
    ///
    /// **不走 [`Self::show_status`]**：那条路会因「文本与上次相同」整个跳过，而焦点切换正是
    /// 「状态没变但仍要提示」的场景——走去重就等于这个开关在同状态下完全不生效。
    ///
    /// ## 坐标可信度闸门
    ///
    /// `follow_caret` 模式下只在坐标属 TSF 语义域时才显示。理由：`OnSetFocus` 不是按键上下文，
    /// 同步 edit session 必被宿主拒绝，回退链交出的是**跨窗口的** Win32 光标——Word 只在正文行
    /// 维护它，标题行上取到的是别处的陈旧值（实测偏差 814px）。用那种坐标弹气泡，正是用户
    /// 反馈的「还没输入时定位非常不准」。
    ///
    /// 拿不到就**不显示**，不做任何回退。下界 = 和没有这个功能一样好，不存在比原状更差的分支；
    /// 而弹在错误位置是实实在在的负价值。DLL 侧排队档会在 1~2ms 内补一条 TSF 坐标，
    /// 由 [`Self::handle_caret_update`] 消费本次挂起并补显示，故绝大多数宿主上并不会真的落空。
    ///
    /// `fixed` 模式不读 caret（用 custom_x/custom_y），故不受本闸门约束，一律直接显示。
    /// `client_token` 用于按**宿主**去重，见 [`Self::last_focus_tip_token`]：同一宿主内部换
    /// docMgr（Excel 单元格 ↔ 公式编辑栏）不该重复弹。
    pub(crate) fn show_focus_status_if_enabled(&self, client_token: u64) {
        let si = &self.rt().config.ui.status;
        if !si.enabled || !si.show_on_focus {
            return;
        }
        // 宿主去重。放在最前面：后面几条分支（fixed / TSF 闸门 / 挂起）都属于「这一次该怎么弹」，
        // 而这里回答的是**该不该弹**，语义在先。token=0 是旧 DLL 未携带的占位，不参与去重。
        if client_token != 0 {
            let mut last = self
                .last_focus_tip_token
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *last == client_token {
                debug!("focus_tip → 跳过: 同一宿主内换 docMgr（token={client_token:#x}）");
                return;
            }
            *last = client_token;
        }
        // always 模式已由 show_persistent_status_if_always 在同一处焦点回调里显示过，
        // 这里再来一次只会重复下发同一帧。
        if si.display_mode.eq_ignore_ascii_case("always") {
            return;
        }
        if si.position_mode.eq_ignore_ascii_case("fixed") {
            self.show_tip(&self.status_indicator_text());
            return;
        }
        let source = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.caret_source
        };
        if wind_ipc::protocol::caret_source::is_tsf(source) {
            self.pending_focus_tip
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.show_tip(&self.status_indicator_text());
        } else {
            // 挂起，等 DLL 补来的 TSF 坐标。挂起在下次焦点事件/失焦时作废，不设超时兜底——
            // 超时到期只能拿现有的不可信坐标显示，那正是本闸门要挡的东西。
            debug!(
                "focus_tip → 挂起: 坐标来源 {} 非 TSF 域，等待权威坐标",
                wind_ipc::protocol::caret_source::name(source)
            );
            self.pending_focus_tip
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// 合成当前 IME 核心状态文本：方案/中英(+大写) · 标点 · [全角] · [繁]。
    /// 默认态省略（半角/简体不显示），减少干扰；标点总显示（。/.）。
    pub(crate) fn status_indicator_text(&self) -> String {
        let (chinese, punct_cn, full, s2t, caps) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.chinese_mode,
                s.chinese_punct,
                s.full_width,
                s.s2t_enabled,
                s.caps_lock,
            )
        };
        // 内容段过滤：ui.status.items 未列出的段不参与拼接。空列表 = 全部显示
        // （既是未配置时的合理默认，也让无此键的旧配置行为不变）。
        let items = self.rt().config.ui.status.items.clone();
        let show = |k: &str| items.is_empty() || items.iter().any(|i| i == k);

        let mut parts: Vec<String> = Vec::new();
        // 方案 / 中英 / 大写锁定。三者共用首个槽位：关掉 caps 段时大写锁定不再顶替，
        // 落回正常的中英/方案显示。
        if caps && show("caps") {
            parts.push("A".into());
        } else if !show("schema") {
            // 方案段关闭：首槽整体略过（含英文态标记）
        } else if !chinese {
            parts.push("英".into());
        } else {
            let id = self.engine_mgr.active_schema_id();
            // short 样式优先图标短称(icon_label)，无则回退全名；对齐 Go schema_name_style。
            let short = self.rt().config.ui.status.schema_name_style == "short";
            let label = if short {
                let icon = self.engine_mgr.schema_icon_label(&id);
                if icon.is_empty() {
                    self.engine_mgr.schema_name(&id)
                } else {
                    icon
                }
            } else {
                self.engine_mgr.schema_name(&id)
            };
            parts.push(if label.is_empty() {
                "中".into()
            } else {
                label
            });
        }
        // 标点（本段启用时总显示）：英文模式（含大写锁定）下固定显示半角，
        // 不看内部 punct_cn 状态。
        if show("punct") {
            let effective_chinese = chinese && !caps;
            parts.push(if effective_chinese && punct_cn {
                "。".into()
            } else {
                ".".into()
            });
        }
        // 全角（仅全角时）
        if full && show("full_width") {
            parts.push("全".into());
        }
        // 繁（仅繁体时）
        if s2t && show("s2t") {
            parts.push("繁".into());
        }
        parts.join(" ")
    }

    /// 显示合成的核心状态气泡（中英/标点/全半角/简繁/方案切换共用）。
    ///
    /// 文本与上次显示的完全相同时**整个跳过**，不弹窗——用户通过 `ui.status.items`
    /// 关掉某段后，切换该状态不再改变气泡文本，弹一个和上次一模一样的气泡纯属噪声。
    pub(crate) fn show_status(&self) {
        let text = self.status_indicator_text();
        {
            let last = self
                .last_status_text
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *last == text {
                return;
            }
        }
        self.show_tip(&text);
    }

    /// 分发热键动作；返回是否已处理
    pub(crate) fn dispatch_hotkey(&self, action: &str) -> bool {
        match action {
            "toggle_mode" => {
                let (status, _) = self.handle_toggle_mode();
                status.is_some()
            }
            "switch_engine" => {
                self.cycle_schema();
                true
            }
            "toggle_full_width" => {
                {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.full_width = !s.full_width;
                }
                self.record_last_state();
                self.push_state_update();
                self.show_status();
                self.notify_toolbar();
                true
            }
            "toggle_punct" => {
                let effective_chinese = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.chinese_mode && !s.caps_lock
                };
                if effective_chinese {
                    {
                        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        s.chinese_punct = !s.chinese_punct;
                    }
                    self.record_last_state();
                    self.push_state_update();
                    self.show_status();
                    self.notify_toolbar();
                }
                true
            }
            "toggle_s2t" => {
                if self.s2t.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
                    self.show_toast(
                        "简繁数据缺失",
                        ToastPosition::BottomCenter,
                        ToastKind::Error,
                    );
                    return true;
                }
                let on = {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.s2t_enabled = !s.s2t_enabled;
                    s.s2t_enabled
                };
                self.persist_s2t_enabled(on);
                self.show_status();
                // 工具栏「繁」格随切即刷（对齐 toggle_full_width 与菜单路径）。缺这步时
                // 只有一闪而过的状态气泡，工具栏状态滞后到下次刷新事件，被误感知为“切换卡”。
                self.notify_toolbar();
                true
            }
            "toggle_toolbar" => {
                self.toggle_toolbar();
                true
            }
            "open_settings" => {
                self.open_settings(None);
                true
            }
            "take_screenshot" => {
                self.trigger_screenshot();
                true
            }
            // macOS 桌面专属：Windows 上该动作由 ctfmon 原生处理，本进程收不到、也不该处理；
            // headless（无 desktop-ui）落 `_` 臂 debug 忽略。
            #[cfg(all(feature = "desktop-ui", target_os = "macos"))]
            "activate_ime" => wind_ui::input_source_macos::select_self(),
            _ => {
                debug!("Unhandled hotkey action: {}", action);
                false
            }
        }
    }

    /// 全局热键触发（Win32 RegisterHotKey 的 WM_HOTKEY，UI 线程回送）：统一走 dispatch_hotkey。
    /// 此路径无 TSF 按键上下文，需要 composition 的动作（add_word）不参与全局注册
    /// （见 build_global_hotkey_entries），直接复用分发即可。
    fn handle_global_hotkey(&self, action: &str) {
        debug!("Global hotkey: {}", action);
        self.dispatch_hotkey(action);
    }

    /// 从 keys.global_hotkeys（动作名列表）构建全局热键条目（Win32 RegisterHotKey /
    /// macOS Carbon RegisterEventHotKey）。对齐 Go buildGlobalHotkeyEntries：仅支持无需
    /// 按键上下文的动作。
    ///
    /// activate_ime 是个例外，不读 keys.global_hotkeys：Windows 上它由 ctfmon 从
    /// DirectSwitchHotkeys 注册表直接接管（见 `sync_direct_switch_hotkey`），macOS 无对应
    /// 机制，改由本进程注册 Carbon 热键并调 TISSelectInputSource（见函数末尾的 macOS 分支）。
    fn build_global_hotkey_entries(&self) -> Vec<GlobalHotkeyEntry> {
        let rt = self.rt();
        let k = &rt.config.keys;
        let supported: [(&str, &str); 7] = [
            ("switch_engine", k.switch_engine.as_str()),
            ("toggle_full_width", k.toggle_full_width.as_str()),
            ("toggle_punct", k.toggle_punct.as_str()),
            ("toggle_toolbar", k.toggle_toolbar.as_str()),
            ("open_settings", k.open_settings.as_str()),
            ("take_screenshot", k.take_screenshot.as_str()),
            ("toggle_s2t", k.toggle_s2t.as_str()),
        ];
        let mut entries: Vec<GlobalHotkeyEntry> = Vec::new();
        for name in &k.global_hotkeys {
            let Some((_, value)) = supported.iter().find(|(n, _)| *n == name.as_str()) else {
                warn!("global_hotkeys: 不支持的动作 {:?}，忽略", name);
                continue;
            };
            let Some(hash) = hotkey::parse_hotkey(value) else {
                warn!("global_hotkeys: {} 的热键 {:?} 解析失败，忽略", name, value);
                continue;
            };
            // key_hash 布局 = (wind 修饰位 << 16) | vk（见 wind-config hotkey.rs）
            let (mods, vk) = (hash >> 16, hash & 0xFFFF);
            entries.push(GlobalHotkeyEntry {
                id: entries.len() as i32 + 1,
                modifiers: wind_mods_to_win32(mods),
                vk,
                action: name.clone(),
            });
        }
        // macOS：activate_ime 也走本进程的 Carbon 全局热键。
        //
        // 它**不**读 keys.global_hotkeys——那个列表是「哪些动作要额外提升为全局」的开关，
        // 而 activate_ime 的语义本来就只有全局一种（本输入法没激活时才需要它）。Windows 上
        // 它同样不在该列表里，是由 ctfmon 从注册表直接接管的；macOS 无对应机制，只能自己注册。
        // 判据因此是「配了就注册」，与 sync_direct_switch_hotkey 的 Windows 分支一致。
        #[cfg(target_os = "macos")]
        {
            let hotkey = self.rt().config.keys.activate_ime.trim().to_string();
            if !hotkey.is_empty() && !hotkey.eq_ignore_ascii_case("none") {
                match hotkey::parse_hotkey(&hotkey) {
                    Some(hash) => entries.push(GlobalHotkeyEntry {
                        id: entries.len() as i32 + 1,
                        modifiers: wind_mods_to_win32(hash >> 16),
                        vk: hash & 0xFFFF,
                        action: "activate_ime".to_string(),
                    }),
                    None => warn!("activate_ime 热键 {:?} 解析失败，忽略", hotkey),
                }
            }
        }
        entries
    }

    /// 配置里是否给 CapsLock 配了会话态绑定（决定要不要装全局钩子）。
    ///
    /// 判据取**编译后的绑定表**而非原始配置串：动词写错、键名写错的条目在 `ConfigBundle::build`
    /// 里已被剔除，那些情况不该装钩子（用户的配置根本不会生效，装了纯属白担全局钩子的风险）。
    /// ★ **方案级取并集，不取活跃方案那一份。** 钩子是进程级资源，且
    /// `SetWindowsHookExW` 重复装会留下卸不掉的旧钩子（见 `sync_capslock_hook`）——
    /// 按活跃方案取值就成了「方案 A 配了、方案 B 没配 ⇒ 每次切方案装卸一次」，
    /// 表现是「切完方案 CapsLock 时灵时不灵」。这与 C++ 转发表取并集是同一条判据
    /// （资源进程级 + 切换不幂等），只是它落在 Rust 侧。
    pub fn capslock_bound(&self) -> bool {
        let rt = self.rt();
        rt.session_keys
            .classify(keymap::VK_CAPITAL, false, true)
            .is_some()
            || rt.schema_session_vks.contains(&keymap::VK_CAPITAL)
    }

    /// 按配置装/卸 CapsLock 全局钩子（启动与配置热重载时调用）。
    ///
    /// ★ 幂等：已装且仍该装 → 不动（重复 `SetWindowsHookExW` 会留下卸不掉的旧钩子）。
    pub(crate) fn sync_capslock_hook(&self) {
        let want = self.capslock_bound();
        let mut slot = self.capslock_hook.lock().unwrap_or_else(|e| e.into_inner());
        if want == slot.is_some() {
            return;
        }
        if !want {
            // Drop 即卸载（内部会先停拦截再停消息泵）。
            *slot = None;
            wind_keys::capslock_hook::set_should_eat(false);
            info!("CapsLock 未配置会话态绑定 → 全局钩子已卸载");
            return;
        }
        // 钩子回调在钩子线程执行，**必须只做非阻塞投递**：它超时会被系统静默移除且无从察觉。
        // 故这里只 send，真正的动作在 new 起的消费线程里做（可安全加锁）。
        let tx = self.capslock_press_tx.clone();
        match wind_keys::capslock_hook::CapsLockHook::install(Box::new(move || {
            let _ = tx.send(());
        })) {
            Ok(h) => {
                *slot = Some(h);
                // 立刻按当前会话状态校准一次，避免装好后到下一次按键之间状态为默认值。
                let eat = {
                    let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    Self::has_input_session(&s)
                };
                wind_keys::capslock_hook::set_should_eat(eat);
            }
            Err(e) => {
                // 装不上就退化为「CapsLock 绑定不生效」，不影响其余功能。绝不回退到
                // 「翻转再回敲」——那条路已被真机否掉（竞态 + 厂商 OSD 弹窗）。
                tracing::error!("CapsLock 全局钩子安装失败，该绑定将不生效: {e}");
            }
        }
    }

    /// 同步「钩子此刻该不该吃 CapsLock」。
    ///
    /// ★★ 这个标志为 true 的时间窗必须尽量短。钩子是**全局**的：标志滞留意味着用户在
    /// **别的应用**里按 CapsLock 也切不动大小写——比功能不生效糟糕得多。故凡是会改变
    /// 「有没有输入会话」的出口都要调它，宁可多调（幂等的原子写，开销可忽略）。
    pub(crate) fn sync_capslock_gate(&self, state: &State) {
        // 未装钩子时也照常写：装钩子那一刻会重新校准，这里写了不会有副作用。
        wind_keys::capslock_hook::set_should_eat(Self::has_input_session(state));
    }

    /// 钩子报告「CapsLock 被按下」（在专用消费线程执行，可安全加锁）。
    ///
    /// 走的是与键盘路径**同一个** `apply_session_action`，故动词值域、守卫、各模式的翻页
    /// 出口都不会分叉。钩子只负责「这个键被按了」，「按了该干什么」仍归那一张表。
    fn handle_capslock_hook_press(&self) {
        let action = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            // 合成一个 keyup 事件：CapsLock 在键盘路径上本来就只有 keyup 到得了这里
            // （见 `handle_session_action_key_up`），保持同形以免两条路径的守卫产生差异。
            let data = KeyEventData {
                key_code: keymap::VK_CAPITAL,
                scan_code: 0,
                modifiers: 0,
                event_type: EVENT_KEY_UP,
                toggles: 0,
                event_seq: 0,
                prev_char: 0,
            };
            self.apply_session_action(&mut state, &data, true)
        };
        // 候选窗刷新已在 `apply_session_action` 内部完成（`notify_ui_update`），此处无须再推。
        // 返回值是给 TSF 的按键结果，而钩子路径**没有 TSF 按键上下文**可回传——与既有的
        // 全局热键路径（`handle_global_hotkey`）同一处境。
        //
        // ⚠️ 已知限制：`app_inline`（编码嵌入宿主）模式下，需要回写宿主内联串的结果
        // （`UpdateComposition` / `ClearComposition`）无法送达，宿主里的编码会滞留到下一次
        // 真实按键。翻页/高亮在候选窗模式下不受影响——那是本功能的主诉求。
        match action {
            Some(KeyAction::Consumed) | None => {}
            Some(_) => {
                debug!(
                    "CapsLock 钩子：该动作需回写宿主内联编码，钩子路径无法回传（app_inline 下会滞留）"
                );
            }
        }
    }

    /// 注册/刷新全局热键（启动与配置热重载时调用）。空列表也下发，用于清除旧注册。
    pub(crate) fn sync_global_hotkeys(&self) {
        let entries = self.build_global_hotkey_entries();
        debug!("sync_global_hotkeys: {} entries", entries.len());
        let _ = self.ui_tx.send(UiCommand::RegisterGlobalHotkeys(entries));
    }

    /// 同步 activate_ime 到 Windows DirectSwitchHotkeys 注册表（启动与配置热重载时调用）。
    /// 该热键由 ctfmon 原生处理（per-app 切换到本输入法），本进程不参与按键分发；
    /// 未配置/解析失败 → 仅清理注册表旧条目。
    ///
    /// 非 Windows 为空操作：macOS 的 activate_ime 走 `build_global_hotkey_entries` 里的
    /// Carbon 注册（切换是**全局**的，非 per-app——系统无对应 API，差异不可消除）。
    pub(crate) fn sync_direct_switch_hotkey(&self) {
        #[cfg(windows)]
        {
            let hotkey = self.rt().config.keys.activate_ime.trim().to_string();
            let entry = if hotkey.is_empty() || hotkey.eq_ignore_ascii_case("none") {
                None
            } else {
                match hotkey::parse_hotkey(&hotkey) {
                    // DirectSwitch Modifiers 低位与 Win32 RegisterHotKey 同位序（TF_MOD_*）
                    Some(hash) => Some((wind_mods_to_win32(hash >> 16), hash & 0xFFFF)),
                    None => {
                        warn!(
                            "activate_ime 热键 {:?} 解析失败，仅清理注册表旧条目",
                            hotkey
                        );
                        None
                    }
                }
            };
            crate::direct_switch::sync(&hotkey, entry);
        }
    }

    /// 放弃整段输入、上屏原码时该**归还**的引导符（不归还则为空串）。
    ///
    /// 三个同源出口共用：临拼回车 / mix 回车 / 切中英文（`take_input_on_mode_switch`）。
    /// 只改其中一处就会造成「回车带 z、切英文不带」这类不一致，故判据收在这里。
    ///
    /// # 为什么字母归还、符号不归还
    ///
    /// 符号引导键（`` ` ``、`;`）在码表里不产出编码，用户按它只可能是为了开模式；字母
    /// （z）在码表里是**合法编码字符**，按下时它既可能是开关也可能是码。放弃整段的语义正是
    /// 「别猜了，把我打的原样给我」，此时吞掉那个字母就是猜错了还不还。z-fallback 进来的
    /// 更是如此——那个 z 是从 `input_buffer` 里抢走的真实击键。
    ///
    /// # 为什么 committed_text 非空就不归还
    ///
    /// 用户已经在模式内选过词，说明他认可了这次进入，引导符归模式所有；再吐出来只会得到
    /// 「z你好ma」这种谁也不想要的东西。
    pub(crate) fn guide_to_return(prefix: &str, committed_text: &str) -> String {
        if committed_text.is_empty()
            && prefix
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        {
            prefix.to_string()
        } else {
            String::new()
        }
    }

    /// 切换中英文时取消当前输入：清空缓冲/候选/preedit，并按 `hotkeys.commit_on_switch`
    /// 决定是否把已输入的原始编码上屏（仅在切到英文且有待输入时）。返回待上屏文本。
    pub(crate) fn take_input_on_mode_switch(&self, state: &mut State, chinese: bool) -> String {
        // 切中英 / CapsLock / 系统切换三条路径全部经此。语境变了，上一句中文后面的联想
        // 已无意义。三个调用方随后都会 `notify_ui_hide`，故此处只清状态不动 UI。
        self.exit_assoc(state, crate::handle_assoc::AssocExit::ModeSwitch);
        // 独占模式的「模式切换上屏」策略：
        // - 临时英文：残留缓冲按模式切换语义无条件提交（英文原文，可全角）。
        // - 临时拼音 / mix（含快捷输入）：与下方普通组合一致，遵循 keys.commit_on_switch——
        //   切英文且有待输入且开关开时上屏「已转换前缀 committed_text + 剩余原码缓冲」，否则
        //   清空；触发键前缀（`/;）不输出，与各自回车上屏一致。
        // - 其余独占模式（网址）：丢弃。
        // 独占模式下 input_buffer 必为空，与下方普通组合分支互斥，故在此提前返回。
        if state.active.is_some() {
            let text = if state.active == Some(ModeKind::TempEnglish)
                && !state.temp_english_buffer.is_empty()
            {
                if state.full_width {
                    to_full_width(&state.temp_english_buffer)
                } else {
                    state.temp_english_buffer.clone()
                }
            } else if let Some((buf, prefix)) = match state.active {
                Some(ModeKind::TempPinyin) => Some((
                    state.temp_pinyin_buffer.clone(),
                    state.temp_pinyin_prefix.clone(),
                )),
                Some(ModeKind::Mix(_)) => {
                    Some((state.mix_buffer.clone(), state.mix_prefix.clone()))
                }
                // 辅助码是唯一**不清空 `input_buffer`** 的独占模式（它只筛候选，拼音码
                // 原封不动留在主缓冲里），故上面那句「独占模式下 input_buffer 必为空」
                // 对它不成立。取主缓冲、无引导前缀——语义与普通拼音态切英文完全一致，
                // 否则辅助码态下切英文会把待上屏的拼音原码静默丢掉。
                Some(ModeKind::AuxCode) => Some((
                    preedit_cursor::cased_or_buffer(&state.input_buffer, &state.input_buffer_cased)
                        .to_string(),
                    String::new(),
                )),
                _ => None,
            } {
                // 临拼 / mix：镜像普通组合的 commit_on_switch，且对齐各自的回车上屏语义。
                let has_pending = !buf.is_empty() || !state.committed_text.is_empty();
                if !chinese && self.rt().config.keys.commit_on_switch {
                    if has_pending {
                        // 有待输入：上屏「引导字母 + 已转换前缀 committed_text + 剩余原码」。
                        // 符号引导符不输出、字母引导符归还，判据见 `guide_to_return`
                        // ——与临拼/mix 的回车上屏共用同一条，三处必须同进同出。
                        // committed 段已在选词时记过，此处只记本次实际上屏的原码（来源模式切换）。
                        let guide = Self::guide_to_return(&prefix, &state.committed_text);
                        let code = format!("{}{}", guide, buf);
                        self.record_commit(&code, code.len() as u32, -1, CommitSource::ModeSwitch);
                        let raw = format!("{}{}{}", guide, state.committed_text, buf);
                        self.maybe_s2t(state, &raw)
                    } else if !prefix.is_empty() && !self.enter_clears_composition() {
                        // 只按了模式进入符（缓冲空）：原样上屏该前缀符号本身，与回车空缓冲上屏一致
                        // （enter_behavior=clear 时回车也不上屏，故一并放弃）。
                        self.record_commit(&prefix, 0, -1, CommitSource::Punctuation);
                        prefix
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            self.reset_exclusive_modes(state);
            self.notify_ui_hide();
            return text;
        }
        let has_pending = !state.input_buffer.is_empty() || !state.committed_text.is_empty();
        let commit = has_pending && !chinese && self.rt().config.keys.commit_on_switch;
        let text = if commit {
            // 切到英文且配置上屏：把「已转换前缀 + 剩余原码」一并上屏。
            let prefix = self.take_committed(state);
            // 上屏原码 → 同回车，用用户所打的大小写形态（缓冲本身恒小写）。
            let raw_code =
                preedit_cursor::cased_or_buffer(&state.input_buffer, &state.input_buffer_cased)
                    .to_string();
            // 模式切换上屏：committed 段已在选词时记过，此处只记剩余原码（来源模式切换）。
            self.record_commit(
                &raw_code,
                raw_code.len() as u32,
                -1,
                CommitSource::ModeSwitch,
            );
            self.maybe_s2t(state, &format!("{}{}", prefix, raw_code))
        } else {
            String::new()
        };
        state.committed_text.clear();
        state.committed_segs.clear();
        state.input_buffer.clear();
        state.input_buffer_cased.clear();
        state.candidates.clear();
        state.preedit.clear();
        text
    }
}

impl Coordinator {
    /// 从一次按键的最终 KeyAction 提取上屏文本，按中文/英文字符埋点到每日统计。
    /// 受 `features.stats.enabled` 控制；`track_english` 关闭时不计英文。无 store 静默跳过。
    /// 记录一次上屏事件到统计采集器。各上屏路径在已知码长/候选位/来源时调用，
    /// 并置位 stat_recorded，使顶层 record_input_stats 跳过兜底（避免重复计数）。
    /// 对齐 Go `recordCommit`：track_english 仅作用于 TSF 英文路径（Rust 暂无），
    /// 普通上屏按 4 分类记录全部字符。
    pub(crate) fn record_commit(
        &self,
        text: &str,
        code_len: u32,
        candidate_pos: i32,
        source: CommitSource,
    ) {
        if text.is_empty() {
            return;
        }
        let collector = match self.stat_collector.as_ref() {
            Some(c) => c,
            None => return,
        };
        if !self.rt().config.stats.enabled {
            return;
        }
        let (chinese, english, punct, other) = wind_store::stats::classify_chars_full(text);
        collector.record(StatEvent {
            timestamp: chrono::Local::now(),
            chinese,
            english,
            punct,
            other,
            code_len,
            candidate_pos,
            schema_id: self.active_schema_id(),
            source,
        });
        self.stat_recorded
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 顶层兜底统计（对齐 Go `recordCommitFallback`）：若本次按键已被具体上屏路径
    /// 记录则跳过；否则按文本推测来源（含非 ASCII→候选，纯 ASCII→标点）记录，
    /// 码长/候选位未知置 0/-1。
    pub(crate) fn record_input_stats(&self, action: &KeyAction) {
        if self
            .stat_recorded
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let text = match action {
            KeyAction::InsertText { text, .. } => text.as_str(),
            KeyAction::InsertTextWithCursor { text, .. } => text.as_str(),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let source = if !text.is_ascii() {
            CommitSource::Candidate
        } else {
            CommitSource::Punctuation
        };
        self.record_commit(text, 0, -1, source);
    }

    /// 从 store 重建短语层（短语类 RPC 改动后调用，使输入期即时生效）。
    pub(crate) fn rebuild_phrases(&self) {
        let recs: Vec<(String, String, i32, i32, bool)> = match self.store.as_ref() {
            Some(store) => store
                .enabled_phrases_for_input()
                .unwrap_or_default()
                .into_iter()
                .map(|p| (p.code, p.text, p.weight, p.position, p.is_system))
                .collect(),
            None => Vec::new(),
        };
        let mut g = self.phrases.write().unwrap_or_else(|e| {
            warn!("phrases 写锁中毒，恢复后重建");
            e.into_inner()
        });
        *g = wind_phrase::PhraseLayer::from_records(recs);
    }

    /// 当前有效的系统短语条目：重读 system.phrases.toml，为空则回退启动缓存。
    ///
    /// 重读使手工编辑 TOML 后无需重启服务。`parse_system_entries` 对"文件缺失"与
    /// "TOML 语法错误"同样返回空，二者不可区分，故重读为空时回退到启动缓存——
    /// 否则一个语法错误就会让调用方的 sync 把库里系统短语全部删除。
    pub(crate) fn current_system_phrase_entries(
        &self,
        reason: &str,
    ) -> Vec<wind_phrase::SystemPhraseEntry> {
        let reread = self
            .system_phrase_path
            .as_ref()
            .map(|p| wind_phrase::PhraseLayer::parse_system_entries(p))
            .unwrap_or_default();

        if reread.is_empty() {
            if self.system_phrase_path.is_some() {
                warn!(
                    "{reason}：重读 system.phrases.toml 为空（文件缺失或语法错误），沿用启动缓存"
                );
            }
            self.system_phrase_entries
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        } else {
            // 重读成功：刷新缓存，后续回退以最新文件内容为准。
            let mut g = self
                .system_phrase_entries
                .write()
                .unwrap_or_else(|e| e.into_inner());
            *g = reread.clone();
            reread
        }
    }

    /// 把**缺失**的系统短语条目补回库里（不动任何已存在的行）。
    ///
    /// 用户短语遮蔽同键系统条目时该行**归属用户**（`is_system=false`，见
    /// `Store::add_phrase`），于是任何「清空用户短语」的动作都会把它连同遮蔽关系一起删掉——
    /// 库里该 `(code,text)` 彻底消失，系统条目也随之不见。sync 平时只在 TOML 哈希变化或
    /// 「系统恢复默认」时才跑，不补这一次，被遮蔽过的系统短语要等到下次哈希变动才回来。
    ///
    /// **两个调用点**（漏一个就等于那条路上的系统短语静默丢失）：设置页「清空用户短语」、
    /// 备份还原的 replace 模式（`restore_backup` 内部会先 `reset_user_phrases`）。
    ///
    /// ⚠️ **走 `ensure_system_phrases` 而非 `sync_system_phrases`**：后者会用 TOML 值覆盖已存在
    /// 系统行的 weight/position，那样一次「清空用户短语」会顺带把用户在系统短语列表里改过的
    /// 权重重置掉——用户没要求这件事。补齐只应补缺失的。
    pub(crate) fn restore_missing_system_phrases(&self, reason: &str) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let entries = self.current_system_phrase_entries(reason);
        if entries.is_empty() {
            return;
        }
        let sys: Vec<wind_store::phrases::SystemPhrase> = entries
            .iter()
            .map(|e| wind_store::phrases::SystemPhrase {
                code: e.code.clone(),
                text: e.text.clone(),
                weight: e.weight,
                position: e.position,
            })
            .collect();
        match store.ensure_system_phrases(&sys) {
            Ok(n) if n > 0 => info!("{reason}：补回 {n} 条缺失的系统短语"),
            Err(e) => warn!("{reason}：系统短语补齐失败: {e}"),
            _ => {}
        }
    }

    /// 恢复默认系统短语：重读 system.phrases.toml → 强制同步入库 + 全部启用 + 重建输入层。
    pub(crate) fn restore_system_phrases(&self) -> usize {
        let Some(store) = self.store.as_ref() else {
            return 0;
        };

        let entries = self.current_system_phrase_entries("恢复默认");
        if entries.is_empty() {
            return 0;
        }

        let sys: Vec<wind_store::phrases::SystemPhrase> = entries
            .iter()
            .map(|e| wind_store::phrases::SystemPhrase {
                code: e.code.clone(),
                text: e.text.clone(),
                weight: e.weight,
                position: e.position,
            })
            .collect();
        // 先认领：历史上 `add_phrase`/wdict 导入撞键时会把系统行降级成用户行，此后
        // `sync_system_phrases` 的 `!cur.is_system → continue` 分支永远跳过它，该条目
        // 从「系统短语」列表里再也回不来。「恢复默认」是显式动作，在此把归属改回去。
        // 必须排在 sync 之前，认领后的行才能被 sync 刷新 weight/position。
        match store.reclaim_system_phrases(&sys) {
            Ok(n) if n > 0 => info!("恢复默认：认领回 {n} 条被降级的系统短语"),
            Err(e) => warn!("恢复默认：系统短语认领失败: {e}"),
            _ => {}
        }
        if let Err(e) = store.sync_system_phrases(&sys) {
            warn!("恢复默认：系统短语同步失败: {e}");
            return 0;
        }
        // 哈希随之更新，否则下次启动会因哈希不符再同步一次（无害但多余）。
        let _ = store.set_phrase_sys_hash(&phrase_entries_hash(&entries));

        let n = store.reset_system_enabled().unwrap_or(0);
        self.rebuild_phrases();
        entries.len().max(n)
    }
}

/// 对 SystemPhraseEntry 列表做稳定内容哈希（用于启动时判断 TOML 是否有变更）。
/// 使用标准库 DefaultHasher，无新依赖。
fn phrase_entries_hash(entries: &[wind_phrase::SystemPhraseEntry]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    entries.len().hash(&mut h);
    for e in entries {
        e.code.hash(&mut h);
        e.text.hash(&mut h);
        e.weight.hash(&mut h);
        e.position.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// 悬停调试段的方案归属上下文（每次候选推送解析一次，镜像 `apply_freq_rerank` 的方案解析）。
struct DebugSchemaCtx {
    /// 是否混输方案（英文/码表/拼音候选各归子方案）。
    is_mixed: bool,
    /// 非混输统一方案 id（拼音族折叠为 "pinyin"）。
    schema: String,
    /// 混输码表子方案 id（英文候选亦归此）。
    ct_id: Option<String>,
    /// 混输拼音子方案 id。
    py_id: Option<String>,
}

impl Coordinator {
    /// 解析调试归属上下文。`schema_override` = 生效方案（特殊模式），`None` 用 active。
    fn build_debug_schema_ctx(&self, schema_override: Option<&str>) -> DebugSchemaCtx {
        use wind_candidate::CandidateSource as S;
        let active = schema_override
            .map(str::to_string)
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        let is_mixed = self.engine_mgr.schema_engine_type(&active).as_deref() == Some("mixed");
        let schema = self.engine_mgr.data_schema_id(&active);
        let (ct_id, py_id) = if is_mixed {
            (
                self.engine_mgr.write_data_schema_id(&active, S::CodeTable),
                self.engine_mgr.write_data_schema_id(&active, S::Pinyin),
            )
        } else {
            (None, None)
        };
        DebugSchemaCtx {
            is_mixed,
            schema,
            ct_id,
            py_id,
        }
    }

    /// 候选归属的方案 id（混输按来源取子方案，非混输取统一方案）。
    fn debug_schema_id_for(&self, c: &Candidate, ctx: &DebugSchemaCtx) -> String {
        use wind_candidate::CandidateSource as S;
        if ctx.is_mixed {
            match c.source {
                S::CodeTable | S::English => ctx.ct_id.clone().unwrap_or_default(),
                S::Pinyin => ctx.py_id.clone().unwrap_or_default(),
                _ => ctx.schema.clone(),
            }
        } else {
            ctx.schema.clone()
        }
    }

    /// 候选来源标签：短语（系统/用户 + 组/成员）优先，其次用户/临时词库，再按来源 + 方案名。
    /// 混输下英文候选归码表体系。
    fn debug_source_label(&self, c: &Candidate, ctx: &DebugSchemaCtx) -> String {
        use wind_candidate::CandidateSource as S;
        if c.is_phrase {
            let kind = if c.meta.is_system_phrase {
                "系统短语"
            } else {
                "用户短语"
            };
            if c.is_group {
                return format!("{kind}·组");
            }
            if c.phrase_template.starts_with("$SS") || c.phrase_template.starts_with("$AA") {
                return format!("{kind}·成员");
            }
            return kind.to_string();
        }
        if c.meta.is_user_dict {
            return "用户词库".to_string();
        }
        if c.meta.is_temp_dict {
            return "临时词库".to_string();
        }
        match c.source {
            S::CodeTable => format!(
                "码表·{}",
                Self::schema_display_name(&self.debug_schema_id_for(c, ctx))
            ),
            S::English => "码表·英文".to_string(),
            S::Assoc => "联想".to_string(),
            S::Pinyin => {
                let sid = self.debug_schema_id_for(c, ctx);
                if sid.is_empty() || sid == "pinyin" {
                    "拼音".to_string()
                } else {
                    format!("拼音·{}", Self::schema_display_name(&sid))
                }
            }
            S::Phrase => "短语".to_string(),
            S::None => "系统词".to_string(),
        }
    }

    /// 候选词频使用次数（按候选归属方案点查 redb FREQ；无 store/无记录 → 0）。
    ///
    /// 查询码走 [`Self::freq_code`]（按来源分流：拼音/英文用候选存储码，码表用输入码）。
    ///
    /// 拼音侧不能用击键缓冲——双拼 `siyr`/分隔符 `xi'an`/前缀补全下与候选码不同域，用后者
    /// 查恒 miss、显示恒 0；码表侧反过来必须用输入码，否则 `d`/`de`/`def` 三个码位串扰。
    ///
    /// 与 `apply_freq_rerank` 及写入端 `record_selection` 同口径，**三处必须一致**——
    /// 本处不同步的后果最隐蔽：调试信息显示的计数与排序实际用的那条不是同一个 key，
    /// 排查时会被它带偏。
    fn debug_freq_count(&self, c: &Candidate, input_code: &str, ctx: &DebugSchemaCtx) -> u32 {
        let Some(store) = &self.store else {
            return 0;
        };
        let sid = self.debug_schema_id_for(c, ctx);
        let code = self.freq_code(input_code, c);
        if sid.is_empty() || code.is_empty() {
            return 0;
        }
        store
            .get_freq(&sid, &code, &c.text)
            .ok()
            .flatten()
            .map(|r| r.count)
            .unwrap_or(0)
    }

    /// 候选调试信息段：`[调试]` 独占一行 + 来源行 + 合并的（编码/权重/序/词频/标记）行。
    /// 保持约 3 行；来源区分系统/用户短语、用户/临时词库、码表(方案)、拼音、英文。
    fn debug_tooltip_section(
        &self,
        c: &Candidate,
        input_code: &str,
        ctx: &DebugSchemaCtx,
    ) -> String {
        let source = self.debug_source_label(c, ctx);
        let count = self.debug_freq_count(c, input_code, ctx);
        let mut parts: Vec<String> = Vec::new();
        if !c.code.is_empty() {
            parts.push(format!("码 {}", c.code));
        }
        parts.push(format!("权 {}", c.weight));
        parts.push(format!("序 {}", c.natural_order));
        parts.push(format!("用 {count}次"));
        if c.has_shadow {
            parts.push("✎已调整".to_string());
        }
        format!("[调试]\n来源: {source}\n{}", parts.join(" · "))
    }
}

#[cfg(test)]
mod mode_comment_e2e_tests {
    //! 模式级注释模板走到**发往 UI 的候选**上——决策函数 `comment::template_for` 的单元测试
    //! 证明不了消费端接上了它（本仓反复出现的「半接线」欠账）。
    //!
    //! 注释段在发送路径上算、不回写 `state.candidates`，故这里收 UI 通道断言。放在 crate 内
    //! 而非 tests/ 下，是因为要预置 caret 绕过首显闸门——headless 无宿主坐标，首帧会被
    //! `first_show` 闸门拦下不下发候选（见 `ready_coords_bypass_first_show_wait`）。
    use super::*;

    /// 造协调器并把坐标预置成「已就绪」，使候选能立即下发。
    fn coord_with_ui(cfg: Config) -> (Arc<Coordinator>, std::sync::mpsc::Receiver<UiCommand>) {
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
        *c.last_valid_caret.lock().unwrap() = (100, 200, 20);
        *c.composition_start.lock().unwrap() = (100, 200, true);
        (c, rx)
    }

    /// 直接驱动候选下发：造一条候选、进指定模式，然后走真实的 `notify_ui_update`。
    ///
    /// 候选带 `comment`（`${code_hint}` 的取值源）——模板**必须含至少一个非空变量**，
    /// 否则「变量全空则整个模板输出空串」的隐式可选段规则会让纯字面量模板恒渲染成空，
    /// 三个用例会一起拿到 `Some("")`，看起来像「模板没生效」其实是测试自己写错了。
    fn emit(c: &Arc<Coordinator>, active: Option<ModeKind>) {
        {
            let mut st = c.state.lock().unwrap();
            st.active = active;
            st.candidates = vec![wind_candidate::Candidate {
                text: "测".into(),
                comment: "码".into(),
                ..Default::default()
            }];
            st.input_buffer = "a".into();
        }
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }

    /// 取最近一条 `UpdateCandidates` 里首候选的注释段。
    fn last_comment(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Option<String> {
        let mut found = None;
        // 排空取**最后**一条：一次刷新会发多条 UI 命令，取第一条会拿到上一轮残留。
        while let Ok(cmd) = rx.try_recv() {
            if let UiCommand::UpdateCandidates { candidates, .. } = cmd {
                found = candidates.first().map(|c| c.comment.clone());
            }
        }
        found
    }

    fn cfg_with_templates() -> Config {
        let mut c = Config::default();
        // 用字面量而非变量，断言才不依赖词库内容
        c.ui.candidate.comment_template_vertical = "全局${code_hint}".into();
        c.ui.candidate.comment_template_horizontal = "全局${code_hint}".into();
        c
    }

    #[test]
    fn mode_override_reaches_ui() {
        let mut cfg = cfg_with_templates();
        cfg.input.temp_english.comment_template_vertical = Some("临英${code_hint}".into());
        cfg.input.temp_english.comment_template_horizontal = Some("临英${code_hint}".into());
        let (c, rx) = coord_with_ui(cfg);

        emit(&c, None);
        assert_eq!(
            last_comment(&rx),
            Some("全局码".to_string()),
            "无模式时取全局模板"
        );

        emit(&c, Some(ModeKind::TempEnglish));
        assert_eq!(
            last_comment(&rx),
            Some("临英码".to_string()),
            "临英期间必须改用模式级模板——只测 template_for 抓不到消费端没接线"
        );
    }

    /// ★ 空串 = 本模式不显示注释（与「跟随全局」是两回事），且这条语义要一路走到 UI。
    #[test]
    fn empty_override_hides_comment_at_ui() {
        let mut cfg = cfg_with_templates();
        cfg.input.temp_pinyin.comment_template_vertical = Some(String::new());
        cfg.input.temp_pinyin.comment_template_horizontal = Some(String::new());
        let (c, rx) = coord_with_ui(cfg);

        emit(&c, Some(ModeKind::TempPinyin));
        assert_eq!(
            last_comment(&rx),
            Some(String::new()),
            "空串必须让本模式不显示注释，而不是回落全局"
        );
    }

    /// 退出模式后自动回到全局模板——声明式重算的自愈性，无需任何「恢复」动作。
    #[test]
    fn leaving_mode_restores_global_template() {
        let mut cfg = cfg_with_templates();
        cfg.input.temp_english.comment_template_vertical = Some("临英${code_hint}".into());
        cfg.input.temp_english.comment_template_horizontal = Some("临英${code_hint}".into());
        let (c, rx) = coord_with_ui(cfg);

        emit(&c, Some(ModeKind::TempEnglish));
        assert_eq!(last_comment(&rx), Some("临英码".to_string()));
        emit(&c, None);
        assert_eq!(
            last_comment(&rx),
            Some("全局码".to_string()),
            "退出模式后应自动算回全局，不依赖任何显式恢复"
        );
    }
}

#[cfg(test)]
mod caret_compat_tests {
    //! caret_use_top 兼容变换：微信等 WebView 下把候选窗定位基准从 rect.bottom 改为 rect.top。
    use super::*;

    fn coord() -> Arc<Coordinator> {
        Coordinator::new_headless(Config::default(), None)
    }

    fn caret(y: i32, height: i32) -> CaretData {
        CaretData {
            x: 100,
            y,
            height,
            composition_start_x: 100,
            composition_start_y: y,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        }
    }

    /// 组合起点锚定的 500px 校验必须**只在 caret 与组合起点同源时**生效。
    fn far_comp_start(source: i32) -> CaretData {
        // 桌面输入实测形态：caret (0,1388) 是 GUI 回退取到的任务栏残留光标，
        // compStart (473,217) 才是真实组合位置，两者 dy=1171 ≥500px。
        CaretData {
            x: 0,
            y: 1388,
            height: 20,
            composition_start_x: 473,
            composition_start_y: 217,
            source,
        }
    }

    fn lock_comp_start_with(source: i32) -> bool {
        let c = coord();
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "ab".to_string(); // 置为组合中，否则 caret_update 只落缓存
        }
        c.handle_caret_update(&far_comp_start(source));
        c.composition_start.lock().unwrap().2
    }

    #[test]
    fn non_tsf_caret_skips_composition_start_distance_check() {
        // caret 来自 GUI 回退时，它与组合起点根本不是一个语义域，距离比较无意义。
        // 旧行为在此把**唯一正确**的组合起点当异常丢弃了（桌面输入定位到任务栏的直接原因）。
        assert!(
            lock_comp_start_with(wind_ipc::protocol::caret_source::GUI_CARET),
            "caret 为 GUI 回退源时应跳过距离校验、直接锁定组合起点"
        );
    }

    #[test]
    fn tsf_caret_still_rejects_far_composition_start() {
        // 反向对照：同样的距离，caret 若来自 TSF 域则 500px 保护仍须生效——同源却相差离谱，
        // 那才是它本来要抓的坐标系不一致。**缺了这条，上面那个测试无法区分「按来源放行」
        // 与「干脆不再校验」**，把保护删光也能让它变绿。
        assert!(
            !lock_comp_start_with(wind_ipc::protocol::caret_source::TSF_SELECTION),
            "TSF 同源时超 500px 仍应判为坐标系不一致而丢弃"
        );
    }

    #[test]
    fn caret_use_top_shifts_y_to_top_and_keeps_real_line_height() {
        let c = coord();
        // 模拟焦点进程命中 caret_use_top 规则。
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1234,
            caret_use_top: true,
            ..Default::default()
        };
        c.handle_caret_update(&caret(200, 20));
        let s = c.state.lock().unwrap();
        // bottom(200) → top：200 - 20 = 180（下方显示锚此稳定值）。
        assert_eq!(s.caret_y, 180);
        // 保留真实行高 20（> 下限）供上方显示避让正文，而非压成 1。
        assert_eq!(s.caret_height, 20);
    }

    #[test]
    fn caret_use_top_degenerate_height_floored_to_min() {
        let c = coord();
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1234,
            caret_use_top: true,
            ..Default::default()
        };
        // 退化帧 height=1：top 仍稳定（bottom-1），但行高落到下限避免上方遮挡。
        c.handle_caret_update(&caret(200, 1));
        let s = c.state.lock().unwrap();
        assert_eq!(s.caret_y, 199);
        assert_eq!(s.caret_height, CARET_USE_TOP_MIN_LINE_H);
    }

    /// 走一次 notify_ui_update 的首显闸门，返回「是否 arm 了等待」。
    /// 缓冲非空是必要前提，否则会先命中「空则隐藏」守卫、根本到不了闸门。
    fn armed_after_first_frame(c: &Arc<Coordinator>) -> bool {
        {
            let mut s = c.state.lock().unwrap();
            s.input_buffer = "a".to_string();
        }
        let s = c.state.lock().unwrap();
        c.notify_ui_update(&s);
        drop(s);
        *c.pending_first_show.lock().unwrap()
    }

    /// 造一个「正等首显、且已有上一轮权威坐标」的局面，返回喂入 probe 后是否仍在等待。
    fn still_waiting_after_probe(c: &Arc<Coordinator>, probe: CaretData) -> bool {
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".to_string();
        }
        *c.last_authoritative_caret.lock().unwrap() = (500, 300, true);
        // 与生产代码同源：`last_authoritative_caret` 置 true 和 `caret_cache_verified` 置 true
        // 是 `handle_caret_update` 里**同一行判据**下的两个动作，现实中不可能只有前者。
        // 二者不复用同一个字段，是因为清位不同——前者从不清（跨焦点仍为 true），后者在焦点
        // 到达/用户移动光标时清零。probe 判据需要的恰恰是后者（"基准可比"），拿前者判就会
        // 在焦点切换后把另一个单元格的坐标当基准，必然误判成"已 reflow"。
        c.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        *c.pending_first_show.lock().unwrap() = true;
        c.handle_caret_probe(&probe);
        *c.pending_first_show.lock().unwrap()
    }

    fn probe_at(x: i32, y: i32, height: i32) -> CaretData {
        CaretData {
            x,
            y,
            height,
            composition_start_x: x,
            composition_start_y: y,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        }
    }

    fn set_mode(c: &Arc<Coordinator>, mode: wind_config::app_compat::FirstShowMode) {
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1234,
            first_show_mode: mode,
            ..Default::default()
        };
    }

    /// fast 档的兜底必须远短于 wait 档：Word 这类宿主不发 OnLayoutChange、组合坐标 60~190ms
    /// 才到，而连打时组合只活 27~57ms，150ms 兜底永远等不到到期 ⇒ fast 退化成 wait、候选窗不显示。
    #[test]
    fn fast_mode_uses_short_first_show_fallback() {
        use wind_config::app_compat::FirstShowMode;
        let c = coord();
        set_mode(&c, FirstShowMode::Wait);
        assert_eq!(c.first_show_fallback_ms(), 150, "wait 档保持既有 150ms");
        set_mode(&c, FirstShowMode::Instant);
        assert_eq!(
            c.first_show_fallback_ms(),
            150,
            "instant 档走逃生口不 arm，取值无所谓但不应被 fast 的短值污染"
        );
        set_mode(&c, FirstShowMode::Fast);
        let cfg = c.rt().config.ui.candidate.fast_first_show_fallback_ms;
        assert_eq!(c.first_show_fallback_ms(), cfg);
        assert!(cfg < 150, "fast 档兜底必须短于 wait 档，否则本修复失效");
    }

    /// DLL 的「坐标待定」握手会把 wait 档延长到 600ms。fast 档必须拒绝这次延长，
    /// 否则短兜底当场作废、又变回干等。观察点取 token：arm 会 bump 它，early return 不会。
    #[test]
    fn caret_pending_does_not_extend_fast_mode_timeout() {
        use wind_config::app_compat::FirstShowMode;
        let c = coord();
        set_mode(&c, FirstShowMode::Fast);
        *c.pending_first_show.lock().unwrap() = true;
        let before = *c.pending_first_show_token.lock().unwrap();
        c.handle_caret_pending();
        assert_eq!(
            *c.pending_first_show_token.lock().unwrap(),
            before,
            "fast 档不得重 arm（token 未变即未重 arm）"
        );
    }

    /// 上一条的对照：wait 档必须照旧延长，证明那条不是被别的守卫挡住的。
    #[test]
    fn caret_pending_still_extends_wait_mode_timeout() {
        use wind_config::app_compat::FirstShowMode;
        let c = coord();
        set_mode(&c, FirstShowMode::Wait);
        *c.pending_first_show.lock().unwrap() = true;
        let before = *c.pending_first_show_token.lock().unwrap();
        c.handle_caret_pending();
        assert_ne!(
            *c.pending_first_show_token.lock().unwrap(),
            before,
            "wait 档应重 arm 到 600ms"
        );
    }

    // ── ui.status.show_on_focus：焦点气泡与坐标可信度闸门 ────────────────────────────

    /// 造一个开了 `show_on_focus` 的协调器，并**保留 UI 通道接收端**——「气泡有没有真的发出去」
    /// 只能从 `ui_tx` 上观察。用 debug 方法「按同样规则再算一遍」是假测试：决策函数写对但
    /// 生产路径没接上时，那种测试照样全绿。
    fn coord_focus_tip(
        show_on_focus: bool,
        position_mode: &str,
    ) -> (Arc<Coordinator>, std::sync::mpsc::Receiver<UiCommand>) {
        let mut cfg = Config::default();
        cfg.ui.status.enabled = true;
        cfg.ui.status.show_on_focus = show_on_focus;
        cfg.ui.status.display_mode = "temp".to_string();
        cfg.ui.status.position_mode = position_mode.to_string();
        Coordinator::new_headless_with_ui(cfg, None)
    }

    /// 通道里是否收到了「显示状态气泡」指令。
    fn got_status_tip(rx: &std::sync::mpsc::Receiver<UiCommand>) -> bool {
        rx.try_iter()
            .any(|c| matches!(c, UiCommand::ShowStatusTip { .. }))
    }

    /// 把坐标缓存设成指定来源。
    fn set_caret(c: &Arc<Coordinator>, x: i32, y: i32, source: i32) {
        let mut st = c.state.lock().unwrap();
        st.caret_x = x;
        st.caret_y = y;
        st.caret_height = 25;
        st.caret_source = source;
    }

    /// 两个不同宿主的 client_token。用具名常量而非字面量，是因为下面「同宿主不重复弹」
    /// 那组用例的全部含义就在于**这两个值相不相等**，字面量会让它退化成看不出意图的魔数。
    const TOKEN_A: u64 = 0x1111_0000_0001;
    const TOKEN_B: u64 = 0x2222_0000_0001;

    /// 同一宿主内换 docMgr（Excel 单元格 ↔ 公式编辑栏）不得重复弹气泡。
    /// 这是「输入一次闪两下」的直接成因——闪的时机与用户的操作节奏对不上。
    #[test]
    fn focus_tip_skips_same_host_docmgr_switch() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(
            &c,
            100,
            200,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(got_status_tip(&rx), "首次进入该宿主应弹一次");

        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(
            !got_status_tip(&rx),
            "同一 token = 同一宿主内换 docMgr，不得重复弹"
        );
    }

    /// 反向对照：换了宿主必须照弹。
    /// **缺了这条，上一条用「弹过一次就再也不弹」的实现也能通过**——那会让切换应用时
    /// 气泡彻底消失，比重复弹更糟。
    #[test]
    fn focus_tip_shows_again_for_different_host() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(
            &c,
            100,
            200,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(got_status_tip(&rx));

        c.show_focus_status_if_enabled(TOKEN_B);
        assert!(got_status_tip(&rx), "换宿主必须重新提示一次");
    }

    /// 离开宿主（Thread 级失焦）后再回来，应当重新提示。
    /// ⚠ 只有 Thread 档清去重记录：CtxLost/DocChanged 是宿主内换 docMgr 的噪声，
    /// 若也清就等于按 docMgr 计数，Excel 下会退回「输入一次闪两下」。
    #[test]
    fn focus_tip_resets_after_leaving_host() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(
            &c,
            100,
            200,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(got_status_tip(&rx));

        // docMgr 级失焦：不清记录，回来仍不弹
        c.handle_focus_lost(TOKEN_A, wind_bridge::handler::FocusLostReason::CtxLost);
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(!got_status_tip(&rx), "CtxLost 属 docMgr 噪声，不该解除去重");

        // 真正离开宿主：清记录，回来重新弹
        c.handle_focus_lost(TOKEN_A, wind_bridge::handler::FocusLostReason::Thread);
        set_caret(
            &c,
            100,
            200,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(got_status_tip(&rx), "离开宿主后再进入应重新提示");
    }

    /// follow_caret 下，坐标来自 GUI 回退时**不得**直接弹气泡——那正是用户反馈的
    /// 「还没输入时定位非常不准」：`OnSetFocus` 拿不到同步锁，回退链交出的是跨窗口的
    /// Win32 光标（Word 标题行实测偏差 814px）。应转为挂起等权威坐标。
    #[test]
    fn focus_tip_defers_when_caret_source_is_not_tsf() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(
            !got_status_tip(&rx),
            "GUI 回退坐标不可作气泡锚点，此时不得下发显示"
        );
        assert!(
            c.pending_focus_tip
                .load(std::sync::atomic::Ordering::Relaxed),
            "应转为挂起，等 DLL 补来的权威坐标"
        );
    }

    /// 上一条的续集：权威坐标到达后必须补显示，且挂起位清掉。
    ///
    /// ⚠ 消费点必须在 `handle_caret_update` 的 `composing` 闸门**之前**——焦点刚到达时用户
    /// 还没输入，`composing` 恒 false，放在闸门之后就是永远不执行且完全静默。本用例正是
    /// 钉住这个顺序：`input_buffer` 特意留空。
    #[test]
    fn focus_tip_shows_when_authoritative_caret_arrives() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(!got_status_tip(&rx));

        c.handle_caret_update(&CaretData {
            x: 473,
            y: 217,
            height: 28,
            composition_start_x: 0,
            composition_start_y: 0,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        });
        assert!(got_status_tip(&rx), "等到 TSF 权威坐标后应补显示气泡");
        assert!(
            !c.pending_focus_tip
                .load(std::sync::atomic::Ordering::Relaxed),
            "补显示后挂起位必须清掉，否则下一帧坐标会再弹一次"
        );
    }

    /// 反向对照：非 TSF 域的坐标即便到达也**不得**解除挂起。
    /// 少了这条，上一条用「任何 caret_update 都补显示」的实现也能通过。
    #[test]
    fn focus_tip_stays_pending_for_non_tsf_caret_update() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
        c.show_focus_status_if_enabled(TOKEN_A);
        let _ = got_status_tip(&rx); // 排空

        c.handle_caret_update(&CaretData {
            x: 10,
            y: 20,
            height: 20,
            composition_start_x: 0,
            composition_start_y: 0,
            source: wind_ipc::protocol::caret_source::GUI_CARET,
        });
        assert!(!got_status_tip(&rx), "又一个 GUI 回退坐标，仍不该显示");
        assert!(
            c.pending_focus_tip
                .load(std::sync::atomic::Ordering::Relaxed),
            "挂起必须保持，直到真的等到 TSF 坐标"
        );
    }

    /// 坐标本就可信时立即显示，不该被闸门误伤。
    #[test]
    fn focus_tip_shows_immediately_for_tsf_caret() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(
            &c,
            473,
            217,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(got_status_tip(&rx), "TSF 域坐标应立即显示");
        assert!(
            !c.pending_focus_tip
                .load(std::sync::atomic::Ordering::Relaxed),
            "无需挂起"
        );
    }

    /// fixed 模式压根不读 caret（用 custom_x/custom_y），故不受可信度闸门约束。
    /// 把闸门一刀切地套到所有模式上，会让固定位置的用户永远看不到焦点气泡。
    #[test]
    fn focus_tip_ignores_caret_source_in_fixed_mode() {
        let (c, rx) = coord_focus_tip(true, "fixed");
        set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(got_status_tip(&rx), "fixed 模式不读 caret，应照常显示");
    }

    /// 反向对照：开关关闭时一律不显示。
    /// 少了这条，「无条件显示」的实现能让上面四条里的三条通过。
    #[test]
    fn focus_tip_silent_when_disabled() {
        let (c, rx) = coord_focus_tip(false, "follow_caret");
        set_caret(
            &c,
            473,
            217,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(!got_status_tip(&rx), "show_on_focus=false 时不得显示");
        assert!(
            !c.pending_focus_tip
                .load(std::sync::atomic::Ordering::Relaxed),
            "开关关闭时连挂起都不该发生"
        );
    }

    /// 焦点气泡必须绕过 `show_status` 的**文本**去重。
    ///
    /// 焦点切换正是「状态文本没变但仍要提示」的场景——走文本去重路径的话，连着切两个宿主
    /// 只有第一次会弹，而这恰恰是本功能最主要的使用场景，等于开关基本无效。
    ///
    /// ⚠ 与 [`focus_tip_skips_same_host_docmgr_switch`] 的**宿主**去重是两回事，别混：
    /// 这里换的是宿主（TOKEN_A → TOKEN_B），本就该弹；那里是同一宿主内换 docMgr，不该弹。
    /// 本用例原先第二次也传同一 token，测到的其实是宿主去重引入前的旧语义。
    #[test]
    fn focus_tip_bypasses_text_dedup() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(
            &c,
            473,
            217,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(got_status_tip(&rx), "第一次焦点切换应显示");
        // 状态一字未改，模拟切到**另一个宿主**的输入框
        c.show_focus_status_if_enabled(TOKEN_B);
        assert!(
            got_status_tip(&rx),
            "文本相同也必须再显示一次——文本去重会让这个开关形同虚设"
        );
    }

    /// 失焦要作废挂起中的焦点气泡，否则权威坐标晚到时会在**已经切走之后**才弹出来。
    #[test]
    fn hide_tip_cancels_pending_focus_tip() {
        let (c, rx) = coord_focus_tip(true, "follow_caret");
        set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
        c.show_focus_status_if_enabled(TOKEN_A);
        assert!(
            c.pending_focus_tip
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        c.hide_tip();
        assert!(
            !c.pending_focus_tip
                .load(std::sync::atomic::Ordering::Relaxed),
            "失焦后挂起必须作废"
        );
        let _ = got_status_tip(&rx); // 排空
        c.handle_caret_update(&CaretData {
            x: 473,
            y: 217,
            height: 28,
            composition_start_x: 0,
            composition_start_y: 0,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        });
        assert!(
            !got_status_tip(&rx),
            "失焦之后到达的权威坐标不得再触发补显示"
        );
    }

    /// 焦点 caret 的 `height == 0`（宿主尚未 reflow 的退化矩形）不得进缓存。
    ///
    /// 这条守卫原先只在同步段的 `handle_focus_gained_caret` 里有，重型段
    /// `handle_focus_gained` 自己直写 `state.caret_*`——而**重型段必然晚于同步段执行**，
    /// 于是守卫被后到的直写整个抹掉。两处口径分裂既不报错也不 panic，只表现为定位偏一行。
    #[test]
    fn focus_caret_degenerate_rect_does_not_overwrite_cache() {
        let c = coord();
        set_caret(
            &c,
            473,
            217,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.apply_focus_caret(
            &CaretData {
                x: 9999,
                y: 9999,
                height: 0, // 退化矩形
                composition_start_x: 0,
                composition_start_y: 0,
                source: wind_ipc::protocol::caret_source::GUI_CARET,
            },
            "test",
        );
        let st = c.state.lock().unwrap();
        assert_eq!(st.caret_x, 473, "退化帧不得覆盖已有的好坐标");
        assert_eq!(st.caret_y, 217);
        assert_eq!(
            st.caret_source,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
            "来源必须与坐标同进退——只回滚其一等于伪造了一个不存在的组合"
        );
    }

    /// 上一条只证明了守卫**存在于** `apply_focus_caret`，证明不了重型段真的路由过去。
    /// 这条走 `handle_focus_gained` 生产入口：它一旦退回自己直写 `state.caret_*`，本用例即红。
    ///
    /// 顺带钉住 `caret_use_top` 也在重型段生效——那是同一次覆写抹掉的第二样东西。
    #[test]
    fn handle_focus_gained_routes_caret_through_shared_guard() {
        let c = coord();
        set_caret(
            &c,
            473,
            217,
            wind_ipc::protocol::caret_source::TSF_SELECTION,
        );
        c.handle_focus_gained(&FocusData {
            x: 9999,
            y: 9999,
            height: 0, // 退化矩形：同步段会挡，重型段直写则不会
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: 0,
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: wind_ipc::protocol::caret_source::GUI_CARET,
            bundle_id: String::new(),
            window_class: String::new(),
        });
        let st = c.state.lock().unwrap();
        assert_eq!(
            st.caret_x, 473,
            "重型段必须经 apply_focus_caret；直写会让退化帧覆盖好坐标"
        );
        assert_eq!(st.caret_y, 217);
        assert_eq!(
            st.caret_source,
            wind_ipc::protocol::caret_source::TSF_SELECTION
        );
    }

    /// 焦点事件必须作废组合起点锚定。
    ///
    /// 锚定「同一组合只锁一次、之后不再更新」的前提是**起点不会移动**，而 focus_gained 意味着
    /// 换了 docMgr——Excel 输入时在「单元格」与「公式编辑栏」之间来回切，组合整体迁移（实测
    /// 从 (593,572) 到 (1457,959)），锚点若不作废，候选窗就钉死在旧 docMgr 上：协调器拿
    /// state.caret_* 判出 reshow，下发却用锁死的组合起点，日志表现为「reshow 说要重定位、
    /// UI 位置纹丝不动」。
    #[test]
    fn focus_gained_invalidates_composition_start_anchor() {
        let c = coord();
        *c.composition_start.lock().unwrap() = (593, 572, true);
        c.handle_focus_gained(&FocusData {
            x: 1457,
            y: 959,
            height: 37,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: TOKEN_A,
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            bundle_id: String::new(),
            window_class: String::new(),
        });
        assert!(
            !c.composition_start.lock().unwrap().2,
            "换 docMgr 后组合起点必须作废，交由下一帧 caret_update 就地重锁"
        );
    }

    /// `caret_use_top` 变换在重型段同样要生效。
    /// 该变换原先只在同步段做，重型段的直写把它抹掉，表现为微信一类宿主定位差一个行高。
    #[test]
    fn handle_focus_gained_applies_caret_use_top() {
        let c = coord();
        {
            let mut ac = c.active_compat.lock().unwrap();
            ac.caret_use_top = true;
        }
        c.handle_focus_gained(&FocusData {
            x: 100,
            y: 300,
            height: 30,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: 0,
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            bundle_id: String::new(),
            window_class: String::new(),
        });
        let st = c.state.lock().unwrap();
        assert_eq!(
            st.caret_y,
            300 - 30,
            "caret_use_top 应把 Y 上移一个行高；重型段直写则原样落缓存"
        );
    }

    /// `handle_focus_gained` 内 `update_active_compat` 必须先于它自己那次 `apply_focus_caret`
    /// 调用跑完，否则本次焦点事件带来的第一份坐标会拿**上一个进程**的规则去变换——
    /// 2026-08-17 真机复现：切到配了 `caret_offset_y` 的应用后，第一次候选框/状态气泡位置
    /// 没有校正，之后的坐标更新才对，表现为「多屏下坐标还是有点偏」，一度被误判成 DPI
    /// 换算没生效。用 `pid_names` 预置该 pid 的名字（同 `update_active_compat_prefers_cached_name_over_process_lookup`
    /// 的手法），让 `handle_focus_gained` 内对新进程的规则查找无需真实 `OpenProcess` 也能命中。
    #[test]
    fn handle_focus_gained_applies_new_process_caret_offset_on_first_caret() {
        let c = coord();
        let pid = 8848u32;
        let token = (pid as u64) << 32 | 1;
        c.pid_names
            .lock()
            .unwrap()
            .insert(pid, "windowsterminal.exe".to_string());
        let mut rules = Vec::new();
        wind_config::app_compat::set_caret_offset(&mut rules, "windowsterminal.exe", 0, 12);
        *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

        c.handle_focus_gained(&FocusData {
            x: 100,
            y: 300,
            height: 30,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: token,
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            bundle_id: String::new(),
            window_class: String::new(),
        });
        let st = c.state.lock().unwrap();
        assert_eq!(
            st.caret_y, 312,
            "新进程的 caret_offset_y 必须在本次焦点事件的第一份坐标上就生效，\
             不能等到下一次 caret_update 才校正"
        );
    }

    /// 造一个 fast 档协调器并指定坐标缓存可信与否。
    fn fast_coord(verified: bool) -> Arc<Coordinator> {
        let c = coord();
        set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".to_string();
            st.caret_x = 100;
            st.caret_y = 200;
            st.caret_height = 25;
        }
        c.caret_cache_verified
            .store(verified, std::sync::atomic::Ordering::Relaxed);
        c
    }

    /// 首帧信任门：坐标缓存未经当前插入点验证时不得走短兜底——拿旧坐标首显正是
    /// Excel「进单元格第一个字漂移」的成因（手里那份属于上一个单元格）。
    #[test]
    fn untrusted_caret_arms_long_fallback() {
        let c = fast_coord(false);
        c.arm_pending_first_show();
        assert!(
            c.first_show_extended
                .load(std::sync::atomic::Ordering::Relaxed),
            "坐标不可信时应进入长兜底等待"
        );
        assert!(*c.pending_first_show.lock().unwrap());
    }

    /// 反向对照：坐标可信时必须照常走短兜底，否则信任门就成了无差别拖慢，
    /// fast 档整个失去意义。
    #[test]
    fn trusted_caret_keeps_short_fallback() {
        let c = fast_coord(true);
        c.arm_pending_first_show();
        assert!(
            !c.first_show_extended
                .load(std::sync::atomic::Ordering::Relaxed),
            "坐标可信时不应进入长兜底"
        );
        assert_eq!(
            c.first_show_fallback_ms(),
            c.rt().config.ui.candidate.fast_first_show_fallback_ms
        );
    }

    /// ★ 长等待不得被后续按键重置。闸门在候选窗显示前对**每一个字母**都会调 arm，若照常
    /// bump token 重新计时，用户多打几个字母就把这段等待反复推后 → 长兜底静默退化回短兜底、
    /// 错位照旧。Excel 建单元格上下文要 558ms，其间用户往往已敲了三五个字母。
    ///
    /// 这是「兜底超时长于组合寿命 ⇒ 永不到期」那个死结的镜像，独立守一条测试。
    #[test]
    fn long_fallback_survives_subsequent_keystrokes() {
        let c = fast_coord(false);
        c.arm_pending_first_show();
        let token = *c.pending_first_show_token.lock().unwrap();
        // 用户继续输入：闸门对第 2、3 个字母同样调 arm
        c.arm_pending_first_show();
        c.arm_pending_first_show();
        assert_eq!(
            *c.pending_first_show_token.lock().unwrap(),
            token,
            "后续按键不得重置长兜底计时，否则等待被无限推后"
        );
    }

    /// 反向对照：坐标可信的正常连打必须照旧每次重新计时（既有行为，不能被上一条误伤）。
    #[test]
    fn short_fallback_still_rearms_per_keystroke() {
        let c = fast_coord(true);
        c.arm_pending_first_show();
        let token = *c.pending_first_show_token.lock().unwrap();
        c.arm_pending_first_show();
        assert_ne!(
            *c.pending_first_show_token.lock().unwrap(),
            token,
            "短兜底路径的既有行为是每次按键重新计时"
        );
    }

    /// 长兜底到期后不再续：用旧坐标首显仍优于候选窗一直不出现。
    #[test]
    fn long_fallback_shows_when_it_finally_expires() {
        let c = fast_coord(false);
        c.arm_pending_first_show();
        // ⚠ token 必须先 let 绑定再传：写成 `fire(*c...lock().unwrap())` 会让临时 MutexGuard
        // 活到整个语句结束（Rust 临时值生命周期），而 fire 内部要再锁同一个 Mutex ⇒ 自死锁。
        let token = *c.pending_first_show_token.lock().unwrap();
        c.fire_pending_first_show(token);
        assert!(
            !*c.pending_first_show.lock().unwrap(),
            "长兜底到期必须放行，否则候选窗永不出现"
        );
        assert!(
            c.first_show_was_provisional
                .load(std::sync::atomic::Ordering::Relaxed),
            "用的是旧坐标，须记为非权威以享放宽容差"
        );
    }

    /// wait/instant 档一字不变：它们的长兜底由 caret_pending 握手负责，信任门若也插一脚，
    /// 两条路径叠加会让 wait 最坏等到 1200ms。
    #[test]
    fn trust_gate_does_not_touch_wait_mode() {
        let c = fast_coord(false);
        set_mode(&c, wind_config::app_compat::FirstShowMode::Wait);
        c.arm_pending_first_show();
        assert!(
            !c.first_show_extended
                .load(std::sync::atomic::Ordering::Relaxed),
            "wait 档不受信任门影响"
        );
        assert_eq!(c.first_show_fallback_ms(), 150, "wait 档保持既有 150ms");
    }

    /// 闸门日志打印的超时必须等于实际 arm 的超时。此前闸门直接打 `first_show_fallback_ms()`，
    /// 信任门命中时会「日志说 25ms、实际等 600ms」——排查首显延迟时这种分叉最坑人。
    #[test]
    fn logged_timeout_matches_actual_arm() {
        let c = fast_coord(false);
        assert_eq!(
            c.planned_first_show_timeout_ms(),
            FIRST_SHOW_LONG_FALLBACK_MS,
            "信任门命中时闸门日志须报长兜底"
        );
        c.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            c.planned_first_show_timeout_ms(),
            c.rt().config.ui.candidate.fast_first_show_fallback_ms,
            "未命中时须报 fast 短兜底"
        );
    }

    /// 上屏 / 组合结束必须复位长等待标记——「这一轮已在长等待中」是**每轮独立**的事实，
    /// 跨轮残留会让 `already_waiting` 的判据失去意义（当前因 `pending` 同时被复位而侥幸
    /// 不出错，但那是巧合不是设计）。
    #[test]
    fn reset_first_show_clears_extended_flag() {
        let c = fast_coord(false);
        c.arm_pending_first_show();
        assert!(
            c.first_show_extended
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        c.reset_first_show();
        assert!(
            !c.first_show_extended
                .load(std::sync::atomic::Ordering::Relaxed),
            "组合结束必须复位，否则下一轮 arm 被永久跳过"
        );
    }

    /// 焦点到达 = 换了 DocMgr，此刻 state 里那份是焦点事件随包携带的坐标（宿主多半还没
    /// reflow，Excel 甚至还没建好编辑上下文），不够格让 fast 跳过等待。
    #[test]
    fn focus_gained_invalidates_caret_cache() {
        let c = coord();
        c.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        c.handle_focus_gained(&FocusData {
            x: 100,
            y: 300,
            height: 30,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: 0,
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            bundle_id: String::new(),
            window_class: String::new(),
        });
        assert!(
            !c.caret_cache_verified
                .load(std::sync::atomic::Ordering::Relaxed),
            "焦点到达必须作废坐标缓存的可信标记"
        );
    }

    /// 用户在同一 DocMgr 内点到别处：不发 focus_gained，宿主也只在有 composition 时才回送
    /// caret_update，所以缓存里仍是上次输入的位置——必须作废。
    #[test]
    fn user_caret_move_invalidates_cache_but_self_commit_echo_does_not() {
        let c = coord();
        c.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        c.handle_selection_changed(0);
        assert!(
            !c.caret_cache_verified
                .load(std::sync::atomic::Ordering::Relaxed),
            "用户移动光标必须作废坐标缓存"
        );

        // 反向对照：自提交回声（上屏后宿主插入文本导致的光标移动）不得作废，否则每上屏
        // 一个字就作废一次，fast 档在连打时完全退化。
        c.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        *c.last_self_commit.lock().unwrap() = Some(std::time::Instant::now());
        c.handle_selection_changed(0);
        assert!(
            c.caret_cache_verified
                .load(std::sync::atomic::Ordering::Relaxed),
            "自提交回声不得作废坐标缓存"
        );
    }

    /// 兜底首显用的是按键前的旧坐标，必须记为「非权威」，否则随后到达的权威坐标会被 3px
    /// 常规容差判成要校正而跳一下——兜底路径正是抖动最容易被看见的地方。
    #[test]
    fn fallback_first_show_marks_provisional() {
        let c = coord();
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".to_string();
            st.caret_x = 100;
            st.caret_y = 200;
            st.caret_height = 25;
        }
        *c.pending_first_show.lock().unwrap() = true;
        let token = *c.pending_first_show_token.lock().unwrap();
        c.fire_pending_first_show(token);
        assert!(
            c.first_show_was_provisional
                .load(std::sync::atomic::Ordering::Relaxed),
            "兜底显示后应置位 provisional 以享放宽容差"
        );
    }

    /// 首显用过非权威坐标后，随后到达的权威坐标若只差不到 80% 行高，不得 reshow。
    /// 抖动的观感来自校正动作本身——这条钉住「小偏差不动」的行为。
    #[test]
    fn provisional_first_show_tolerates_small_correction() {
        let c = coord();
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".to_string();
            st.caret_x = 100;
            st.caret_y = 200;
            st.caret_height = 25;
        }
        *c.candidate_shown.lock().unwrap() = true;
        c.first_show_was_provisional
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // 偏差 15px < 25 × 0.8 = 20px ⇒ 应被吞掉
        c.handle_caret_update(&CaretData {
            x: 115,
            y: 200,
            height: 25,
            composition_start_x: 115,
            composition_start_y: 200,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        });
        assert_eq!(
            c.last_valid_caret.lock().unwrap().0,
            0,
            "小于 80% 行高的偏差不应触发 reshow（未走到 notify_ui_update）"
        );
    }

    /// 换行那种大偏差必须照常校正——容差放宽不能把真正的错位也一起吞掉。
    #[test]
    fn provisional_first_show_still_corrects_large_jump() {
        let c = coord();
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".to_string();
            st.caret_x = 900;
            st.caret_y = 200;
            st.caret_height = 25;
        }
        *c.candidate_shown.lock().unwrap() = true;
        c.first_show_was_provisional
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // 换行：x 回行首、y 下移两行（实测 EverEdit 曾出现 dx=156 dy=194）
        c.handle_caret_update(&CaretData {
            x: 726,
            y: 250,
            height: 25,
            composition_start_x: 726,
            composition_start_y: 250,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        });
        assert_eq!(
            c.last_valid_caret.lock().unwrap().0,
            726,
            "换行级偏差必须校正"
        );
    }

    /// 容差只作用于「首显用过非权威坐标」的那一次：常规光标更新仍按 3px 走，
    /// 否则正常输入时的小幅移动会被误吞、候选窗跟不上光标。
    #[test]
    fn settle_tolerance_applies_only_after_provisional_first_show() {
        let c = coord();
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".to_string();
            st.caret_x = 100;
            st.caret_y = 200;
            st.caret_height = 25;
        }
        *c.candidate_shown.lock().unwrap() = true;
        // 未置位 first_show_was_provisional
        c.handle_caret_update(&CaretData {
            x: 115,
            y: 200,
            height: 25,
            composition_start_x: 115,
            composition_start_y: 200,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        });
        assert_eq!(
            c.last_valid_caret.lock().unwrap().0,
            115,
            "常规路径下 15px 偏移仍应 reshow"
        );
    }

    #[test]
    fn probe_ignored_unless_fast_mode() {
        // `wait` 档的底线：退回该档的宿主必须拿到「等 reflow 权威坐标」的原行为，
        // probe 一条都不许消费。
        //
        // ⚠ 2026-08-03 前本条靠 `coord()` 的默认档恰好是 `wait` 来表达，默认档改成
        // `fast` 后那个前提失效，故改为显式设档。**测试若靠「默认值恰好是某值」间接
        // 表达语义，默认值一变它就从"守住语义"退化成"守住巧合"。**
        let c = coord();
        set_mode(&c, wind_config::app_compat::FirstShowMode::Wait);
        assert!(
            still_waiting_after_probe(&c, probe_at(800, 600, 24)),
            "非 fast 档时 probe 必须被完全忽略"
        );
    }

    /// ★ 首显有多条通路，信任门必须每条都接。本条守住 `caret_probe` 这条——它绕过闸门
    /// 直接首显，实测（2026-08-03 Excel）在闸门刚 arm 600ms 长兜底后 **6ms** 就用
    /// `(1299,535)` 抢先显示，而 200ms 后真坐标是 `(1344,744)` ⇒ 显示后跳一次。
    ///
    /// 根因是 probe 的两条判据在坐标缓存失效时**都失去判断力**：判据 1 靠「≠ 上一轮权威
    /// 坐标」推断宿主已 reflow，而焦点切换后那个基准属于另一个单元格，probe 值当然不等于
    /// 它 ⇒ 判据恒成立；判据 2 的"上次按键间隔"跨了焦点，同样说明不了当前帧可信。
    #[test]
    fn probe_defers_to_long_wait_when_cache_unverified() {
        let c = coord();
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1,
            first_show_mode: wind_config::app_compat::FirstShowMode::Fast,
            ..Default::default()
        };
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".to_string();
        }
        // 有基准、坐标也「变了」——判据 1 本会采信；但缓存未验证，基准不可比。
        *c.last_authoritative_caret.lock().unwrap() = (500, 300, true);
        c.caret_cache_verified
            .store(false, std::sync::atomic::Ordering::Relaxed);
        *c.pending_first_show.lock().unwrap() = true;
        c.handle_caret_probe(&probe_at(800, 600, 24));
        assert!(
            *c.pending_first_show.lock().unwrap(),
            "坐标缓存未验证时 probe 不得提前首显，须让位给长兜底等真坐标"
        );

        // 连打快路径（判据 2）同样要被拦住，否则换个入口照样绕过去。
        *c.last_key_interval_ms.lock().unwrap() = Some(60);
        c.handle_caret_probe(&probe_at(500, 300, 24));
        assert!(
            *c.pending_first_show.lock().unwrap(),
            "连打快路径也必须过信任门——只堵判据 1 等于没堵"
        );
    }

    #[test]
    fn probe_releases_first_show_when_caret_moved() {
        // 坐标已不同于上一轮权威 ⇒ 宿主已 reflow ⇒ 采信并提前首显。
        let c = coord();
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1,
            first_show_mode: wind_config::app_compat::FirstShowMode::Fast,
            ..Default::default()
        };
        assert!(
            !still_waiting_after_probe(&c, probe_at(800, 600, 24)),
            "坐标已变应提前首显"
        );
    }

    /// 连打快路径必须由**相邻按键间隔**驱动，不能由「距上次按键多久」驱动。
    ///
    /// 后者恒成立（试探坐标总在按键后 10ms 内到达），会让判据被完全绕过——本功能就这么
    /// 空跑过一轮。这条测试构造「间隔很大（慢速手打）」的局面：此时即使坐标等于上一轮权威
    /// （即宿主尚未 reflow），也必须继续等待，绝不能被快路径放行。
    #[test]
    fn slow_typing_does_not_take_fast_path() {
        let c = coord();
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1,
            first_show_mode: wind_config::app_compat::FirstShowMode::Fast,
            ..Default::default()
        };
        // 慢速手打：相邻按键间隔 800ms，远超默认 100ms 窗口
        *c.last_key_interval_ms.lock().unwrap() = Some(800);
        assert!(
            still_waiting_after_probe(&c, probe_at(500, 300, 24)),
            "慢速输入下不得走连打快路径，须回落到「≠上一轮权威」判据"
        );
    }

    /// 连打（间隔在窗口内）时直接采信首条试探坐标——即使它等于上一轮权威坐标。
    /// 依据：连打时光标沿同一行顺序前移、不重排，跟手比精确更重要。
    #[test]
    fn fast_typing_takes_fast_path_even_when_caret_unchanged() {
        let c = coord();
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1,
            first_show_mode: wind_config::app_compat::FirstShowMode::Fast,
            ..Default::default()
        };
        *c.last_key_interval_ms.lock().unwrap() = Some(60); // 与真机脚本同节奏
        assert!(
            !still_waiting_after_probe(&c, probe_at(500, 300, 24)),
            "连打间隔内应走快路径立即首显"
        );
    }

    #[test]
    fn probe_keeps_waiting_when_caret_equals_previous() {
        // 与上一轮权威坐标相同 ⇒ 宿主尚未 reflow（实测 WPS 前两次采样即如此）⇒ 继续等。
        // 采信它就会把候选窗定在上一轮的位置，正是要避免的抖动。
        let c = coord();
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1,
            first_show_mode: wind_config::app_compat::FirstShowMode::Fast,
            ..Default::default()
        };
        assert!(
            still_waiting_after_probe(&c, probe_at(500, 300, 24)),
            "坐标等于上一轮权威时必须继续等待"
        );
    }

    #[test]
    fn probe_rejects_degenerate_rect() {
        // 退化 rect（h<=0）：实测 WPS 采到过 top==bottom 的样本，其 x 与真实位置差 1687px。
        let c = coord();
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1,
            first_show_mode: wind_config::app_compat::FirstShowMode::Fast,
            ..Default::default()
        };
        assert!(
            still_waiting_after_probe(&c, probe_at(9999, 8888, 0)),
            "退化 rect 不得采信"
        );
    }

    #[test]
    fn default_host_waits_for_authoritative_caret() {
        // 对照组：无 compat 规则、坐标未就绪 → 保持原行为，等 reflow 权威坐标。
        // 这条也是另外两个测试的有效性保证：若闸门被改成恒放行，此测试会挂。
        let c = coord();
        assert!(
            armed_after_first_frame(&c),
            "默认宿主首帧应 arm 等待权威坐标"
        );
    }

    #[test]
    fn instant_mode_bypasses_first_show_wait() {
        // 逃生口②：compat.toml 标记「光标稳定」的宿主直接首显。连打场景只有这一项能生效。
        let c = coord();
        *c.active_compat.lock().unwrap() = ActiveCompat {
            pid: 1234,
            first_show_mode: wind_config::app_compat::FirstShowMode::Instant,
            ..Default::default()
        };
        assert!(
            !armed_after_first_frame(&c),
            "instant 档应立即首显，不得 arm 等待"
        );
    }

    #[test]
    fn ready_coords_bypass_first_show_wait() {
        // 逃生口③：已有过有效 caret 且本轮组合起点已锁定 ⇒ 没有漂移可等。
        // 对应 Go 的 `!caretValid || !compositionStartValid` 取反。
        let c = coord();
        *c.last_valid_caret.lock().unwrap() = (100, 200, 20);
        *c.composition_start.lock().unwrap() = (100, 200, true);
        assert!(!armed_after_first_frame(&c), "坐标已就绪时不应再等 reflow");
    }

    #[test]
    fn ready_coords_requires_both_caret_and_composition_start() {
        // 逃生口③的两个分量必须同时成立：只有 caret 有效、组合起点未锁定时仍须等待
        // ——组合起点未锁定正说明本轮 composition 的 reflow 坐标还没到。
        let c = coord();
        *c.last_valid_caret.lock().unwrap() = (100, 200, 20);
        // composition_start 保持 (0,0,false)
        assert!(
            armed_after_first_frame(&c),
            "仅 caret 有效、组合起点未锁定时应继续等待"
        );
    }

    #[test]
    fn no_rule_keeps_bottom_coordinates() {
        let c = coord();
        // 未命中规则（默认 (0,false)）：坐标保持原样，不做 top 变换。
        c.handle_caret_update(&caret(200, 20));
        let s = c.state.lock().unwrap();
        assert_eq!(s.caret_y, 200);
        assert_eq!(s.caret_height, 20);
    }

    /// 真机复现（2026-08-17）：服务重启时 alacritty.exe 早已在前台，管道重连只续发
    /// `caret_update`，从没有新的 `FOCUS_GAINED` 促发 `update_active_compat`——
    /// `caret_offset_*` 等 per-app 规则整个会话都停在默认值。`handle_client_connected`
    /// 应该在连接建立、确认该 pid 就是当前前台窗口时，就提前把规则字段解析好。
    #[test]
    fn apply_connected_pid_compat_loads_rule_when_pid_is_foreground() {
        let c = coord();
        let pid = 8848u32;
        c.pid_names
            .lock()
            .unwrap()
            .insert(pid, "alacritty.exe".to_string());
        let mut rules = Vec::new();
        wind_config::app_compat::set_caret_offset(&mut rules, "alacritty.exe", 0, 12);
        *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

        c.apply_connected_pid_compat(pid, pid);

        assert_eq!(
            c.active_compat.lock().unwrap().caret_offset_y,
            12,
            "该 pid 确认在前台时，连接建立即应解析出它的 per-app 规则字段"
        );
    }

    /// code review 发现（2026-08-17，未真机复现，逻辑推导）：连接建立不是真实的
    /// `FOCUS_GAINED`。对一个全新启动、TSF DLL 第一次加载、且此刻恰好已在前台的进程，
    /// 管道连接必然先于它有史以来第一条 `FOCUS_GAINED`（发不出消息就说明还没连上）。
    /// `apply_connected_pid_compat` 若像 `update_active_compat` 一样整体覆写
    /// `active_compat.pid`，会让随后真正到达的那条 `FOCUS_GAINED` 被 `crossed` 判据
    /// （`get_current_mode` / `handle_focus_gained`）误判成「同进程、未跨进程切入」，
    /// 吞掉 `initial_mode`/`initial_punct` 规则与首键竞态消除逻辑——本测试钉住
    /// `.pid`（此处默认值 0）必须原封不动。
    #[test]
    fn apply_connected_pid_compat_does_not_claim_pid_identity() {
        let c = coord();
        let pid = 8848u32;
        c.pid_names
            .lock()
            .unwrap()
            .insert(pid, "alacritty.exe".to_string());
        let mut rules = Vec::new();
        wind_config::app_compat::set_caret_offset(&mut rules, "alacritty.exe", 0, 12);
        *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

        c.apply_connected_pid_compat(pid, pid);

        assert_eq!(
            c.active_compat.lock().unwrap().pid,
            0,
            "连接建立不得提前认领 pid 身份，否则该进程随后第一条真实 FOCUS_GAINED 的 \
             crossed 判据会被误判成「同进程」"
        );
    }

    /// 后台宿主的无关重连（管道抖动等）不得覆盖真正聚焦应用的规则字段——
    /// 否则「哪个应用的规则生效」会被连接顺序而非焦点决定。
    #[test]
    fn apply_connected_pid_compat_ignores_non_foreground_pid() {
        let c = coord();
        let focused_pid = 100u32;
        let background_pid = 200u32;
        c.pid_names
            .lock()
            .unwrap()
            .insert(background_pid, "alacritty.exe".to_string());
        let mut rules = Vec::new();
        wind_config::app_compat::set_caret_offset(&mut rules, "alacritty.exe", 0, 12);
        *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

        // background_pid 建立连接，但当前前台窗口仍是 focused_pid。
        c.apply_connected_pid_compat(background_pid, focused_pid);

        assert_eq!(
            c.active_compat.lock().unwrap().caret_offset_y,
            0,
            "非前台 pid 的连接不得把它的规则字段写进 active_compat"
        );
    }

    #[test]
    fn update_active_compat_extracts_pid_and_caches() {
        let c = coord();
        // client_token = PID<<32 | instance。PID=0（无效）不更新缓存。
        c.update_active_compat(0);
        assert_eq!(*c.active_compat.lock().unwrap(), ActiveCompat::default());
        // 合法 PID：headless（非真实进程）下 process_name 取不到名字 → caret_use_top=false，
        // 但 pid 应被缓存（避免重复 OpenProcess）。
        let token = (4321u64 << 32) | 7;
        c.update_active_compat(token);
        assert_eq!(c.active_compat.lock().unwrap().pid, 4321);
    }

    #[test]
    fn update_active_compat_prefers_cached_name_over_process_lookup() {
        // macOS 路径：宿主名由 `.app` 随焦点事件送进 pid_names，`process_name` 恒空串。
        // 缓存必须优先于反查，否则 compat 规则永远匹配不到宿主。
        let c = coord();
        let pid = 5150u32;
        let token = (pid as u64) << 32 | 3;
        c.pid_names
            .lock()
            .unwrap()
            .insert(pid, "com.apple.textedit".into());
        let mut rules = Vec::new();
        wind_config::app_compat::set_first_show_mode(
            &mut rules,
            "com.apple.textedit",
            wind_config::app_compat::FirstShowMode::Fast,
        );
        *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);
        c.update_active_compat(token);
        assert_eq!(
            c.active_compat.lock().unwrap().first_show_mode,
            wind_config::app_compat::FirstShowMode::Fast,
            "缓存里的 bundle id 必须参与 compat 规则匹配"
        );
    }
}

#[cfg(test)]
mod initial_mode_tests {
    //! 初始状态语义矩阵验证：激活重置 / 全局记忆 / per-app 独立（均纯内存，无词典/UI 依赖）。
    use super::*;

    fn coord_with(f: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
        let mut cfg = Config::default();
        f(&mut cfg);
        Coordinator::new_headless(cfg, None)
    }

    /// 注入焦点进程（headless 下 OpenProcess 取不到真实进程名，手动填缓存）。
    /// 夹具语义＝「焦点与**模式归属**都已落在这个进程上」，即真实 focus_gained 跑完之后
    /// 的稳态。两个字段都要设：`active_compat.pid` 是「焦点在哪」，`mode_scope` 是
    /// 「初始模式该按谁算」，生产里由重型段一起推进（过渡窗口除外）。
    /// 只设前者会让 `crossed` 判据读到陈旧的 `mode_scope`，测试便不再对应任何真实状态。
    fn set_focus_proc(c: &Arc<Coordinator>, pid: u32, name: &str) {
        c.active_compat.lock().unwrap().pid = pid;
        c.pid_names.lock().unwrap().insert(pid, name.to_string());
        let has_rule = c.rule_initial_mode(name).is_some() || c.rule_initial_punct(name).is_some();
        *c.mode_scope.lock().unwrap() = (pid, has_rule);
    }

    fn token(pid: u32) -> u64 {
        ((pid as u64) << 32) | 1
    }

    /// global + remember=false（默认）：状态被污染成英文后，激活时重置回配置默认（中文），
    /// 全半角/标点一并重置——本 bug 的核心修复。
    #[test]
    fn activation_resets_to_default_when_not_remembering() {
        let c = coord_with(|cfg| {
            cfg.input.default.remember_last_state = false;
            cfg.input.default.chinese_mode = true;
            cfg.input.default.full_width = false;
            cfg.input.default.chinese_punct = true;
        });
        {
            let mut s = c.state.lock().unwrap();
            s.chinese_mode = false; // 模拟 compartment 脏事件污染
            s.full_width = true;
            s.chinese_punct = false;
        }
        c.apply_initial_mode(token(100), true);
        let s = c.state.lock().unwrap();
        assert!(s.chinese_mode);
        assert!(!s.full_width);
        assert!(s.chinese_punct);
    }

    /// global + remember=true：激活时保持用户最后一次主动切换的状态，不重置。
    #[test]
    fn activation_keeps_last_state_when_remembering() {
        let c = coord_with(|cfg| {
            cfg.input.default.remember_last_state = true;
            cfg.input.default.chinese_mode = true;
        });
        {
            let mut s = c.state.lock().unwrap();
            s.chinese_mode = false; // 用户切到英文
            s.full_width = true;
        }
        // 直接注入"最后状态"内存镜像（不调 record_last_state，避免测试写真实 state.toml）。
        *c.runtime_last.lock().unwrap() = (false, true, true);
        c.apply_initial_mode(token(100), true);
        let s = c.state.lock().unwrap();
        assert!(!s.chinese_mode, "remember=true 激活不得重置回默认");
        assert!(s.full_width);
    }

    /// scope=app：首见进程用配置默认；record_app_mode 写表后按进程恢复各自状态。
    #[test]
    fn per_app_scope_remembers_mode_per_process() {
        let c = coord_with(|cfg| {
            cfg.input.default.state_scope = "app".into();
            cfg.input.default.chinese_mode = true;
        });
        // 游戏进程：首见 → 默认中文；用户切英文 → 写表。
        set_focus_proc(&c, 100, "game.exe");
        assert!(
            c.initial_chinese_mode_for("game.exe"),
            "首见进程应为配置默认"
        );
        c.state.lock().unwrap().chinese_mode = false;
        c.record_app_mode(false);
        // 切到聊天进程：首见 → 默认中文。
        set_focus_proc(&c, 200, "chat.exe");
        c.apply_initial_mode(token(200), false);
        assert!(c.state.lock().unwrap().chinese_mode);
        // 切回游戏进程：恢复英文记忆。
        set_focus_proc(&c, 100, "game.exe");
        c.apply_initial_mode(token(100), false);
        assert!(!c.state.lock().unwrap().chinese_mode);
    }

    /// scope=app：FOCUS_GAINED 同步路径（get_current_mode）命中记忆表时先切换再回传；
    /// 未入缓存的进程保持现状（由重型段修正）。
    #[test]
    fn get_current_mode_switches_per_app_on_cache_hit() {
        let c = coord_with(|cfg| {
            cfg.input.default.state_scope = "app".into();
            cfg.input.default.chinese_mode = true;
        });
        // 焦点原本在别的进程。同步段先于重型段的 update_active_compat 执行，此刻
        // active_compat.pid 仍是**上一个**进程——`crossed` 判据正是靠这一点识别「跨进程
        // 切入」。故夹具必须把旧进程留在 active_compat 里，只把新进程名喂进 pid_names。
        set_focus_proc(&c, 1, "other.exe");
        c.pid_names
            .lock()
            .unwrap()
            .insert(100, "game.exe".to_string());
        c.mode_states
            .lock()
            .unwrap()
            .insert("game.exe".to_string(), false);
        // 当前全局是中文，焦点到 game.exe → 同步切英文并回传。
        let (chinese, _) = c.get_current_mode(token(100), "");
        assert!(!chinese);
        assert!(!c.state.lock().unwrap().chinese_mode);
        // 未缓存的 pid（首次聚焦）：保持现状不误切。
        let (chinese, _) = c.get_current_mode(token(999), "");
        assert!(!chinese, "未知进程应回传当前状态");
    }

    /// global（默认作用域）：get_current_mode 不做 per-app 切换，直接回权威状态。
    #[test]
    fn get_current_mode_global_scope_passthrough() {
        let c = coord_with(|_| {});
        c.state.lock().unwrap().chinese_mode = false;
        let (chinese, _) = c.get_current_mode(token(100), "");
        assert!(!chinese);
    }

    /// 注入 compat.toml 的应用规则（纯内存，不碰文件系统）。
    fn set_rule(
        c: &Arc<Coordinator>,
        process: &str,
        mode: Option<wind_config::app_compat::InitialMode>,
        punct: Option<wind_config::app_compat::InitialMode>,
    ) {
        use wind_config::app_compat::{AppCompat, AppCompatRule};
        *c.app_compat.lock().unwrap() = AppCompat::from_rules(vec![AppCompatRule {
            process: process.to_string(),
            initial_mode: mode,
            initial_punct: punct,
            ..Default::default()
        }]);
    }

    /// 应用规则**压过** per-app 记忆表。
    ///
    /// 顺序反了（规则排记忆表之后）对 Everything / Listary 这类**常驻隐藏式**进程等于
    /// 只在开机后第一次唤出时生效：进程不退出，会话级记忆表里「首次」永远只有一次。
    #[test]
    fn app_rule_beats_per_app_memory() {
        use wind_config::app_compat::InitialMode as IM;
        let c = coord_with(|cfg| {
            cfg.input.default.state_scope = "app".into();
            cfg.input.default.chinese_mode = true;
        });
        set_rule(&c, "everything.exe", Some(IM::English), None);
        c.mode_states
            .lock()
            .unwrap()
            .insert("everything.exe".into(), true); // 记忆表说中文
        assert!(
            !c.initial_chinese_mode_for("everything.exe"),
            "规则必须压过记忆表，否则对常驻进程只生效一次"
        );
        // 没有规则的进程仍旧走记忆表，既有语义不变。
        c.mode_states
            .lock()
            .unwrap()
            .insert("game.exe".into(), true);
        assert!(c.initial_chinese_mode_for("game.exe"));
    }

    /// 重算门控的完整矩阵。这是本功能唯一容易写错又最难从现象反推的地方，
    /// 逐条锁死；每条注释即该组合对应的真实场景。
    #[test]
    fn reapply_gate_matrix() {
        // 同应用内焦点跳转（Everything 搜索框 ↔ 结果列表）：一律不重算，保住用户手切。
        assert!(!should_reapply_initial(false, true, true, true, false));
        // 跨进程、无 per_app、两边都没规则（Word → Chrome）：不动。放宽成「规则表非空」
        // 就会在这里重算，把用户在 Word 手切的英文冲成配置默认。
        assert!(!should_reapply_initial(true, false, false, false, false));
        // 进入规则应用（Word → Everything）。
        assert!(should_reapply_initial(true, false, false, true, false));
        // **离开**规则应用（Everything → Word）：只看 new_has_rule 会漏掉这条，
        // 表现为 Everything 的英文残留给之后的每一个应用。
        assert!(should_reapply_initial(true, false, true, false, false));
        // per_app 既有语义不受规则影响。
        assert!(should_reapply_initial(true, true, false, false, false));

        // ── 壳过渡窗口一票否决（2026-08-18）──
        // 点任务栏 / Alt+Tab 切入 explorer.exe：即便它配了 initial_mode 规则、也确实是
        // 跨进程切入，仍不重算——用户点它是为了去别处。上面每一条为真的组合都要被否掉，
        // 否则「一票否决」就退化成了「参与投票」。
        assert!(!should_reapply_initial(true, false, false, true, true));
        assert!(!should_reapply_initial(true, false, true, false, true));
        assert!(!should_reapply_initial(true, true, false, false, true));
        assert!(!should_reapply_initial(true, true, true, true, true));
    }

    /// 端到端：作用域外窗口的 focus_gained 不得改动中英状态，桌面窗口必须照改。
    ///
    /// 单测 `reapply_gate_matrix` 只锁住纯判据；这条锁住**接线**——判据写对了但取错窗口类、
    /// 或压根没把 `window_class` 传进来，矩阵测试照样全绿。本仓已有多次「门控退化后测试
    /// 仍全绿」的先例，两层都要有。
    #[test]
    fn out_of_scope_window_skips_initial_mode_but_desktop_does_not() {
        use wind_config::app_compat::{
            AppCompat, AppCompatRule, InitialMode as IM, InitialModeScopeRule,
        };

        let build = || {
            let c = coord_with(|cfg| cfg.input.default.chinese_mode = true);
            *c.app_compat.lock().unwrap() = AppCompat::from_parts(
                vec![AppCompatRule {
                    process: "explorer.exe".into(),
                    initial_mode: Some(IM::English),
                    ..Default::default()
                }],
                vec![InitialModeScopeRule {
                    process: "explorer.exe".into(),
                    comment: String::new(),
                    classes: vec!["Progman".into()],
                }],
            );
            // 先停在别的进程上、中文态，下面才构成「跨进程切入 explorer」。
            set_focus_proc(&c, 100, "notepad.exe");
            c.state.lock().unwrap().chinese_mode = true;
            c.pid_names
                .lock()
                .unwrap()
                .insert(200, "explorer.exe".into());
            c
        };

        let focus = |class: &str| FocusData {
            x: 0,
            y: 0,
            height: 0,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: token(200),
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: 0,
            bundle_id: String::new(),
            window_class: class.into(),
        };

        // 任务栏（作用域外）：规则不套用，保持切入前的中文。
        let c = build();
        c.handle_focus_gained(&focus("Shell_TrayWnd"));
        assert!(
            c.state.lock().unwrap().chinese_mode,
            "作用域外的窗口不该触发 initial_mode 重算"
        );

        // 桌面（作用域内）：规则照常生效，切成英文。用户配 explorer.exe=english 就是为了它。
        let c = build();
        c.handle_focus_gained(&focus("Progman"));
        assert!(
            !c.state.lock().unwrap().chinese_mode,
            "桌面必须照常套用 initial_mode=english"
        );

        // ★★★ 拿不到窗口类：**保持现状**。
        // 这是 2026-08-18 17:24:08 现场的直接钉子——explorer 新起 TSF 连接后的头一个
        // focus_gained 没有窗口类（caret 也退到 last_known），旧判据（黑名单）把它放行、
        // 当场套上英文规则，用户看到图标闪「英」。信息缺失时的正确答案是「别动」。
        let c = build();
        c.handle_focus_gained(&focus(""));
        assert!(
            c.state.lock().unwrap().chinese_mode,
            "窗口类为空 = 不知道焦点在哪，必须保持现状而不是按规则重算"
        );

        // ★★ 按**生产真实顺序**再走一遍：同步段 get_current_mode 先跑（DLL 正阻塞等它），
        // 重型段 handle_focus_gained 后跑。上面那三条只调了重型段，于是「同步段没挡」这个
        // 缺陷可以全程不被发现——实测就是这样：日志显示重型段已跳过，图标照样切成英文，
        // 因为真正改掉状态的是先跑的那一个。
        //
        // 「按应用套用初始模式」有**两个落点**，测试必须两个都走，否则等于只测了一半。
        let c = build();
        let (chinese, _) = c.get_current_mode(token(200), "Shell_TrayWnd");
        assert!(chinese, "同步段也必须跳过作用域外的窗口");
        c.handle_focus_gained(&focus("Shell_TrayWnd"));
        assert!(
            c.state.lock().unwrap().chinese_mode,
            "两个落点都跳过后，状态才真的不变"
        );

        // 对照：桌面走同一条顺序，规则必须照常生效（防过度修复）。
        let c = build();
        let (chinese, _) = c.get_current_mode(token(200), "Progman");
        assert!(!chinese, "桌面的 initial_mode=english 必须在同步段就生效");
        c.handle_focus_gained(&focus("Progman"));
        assert!(!c.state.lock().unwrap().chinese_mode);
    }

    /// ★★ 作用域外的窗口**不得消费掉「跨进程切入」这个一次性事件**。
    ///
    /// 真实序列：记事本 → 点任务栏（作用域外，跳过）→ 真正回到桌面。两次焦点是**同一个
    /// explorer 进程**，若用 `active_compat.pid` 判 `crossed`，第一次就把它变成 explorer，
    /// 第二次便成了「同进程」，桌面配的 initial_mode 永远不生效——这正是修完作用域门控
    /// 之后冒出来的第二级缺陷（2026-08-18 实测：DLL 报了 Progman，服务端毫无反应）。
    #[test]
    fn out_of_scope_window_does_not_consume_the_cross_process_transition() {
        use wind_config::app_compat::{
            AppCompat, AppCompatRule, InitialMode as IM, InitialModeScopeRule,
        };

        let c = coord_with(|cfg| cfg.input.default.chinese_mode = true);
        *c.app_compat.lock().unwrap() = AppCompat::from_parts(
            vec![AppCompatRule {
                process: "explorer.exe".into(),
                initial_mode: Some(IM::English),
                ..Default::default()
            }],
            vec![InitialModeScopeRule {
                process: "explorer.exe".into(),
                comment: String::new(),
                classes: vec!["Progman".into()],
            }],
        );
        set_focus_proc(&c, 100, "notepad.exe");
        c.state.lock().unwrap().chinese_mode = true;
        c.pid_names
            .lock()
            .unwrap()
            .insert(200, "explorer.exe".into());

        let focus = |class: &str| FocusData {
            x: 0,
            y: 0,
            height: 0,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: token(200),
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: 0,
            bundle_id: String::new(),
            window_class: class.into(),
        };

        // ① 点任务栏（作用域外）：跳过，状态不变。
        c.get_current_mode(token(200), "Shell_TrayWnd");
        c.handle_focus_gained(&focus("Shell_TrayWnd"));
        assert!(c.state.lock().unwrap().chinese_mode, "任务栏不该改模式");

        // ② 真正回到桌面：**同一个 explorer pid**，仍必须算作跨进程切入并套用英文。
        let (chinese, _) = c.get_current_mode(token(200), "Progman");
        assert!(
            !chinese,
            "桌面必须仍被判为跨进程切入——作用域外的窗口不能提前消费掉这次切换"
        );
        c.handle_focus_gained(&focus("Progman"));
        assert!(!c.state.lock().unwrap().chinese_mode);
    }

    /// 显式 `initial_punct` 压过 `follow_mode` 的推导。
    /// 顺序反了的话，用户配了标点规则却恰好开着 follow_mode 时它会被静默覆盖。
    #[test]
    fn initial_punct_rule_beats_follow_mode() {
        use wind_config::app_compat::InitialMode as IM;
        let c = coord_with(|cfg| {
            cfg.input.punct.follow_mode = true;
            cfg.input.default.chinese_mode = true;
        });
        set_rule(&c, "everything.exe", Some(IM::English), Some(IM::Chinese));
        set_focus_proc(&c, 100, "everything.exe");
        c.apply_initial_mode(token(100), false);
        let s = c.state.lock().unwrap();
        assert!(!s.chinese_mode, "规则要求初始英文");
        assert!(
            s.chinese_punct,
            "initial_punct=chinese 必须压过 follow_mode 推出的英文标点"
        );
    }

    /// 同步路径：跨进程切入规则应用时当场回传英文（消除首键竞态），
    /// 而同应用内的再次 focus_gained 不得把用户手切的模式拉回规则值。
    #[test]
    fn get_current_mode_rule_applies_only_on_cross_process_switch() {
        use wind_config::app_compat::InitialMode as IM;
        let c = coord_with(|cfg| cfg.input.default.chinese_mode = true);
        set_rule(&c, "everything.exe", Some(IM::English), None);
        // 焦点原本在别的进程（active_compat.pid=1），现在切入 everything.exe。
        set_focus_proc(&c, 1, "other.exe");
        c.pid_names
            .lock()
            .unwrap()
            .insert(100, "everything.exe".to_string());
        let (chinese, _) = c.get_current_mode(token(100), "");
        assert!(!chinese, "跨进程切入规则应用 → 同步段即回传英文");

        // 重型段已把焦点与模式归属都更新为 100；用户随后手切回中文。
        set_focus_proc(&c, 100, "everything.exe");
        c.state.lock().unwrap().chinese_mode = true;
        let (chinese, _) = c.get_current_mode(token(100), "");
        assert!(
            chinese,
            "同应用内跳转不得把手切的中文拉回规则的英文——规则是初始值不是锁定"
        );
    }

    /// ★★ 同步段与重型段对「初始模式」必须给出**同一个**答案。
    ///
    /// 真实场景：桌面配 `initial_mode = "english"`，从桌面切到无规则的记事本。
    /// 同步段曾只处理「规则表/记忆表命中」，两者都没命中就保持现状——而现状正是桌面留下的
    /// 英文，于是 DLL 先拿到「英」、3~5ms 后才被重型段改成「中」。后果：DLL 据此写
    /// OPENCLOSE compartment，系统语言指示器闪一下；且这几毫秒里到达的首键按英文处理，
    /// 而同步回传的全部意义就是消除这个首键竞态。
    ///
    /// 断言写成「两者相等」而不是「等于某个具体值」：要钉的是**一致性**这个不变量，
    /// 钉死具体值会让将来调整默认模式时这条测试变成噪声。
    #[test]
    fn sync_and_heavy_paths_agree_on_initial_mode() {
        use wind_config::app_compat::InitialMode as IM;

        let c = coord_with(|cfg| {
            cfg.input.default.chinese_mode = true;
            cfg.input.default.remember_last_state = false;
        });
        // 上一个应用：桌面，规则强制英文，且已经把全局状态带成英文。
        set_rule(&c, "explorer.exe", Some(IM::English), None);
        set_focus_proc(&c, 200, "explorer.exe");
        c.apply_initial_mode(token(200), false);
        assert!(!c.state.lock().unwrap().chinese_mode, "前提：现状是英文");

        // 切到无任何规则、无记忆的记事本。
        c.pid_names
            .lock()
            .unwrap()
            .insert(300, "notepad.exe".into());
        let (sync_chinese, _) = c.get_current_mode(token(300), "Notepad");

        // 重型段随后落地的值。
        c.handle_focus_gained(&FocusData {
            x: 0,
            y: 0,
            height: 0,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: token(300),
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: 0,
            bundle_id: String::new(),
            window_class: "Notepad".into(),
        });
        let heavy_chinese = c.state.lock().unwrap().chinese_mode;

        assert_eq!(
            sync_chinese, heavy_chinese,
            "同步段回传值必须等于重型段落地值，否则 DLL 会先按前者写 compartment 再被改回"
        );
        assert!(
            sync_chinese,
            "记事本无规则应回落到配置默认（中文），而不是沿用桌面留下的英文"
        );
    }

    /// 三档的优先级与「未激活时不表态」。
    ///
    /// 优先级不是随意排的：线程级禁用时引擎压根收不到键，密码框次之，两者都不成立才轮到
    /// 「焦点不在可编辑控件里」。排错了不会有编译或测试信号，只会让 tooltip / 变淡呈现错档。
    #[test]
    fn input_block_priority_and_inactive_short_circuit() {
        let c = coord_with(|_| {});

        // 未激活：一律不表态。否则 has_edit_context 恒假会把图标永久钉成「英」。
        {
            let mut s = c.state.lock().unwrap();
            s.ime_active = false;
            // 用权威信号，不用 has_edit_context——后者被噪声层驱动，不是图标的判据。
            s.focus_no_edit_ctx = true;
        }
        c.password_suppress
            .store(true, std::sync::atomic::Ordering::Relaxed);
        c.last_input_diag.lock().unwrap().disabled = true;
        assert_eq!(c.input_block(), InputBlock::None, "未激活时不该表态");

        c.state.lock().unwrap().ime_active = true;
        assert_eq!(
            c.input_block(),
            InputBlock::KeyboardDisabled,
            "线程级禁用最高"
        );

        c.last_input_diag.lock().unwrap().disabled = false;
        assert_eq!(c.input_block(), InputBlock::Password, "其次密码框");

        c.password_suppress
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            c.input_block(),
            InputBlock::NoEditContext,
            "最后无编辑上下文"
        );

        c.state.lock().unwrap().focus_no_edit_ctx = false;
        assert_eq!(c.input_block(), InputBlock::None);
    }

    /// `pid_names` 是一次写入、永不失效的缓存，而 Windows 会复用已退出进程的 PID。
    /// 连接时校正是它唯一的自愈时机——没有它，「B 复用了 A 的 pid」之后 B 会被永久
    /// 当成 A，整条 per-app 链（compat 规则 + 中英记忆）一起错且无任何自愈路径。
    #[test]
    fn pid_name_cache_is_corrected_on_reconnect() {
        let c = coord_with(|_| {});
        c.pid_names
            .lock()
            .unwrap()
            .insert(4242, "everedit.exe".into());

        // 同一个 pid 换了进程：必须以现查为准。
        c.revalidate_pid_name(4242, "WindowsTerminal.exe");
        assert_eq!(
            c.pid_names.lock().unwrap().get(&4242).map(String::as_str),
            Some("windowsterminal.exe"),
            "PID 复用后必须纠正，否则新进程会一直套用旧进程的 per-app 配置"
        );

        // ⚠ 现查为空时**保留缓存**：macOS 的 process_name 恒为空串，名字由 `.app`
        // 随焦点事件送进来。清掉会让 compat 规则在下一次 focus_gained 之前全部失配。
        c.revalidate_pid_name(4242, "");
        assert_eq!(
            c.pid_names.lock().unwrap().get(&4242).map(String::as_str),
            Some("windowsterminal.exe"),
            "查不到名字不等于名字变了——不许把已有的清掉"
        );

        // 首次见到该 pid 时照常落缓存。
        c.revalidate_pid_name(777, "Notepad.exe");
        assert_eq!(
            c.pid_names.lock().unwrap().get(&777).map(String::as_str),
            Some("notepad.exe"),
            "缓存键与查询都按小写，与 update_active_compat 同口径"
        );
    }

    /// ★★ 噪声层的 `CtxLost` **不得**让图标显「英」，只有权威的 `NoEditCtx` 才可以。
    ///
    /// 实测缺陷（2026-08-18，本重构自己引入的）：
    ///   `handle_focus_lost reason=CtxLost` → 200ms 后 `input_block → NoEditContext`
    ///   → 图标发布「英」，而那次焦点根本没离开可编辑控件。
    /// 根子是我把判定收归 Rust 时直接读了 `has_edit_context`——那个量被 CtxLost 置假，
    /// 用于工具栏可以（翻错了 50ms 防抖吸收），用于持续可见的图标就是误报。
    ///
    /// 这条测试同时钉住「工具栏仍然要跟着 CtxLost 隐藏」，防止有人把两者又合并回去。
    #[test]
    fn ctx_lost_is_noise_for_icon_but_still_hides_toolbar() {
        use wind_bridge::handler::FocusLostReason;

        let c = coord_with(|_| {});
        {
            let mut s = c.state.lock().unwrap();
            s.ime_active = true;
            s.has_edit_context = true;
            s.focus_no_edit_ctx = false;
        }

        // 噪声层：DocMgr 级失焦。
        c.handle_focus_lost(0, FocusLostReason::CtxLost);
        {
            let s = c.state.lock().unwrap();
            assert!(!s.has_edit_context, "工具栏仍应隐藏——这一档的既有语义不变");
            assert!(
                !s.focus_no_edit_ctx,
                "但图标不许据此表态：CtxLost 回答的是「DocMgr 走了」而非「进了不可输入的地方」"
            );
        }
        assert_eq!(
            c.input_block(),
            InputBlock::None,
            "CtxLost 之后图标必须仍显方案标签"
        );

        // 权威层：新文档确实没有可编辑上下文。
        c.handle_focus_lost(0, FocusLostReason::NoEditCtx);
        assert_eq!(
            c.input_block(),
            InputBlock::NoEditContext,
            "NoEditCtx 才是「确实打不进去」的权威信号"
        );
    }

    /// ★★★ 三个档位里**只有罕见的两档**该把图标覆盖成「英」。
    ///
    /// `NoEditContext` 是日常状态：实测 VS Code 8 分钟发 35 次 `NoEditCtx`（每换一次
    /// docMgr 一次）。让它翻图标，用户看到的是图标自己在抖。且此刻图标显示什么都不影响
    /// 功能——焦点不在输入控件上，敲键盘本来就没有落点。
    ///
    /// 这条与 `ctx_lost_is_noise_for_icon_but_still_hides_toolbar` 分工不同：那条钉的是
    /// **档位判定**（哪个信号能置位），这条钉的是**呈现映射**（置位了要不要变英）。
    /// 两者都写对才不闪，只测一层会漏。
    #[test]
    fn only_rare_blocks_show_english_on_icon() {
        assert!(!InputBlock::None.shows_english());
        assert!(
            !InputBlock::NoEditContext.shows_english(),
            "无可编辑上下文是日常状态，不配翻图标（2026-08-18 实测：VS Code 里每点一下就翻一次）"
        );
        assert!(
            InputBlock::Password.shows_english(),
            "密码框必须显英——用户要能一眼看出这里敲进去的不是中文"
        );
        assert!(
            InputBlock::KeyboardDisabled.shows_english(),
            "输入法被系统整个禁用同理"
        );
        // 变淡仍然只留给线程级禁用，不因本次改动而放宽。
        assert!(InputBlock::KeyboardDisabled.dims_icon());
        assert!(!InputBlock::Password.dims_icon());
        assert!(!InputBlock::NoEditContext.dims_icon());
    }

    /// 呈现档位的**两个方向不对称**：进入要稳够 INPUT_BLOCK_DELAY，恢复立即。
    ///
    /// 这条钉的是「误显英很刺眼、晚显英无感」这个取舍本身。写成对称迟滞会让
    /// QQ 密码框那种 180ms churn 把图标打得一闪一闪，那正是当初加迟滞的起因。
    #[test]
    fn effective_input_block_is_asymmetric() {
        let c = coord_with(|_| {});
        {
            let mut s = c.state.lock().unwrap();
            s.ime_active = true;
            s.focus_no_edit_ctx = false;
        }
        assert_eq!(c.effective_input_block(), InputBlock::None);

        // 进入方向：真值已变，但没稳够 ⇒ 呈现仍是旧值。
        c.state.lock().unwrap().focus_no_edit_ctx = true;
        assert_eq!(c.input_block(), InputBlock::NoEditContext, "真值立刻就变了");
        assert_eq!(
            c.effective_input_block(),
            InputBlock::None,
            "呈现要等稳定，否则一次抖动就闪一下"
        );

        // churn：中途变回去，应撤销待定，之后再变也要重新计时。
        c.state.lock().unwrap().focus_no_edit_ctx = false;
        assert_eq!(c.effective_input_block(), InputBlock::None);

        // 稳够之后落地。
        c.state.lock().unwrap().focus_no_edit_ctx = true;
        let _ = c.effective_input_block(); // 起计时
        std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
        assert_eq!(c.effective_input_block(), InputBlock::NoEditContext);

        // 恢复方向：立即，不等迟滞。
        c.state.lock().unwrap().focus_no_edit_ctx = false;
        assert_eq!(
            c.effective_input_block(),
            InputBlock::None,
            "恢复必须立即——迟滞只该拖慢「变英」，不该拖慢「变回来」"
        );
    }

    /// 语言栏悬停提示的六个分支。文案是 DLL 唯一的信息来源（它那边已改成原样返回），
    /// 写错不会有任何编译或运行期信号——只有用户悬停时看到胡话。
    #[test]
    fn langbar_tooltip_covers_every_branch() {
        let c = coord_with(|_| {});
        {
            let mut s = c.state.lock().unwrap();
            s.ime_active = true;
            s.focus_no_edit_ctx = false;
        }
        let set = |chinese: bool, caps: bool| {
            let mut s = c.state.lock().unwrap();
            s.chinese_mode = chinese;
            s.caps_lock = caps;
        };

        set(true, false);
        assert_eq!(c.langbar_tooltip(), "清风输入法 - 中文模式");
        set(true, true);
        assert_eq!(
            c.langbar_tooltip(),
            "清风输入法 - 英文大写 (中文模式, Caps Lock)"
        );
        set(false, true);
        assert_eq!(c.langbar_tooltip(), "清风输入法 - 英文模式 (Caps Lock 开)");
        set(false, false);
        assert_eq!(c.langbar_tooltip(), "清风输入法 - 英文模式 (Caps Lock 关)");

        // ★★ 不可输入那两档要先稳够 INPUT_BLOCK_DELAY 才呈现——tooltip 与图标读**同一个**
        // effective_input_block，所以文案也跟着迟滞。这是对的：图标还显着方案标签、
        // tooltip 却已经说「密码框」，那才是错位。第一次调用起计时，等到期后再断言。
        c.password_suppress
            .store(true, std::sync::atomic::Ordering::Relaxed);
        set(true, false);
        assert_eq!(
            c.langbar_tooltip(),
            "清风输入法 - 中文模式",
            "迟滞期内仍说旧文案，与图标同步"
        );
        std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
        assert_eq!(c.langbar_tooltip(), "清风输入法 - 密码框，已切英文");
        c.password_suppress
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            c.langbar_tooltip(),
            "清风输入法 - 中文模式",
            "恢复方向不迟滞，立即回到常态文案"
        );

        c.last_input_diag.lock().unwrap().disabled = true;
        let _ = c.langbar_tooltip(); // 起计时
        std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
        assert_eq!(c.langbar_tooltip(), "清风输入法 - 已禁用");
        c.last_input_diag.lock().unwrap().disabled = false;
        // 先让闸门回到 None 再测下一档：从「已禁用」直接转向另一个非 None 档走的是
        // **进入**方向，仍会呈现旧值——那是闸门的正确行为，不是本条要测的东西。
        assert_eq!(c.langbar_tooltip(), "清风输入法 - 中文模式");

        // NoEditContext 刻意**不**单独成档：它已不再让图标显「英」（是日常状态），
        // tooltip 再提就与看到的对不上。**必须等稳够之后再断言**——迟滞期内不变是
        // 闸门的功劳，证明不了这一档没被单独处理。
        c.state.lock().unwrap().focus_no_edit_ctx = true;
        set(true, false);
        std::thread::sleep(INPUT_BLOCK_DELAY + std::time::Duration::from_millis(30));
        assert_eq!(
            c.input_block(),
            InputBlock::NoEditContext,
            "档位确实已落到 NoEditContext"
        );
        assert_eq!(
            c.langbar_tooltip(),
            "清风输入法 - 中文模式",
            "无可编辑上下文不该改变文案——图标此时也没变"
        );
    }

    /// tooltip 推送要去重：状态推送远比 tooltip 变化频繁（全半角、标点、方案切换都推状态
    /// 却不改文案），不去重就是每次状态变化都白发一条 IPC 给所有宿主。
    #[test]
    fn langbar_tooltip_push_is_deduped_but_handshake_is_forced() {
        let c = coord_with(|_| {});
        {
            let mut s = c.state.lock().unwrap();
            s.ime_active = true;
            s.chinese_mode = true;
        }
        // 首次广播：缓存为空 ⇒ 必发
        c.push_langbar_tooltip(0);
        let first = c.last_langbar_tooltip.lock().unwrap().clone();
        assert_eq!(first, "清风输入法 - 中文模式");

        // 文案没变 ⇒ 缓存不动（下游是否真的发送由 push_server 决定，这里钉的是判据）
        c.push_langbar_tooltip(0);
        assert_eq!(*c.last_langbar_tooltip.lock().unwrap(), first);

        // 文案变了 ⇒ 缓存更新
        c.state.lock().unwrap().chinese_mode = false;
        c.push_langbar_tooltip(0);
        assert_eq!(
            *c.last_langbar_tooltip.lock().unwrap(),
            "清风输入法 - 英文模式 (Caps Lock 关)"
        );

        // 握手（token != 0）绕过去重、也不写缓存：新连接手里没有任何文本，被全局去重
        // 挡掉就会一直显示本地回落值。push_connect_fix 与 diag_snapshot 都栽过这形状。
        let before = c.last_langbar_tooltip.lock().unwrap().clone();
        c.push_langbar_tooltip(token(100));
        assert_eq!(
            *c.last_langbar_tooltip.lock().unwrap(),
            before,
            "定向推送不该污染广播用的去重缓存"
        );
    }

    /// cancel_on_mode_switch=false（默认）：CapsLock 开着按切换键，保持翻转语义、不动 CapsLock。
    /// （注入路径涉及真实 SendInput，不在单测覆盖，真机验证。）
    #[test]
    fn toggle_mode_keeps_caps_when_cancel_disabled() {
        let c = coord_with(|_| {});
        {
            let mut s = c.state.lock().unwrap();
            s.caps_lock = true;
            s.chinese_mode = true;
        }
        c.handle_toggle_mode();
        let s = c.state.lock().unwrap();
        assert!(s.caps_lock, "配置关不得动 CapsLock");
        assert!(!s.chinese_mode, "配置关保持原翻转语义");
    }

    /// cancel_on_mode_switch=true 但 CapsLock 未开：不注入、正常翻转。
    #[test]
    fn toggle_mode_normal_flip_when_caps_off() {
        let c = coord_with(|cfg| cfg.input.capslock.cancel_on_mode_switch = true);
        c.state.lock().unwrap().chinese_mode = false;
        c.handle_toggle_mode();
        let s = c.state.lock().unwrap();
        assert!(s.chinese_mode, "caps 未开时应正常翻转");
        assert!(!s.caps_lock);
    }

    /// 决策顺序：per-app 表命中优先于全局默认。
    #[test]
    fn initial_mode_decision_order() {
        let c = coord_with(|cfg| {
            cfg.input.default.state_scope = "app".into();
            cfg.input.default.chinese_mode = true;
        });
        c.mode_states
            .lock()
            .unwrap()
            .insert("x.exe".to_string(), false);
        assert!(!c.initial_chinese_mode_for("x.exe"), "表命中优先");
        assert!(c.initial_chinese_mode_for("y.exe"), "未命中落默认");
        assert!(c.initial_chinese_mode_for(""), "空进程名落默认");
    }
}

#[cfg(test)]
mod capslock_tests {
    //! CapsLock 大写模式路由验证（不需要词典文件）。
    //! 覆盖三条路径：字母透传 / 标点透传 / 全角提交。
    use super::*;

    fn coord_cn() -> Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        // 关闭智能符号，避免 CommitAndHoldComposition 干扰标点断言
        cfg.input.symbol.smart_mode = false;
        Coordinator::new_headless(cfg, None)
    }

    /// 构造最简按键事件
    fn kev(key_code: u32, event_type: u8) -> KeyEventData {
        KeyEventData {
            key_code,
            scan_code: 0,
            modifiers: 0,
            event_type,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        }
    }

    /// 向 coordinator 注入 CapsLock 状态（模拟 C++ 端发 key_up + toggles 位）。
    fn set_caps_lock(c: &Coordinator, on: bool) {
        let mut ev = kev(0x14 /* VK_CAPITAL */, EVENT_KEY_UP);
        ev.toggles = if on { 0x01 } else { 0x00 };
        c.handle_key_event(&ev);
    }

    /// CapsLock 开启期间的按键事件：真实 C++ 每键都带 toggles 快照（GetKeyState 实时值），
    /// caps 开着时 bit0=1。handle_key_event 入口会按此快照校准镜像，故必须如实构造。
    fn kev_caps(key_code: u32, event_type: u8) -> KeyEventData {
        let mut ev = kev(key_code, event_type);
        ev.toggles = 0x01;
        ev
    }

    /// 每键 toggles 快照校准镜像：英文模式（TSF 不吃 VK_CAPITAL）或在其它应用/输入法
    /// 期间切换大写时，专门的状态通知不会到达、镜像陈旧——此校准是 cancel_on_mode_switch
    /// 在"英文+大写"场景能生效的前提（真机回归：切方案取消不了 CapsLock 的根因）。
    #[test]
    fn key_event_toggles_recalibrates_caps_mirror() {
        let c = coord_cn();
        assert!(!c.state.lock().unwrap().caps_lock);
        // 未收到过 VK_CAPITAL 通知，但按键快照显示 caps 已开 → 入口校准。
        c.handle_key_event(&kev_caps(0x41, EVENT_KEY_DOWN));
        assert!(
            c.state.lock().unwrap().caps_lock,
            "入口应按 toggles 快照校准 CapsLock 镜像"
        );
    }

    // ── 字母透传 ────────────────────────────────────────────────────────────

    #[test]
    fn capslock_on_letter_passthrough() {
        let c = coord_cn();
        set_caps_lock(&c, true);
        // 字母 A：中文 + CapsLock + 无 session → 系统产生大写 A，coordinator 不介入
        let action = c.handle_key_event(&kev_caps(0x41, EVENT_KEY_DOWN));
        assert!(
            matches!(action, KeyAction::PassThrough),
            "中文+CapsLock+字母应透传，实际: {:?}",
            action
        );
    }

    #[test]
    fn capslock_off_letter_enters_chinese_flow() {
        let c = coord_cn();
        // CapsLock 关：字母进入中文输入流
        let action = c.handle_key_event(&kev(0x41, EVENT_KEY_DOWN));
        assert!(
            matches!(action, KeyAction::UpdateComposition { .. }),
            "CapsLock关+字母应进输入流，实际: {:?}",
            action
        );
    }

    // ── 标点透传（无 input session）──────────────────────────────────────────

    #[test]
    fn capslock_on_punct_no_session_passthrough() {
        let c = coord_cn();
        set_caps_lock(&c, true);
        // VK 0xBC = ','，无 input_buffer → 透传给系统
        let action = c.handle_key_event(&kev_caps(0xBC, EVENT_KEY_DOWN));
        assert!(
            matches!(action, KeyAction::PassThrough),
            "中文+CapsLock+无session+标点应透传，实际: {:?}",
            action
        );
    }

    #[test]
    fn capslock_off_punct_commits_chinese_punct() {
        let c = coord_cn();
        let action = c.handle_key_event(&kev(0xBC, EVENT_KEY_DOWN));
        // CapsLock 关 + 中文标点：',' → "，"
        let text = match &action {
            KeyAction::InsertText { text, .. } => text.clone(),
            other => panic!("CapsLock关+逗号应上屏中文标点，实际: {:?}", other),
        };
        assert_eq!(text, "，", "实际文本: {:?}", text);
    }

    // ── 智能符号 HoldComposition：press2 的替换语义 ──────────────────────────

    /// press1 把中文符号放进 TSF 组合（hold 预览态），press2 必须返回
    /// `CommitReplacingHeld` 而非普通 `InsertText`。
    ///
    /// 两者在 IPC 载荷上完全同构，C++ 端只能靠这个 action 带的 flags 位判断该
    /// **覆盖**还是**追加** held 符号。退回 InsertText 的后果是 press2 打出「，,」
    /// ——中文符号被并入前缀跟着一起上屏了。
    #[test]
    fn smart_symbol_hold_press2_replaces_held_symbol() {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.input.symbol.smart_mode = true;
        cfg.input.symbol.smart_method = wind_config::config::SmartMethod::HoldComposition;
        let c = Coordinator::new_headless(cfg, None);

        // press1：空缓冲 + 中文标点 → 中文符号进组合态，等 press2
        let a1 = c.handle_key_event(&kev(0xBC, EVENT_KEY_DOWN));
        match &a1 {
            KeyAction::HoldComposition { text, .. } => {
                assert_eq!(text, "，", "press1 应把中文逗号放进组合")
            }
            other => panic!("press1 应开 hold 组合，实际: {:?}", other),
        }

        // press2：超时窗口内重按同键 → 英文符号 + 替换语义
        let a2 = c.handle_key_event(&kev(0xBC, EVENT_KEY_DOWN));
        match &a2 {
            KeyAction::CommitReplacingHeld { text, .. } => {
                assert_eq!(text, ",", "press2 应换成英文逗号")
            }
            other => panic!(
                "press2 必须返回 CommitReplacingHeld（替换语义），实际: {:?}",
                other
            ),
        }
    }

    // ── 全角模式：提交全角字符 ───────────────────────────────────────────────

    #[test]
    fn capslock_on_fullwidth_letter_commits_uppercase_fullwidth() {
        let c = coord_cn();
        c.state.lock().unwrap().full_width = true;
        set_caps_lock(&c, true);
        // CapsLock ON + 无 Shift + 字母 A → 大写 A → 全角 "Ａ"
        let action = c.handle_key_event(&kev_caps(0x41, EVENT_KEY_DOWN));
        match &action {
            KeyAction::InsertText { text, .. } => {
                assert_eq!(
                    text, "Ａ",
                    "CapsLock+全角+A应输出全角大写，实际: {:?}",
                    text
                );
            }
            other => panic!("CapsLock+全角+字母应上屏，实际: {:?}", other),
        }
    }

    #[test]
    fn capslock_on_fullwidth_shift_letter_commits_lowercase_fullwidth() {
        let c = coord_cn();
        c.state.lock().unwrap().full_width = true;
        set_caps_lock(&c, true);
        // CapsLock ON + Shift + 字母 A → 翻转大小写 → 小写 a → 全角 "ａ"
        let mut ev = kev_caps(0x41, EVENT_KEY_DOWN);
        ev.modifiers = MOD_SHIFT;
        let action = c.handle_key_event(&ev);
        match &action {
            KeyAction::InsertText { text, .. } => {
                assert_eq!(
                    text, "ａ",
                    "CapsLock+Shift+全角+A应输出全角小写，实际: {:?}",
                    text
                );
            }
            other => panic!("CapsLock+Shift+全角+字母应上屏，实际: {:?}", other),
        }
    }

    #[test]
    fn capslock_on_fullwidth_punct_commits_fullwidth() {
        let c = coord_cn();
        c.state.lock().unwrap().full_width = true;
        set_caps_lock(&c, true);
        // ',' 经英全列转换后上屏（不透传）
        let action = c.handle_key_event(&kev_caps(0xBC, EVENT_KEY_DOWN));
        assert!(
            matches!(action, KeyAction::InsertText { .. }),
            "CapsLock+全角+标点应上屏，实际: {:?}",
            action
        );
    }

    // ── CapsLock 状态切换正确传播 ────────────────────────────────────────────

    #[test]
    fn capslock_toggle_updates_state() {
        let c = coord_cn();
        assert!(
            !c.state.lock().unwrap().caps_lock,
            "初始 CapsLock 应为 false"
        );
        set_caps_lock(&c, true);
        assert!(
            c.state.lock().unwrap().caps_lock,
            "set_caps_lock(true) 后应为 true"
        );
        set_caps_lock(&c, false);
        assert!(
            !c.state.lock().unwrap().caps_lock,
            "set_caps_lock(false) 后应为 false"
        );
    }
}

#[cfg(test)]
mod focus_ownership_tests {
    //! 失焦事件的客户端归属校验：旧宿主迟到的 focus_lost 不得清掉新宿主刚建立的激活态。
    //!
    //! 复现自 2026-07-26 的工具栏缺陷——从 Windows Terminal 切到记事本，记事本
    //! focus_gained 让工具栏显示，86ms 后 Terminal 的 OnKillThreadFocus 才发出 focus_lost，
    //! 把 `ime_active` 清成 false，工具栏闪一下即隐藏。
    use super::*;

    /// 已有宿主 `token` 处于激活态、且焦点在可编辑控件里的协调器。
    fn activated(token: u64) -> Arc<Coordinator> {
        let c = Coordinator::new_headless(Config::default(), None);
        c.push_server.set_active_token(token);
        let mut s = c.state.lock().unwrap();
        s.ime_active = true;
        s.has_edit_context = true;
        drop(s);
        c
    }

    /// 四种 reason 的后果矩阵——本设计的核心契约。
    ///
    /// 三项后果彼此独立，任何一格改错都会复活一个已修的缺陷：
    /// - `CtxLost` 那行的「输入态不清」＝ Excel「首字符不进编码、直接上屏」的防线；
    /// - `DocChanged` 那行的「ime_active 不动」＝ 同宿主换文档不再误关工具栏；
    /// - 各行的 `has_edit_context`＝ 应用内点到非文本框时工具栏能否隐藏。
    #[test]
    fn focus_lost_reason_consequence_matrix() {
        // (reason, ime_active 保留?, has_edit_context 保留?, 输入态保留?)
        let cases = [
            (FocusLostReason::Thread, false, false, false),
            (FocusLostReason::DocChanged, true, true, false),
            (FocusLostReason::CtxLost, true, false, true),
            (FocusLostReason::NoEditCtx, true, false, false),
        ];
        for (reason, keep_ime, keep_edit, keep_input) in cases {
            let c = activated(NOTEPAD);
            c.state.lock().unwrap().input_buffer.push_str("abc");

            c.handle_focus_lost(NOTEPAD, reason);

            let s = c.state.lock().unwrap();
            assert_eq!(
                s.ime_active, keep_ime,
                "{reason:?}: ime_active 应为 {keep_ime}"
            );
            assert_eq!(
                s.has_edit_context, keep_edit,
                "{reason:?}: has_edit_context 应为 {keep_edit}"
            );
            assert_eq!(
                !s.input_buffer.is_empty(),
                keep_input,
                "{reason:?}: 输入态保留应为 {keep_input}"
            );
        }
    }

    /// CtxLost 来自 DocMgr 噪声层（Excel 同一 DocMgr 6ms 内掉了又回），在那里清输入态
    /// 就是「首字符直接上屏」的根因。单独立一条守住这个不变量。
    #[test]
    fn ctx_lost_never_touches_input_buffer() {
        let c = activated(NOTEPAD);
        c.state.lock().unwrap().input_buffer.push_str("nihao");
        c.handle_focus_lost(NOTEPAD, FocusLostReason::CtxLost);
        assert_eq!(
            c.state.lock().unwrap().input_buffer,
            "nihao",
            "CtxLost 绝不可清输入态，否则复发 Excel 首字符丢失"
        );
    }

    /// 陈旧失焦被丢弃时，四种 reason 都不得改动任何**输入/激活**状态。
    ///
    /// ⚠️ 菜单是刻意的例外（见 `stale_focus_lost_still_closes_menu`）：关菜单在 stale 判定
    /// 之前执行，因为「这条失焦不该动激活态」不等于「没发生焦点变动」。往本测试里补断言时
    /// 别顺手把菜单也算进"任何状态"。
    #[test]
    fn stale_focus_lost_is_inert_for_all_reasons() {
        for reason in [
            FocusLostReason::Thread,
            FocusLostReason::DocChanged,
            FocusLostReason::CtxLost,
            FocusLostReason::NoEditCtx,
        ] {
            let c = activated(NOTEPAD);
            c.handle_focus_lost(TERMINAL, reason);
            let s = c.state.lock().unwrap();
            assert!(s.ime_active, "{reason:?}: 陈旧失焦不得清 ime_active");
            assert!(
                s.has_edit_context,
                "{reason:?}: 陈旧失焦不得清 has_edit_context"
            );
        }
    }

    const NOTEPAD: u64 = 0x0000_3644_0000_0001;
    const TERMINAL: u64 = 0x0000_3ECC_0000_0001;

    #[test]
    fn stale_focus_lost_keeps_activation() {
        let c = activated(NOTEPAD);
        c.handle_focus_lost(TERMINAL, FocusLostReason::Thread);
        assert!(
            c.state.lock().unwrap().ime_active,
            "旧宿主迟到的失焦不得清激活态，否则工具栏闪一下即隐藏"
        );
    }

    #[test]
    fn own_focus_lost_clears_activation() {
        let c = activated(NOTEPAD);
        c.handle_focus_lost(NOTEPAD, FocusLostReason::Thread);
        assert!(
            !c.state.lock().unwrap().ime_active,
            "当前活动客户端自己失焦仍须正常清激活态"
        );
    }

    #[test]
    fn legacy_zero_token_still_clears() {
        let c = activated(NOTEPAD);
        c.handle_focus_lost(0, FocusLostReason::Thread);
        assert!(
            !c.state.lock().unwrap().ime_active,
            "旧 DLL 不带 token，保守放行以保持既有行为"
        );
    }

    #[test]
    fn stale_ime_deactivated_keeps_activation() {
        let c = activated(NOTEPAD);
        c.handle_ime_deactivated(TERMINAL);
        assert!(
            c.state.lock().unwrap().ime_active,
            "IME_DEACTIVATED 与 focus_lost 同为异步写，乱序风险相同"
        );
    }

    #[test]
    fn own_ime_deactivated_clears_activation() {
        let c = activated(NOTEPAD);
        c.handle_ime_deactivated(NOTEPAD);
        assert!(!c.state.lock().unwrap().ime_active);
    }

    // ———————————————— 焦点变化关闭菜单 ————————————————
    //
    // 菜单是模态 UI，任何焦点变动都该终结它；而输入态清理必须保守。此前两者绑在同一个
    // `clears_input` 上，于是 CtxLost 豁免 / 陈旧失焦丢弃这两道为保护输入态而设的闸门
    // 顺带把关菜单也吞了——表现为「切走窗口菜单还挂着」。以下几条守住解耦后的语义。

    /// 构造「菜单已打开 `age` 时长」的状态。
    /// `checked_sub` 失败（机器刚启动不足 `age`）时落到 `None`，守卫按"无时间戳=不豁免"
    /// 处理，与本组测试期望的方向一致，故无需特殊处理。
    fn open_menu(c: &Coordinator, age: std::time::Duration) {
        let mut s = c.state.lock().unwrap();
        s.menu_open = true;
        s.menu_opened_at = std::time::Instant::now().checked_sub(age);
    }

    /// 打开够久的菜单
    fn open_menu_settled(c: &Coordinator) {
        open_menu(c, crate::handle_menu::MENU_FOCUS_GUARD * 4);
    }

    /// `CtxLost` 是本组的关键用例：它**不清输入态**（Excel 首字符防线），但**必须关菜单**。
    /// 两者从此各行其是——这正是本次解耦要证明的事。
    #[test]
    fn ctx_lost_closes_menu_but_keeps_input() {
        let c = activated(NOTEPAD);
        c.state.lock().unwrap().input_buffer.push_str("nihao");
        open_menu_settled(&c);

        c.handle_focus_lost(NOTEPAD, FocusLostReason::CtxLost);

        let s = c.state.lock().unwrap();
        assert!(!s.menu_open, "CtxLost 必须关菜单（它是一次真实的焦点变动）");
        assert_eq!(
            s.input_buffer, "nihao",
            "CtxLost 仍绝不可清输入态，否则复发 Excel 首字符丢失"
        );
    }

    /// 陈旧失焦同样要关菜单：判成 stale 只说明「这条失焦不该动激活态」，
    /// 不说明「没发生焦点变动」。跨宿主切换时旧宿主的失焦恒被判 stale，
    /// 若跟着一起丢弃，切走应用后菜单就永远挂着。
    #[test]
    fn stale_focus_lost_still_closes_menu() {
        let c = activated(NOTEPAD);
        open_menu_settled(&c);

        c.handle_focus_lost(TERMINAL, FocusLostReason::Thread);

        let s = c.state.lock().unwrap();
        assert!(!s.menu_open, "陈旧失焦也要关菜单");
        assert!(s.ime_active, "但仍不得清激活态（工具栏闪隐的老缺陷）");
    }

    /// 切进新的可编辑上下文也算外部动作。
    #[test]
    fn focus_gained_closes_menu() {
        let c = activated(NOTEPAD);
        open_menu_settled(&c);
        c.handle_focus_gained(&FocusData {
            x: 10,
            y: 20,
            height: 16,
            composition_start_x: 0,
            composition_start_y: 0,
            client_token: NOTEPAD,
            input_scope_mask: 0,
            disabled: false,
            reason: 0,
            caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            bundle_id: String::new(),
            window_class: String::new(),
        });
        assert!(!c.state.lock().unwrap().menu_open);
    }

    /// 守卫期：菜单刚弹出时到达的焦点事件是「打开菜单这个动作本身」的尾迹，不是用户切走。
    /// 跨宿主切换时旧宿主 focus_lost 实测晚约 100ms，从任务栏语言栏图标点开菜单正落在这个
    /// 窗口里——不豁免就会「菜单弹出即消失」。
    ///
    /// 用 `CtxLost` 而非 `Thread`：后者走 `clears_input` 分支，那里会无条件复位菜单态
    /// （因为 `notify_ui_hide` 已把窗口隐藏，留 `menu_open=true` 反而状态不一致），
    /// 刻意不受守卫保护，拿它测守卫会测错对象。
    #[test]
    fn menu_survives_focus_event_within_guard() {
        let c = activated(NOTEPAD);
        open_menu(&c, std::time::Duration::from_millis(0));

        c.handle_focus_lost(NOTEPAD, FocusLostReason::CtxLost);

        assert!(
            c.state.lock().unwrap().menu_open,
            "守卫期内的焦点事件不得关掉刚弹出的菜单"
        );
    }

    /// 同一宿主内多个 DocMgr 共用一个 token，一律放行——那层抖动（doc_changed 先发
    /// focus_lost 紧接 focus_gained，间隔 <10ms）由 UI 层 50ms 隐藏防抖吸收，不归本校验管。
    #[test]
    fn same_host_doc_churn_is_not_stale() {
        let c = activated(NOTEPAD);
        assert!(!c.is_stale_focus_event(NOTEPAD, "test"));
    }

    /// 服务端刚启动、尚无任何客户端获焦：无从判定归属，放行。
    #[test]
    fn no_active_client_is_not_stale() {
        let c = Coordinator::new_headless(Config::default(), None);
        assert!(!c.is_stale_focus_event(TERMINAL, "test"));
    }
}

#[cfg(test)]
mod per_app_compat_tests {
    //! per-app 兼容规则：自动配对开关、智能符号方案、光标坐标校正。
    use super::*;
    use wind_config::config::SmartMethod;

    fn coord_with(cfg: Config) -> Arc<Coordinator> {
        Coordinator::new_headless(cfg, None)
    }

    /// CaretData 无 `Default`，测试里显式构造（字段少，且显式写出更能看清哪些参与变换）。
    fn caret(x: i32, y: i32, height: i32, cs_x: i32, cs_y: i32) -> CaretData {
        CaretData {
            x,
            y,
            height,
            composition_start_x: cs_x,
            composition_start_y: cs_y,
            source: 0,
        }
    }

    fn pair_cfg() -> Config {
        let mut cfg = Config::default();
        cfg.input.auto_pair.chinese = true;
        cfg.input.auto_pair.english = true;
        cfg.input.auto_pair.chinese_pairs = vec!["（）".to_string()];
        cfg.input.auto_pair.english_pairs = vec!["()".to_string()];
        cfg
    }

    /// per-app 关闭后，`active_pairs` 在**中英两种标点态**都必须返回 None。
    ///
    /// 分别断言两种标点态而不是只测一种：全局开关本来就是 chinese / english 两个独立字段，
    /// 只在其中一条上加闸门是本仓反复出现的「半截修复」形态。
    #[test]
    fn auto_pair_rule_off_kills_both_punct_modes() {
        let c = coord_with(pair_cfg());
        assert!(c.active_pairs(true).is_some(), "默认（未配规则）应跟随全局");
        assert!(c.active_pairs(false).is_some());

        c.active_compat.lock().unwrap().auto_pair = Some(false);
        assert!(c.active_pairs(true).is_none(), "中文标点态应被关掉");
        assert!(c.active_pairs(false).is_none(), "英文标点态应被关掉");

        // 显式启用 = 跟随全局的开关，不是无条件开。
        c.active_compat.lock().unwrap().auto_pair = Some(true);
        assert!(c.active_pairs(true).is_some());
    }

    /// `is_auto_pair_char` 建立在 `active_pairs` 之上，规则关闭后必须一并失效——
    /// 它是「智能符号与自动配对互斥」的判据，若还认为字符参与配对，智能符号会被误让位。
    #[test]
    fn auto_pair_rule_off_releases_smart_symbol_interlock() {
        let c = coord_with(pair_cfg());
        c.state.lock().unwrap().chinese_punct = true;
        {
            let state = c.state.lock().unwrap();
            assert!(c.is_auto_pair_char(&state, '（'), "默认应认为参与配对");
        }

        c.active_compat.lock().unwrap().auto_pair = Some(false);
        {
            let state = c.state.lock().unwrap();
            assert!(!c.is_auto_pair_char(&state, '（'), "规则关闭后互锁应解除");
        }
    }

    /// 光标坐标校正：两个消费点（`apply_focus_caret` / `handle_caret_update`）共用
    /// `apply_caret_compat`，此处直接锁住那个变换本身。`dpi_scale_for_point` 在
    /// `cfg(test)` 下恒回退 1.0（见其文档），故这里的期望坐标等同于 dp 值本身；
    /// 缩放本身的数学在 [`dp_offset_to_pixels_scales_with_dpi`] 单独覆盖。
    #[test]
    fn caret_offset_shifts_coordinates() {
        let c = coord_with(Config::default());
        {
            let mut ac = c.active_compat.lock().unwrap();
            ac.caret_offset_x = -3;
            ac.caret_offset_y = 7;
        }
        let mut data = caret(100, 200, 20, 0, 0);
        c.apply_caret_compat(&mut data);
        assert_eq!((data.x, data.y), (97, 207));
        assert_eq!(data.height, 20, "偏移不应改动行高");
        // compStart 为 0 表示"未提供"，不能被平移成一个假坐标。
        assert_eq!((data.composition_start_x, data.composition_start_y), (0, 0));

        // compStart 有真值时随之平移，保持与 caret 的锚点关系。
        let mut with_cs = caret(100, 200, 20, 50, 180);
        c.apply_caret_compat(&mut with_cs);
        assert_eq!(
            (with_cs.composition_start_x, with_cs.composition_start_y),
            (47, 187)
        );
    }

    /// dp→物理像素换算：同一份 dp 配置在不同缩放的显示器上，换算出的物理像素偏移应随
    /// 缩放等比放大，这正是本功能要解决的「多屏不同缩放下无法完美兼容」的核心数学。
    #[test]
    fn dp_offset_to_pixels_scales_with_dpi() {
        assert_eq!(
            dp_offset_to_pixels(12, -2, 1.0),
            (12, -2),
            "100% 缩放下 dp==物理像素"
        );
        assert_eq!(
            dp_offset_to_pixels(12, -2, 1.5),
            (18, -3),
            "150% 缩放等比放大"
        );
        assert_eq!(dp_offset_to_pixels(12, -2, 2.0), (24, -4), "200% 缩放翻倍");
        assert_eq!(
            dp_offset_to_pixels(3, 0, 1.25),
            (4, 0),
            "四舍五入到最近物理像素"
        );
    }

    /// `caret_offset_shifts_coordinates` 只在 `cfg(test)` 恒 1.0 的 scale 下测过
    /// `apply_caret_compat`，证明不了非 1.0 缩放真的接了进去（`dp_offset_to_pixels_scales_with_dpi`
    /// 也只测纯数学，不碰 `apply_dp_offset` 这条落地路径）。此处直接调 `apply_dp_offset`
    /// 本体、显式传 150% 缩放，钉住 caret 与 composition_start 两处都按 scale 换算
    /// （2026-08-17 code review 指出的 test-wiring gap）。
    #[test]
    fn apply_dp_offset_wires_scale_into_full_transform() {
        let mut data = caret(100, 200, 20, 50, 180);
        apply_dp_offset(&mut data, -3, 7, 1.5);
        assert_eq!(
            (data.x, data.y),
            (100 - 5, 200 + 11),
            "150% 下 -3dp→-4.5→round(-5)px，7dp→10.5→round(11)px"
        );
        assert_eq!(
            (data.composition_start_x, data.composition_start_y),
            (50 - 5, 180 + 11),
            "组合起点须按同一 scale 同步平移，不能只动 caret"
        );
    }

    /// 零偏移必须是彻底的 no-op：未配规则的应用绝不能因为这条链路而坐标漂移。
    #[test]
    fn caret_offset_zero_is_noop() {
        let c = coord_with(Config::default());
        let orig = caret(100, 200, 20, 50, 180);
        let mut data = orig;
        c.apply_caret_compat(&mut data);
        assert_eq!((data.x, data.y), (orig.x, orig.y));
        assert_eq!(
            (data.composition_start_x, data.composition_start_y),
            (orig.composition_start_x, orig.composition_start_y)
        );
    }

    /// 智能符号方案：per-app 覆盖优先，未配则跟随全局。
    #[test]
    fn smart_method_per_app_overrides_global() {
        let mut cfg = Config::default();
        cfg.input.symbol.smart_method = SmartMethod::DeleteReplace;
        let c = coord_with(cfg);
        assert_eq!(c.effective_smart_method(), SmartMethod::DeleteReplace);

        c.active_compat.lock().unwrap().smart_method = Some(SmartMethod::HoldComposition);
        assert_eq!(c.effective_smart_method(), SmartMethod::HoldComposition);

        c.active_compat.lock().unwrap().smart_method = None;
        assert_eq!(
            c.effective_smart_method(),
            SmartMethod::DeleteReplace,
            "清除规则应回到全局值"
        );
    }
}

#[cfg(test)]
mod input_diag_tests {
    //! last_input_diag 存储 + 密码框强制英文抑制。
    use super::*;
    use crate::input_diag::InputDiagReason;

    fn test_coordinator() -> Arc<Coordinator> {
        Coordinator::new_headless(Config::default(), None)
    }

    #[test]
    fn password_scope_sets_suppress_and_state() {
        let c = test_coordinator();
        c.apply_input_diag(1234, false, /*reason*/ 2, 1 << 31);
        assert!(
            c.password_suppress
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        let d = c.last_input_diag.lock().unwrap();
        assert_eq!(d.reason, InputDiagReason::InputScopePassword);
        assert_eq!(d.pid, 1234);
    }

    #[test]
    fn suppress_cleared_when_mask_clears() {
        let c = test_coordinator();
        c.apply_input_diag(1, false, 2, 1 << 31);
        c.apply_input_diag(1, false, 0, 0);
        assert!(
            !c.password_suppress
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[test]
    fn disabled_policy_no_suppress_when_off() {
        let c = test_coordinator();
        c.password_suppress_enabled
            .store(false, std::sync::atomic::Ordering::Relaxed);
        c.apply_input_diag(1, false, 2, 1 << 31);
        assert!(
            !c.password_suppress
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    /// 构造最简按键事件（对齐 capslock_tests::kev 的写法）。
    fn kev(key_code: u32, event_type: u8) -> KeyEventData {
        KeyEventData {
            key_code,
            scan_code: 0,
            modifiers: 0,
            event_type,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        }
    }

    /// 真实输入路径验证：密码框抑制期间字母键必须透传（强制英文），
    /// 解除抑制后同一按键应回到中文组词流——防止「只改图标不拦输入」的回归。
    #[test]
    fn password_suppress_forces_english_passthrough() {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        let c = Coordinator::new_headless(cfg, None);
        assert!(
            c.state.lock().unwrap().chinese_mode,
            "前置条件：应处于中文模式"
        );

        let pid = 4321u32;
        c.apply_input_diag(pid, false, 2, 1 << 31);
        assert!(
            c.password_suppress
                .load(std::sync::atomic::Ordering::Relaxed),
            "前置条件：密码框抑制应已置位"
        );
        let action = c.handle_key_event(&kev(0x41 /* VK_A */, EVENT_KEY_DOWN));
        assert!(
            matches!(action, KeyAction::PassThrough),
            "密码框抑制期间字母键应强制透传（英文），实际: {:?}",
            action
        );
        assert!(
            c.state.lock().unwrap().chinese_mode,
            "抑制不应改动 chinese_mode 持久值（图标保持不变）"
        );

        // 解除抑制：mask 清零。
        c.apply_input_diag(pid, false, 0, 0);
        assert!(
            !c.password_suppress
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        let action = c.handle_key_event(&kev(0x41 /* VK_A */, EVENT_KEY_DOWN));
        assert!(
            !matches!(action, KeyAction::PassThrough),
            "解除抑制后字母键应进入中文组词流，不应透传，实际: {:?}",
            action
        );
    }

    #[test]
    fn toggle_hud_flips_visibility() {
        use std::sync::atomic::Ordering::Relaxed;
        let c = test_coordinator();
        assert!(!c.input_diag_hud_visible.load(Relaxed));
        c.toggle_input_diag_hud();
        assert!(c.input_diag_hud_visible.load(Relaxed));
        c.toggle_input_diag_hud();
        assert!(!c.input_diag_hud_visible.load(Relaxed));
    }

    #[test]
    fn toggle_password_suppress_flips_enabled() {
        use std::sync::atomic::Ordering::Relaxed;
        let c = test_coordinator();
        assert!(c.password_suppress_enabled.load(Relaxed)); // 默认开
        c.toggle_password_suppress();
        assert!(!c.password_suppress_enabled.load(Relaxed));
    }

    #[test]
    fn focus_lost_clears_password_suppress() {
        use std::sync::atomic::Ordering::Relaxed;
        let c = test_coordinator();
        c.apply_input_diag(1234, false, 2, 1 << 31);
        assert!(
            c.password_suppress.load(Relaxed),
            "前置条件：密码框抑制应已置位"
        );
        c.handle_focus_lost(0, FocusLostReason::Thread);
        assert!(
            !c.password_suppress.load(Relaxed),
            "失焦后应清除密码框抑制态，避免残留到下次 focus_gained 之前"
        );
    }

    /// 回归（2026-07-27）：Chromium 网页密码框必须强制英文，**即便上报的 disabled=true**。
    ///
    /// 此前判据里有一条 `&& !disabled`，本意是「compartment 禁用时 DLL 已全放行、抑制 moot」。
    /// 但 DLL 放行看的是**线程级** KEYBOARD_DISABLED，而 Windows 侧当时往 `disabled` 字段传的
    /// 是 **context 级**的 `_focusIsPassword` —— 网页密码框恒为 true，于是抑制被自我否决：
    /// 键没被放行、中文照打，高级菜单的开关看着像坏了。
    ///
    /// ⚠ 本用例的要害是 `disabled=true`。改动前所有密码框用例都传 false（macOS 只发 mask、
    /// 不发 disabled，走的正是那条路），恰好绕开失效分支，所以旧代码测试全绿。
    /// **动这条判据时必须保住这个取值**，否则回归保护形同虚设。
    #[test]
    fn password_scope_suppresses_even_when_disabled_flag_set() {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        let c = Coordinator::new_headless(cfg, None);

        // disabled=true + 密码位：正是 Chromium 网页密码框改动前的上报组合。
        c.apply_input_diag(4321, true, 1, 1 << 31);
        assert!(
            c.password_suppress
                .load(std::sync::atomic::Ordering::Relaxed),
            "context 级密码框（disabled=true）必须触发强制英文抑制"
        );

        let action = c.handle_key_event(&kev(0x41 /* VK_A */, EVENT_KEY_DOWN));
        assert!(
            matches!(action, KeyAction::PassThrough),
            "密码框里字母键应强制透传为英文，实际: {:?}",
            action
        );
        assert!(
            c.state.lock().unwrap().chinese_mode,
            "抑制不应改动 chinese_mode 持久值（图标保持不变）"
        );
    }

    /// disabled 只参与 `reason_from` 的展示推导，**不参与** suppress 决策——单一来源。
    ///
    /// 本用例取代旧的 `compartment_disabled_does_not_set_suppress`：那条断言同样的输入
    /// （disabled=true + 密码位）**不该**置 suppress，把「compartment 禁用 ⇒ DLL 已放行所有键」
    /// 这条前提固化成了契约。前提只对**线程级** KEYBOARD_DISABLED 成立，而当时 Windows 侧
    /// 往该字段传的是 context 级的密码框判定 —— 契约锁住的恰是 bug 本身。reason 断言保留。
    #[test]
    fn disabled_flag_drives_reason_display_not_suppression() {
        use std::sync::atomic::Ordering::Relaxed;
        let c = test_coordinator();

        // 线程级禁用 + 密码位：reason 展示为 compartment（优先级最高），suppress 仍置位。
        // suppress=true 在此场景无害：DLL 已全放行，引擎收不到键，取值无从被观测。
        c.apply_input_diag(1, true, 1, 1 << 31);
        assert_eq!(
            c.last_input_diag.lock().unwrap().reason,
            crate::input_diag::InputDiagReason::CompartmentDisabled,
            "disabled=true 时 reason 展示应为 compartment"
        );
        assert!(
            c.password_suppress.load(Relaxed),
            "reason 的展示优先级不应反过来否决抑制决策"
        );

        // 无密码位：无论 disabled 与否都不抑制。
        c.apply_input_diag(1, true, 1, 0);
        assert!(
            !c.password_suppress.load(Relaxed),
            "mask 无密码位时不应抑制"
        );
    }

    /// 策略开关关闭后，即便命中密码位也不抑制（高级菜单的逃生阀必须真的管用）。
    #[test]
    fn disabled_switch_defeats_password_scope() {
        use std::sync::atomic::Ordering::Relaxed;
        let c = test_coordinator();
        c.toggle_password_suppress();
        assert!(
            !c.password_suppress_enabled.load(Relaxed),
            "前置条件：开关已关"
        );

        c.apply_input_diag(1, true, 1, 1 << 31);
        assert!(
            !c.password_suppress.load(Relaxed),
            "开关关闭时密码框不应强制英文"
        );
        c.apply_input_diag(1, false, 2, 1 << 63);
        assert!(
            !c.password_suppress.load(Relaxed),
            "数字密码位同样受开关约束"
        );
    }
}

#[cfg(test)]
mod hover_reset_tests {
    //! 鼠标悬停目标（`Coordinator::hover_index`）的**清空覆盖面**。
    //!
    //! 本组测试锁的是一个曾经静默存在的缺陷：悬停目标此前是 `State` 的字段，清空只能由每个
    //! 候选装填点手工执行。主路径 `update_candidates` 做了，overlay 各路径（特殊模式 / 临拼 /
    //! 临英 / 混输·快捷输入 / 拼音组合复位）全部漏了——悬停高亮与 tooltip 于是跨按键、跨组合、
    //! 跨模式存活，用户看到的是「候选窗再次弹出时，鼠标没动却已经有一项被高亮并弹出了 tooltip」。
    //!
    //! ★ 该缺陷在主路径上**物理不可观测**：普通输入每敲一键都重走 `update_candidates`，
    //! 残留被持续覆盖掉。所以只测普通输入路径等于什么都没测——下面必须逐个 overlay 入口点名。
    use super::*;

    fn coord() -> Arc<Coordinator> {
        Coordinator::new_headless(Config::default(), None)
    }

    /// 造一页候选，好让 `mouse_hover` 有合法落点（它对空候选另有分支，见下面的专项测试）。
    fn seed_candidates(c: &Coordinator, n: usize) {
        let mut st = c.state.lock().unwrap();
        st.candidates = (0..n)
            .map(|i| wind_candidate::Candidate {
                text: i.to_string(),
                ..Default::default()
            })
            .collect();
    }

    /// **反向对照**：悬停确实设得上。
    ///
    /// 少了本条，下面所有「××之后归零」都可能因为悬停压根没设上而全部假绿——本仓
    /// 「测了个恒为真的断言」已经栽过不止一次。
    #[test]
    fn mouse_hover_sets_target() {
        let c = coord();
        seed_candidates(&c, 5);
        c.mouse_hover(2);
        assert_eq!(c.hover_target(), 2, "有候选时悬停应设得上");
    }

    /// 候选窗隐藏 = 会话终结，悬停必须归零。
    ///
    /// 这是根治点：`notify_ui_hide` 有 40+ 个调用点，把清空放在这里，任何一条隐藏通路都覆盖到。
    /// （UI 侧 `CandidateMouse::reset_hover` 清的是防抖闸门，决定何时**发**事件；
    /// 高亮与 tooltip 读的是本值，两者不是一回事。）
    #[test]
    fn notify_ui_hide_clears_hover() {
        let c = coord();
        seed_candidates(&c, 5);
        c.mouse_hover(2);
        c.notify_ui_hide();
        assert_eq!(c.hover_target(), -1, "候选窗隐藏后悬停必须归零");
    }

    /// 每一个 overlay 候选装填入口，装填后都必须已清除悬停。
    ///
    /// 逐个点名而不是抽样：它们是**平行的独立落点**，历史上正是「主路径做了、其余全漏」。
    /// 新增候选来源时若忘了 `reset_candidate_view`，本测试不会自动覆盖到——但把入口逐个
    /// 列在这里，至少让「又多了一个装填点」这件事在评审时看得见。
    #[test]
    fn every_overlay_refill_clears_hover() {
        // (入口名, 调用) —— 名字进断言消息，失败时直接指出是哪条路径漏了。
        type RefillCase = (&'static str, fn(&Coordinator, &mut State));
        let cases: Vec<RefillCase> = vec![
            ("特殊模式 update_special_candidates", |c, st| {
                let _ = c.update_special_candidates(st);
            }),
            ("临时拼音 update_temp_pinyin_candidates", |c, st| {
                c.update_temp_pinyin_candidates(st)
            }),
            ("临时英文 update_temp_english_candidates", |c, st| {
                c.update_temp_english_candidates(st)
            }),
            ("混输·快捷输入 update_mix_candidates", |c, st| {
                c.update_mix_candidates(st)
            }),
            ("拼音组合复位 reset_pinyin_composition", |c, st| {
                c.reset_pinyin_composition(st)
            }),
        ];
        for (name, refill) in cases {
            let c = coord();
            seed_candidates(&c, 5);
            c.mouse_hover(2);
            assert_eq!(c.hover_target(), 2, "{name}：前置条件——悬停应已设上");

            let mut st = c.state.lock().unwrap();
            refill(&c, &mut st);
            assert_eq!(c.hover_target(), -1, "{name}：候选重新装填后悬停必须清除");
        }
    }

    /// 「鼠标移出候选窗」这条 `Hover(-1)` 在候选恰好已清空时**不能被吞掉**。
    ///
    /// 旧实现在 `mouse_hover` 开头对空候选直接 early-return，于是离开事件丢失、旧值残留。
    /// 「候选没了」正是最该归零的时刻，拿它当早退条件恰好搞反了。
    #[test]
    fn leaving_clears_hover_even_when_candidates_already_empty() {
        let c = coord();
        seed_candidates(&c, 5);
        c.mouse_hover(2);
        c.state.lock().unwrap().candidates.clear();

        c.mouse_hover(-1);
        assert_eq!(c.hover_target(), -1, "候选已空时的离开事件不能被吞掉");
    }

    /// 键盘操作（移动高亮 / 翻页）同样取消悬停：两种高亮并存时视觉上会有两个「选中项」。
    /// 此前这四处是仅有的清空点之一，改造成 `clear_hover` 后需确认语义没丢。
    #[test]
    fn keyboard_navigation_clears_hover() {
        let c = coord();
        seed_candidates(&c, 5);
        c.mouse_hover(2);
        let mut st = c.state.lock().unwrap();
        assert!(c.move_down(&mut st), "前置条件——应能下移");
        assert_eq!(c.hover_target(), -1, "键盘移动高亮后悬停应取消");
    }
}

#[cfg(test)]
mod caret_for_ui_tests {
    //! 「用于 UI 定位的光标坐标」闸门（[`Coordinator::resolve_caret_for_ui`]）。
    //!
    //! 本组测试锁的是一个曾按消费者分裂的缺陷：`state.caret_*` 里可以躺着 (0,0)
    //! （`handle_caret_update` 先写缓存、后判有效性），候选窗一直有回退闸门、状态气泡没有，
    //! 于是同一份 (0,0) 只让气泡飞到**主显示器左上角**——多显示器下表现为「气泡永远在主屏」，
    //! 而候选窗一切正常。两者现已共用本函数，测试同时钉住判据与回退。
    use super::*;

    fn coord() -> Arc<Coordinator> {
        Coordinator::new_headless(Config::default(), None)
    }

    /// ★ 负坐标是**合法**的：主屏左上角才是虚拟桌面原点，左侧/上方的副屏整块为负。
    /// 若把负数一并判为异常，副屏用户就永远取不到有效坐标 → 永远走回退 → 症状同样是「永远在主屏」。
    #[test]
    fn negative_coords_are_valid() {
        assert!(Coordinator::caret_is_valid(-1200, 500, 20), "左侧副屏");
        assert!(Coordinator::caret_is_valid(300, -600, 20), "上方副屏");
    }

    #[test]
    fn sentinel_and_degenerate_inputs_are_invalid() {
        assert!(
            !Coordinator::caret_is_valid(0, 0, 20),
            "(0,0) 是宿主「没有坐标」的哨兵，不能当成主屏左上角来用"
        );
        assert!(
            !Coordinator::caret_is_valid(500, 500, 0),
            "height=0 = 宿主尚未 reflow，整组坐标不可信"
        );
        assert!(!Coordinator::caret_is_valid(40000, 500, 20), "越界脏数据");
    }

    /// ★ 修复核心：无效坐标必须回退到最近一次有效坐标，且该坐标可以在副屏（负值）。
    /// 原样交给 UI 就是「气泡跳到主显示器左上角」。
    #[test]
    fn invalid_falls_back_to_last_valid_on_secondary_monitor() {
        let c = coord();
        assert_eq!(
            c.resolve_caret_for_ui(-1500, 400, 20),
            (-1500, 400, 20, true),
            "前置条件——副屏坐标应被认为有效并记为最近有效值"
        );
        assert_eq!(
            c.resolve_caret_for_ui(0, 0, 20),
            (-1500, 400, 20, true),
            "(0,0) 必须回退到副屏那条，而不是留在主屏原点"
        );
    }

    /// 尚无任何历史有效坐标时如实报 `valid=false`，由调用方决定临时显示 / 待重定位。
    #[test]
    fn no_history_reports_invalid() {
        let c = coord();
        assert_eq!(c.resolve_caret_for_ui(0, 0, 20), (0, 0, 20, false));
    }
}
