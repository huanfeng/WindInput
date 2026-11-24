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

use crate::comment::VarValue;
use crate::handle_mode::MixLens;
use crate::pipeline::{ModeKind, Rewind};
// 子模块（src/coordinator/ 目录）：这批切片重度访问本模块**私有**字段/函数，
// 子模块对父私有项可见，平级模块则须放开可见性——归属判据即「是否需要碰私有态」。
mod app_schema;
mod ctrl_space;
mod first_show;
pub(crate) mod fullscreen_watch;
mod langbar_icon;
mod message_handler;
mod push_config;
mod state_writer;
mod status_placement;
mod window_ctx;
pub(crate) use window_ctx::{FocusWindow, ModeScope, entry_window_key, focus_crossed};
// 单测不碰真实用户目录（`build_status` 每次都会走到它）。
#[cfg(all(target_os = "linux", ext_presenter, not(test)))]
mod tray_icon;

// 平移到子模块的项以原路径保真（handle_* 均经 `crate::coordinator::` 引用，勿改回直连）。
pub(crate) use crate::config_bundle::{ConfigBundle, schema_key_union};
pub(crate) use crate::key_convert::{
    char_to_main_vk, full_width_source_char, numpad_char, numpad_to_main, printable_char,
    punct_char, wind_mods_to_win32,
};
use crate::preedit_cursor;
use crate::theme_style::ThemeStyle;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, trace, warn};
use wind_keys::keymap;

/// CapsLock 作中英切换键时，按住超过这么久（毫秒）松开算长按：开关大写锁定而不切中英。
const CAPS_TOGGLE_LONG_PRESS_MS: u32 = 500;

use wind_bridge::handler::*;
use wind_bridge::push::PushServer;
use wind_candidate::{Candidate, CandidateSource};
use wind_config::Config;
use wind_config::Orientation;
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
use wind_ui_types::{GlobalHotkeyEntry, UiCommand, UiEvent};
use wind_ui_types::{ToastKind, ToastPosition};
use wind_ui_types::{ToolbarItem, ToolbarState};

/// caret_use_top 兼容下保留给「上方显示」避让正文的最小行高（物理像素——宿主上报的
/// caret rect 本就是物理像素，此处刻意不做 dp 换算，与 `caret_offset_*` 不是同一件事：
/// 那是用户配置的校正量，这是拿宿主自己上报的物理量兜底，两者单位巧合都叫「像素」但
/// 出处不同）。微信 reflow 后的权威帧通常上报真实行高（~20px，随 DPI 缩放），直接取用；
/// 仅退化帧（height=1）落到此下限，保证上方候选窗底边抬到正文之上而不遮挡。偏大只是
/// 多留空隙，故取一个稳妥的正文行高量级。
const CARET_USE_TOP_MIN_LINE_H: i32 = 18;

/// direct_commit 顶码余码新组合的 keyup 兜底定时器时长（ms）。见 top-commit-mode 设计文档 §5。
pub(crate) const DEFERRED_COMPOSITION_FALLBACK_MS: u32 = 150;

/// 「正在建立词库索引…」提示延后多久才弹（见 `Coordinator::spawn_index_warm`）。
///
/// 索引自 2026-08-24 起落盘为 `.wridx`，于是同一件事有两种量级完全不同的结果：
/// **复用磁盘缓存约几十毫秒**（feihuzj2 251 万词实测 47.8ms），冷建才是秒级。
/// 无条件先弹提示就等于给一次根本不存在的等待配了个说明，用户看到的是
/// 「明明该直接 mmap，怎么还在建索引」。
///
/// 判据刻意选「**慢不慢**」而不是「**要不要做事**」：慢不慢不需要预测，到点还没干完
/// 就是真慢。预测式的做法（先问缓存新不新鲜）得在协调器里复刻一份
/// `build_reverse_index_for` 的判定，那正是本仓反复吃亏的「同一份推导写两处」。
///
/// 取值：明显长于复用（几十毫秒）、又明显短于冷建（秒级），且落在「用户开始觉得卡」
/// 的门槛附近。
const INDEX_TOAST_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// toast 默认显示时长。`ui.toast(ms=…)` 与配置语法错误那条（8s）各自覆盖。
pub(crate) const DEFAULT_TOAST_MS: u64 = 2500;

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

// 英文系统词的名次权重须低于临时词初始权重：否则加了词频名次后，自动造的英文临时词会
// 沉到常用系统词之后（名次引入前系统词全是 0）。见 `wind_dict::freq_rank`。
const _: () = assert!(wind_dict::freq_rank::RANK_WEIGHT_MAX < LEARN_ADD_WEIGHT);
// 晋升进用户库的权重同理（用户层另有托底，见 `StoreUserLayer::with_min_weight`）。
const _: () =
    assert!(wind_dict::freq_rank::RANK_WEIGHT_MAX < wind_store::temp_words::PROMOTED_WEIGHT);

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

/// 尚未从宿主观测到可靠行高时的保守下限，单位 px。见 [`Coordinator::last_sane_caret_height`]。
///
/// 只用于两个**容差**计算（settle 的偏差吸收、组合起点重锁阈值），不参与任何定位。取值比
/// 常见正文行高（20~55px）小，但足够让容差脱离 3px 那个下限——偏小会让容差偏紧（多校正
/// 几次，观感上多跳一下），偏大只会多吸收几像素微移，两个方向都不致命；而塌到 1px 会让
/// 两个消费点同时失效，那是已经踩过的故障。
pub(crate) const FALLBACK_LINE_HEIGHT: i32 = 16;

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

/// 单字输入开 / 关的状态泡文案。
///
/// ⚠️ 与 [`FILTER_MODES`] 是两件事：那三档是检索范围（按字符常用度），本项是单字输入
/// （按候选长度），两根轴正交、可同时生效。并进同一个菜单会让用户以为它们互斥。
const SINGLE_CHAR_TIPS: (&str, &str) = ("单字输入 开", "单字输入 关");

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
#[cfg(not(ext_presenter))]
pub(crate) fn settings_url() -> Option<String> {
    SETTINGS_URL_PROVIDER.get().and_then(|f| f())
}

/// 取同目录下 wind_setting 设置应用的可执行路径（None=不存在）。
/// 由当前 exe 名推导变体：wind_input[_dev].exe → wind_setting[_dev].exe，
/// 故无需感知编译期变体，正式/dev 版自动对应。
/// macOS 经 CmdOpenSettings(0x0507) 由 .app 按 bundleID 启动设置应用，不需可执行路径，故仅非 macOS。
#[cfg(not(ext_presenter))]
pub(crate) fn settings_app_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let stem = exe.file_stem()?.to_str()?; // wind_input 或 wind_input_dev
    let setting = stem.replacen("wind_input", "wind_setting", 1);
    let path = dir.join(format!("{setting}.exe"));
    path.exists().then(|| path.display().to_string())
}

/// 组合区「逐步转换」已确认的一段（拼音专属；码表选词消费整串、绝不进入此态）。
///
/// 此前是个五元组，`(raw_code, _, _, _, _)` 这样的解构在文件里到处都是；加第六个成员
/// （`learn`）之后可读性撑不住，改成具名结构体。字段语义与旧元组逐位一一对应。
#[derive(Debug, Clone)]
pub(crate) struct CommittedSeg {
    /// **原始输入空间**的消费码（双拼下是击键 `hc`）。退格回退（`pop_*_seg`）把它并回
    /// 输入缓冲，故必须与缓冲同域。
    pub(crate) raw_code: String,
    /// **全拼语义**码（`hao`）。词频记账用它，`boundary` 的位移量按它的 `len()` 算。
    pub(crate) code: String,
    pub(crate) text: String,
    pub(crate) source: CandidateSource,
    /// 该段 `code` 的音节边界。段自身可能是多音节整词（选「你好」→ 段码 nihao、
    /// 段内边界 ni|hao），故自动造词拼接各段时须把段内边界平移到全局位置。
    pub(crate) boundary: u64,
    /// **造词专用**的规范词条码与其边界；`None` = 就用 `code`/`boundary`。
    ///
    /// 只有模糊音命中会让两者分家（用户敲 `senri`、词典里是 `shengri`）。`code` 必须留
    /// 用户那份——它绑着 `consumed_length` 的 `starts_with` 判据、preedit 跟随与词频记账；
    /// 而写进词库的码得是用户下次真能打出来的。来源见
    /// `wind_candidate::CandidateMeta::learn_code`。
    pub(crate) learn: Option<(String, u64)>,
}

/// 临拼 / mix 文本透镜从引擎结果取的编码栏形态（见 `State::overlay_forms`），各字段同主路
/// `preedit_split_body` / `preedit_fp_body` / `preedit_abbrev_body`。空串 = 无此形态。
#[derive(Debug, Clone, Default)]
pub(crate) struct OverlayPreeditForms {
    pub(crate) split: String,
    pub(crate) fullpinyin: String,
    pub(crate) abbrev: String,
}

impl OverlayPreeditForms {
    /// 默认形态取 `preedit_pinyin`（双拼下是击键切法，首选是简拼时也不变），缺了才用
    /// `preedit_display`——同主路，高亮从简拼候选移回时要取得回双拼切法。
    pub(crate) fn of(result: &wind_engine::ConvertResult) -> Self {
        let split = if result.preedit_pinyin.is_empty() {
            &result.preedit_display
        } else {
            &result.preedit_pinyin
        };
        Self {
            split: split.clone(),
            fullpinyin: result.preedit_fullpinyin.clone(),
            abbrev: result.preedit_abbrev.clone(),
        }
    }
}

/// Alt 键自按下起的追踪：按下时刻与「期间是否按过别的键」。
pub(crate) struct AltTrack {
    pub down_at: std::time::Instant,
    pub dirty: bool,
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
    /// 插入点前一字符的类别——中英/中数间补空格与英文后智能标点的「上一笔」。
    ///
    /// 两个写入点（见 `crate::auto_spacing`）：无输入会话时的 keydown 按客户端 `seq_char`
    /// 重新锁存；服务端每发出一笔上屏就改成那笔的末字符。组合中的后续按键不改它——组字键
    /// 没写进文档，真正的「前一字符」是组合开始时那个。
    pub(crate) spacing_prev: wind_punct::spacing::CharClass,
    /// 方案级行为覆盖已落地到的**活跃方案代际**（`EngineManager::schema_generation`）。
    ///
    /// 见 `docs/design/schema-scoped-behavior.md` §4。初值是哨兵 `u64::MAX`，保证首次
    /// `sync_schema_scope` 必然执行一次——用 0 的话它恰好等于代际初值，启动时方案声明的
    /// `[punct]` 就永远落不下去（且完全静默）。
    pub(crate) schema_scope_gen: u64,
    /// 被方案级 `[punct]` 覆盖**之前**的标点态。`None` = 当前没有方案在覆盖它。
    ///
    /// # 为什么标点需要它而布局不需要
    ///
    /// 布局的基线是 `candidate_vertical` 镜像，方案意图不写它，`Follow` 每次重算时自然
    /// 回落到基线。标点没有这样一层：`state.chinese_punct` 既是当前值又是唯一存储，
    /// 被英文方案覆盖成 `false` 之后，切回一个 `Follow`（= 不干预）的方案时就**没有可回落
    /// 的原值**——真机现象是「从五笔切到英文，标点变英文；切回五笔，标点还是英文」。
    ///
    /// # 这不是被否决的那种「保存 / 回放」
    ///
    /// 被否决的是「进入模式时保存、在 8 个清空点各写一遍恢复」那种形态。这里保存与恢复
    /// **都在 `sync_schema_scope` 一个函数里**，由代际驱动、幂等、代际不变就不动，
    /// 没有任何分散的恢复点可漏。
    pub(crate) punct_before_schema: Option<bool>,
    /// 候选布局的**本代际手动覆盖**（`Some(true)` = 用户在本方案期间手动切成了竖排）。
    ///
    /// 只在方案声明了非 `Follow` 的 `[candidate] layout` 时才会被写入——方案没意见时
    /// 命令栏切换照旧改全局基线并写盘（那才是用户要的「全局改方向」）。
    /// 随 `schema_scope_gen` 变化清空：切走再切回，回到方案意图。
    pub(crate) layout_manual: Option<bool>,
    /// 简繁转换开关（运行时切换；commit 时把简体输出转繁体）
    pub(crate) s2t_enabled: bool,
    /// 繁简转换开关（运行时切换；commit 时把繁体输出转简体）。给「词库是繁体、
    /// 想打繁体出简体」的用户。与 [`Self::s2t_enabled`] **互斥**，见
    /// `Coordinator::toggle_conversion_direction`。
    pub(crate) t2s_enabled: bool,
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
    /// 单字输入的**运行时临时态**（热键 / 工具栏 / 托盘切换的结果）。
    ///
    /// `None` = 用户本次没切过，走配置层（码表：方案级 `[engine.codetable] single_char`
    /// → 全局 `schema.codetable.single_char`；拼音：全局 `schema.pinyin.single_char` 一份）。
    /// 取值收口在 [`Coordinator::effective_single_char`]。
    ///
    /// # 只在内存，绝不写配置（2026-09-07 用户拍板）
    ///
    /// 与 `scope_relaxed` 同族、与 `set_filter_mode` 相反：后者切一次就写回
    /// `input.filter_mode`。这里不写盘的直接好处是**不必碰 `schema_overrides/{id}.toml`
    /// 那条写盘链**（本项是方案级配置，持久化就得往那儿写，还要登记 `SideCommitter`
    /// 并处理「设置页正开着时被后台改写」）。
    ///
    /// # ★ 它压过**配置**的一切层级，但压不过引擎不变量
    ///
    /// 配置层按当前引擎分流（码表/混输一份、拼音一份，见
    /// [`wind_config::schema::CodeTableSpec::single_char`]）；本字段是「用户此刻的意图」
    /// ——他刚按下热键，就该立刻生效，不该再问一遍这个码归哪个方案的数据管。
    ///
    /// **唯一压得过它的是「英文引擎恒 `All`」**：那是不变量不是默认值（`Char` 档会把
    /// 英文候选全滤光），故那道判据问在本字段之前。见 `effective_single_char`。
    ///
    /// # 失效点：切方案时清空
    ///
    /// 收口在 `sync_schema_scope`（代际驱动，与 `punct_before_schema` 同批）——它是本仓
    /// 唯一能覆盖全部五条切方案路径的地方，命令式地在切方案处逐个清必然漏接。
    /// ⚠️ 与 `scope_relaxed` 的失效点**刻意不同**：那个按「缓冲清空」失效（一次组合内
    /// 的探索），本项要跨组合保持到用户切走方案为止。
    pub(crate) single_char_override: Option<bool>,
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
    /// **本次按键**是否来自小键盘（原始键码在 `numpad_to_main` 覆盖内）。
    ///
    /// 存在的唯一理由是 `numpad_behavior = follow_main` 会在 `handle_key_event` 入口
    /// 把 `VK_NUMPAD2` **改写成** `VK_2`，此后全部下游再也分不出这键来自哪块键盘，
    /// 而 `input.numpad_half_width` 恰恰要在**出字那一步**知道来源。
    ///
    /// ⚠️ 每次 `handle_key_event` 拿到锁后**无条件写入**（不是「只在为真时置位」），
    /// 否则会留下上一次按键的陈旧值。读它的地方只有出字点，见
    /// [`Coordinator::numpad_raw_output`]。
    pub(crate) numpad_origin: bool,
    pub(crate) input_buffer: String,
    /// `input_buffer` 的「原始大小写」影子串：用户按 Shift+字母打出的大写只存在这里。
    /// 空 = 没有大写；与缓冲失配同样视为没有大写（见 `preedit_cursor::cased_is_valid`）。
    ///
    /// **缓冲本身恒为全小写**——引擎查询、顶码判定、词频记账、加词取码一律按它，大小写对
    /// 匹配零影响。本字段只出现在两个出口：组合区显示，以及「上屏原码」（回车/空格空码/
    /// 标点顶屏）。读写走 `preedit_cursor::BufEdit::new_cased`，勿裸改。
    pub(crate) input_buffer_cased: String,
    /// 英文候选的大小写档位（CapsLock 在英文输入态循环切换，见
    /// `input.capslock.english_case_cycle`）。**一次组合结束即复位**。
    ///
    /// ⚠️ 复位点与 `input_buffer_cased` 的清空点是同一批（组合结束的四条路）——
    /// 新增任何一个清空点都要同步复位它，否则「上一个词按出来的全大写档」会串到下一个词。
    pub(crate) english_case_variant: crate::english_candidates::CaseVariant,
    /// 编码区光标：`input_buffer` 内的字节偏移，定义域 `[0, input_buffer.len()]`。
    /// 恒指向剩余编码内部——已转换前缀（`committed_text`）是只读前缀，光标进不去（Home 只到
    /// 剩余编码开头）。光标**不参与引擎查询**：`update_candidates` 恒查整串，移动光标不重算
    /// 候选（对齐 Go `inputCursorPos`）。所有读写走 `preedit_cursor::BufEdit`，勿裸改。
    pub(crate) input_cursor_pos: usize,
    /// 手机端展开页「首单元读法」的当前选择（见 [`crate::leading_reading`]）：只对**记录它时的那份
    /// 缓冲**有效——缓冲一变（打字、退格、选词吃掉前缀）就自动失效，不用在十几处清缓冲的地方逐个复位。
    pub(crate) leading: Option<crate::leading_reading::Applied>,
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
    /// **码表整句**的编码单元切分形态（`aawtaawt` → `aawt'aawt`）。
    /// 高亮到码表整句候选时 preedit 用它；其余情形不读。
    /// 空串 = 本次没有整句解（或方案未开整句）。每次 build_candidates 重置。
    pub(crate) preedit_codetable_body: String,
    /// **直接辅助码**命中时的组码区形态：前缀的双拼音节分段 + 空格 + 辅码（`ui'du p`）。
    /// 高亮到 `is_direct_aux` 的候选时 preedit 用它，让人看出末尾被当成了辅码；其余情形不读。
    /// 空串 = 本键没有直接辅助命中。每次 build_candidates 重置（见 `apply_direct_aux`）。
    pub(crate) direct_aux_body: String,
    /// 直接辅助码：上一个整音节输入的主候选快照，供后面的键当前缀候选（见 `DirectAuxPrev`）。
    pub(crate) direct_aux_prev: Option<crate::handle_direct_aux::DirectAuxPrev>,
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
    /// 整句 N-best 的池子（本次转换的全部整句，按名次、已去重），供整句切换键滚动窗口。
    ///
    /// 来自 `ConvertResult::sentence_pool`，**只在 `schema.pinyin.sentence_max_count > 1` 时
    /// 非空**。每次 `update_candidates` 清空重填：池子只对「当前这串输入」有效，输入一变
    /// 旧池子里的整句就不再是这串键的解读了。
    pub(crate) sentence_pool: Vec<Candidate>,
    /// 整句切换键把显示窗口在池子里滚到了第几条（0 = 原始名次，即解码最优解打头）。
    /// 与 `sentence_pool` 同生命周期，随它一起清零。
    pub(crate) sentence_window: usize,
    /// 窗口为 0 时的候选列表快照，第一次切换时拍下；每次切换从它重建（见
    /// `cycle_sentence_window`）。与 `sentence_pool` 同生命周期。
    pub(crate) sentence_base: Vec<Candidate>,
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
    /// 用户**显式选中**的 `$SS`/`$AA` 组：`(完整码, 组源文本)`。
    ///
    /// 精确码下有重码时组默认折叠成一条组名候选（见 `resolve_group_expansion`），用户选中它
    /// 才展开成员；选中动作把输入补全到完整码并重查，重查时靠这条记录知道「这次要展开」，
    /// 否则会又折叠回去、永远进不了二级选择。失效与 `shortcode_tops` 同理——`build_candidates`
    /// 开头按「输入码已不是记录里的码」统一淘汰，不逐个接线。
    pub(crate) expanded_group: Option<(String, String)>,
    pub(crate) candidates: Vec<Candidate>,
    /// 当前页内高亮候选下标（0-based，相对当前页）——键盘选中项，空格上屏的目标
    pub(crate) selected_index: usize,
    /// 当前页码（0-based）
    pub(crate) current_page: usize,
    /// 这批候选被翻过页没有（`current_page` 的**历史**，不是它的现值）。
    ///
    /// 唯一消费者是 `Coordinator::symbol_buffer_key_free`：未翻过页时「上一页」是一格空转，
    /// 把 `-` 让给字符输入（打 `sun-panel`）；翻过页之后 `-` 恢复翻页身份。判据取历史而非
    /// `current_page == 0`，是因为从第 2 页翻回第 1 页的用户**正在翻页**，此刻 `-` 突然
    /// 变成字符会让人当场打错字。
    ///
    /// 与 `current_page` 同生命周期：置位在 `page_prev` / `page_next` 真的翻动那一帧，
    /// 清零在 [`Coordinator::reset_candidate_view`]（候选重新装填 = 换了一批候选，
    /// 上一批的翻页史与新的这批无关）。**别在别处清**——`current_page` 归零的地方就是
    /// 它该归零的地方，两者分家就会漂移。
    pub(crate) paged: bool,
    /// 候选展开态（多行网格）的可见区首行：`Some(p)` = 已展开，第 `p` 页在可见区第一行。
    ///
    /// 网格的**一行就是一页**，高亮行就是 `current_page`。于是页模型的一切——数字键、空格、
    /// 扩容、末页放宽——原样适用，这里只多记一个滚动位置。设计见
    /// `docs/design/candidate-grid-expand.md`。
    ///
    /// 清零在 [`Coordinator::reset_candidate_view`]：候选重新装填（打字、退格、分步上屏后续查）
    /// 即收起，与 `paged` 同一个时机、同一个理由；本次组合展开过的话，装填完再由
    /// [`Self::grid_keep`] 展开回来。
    pub(crate) grid_top: Option<usize>,
    /// 本次组合展开过：候选重新装填后（继续打字、分步上屏）自动再展开，直到 Esc 收起或组合结束。
    ///
    /// 组合结束的出口有几十处，不逐个清：每次按键开头若已没有候选，就说明是一次新组合，在
    /// `handle_key_event` 开头清掉。
    pub(crate) grid_keep: bool,
    /// 注释态的「仅本次候选窗」翻转：与持久态（`ui.comment.state`）异或得当前态。
    /// 候选窗关闭或被新组合替换即清：`cancel_session`、进出联想、顶码余码、顶屏进模式、`handle_key_event` 开头无会话时。
    pub(crate) comment_session_flip: bool,
    /// 注释态的「按住切换键」翻转：按住期间为真，与上面那份各自独立地异或。
    pub(crate) comment_hold_flip: bool,
    /// Alt 单击 / 按住的判定中间态：`Some` = Alt 已按下、尚未松开。
    pub(crate) alt_track: Option<AltTrack>,
    /// 如实上报的 Ctrl（`EVENT_MOD_DOWN`）单击判定中间态：`Some` = 已按下、尚未松开。
    pub(crate) ctrl_track: Option<crate::comment_switch::CtrlTrack>,
    /// 展开态当前的分类标签（P3）。非 `All` 时 `candidates` 是**筛过的子集**，完整列表暂存在
    /// [`Self::grid_full`]——下标在几十处调用里按 `candidates` 解释，就地换成子集，选词、序号、
    /// 右键、置顶删除就都不用知道筛选的存在。重建候选的几处（扩容、末页放宽）之后要重新套一次
    /// 筛选，见 `Coordinator::grid_reapply_filter`。
    ///
    /// 与 `grid_top` 同一个清零点（`reset_candidate_view`）。
    pub(crate) grid_tab: wind_ui_types::GridTab,
    /// 筛选期间的完整候选列表；`grid_tab == All` 时为空。
    pub(crate) grid_full: Vec<Candidate>,
    /// 连续切分类时的锚点，见 [`crate::candidate_grid::GridTabAnchor`]。
    pub(crate) grid_tab_anchor: Option<crate::candidate_grid::GridTabAnchor>,
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
    pub(crate) committed_segs: Vec<CommittedSeg>,
    /// 当前激活的独占输入模式（临时拼音/快捷输入/临时英文）。`None` = 普通输入。
    /// 单点决策的唯一真相源：结构上保证同一时刻至多一个独占模式（见 `pipeline.rs`）。
    pub(crate) active: Option<ModeKind>,
    /// 各 overlay 模式组合区显示主体（= preedit 去掉只读前缀的部分），供光标位置换算。
    /// 仅临拼 / mix 需要维护——它们的主体是引擎 `preedit_display`（含插入的音节分隔符），
    /// 与缓冲不同形；临英 / 特殊 / URL 的主体恒等于自身缓冲，直接用缓冲即可（见
    /// `overlay_caret_parts`）。缓冲空时可能为 stale，但此时光标必为 0、换算不读它，无害。
    pub(crate) overlay_body: String,
    /// 临拼 / mix 文本透镜的编码栏各形态，供高亮跟随（主路那几个 `preedit_*_body` 的 overlay
    /// 对位，生命周期跟各自的候选刷新走）。`overlay_body` 是按高亮从中选出的那一份。
    pub(crate) overlay_forms: OverlayPreeditForms,
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
    /// 邮箱模式输入缓冲（**含 `@` 与用户名**，如 `abc@gmail.com`）。
    ///
    /// 连用户名一起存是刻意的，同 [`Self::unicode_buffer`] 连前缀一起存：编码区要把
    /// 用户打的整串原样显示出来，且退格退到只剩 `abc@` 时才是夺取边界，
    /// `Rewind::host_text` 拿它直接比对。拆后缀是候选查询时的事（`email_suffix_part`）。
    pub(crate) email_buffer: String,
    /// 邮箱模式编码区光标（`email_buffer` 内字节偏移）
    pub(crate) email_cursor: usize,
    /// Unicode 模式输入缓冲（**含触发前缀**，如 `u+4e00`）。
    ///
    /// 连前缀一起存是刻意的：编码区要把用户打的整串原样显示出来（`U+` 起手时显示的
    /// 就该是 `U+`），且退格退到只剩前缀时才是「夺取边界」，`Rewind::host_text`
    /// 拿它直接比对。码点解析时再把前缀剥掉（`unicode_hex_part`）。
    pub(crate) unicode_buffer: String,
    /// Unicode 模式编码区光标（`unicode_buffer` 内字节偏移）
    pub(crate) unicode_cursor: usize,
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
    /// 快照而非每次查注册表，两个理由：
    /// 1. 布局取值在候选更新路径上，省掉每次的整表 clone；
    /// 2. ★ 注册表按 id 排序，装一个新 overlay 方案会让其后方案的下标平移——快照让
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
    /// Linux 自绘菜单最近一次被操作（打开 / 菜单键 / 指针事件）的时刻，空闲超时兜底用
    /// （见 `forward_menu_key`）。与 `menu_opened_at` 分开：后者是焦点守卫的「刚打开」判据，
    /// 随操作刷新会把守卫期无限延长。
    #[cfg(ext_presenter)]
    pub(crate) menu_touched_at: Option<std::time::Instant>,
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
    /// 加词的**剪贴板字符池**，惰性读取：
    /// `None` = 本轮还没读过，`Some(空)` = 读过、剪贴板没有可用内容。
    ///
    /// ⚠️ 「还没读」与「读了是空」必须分得开，否则每次问「剪贴板有没有东西」都会再去读
    /// 一次系统剪贴板——那正是本字段改成 `Option` 要避免的（读一次最坏 40ms，见
    /// `clipboard_add_word_chars`）。
    pub(crate) add_word_clip: Option<Vec<char>>,
    /// 当前生效的字符池来源：false = 最近上屏（默认），true = 剪贴板。
    /// 两个池子的**裁剪方向相反**，读取一律走 `add_word_pool` / `add_word_current_word`。
    pub(crate) add_word_from_clip: bool,
}

impl State {
    /// 工具栏显示的**三项合取**：本输入法在服务某宿主（`ime_active`）、焦点在可编辑控件里
    /// （`has_edit_context`）、用户开着工具栏（`toolbar_visible`）。三项正交、缺一不可，
    /// 各自的失效形态见字段注释。
    ///
    /// 单独成函数是因为有**两个**判定点：`notify_toolbar` 的可见性决策，与全屏复查线程的
    /// 开销闸（见 `coordinator/fullscreen_watch.rs`）。后者必须与前者同源——闸门比判据宽
    /// 只是白探测，比判据窄则会漏掉「工具栏正显示着、全屏却没人复查」这一格，而那正是
    /// 复查线程唯一要服务的场景。
    ///
    /// ⚠ 全屏否决**不在**本函数里：那是另一层（配置开关 × 探测缓存），且复查线程恰恰要在
    /// 它为真、工具栏已因此隐藏时继续工作，否则退出全屏后没人把工具栏放回来。
    pub(crate) fn toolbar_conjunction(&self) -> bool {
        self.ime_active && self.has_edit_context && self.toolbar_visible
    }
}

#[cfg(test)]
thread_local! {
    /// 测试钩子：本线程上 `notify_ui_update` 为悬停「编码」段调 `word_codes_display` 的次数
    /// （`ui.tooltip.enabled = false` 时应为 0）。线程局部：并行测试互不干扰。
    ///
    /// ⚠️ 只覆盖**同步**调用路径：别的线程（后台预热、异步刷新等）上的调用计在那个线程的
    /// 副本里，这里看不见。计数为 0 只证明调用线程上的候选刷新没查。
    pub(crate) static WORD_CODE_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 配置变更后**引擎是否需要重建**（`reload_user_config` 的 `schema_dirty`）。
///
/// 抽成自由函数而非内联表达式，就是为了**能被单测直接喂两份配置**——它守的那类缺陷
/// （某个键漏进判据）在端到端层面只表现为「设置页改了没反应、重启就好」，那是本仓
/// 反复出现、且最难从现象定位回判据的一类。
///
/// # 收什么
///
/// - `schema` 整段：已含全局 codetable / pinyin / mix（上屏策略、调频等）。
/// - `input.temp_pinyin`、`input.temp_english`：两者都在 `input` 段却被引擎按需缓存，
///   [`wind_engine::EngineManager`] 各持一份镜像，只在构造与 `reload_from_config` 里写。
/// - `input.reverse.lookup_disabled_dicts`：引擎据它挂不挂影子层（未启用扩展库，只给反查模式），
///   镜像在 `EngineManager`；同段的 `candidate_layout` 只给协调器读，**不收**（收了每次改布局都丢词典缓存）。
///
/// # ⚠️ 新增「协调器实时读、引擎走镜像」的开关时必须同步这里
///
/// `input.temp_english.phrase_seg` 就栽过：协调器谓词
/// [`Coordinator::temp_english_phrase_separator_key`] 每次按键现读 `rt().config`，开关一开
/// `'` 立刻进临英缓冲；而引擎镜像不标脏就永远是旧值 ⇒ `seg_sep` 仍是 `None`、分词候选
/// 恒空，原路径拿 `ip'pro` 去查 code 前缀也必然落空 ⇒ **候选塌成只剩原文，直到重启**。
///
/// 判据是「这个键有没有第二个消费者在引擎侧」，不是「它在不在 schema 段」。
/// # 不收什么
///
/// - `input.correction`：引擎侧 `EngineManager::sync_correction` 自己判要不要重建（只改提示开关的
///   不重建），结果在 `reload_user_config` 里并进 `schema_dirty`。
/// - `schema.keep_all_loaded`：方案常驻策略由 `apply_residency_change` 就地对齐（摘除 /
///   后台预热），引擎侧没有它的镜像；收进来会让「切换常驻开关」把全部引擎丢掉重建。
/// - `schema.dict_cache_policy`：只在服务启动时读一次（改了要重启才生效），重建引擎也换不了策略。
pub(crate) fn engine_reload_needed(
    old: &Config,
    new: &Config,
    overlay: wind_config::ConfigOverlay,
) -> bool {
    let schema_changed = {
        let mut old_schema = old.schema.clone();
        old_schema.keep_all_loaded = new.schema.keep_all_loaded;
        old_schema.dict_cache_policy = new.schema.dict_cache_policy.clone();
        old_schema != new.schema
    };
    schema_changed
        || old.input.temp_pinyin != new.input.temp_pinyin
        || old.input.temp_english != new.input.temp_english
        || old.input.reverse.lookup_disabled_dicts != new.input.reverse.lookup_disabled_dicts
        // 引擎的拼音全局配置是 `pinyin_effective(overlay)` 的快照，只在重建时重算。
        // 移动档下语法取 `[mobile.grammar]`，它不在 `schema` 里——只改它时上面几项都不变，
        // 引擎就停在旧值。Base 档的有效值就是 `schema.pinyin`，已被 schema_changed 覆盖。
        || old.pinyin_effective(overlay) != new.pinyin_effective(overlay)
}

/// 空码时按标点/符号键怎么处置这串废码（`input.punct_on_empty_behavior` 的解释结果）。
///
/// ★ 这一族配置描述的行为有**两根轴**——「废码上不上屏」与「按键字符本身出不出」——而配置
/// 值域只有一维，是两轴的合法组合枚举：
///
/// | | 键字符输出 | 键字符吞掉 |
/// |---|---|---|
/// | 废码上屏 | [`Self::Commit`] | （无意义，不设值） |
/// | 废码丢弃 | [`Self::Clear`] | [`Self::ClearNoInput`] |
///
/// 回车/空格的 `clear` 落在**吞键**那一格，标点的 `clear` 落在**出键**那一格——同一字面值在
/// 第二根轴上取值相反，刻意如此：标点是用户真正想输入的可见字符，吞掉等于吞掉用户意图。
///
/// 唯一解释器是 [`Coordinator::punct_empty_code_policy`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PunctEmptyCodePolicy {
    /// 上屏原码，标点跟在其后照常上屏。非空码时也是这一态（本开关不管有候选的情形）。
    Commit,
    /// 丢弃废码（连同已转换前缀），标点照常上屏。出厂值。
    Clear,
    /// 丢弃废码，标点本身也不上屏——整个按键当没按过。
    ///
    /// ⚠️ 标点不上屏 ⇒ **没有可 hold 的对象**。这一态必须在智能符号 `hold_info` 之前短路，
    /// 否则会挂一个屏幕上并不存在的 hold 态，下一次同键 press2 会去删一个从未上屏的符号。
    ClearNoInput,
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
/// 用命名结构体而非元组：多个 bool 语义完全不同，元组下标在调用点无从分辨——本仓已有
/// 多次「下标/名字与实际语义脱节」的返工。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ActiveCompat {
    /// 已解析的焦点进程 PID（0 = 尚未解析，此时其余字段无意义）。
    pub(crate) pid: u32,
    /// 用 caret rect 的 top 而非 bottom 定位候选窗。微信等 WebView 宿主的 GetTextExt
    /// height 在 1↔20px 间跳变致 bottom 漂移，top 稳定。
    pub(crate) caret_use_top: bool,
    /// 见 [`wind_config::app_compat::AppCompatRule::stale_probe_guard`]：
    /// 该宿主组合期间上报的 caret rect 会停在上一次组合的位置，需拦截。
    pub(crate) stale_probe_guard: bool,
    /// 见 [`wind_config::app_compat::AppCompatRule::composition_start_pair_guard`]：
    /// 该宿主会把组合起点降级帧与当前 selection 成对上报，后者不得触发大偏移重锁。
    pub(crate) composition_start_pair_guard: bool,
    /// 见 [`wind_config::app_compat::AppCompatRule::pin_anchor_when_start_drifts`]：
    /// 该宿主报的组合起点跟着插入点漂移，锚点须钉在首帧、不得由逃生阀跟着推。
    pub(crate) pin_anchor_when_start_drifts: bool,
    /// 候选窗首显策略（见 `AppCompatRule::first_show_mode`）。三档互斥；
    /// `None` = 跟随全局 `ui.candidate.first_show_mode`。
    ///
    /// ⚠ **刻意保留 `Option` 而不在这里就地回落到全局值**：这是一份随焦点切换刷新的
    /// 镜像态，若在写入时就把全局默认烘进来，用户在设置页改了全局档位后要等到下次切
    /// 焦点才生效（本仓已有一类「设置页改了不生效、重启后生效」的缺陷都源于此）。
    /// 回落统一放在读取侧的 [`Coordinator::effective_first_show_mode`]。
    pub(crate) first_show_mode: Option<wind_config::app_compat::FirstShowMode>,
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
    /// 本进程的中英 / 中数间自动补空格；`None` = 跟随全局 `input.spacing.*`，`Some(false)` 关闭。
    /// 读取见 `Coordinator::auto_spacing_allowed_here`。
    pub(crate) auto_spacing: Option<bool>,
    /// 光标坐标校正偏移（dp，96dpi 基准逻辑像素，正=右/下）。宿主报告的 caret 系统性偏移时用，
    /// 与 `caret_use_top` 在同两处消费（`apply_focus_caret` / `handle_caret_update`）。
    /// 应用时按目标点所在显示器的 DPI 换算成物理像素，见 [`Coordinator::apply_caret_compat`]。
    pub(crate) caret_offset_x: i32,
    pub(crate) caret_offset_y: i32,
    /// 规则是按哪个窗口解析的：[`FocusWindow::cache_hash`]（类名小写 + 标题）。与 `pid` 一起构成
    /// 缓存键——同进程换了窗口（类名 / 标题不同）要重新解析，持续类字段随之即时刷新。
    /// 窗口本身（解析要用的字符串）存在 `Coordinator::focus_windows` 的 active 槽里。
    pub(crate) window_hash: u64,
    /// 按键路径要读的规则值，按 active 槽（`pid` 的缓存名 + active 窗口）预提取，按键时不再查表。
    /// 刷新点见 [`Coordinator::refresh_active_compat_lookups`]。
    pub(crate) lookups: ActiveLookups,
}

/// [`ActiveCompat::lookups`]：按键路径上的三项规则值，焦点应用（active 槽）的。
///
/// 这几项原先在按键路径上现查（每次候选窗刷新 / 每键 / 每次收窗判定一次合成查表）；规则表与
/// 焦点窗口都没变时结果必然不变，故改成在「规则表或焦点窗口变了」的地方预提取：
/// - 焦点变化、同 pid 换窗口：`update_active_compat`（缓存键 pid + 窗口哈希，变了就整份重算）；
/// - `reload_app_compat`：规则表重载，右键菜单写回、设置端 per-app 设置变更都经它；
/// - `revalidate_pid_name`：焦点 pid 的缓存名被改写（PID 复用）。
///
/// 全局回落（候选窗定位的全局档）**不**烘进来，仍在读取侧现取，理由同 `first_show_mode`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ActiveLookups {
    /// 协调器自己发占位时用的字符（`composition_placeholder`，没配 = 空格）。
    pub(crate) composition_placeholder: wind_config::app_compat::PlaceholderChar,
    /// 联想态的占位字符（`assoc_placeholder`，没配 = 跟随 `composition_placeholder`，已在此合并）。
    pub(crate) assoc_placeholder: wind_config::app_compat::AssocPlaceholder,
    /// 拼音音节分隔符的应用级覆盖（`syllable_separator`）；`None` = 跟随全局。
    pub(crate) syllable_separator: Option<wind_config::app_compat::SyllableSeparator>,
    /// per-app 候选窗定位 `(fixed, x, y)`；`None` = 规则没配定位方式，跟随全局。
    pub(crate) candidate_pos: Option<(bool, i32, i32)>,
    /// `host_drawn_candidates`（UIElement 推断收窗的 opt-in）。进程名未知时取 `"*"` 那条，
    /// 与 `uielement_host_draws_by_inference` 的口径一致。
    pub(crate) host_drawn_candidates: Option<bool>,
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
    /// 宿主明说的密码框：`IS_PASSWORD` / `IS_NUMERIC_PASSWORD` InputScope。
    Password,
    /// 焦点 context 上的 KEYBOARD_DISABLED 置位，但宿主**没报**密码 scope。
    /// 与 `Password` 同样强制英文（Chromium 密码框就靠这一信号），但成因不同：Gecko 系
    /// （Zen / Firefox）在页面无输入焦点时也会置它（t197）。分开只为让提示语说真话——
    /// 把「宿主禁用了输入」说成「密码框」，就会让用户在普通页面上找不到密码框。
    ContextDisabled,
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
        matches!(
            self,
            Self::KeyboardDisabled | Self::Password | Self::ContextDisabled
        )
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

/// `ui.toolbar.items` → 渲染项序列（`SetToolbarLayout` 的载荷）。
///
/// **数组顺序即渲染顺序**，故本函数保序、不去重排序。
///
/// # `-` 前缀 = 「在这个位置，但不显示」
///
/// `["mode", "-full_width", "punct"]` 里的 `-full_width` 不渲染，但它**占着位置**：
/// 用户在设置页把某格拖到第 2 位再关掉，重新打开时它还在第 2 位。
///
/// 若关闭的项直接从数组里删掉（本键最初的形态），位置信息就没了，重开时只能补在声明
/// 序位——用户的体感是「我排好的顺序，关一下再开就乱了」。前缀让顺序与启用态留在**同一个
/// 数组**里：拆成 `items` + `hidden` 两个键就是本仓栽过的「两张表要同步」形态
/// （一边有一边没有，两种不一致都得有人处置）。
///
/// # 其余四条判据
///
/// - **未知键跳过 + 告警**：拼错一个词只让那一格消失，其余照常——比整条回落默认更接近
///   用户意图，且日志里查得到。带 `-` 前缀的未知键同样告警：`-punkt` 是拼错，不是
///   「刻意关掉一个不存在的格」。
/// - **重复项保留**：同一个键写两次就画两格。不特殊处理是因为它无害且无歧义，而"静默
///   去重"会让用户以为自己没写对。
/// - **结果为空则回落全集**：合法的"全部隐藏"表达是 `visible = false`（见
///   `ToolbarConfig::items` 文档）。留着它是因为**空工具栏是一条看着像 bug 的路**——
///   只剩一个拖动柄，而用户多半不知道那是自己配出来的。
///   ⚠️ 设置页那侧另有一道「至少留一格」的闸门，故这条只在**手写配置**时可达。
/// - **`-` 只在最外层剥一次**：`--mode` 剥成 `-mode`，不是合法条目 ⇒ 告警跳过。
///   不递归剥是有意的——`--mode` 只可能是手滑，把它解释成「关掉的关掉的 mode」没有意义。
///
/// `custom:<id>` 条目里 `id` 的最大下标——`ToolbarAction::Custom` 的载荷是 `u8`
/// （为保住 `Copy`，见其文档），故第 256 个及以后的按钮无法回指，解析时即拒绝。
const MAX_CUSTOM_BUTTONS: usize = u8::MAX as usize + 1;

/// 抽成模块级纯函数而非 `Coordinator` 方法：它不碰任何状态，单测无需构造协调器。
pub(crate) fn parse_toolbar_items(
    items: &[String],
    buttons: &[wind_config::ToolbarButtonSpec],
) -> Vec<ToolbarItem> {
    // 留空 = 全部显示（旧配置无此键时行为不变）。与「写了但全非法」分开处理：
    // 那条要告警，这条是正常默认，不该刷日志。
    //
    // ⚠️ 留空**不含**自定义按钮：`items` 是显示与顺序的唯一真相源，光在
    // `[[ui.toolbar.buttons]]` 里定义一个按钮不等于要显示它（否则用户没法"先定义、
    // 暂时不放上去"）。要显示就得在 items 里写一条 `custom:<id>`——设置页会代劳。
    if items.is_empty() {
        return wind_ui_types::DEFAULT_TOOLBAR_ITEMS.to_vec();
    }
    let mut out = Vec::with_capacity(items.len());
    for raw in items {
        let trimmed = raw.trim();
        // `-` 前缀 = 关着的格：仍要**走完整解析**（拼错要告警），只是不产出渲染项。
        // 用 `sink` 而不是「先判前缀再 continue」：后者会让 `-punkt` 这种拼错悄悄溜过去，
        // 而它与 `punkt` 是同一个错误，用户同样需要日志线索。
        let (key, shown) = match trimmed.strip_prefix('-') {
            Some(rest) => (rest, false),
            None => (trimmed, true),
        };
        // 关着的格解析进这个临时篮子，随即丢弃——只为触发与显示项完全相同的校验与告警。
        let mut discard = Vec::new();
        let sink = if shown { &mut out } else { &mut discard };
        match key {
            "mode" => sink.push(ToolbarItem::Mode),
            "punct" => sink.push(ToolbarItem::Punct),
            "full_width" => sink.push(ToolbarItem::FullWidth),
            "s2t" => sink.push(ToolbarItem::S2t),
            "t2s" => sink.push(ToolbarItem::T2s),
            "soft_keyboard" => sink.push(ToolbarItem::SoftKeyboard),
            "settings" => sink.push(ToolbarItem::Settings),
            "" => {}
            other => match other.strip_prefix("custom:") {
                Some(id) => push_custom_item(sink, id, buttons),
                // 前缀写错（如 `custom-sym`）落到这里，与拼错内置键同样处置。
                None => warn!("ui.toolbar.items: 未知条目 {other:?}，已跳过"),
            },
        }
    }
    if out.is_empty() {
        warn!(
            "ui.toolbar.items 无任何合法条目，回落为全部显示；整条不要请用 ui.toolbar.visible = false"
        );
        return wind_ui_types::DEFAULT_TOOLBAR_ITEMS.to_vec();
    }
    out
}

/// 把一条 `custom:<id>` 解析成渲染项，逐条判非法情形并告警。
///
/// 每一种「配了不出现」的情形都要说清是哪一种：这条链上有 4 个位置能让按钮消失
/// （id 找不到 / 被 enabled 关掉 / label 为空 / 超出可回指范围），而用户看到的都是
/// 「我配的按钮没了」。日志里不分辨，就只能靠猜。
fn push_custom_item(
    out: &mut Vec<ToolbarItem>,
    id: &str,
    buttons: &[wind_config::ToolbarButtonSpec],
) {
    let Some(idx) = buttons.iter().position(|b| b.id == id) else {
        warn!("ui.toolbar.items: custom:{id} 没有对应的 [[ui.toolbar.buttons]] 定义，已跳过");
        return;
    };
    let btn = &buttons[idx];
    if !btn.enabled {
        // 不是错误：设置页关掉某个按钮时，items 里那条 `custom:<id>` 要留着记住位置。
        return;
    }
    if idx >= MAX_CUSTOM_BUTTONS {
        warn!("ui.toolbar.items: custom:{id} 的下标 {idx} 超出上限，已跳过");
        return;
    }
    let label = wind_config::toolbar_label_trunc(&btn.label);
    if label.is_empty() {
        // 空 label 会画出一个看不见的按钮——用户点得到却不知道点的是什么，比不显示更糟。
        warn!("ui.toolbar.items: custom:{id} 的 label 为空，已跳过");
        return;
    }
    if label.chars().count() != btn.label.trim().chars().count() {
        warn!("ui.toolbar.buttons[{id}].label 超出一格宽度（一个汉字或两个 ASCII），已截断显示");
    }
    out.push(ToolbarItem::Custom {
        index: idx as u8,
        label,
    });
}

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

/// `toggle_schema` 去程的**落点快照**：上次这把键把用户送到的那个输入状态。
///
/// # 字段为什么恰好是这三项
///
/// 判据不是「我能想到哪些状态」，而是**去程自己断言了哪些状态**。
/// `Coordinator::finish_user_schema_switch` 断言的正是：活跃方案 = 目标、中文态、
/// CapsLock 关（后两者的理由写在那个函数里——它们开着时按键根本不进引擎）。
///
/// 回程是去程的撤销；**去程断言过的东西不再成立时，就没有可撤销的东西了**，此时该做的
/// 是重新落地而不是回程。
///
/// ⚠ 将来给 `finish_user_schema_switch` 加新断言时，这里必须同步加一项，否则那项被扰动
/// 后回程会静默失准——漏项没有任何编译期或测试期信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToggleLanding {
    /// 活跃方案变更代际（`EngineManager::schema_generation`）。
    pub(crate) generation: u64,
    /// 中英模式。
    pub(crate) chinese_mode: bool,
    /// CapsLock 镜像态。
    pub(crate) caps_lock: bool,
}

/// 一次「切换」对**切换前那串未上屏编码**的处置结果。
///
/// 两个字段各回答一个必须分开问的问题：
/// - `text`：要不要上屏（`keys.commit_on_switch`：开则上屏原码，关则丢弃）；
/// - `had_pending`：切换前**有没有**编码/候选在打。它单独决定要不要让宿主结束
///   composition —— 丢弃分支下 `text` 恒空，但宿主里那串编码还在，不结束组合就会
///   原样残留在应用里。「切英文后编码不清空」栽的正是这条判据（见
///   `message_handler` 的 toggle_mode 分支注释）。
///
/// 出口有两个，按调用方有没有按键上下文选：[`Coordinator::schema_switch_key_action`]
/// （按键路径，经 KeyAction 回给宿主）/ [`Coordinator::push_switch_commit`]（菜单、
/// 全局热键、命令直通车，只能走 push 管道）。
/// `#[must_use]`：丢弃它 = 编码已被取走（内部状态清了）却没人交给宿主 —— 用户那串码
/// 凭空消失，且没有任何编译期或运行期信号。
#[must_use]
#[derive(Debug, Clone, Default)]
pub(crate) struct SwitchCommit {
    /// 待上屏文本；`keys.commit_on_switch` 关闭时恒空。
    pub(crate) text: String,
    /// 切换前是否有未上屏的编码/候选/独占模式缓冲。
    pub(crate) had_pending: bool,
}

/// `toggle_schema:<id>` 的去程记录。字段语义见 `Coordinator::schema_toggle_origin`。
#[derive(Debug, Clone)]
pub(crate) struct SchemaToggleOrigin {
    /// 从哪个方案按进来的。
    pub(crate) origin: String,
    /// 送达时的落点快照。
    pub(crate) landing: ToggleLanding,
    /// 触发本次去程的键 VK（0 = 非方案级绑定触发，如全局组合热键）。
    pub(crate) trigger_vk: u32,
}

/// 触发「取消大写」的切换来自哪条路径，见 [`Coordinator::mode_after_caps_cancel`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapsCancelSwitch {
    /// 本地切换键（Shift 等）：没有方向，只说「切一下」。
    Toggle,
    /// 用户按出来的系统切换（Ctrl+空格 / 系统输入法开关热键）：携带目标模式（`true`＝中文）。
    System(bool),
    /// 非用户发起的切换（宿主写 compartment、功能菜单、旧版 DLL 无 Ctrl）：携带目标模式，
    /// **恒按请求落地**、不吃 `mode_after_cancel` 的改写——宿主请求「中文」时 CapsLock
    /// 恰好开着被取消，不能因为用户选了 `english` 就落成英文。
    Host(bool),
    /// 定向切换（`ime.set("cn-en", …)`）：用户**明确点了目标**，取消锁定只是附带动作，
    /// 模式必须直达目标——出厂 `mode_after_cancel` 只该管「切一下」这种没有方向的翻转
    /// （Shift / 切换键）。
    Directed(bool),
}

impl CapsCancelSwitch {
    /// 按 `CMD_SYSTEM_MODE_SWITCH` 的来源分派。「是不是用户按的」判据与
    /// [`Coordinator::host_ime_switch_ignored`] 同源：系统热键（Ctrl+空格）翻 compartment 时
    /// Ctrl 尚未释放，宿主自己写则没有伴随按键；按键侧兜底（`CtrlSpaceKey`）恒算用户发起。
    /// 菜单是用户**明确点了目标**，没有「切一下」的歧义，按请求。
    pub(crate) fn from_system(
        requested: bool,
        source: wind_ipc::protocol::ModeSwitchSource,
        ctrl_held: bool,
    ) -> Self {
        use wind_ipc::protocol::ModeSwitchSource as Src;
        let by_user = match source {
            Src::CtrlSpaceKey => true,
            Src::CompartmentOpenClose | Src::CompartmentConversion | Src::Unknown => ctrl_held,
            Src::Menu => false,
        };
        if by_user {
            Self::System(requested)
        } else {
            Self::Host(requested)
        }
    }
}

/// 中央协调器
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
    /// 输入行为的平台档位，构造时定下，之后不变。见 [`crate::input_profile`]。
    pub(crate) profile: crate::input_profile::InputProfile,
    /// 中立输出通道（`new_session` 构造时给）。有它就只发 [`crate::SessionEvent`]，并跳过
    /// 桌面专属的首显闸门、注释渲染与候选窗布局；`None` = 桌面路径，行为不变。
    /// 发送点集中在 `session_output.rs`。
    pub(crate) session_tx: Option<crate::session_output::SessionSender>,
    /// redb 持久化存储（用户词/临时词/词频/影子规则）；None=无持久化（headless 测试）。
    pub(crate) store: Option<Arc<Store>>,
    /// 只读模式的原因（用户数据设计 §7.1 降级保护：meta.toml 版本高于本版本 / 读不出）；
    /// `None` = 正常读写。只读时 `store` 恒 None。
    pub(crate) storage_readonly: Option<String>,
    /// 标点转换器（引号左右状态）
    pub(crate) punct: Mutex<PunctuationConverter>,
    /// 智能符号模式待命态（同键连按删中文标点改英文）
    pub(crate) smart_symbol: Mutex<SmartSymbolArm>,
    /// 滑窗草稿的**落屏文本流**缓冲。独立于 `State` 的理由同 `auto_phrase`。
    ///
    /// 与 `auto_phrase` 的分界：那边缓冲「连续单字序列」（多字词即终止），这边缓冲
    /// 「最近落屏的文本流」（词组照样进流）。见 `draft_window` 模块头注释。
    pub(crate) draft_window: Mutex<crate::draft_window::DraftWindowBuf>,
    /// 待落库的草稿词队列。
    ///
    /// ★ **按键路径上只做「滑窗切分 + push」**，取码、查重、写库全在后台线程
    /// （`spawn_draft_flush`）。草稿的产生速率约每字 4 条，而取码要查单字全码表、
    /// 查重要查反查索引与用户词库——任何一项落在上屏线程上都是在给每次按键加钱。
    pub(crate) draft_queue: Mutex<Vec<crate::draft_window::DraftWord>>,
    /// 是否已有草稿 flush 线程在跑。没有这道闸，队列每满一次就会 spawn 一个新线程去
    /// 抢同一把 redb 写锁（同 `is_building_reverse_index` 那道闸的理由）。
    pub(crate) draft_flushing: std::sync::atomic::AtomicBool,
    /// 最近一次**本输入法自己**向宿主吐字的时刻（`note_self_commit` 打点，经
    /// `finish_session_action` 按来源调用）。
    ///
    /// 用途只有一个：宿主插入我们提交的文字后会回送 `SelectionChanged`，它和「用户真的
    /// 移动了光标」在协议层**长得一模一样**，只能靠时间区分。若不区分，每上屏一个字就会
    /// 被自己的回声判成「用户移动光标」→ flush → 缓冲永远只有 1 个字 → 造词恒不触发。
    ///
    /// **打点必须收口在 `finish_session_action` 一处**：漏掉任一吐字路径，该路径的回声就会
    /// 切碎序列。不经它的推送通路（落点指引 §4.3 末表）目前不打点。
    pub(crate) last_self_commit: Mutex<Option<std::time::Instant>>,
    /// 自动造词写入计数，供临时词库淘汰按次节流（见 `maybe_evict_temp`）。
    pub(crate) auto_phrase_writes: std::sync::atomic::AtomicUsize,
    /// 全局低级键盘钩子（CapsLock，兼管 Windows 上的注释切换键 Alt）。
    ///
    /// ★ **只有用户在 `keys.session_actions` 里真的配了 `capslock`、或注释切换键选了 Alt
    /// 时才是 `Some`**。没配的用户进程里根本不存在全局键盘钩子——这是本功能唯一的风险
    /// 控制手段（用户明确要求）。
    ///
    /// 为什么非钩子不可：CapsLock 的锁定态由系统在 TSF **之前**维护，`pfEaten` 压不住；
    /// 而「让它翻转再回敲复原」在快速连按下有竞态（大写会卡住），还会被厂商 OSD 工具
    /// 观测到并弹窗。详见 `wind_keys::capslock_hook` 模块文档。
    pub(crate) capslock_hook: Mutex<Option<wind_keys::capslock_hook::CapsLockHook>>,
    /// 钩子线程 → 动作消费线程的投递口。
    ///
    /// 在 `new` 里就建好并起好消费线程（那里才有 `Arc<Self>`），钩子装卸只是复用它。
    /// 消费线程空闲时阻塞在 channel 上，未装钩子时零开销。
    capslock_press_tx: std::sync::mpsc::Sender<wind_keys::capslock_hook::HookEvent>,
    /// 钩子装着且 CapsLock 那份要它（只为 Alt 装时为假：CapsLock 照旧归系统）。
    hook_caps: std::sync::atomic::AtomicBool,
    /// 钩子装着时要接管的 Alt 侧（`wind_keys::alt_tap::GATE_*`），0 = 不接管。有候选时才下发。
    hook_alt_mask: std::sync::atomic::AtomicU32,
    /// 这次被钩子接管的 CapsLock 按下已被会话动作（翻页、选词、大小写档位等）用掉：
    /// 松开时就不再当中英切换键处理。只在 CapsLock 动作消费线程里读写。
    caps_press_claimed: std::sync::atomic::AtomicBool,
    /// 短语层（系统+用户，来自 store，仅 enabled）。变更后可 rebuild_phrases 重建。
    pub(crate) phrases: std::sync::RwLock<wind_phrase::PhraseLayer>,
    /// 最近一次解析的系统短语条目（启动时填充；"恢复默认"重读文件成功后刷新）。
    /// 作为重读失败（文件缺失/TOML 语法错误）时的回退，避免把库里系统短语清空。
    pub(crate) system_phrase_entries: std::sync::RwLock<Vec<wind_phrase::SystemPhraseEntry>>,
    /// 智能联想「下一词」表的路径（`assoc/assoc_bigram.txt`，用户目录同名文件整体替代）；
    /// None = 数据缺失，Bigram 源不出、配额顺延。
    pub(crate) assoc_bigram_path: Option<std::path::PathBuf>,
    /// 「下一词」表本体：**用得上才读**（桌面联想默认关，不该为它多占内存），且只在后台线程读
    /// （约 2 MB 文本，见 `ensure_assoc_bigram_async`）。读失败记一条警告、之后一直是 None，
    /// 不反复重试。`Arc` 是为了让加载线程不必持有协调器。
    pub(crate) assoc_bigram: std::sync::Arc<
        std::sync::OnceLock<Option<std::sync::Arc<wind_assoc::bigram::AssocBigram>>>,
    >,
    /// 下一词表的后台加载已派出（只派一次）。
    pub(crate) assoc_bigram_loading: std::sync::atomic::AtomicBool,
    /// system.phrases.toml 路径（None=无 data_dir，如 headless 测试）。
    /// "恢复默认"据此重读文件，使手工编辑无需重启服务即可生效。
    pub(crate) system_phrase_path: Option<std::path::PathBuf>,
    /// `system.comments.toml`（内置注释模板与出厂分配），启动时读一次。
    pub(crate) system_comments: wind_config::comment_lib::SystemComments,
    /// 合并后的注释模板库，配置生效时重建，见 [`Self::rebuild_comment_lib`]。
    pub(crate) comment_lib: std::sync::RwLock<Arc<wind_config::comment_lib::CommentLibrary>>,
    /// 简繁转换器（OpenCC；None=数据缺失不可用）。变体由配置 features.s2t.variant 决定，
    /// 启动时加载；菜单仅提供开/关。置于 Mutex 兼容 reload 时整体替换。
    pub(crate) s2t: Mutex<Option<wind_transform::s2t::Converter>>,
    /// 繁 → 简转换链（`input.t2s`）。与 `s2t` 各自独立加载：两条链的词典完全不同
    /// （TS* vs ST*），共用一个 `Option` 就得在每次切方向时重新读盘。
    pub(crate) t2s: Mutex<Option<wind_transform::s2t::Converter>>,
    /// 通用规范汉字表（检索范围"常用字"判定；空集时退化为不过滤）。
    ///
    /// 置于 `RwLock`：出厂基表在构造期一次性加载，而**用户覆盖**（候选右键「设为生僻字 /
    /// 设为常用字」、词库管理界面）随时可改，改完必须当场生效。若仍是不可变字段，用户
    /// 就会撞上那类最难查的现象——「设了没反应，重启后才对」。
    /// 常用字表。**`Arc` 是为了让准入判据能被交给引擎**（`ConvertOptions::admit` 要求
    /// `Send + Sync + 'static` 的闭包，捕获裸字段做不到）；除此之外用法与普通 `RwLock` 相同。
    pub(crate) common_chars: std::sync::Arc<std::sync::RwLock<wind_candidate::CommonChars>>,
    // Shadow 规则已迁至 redb（self.store 的 SHADOW 表）。
    /// 工具栏锚点（窗口**右下角**屏幕坐标），按显示器 key（`monitor_key` 的
    /// `"workRight,workBottom@scalePct"`）独立记录。语义与落盘形态见
    /// [`wind_config::RuntimeState::toolbar_anchors`]。
    pub(crate) toolbar_anchors: Mutex<std::collections::HashMap<String, (i32, i32)>>,
    /// 软键盘锚点（面板**右下角**屏幕坐标），按显示器 key 独立记录。与
    /// [`Self::toolbar_anchors`] 同构。
    pub(crate) softkeyboard_anchors: Mutex<std::collections::HashMap<String, (i32, i32)>>,
    /// 工具栏当前所在显示器的 key（None=尚未定位）。`sync_toolbar_monitor` 的去重依据：
    /// notify_toolbar 在每次模式切换/焦点事件上都跑，无此缓存就会把用户拖动过的位置
    /// 反复重置回记忆值。拖动落盘时（`save_toolbar_anchor`）同步更新，否则拖到别的屏之后
    /// 这里仍记着旧 key，下一次校正会被误判为「屏没变」而跳过。
    pub(crate) current_toolbar_monitor: Mutex<Option<String>>,
    /// Linux：addon 最近一次 `host.display` 报的焦点输出（显示器）名与其工作区。服务端问不到
    /// 屏幕几何，工具栏 / 软键盘的位置记忆改以输出名分桶（见 `Coordinator::current_monitor`）。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub(crate) host_output: Mutex<Option<String>>,
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub(crate) host_work: Mutex<Option<(i32, i32, i32, i32)>>,
    /// Linux：`pos.toolbar` / `pos.softkeyboard` 报的落点输出名，按浮层 kind 暂存，等渲染端把
    /// 左上角换算成右下角回报（`ToolbarMoved` / `SoftKeyboardMoved`）时作为落盘的显示器键取走。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub(crate) panel_drop_output: Mutex<std::collections::HashMap<u32, String>>,
    /// Linux：addon 在 `host.display` 里声明过 `panels`（能贴工具栏 / 软键盘浮层）。旧 addon 不认
    /// 这两种浮层、收到就丢——升级 deb 后 fcitx5 没重启，旧 addon 拉起的是新服务，放任软键盘
    /// 开着会接管主键区而屏上什么都没有。请求连接断开即复位（下一个 addon 实例得重新声明）。
    #[cfg(all(target_os = "linux", ext_presenter))]
    pub(crate) host_panels: std::sync::atomic::AtomicBool,
    /// `state.toml` 的单点延迟写入器：合并连续微调、进程内串行化 load-modify-save。
    /// 无 `store`（headless 测试夹具）时是不写盘的空实现，见 [`state_writer::StateWriter`]。
    pub(crate) state_writer: state_writer::StateWriter,
    /// 候选反查（编码/拆字/拼音）供悬停提示与加词出码；拆字段只在有消费者时装、随主码表
    /// 方案与配置热重载（见 `sync_chaizi_assets` / `ensure_chaizi_async`），拼音段启动加载后
    /// 不变。
    pub(crate) reverse: std::sync::RwLock<wind_reverse::ReverseLookup>,
    /// 辅助码运行时来源（进入辅助码时按 `[engine.aux_code].files` 建，来源清单即缓存键）。
    /// `None` = 尚未建 / 已失效。见 [`crate::aux_code_source`]。
    pub(crate) aux_code_runtime:
        std::sync::RwLock<Option<Arc<crate::aux_code_source::AuxCodeRuntime>>>,
    /// emoji 扩展表（`[input.emoji]`，mmap 只读）。`None` = 功能关闭 / 数据缺失 / 建缓存失败。
    ///
    /// ★ 功能关闭时恒为 `None` 且**数据文件根本不打开**——未启用的用户为本功能付出的
    /// 常驻内存与启动开销都是零。这是 `.wemj` 走 mmap 的同一条理由的延伸：本表按需读页，
    /// 常驻内存与库大小基本无关，但「不加载」比「加载得很省」还要省。
    pub(crate) emoji_dict: std::sync::RwLock<Option<EmojiTables>>,
    /// 上次生效的 emoji 加载参数 `(enabled, categories, 开着的附加模块 id)`，
    /// `sync_emoji_dict` 的去重依据。
    ///
    /// 只记**影响加载结果**的几项：`show_as` / `max_per_word` 这些是查询期参数，改了不必
    /// 重建 mmap。判据写宽了会让每次 reload 都重解析一遍词表。
    pub(crate) emoji_spec: Mutex<Option<EmojiSpec>>,
    /// 「辅助码触发键被音节分隔符占用」的告警是否已发过（每方案一次，随
    /// `invalidate_aux_code_table` 复位）。见 `handle_aux_code::warn_aux_code_key_taken`。
    pub(crate) aux_code_key_warned: std::sync::atomic::AtomicBool,
    /// 快捷输入格式表（`system.quick.toml`，支持用户目录整份覆盖）。
    ///
    /// 启动加载后不变，故无锁：与 `system.phrases.toml` 同语义——**改完必须重启服务**，
    /// 全仓的覆盖点都没有文件监视器。加载失败已在 `FormatTable::load` 内回落内置默认表，
    /// 此处恒是一张可用的表。
    pub(crate) quick_formats: wind_quick_input::FormatTable,
    /// 软键盘映射表（出厂画布 ⊕ 用户按面覆盖）。设置端编辑器写完用户层后经
    /// `reload_softkeyboard` 整表换新，故是 `RwLock<Arc<_>>`：读侧取一份 `Arc` 快照即放锁，
    /// 按键路径上不会与重载互等。读用 [`Self::softkeyboard_table`]。
    pub(crate) softkeyboard: std::sync::RwLock<Arc<wind_softkeyboard::SoftKeyboardTable>>,
    /// 软键盘面板开着。
    ///
    /// ★ **刻意不放 `State`**，虽然它看起来像一个输入状态。理由是死锁：状态推送
    /// （`push_state_update` → `build_status`）自己要 `state.lock()`，而开关软键盘发生在
    /// 已经持有 `&mut State` 的按键路径里——放进 `State` 就意味着「改完必须先还锁再推送」，
    /// 那是个只能靠注释维持的不变量，漏一处的症状还极隐蔽（Rust 认为开着并接管按键，
    /// C++ 仍按没开判定而不吃数字键，数字行整行失效）。放在 atomic 上，两条路径互不相干。
    ///
    /// 语义上它也确实与输入状态正交：软键盘不组码、不产候选、不进 `ModeKind`。
    pub(crate) softkeyboard_active: std::sync::atomic::AtomicBool,
    /// 当前面在 [`Self::softkeyboard`] 里的下标。关闭再开会停在同一面，
    /// **重启也会**（启动时按 `RuntimeState::last_softkeyboard_page` 的面 id 还原）。
    pub(crate) softkeyboard_page: std::sync::atomic::AtomicUsize,
    /// 已写进 `state.toml` 的面 id，用于去重写盘。
    ///
    /// 落盘挂在「软键盘状态变了」那个收口上（`after_softkeyboard_change`），而**开合远比
    /// 换面频繁**——每次开关都 load-modify-save 一遍整个 state.toml 是白费。初值取自启动
    /// 时读到的那一份，故「开了面板、没换面、关掉」全程零写盘。
    pub(crate) softkeyboard_page_saved: std::sync::Mutex<String>,
    /// 软键盘状态有变、待推给 C++。由 `SoftKeyboardPushOnDrop` 在按键返回时消费。
    pub(crate) softkeyboard_dirty: std::sync::atomic::AtomicBool,
    /// **DLL 最后收到的** `STATUS_HOTKEY_SESSION` 取值。
    ///
    /// ⚠️ 语义是「对端收到了什么」，不是「我上次算出什么」—— 每一条把这一位发出去的
    /// 路径都必须对齐它（`push_hotkey_session_if_changed` 自己对齐，其余路径调
    /// `sync_hotkey_session_cache`）。脱节的后果是**漏推**：缓存停在 A 而 DLL 已收到 B，
    /// 此后状态从 B 变回 A 时 `prev == now` 被判成「没变」而不推，DLL 就永远停在 B。
    /// 那正是 C2 的形状 —— 不是「白推一次 IPC」那种良性代价。
    pub(crate) last_pushed_hotkey_session: std::sync::atomic::AtomicBool,
    /// 本次按键命中过 key_down 热键 ⇒ 返回时**无条件**推一次状态（不走比较）。
    ///
    /// 与 C++ `WM_HOTKEY` 那一处乐观置位一一对称：那边置位不看动作，这边就不看状态有没有
    /// 变。详见 `message_handler.rs` 里置位处的长注释。
    pub(crate) hotkey_session_force_push: std::sync::atomic::AtomicBool,
    /// 软键盘打开时刻，供焦点路径的关闭守卫用。**必须与开启成对写入**，理由同
    /// `State::menu_opened_at`：漏写会让守卫读到上一次的时间戳，于是刚弹出的面板
    /// 被一条迟到的焦点事件当场关掉。
    pub(crate) softkeyboard_opened_at: std::sync::Mutex<Option<std::time::Instant>>,
    /// 软键盘开着时「它在服务哪个宿主进程」，0 = 未知（开启时尚无活动宿主）。
    ///
    /// 关闭判据按**宿主进程**粒度而不是按焦点事件粒度：同宿主内的 DocMgr 抖动、退出编辑、
    /// 换文档都不算「用户离开」（Excel 退出单元格再切到下一格就是这样），只有换了宿主才算。
    /// 与开启成对写入，见 `open_softkeyboard`。
    pub(crate) softkeyboard_host_pid: std::sync::atomic::AtomicU32,
    /// 钉住：面板不随焦点变化自动关闭（跨应用也留着）。是**用户偏好**：关闭不复位，
    /// 下次打开照旧钉着，跨重启经 `RuntimeState::softkeyboard_pinned` 还原。
    /// 唯一的翻转入口是 `toggle_softkeyboard_pin`。
    pub(crate) softkeyboard_pinned: std::sync::atomic::AtomicBool,
    /// 快捷输入格式表的**用户调整**（右键调序 / 停用）运行时镜像，键为格式类别。
    ///
    /// 真相在用户数据目录的 `quick_format.toml`，这里是读缓存：候选生成在热路径上
    /// （每次按键都跑），每次去读文件不划算。
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
    /// 注释词库当前生效的 `(解析路径, 适用方案白名单)` 列表（顺序即优先级），
    /// reload 变更检测用。白名单也进变更判据——只比路径的话，用户改完「适用方案」
    /// 会因「路径没变」被判成空操作，表现是「改了要重启才生效」。见 `sync_comment_dicts`。
    pub(crate) comment_dict_paths: Mutex<Vec<(std::path::PathBuf, Vec<String>)>>,
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
    /// 宿主对配置的**强制改写**（见 [`Self::set_config_overlay`]），每次从磁盘重载配置后
    /// 重新套上。只设一次（构造后），故用 `OnceLock` 而不加锁。
    config_overlay: std::sync::OnceLock<fn(&mut Config)>,
    /// 暂停学习（隐私 / 无痕输入）：置位期间上屏照常、候选照常，但**不把这次输入写进任何
    /// 用户数据**（词频、自动造词 / 草稿、联想历史与下一词、英文造词、输入统计、快捷输入 /
    /// 网址 / 邮箱历史、上屏历史）。见 [`Coordinator::set_learning_suspended`]。
    ///
    /// 纯运行时、**不进配置**：它描述的是「当前这个输入框」的属性（Android 的
    /// `IME_FLAG_NO_PERSONALIZED_LEARNING`），宿主每次聚焦重新声明；持久化下来反而会让
    /// 一次无痕输入把之后所有输入框都变成不学习。
    learning_suspended: std::sync::atomic::AtomicBool,
    /// 宿主报的光标坐标可直接当权威用（Linux addon：来自应用自己上报的光标矩形，没有 Windows
    /// 那种跨窗口 Win32 光标冒充插入点的问题，但来源字段恒为 UNKNOWN）。见 `host.display`。
    host_caret_trusted: std::sync::atomic::AtomicBool,
    /// 焦点宿主**显示不了**应用内预编辑（Linux addon：客户端没声明 Fcitx5 的 Preedit 能力位，
    /// 如 WPS、QQ 一类走 XIM 的应用）。置位时编码改由我们自己的候选窗画，见
    /// [`Coordinator::preedit_in_app_effective`]。见 `host.display` 的 `inline_preedit`。
    host_no_inline_preedit: std::sync::atomic::AtomicBool,
    /// 热重载成功后**不弹**「设置已更新」（移动端）。见 [`Coordinator::set_quiet_reload_toast`]。
    quiet_reload_toast: std::sync::atomic::AtomicBool,
    /// 被压制的引导键 VK（0=无）。命中时 `bound_action_for` 按「未绑定」处理。
    /// 见 [`Coordinator::set_trigger_suppressed`]。
    pub(crate) trigger_suppressed: std::sync::atomic::AtomicU32,
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
    /// 联想的占位组合在宿主里成了**孤儿**：服务端已退出联想态，宿主那边的组合还挂着。
    ///
    /// 只有自动隐藏超时会造出孤儿——其余退出联想的路径都发生在按键上下文里，收口动作
    /// 搭着那次按键的应答就送到宿主了（见 `handle_assoc::assoc_dismiss_with`）；超时由
    /// 定时器线程触发，**没有待应答的按键可搭**，而服务端→TSF 的 push 通道里没有任何
    /// 一条能结束组合。见 [`Coordinator::fire_assoc_hide`]。
    ///
    /// 置位后由 [`Coordinator::adopt_orphaned_placeholder`] 在按键处理的唯一出口消费。
    pub(crate) assoc_placeholder_orphaned: std::sync::atomic::AtomicBool,
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
    /// 本轮组合紧接在一次**上屏**之后开（Shift+字母上屏进临英）：宿主插入点必然已前移，
    /// 兜底首显用的旧坐标是确定的陈旧值，不是「可能差一点」。置位时兜底首显**不记**
    /// `first_show_was_provisional`，让随后到达的权威坐标按常规 3px 容差校正，
    /// 而不是被放宽容差（≈2 个行高）当微移吞掉、候选窗留在上屏前的位置。
    /// 由 [`Coordinator::reset_first_show`] 复位，兜底首显消费。
    pub(crate) first_show_after_commit: std::sync::atomic::AtomicBool,
    /// 坐标缓存是否已被**当前插入点**验证过（= `state.caret_*` 还算不算数）。
    ///
    /// `fast` 档短兜底的隐含前提是「手里的旧坐标 ≈ 当前插入点」——同一行连打时它只差一个
    /// 字宽，所以拿它首显毫无问题。本标志就是那个前提的显式化：
    ///
    /// - **置位**：[`Coordinator::handle_caret_update`] 采纳一帧权威坐标（与
    ///   `last_authoritative_caret` 同一处，同一条「够格当基准」的判据）。
    ///   ⛔ 唯一例外是 [`wind_ipc::protocol::caret_source::TSF_DEFAULT_POS`]：它虽被采纳为
    ///   定位坐标，却按定义**不对应任何插入点**（宿主答的是「我不知道插入点在哪」）。
    ///   置位会让下一次组合错走 25ms 短兜底 ⇒ 先在旧坐标处闪一下再跳到默认位置。
    ///   ⚠ 该来源有**两个**入口能置位本标志，组合前那处靠 `is_tsf` 天然挡住，组合期那处
    ///   原本是无条件置位、需要单独挡——挡一处不等于挡住了。
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
    /// 坐标缓存当前这份值，是不是**本次组合开始之前**由宿主主动上报的「空闲光标位置」。
    ///
    /// 上屏后到下一次按键之间，宿主仍会上报 caret（`handle_caret_update` 的「无组合」
    /// 分支）。那一帧是宿主对**当前插入点**的直接测量，比组合期间的 rect 更可信——
    /// 尤其在用户上屏后又移动过光标（打空格/点一下）的时候，它是唯一说得出「光标现在
    /// 到底在哪」的数据。
    ///
    /// 本标志存在的唯一理由，是让 [`Coordinator::handle_caret_probe`] 能识破一类陈旧
    /// probe：微信（Qt WebView）在 composition 期间报的 rect 仍是**上一次组合**的位置，
    /// 实测与真实插入点差 136px。而 probe 判据 1（「≠ 上一轮权威坐标 ⇒ 已 reflow」）
    /// 对此完全没有判断力——正确答案和陈旧值**都** ≠ 那个基准，判据把两者一视同仁。
    /// 换句话说这不是判据太松，是判据问错了问题；只能换依据，不能调松紧。
    ///
    /// - **置位**：`handle_caret_update` 的「无组合」分支（那一帧只更新缓存、不做显示决策）。
    /// - **清位**：`handle_caret_update` 采纳一帧组合期间的权威坐标时——此后缓存里装的
    ///   是本次组合的位置，不再是「组合前的空闲上报」，本标志的前提随之消失。
    ///
    /// ⚠ 不与 [`Self::caret_cache_verified`] 合并：那个答「手里的值可不可信」，本字段答
    /// 「手里的值是不是组合前刚测的」。微信这个场景里前者为真、后者也为真，但正是靠后者
    /// 才能判定 probe 陈旧——合成一个就再也分不出「缓存可信」与「probe 可信」了。
    pub(crate) caret_cache_is_idle_report: std::sync::atomic::AtomicBool,
    /// 本次焦点到达后，**还没有**经历过一次组合期间的权威坐标。
    ///
    /// 专门用来把 `idle_anchor` 逃生口挡在「焦点后的第一个组合」之外。空闲上报在**组合之间**
    /// 是可信的（焦点没变、文档没变，宿主报的就是当前插入点），但在**焦点刚到达时**恰恰最
    /// 不可信——宿主还没 reflow，报的往往是上一处的位置。EverEdit 实测：焦点坐标 (2599,703)、
    /// 空闲上报 (1949,527)、真实位置 (749,529)，三个互不相同，拿空闲上报立即首显错 1200px。
    ///
    /// ★ 这正是 `caret_cache_verified` 在 `focus_gained` 处清位要防的那件事。让空闲上报也能
    /// 置 `verified` 之后，焦点后的第一次空闲上报就把那道门重新顶开了——**修复不能把它本来
    /// 要堵的洞重新打开**，故单独立一个字段挡住逃生口，而不是收窄 `verified` 的置位
    /// （收窄了焦点后首字就退回 600ms 长兜底，那是真正要避免的）。
    ///
    /// 挡住之后焦点后首字走 25ms 短兜底：其间 probe 会把缓存刷成正确位置，EverEdit 实测
    /// 9ms 后就到了，兜底到期时用的已是对的坐标（甚至更早由 probe 判据提前首显）。
    ///
    /// - **置位**：`handle_focus_gained` 清 `caret_cache_verified` 处（同一件事的两面）。
    /// - **清位**：`handle_caret_update` 采纳一帧组合期间的权威坐标处。
    awaiting_first_authority_after_focus: std::sync::atomic::AtomicBool,
    /// 候选窗**当前实际显示**所用的位置基准（x, y, 有效）。
    ///
    /// 存在的理由：`state.caret_x/y` 是**缓存**，它会被 probe 的 `absorb_probe_coords` 和被
    /// `settle` 吸收掉的那次权威坐标悄悄改写，而候选窗并不跟着动。于是缓存跑到候选窗前面，
    /// 此后任何**非坐标原因**的重绘（悬停、翻页、候选变化）都会把这段差额一次性补上——
    /// 表现为「鼠标一指候选窗，位置跳一个字符」（微信实测 483→496）。
    ///
    /// 组合起点 `composition_start` 锁定时不需要本字段（那时位置本就钉死），但它在连打时
    /// 每次上屏都被 `reset_first_show` 清掉，`idle_anchor` 这条路首显时它还没锁上。
    ///
    /// - **更新**：`notify_ui_update` 每次**首显**或**坐标校正**（`show_authorized`）下发之后。
    /// - **清位**：`reset_first_show`（组合结束，锚点随之失效）。
    shown_anchor: Mutex<(i32, i32, bool)>,
    /// 定位调试浮窗开关（仅 Dev 变体的菜单可切；默认关）。
    ///
    /// ★ 它存在的理由见 `wind_ui::caret_overlay` 的模块文档：定位缺陷是**空间**问题，
    /// 日志只能给出离散数值。开关放运行时而非配置——它是排查动作，不该被记住。
    pub(crate) caret_overlay_enabled: std::sync::atomic::AtomicBool,
    /// 最近一帧 caret_update 的几何与判据结果，供浮窗渲染。
    ///
    /// 分两处填：`handle_caret_update` 知道宿主上报了什么、判据走了哪条；
    /// `notify_ui_update` 知道候选窗**实际**用了哪个锚点。两者合起来才回答得了
    /// 「算出来的位置对不对」，所以先暂存，下发时补齐锚点再推给 UI。
    caret_overlay_frame: Mutex<Option<wind_ui_types::diag::CaretOverlayView>>,
    /// 坐标校正判据的比较基准（x, y, 有效）——「上一次**被认可**的插入点位置」。
    ///
    /// ⚠ 与 [`Self::shown_anchor`] 只差一处，但那一处正是缺陷发生的地方，**不可合并**：
    /// `settle` 吸收一帧权威坐标时（判定这点偏差不值得移动候选窗），候选窗不动、故
    /// `shown_anchor` 不动，但那一帧的坐标**已被认可**，基准必须跟上。
    ///
    /// 合用一个字段的后果：被吸收的偏差永久留在基准里，下一帧权威坐标与它一比又是同样的
    /// 偏差，而 `settle` 的放宽容差只在本轮第一帧有效（`swap` 消费掉），于是 tol 掉回 3px
    /// ⇒ 必然 reshow。微信实测「打第二三个字时候选窗自己挪一格」（483→496，dx=13）即此。
    ///
    /// `settle` 说的是「这次偏差不值得校正」，它没说「以后也不值得」。
    ///
    /// - **更新**：`notify_ui_update` 下发新位置时；`handle_caret_update` 的 settle 吸收分支。
    /// - **清位**：`reset_first_show`。
    caret_baseline: Mutex<(i32, i32, bool)>,
    /// 最近一次**非退化**的 caret 高度，用作行高估计。
    ///
    /// 行高是**宿主的属性**，不是单帧的属性——同一个输入框不会这一帧 20px、下一帧 1px。
    /// 但微信等 WebView 的 `GetTextExt` 返回的 height 恰好在 1↔20px 之间跳变（`caret_use_top`
    /// 这条 per-app 规则就是为它加的），而 settle 容差是「行高 × ratio」：
    ///
    /// ```text
    /// h = 1  ⇒  settle = (1 × 0.8) as i32 = 0  ⇒  tol = max(0, 3) = 3px
    /// ```
    ///
    /// 容差整个塌缩到下限，放宽多少倍都没用（2 × 0 还是 0），于是一个字符宽的偏差必然触发
    /// 校正——微信实测「每次自动上屏后回退 20px」。故行高单独记住，不跟着单帧的退化值走。
    ///
    /// ⚠ 跨宿主**刻意不重置**：换到行高不同的宿主时它会偏大一阵，而偏大只意味着多吸收几个
    /// 像素的微移（settle 本就只作用于本轮第一帧），比「刚切过去那一帧恰好 h=1、容差又塌成
    /// 3px」这个已知故障安全得多。
    ///
    /// ⚠⚠ 初值是 [`FALLBACK_LINE_HEIGHT`] 这个**保守下限**而不是 0：本字段有两种「取不到可靠
    /// 值」的方式——被单帧退化值污染（已由 `> 1` 的写入闸挡住）、以及**尚未观测到任何一帧**。
    /// 后者同样会让两个消费点（settle 容差、组合起点重锁阈值）塌缩到下限 3px，后果分别是
    /// 「每次自动上屏后回退一个字宽」和「任何微移都重锁组合起点」。它只参与容差计算，偏大
    /// 远比塌成 1px 安全。
    last_sane_caret_height: std::sync::atomic::AtomicI32,
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
    /// **不配「拿现有坐标显示」的兜底 timer**：超时后那样做只能用不可信坐标，正是本机制要挡的
    /// 事。等不到就不显示，失焦/下一次焦点事件清位。唯一的超时是兜底为**锚点**时（锚点不读
    /// 光标，不在上述反对理由之列），见 `status_placement::park_focus_tip`。
    pending_focus_tip: std::sync::atomic::AtomicBool,
    /// 焦点气泡挂起的代际：每次挂起 +1。锚点超时到期时比对它，被新的挂起取代则作废。
    pending_focus_tip_gen: std::sync::atomic::AtomicU64,
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
    /// 本轮组合里 DLL 标记为 `PRE_REFLOW` 的那一帧坐标 (x, y, seen)——**宿主重排前的位置**。
    ///
    /// 用途只有一个：给 probe 的「宿主 reflow 了没有」判据当基准。原判据拿的是**上一轮**
    /// 权威坐标，而终端换行时陈旧值与它天然不等（行尾 1432 vs 上一轮 1401，差一个字符宽），
    /// 于是陈旧值被判成「已 reflow」⇒ 候选窗画在行尾、67ms 后横穿屏幕跳到行首 1044px。
    /// 本轮的 pre_reflow 帧是更贴切的基准：DLL 已经明说它是重排前的，probe 与它相同就是
    /// 「还没重排」。
    ///
    /// ★ 这是**换依据**，不是给位置关系再加一条物理约束——后者的四个变体全被真机推翻过
    /// （见 `handle_caret_probe` 里那段「已否定」清单）。本条不问坐标之间的几何关系，
    /// 只问「这一帧和宿主自己声明的重排前坐标是不是同一个值」。
    pub(crate) last_pre_reflow_probe: Mutex<(i32, i32, bool)>,
    /// 本轮 `PRE_REFLOW` 帧携带的组合起点 (x, y, seen)，**只供兜底首显定位**（A2-42）。
    ///
    /// 首显等不到权威坐标时（记事本常见：OnLayoutChange 25ms 内不来），兜底只能用缓存——
    /// 而缓存 caret 已被这一帧刷成**首字母之后**的位置，组合起点却要等随后的矩形帧才锁。
    /// 于是首显画在 caret、第 2 个字母 reshow 时改取组合起点，当着用户的面左跳一格。
    /// 组合起点是按键前的插入点，宿主重排不挪它，比同一帧的 caret 更接近最终锚点。
    /// ⚠ 只作显示位置，**不锁进 `composition_start`**：锁错了本组合内救不回来，
    /// 而只显示的话，后续权威帧与它不符时照常 reshow 纠正。组合结束 / 换 docMgr 时清。
    pub(crate) pre_reflow_comp_start: Mutex<(i32, i32, bool)>,
    /// 本组合内**组合矩形**首帧给出的锚点 (x, y, locked)——同一行里它就是最终答案。
    ///
    /// ★★ 存的是**值**而不是一个 bool，因为矩形这条路必须保持「**每帧如实**」的语义。
    /// `composition_start` 在同一次组合里还有另外两个写入者，它们都只要求「本帧没有可信矩形」：
    /// `shown_anchor` 回灌（把「候选窗画在哪」写回起点）与大偏移逃生阀（把起点推到当前 caret
    /// ＝ 组合**末端**）。两处的注释都明写自己敢这么做的前提是「矩形每帧如实到达，锁不错也
    /// 就无需逃生」。若本锁只是「置位后什么都不做」，那么全零矩形帧（组合 range 的
    /// `GetTextExt` 回 `TS_E_NOLAYOUT`，`comp_rect_from_bytes` 给 `None` 而 `frame_layout_degenerate`
    /// 为 false）让逃生阀武装一次，`cs` 就被改成组合末端，**此后本行再也纠不回来**——改动前
    /// 每帧无条件覆盖时下一帧矩形会自动自愈。存值后锁定分支改为把 `cs` 恢复成本值，
    /// 那个前提重新成立，另外两个写入者一行都不用动。
    ///
    /// ★ 为什么不能复用 `composition_start.2`：那一位是**路径 A**（DLL 上报的 `compStart`）的锁，
    /// 而新组合第一帧的 `compStart` 常常是陈旧的——Tabby 实测新组合第一帧路径 A 锁进上一组合的
    /// 末端 `453`，随后由矩形修正成真起点 `442`。绑在 `.2` 上会把这次**必要的修正**一并挡掉，
    /// 错位 11px 且不自愈。本位只锁矩形自己那条路，两条路各锁各的。
    ///
    /// ⚠ **已知盲区**：同一行内**合法**的起点变化（终端行内重排——D 类记着 WindTerm 同行左移
    /// 312px；单行编辑框组合超宽后横向滚动）在本组合内同样不再跟随，要等换行或组合结束才恢复。
    /// 没有收窄成「只挡小幅右移」是刻意的：那要回到「用位置关系判断这一帧准不准」，而那条路的
    /// 四个变体全被真机推翻过（见 `handle_caret_probe` 里的清单）。宁可要一个**形状确定**的盲区。
    ///
    /// 清位与 `composition_start` 同步：组合结束（`reset_first_show`）与焦点换 docMgr 各一处，
    /// 少清一处就会跨组合钉死。
    pub(crate) locked_rect_anchor: Mutex<(i32, i32, bool)>,
    /// 宿主**上一帧如实上报**的组合起点 (x, y, seen)。只由 `handle_caret_update` 按帧写入，
    /// 且只在本帧 compStart 非零时更新——零值表示「宿主这帧没报」，不是「起点变成了 0」。
    ///
    /// ★ 它与 `composition_start` 是**两个语义**，不可互相替代：`composition_start` 有三个
    /// 写入者（宿主上报、空闲缓存首显、微移路径的 `shown_anchor` 回灌），后两者写进去的是
    /// 「候选窗该画在哪」而非「宿主说的起点在哪」。QQ 实测（2026-09-05 09:51）宿主报的
    /// compStart 全程恒为 (2268,1070)，而 `composition_start` 被首显缓存写成 (2266,1072)
    /// ——差 2px，足以让任何拿 `composition_start` 与 reported compStart 做相等比较的判据
    /// 整条失效。要问「宿主是否说起点没动」，只能问本字段。
    ///
    /// 无需跨组合显式复位：换了组合，宿主报的起点自然与上一帧不同，判据随之不成立。
    last_reported_comp_start: Mutex<(i32, i32, bool)>,
    /// 本组合内是否已收到过 TSF 域的 caret 帧。置位后，`GUI_CARET` 帧一律不再采信。
    ///
    /// 背景：Word 在按键期可拒绝同步 edit session（`TS_E_SYNCHRONOUS`），DLL 回退到 Win32
    /// 光标上报——那是个跨窗口的陈旧位置（实测恒在别的行），不带组合起点与组合矩形。它若
    /// 进入组合期的跟行 / 重锁，候选窗就在它与 TSF 真坐标之间来回跳。
    ///
    /// 只在「已有 TSF 坐标」后才屏蔽：纯 GUI 回退的宿主（无 TSF 坐标可言）行为不变。
    /// 清位与 `composition_start` 同步：组合结束（`reset_first_show`）与焦点换 docMgr 各一处。
    tsf_frame_seen_in_composition: std::sync::atomic::AtomicBool,
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
    ///
    /// 第三项（[`ModeScope::entry_windows`]）是命中的带窗口条件的进入类规则：同进程内它变了也算
    /// 「切进来」（AutoHotkey 两个 GUI 窗口各配各的 initial_mode），见 [`focus_crossed`]。
    pub(crate) mode_scope: Mutex<ModeScope>,
    /// `client_token → 最近一次 focus_gained 的窗口`（+ `active_compat` 当前的窗口），见
    /// `coordinator/window_ctx.rs`。
    pub(crate) focus_windows: Mutex<window_ctx::FocusWindows>,
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
    /// 主题搜索层：各资源层的 `themes/` 目录，靠前者优先（user > custom > data）。
    ///
    /// 带层名而不是裸路径列表：`list_themes_full` 要靠层名区分「内置（随安装包分发，
    /// 不可删）」与「用户自带」，靠路径前缀猜会在便携版/自定义 data 目录下猜错。
    pub(crate) theme_layers: Vec<wind_config::ResourceLayer>,
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
    /// `toggle_schema:<id>` 的**去程记录**：从哪个方案按进来、送达时的落点、用的哪个键。
    ///
    /// 刻意只存运行时、不落配置：它描述的是「用户此刻的往返意图」，不是偏好。持久化会让
    /// 重启后第一次按跳到一个用户早忘了的方案——那正是「回到来源」这个语义最容易失信的
    /// 时刻。无有效记录时按 `toggle_schema` 到已在的方案是 no-op（不切走）。
    ///
    /// # 判据分两层：代际决定「来源还算不算数」，落点决定「这次是回程还是重新落地」
    ///
    /// ★ 早期只有代际一层，于是**去程之后在目标方案里切中英、开大写时代际不动、记录照旧
    /// 有效**，再按就把用户送回来源。真机报障原话：「从五笔一键切到英文方案，又用别的方式
    /// 切了英文状态，再按却切回了五笔」。根因是「来源还算不算数」与「这次该回程还是该去
    /// 程」两个问题压在同一个判据上，必错一个。现按三分支裁决（见 `toggle_schema_by_id`）：
    ///
    /// - 代际不等 ⇒ 期间用别的方式切过方案，来源已是几步之前的地方，**作废且不动作**
    ///   （此时没有任何依据说明用户想去哪，随便挑一个会把往返键变成随机跳转键）。
    /// - 代际相等、落点整体不等 ⇒ 用户没离开过目标方案，只是中英态/大写被扰动了。来源
    ///   仍然成立，本次按键退回本义「去目标」——重新落地一次，**来源保留**，再按仍回得去。
    /// - 落点整体相等 ⇒ 回程。
    ///
    /// # 代际为什么是代际，而不是在切方案时清空
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
    /// [`SchemaToggleOrigin::trigger_vk`] 是**触发键 VK**（0 = 非方案级绑定触发，如全局
    /// 组合热键）。有它，往返才真正「不依赖目标方案的配置」：去程后该键在目标方案里临时
    /// 获得往返语义，哪怕目标方案的 `[key_actions]` 是空的。
    ///
    /// ★ 没有这一项时，「五笔按 RShift 去英文方案」要求英文方案**自己也配一遍** RShift
    /// 才回得来——设计文档 §5 原本断言 `toggle_schema` 对锁死「从结构上免疫」，那只覆盖了
    /// 「回到哪」，没覆盖「怎么按得动」。测试里复现过。
    pub(crate) schema_toggle_origin: Mutex<Option<SchemaToggleOrigin>>,
    /// **单向** `switch_schema` 的送达记录：`(触发键 VK, 送达时的方案代际)`。
    ///
    /// 用途**只有一个**：目标方案里再按这把键时**吞键**，不让它漏回全局链。
    /// 方案级 `[key_actions]` 按活跃方案查表，单向切走后目标方案没有这条绑定 ⇒ 走到
    /// `NotBound` ⇒ 若就此返回 `None`，键会落到 `is_toggle_mode_keycode`，而
    /// `lshift`/`rshift` 出厂就是 `toggle_mode` 键 ⇒ **用户配的是「切方案」却切了中英文**。
    /// 这正是方案级单向曾被整条禁掉的理由；改为放行 + 本记录兜底后，那个后果不再成立。
    ///
    /// ★ **刻意不复用 [`SchemaToggleOrigin`]**：那个结构的每个字段都为**回程**服务
    /// （`origin` 是回程目标、`landing` 是「该回程还是该重新落地」的判据），而单向记录
    /// 两样都不需要——它只回答「这把键刚把用户送到这儿吗」。塞进同一个结构等于让
    /// `origin` 对一半记录无意义，是「一条记录承载两种语义」的开端。
    ///
    /// 代际用于失效：期间用别的方式切过方案 ⇒ 记录作废 ⇒ 该键恢复它原本的语义
    /// （与往返记录的失效判据同源，理由见 `schema_toggle_key_authorized`）。
    pub(crate) schema_switch_arrival: Mutex<Option<(u32, u64)>>,
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
    /// 下一词学习的上文：最近一次**选词整体上屏**的词（简体）。默认断开：任何上屏记账
    /// （`record_commit`）、焦点变化、宿主自行出字 / 挪光标、取消、撤销上屏都清空，只有
    /// `commit_selected` 接得上——只记真正相邻的两个词，见 [`Self::learn_next_word`]。
    pub(crate) next_word_prev: Mutex<Option<String>>,
    /// 光标前的正文（末尾若干字），联想「下一词」的前文。宿主报真值
    /// （[`Self::set_preceding_text`]），核心自己上屏 / 删字时随手跟上（[`Self::note_host_edits`]），
    /// 两次报告之间也不脱节。桌面宿主不报，恒空。
    pub(crate) preceding_text: Mutex<String>,
    /// 撤销上屏（`ime.undo_commit`）删除量：最近一次「同步落到光标前」的字符数（UTF-16 单元，
    /// 与 TSF ShiftStart / macOS NSRange 同量纲）。**刻意与 `recent_commits` 分离**——历史队列
    /// 记「上过什么」（供 last/加词，深度 16），本值记「光标前紧邻的还是不是它、有几个字」这一
    /// 时效态。默认 1 → undo 永远有动作；每次上屏经 `note_commit_action` 覆盖 → 只有「刚输入完
    /// 那次」精准删多个；撤销一次即复位 1、焦点变化亦复位 → 之后回落删 1（宁可少删多按几次，
    /// 也不按陈旧计数误删多个）。
    pub(crate) last_commit_len: std::sync::atomic::AtomicUsize,
    /// 最近一次上屏的文本开头是不是 `crate::auto_spacing` 补的空格。造词草稿流要把它剥掉：
    /// 草稿窗口遇到非汉字就断流并丢掉整笔，不剥的话交界后的第一个字永远进不了草稿。
    pub(crate) auto_space_prefixed: std::sync::atomic::AtomicBool,
    /// 编码显示方式运行时态（命令栏 ime.toggle("preedit") 循环切换；初值随配置）。
    /// 统一权威：决定候选窗是否显示 preedit（in_app→不显示）及是否内联首单元（embedded）。
    pub(crate) preedit_display: Mutex<PreeditDisplay>,
    /// 方案级通用 UI 覆盖的折叠缓存，见 `schema_ui.rs`。
    pub(crate) eff_candidate_cache: Mutex<Option<crate::schema_ui::EffCandidateCache>>,
    /// 候选窗隐藏开关（命令栏 ime.toggle("candwin") 切换；隐藏时 notify_ui_update 不显示候选）。
    hide_candidate_window: Mutex<bool>,
    /// 候选布局方向运行时态（命令栏 ime.toggle("layout") 切换；true=竖排，初值随配置，持久化）。
    ///
    /// 这是布局方向的**基线真相源**——模式级覆盖（`layout.rs`）在它之上叠加，不改写它。
    pub(crate) candidate_orientation: Mutex<Orientation>,
    /// 上次真正下发给 UI 的候选方向（`layout.rs` 的去重缓存，避免每次按键重发致重排抖动）。
    /// 与 `candidate_vertical` 的区别：后者是基线，本字段是**叠加模式意图后实际生效**的值。
    pub(crate) candidate_layout_sent: Mutex<Orientation>,
    /// 上次下发的方案级候选字族（去重用）。见 `sync_candidate_font`。
    pub(crate) candidate_font_sent: Mutex<String>,
    /// 输入统计采集器（内存聚合 + 后台 flush，与 store 共享 Arc）；None=无持久化/headless。
    pub(crate) stat_collector: Option<StatCollector>,
    /// 本次按键是否已被具体上屏路径记录统计（AtomicBool，避免与 state 锁冲突致死锁）。
    pub(crate) stat_recorded: std::sync::atomic::AtomicBool,
    /// 本次按键命中了会话态动词 `commit_code`，出口处要把它当成一次回车重新分派。
    /// 见 [`Coordinator::redispatch_commit_code`]。
    pub(crate) commit_code_pending: std::sync::atomic::AtomicBool,
    /// 正在为 `commit_code` 重新分派回车：这一下回车恒上屏编码，不看 `input.enter_behavior`。
    pub(crate) commit_code_forced: std::sync::atomic::AtomicBool,
    /// 统计事件捕获（仅测试）：`None` = 不捕获（生产恒 `None`，零增长）；
    /// `debug_capture_stat_events` 置 `Some` 后，每次 `record_commit*` 追加 (来源, 文本)。
    pub(crate) debug_stat_events: Mutex<Option<Vec<(CommitSource, String)>>>,
    /// 全屏状态缓存：由 notify_toolbar_async 在后台线程异步刷新，notify_toolbar 直接读取，
    /// 消除 bridge handler 线程上的 SHQueryUserNotificationState 阻塞。
    ///
    /// ⚠ 这一位的**含义随写者而异**，读它之前先想清楚：焦点事件那条（`commit_fullscreen_kind`）
    /// 写的是「任意全屏形态」（含 D3D 独占 / 演示模式），周期复查那条
    /// （`commit_fullscreen_covering`）写的只是判据②「铺满显示器」——后者刻意不问判据①，
    /// 因为那要跨进程问 shell，不能按拍反复发。两者可在「演示模式已置但窗口未铺满」这类
    /// 罕见格上给出不同的值。目前唯一的生产读者是 `notify_toolbar` 的 `hide_fullscreen`，
    /// 它只关心「要不要收工具栏」，对这点差异不敏感；独占那一侧另有
    /// `fullscreen_exclusive_cached` 与 UI 线程的按需闸负责。**新增读者前先确认你要的是哪个**。
    pub(crate) fullscreen_cached: std::sync::atomic::AtomicBool,
    /// 全屏探测的单飞闸：已有探测在途时跳过新的。焦点变化是成串来的，而探的是同一个
    /// 全局前台状态，此前每次都 spawn 一个线程。
    ///
    /// 两个获取点（焦点事件的 `notify_toolbar_async`、复查线程的 `ensure_fullscreen_watch`）
    /// 都经 `try_take_probe_gate`，归还统一由 `ProbeGate` 的 `Drop` 负责——**不要**直接写它。
    pub(crate) fullscreen_probing: std::sync::atomic::AtomicBool,
    /// 全屏复查线程是否已起——懒启动的单次闸，见 `coordinator/fullscreen_watch.rs`。
    #[cfg(windows)]
    pub(crate) fullscreen_watch_started: std::sync::atomic::AtomicBool,
    /// 前台是否 **D3D 独占**全屏（`FullscreenKind::D3dExclusive`），与 `fullscreen_cached`
    /// 同一探测线程刷新。独立成位是因为后果不同：它压的是候选窗（弹了会把游戏踢出独占态），
    /// 而 `fullscreen_cached` 只管工具栏。见 `handle_uielement.rs`。
    pub(crate) fullscreen_exclusive_cached: std::sync::atomic::AtomicBool,
    /// 接管了候选绘制的宿主进程（TSF UI-less / `pbShow=FALSE`），按 pid 记账。
    /// 焦点落在其中任一进程时不弹本地候选窗。写入见 `handle_uielement.rs`。
    pub(crate) uielement_host_pids: Mutex<std::collections::HashSet<u32>>,
    /// 实际读走过候选串的进程（`UIELEMENT_FLAG_HOST_READS`）。与上面那张分开：
    /// 那是宿主的声明，这是推断，只有这张受 compat `host_drawn_candidates` 管，
    /// 且 2026-09-15 起是 opt-in（默认不收窗）。
    pub(crate) uielement_reader_pids: Mutex<std::collections::HashSet<u32>>,
    /// 「当前在输入的进程」：焦点/激活事件与**每个按键**都会写它（bridge 按管道对端 pid）。
    /// 与 `active_compat.pid` 的区别：那份只由焦点/激活事件维护，而游戏这类宿主常常没有
    /// 可编辑 TSF 上下文、`focus_gained` 一次都不来——按键才是「谁在输入」的最强证据
    /// （host-render 的 `note_focus` 是同一条理由的先例）。0 = 未知。
    pub(crate) focus_pid: std::sync::atomic::AtomicU32,
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
    /// 扩展程序事件出口与会话状态，见 `hook_events`。
    pub(crate) hook: Arc<wind_hook::tap::Tap>,
    pub(crate) hook_session: std::sync::Mutex<crate::hook_events::HookSession>,
    pub(crate) extensions_config_hook:
        std::sync::OnceLock<crate::hook_events::ExtensionsConfigHook>,
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
    /// 当前页每个候选的悬停提示（含原始行），每次候选页组装时整体覆盖。
    pub(crate) tooltip_page: Mutex<Vec<crate::handle_tooltip::TooltipPageEntry>>,
    /// 悬停提示右键菜单弹出时的目标快照；菜单动作执行前拿它核对候选有没有变。
    pub(crate) tooltip_menu_target: Mutex<Option<crate::handle_tooltip::TooltipMenuTarget>>,
    /// Linux：开着的菜单是候选右键菜单（`State::menu_target_text` 的无锁镜像）。
    ///
    /// 只给 `notify_ui_hide` 用：它的不少调用点正持着 `state` 锁，在里面再取锁就是自锁死
    /// （实测：服务端卡死、每个键 2s 超时）。所以这里只读原子量、只发命令不碰 `state`，
    /// 复位 `menu_open` 交给 UI 回送的 `MenuClose`（见 `menu_host::MenuHost::hide`）。
    #[cfg(ext_presenter)]
    pub(crate) candidate_menu_open: std::sync::atomic::AtomicBool,
    /// 密码框抑制策略开关，`input.password_force_english` 的运行时镜像（构造与热重载时回灌，
    /// 见 `set_password_suppress_enabled`）。这是**全局**值：未配 per-app 规则的进程跟随它；
    /// 判定一律经 `password_force_english_for_pid`，不要直接读本字段做抑制决策。
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
    /// 按应用方案（compat.toml `schema`）的运行时态：全局方案、`@remember` 记忆表等，
    /// 见 `coordinator/app_schema.rs`。
    pub(crate) app_schema: app_schema::AppSchemaState,
}

/// 拆字资产当前生效状态：库的解析后绝对路径 + 已下发的字根字体（路径, DWrite 家族名）。
/// 变更检测用——库变了才重载反查表，字体变了才重发（渲染端每次 set 都重建字体集）。
#[derive(Default)]
pub(crate) struct ChaiziAssets {
    pub(crate) db: Option<std::path::PathBuf>,
    pub(crate) font: Option<(String, String)>,
    /// 运行期格式串触发的后台加载正在进行（单飞闸，见 `Coordinator::ensure_chaizi_async`）。
    pub(crate) loading: bool,
    /// 每次 sync 加一。后台加载换入前比对，过期（期间切了方案）就丢弃。
    pub(crate) generation: u64,
    /// 运行期路径本代次已经试过（含「方案没配拆字库 / 库文件不存在」）：不再每键重读方案
    /// 文件、每键告警。sync 时清掉，给换了方案之后一次重试。
    pub(crate) tried: bool,
    /// 当前这份表是运行期格式串按需装的（配置里没有拆字消费者）。sync 据此**不卸**它——
    /// 否则每次配置生效都卸、下一次 `dict.rev` 又装，来回读盘。方案的拆字库路径变了才卸。
    pub(crate) runtime: bool,
    /// 测试计数：运行期路径解析拆字库（读方案文件）的次数。
    #[cfg(test)]
    pub(crate) resolve_attempts: usize,
}

/// 拆字加载线程的 `loading` 复位守卫：线程中途 panic 也复位，否则单飞闸卡死、此后永不再试
/// （与 `wind_engine::text_codes` 的 `BuildingGuard` 同一做法）。
pub(crate) struct ChaiziLoadingGuard(pub(crate) std::sync::Weak<Coordinator>);

impl Drop for ChaiziLoadingGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.upgrade() {
            c.chaizi_assets
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .loading = false;
        }
    }
}

/// 读 `system.comments.toml`；缺失或解析失败时用内置副本并告警。
fn load_system_comments(data_dir: Option<&Path>) -> wind_config::comment_lib::SystemComments {
    let path = Config::resolve_data_file(data_dir, "system.comments.toml");
    let (sys, warning) =
        wind_config::comment_lib::SystemComments::load_or_embedded(path.as_deref());
    if let Some(w) = warning
        && (data_dir.is_some() || path.is_some())
    {
        warn!("{w}");
    }
    sys
}

/// 合并出厂与用户注释配置，问题逐条告警。
fn build_comment_lib(
    system: &wind_config::comment_lib::SystemComments,
    cfg: &Config,
) -> wind_config::comment_lib::CommentLibrary {
    let (lib, warnings) = wind_config::comment_lib::CommentLibrary::build(system, &cfg.ui.comment);
    for w in warnings {
        warn!("{w}");
    }
    lib
}

/// 读盘加载一个方向的简繁转换器（`to_traditional` = 简入繁出）。数据缺失返回 `None`。
///
/// 转换链里的**每本 octrie 各自按层序解析**（走 resolve_data_file 这个单文件收口点，
/// `opencc/<名>.octrie`），定制层只放一本 `STPhrases.octrie` 也能正常工作，缺的那本自动
/// 落回出厂。**不能整份目录胜出**——理由见 `Converter::load_variant_resolved` 的文档
/// （残链会一个字都不转，且无从察觉）。
pub(crate) fn load_converter(
    data_dir: Option<&Path>,
    config: &Config,
    to_traditional: bool,
) -> Option<wind_transform::s2t::Converter> {
    let variant = if !to_traditional {
        "t2s".to_string()
    } else if config.input.s2t.variant.is_empty() {
        "s2t".to_string()
    } else {
        config.input.s2t.variant.clone()
    };
    let t0 = std::time::Instant::now();
    let conv = wind_transform::s2t::Converter::load_variant_resolved(&variant, |file| {
        Config::resolve_data_file(data_dir, &format!("opencc/{file}"))
    });
    if conv.is_some() {
        info!(
            "Loaded converter (variant={variant}) in {} ms",
            t0.elapsed().as_millis()
        );
    } else {
        warn!("简繁数据缺失（variant={variant}），该方向不可用");
    }
    conv
}

/// 字根字体从 `sent` 变到 `want` 时要下发的 `(路径, 家族名)`；不用发时 `None`。
///
/// 变为「没有」时下发**空路径**＝撤掉，而不是什么都不发：渲染端的私用区字体位只有一个，
/// 且压过方案级 `[candidate] font_family`。以前只停发不撤，五笔（黑体字根）切到蒙古文
/// 方案后，蒙文的私用区码位仍被切到字根字体，画成字根或空白。
fn chaizi_font_to_send(
    sent: &Option<(String, String)>,
    want: &Option<(String, String)>,
) -> Option<(String, String)> {
    if sent == want {
        return None;
    }
    Some(want.clone().unwrap_or_default())
}

#[cfg(test)]
mod chaizi_font_to_send_tests {
    use super::chaizi_font_to_send;

    fn font(p: &str) -> Option<(String, String)> {
        Some((p.to_string(), "F".to_string()))
    }

    #[test]
    fn switching_to_a_schema_without_chaizi_font_clears_it() {
        assert_eq!(
            chaizi_font_to_send(&font("wubi86/HeiTiZiGen.ttf"), &None),
            Some((String::new(), String::new()))
        );
    }

    #[test]
    fn a_new_font_is_sent() {
        assert_eq!(chaizi_font_to_send(&None, &font("a.ttf")), font("a.ttf"));
        assert_eq!(
            chaizi_font_to_send(&font("a.ttf"), &font("b.ttf")),
            font("b.ttf")
        );
    }

    #[test]
    fn unchanged_sends_nothing() {
        assert_eq!(chaizi_font_to_send(&None, &None), None);
        assert_eq!(chaizi_font_to_send(&font("a.ttf"), &font("a.ttf")), None);
    }
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
                    seq_char: 0,
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

    /// 登记宿主对配置的强制改写（移动端：编码区归候选区、联想态空格不选词…），
    /// [`Self::reload_user_config`] 从磁盘重读后会再套一遍。
    ///
    /// 宿主在构造前已经改写过一次传进来的 `Config`，但那只管到第一次重载：设置页每改一项
    /// 就发一次重载，`Config::load` 读回的是盘上的值，强制项随之悄悄回到桌面默认——
    /// 表现为「改了任意一个设置之后，联想态空格又开始选词」，而且与改的那一项毫无关系。
    /// 只能设一次：第二次调用被忽略（返回 false），防止两处各登记一份互相覆盖。
    pub fn set_config_overlay(&self, overlay: fn(&mut Config)) -> bool {
        self.config_overlay.set(overlay).is_ok()
    }

    /// 暂停 / 恢复学习（隐私 / 无痕输入，见字段 `learning_suspended`）。
    ///
    /// 判断收口在各条**学习入口**（`record_selection_cased_in`、`learn_phrase_on_commit_in`、
    /// `bump_selected_temp_word`、`promote_draft_on_commit`、`note_draft_commit`、
    /// `learn_english_raw`、`record_assoc_pick`、`learn_next_word`、`record_commit_ks` /
    /// `handle_english_stats`、`learn_quick_history`、`learn_url_history`、
    /// `learn_email_suffix`、`push_commit_history`），而不是散在各个 store 写函数里：
    /// 用户**主动**的操作（加词、删词、置顶、清空）也走同一批 store 写函数，那是用户意图、
    /// 不是学习，暂停期间必须照常生效。读路径一概不看本开关。
    ///
    /// 切换时顺带打一个下一词断点：暂停前最后一个词不该和恢复后的第一个词连成一对。
    pub fn set_learning_suspended(&self, on: bool) {
        self.learning_suspended
            .store(on, std::sync::atomic::Ordering::Relaxed);
        self.note_word_boundary();
    }

    /// 当前是否暂停学习（见 [`Self::set_learning_suspended`]）。
    pub fn learning_suspended(&self) -> bool {
        self.learning_suspended
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 热重载成功后不弹「设置已更新」（失败、语法错误照常提示）。
    ///
    /// 电脑上改设置是在独立的设置程序里点「保存」，弹一句确认是合适的；手机设置页**每改一项**
    /// 就重载一次，连改几项就连弹几条，而且改动本身在设置页上就看得见（真机反馈）。
    pub fn set_quiet_reload_toast(&self, value: bool) {
        self.quiet_reload_toast
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }

    /// 临时压制某个键的「引导键」身份（`vk` 为 0 = 清除）：压制期间该键在
    /// `bound_action_for` 里按「未绑定」处理，空码时走普通标点管线。
    /// 只影响引导键绑定，不影响选词键等会话态按键。默认不压制，桌面路径从不调用。
    /// 宿主应在 `key_down` 前设置、之后立即清除（单线程串行调用）。
    pub fn set_trigger_suppressed(&self, vk: u32) {
        self.trigger_suppressed
            .store(vk, std::sync::atomic::Ordering::Relaxed);
    }

    /// 声明宿主报的光标坐标可直接当权威（见字段 `host_caret_trusted`）。
    pub fn set_host_caret_trusted(&self, value: bool) {
        self.host_caret_trusted
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }

    /// 声明焦点宿主能否显示应用内预编辑（见字段 `host_no_inline_preedit`）。
    pub fn set_host_inline_preedit(&self, supported: bool) {
        self.host_no_inline_preedit
            .store(!supported, std::sync::atomic::Ordering::Relaxed);
    }

    /// 宿主能否显示工具栏 / 软键盘面板。只有 Linux addon 要声明（见字段 `host_panels`），
    /// 其它形态的面板由本进程或 `.app` 自己画，恒为真。
    pub(crate) fn host_has_panels(&self) -> bool {
        #[cfg(all(target_os = "linux", ext_presenter))]
        {
            self.host_panels.load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(all(target_os = "linux", ext_presenter)))]
        {
            true
        }
    }

    /// 退出（重启服务、注销 / 关机、SIGTERM）前把统计与待处理的用户数据落盘：`process::exit`
    /// 与系统结束进程都不跑析构。
    /// 失败只 WARN——日志还在，下次启动照样回放。
    pub fn flush_user_data_before_exit(&self) {
        // 统计收集器的内存增量先落到统计文件
        if let Some(sc) = self.stat_collector.as_ref() {
            sc.flush();
        }
        if let Some(s) = self.store.as_ref()
            && let Err(e) = s.flush_user_data()
        {
            warn!("退出前用户数据落盘失败（日志仍在，下次启动回放）：{e:#}");
        }
    }

    /// 注销 / 关机（Windows `WM_ENDSESSION`）与 SIGTERM（Linux / macOS）时先落盘：服务 main
    /// 在协调器就绪后调一次。
    pub fn install_exit_flush(self: &Arc<Self>) {
        #[cfg(all(windows, feature = "desktop-ui"))]
        {
            let c = self.clone();
            wind_ui::window::set_session_end_hook(Box::new(move || {
                tracing::info!("会话结束：用户数据落盘");
                c.flush_user_data_before_exit();
            }));
        }
        // Android 的输入法跑在应用进程里，进程生命周期归系统管，不接 SIGTERM。
        #[cfg(all(unix, not(target_os = "android")))]
        {
            let c = self.clone();
            crate::exit_signal::install_sigterm(Box::new(move || {
                c.flush_user_data_before_exit();
                std::process::exit(0);
            }));
        }
    }

    /// 外部宿主（Linux addon / macOS `.app`）的 push 连接每（重）建立一次，就让渲染端复位它记着的
    /// 宿主状态：Linux 补推可见的工具栏 / 软键盘（addon 收到 SERVICE_READY 会丢掉手里的面板，而同一
    /// 服务换了连接时面板状态并没有变，不会自己再推）；macOS 作废皮肤换帧的分层去重（新 `.app` 手里
    /// 什么都没有）。服务 main 在 `PushServer::mark_ready` 之后调一次；其它形态是空操作。
    pub fn install_host_reconnect_hook(&self) {
        #[cfg(ext_presenter)]
        {
            let tx = self.ui_tx.clone();
            self.push_server
                .set_client_connected_hook(Box::new(move |_token| {
                    let _ = tx.send(wind_ui_types::UiCommand::HostReconnected);
                }));
        }
    }

    /// 构造时选定的输入档位。
    pub fn profile(&self) -> crate::input_profile::InputProfile {
        self.profile
    }

    /// 只读模式的原因（用户数据设计 §7.1 降级保护）；`None` = 正常读写。
    pub fn storage_readonly_reason(&self) -> Option<&str> {
        self.storage_readonly.as_deref()
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
        Self::build_with_readonly(
            config,
            data_dir,
            push_server,
            ui_tx,
            user_dir,
            store,
            override_dir,
            None,
            crate::input_profile::InputProfile::desktop(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_with_readonly(
        config: Config,
        data_dir: Option<&Path>,
        push_server: Arc<PushServer>,
        ui_tx: crate::UiSender,
        user_dir: Option<std::path::PathBuf>,
        store: Option<Arc<Store>>,
        override_dir: Option<std::path::PathBuf>,
        storage_readonly: Option<String>,
        profile: crate::input_profile::InputProfile,
    ) -> Arc<Self> {
        Self::build_with_session(
            config,
            data_dir,
            push_server,
            ui_tx,
            user_dir,
            store,
            override_dir,
            storage_readonly,
            profile,
            None,
        )
    }

    /// 同 [`Self::build_with_readonly`]，另给中立输出通道（`new_session` 用）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_with_session(
        config: Config,
        data_dir: Option<&Path>,
        push_server: Arc<PushServer>,
        ui_tx: crate::UiSender,
        user_dir: Option<std::path::PathBuf>,
        store: Option<Arc<Store>>,
        override_dir: Option<std::path::PathBuf>,
        storage_readonly: Option<String>,
        profile: crate::input_profile::InputProfile,
        session_tx: Option<crate::session_output::SessionSender>,
    ) -> Arc<Self> {
        // 注入 redb Store：码表引擎注册用户词/临时词层，用户词进候选合并。
        // override_dir 为 None 时由 EngineManager 取默认（用户配置目录下的 schema_overrides）。
        // 覆盖层在构造时传入（引擎一次建成）；override_dir 为 None 取默认目录。
        let engine_mgr = EngineManager::with_store_override_overlay(
            &config,
            data_dir,
            store.clone(),
            override_dir.or_else(EngineManager::default_override_dir),
            profile.config_overlay,
        );
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
        let system_comments = load_system_comments(data_dir);
        let comment_lib = build_comment_lib(&system_comments, &bundle.config);
        // 按需数据（拆字表、简繁表）只装有消费者的，见 `crate::data_needs`。
        let needs = crate::data_needs::DataNeeds::derive(
            &bundle.config,
            &comment_lib,
            &crate::data_needs::SchemaFacts::collect(&engine_mgr),
            profile.config_overlay,
        );
        // 模板引用 `${correct_pinyin}` 时拼音引擎要算提示（只翻开关，不重建引擎）。
        engine_mgr.set_correct_pinyin_in_templates(needs.correct_pinyin);

        // 短语层（方案 B）：TOML 变更时同步进 store，再从 store（仅 enabled）建层。
        // 启动解析的条目缓存进结构体，作为"恢复默认"重读文件失败时的回退。
        let mut system_phrase_entries: Vec<wind_phrase::SystemPhraseEntry> = Vec::new();
        // 用户目录同名文件整体替代安装目录那份（覆盖替换，非合并）。
        // ⚠️ 解析在此**一次定死**：后续 `current_system_phrase_entries` 的重读走同一路径，
        // 故运行时新放的覆盖文件要下次启动才生效（与全仓其它覆盖点一致，无文件监视）。
        let assoc_bigram_path = Config::resolve_data_file(data_dir, "assoc/assoc_bigram.txt");
        let system_phrase_path = Config::resolve_data_file(data_dir, "system.phrases.toml");
        if system_phrase_path.is_none() && data_dir.is_some() {
            warn!("system.phrases.toml 缺失（用户/安装目录均未找到），系统短语将为空");
        }
        let phrases = {
            if let Some(store) = store.as_ref() {
                {
                    // 没有 system.phrases.toml（定制版可能删掉）⇒ 系统短语为空，但照样交给 store：
                    // 短语迁移要在这里触发，否则旧表里的用户短语永远迁不过来、看不见。
                    let entries = system_phrase_path
                        .as_ref()
                        .map(|p| wind_phrase::PhraseLayer::parse_system_entries(p))
                        .unwrap_or_default();
                    // 内容哈希：条目稳定序列化后哈希
                    let hash = phrase_entries_hash(&entries);
                    let sys: Vec<wind_store::phrases::SystemPhrase> = entries
                        .iter()
                        .map(|e| wind_store::phrases::SystemPhrase {
                            code: e.code.clone(),
                            text: e.text.clone(),
                            weight: e.weight,
                            position: e.position,
                            category: e.category.clone(),
                            enabled: e.enabled,
                        })
                        .collect();
                    // 系统短语只在内存（P3 叠加模型），每次启动都交给 store；首次还会触发旧表迁移。
                    // 没有 TOML 时不动 store 里的系统条目，只确保旧表迁移照样做。
                    let r = if system_phrase_path.is_some() {
                        store.set_system_phrases(&sys)
                    } else {
                        store.migrate_phrases_now()
                    };
                    if let Err(e) = r {
                        warn!("系统短语设定 / 短语迁移失败: {e}");
                    }
                    // 短语迁移刚完成就升版本：等到下次启动才升，中间回退的旧版本会继续往旧表写
                    if let Some(dir) = store.path().parent()
                        && dir.join("meta.toml").is_file()
                    {
                        crate::construct::finish_format_upgrade(store, dir);
                    }
                    // TOML 改了（哈希不一致）才同步：修改行回到新 TOML 值、已删条目的修改行清掉。
                    if system_phrase_path.is_some()
                        && store.phrase_sys_hash().ok().flatten().as_deref() != Some(hash.as_str())
                        && let Ok(st) = store.sync_system_phrases(&sys)
                    {
                        info!(
                            "Synced system phrases: +{} ~{} -{}",
                            st.added, st.updated, st.removed
                        );
                        let _ = store.set_phrase_sys_hash(&hash);
                    }
                    system_phrase_entries = entries;
                }
                let recs = store
                    .enabled_phrases_for_input()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| wind_phrase::PhraseSeed {
                        code: p.code,
                        text: p.text,
                        weight: p.weight,
                        position: p.position,
                        is_system: p.is_system,
                        category: p.category,
                    });
                std::sync::RwLock::new(wind_phrase::PhraseLayer::from_records(recs))
            } else {
                std::sync::RwLock::new(wind_phrase::PhraseLayer::default())
            }
        };

        // 简繁转换器（解析规则见 `load_converter`）：只预载**开着的**方向（两份合计约
        // 1.8 MB）。关着的方向在切换入口（热键 / 菜单 / 工具栏 / 命令栏）先同步加载再切，
        // 见 `ensure_converter`——切换是一次性的用户动作，不是逐键路径。
        let s2t = needs
            .s2t
            .then(|| load_converter(data_dir, &config, true))
            .flatten();
        let t2s = needs
            .t2s
            .then(|| load_converter(data_dir, &config, false))
            .flatten();

        // 词频已迁 redb（self.store 的 FREQ 表，选词时 record_freq）。

        // 标点配对表（中/英）已在 ConfigBundle 内构建。

        // 通用规范汉字表（检索范围"常用字"判定）。用户目录同名文件整体替代（见
        // docs/architecture/user-override.md）——自定义"常用字"范围是这张表的主要用途。
        // 全表与判定同源：都来自装配好的字符类 registry（常用字表在
        // `charsets/common_han.yaml`）。两处各读一份文件的话，用户换了字表却只有一半
        // 生效——而那半是哪半，取决于他改的是哪个文件。
        let common_chars = wind_candidate::CommonChars::from_registry(&engine_mgr.charsets());
        if common_chars.is_empty() {
            warn!("common_chars.txt 缺失，检索范围过滤将退化为不过滤");
        } else {
            info!("Loaded common chars table");
        }

        // 候选反查表（拆字/拼音）：拆字库路径取自主码表方案 [engine.chaizi].db_path（相对 schemas/，
        // 用户方案目录优先——第三方方案的拆字库只在用户目录下）。
        // 只在有消费者时加载（`DataNeeds::chaizi`）；运行期格式串另走首次使用时后台加载。
        let chaizi_db = needs
            .chaizi
            .then(|| engine_mgr.chaizi_spec())
            .flatten()
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

        // 软键盘映射表：出厂画布 ⊕ 定制层按面覆盖 ⊕ 用户层按面覆盖（分层理由见函数文档）。
        let softkeyboard = crate::handle_softkeyboard::load_softkeyboard_table(data_dir, true);

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
        let toolbar_anchors_init = runtime_state.toolbar_anchors.clone();
        let softkeyboard_anchors_init = runtime_state.softkeyboard_anchors.clone();
        // 在 `store` 被 move 进结构体之前取：字段字面量按书写顺序求值，`state_writer`
        // 排在 `store` 之后，那时已经借不到了。
        let persists_state = store.is_some();
        // 全局方案初值取引擎解析后的活跃方案（含定制版隐藏降级），而不是 `schema.active` 原值。
        let global_schema_init = engine_mgr.active_schema_id();
        let app_schemas_init = runtime_state.app_schemas.clone();
        // 软键盘上次停在哪一面：按**面 id** 还原（面表来自配置，两次运行之间可能增删面，
        // 存下标必然指到别处）。id 找不到就当没记录过、开在第一面——那比默默开到一个
        // 陌生的面好。
        let softkeyboard_page_init = softkeyboard
            .index_of(&runtime_state.last_softkeyboard_page)
            .unwrap_or(0);
        // 主题目录按层序展开（user > custom > data），各层的 `themes/`。
        let theme_layers: Vec<wind_config::ResourceLayer> =
            Config::resource_layers_named_with(data_dir)
                .into_iter()
                .map(|l| l.sub("themes"))
                .collect();
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
        let candidate_orientation_init = Orientation::from_layout_str(&config.ui.candidate.layout);

        // 候选窗显隐运行时初值（ui.candidate.hide_window；此前恒为 false，配置不生效）。
        let hide_candidate_window_init = config.ui.candidate.hide_window;

        // 密码框强制英文开关初值（input.password_force_english；此前恒为 true，菜单关掉重启即复原，t197）。
        let password_suppress_enabled_init = config.input.password_force_english;

        // 统计采集器：与 store 共享 Arc，内存聚合 + 后台定时 flush。
        let stat_collector = store
            .clone()
            .map(|s| StatCollector::new(s, config.stats.speed_factor));
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
        let (capslock_press_tx, capslock_press_rx) =
            std::sync::mpsc::channel::<wind_keys::capslock_hook::HookEvent>();
        // 简繁两方向的生效口径（互斥，冲突时 s2t 赢）收在 config 层的单一函数里，
        // 与 reload 那处同源——散着写迟早漏改一处。
        let (conv_s2t, conv_t2s) = config.input.conversion_directions();
        let coordinator = Arc::new(Self {
            state: Mutex::new(State {
                chinese_mode: init_chinese,
                full_width: init_full,
                chinese_punct: init_punct,
                spacing_prev: wind_punct::spacing::CharClass::Other,
                // 哨兵：与任何真实代际都不相等，保证首次 sync_schema_scope 必执行。
                schema_scope_gen: u64::MAX,
                punct_before_schema: None,
                layout_manual: None,
                s2t_enabled: conv_s2t,
                t2s_enabled: conv_t2s,
                filter_mode: wind_candidate::FilterMode::from_config(&config.input.filter_mode),
                scope_relaxed: false,
                // 启动时没有临时态：字词范围走配置层（方案级 → 全局）。热键切过才置 Some。
                single_char_override: None,
                toolbar_visible: config.ui.toolbar.visible, // 启动初值来自配置(运行时可菜单切换)
                ime_active: false, // 启动未激活：工具栏待 IME_ACTIVATED/FocusGained 才显示
                has_edit_context: false, // 同上：焦点尚未落到任何可编辑控件
                focus_no_edit_ctx: false, // 尚无权威判定，不表态
                caps_lock: false,
                numpad_origin: false, // 每次按键入口无条件重写，此处只是占位初值
                input_buffer: String::new(),
                input_buffer_cased: String::new(),
                english_case_variant: crate::english_candidates::CaseVariant::default(),
                input_cursor_pos: 0,
                leading: None,
                preedit: String::new(),
                preedit_split_body: String::new(),
                preedit_fp_body: String::new(),
                preedit_abbrev_body: String::new(),
                preedit_codetable_body: String::new(),
                direct_aux_body: String::new(),
                direct_aux_prev: None,
                shadow_code: String::new(),
                sentence_pool: Vec::new(),
                sentence_window: 0,
                sentence_base: Vec::new(),
                shortcode_tops: [const { None }; 3],
                expanded_group: None,
                candidates: Vec::new(),
                selected_index: 0,
                current_page: 0,
                grid_top: None,
                grid_keep: false,
                comment_session_flip: false,
                comment_hold_flip: false,
                alt_track: None,
                ctrl_track: None,
                grid_tab: wind_ui_types::GridTab::All,
                grid_full: Vec::new(),
                grid_tab_anchor: None,
                paged: false,
                candidate_input: String::new(),
                candidate_limit: 0,
                has_more: false,
                committed_text: String::new(),
                committed_segs: Vec::new(),
                active: None,
                overlay_body: String::new(),
                overlay_forms: OverlayPreeditForms::default(),
                temp_pinyin_buffer: String::new(),
                temp_pinyin_cursor: 0,
                temp_pinyin_schema: String::new(),
                temp_pinyin_prefix: String::new(),
                temp_english_buffer: String::new(),
                temp_english_cursor: 0,
                temp_english_prefix: String::new(),
                url_buffer: String::new(),
                email_buffer: String::new(),
                email_cursor: 0,
                url_cursor: 0,
                unicode_buffer: String::new(),
                unicode_cursor: 0,
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
                #[cfg(ext_presenter)]
                menu_touched_at: None,
                menu_target_page_local: 0,
                menu_target_text: String::new(),
                add_word_active: false,
                add_word_chars: Vec::new(),
                add_word_len: 0,
                add_word_code: String::new(),
                add_word_boundary: 0,
                add_word_clip: None,
                add_word_from_clip: false,
            }),
            push_server,
            rt: std::sync::RwLock::new(std::sync::Arc::new(bundle)),
            ui_tx,
            engine_mgr,
            profile,
            session_tx,
            store,
            storage_readonly,
            punct: Mutex::new(punct_conv),
            capslock_hook: Mutex::new(None),
            capslock_press_tx,
            hook_caps: std::sync::atomic::AtomicBool::new(false),
            hook_alt_mask: std::sync::atomic::AtomicU32::new(0),
            caps_press_claimed: std::sync::atomic::AtomicBool::new(false),
            smart_symbol: Mutex::new(SmartSymbolArm::default()),
            draft_window: Mutex::new(crate::draft_window::DraftWindowBuf::new()),
            draft_queue: Mutex::new(Vec::new()),
            draft_flushing: std::sync::atomic::AtomicBool::new(false),
            last_self_commit: Mutex::new(None),
            auto_phrase_writes: std::sync::atomic::AtomicUsize::new(0),
            phrases,
            system_phrase_entries: std::sync::RwLock::new(system_phrase_entries),
            system_phrase_path,
            assoc_bigram_path,
            assoc_bigram: std::sync::Arc::new(std::sync::OnceLock::new()),
            assoc_bigram_loading: std::sync::atomic::AtomicBool::new(false),
            system_comments,
            comment_lib: std::sync::RwLock::new(Arc::new(comment_lib)),
            s2t: Mutex::new(s2t),
            t2s: Mutex::new(t2s),
            // 只含出厂基表：用户覆盖住在 store 里，而 store 在本结构体构造之后才可用，
            // 故由 new() 里的 `reload_common_chars` 补灌（与 `quick_adjust` 同一套路）。
            common_chars: std::sync::Arc::new(std::sync::RwLock::new(common_chars)),
            toolbar_anchors: Mutex::new(toolbar_anchors_init),
            softkeyboard_anchors: Mutex::new(softkeyboard_anchors_init),
            current_toolbar_monitor: Mutex::new(None),
            #[cfg(all(target_os = "linux", ext_presenter))]
            host_output: Mutex::new(None),
            #[cfg(all(target_os = "linux", ext_presenter))]
            host_work: Mutex::new(None),
            #[cfg(all(target_os = "linux", ext_presenter))]
            panel_drop_output: Mutex::new(std::collections::HashMap::new()),
            #[cfg(all(target_os = "linux", ext_presenter))]
            host_panels: std::sync::atomic::AtomicBool::new(false),
            // 判据借 `store`：它的文档本来就写着「None = 无持久化（headless 测试）」。
            // 只看 `state_dir()` 挡不住测试夹具——那是进程外的全局路径，夹具同样取得到。
            state_writer: state_writer::StateWriter::new(persists_state, Config::state_dir()),
            reverse: std::sync::RwLock::new(reverse),
            aux_code_runtime: std::sync::RwLock::new(None),
            // 空初值 + 下面 new() 里的 sync_emoji_dict 首次加载：与注释库/拆字字体同一套
            // 「声明式变更」写法，启动与后续 reload 走**同一条**代码路径。
            emoji_dict: std::sync::RwLock::new(None),
            emoji_spec: Mutex::new(None),
            aux_code_key_warned: std::sync::atomic::AtomicBool::new(false),
            quick_formats,
            softkeyboard: std::sync::RwLock::new(Arc::new(softkeyboard)),
            softkeyboard_active: std::sync::atomic::AtomicBool::new(false),
            softkeyboard_page: std::sync::atomic::AtomicUsize::new(softkeyboard_page_init),
            softkeyboard_page_saved: std::sync::Mutex::new(
                runtime_state.last_softkeyboard_page.clone(),
            ),
            softkeyboard_dirty: std::sync::atomic::AtomicBool::new(false),
            last_pushed_hotkey_session: std::sync::atomic::AtomicBool::new(false),
            hotkey_session_force_push: std::sync::atomic::AtomicBool::new(false),
            softkeyboard_opened_at: std::sync::Mutex::new(None),
            softkeyboard_host_pid: std::sync::atomic::AtomicU32::new(0),
            softkeyboard_pinned: std::sync::atomic::AtomicBool::new(
                runtime_state.softkeyboard_pinned,
            ),
            // 空初值：真正的装载在 new() 里经 `reload_quick_adjust` 完成（需要 store，
            // 而 store 在本结构体构造之后才可用）。headless 无 store 时保持空 = 出厂顺序。
            quick_adjust: std::sync::RwLock::new(std::collections::HashMap::new()),
            chaizi_assets: Mutex::new(ChaiziAssets {
                db: chaizi_db,
                font: None, // 字体在 new() 经 sync_chaizi_assets 下发（headless 无 UI 不发）
                ..Default::default()
            }),
            // 空初值 + new() 里的 sync_comment_dicts 首次加载：与拆字字体同一套「声明式变更
            // 检测」，构造期不做 IO，加载与热重载走同一条路径（不会出现只在启动生效的分叉）。
            comment_dict_paths: Mutex::new(Vec::new()),
            pair_tracker: Mutex::new(wind_transform::pair_tracker::PairTracker::new()),
            last_valid_caret: Mutex::new((0, 0, 0)),
            caret_independent: std::sync::atomic::AtomicBool::new(false),
            config_overlay: std::sync::OnceLock::new(),
            learning_suspended: std::sync::atomic::AtomicBool::new(false),
            host_caret_trusted: std::sync::atomic::AtomicBool::new(false),
            host_no_inline_preedit: std::sync::atomic::AtomicBool::new(false),
            quiet_reload_toast: std::sync::atomic::AtomicBool::new(false),
            trigger_suppressed: std::sync::atomic::AtomicU32::new(0),
            eager_prewarm: std::sync::atomic::AtomicBool::new(true),
            readiness_state: std::sync::atomic::AtomicU8::new(0),
            pending_first_show: Mutex::new(false),
            pending_first_show_token: Mutex::new(0),
            assoc_hide_token: Mutex::new(0),
            assoc_placeholder_orphaned: std::sync::atomic::AtomicBool::new(false),
            candidate_shown: Mutex::new(false),
            show_authorized: std::sync::atomic::AtomicBool::new(false),
            // 定位调试浮窗：仅 Dev 菜单可开，默认关。
            caret_overlay_enabled: std::sync::atomic::AtomicBool::new(false),
            caret_overlay_frame: Mutex::new(None),
            candidate_flipped: std::sync::atomic::AtomicBool::new(false),
            hover_index: std::sync::atomic::AtomicI32::new(-1),
            composition_start: Mutex::new((0, 0, false)),
            locked_rect_anchor: Mutex::new((0, 0, false)),
            last_pre_reflow_probe: Mutex::new((0, 0, false)),
            pre_reflow_comp_start: Mutex::new((0, 0, false)),
            last_reported_comp_start: Mutex::new((0, 0, false)),
            tsf_frame_seen_in_composition: std::sync::atomic::AtomicBool::new(false),
            last_authoritative_caret: Mutex::new((0, 0, false)),
            last_key_at: Mutex::new(None),
            last_key_interval_ms: Mutex::new(None),
            first_show_was_provisional: std::sync::atomic::AtomicBool::new(false),
            first_show_after_commit: std::sync::atomic::AtomicBool::new(false),
            caret_cache_verified: std::sync::atomic::AtomicBool::new(false),
            caret_cache_is_idle_report: std::sync::atomic::AtomicBool::new(false),
            awaiting_first_authority_after_focus: std::sync::atomic::AtomicBool::new(false),
            shown_anchor: Mutex::new((0, 0, false)),
            caret_baseline: Mutex::new((0, 0, false)),
            last_sane_caret_height: std::sync::atomic::AtomicI32::new(FALLBACK_LINE_HEIGHT),
            first_show_extended: std::sync::atomic::AtomicBool::new(false),
            pending_focus_tip: std::sync::atomic::AtomicBool::new(false),
            pending_focus_tip_gen: std::sync::atomic::AtomicU64::new(0),
            last_focus_tip_token: Mutex::new(0),
            app_compat: Mutex::new(app_compat),
            compat_dirs: (
                data_dir.map(|d| d.to_path_buf()),
                user_dir.as_ref().map(|d| d.to_path_buf()),
            ),
            active_compat: Mutex::new(ActiveCompat::default()),
            pid_names: Mutex::new(HashMap::new()),
            mode_scope: Mutex::new(ModeScope::default()),
            focus_windows: Mutex::new(Default::default()),
            mode_states: Mutex::new(HashMap::new()),
            runtime_last: Mutex::new((init_chinese, init_full, init_punct)),
            last_caps_inject: Mutex::new(None),
            front_ctx: Mutex::new((String::new(), String::new(), String::new())),
            theme_layers,
            theme_name: Mutex::new(initial_theme),
            last_status_text: Mutex::new(String::new()),
            last_toolbar_push: Mutex::new(None),
            schema_toggle_origin: Mutex::new(None),
            schema_switch_arrival: Mutex::new(None),
            theme_style: Mutex::new(theme_style_init),
            theme_index_labels: Mutex::new(Vec::new()),
            cmdbar_services: std::sync::OnceLock::new(),
            host_services: std::sync::OnceLock::new(),
            self_weak: std::sync::OnceLock::new(),
            recent_commits: Mutex::new(std::collections::VecDeque::new()),
            next_word_prev: Mutex::new(None),
            preceding_text: Mutex::new(String::new()),
            last_commit_len: std::sync::atomic::AtomicUsize::new(1),
            auto_space_prefixed: std::sync::atomic::AtomicBool::new(false),
            preedit_display: Mutex::new(preedit_display_init),
            eff_candidate_cache: Mutex::new(None),
            hide_candidate_window: Mutex::new(hide_candidate_window_init),
            candidate_orientation: Mutex::new(candidate_orientation_init),
            candidate_layout_sent: Mutex::new(candidate_orientation_init),
            candidate_font_sent: Mutex::new(String::new()),
            stat_collector,
            stat_recorded: std::sync::atomic::AtomicBool::new(false),
            commit_code_pending: std::sync::atomic::AtomicBool::new(false),
            commit_code_forced: std::sync::atomic::AtomicBool::new(false),
            debug_stat_events: Mutex::new(None),
            fullscreen_cached: std::sync::atomic::AtomicBool::new(false),
            fullscreen_probing: std::sync::atomic::AtomicBool::new(false),
            #[cfg(windows)]
            fullscreen_watch_started: std::sync::atomic::AtomicBool::new(false),
            fullscreen_exclusive_cached: std::sync::atomic::AtomicBool::new(false),
            uielement_host_pids: Mutex::new(std::collections::HashSet::new()),
            uielement_reader_pids: Mutex::new(std::collections::HashSet::new()),
            focus_pid: std::sync::atomic::AtomicU32::new(0),
            #[cfg(windows)]
            host_render: std::sync::OnceLock::new(),
            last_input_diag: Mutex::new(Default::default()),
            input_block_gate: Mutex::new(InputBlockGate::default()),
            last_langbar_tooltip: Mutex::new(String::new()),
            tooltip_page: Mutex::new(Vec::new()),
            tooltip_menu_target: Mutex::new(None),
            #[cfg(ext_presenter)]
            candidate_menu_open: std::sync::atomic::AtomicBool::new(false),
            last_window_diag: Mutex::new(Default::default()),
            password_suppress: std::sync::atomic::AtomicBool::new(false),
            hook: Arc::new(wind_hook::tap::Tap::new()),
            hook_session: std::sync::Mutex::new(Default::default()),
            extensions_config_hook: std::sync::OnceLock::new(),
            password_suppress_enabled: std::sync::atomic::AtomicBool::new(
                password_suppress_enabled_init,
            ),
            input_diag_hud_visible: std::sync::atomic::AtomicBool::new(false),
            input_diag_sections: Mutex::new(Default::default()),
            input_diag_frozen: std::sync::atomic::AtomicBool::new(false),
            input_diag_topmost: std::sync::atomic::AtomicBool::new(true),
            app_schema: app_schema::AppSchemaState::new(global_schema_init, app_schemas_init),
        });
        // CapsLock 钩子的动作消费线程。钩子回调只做非阻塞投递（它超时会被系统静默移除且
        // 无从察觉），真正的动作在这里执行，可安全加锁。未装钩子时它一直阻塞在 channel 上。
        // 起在这里而非 `new`：只有此处能同时拿到 `Arc<Self>` 与 receiver。
        {
            let c = Arc::clone(&coordinator);
            std::thread::Builder::new()
                .name("capslock-action".into())
                .spawn(move || {
                    for ev in capslock_press_rx {
                        match ev {
                            wind_keys::capslock_hook::HookEvent::Caps(ev) => {
                                c.handle_capslock_hook_event(ev)
                            }
                            wind_keys::capslock_hook::HookEvent::Alt(ev) => {
                                c.handle_comment_alt_hook(ev)
                            }
                        }
                    }
                    debug!("CapsLock 钩子事件通道已关闭");
                })
                .ok();
        }
        // 常用字表的用户覆盖（右键「设为生僻字/常用字」）：真相在 store，构造体内只装得下
        // 出厂基表，这里补灌运行时镜像。
        //
        // ⚠️ 落在 `build` 而非 `new`（对比 `reload_quick_adjust`）：`new_headless_with_store`
        // 直接走 build、不经过 new，放在 new 里会让 headless 测试恒看不到已存在的覆盖——
        // 那是一条真实存在但测不到的路径。
        coordinator.reload_common_chars();
        // 命令栏：装配 Services（ime/config/dict 后端）+ 自身 Weak 引用。
        coordinator.init_cmdbar();
        // 外部编辑会话超过租约由 store 自动结束：变了的项照 `dict.reload` 重建内存镜像。只持 Weak。
        if let Some(store) = coordinator.store.as_ref() {
            let weak = Arc::downgrade(&coordinator);
            store.set_edit_expired_hook(Arc::new(move |changed: &[String]| {
                if let Some(c) = weak.upgrade() {
                    c.apply_user_text_changes(changed);
                }
            }));
        }
        // 按词查编码用户层后台建好 → 重刷当前这屏候选，把用户码补上（否则要等下一次按键）。
        // 与 `spawn_index_warm` 建成反查索引后的重渲染同一个做法；只持 Weak。
        {
            let weak = Arc::downgrade(&coordinator);
            coordinator
                .engine_mgr
                .set_user_text_built_hook(Arc::new(move || {
                    let Some(c) = weak.upgrade() else {
                        return;
                    };
                    let s = c.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !s.input_buffer.is_empty() {
                        c.notify_ui_update(&s);
                    }
                }));
        }
        // 启动即显示常驻工具栏（反映初始 中英/方案/标点/全半角）
        coordinator.notify_toolbar();
        // 码元集与按键功能的冲突体检（只告警）。默认字符集下直接返回，无开销。
        coordinator.warn_code_char_conflicts();
        // 通配键与既有按键功能的冲突体检（只告警）：先查反查模式内的通配键（不看通配主开关，
        // 反查模式不可用时跳过），再查组码中的通配键（通配关闭时跳过）。
        coordinator.warn_wildcard_conflicts();
        // 档位循环触发键的撞车体检（只告警）。出厂不配 ⇒ 默认直接返回。
        coordinator.warn_english_case_cycle_conflict();
        coordinator.warn_sentence_cycle_conflict();
        coordinator.warn_schema_sentence_cycle_conflict(&coordinator.engine_mgr.active_schema_id());
        coordinator.warn_comment_switch_conflict();
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
    ///
    /// 会话态动词 `commit_code` 借回车的臂上屏编码，那一下恒为假（见
    /// [`Self::redispatch_commit_code`]）——这里是各臂唯一的读点，故只在这里拦。
    pub(crate) fn enter_clears_composition(&self) -> bool {
        if self
            .commit_code_forced
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }
        self.rt().config.input.enter_behavior == "clear"
    }

    /// 空码时按标点/符号键怎么处置这串废码（`input.punct_on_empty_behavior`）。
    ///
    /// 「空码」= 缓冲非空但一个候选都没有（多为码表打错字根）。既有行为是把废码连同标点
    /// 一起送上屏，用户要的往往是「这串码作废、句号照打」，也有人要「连句号一起当没按过」。
    ///
    /// **非空码时恒返回 [`PunctEmptyCodePolicy::Commit`]**——空码判定折在函数内部，调用方
    /// 不必也不该自己再拼一遍 `candidates.is_empty() && !input_buffer.is_empty()`：那个条件
    /// 曾在两个出口各写一份，两份哪天走岔了没有任何东西拦得住。
    ///
    /// ⚠️ 只管**无候选**那一支。有候选时按标点仍顶屏首选——那是「就选高亮那条吧」的既有
    /// 语义，与本开关无关。
    ///
    /// ⚠️ 与 `schema.codetable.punct_commit` 正交：那一项关掉是「吞键、**保留**编码」，编码
    /// 留在组合区继续输入；本开关的 `clear_no_input` 是「吞键、**丢弃**编码」。
    ///
    /// ★ 返回枚举而非 bool 是刻意的：标点有**两个彼此独立的上屏出口**（普通标点、智能符号
    /// `CommitAndHoldComposition`），第三态加进来时，bool 判据的漏接会静默落进 else 分支，
    /// 而 `match` 的漏接是编译错误。参见 [`Self::enter_clears_composition`] 的同款教训。
    pub(crate) fn punct_empty_code_policy(&self, state: &State) -> PunctEmptyCodePolicy {
        // 缓冲里有入缓冲符号（`gong-`）时候选恒空，但那是用户打的字面文本，不是废码。
        if self.buffer_has_literal_symbol(state) {
            return PunctEmptyCodePolicy::Commit;
        }
        self.punct_empty_code_policy_for(state, &state.input_buffer)
    }

    /// 同 [`Self::punct_empty_code_policy`]，但「编码」取调用方给的缓冲——独占模式
    /// （临拼 / 快捷输入 / 临英）的码不在 `input_buffer` 里。判据与值域只此一份。
    pub(crate) fn punct_empty_code_policy_for(
        &self,
        state: &State,
        buffer: &str,
    ) -> PunctEmptyCodePolicy {
        // 有候选 / 没编码 ⇒ 不是空码，本开关不管。
        if !state.candidates.is_empty() || buffer.is_empty() {
            return PunctEmptyCodePolicy::Commit;
        }
        match self.rt().config.input.punct_on_empty_behavior.as_str() {
            "clear" => PunctEmptyCodePolicy::Clear,
            "clear_no_input" => PunctEmptyCodePolicy::ClearNoInput,
            // 未知值落回 commit（＝历史行为）。存量非法值本已由
            // `Config::migrate_empty_code_behavior_value` 归一，此处只是防御。
            _ => PunctEmptyCodePolicy::Commit,
        }
    }

    /// 焦点/IME 激活时按 client_token 高 32 位的 PID 解析焦点进程名，缓存其 caret 兼容态
    /// （对齐 Go `HandleFocusGained` 设置 activeCompatRule）。仅在重型/异步段调用，不在 DLL
    /// 同步阻塞路径上。
    ///
    /// 规则按**窗口上下文**解析：窗口取该 token 最近一次 focus_gained 记下的那个
    /// （[`Self::note_focus_window`]；ime_activated 不带窗口信息，即沿用它）。缓存键是
    /// (pid, 类名小写 + 标题的哈希)：同进程同窗口直接返回，避免每次焦点事件重复 OpenProcess；
    /// 同进程换了窗口则重新解析——持续类字段（定位、首显、配对…）随之即时刷新。
    fn update_active_compat(&self, client_token: u64) {
        let pid = (client_token >> 32) as u32;
        if pid == 0 {
            return;
        }
        self.focus_pid
            .store(pid, std::sync::atomic::Ordering::Relaxed);
        // 缓存优先于反查：macOS 的 `.app` 随焦点事件把宿主 bundle id 送进 `pid_names`
        // （服务进程那边 `process_name` 恒返回空串），此处必须先读缓存才能拿到宿主名。
        // Windows 上首次见到该 pid 时缓存为空 → 照常 OpenProcess 反查，行为不变。
        //
        // ⚠ 在取 `active_compat` 锁**之前**读缓存：本函数末尾是「先 drop(ac) 再锁
        // pid_names」，两把锁在此嵌套会引入一个方向相反的持有序。
        let cached_name = self.cached_proc_name(client_token);
        // 同理先于 `active_compat` 锁取窗口（`focus_windows` 锁内不取任何别的锁）。
        // 该 token 从没 focus_gained 过（新实例只发了 ime_activated）⇒ 退回该 pid 最近获焦的窗口，
        // 而不是按空窗口重解析——那会把 Chromium 的占位、窗口规则的初始模式一起丢掉。
        let win = self.focus_window_of_token_or_pid(client_token);
        let window_hash = win.cache_hash();
        let mut ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
        if ac.pid == pid && ac.window_hash == window_hash {
            return; // 同进程同窗口，规则已缓存
        }
        let name = if cached_name.is_empty() {
            process_name(pid)
        } else {
            cached_name
        };
        // 名字传小写：与重载 / 改名时按 `pid_names`（小写）重算的口径一致。
        let lookups = self.compute_active_lookups(&name.to_lowercase(), &win);
        let (next, rule_matched, rule_initial_mode, rule_initial_punct) =
            self.with_compat_rule(&name, &win, |rule| {
                let initial_mode = rule.and_then(|r| r.initial_mode);
                let initial_punct = rule.and_then(|r| r.initial_punct);
                (
                    ActiveCompat {
                        pid,
                        caret_use_top: rule.map(|r| r.caret_use_top).unwrap_or(false),
                        stale_probe_guard: rule.map(|r| r.stale_probe_guard).unwrap_or(false),
                        composition_start_pair_guard: rule
                            .and_then(|r| r.composition_start_pair_guard)
                            .unwrap_or(false),
                        pin_anchor_when_start_drifts: rule
                            .and_then(|r| r.pin_anchor_when_start_drifts)
                            .unwrap_or(false),
                        first_show_mode: rule.and_then(|r| r.first_show_mode),
                        has_initial_rule: initial_mode.is_some() || initial_punct.is_some(),
                        auto_pair: rule.and_then(|r| r.auto_pair),
                        smart_method: rule.and_then(|r| r.smart_method),
                        auto_spacing: rule.and_then(|r| r.auto_spacing),
                        caret_offset_x: rule.map(|r| r.caret_offset_x).unwrap_or(0),
                        caret_offset_y: rule.map(|r| r.caret_offset_y).unwrap_or(0),
                        window_hash,
                        lookups,
                    },
                    rule.is_some(),
                    initial_mode,
                    initial_punct,
                )
            });
        // 无条件记录（对齐 Go handle_lifecycle.go:698）。原实现仅在 caret_use_top=true 时打，
        // 规则未命中与「命中但全 false」在日志里无从区分，查「某应用兼容项没生效」时看不到
        // 究竟是没匹配上进程名还是字段没读到。
        debug!(
            "Compat rule for process={name} class={:?} title_len={}: matched={} caret_use_top={} stale_probe_guard={} composition_start_pair_guard={} pin_anchor_when_start_drifts={} first_show_mode={} initial_mode={} initial_punct={} auto_pair={} smart_method={} caret_offset=({},{})",
            win.class,
            win.title.chars().count(),
            rule_matched,
            next.caret_use_top,
            next.stale_probe_guard,
            next.composition_start_pair_guard,
            next.pin_anchor_when_start_drifts,
            next.first_show_mode
                .map(|m| m.as_config())
                .unwrap_or("(follow-global)"),
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
        // 持 `active_compat` 锁写：两者是同一份缓存的两半（锁序 active_compat → focus_windows）。
        self.set_active_focus_window(win);
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
    ///
    /// ★ 真的改写了缓存时，给该进程的推送客户端**补推**按 pid 现算的 DLL 配置（密码框吃键
    /// 门控、英文自动配对）：推送通道的握手可能早于主管道的这次校正，那时推过去的值是按
    /// **旧名**算的——密码框门控错了就打破 core.suppress ⊆ C++.suppress（密码框丢键）。
    /// 首次落缓存（`None` 分支）不用补推：缓存为空时握手现查的就是真名。
    #[cfg(any(windows, test))]
    pub(crate) fn revalidate_pid_name(&self, pid: u32, live_name: &str) {
        if pid == 0 || live_name.is_empty() {
            return;
        }
        let live = live_name.to_lowercase();
        let rewritten = {
            let mut names = self.pid_names.lock().unwrap_or_else(|e| e.into_inner());
            match names.get(&pid) {
                Some(cached) if *cached == live => false,
                Some(cached) => {
                    // 这条 WARN 就是 PID 复用的现场证据。缓存过一个名字、现查却是另一个，
                    // 只可能是那个 pid 换了进程——在此之前它一直是静默错配。
                    tracing::warn!(
                        "pid_names 校正：pid={pid} 缓存={cached} 实际={live}（PID 已被复用，此前按缓存匹配的 per-app 规则是错的）"
                    );
                    names.insert(pid, live);
                    true
                }
                None => {
                    names.insert(pid, live);
                    false
                }
            }
        };
        // 焦点 pid 的名字刚落缓存或被改写：按键路径那三项按新名字重算（它们只读缓存名）。
        // 改写 = PID 复用：active 槽记的窗口也属于上一任进程，一并清掉（与下面
        // `forget_focus_windows_of_pid` 清 token 表同理），window_hash 同步改成空窗口的，
        // 新进程下一次获焦不会被「同 pid 同窗口」早退拦住。
        let is_active = {
            let mut ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
            let is_active = ac.pid == pid;
            if is_active && rewritten {
                let empty = FocusWindow::default();
                ac.window_hash = empty.cache_hash();
                // 锁序 active_compat → focus_windows，与 `update_active_compat` 相同。
                self.set_active_focus_window(empty);
            }
            is_active
        };
        if is_active {
            self.refresh_active_compat_lookups();
        }
        // 放掉 `pid_names` 锁之后再推：推送内容现算时要再取它。
        if rewritten {
            // 换了进程：上一任记下的窗口不属于它（不清的话新进程会按旧窗口匹配窗口规则）。
            self.forget_focus_windows_of_pid(pid);
            for token in self.push_server.tokens_of_pid(pid) {
                self.push_password_suppress_config(token);
                self.push_english_pair_config(token);
                self.push_pair_via_comp_config(token);
                self.push_composition_placeholder_config(token);
                self.push_space_before_digit_config(token);
                self.push_multiline_commit_config(token);
                // 标题采集开关按进程名算（本进程可能命中标题规则），名字纠正后同样要重推。
                self.push_compat_title_match_config(token);
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
    /// `stale_probe_guard` / `composition_start_pair_guard` / `first_show_mode` / `auto_pair` /
    /// `smart_method` / `caret_offset_*`），**刻意不碰
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
    pub(crate) fn refresh_active_compat_rule_fields(&self, pid: u32) {
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
        // 窗口：本就是焦点进程时用 active 槽那个（与 `update_active_compat` 解析时同一个窗口），
        // 否则取该 pid 最近一次获焦的窗口（连接校正时前台进程可能还没 focus_gained 过 ⇒ 空窗口）。
        let win = {
            let ac_pid = self
                .active_compat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pid;
            if ac_pid == pid {
                self.active_focus_window()
            } else {
                self.focus_window_of_pid(pid)
            }
        };
        let (
            caret_use_top,
            stale_probe_guard,
            composition_start_pair_guard,
            pin_anchor_when_start_drifts,
            first_show_mode,
            auto_pair,
            smart_method,
            caret_offset_x,
            caret_offset_y,
        ) = self.with_compat_rule(&name, &win, |rule| {
            (
                rule.map(|r| r.caret_use_top).unwrap_or(false),
                rule.map(|r| r.stale_probe_guard).unwrap_or(false),
                rule.and_then(|r| r.composition_start_pair_guard)
                    .unwrap_or(false),
                rule.and_then(|r| r.pin_anchor_when_start_drifts)
                    .unwrap_or(false),
                rule.and_then(|r| r.first_show_mode),
                rule.and_then(|r| r.auto_pair),
                rule.and_then(|r| r.smart_method),
                rule.map(|r| r.caret_offset_x).unwrap_or(0),
                rule.map(|r| r.caret_offset_y).unwrap_or(0),
            )
        });
        debug!(
            "Connected-pid compat refresh for process={name} (pid={pid}): caret_use_top={} stale_probe_guard={stale_probe_guard} composition_start_pair_guard={composition_start_pair_guard} first_show_mode={} auto_pair={} smart_method={} caret_offset=({},{})",
            caret_use_top,
            first_show_mode
                .map(|m| m.as_config())
                .unwrap_or("(follow-global)"),
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
            ac.stale_probe_guard = stale_probe_guard;
            ac.composition_start_pair_guard = composition_start_pair_guard;
            ac.pin_anchor_when_start_drifts = pin_anchor_when_start_drifts;
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
        // 按键路径那三项跟的是 active 槽（`.pid` 的名字 + 窗口），不是本函数的 `pid`：
        // 上面刚可能填了它的缓存名，按 active 槽重算一次。
        self.refresh_active_compat_lookups();
    }

    /// 按 (进程名, 窗口) 现算 [`ActiveLookups`]。
    pub(crate) fn compute_active_lookups(&self, name: &str, win: &FocusWindow) -> ActiveLookups {
        let (
            composition_placeholder,
            assoc_placeholder,
            candidate_pos,
            host_drawn_candidates,
            syllable_separator,
        ) = self.with_compat_rule(name, win, |rule| {
            let comp = rule
                .and_then(|r| r.composition_placeholder)
                .unwrap_or_default();
            (
                comp,
                rule.and_then(|r| r.assoc_placeholder)
                    .unwrap_or_else(|| comp.into()),
                rule.and_then(|r| {
                    let mode = r.candidate_position_mode?;
                    Some((mode.is_fixed(), r.candidate_x, r.candidate_y))
                }),
                rule.and_then(|r| r.host_drawn_candidates),
                rule.and_then(|r| r.syllable_separator),
            )
        });
        // 收窗推断对无名进程也认 `"*"`（`get_rule("")` 不吃通配），口径见
        // `uielement_host_draws_by_inference`。
        let host_drawn_candidates = if name.is_empty() {
            self.with_compat_rule(wind_config::app_compat::ANY_PROCESS, win, |r| {
                r.and_then(|r| r.host_drawn_candidates)
            })
        } else {
            host_drawn_candidates
        };
        ActiveLookups {
            composition_placeholder,
            assoc_placeholder,
            syllable_separator,
            candidate_pos,
            host_drawn_candidates,
        }
    }

    /// 按 active 槽（`active_compat.pid` 的缓存名 + active 窗口）重算 [`ActiveCompat::lookups`]。
    /// 不动其余字段（`.pid` / `.has_initial_rule` 的另一重身份见 `refresh_active_compat_rule_fields`）。
    ///
    /// 名字只读 `pid_names` 缓存、不反查进程：与这几项原先在按键路径上现查的口径
    /// （`active_process_name`）相同。
    ///
    /// ⚠ 与 `update_active_compat` 并发时不能「先算后写」：算的期间焦点切到了别的应用，旧结果
    /// 就会盖掉新应用的值，而之后同一窗口的焦点又被 `window_hash` 早退拦住，永不自愈。故分两步：
    /// 先在锁外取名字（`pid_names` 不与 `active_compat` 嵌套），再持 `active_compat` 锁核对
    /// (pid, window_hash) 没变、并在锁内现算现写；变了就放弃（换焦点的那一方已按新表算好）。
    /// 在锁内算也顺带保证：两次重载并发时，最后写入的那份读的是最后换上的表。
    pub(crate) fn refresh_active_compat_lookups(&self) {
        let snap = self.active_lookups_snapshot();
        self.store_active_lookups_if_current(&snap);
    }

    /// [`Self::refresh_active_compat_lookups`] 第一步：active 槽的 (pid, window_hash, 缓存名)。
    pub(crate) fn active_lookups_snapshot(&self) -> (u32, u64, String) {
        let (pid, hash) = {
            let ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
            (ac.pid, ac.window_hash)
        };
        (pid, hash, self.cached_proc_name((pid as u64) << 32))
    }

    /// 第二步：active 槽仍是快照那个 (pid, window_hash) 才现算写入，返回是否写了。
    /// 锁序 active_compat → focus_windows → app_compat，与 `update_active_compat` 相同。
    pub(crate) fn store_active_lookups_if_current(&self, snap: &(u32, u64, String)) -> bool {
        let (pid, hash, name) = snap;
        let mut ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
        if ac.pid != *pid || ac.window_hash != *hash {
            debug!(
                "refresh_active_compat_lookups: 焦点已变（pid {pid} → {}），放弃写入",
                ac.pid
            );
            return false;
        }
        let win = self.active_focus_window();
        ac.lookups = self.compute_active_lookups(name, &win);
        true
    }

    /// 按 client_token 高 32 位的 PID 查已缓存的进程名（小写）。未缓存返回空串。
    /// 仅 HashMap 查询，可用于 DLL 同步阻塞路径。
    pub(crate) fn cached_proc_name(&self, client_token: u64) -> String {
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
    /// 快照，并按 `password_force_english_for_pid`（per-app 规则优先，否则全局开关）决定是否
    /// 强制英文抑制（密码框场景）。
    pub(crate) fn apply_input_diag(&self, pid: u32, disabled: bool, reason_byte: u8, mask: u64) {
        self.apply_input_diag_capped(pid, disabled, reason_byte, mask, true);
    }

    /// 同 [`Self::apply_input_diag`]，`allow_raise = false` 时本次**只降不升**：算出要抑制也先不置位。
    ///
    /// 给「同一 token 刚换了窗口」用：DLL 那份门控按旧窗口算、新值的重推还在路上，此刻服务端
    /// 按新窗口当场置位就是 core 抑制而 DLL 照吃——违反 core.suppress ⊆ C++.suppress
    /// （同 [`Self::relax_password_suppress_for_focus`] 的口径）。置位留给 DLL 下一次
    /// input_state_report（那时它手里必然已有新值）。
    pub(crate) fn apply_input_diag_capped(
        &self,
        pid: u32,
        disabled: bool,
        reason_byte: u8,
        mask: u64,
        allow_raise: bool,
    ) {
        use std::sync::atomic::Ordering::Relaxed;
        // 展示原因 = mask/disabled 推导，再叠加 DLL 上报的 reason_byte 里「context 级禁用」这一档：
        // DLL 把 context 级 KEYBOARD_DISABLED 折进了 mask 的 IS_PASSWORD 位，折位前的区分只有
        // 它知道（见 `reason_from_report`）。reason 只管展示与提示语，**不参与**下面的抑制决策
        // ——抑制仍只看 mask，两种来源行为一致，避免「双重来源」歧义。
        let reason = crate::input_diag::reason_from_report(disabled, reason_byte, mask);
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
        //
        // 开关取值按 pid 走 `password_force_english_for_pid`（per-app 规则优先，否则全局）——
        // 与推给该 DLL 的值**同一个函数**，这是上面那条不变量在按应用覆盖下成立的前提。
        let suppress = allow_raise
            && crate::input_diag::is_password_scope(mask)
            && self.password_force_english_for_pid(pid);
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
            // 展示宿主**原始**报的 scope：context 级禁用那档要摘掉 DLL 折进去的 IS_PASSWORD 位，
            // 否则 HUD 上会出现宿主根本没报过的 0x80000001（t197 楼主被它误导成「scope 变密码」）。
            mask: crate::input_diag::display_mask(reason, mask),
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

    /// 查 `compat.toml` 中该进程（该窗口）的候选窗首显规则；`None` = 未配置（跟随全局）。
    pub(crate) fn rule_first_show_mode(
        &self,
        proc_name: &str,
        win: &FocusWindow,
    ) -> Option<wind_config::app_compat::FirstShowMode> {
        self.with_compat_rule(proc_name, win, |r| r.and_then(|r| r.first_show_mode))
    }

    /// 查 `compat.toml` 中该进程（该窗口）的初始中英规则；`None` = 未配置（不干预）。
    ///
    /// 仅锁 + 表查询（含 `resolve` 的纯内存合成），无 OpenProcess，故可用于 DLL 同步阻塞路径
    /// （`get_current_mode`）。
    pub(crate) fn rule_initial_mode(
        &self,
        proc_name: &str,
        win: &FocusWindow,
    ) -> Option<wind_config::app_compat::InitialMode> {
        self.with_compat_rule(proc_name, win, |r| r.and_then(|r| r.initial_mode))
    }

    /// 这次系统模式切换是不是「宿主自作主张切中英」，且当前应用（或全局）配了忽略。
    ///
    /// 不看方向：开和关一样拦（宿主关 IME 让用户点一下按钮就变英文；SSMS 内部焦点切换把
    /// OPENCLOSE 写成开，英文态被切成中文、第一键被吃，GH#312）。三个条件缺一不可
    /// （顺序即成本，先便宜的）：
    /// 1. **Ctrl 未按住**——按住＝用户在按 Ctrl+Space，系统热键必须放行，
    ///    否则该应用里中英切换彻底失灵（判据由 DLL 现场采样，见 `MODE_SWITCH_CTRL_HELD`）；
    /// 2. **来源是 compartment**——按键兜底与功能菜单是我们自己发起的，永远照办；
    /// 3. 生效值为真：该应用规则写了 `ignore_host_ime_switch` 就以它为准，没写取全局
    ///    `input.ignore_host_ime_switch`（出厂关）。
    pub(crate) fn host_ime_switch_ignored(
        &self,
        source: wind_ipc::protocol::ModeSwitchSource,
        ctrl_held: bool,
    ) -> bool {
        use wind_ipc::protocol::ModeSwitchSource as Src;
        if ctrl_held {
            return false;
        }
        if !matches!(
            source,
            Src::CompartmentOpenClose | Src::CompartmentConversion
        ) {
            return false;
        }
        self.with_active_compat_rule(|r| r.and_then(|r| r.ignore_host_ime_switch))
            .unwrap_or_else(|| self.rt().config.input.ignore_host_ime_switch)
    }

    /// 查 `compat.toml` 中该进程（该窗口）的初始中英标点规则；`None` = 未配置（不干预）。
    pub(crate) fn rule_initial_punct(
        &self,
        proc_name: &str,
        win: &FocusWindow,
    ) -> Option<wind_config::app_compat::InitialMode> {
        self.with_compat_rule(proc_name, win, |r| r.and_then(|r| r.initial_punct))
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
    ///
    /// 规则按 (进程, 窗口) 解析；per-app 记忆表仍按进程名（记忆是「这个应用」的，不分窗口）。
    fn initial_chinese_mode_for(&self, proc_name: &str, win: &FocusWindow) -> bool {
        let bundle = self.rt();
        let d = &bundle.config.input.default;
        if let Some(m) = self.rule_initial_mode(proc_name, win) {
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
    /// remember_last_state=true 时经 `state_writer` 落盘 state.toml。
    /// 必须在释放 state 锁后调用。
    ///
    /// 这里是 state.toml 写得**最频繁**的一处：每次切中英/全半角/标点都来一趟，
    /// 而那正是用户连打时最爱按的几个键。走延迟写入器后，一串连续切换只落一次盘。
    pub(crate) fn record_last_state(&self) {
        let (c, f, p) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.chinese_mode, s.full_width, s.chinese_punct)
        };
        *self.runtime_last.lock().unwrap_or_else(|e| e.into_inner()) = (c, f, p);
        if self.rt().config.input.default.remember_last_state {
            self.state_writer.schedule("last_state", move |rs| {
                rs.last_chinese_mode = c;
                rs.last_full_width = f;
                rs.last_chinese_punct = p;
            });
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

    /// 切中英时**已经取消了大写锁定**，此刻该落到哪个模式（`input.capslock.mode_after_cancel`，
    /// 论坛 t283）。只在 [`Self::cancel_caps_on_switch`] 返回真时调用；没取消时两条路径照旧
    /// （Shift 翻转、Ctrl+空格 按请求），不经这里。
    ///
    /// `chinese`（出厂）对两条路径给的是**不同的**结果，这是有意的：它逐字保留本配置出现前
    /// 的行为——Shift 没有方向，旧逻辑归位中文（翻转的话，被大写压着的 `chinese_mode=true`
    /// 会翻成英文，用户看来「切了没反应」）；Ctrl+空格 带着系统算好的目标，旧逻辑只取消大写、
    /// 不改目标。
    pub(crate) fn mode_after_caps_cancel(
        setting: &str,
        switch: CapsCancelSwitch,
        current: bool,
    ) -> bool {
        match (setting, switch) {
            (_, CapsCancelSwitch::Host(requested)) => requested,
            (_, CapsCancelSwitch::Directed(requested)) => requested,
            ("english", _) => false,
            ("toggle", CapsCancelSwitch::Toggle) => !current,
            (_, CapsCancelSwitch::System(requested)) => requested,
            ("chinese", CapsCancelSwitch::Toggle) => true,
            (other, CapsCancelSwitch::Toggle) => {
                warn!("input.capslock.mode_after_cancel 取值 {other:?} 不认识，按 chinese 处理");
                true
            }
        }
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
    ///
    /// 窗口取该 token 最近一次 focus_gained 记下的那个（ime_activated 即沿用它）。
    fn apply_initial_mode(&self, client_token: u64, reset_aux: bool) {
        let win = self.focus_window_of_token_or_pid(client_token);
        self.apply_initial_mode_in(client_token, &win, reset_aux);
    }

    /// 同 [`Self::apply_initial_mode`]，窗口由调用方给：DLL 同步路径（`get_current_mode`）跑在
    /// 重型段记账之前，token 表里还是上一个窗口，只能用它手里那份类名。
    fn apply_initial_mode_in(&self, client_token: u64, win: &FocusWindow, reset_aux: bool) {
        let bundle = self.rt();
        let d = &bundle.config.input.default;
        let proc = self.cached_proc_name(client_token);
        let chinese = self.initial_chinese_mode_for(&proc, win);
        // per-app：首次进入就把这个进程的中英态记进表。只在手动切换时记的话，没切过的进程
        // 表里没有它，下次回来会退回全局「前次状态」——在别的应用里切的那一下就把它带走了。
        // 有应用规则的进程不记：规则每次都压过记忆表。
        if d.per_app_scope() && !proc.is_empty() && self.rule_initial_mode(&proc, win).is_none() {
            self.mode_states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(proc.clone())
                .or_insert(chinese);
        }
        let rule_punct = self.rule_initial_punct(&proc, win);
        let follow = bundle.config.input.punct.follow_mode;
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if reset_aux && !d.remember_last_state {
            s.full_width = d.full_width;
            self.set_punct_below_schema_intent(&mut s, d.chinese_punct);
        }
        if s.chinese_mode != chinese {
            // 模式变更的四个入口都打一条同形日志（见 `handle_system_mode_switch`）。
            // 只在真改变时打：本函数每次焦点切换都会走到，无条件打会淹掉日志。
            // 注意此处已持 state 锁，取旧值只能读 `s`，不可调 `is_chinese_mode()`（会死锁）。
            tracing::debug!(
                "initial_mode: proc={} {} -> {}",
                proc,
                if s.chinese_mode { "中" } else { "英" },
                if chinese { "中" } else { "英" }
            );
            s.chinese_mode = chinese;
            // 标点随中英文切换（对齐 handle_toggle_mode/handle_system_mode_switch）。
            if follow {
                self.set_punct_below_schema_intent(&mut s, chinese);
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
            // 同一个抑制态，成因分两档展示（见 `InputBlock::ContextDisabled`）。
            let ctx_only = self
                .last_input_diag
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reason
                == crate::input_diag::InputDiagReason::ContextDisabled;
            return if ctx_only {
                InputBlock::ContextDisabled
            } else {
                InputBlock::Password
            };
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

    /// 构造时传入的数据根（生产即 `Config::data_dir()`；headless / 移动端是宿主给的那个）。
    /// 运行期按需加载的数据文件经它解析，与构造期加载同源。
    pub(crate) fn data_dir(&self) -> Option<std::path::PathBuf> {
        self.compat_dirs.0.clone().or_else(Config::data_dir)
    }

    /// 拆字库路径（`[engine.chaizi].db_path`，用户方案目录优先）。配了却找不到要告警。
    fn resolve_chaizi_db(
        data_dir: Option<&Path>,
        spec: Option<&wind_config::schema::ChaiziSpec>,
    ) -> Option<std::path::PathBuf> {
        let c = spec.filter(|c| !c.db_path.is_empty())?;
        let p = Config::resolve_schema_resource(data_dir, &c.db_path);
        if p.is_none() {
            warn!(
                "拆字库不存在（用户/系统 schemas 目录均未找到）: {}",
                c.db_path
            );
        }
        p
    }

    /// 运行期格式串（cmdbar `dict.rev`、短语里的反查）要拆字而表没在内存里：后台加载，
    /// 本次照旧返回空、下次就有（与 `ensure_reverse_index_async` 同一模式）。配置里的消费者
    /// 不走这里——它们由 [`Self::sync_chaizi_assets`] 按 `DataNeeds` 装好。
    ///
    /// 可能每键都被调（短语求值），故先过闸：本代次（两次 sync 之间）只派一次，找不到库也记
    /// 「试过」。方案字段走缓存；库路径解析（查文件存在）与加载都在后台线程，按键线程不碰盘。
    /// 只在换入那一刻取反查表写锁：候选渲染持着它的读锁。
    pub(crate) fn ensure_chaizi_async(&self) {
        let generation = {
            let mut a = self.chaizi_assets.lock().unwrap_or_else(|e| e.into_inner());
            if a.db.is_some() || a.loading || a.tried {
                return;
            }
            a.tried = true;
            #[cfg(test)]
            {
                a.resolve_attempts += 1;
            }
            a.generation
        };
        let Some(spec) = self.engine_mgr.chaizi_spec() else {
            return;
        };
        let data_dir = self.data_dir();
        {
            let mut a = self.chaizi_assets.lock().unwrap_or_else(|e| e.into_inner());
            if a.generation != generation || a.db.is_some() || a.loading {
                return;
            }
            a.loading = true;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            self.chaizi_assets
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .loading = false;
            return;
        };
        let guard = ChaiziLoadingGuard(weak.clone());
        let spawned = std::thread::Builder::new()
            .name("chaizi-load".into())
            .spawn(move || {
                let _guard = guard;
                let Some(path) = Self::resolve_chaizi_db(data_dir.as_deref(), Some(&spec)) else {
                    return;
                };
                let fresh = wind_reverse::ReverseLookup::load(None, Some(&path));
                if let Some(c) = weak.upgrade() {
                    c.install_runtime_chaizi(generation, path, fresh);
                }
            });
        if let Err(e) = spawned {
            // 闭包（连同守卫）随 Err 一起被丢弃，守卫已复位 `loading`。
            warn!("起拆字表加载线程失败: {e}");
        }
    }

    /// 后台加载好的拆字表换入。`generation` 是发起加载时记下的代次：期间 sync 过（切了方案、
    /// 配置生效）就丢弃——那份可能是旧方案的表；期间已被 sync 装上（配置改成了要拆字）也不覆盖。
    pub(crate) fn install_runtime_chaizi(
        &self,
        generation: u64,
        path: std::path::PathBuf,
        mut fresh: wind_reverse::ReverseLookup,
    ) {
        let mut a = self.chaizi_assets.lock().unwrap_or_else(|e| e.into_inner());
        a.loading = false;
        if a.generation != generation || a.db.is_some() {
            return;
        }
        self.reverse
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .swap_chaizi(&mut fresh);
        a.db = Some(path);
        a.runtime = true;
        info!("拆字表按运行期用到后台加载完成");
    }

    /// 热重载用户配置：从磁盘重读 Config 并原子替换 bundle（轻量设置即时生效），
    /// 再 best-effort 刷新主题/工具栏。返回是否仍需重启才能完全生效。
    /// 轻量项（标点/智能符号/候选数/热键/配对/导航键等）即时生效；重型项（引擎/方案/
    /// 词典/字体）当前不在 bundle 内，需重启——为不打断使用，这里统一返回 false，
    /// 由调用方/用户按需重启。
    /// 同步拆字资产到当前来源方案（`chaizi_spec`：码表=自身、混输=其主码表成员、拼音=全局
    /// 主码表，与编码段同源）：库路径变了才重载反查表拆字段（含变为无配置时清空释放内存），
    /// 字根字体变了才重发（渲染端每次 set 都重建字体集，勿重复下发）。调用点=启动、方案切换
    /// （菜单/循环/设置页）、配置生效（`apply_data_needs`）。资源相对路径按「用户方案目录
    /// 优先、回落系统数据目录」解析（与方案文件同规则）。
    ///
    /// 库只在 [`DataNeeds::chaizi`](crate::data_needs::DataNeeds) 为真时加载，变为假时卸载
    /// （反查表拆字段清空）；字体照旧随方案下发（它不占本进程内存，撤换另有判据）。
    pub(crate) fn sync_chaizi_assets(&self) {
        self.sync_chaizi_assets_with(&self.data_needs());
    }

    fn sync_chaizi_assets_with(&self, needs: &crate::data_needs::DataNeeds) {
        let data_dir = self.data_dir();
        let spec = self.engine_mgr.chaizi_spec();
        let spec_db = Self::resolve_chaizi_db(data_dir.as_deref(), spec.as_ref());
        let new_font = spec
            .as_ref()
            .filter(|c| !c.font_path.is_empty())
            .and_then(|c| {
                // 缺失必须告警：与上面 `db_path` 对称。此前只有拆字库缺失会 warn，字根字体
                // 缺失则一路静默——而这两者的现象天差地别：库缺失是「悬停里根本没有拆字段」，
                // 字体缺失是「拆字段有内容、但每个字根都是方框」。后者用户看得见、日志里
                // 却一个字都没有，只能凭空猜「字体没装上」还是「家族名不对」还是「渲染没生效」
                // （issue #126 正是这一形态：方案 TOML、字体文件、家族名、字根码位四项全对）。
                let p = Config::resolve_schema_resource(data_dir.as_deref(), &c.font_path);
                if p.is_none() {
                    warn!(
                        "字根字体不存在（用户/系统 schemas 目录均未找到）: {}——\
                         悬停提示里的字根将显示为方框。请确认该文件已随方案一起安装到 schemas 目录下",
                        c.font_path
                    );
                }
                p.map(|p| (p.to_string_lossy().into_owned(), c.font_family.clone()))
            });
        let mut assets = self.chaizi_assets.lock().unwrap_or_else(|e| e.into_inner());
        // 进行中的运行期加载作废、运行期路径给一次重试（可能换了方案）。
        assets.generation += 1;
        assets.tried = false;
        // 配置要拆字 ⇒ 装方案的库。不要 ⇒ 卸，**除非**当前那份是运行期按需装的、且方案的
        // 拆字库没换（见 `ChaiziAssets::runtime`）。
        let keep = needs.chaizi || (assets.runtime && assets.db == spec_db);
        let new_db = if keep { spec_db } else { None };
        if assets.db != new_db {
            if new_db.is_none() && assets.db.is_some() {
                info!("卸载拆字表（当前配置无拆字消费者，或方案无拆字库）");
            }
            // 读盘在反查表写锁之外：候选渲染持着它的读锁，写锁里读文件等于让按键等磁盘。
            let mut fresh = wind_reverse::ReverseLookup::load(None, new_db.as_deref());
            self.reverse
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .swap_chaizi(&mut fresh);
            assets.db = new_db;
            assets.runtime = false;
        }
        if needs.chaizi {
            assets.runtime = false; // 归配置所有了
        }
        if let Some((path, family)) = chaizi_font_to_send(&assets.font, &new_font) {
            if path.is_empty() {
                info!("撤下字根字体（当前方案无可用的 [engine.chaizi] 字体）");
            } else {
                // 打成 info 而不是 debug：「字根字体到底下发过没有」是这条链路上唯一能把
                // 「没解析到」「解析到但渲染端加载失败」「一切正常」三态分开的观测点，
                // 而渲染端的失败告警在另一个日志域里（wind-ui）。缺了这一行，日志上
                // 「没有任何字根字体相关记录」既可能是没配、也可能是没走到这里。
                info!("下发字根字体: {path}（家族名 {family}）");
            }
            let _ = self
                .ui_tx
                .send(UiCommand::SetTooltipChaiziFont { path, family });
        }
        assets.font = new_font;
    }

    /// 同步注释词库（`[[ui.comment_dicts]]`）到反查表：解析路径列表，与上次生效的比对，
    /// **变了才重载**。调用点=启动、reload_user_config。
    ///
    /// 变更检测比的是**解析后的 `(路径, 适用方案)` 序列**（含顺序）而非配置结构：顺序即
    /// 优先级，调换两个库的位置必须触发重载；而只改 `label` 这类不影响加载的字段则不该重载。
    ///
    /// # ★ 挂载**不按方案过滤**——`schemas` 是查询期判据
    ///
    /// 早先这里按 `active_schema_id()` 筛挂载集合，于是切方案就要重挂 mmap（成本几乎全在
    /// 「读整份源文件算内容指纹」），而且判据本身是错的：临英背后是硬编码的 `english`
    /// 方案，`schemas = ["english"]` 在五笔方案下永远挂不上。
    ///
    /// 现在白名单随 spec 一起交给 `ReverseLookup`，在 `comment_of` 查询时求值（见
    /// [`wind_reverse::ReverseLookup::comment_of`]）。挂载集合于是只跟**配置**走、
    /// 不随方案抖动 ⇒ **切方案不再需要调本函数**。mmap 的常驻内存与库大小无关，
    /// 挂载全集的代价只是启动时每库一次指纹校验。
    ///
    /// 路径**以 `schemas/` 为基准**解析（`resolve_schema_resource`，用户目录优先、回落安装
    /// 目录），与拆字库、字根字体这些方案附属资源同一规则 —— 注释库本就是同类东西：
    /// 放在 `schemas/` 下、随整机备份走（`user_schemas_dir` 递归打包）、不参与召回。
    /// 配置里因此写 `comments/xxx.dict.yaml` 而非 `schemas/comments/xxx.dict.yaml`。
    /// 词库自带 `comment` 列时自动派生出的注释源（`[ui.candidate].auto_comment_dicts`）。
    ///
    /// 每次调用都重新检测：方案启用/停用、词库换层（用户目录覆盖安装目录）都会改变结果，
    /// 而检测本身只读每个词库的 YAML 头，代价与词库大小无关。
    ///
    /// 总开关关闭时返回空表 —— 于是配置里的引用空壳一条都认领不到，自动派生整体消失，
    /// 不需要在合并那侧再判一次开关。
    pub fn auto_comment_sources(&self) -> Vec<AutoCommentSource> {
        if !self.rt().config.ui.candidate.auto_comment_dicts {
            return Vec::new();
        }
        self.engine_mgr
            .declared_dict_files()
            .into_iter()
            .filter(|f| wind_reverse::declares_comment_column(&f.path))
            .map(|f| AutoCommentSource {
                id: auto_comment_id(&f.rel),
                label: if f.label.is_empty() {
                    f.rel.clone()
                } else {
                    f.label.clone()
                },
                rel: f.rel,
                path: f.path,
                schemas: f.schemas,
            })
            .collect()
    }

    pub(crate) fn sync_comment_dicts(&self) {
        let data_dir = Config::data_dir();
        let auto = self.auto_comment_sources();
        let specs = {
            let rt = self.rt();
            merge_comment_dicts(
                &rt.config.ui.comment_dicts,
                &self.system_comments.dicts,
                &auto,
            )
        };
        let (paths, ids) = resolve_comment_mounts(&specs, data_dir.as_deref());
        let named_only = named_only_mounts(&specs, &ids);
        let mut cur = self
            .comment_dict_paths
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *cur == paths {
            // 挂载集合没变，id 仍可能改了（只改 id 不改路径）：只刷新 id，不重挂
            let mut rl = self.reverse.write().unwrap_or_else(|e| e.into_inner());
            rl.set_comment_ids(&ids);
            rl.set_named_only(&named_only);
            return;
        }
        // 注释库缓存与词库 .wdat **同根**：`comment_cache_path` 自己按 schemas 下的目录链分
        // 命名空间（`schemas/comments/x.dict.yaml` → `<cache>/comments/x.wcmt`），与
        // `EngineManager::cache_path` 同构，不再另立一层专用目录。
        // 无缓存目录（便携/测试）时传 None，注释库退化为内存加载，功能不受影响。
        let cache_dir = Config::cache_dir();
        {
            let mut rl = self.reverse.write().unwrap_or_else(|e| e.into_inner());
            rl.reload_comments(&paths, cache_dir.as_deref());
            rl.set_comment_ids(&ids);
            rl.set_named_only(&named_only);
        }
        *cur = paths;
    }

    /// 同步 emoji 扩展表（`[input.emoji]`）。调用点=启动、`reload_user_config`，与
    /// [`Self::sync_comment_dicts`] 同一套「声明式变更」写法：变了才重载。
    ///
    /// 变更判据只取 `(enabled, categories)` ——**只有这两项影响加载结果**。`show_as`
    /// `max_per_word` 之类是查询期参数，把它们写进判据会让每次 reload 都重解析一遍
    /// 4600 行词表并重建缓存。
    ///
    /// # 加载失败**不记录** spec
    ///
    /// 三种结局分开处置：功能关（记录，置 None）、加载成功（记录）、加载失败（**不记录**）。
    /// 失败不记录 ⇒ 下次 sync 会重试，于是用户补跑 `gen-data` 后不必改配置就能生效；
    /// 重试成本只是解析一次路径后发现文件不在，可以忽略。
    pub(crate) fn sync_emoji_dict(&self) {
        let (enabled, categories, modules) = {
            let rt = self.rt();
            let e = &rt.config.input.emoji;
            // 列表含用户放进 `emoji/` 的文件：扫目录发生在这里（启动 / 配置重载），
            // 不在按键路径上。新放的文件要重载配置（或重开设置保存一次）才挂上。
            let modules: Vec<wind_config::EmojiModuleEntry> =
                Config::emoji_modules(Config::data_dir().as_deref())
                    .into_iter()
                    .filter(|m| e.module_enabled(&m.id))
                    .collect();
            (e.enabled, e.categories, modules)
        };
        // 值域校验放在去重之前：`scope`/`show_as` 不影响加载、不进变更判据，但配置变更
        // 时正是唯一该告警的时机。放进热路径的话每次按键都会刷一条。
        {
            let rt = self.rt();
            let e = &rt.config.input.emoji;
            const SCOPES: &[&str] = &["off", "exact", "all"];
            const SHOW_AS: &[&str] = &["after", "tail", "comment"];
            if !SCOPES.contains(&e.scope.as_str()) {
                warn!(
                    "input.emoji.scope 值不认识: {:?}（应为 {:?}），本次按 exact 处理",
                    e.scope, SCOPES
                );
            }
            if !SHOW_AS.contains(&e.show_as.as_str()) {
                warn!(
                    "input.emoji.show_as 值不认识: {:?}（应为 {:?}），本次按 after 处理",
                    e.show_as, SHOW_AS
                );
            }
        }
        let spec: EmojiSpec = (
            enabled,
            categories,
            modules.iter().map(|m| m.id.clone()).collect(),
        );
        {
            let cur = self.emoji_spec.lock().unwrap_or_else(|e| e.into_inner());
            if cur.as_ref() == Some(&spec) {
                return;
            }
        }
        // 关闭：连数据文件都不打开。未启用的用户为本功能付出的开销就此归零。
        if !enabled {
            *self.emoji_dict.write().unwrap_or_else(|e| e.into_inner()) = None;
            *self.emoji_spec.lock().unwrap_or_else(|e| e.into_inner()) = Some(spec);
            info!("emoji 扩展已关闭");
            return;
        }
        // 一表一缓存之前是主表 + 分类表合建的一份 `emoji.wemj`，现已无人读它
        if let Some(dir) = Config::cache_dir() {
            for f in ["emoji.wemj", "emoji.wemj.fp", "emoji.wemj.st"] {
                let _ = std::fs::remove_file(dir.join("emoji").join(f));
            }
        }
        // ★ 先放掉旧表，再重建。`categories` 翻转会改变指纹源 ⇒ `load_or_build` 要写新的
        // `.wemj` 并 rename 覆盖旧文件，而旧表此刻还被本字段的 Arc 持着 mmap——Windows 上
        // 覆盖被映射的文件返回拒绝访问（本机以 mmap + `os.replace` 探针实测 WinError 5，
        // `emojidict.rs` 的测试也为此显式 `drop(r)`）。不先放掉，表现为「打开分类词后
        // emoji 整个消失，随便再改一项配置又回来」——spec 未记录使下次 sync 重试成功，
        // 恰好把这个缺陷藏成了偶发。
        //
        // 池里只存 Weak（`reader_pool` 模块文档），本字段是唯一的强引用；写锁会等正在
        // `apply_emoji_suggestions` 里的读者退出，之后旧映射即释放。
        *self.emoji_dict.write().unwrap_or_else(|e| e.into_inner()) = None;
        let loaded = Self::load_emoji_dict(categories, &modules);
        let ok = loaded.is_some();
        *self.emoji_dict.write().unwrap_or_else(|e| e.into_inner()) = loaded;
        if ok {
            *self.emoji_spec.lock().unwrap_or_else(|e| e.into_inner()) = Some(spec);
        }
    }

    /// 实际加载 emoji 表：解析路径 → （繁简归一）→ 建/复用 `.wemj` 缓存 → mmap。
    ///
    /// **一表一缓存**（`<cache>/emoji/<id>.wemj`），查询期按顺序合并（[`EmojiTables`]）：
    /// 开关一个模块只是挂 / 不挂那张表，其余的缓存原样复用、永不因别人重建。合建一份的话，
    /// 翻任何一个开关都要重写整份 `.wemj`，而 Windows 上覆盖被映射的文件会被拒（见
    /// `sync_emoji_dict` 里「先放掉旧表」那段）。
    ///
    /// 顺序：rime-emoji 主表 → 分类表 → 附加模块（[`wind_config::EMOJI_MODULES`] 的顺序）。
    /// 主表在前，开了附加模块的用户原有的首个 emoji 不变。
    ///
    /// 一张都没加载成返回 `None`（＝功能不可用），**不降级成内存表**——理由见
    /// [`wind_dict::emojidict::load_or_build`]：本功能是纯锦上添花的可选项，而「常驻内存
    /// 随功能数量累积」正是它要避开的东西。
    fn load_emoji_dict(
        categories: bool,
        modules: &[wind_config::EmojiModuleEntry],
    ) -> Option<EmojiTables> {
        let data_dir = Config::data_dir();
        let dd = data_dir.as_deref();
        let Some(cache_dir) = Config::cache_dir() else {
            warn!("无缓存目录，emoji 扩展不可用");
            return None;
        };
        let cache_of = |id: &str| cache_dir.join("emoji").join(format!("{id}.wemj"));
        let mut readers = Vec::new();

        // ── rime-emoji 主表 / 分类表：键是繁体，必须归一 ──
        match Self::load_emoji_norm(dd) {
            Some((norm, norm_path)) => {
                let mut rime = vec![("word", "emoji/emoji_word.txt", true)];
                rime.push(("category", "emoji/emoji_category.txt", categories));
                for (id, rel, on) in rime {
                    if !on {
                        continue;
                    }
                    let Some(src) = Config::resolve_data_file(dd, rel) else {
                        warn!(
                            "emoji 表缺失（data/{rel}），已跳过（安装包应已附带；源码构建请跑 gen-data）"
                        );
                        continue;
                    };
                    // 归一表进 `fp_extra`：它不参与解析但决定解析结果，漏传会让 OpenCC 数据
                    // 升级后旧缓存被永久复用（见 `cache_fp::EMOJI_TAG`）。
                    if let Some(r) = wind_dict::emojidict::load_or_build(
                        &[src],
                        std::slice::from_ref(&norm_path),
                        &cache_of(id),
                        &[],
                        Some(&|s: &str| norm.convert_once(s)),
                    ) {
                        readers.push(r);
                    }
                }
            }
            None => warn!("emoji 繁简归一表不可用，rime-emoji 主表与分类表已跳过"),
        }

        // ── 附加模块（rime-frost 与用户放置的 OpenCC 表，键是简体，不归一）──
        for m in modules {
            let Some(src) = Config::resolve_data_file(dd, &m.file) else {
                warn!("emoji 模块 {} 的源文件缺失（{}），已跳过", m.id, m.file);
                continue;
            };
            // 用户模块的缓存名加前缀：用户文件名任意，别和内置模块、主表的缓存撞名。
            let cache_id = if m.user {
                format!("user.{}", m.id)
            } else {
                m.id.clone()
            };
            if let Some(r) = wind_dict::emojidict::load_or_build(
                &[src],
                &[],
                &cache_of(&cache_id),
                &m.sections,
                None,
            ) {
                readers.push(r);
            }
        }

        if readers.is_empty() {
            warn!("emoji 表一张都没加载成，功能不可用");
            return None;
        }
        let n: u32 = readers.iter().map(|r| r.entry_count()).sum();
        info!("Loaded emoji dict: {} 张表 {} entries", readers.len(), n);
        Some(EmojiTables(readers))
    }

    /// rime-emoji 的繁→简归一表。上游键全是繁体（实测 国0/國28、爱0/愛16），而本仓候选
    /// 恒简体 ⇒ 缺了它主表与分类表**一条都命中不了**，此时宁可不挂这两张，也不挂上一张
    /// 看着启用、实际全空的表。
    fn load_emoji_norm(
        dd: Option<&std::path::Path>,
    ) -> Option<(wind_transform::s2t::Dict, std::path::PathBuf)> {
        let Some(norm_path) = Config::resolve_data_file(dd, "opencc/TSCharactersDerived.octrie")
        else {
            warn!(
                "emoji 繁简归一表缺失（opencc/TSCharactersDerived.octrie）（安装包应已附带；源码构建请跑 gen-data）"
            );
            return None;
        };
        let norm_bytes = std::fs::read(&norm_path)
            .map_err(|e| warn!("读 emoji 归一表失败 {}: {}", norm_path.display(), e))
            .ok()?;
        let norm = wind_transform::s2t::Dict::parse(&norm_bytes).or_else(|| {
            warn!("emoji 归一表格式无效: {}", norm_path.display());
            None
        })?;
        Some((norm, norm_path))
    }

    /// 辅助码来源缓存失效：方案切换后置 `None`，下次进入辅助码时按新方案的
    /// `[engine.aux_code]` 重建（`ensure_aux_code_runtime` 另按来源清单比对，来源没变也会
    /// 因这里置空而重读文件表——切方案时顺带刷新磁盘上改过的码表）。
    ///
    /// 缓存是**全局共享一份**、不区分方案，而各方案码表不同（拼音用笔画表、双拼用
    /// 小鹤全码表）——切方案不清缓存会让双拼仍在用拼音那份表。与 `sync_chaizi_assets`
    /// / `sync_comment_dicts` 同源：方案附属资源随活跃方案切换重挂载。
    pub(crate) fn invalidate_aux_code_table(&self) {
        *self
            .aux_code_runtime
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // 键位环境随方案而变（全拼反引号是分隔符、双拼是自由键），故告警配额一并重置。
        self.aux_code_key_warned
            .store(false, std::sync::atomic::Ordering::Relaxed);
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
    /// per-app 规则**整条压过**全局：命中的应用用它自己的定位方式与自己的那份坐标，
    /// 未命中（或规则里没配这一项）才读全局。
    ///
    /// ⚠ 坐标必须与定位方式**同层取**，不能「模式取 per-app、坐标回落全局」：那样
    /// 「这个应用固定、但还没拖过」会去用全局那份为别的应用摆的坐标，候选窗一上来就
    /// 落在莫名其妙的位置，而用户根本没为这个应用设过位置。`(0,0)` 交给 UI 落默认锚点
    /// 才是这一档的正确答案。
    ///
    /// per-app 那一层读 [`ActiveCompat::lookups`] 预提取的值（本函数在每次候选窗刷新上），
    /// 与 [`Self::rule_candidate_fixed_pos`] 对焦点窗口现查同值。先取值放锁再读配置（`rt()` 另有锁）。
    pub(crate) fn candidate_fixed_pos(&self) -> (bool, i32, i32) {
        let rule = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lookups
            .candidate_pos;
        if let Some((fixed, x, y)) = rule {
            return (fixed, x, y);
        }
        let rt = self.rt();
        let c = &rt.config.ui.candidate;
        (c.is_fixed_position(), c.custom_x, c.custom_y)
    }

    /// 查 `compat.toml` 中该进程（该窗口）的候选窗定位规则；`None` = 未配置（跟随全局）。
    pub(crate) fn rule_candidate_fixed_pos(
        &self,
        proc_name: &str,
        win: &FocusWindow,
    ) -> Option<(bool, i32, i32)> {
        self.with_compat_rule(proc_name, win, |rule| {
            let rule = rule?;
            let mode = rule.candidate_position_mode?;
            Some((mode.is_fixed(), rule.candidate_x, rule.candidate_y))
        })
    }

    /// 当前注释模板库快照。
    pub(crate) fn comment_lib(&self) -> Arc<wind_config::comment_lib::CommentLibrary> {
        self.comment_lib
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 按新配置重建注释模板库。须在新配置快照换上之前调，使两者同时可见。
    fn rebuild_comment_lib(&self, cfg: &Config) {
        let lib = build_comment_lib(&self.system_comments, cfg);
        *self.comment_lib.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(lib);
    }

    pub(crate) fn refresh_config_in_memory(&self, mutate: impl FnOnce(&mut Config)) {
        let mut cfg = self.rt().config.clone();
        mutate(&mut cfg);
        let keys = schema_key_union(&self.engine_mgr);
        let bundle = std::sync::Arc::new(ConfigBundle::build(cfg, &keys));
        // 字符类 registry 由配置派生，**不在 schema_dirty 分支内**：
        // `input.rare_char.include_blocks` 在 input 段，改它不会把 schema 标脏，
        // 放进那个分支等于「改了没反应，切一次方案才生效」。见 `rebuild_charsets`。
        self.engine_mgr.rebuild_charsets(&bundle.config);
        self.rebuild_comment_lib(&bundle.config);
        *self.rt.write().unwrap_or_else(|e| e.into_inner()) = bundle;
        // 托盘 / 右键菜单 / 命令栏改配置走这里而不经 `reload_user_config`，扩展同样要知道。
        self.hook_lifecycle(wind_hook::msg::lifecycle::CONFIG_RELOADED);
        // 状态气泡去重缓存只在"内容配置不变"的前提下有效：改了 ui.status.items 之类后，
        // 同一状态该合成出不同文本，留着旧缓存会把改动后的第一次显示误判成"内容没变"而吞掉。
        self.last_status_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn reload_user_config(&self) -> bool {
        self.reload_user_config_quiet(false)
    }

    /// `quiet = true` 只压热重载**成功**的「设置已更新」（`config.set(…, toast="off")` 用）；
    /// 失败 / 语法错误的提示不受它影响，照常弹。
    pub fn reload_user_config_quiet(&self, quiet: bool) -> bool {
        match Config::load(Config::data_dir().as_deref()) {
            Ok(mut cfg) => {
                if let Some(overlay) = self.config_overlay.get() {
                    overlay(&mut cfg);
                }
                // 方案相关项（活跃/可用方案、全局上屏策略）是否变化：变了才热重建引擎，
                // 避免每次保存都丢词典缓存（拼音合并/unigram 重建开销大）。
                let old = self.rt();
                // `[input.correction]` 由引擎侧同步：只改提示开关的就地生效，改纠错规则 /
                // 「误读」勾选的并入重建。`|` 不短路：同步必须跑。
                let schema_dirty =
                    engine_reload_needed(&old.config, &cfg, self.profile().config_overlay)
                        | self.engine_mgr.sync_correction(&cfg);
                // 候选窗定位方式切换的边沿检测（见下方 ReportCandidatePos）。
                let cand_was_fixed = old.config.ui.candidate.is_fixed_position();
                let was_keep_all = old.config.schema.keep_all_loaded;
                drop(old);

                let keys = schema_key_union(&self.engine_mgr);
                let bundle = std::sync::Arc::new(ConfigBundle::build(cfg, &keys));
                let new_cfg = bundle.config.clone();
                // 同 `refresh_config_in_memory`：字符类 registry 不受 schema_dirty 门控。
                self.engine_mgr.rebuild_charsets(&new_cfg);
                self.rebuild_comment_lib(&new_cfg);
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
                // emoji 扩展表同理跟随全局配置（`[input.emoji]`），不属 schema——它按候选
                // 文本查表、与编码域无关，本就不是某个方案的属性。自带变更检测（只看
                // enabled/categories），未变即空操作。
                self.sync_emoji_dict();
                // 扩展段跟随全局配置；订阅方自己做变更检测。
                self.notify_extensions_config();
                // 用户词文本手改后，保存配置 / 设置页触发热重载时一并生效（设计 §5）。
                if let Some(store) = &self.store {
                    let changed = store.sync_user_texts();
                    if !changed.is_empty() {
                        info!("用户文本已重新载入：{} 项", changed.len());
                    }
                    self.apply_user_text_changes(&changed);
                }
                // 语言栏图标的呈现参数同理跟随全局配置（`[ui.langbar]`），也不属 schema。
                // 少了这一步，改角标形状/配色要重启才生效——「改了没反应、重启就好」正是
                // 本仓反复出现的那类缺陷（运行时镜像态没回灌）。自带变更检测，未变即空操作。
                #[cfg(all(feature = "desktop-ui", windows))]
                self.apply_langbar_config();

                if schema_dirty {
                    // 热重建方案集：清输入缓冲、刷新工具栏/状态，免重启切换方案。
                    self.engine_mgr.reload_from_config(&new_cfg);
                    // 主码表可能变更：拆字库/字根字体随之切换——由下面 `apply_data_needs`
                    // 统一做（变更检测，未变不动），它必须排在 `reload_from_config` 之后。
                    // 注释库**不需要**在这里复核：它只跟 `[[ui.comment_dicts]]` 走，
                    // 而那份配置在上面的非 schema_dirty 路径里已经同步过；方案集重建
                    // 换掉活跃方案也不改变该挂载什么（`schemas` 是查询期判据）。
                    {
                        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        s.input_buffer.clear();
                        s.candidates.clear();
                        s.preedit.clear();
                        // 引擎换了：直接辅助码的前缀快照是旧引擎的候选，一并丢弃。
                        s.direct_aux_prev = None;
                    }
                    self.notify_ui_hide();
                    self.push_state_update();
                    self.notify_toolbar(); // 方案名变化 → 刷新工具栏标签
                    // 重建把反查索引与单字全码表都置空了（`reload_from_config`），必须补一次
                    // 预热，否则它们要等**首次上屏**才惰性重建——而自动造词的取码与查重正好
                    // 跑在那条路径上，`flush_draft_batch` 在未就绪时是**整批丢弃**。
                    // 表现：用户在设置页打开自动造词后，最先打的那几个词被静默吞掉，
                    // 大词库上（真机 253 万条实测秒级构建）能吞掉一整句。
                    //
                    // 辅助码引用的方案来源也在 `prewarm_indexes` 里（在用集合只随 `schema` 段与
                    // `input.temp_pinyin` 变，二者都会标脏，故不必在非脏分支另补）。
                    //
                    // ⚠️ 必须后台：`prewarm_indexes` 阻塞秒级，而本函数是设置页 RPC 调过来的。
                    // 与启动线程、测试、移动端 prepare() 共用同一个 `prewarm_indexes`。
                    // 重建把活跃方案重置成了磁盘上的 `schema.active`（= 全局方案）。焦点若在
                    // 配了 schema 规则的应用里，要把它对齐回去，否则设置页保存一次就把该应用
                    // 冲回全局。按应用方案的自动切换不写盘，本身不会走到这里。
                    self.resync_app_schema_after_reload();
                    if let Some(weak) = self.self_weak.get().cloned() {
                        let _ = std::thread::Builder::new()
                            .name("reload-prewarm".into())
                            .spawn(move || {
                                if let Some(c) = weak.upgrade() {
                                    // 重建只建回了当前方案：按常驻策略把常驻集合（全部方案，
                                    // 或临拼目标 / 英文）补回来，否则「切换零等待」要到重启才恢复。
                                    // 宿主声明按需加载（移动端）时与启动同样不建。
                                    if c.eager_prewarm.load(std::sync::atomic::Ordering::Relaxed) {
                                        c.prewarm_schemas_on_start();
                                    }
                                    c.prewarm_indexes();
                                }
                            });
                    }
                }
                // 按需数据随配置收放（拆字表、用户层编码索引、简繁表），两个分支都要：
                // 注释模板 / 悬停段 / 编码来源档都不会把 schema 标脏。
                self.apply_data_needs();
                // 方案常驻策略变了：就地摘除 / 后台预热（这个键不标脏，见 engine_reload_needed）。
                self.apply_residency_change(was_keep_all);
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
                // 简繁两个方向互斥。写入侧的归一在这里**落盘**（见
                // `normalize_conversion_exclusivity`），不能只在下面读时压掉——UI 不是唯一
                // 入口，命令行改 config.toml、导入配置包、别的客户端都能造出两个都开的组合，
                // 只压不写的话配置文件里那个非法组合会一直留着，用户在设置页看到两个都亮、
                // 实际只有一个在工作，而且怎么改都改不掉。
                let (s2t_on, t2s_on) = new_cfg.input.conversion_directions();
                self.normalize_conversion_exclusivity(&new_cfg);
                let filter_changed = {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.toolbar_visible = new_cfg.ui.toolbar.visible;
                    s.s2t_enabled = s2t_on;
                    s.t2s_enabled = t2s_on;
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
                // `ui.toolbar.fullscreen_watch` 从关改到开时把复查线程起回来。关掉那个
                // 方向由线程自己在下一拍读到配置后退出，不必在这里管。
                self.ensure_fullscreen_watch();
                self.sync_global_hotkeys(); // keys.global_hotkeys 增删/改键即时生效
                self.sync_direct_switch_hotkey(); // keys.activate_ime 改键/清空即时生效
                // capslock 绑定的增删即时生效：配上才装全局钩子，删掉立刻卸载。
                self.sync_capslock_hook();
                // 推送英文自动配对配置到 TSF 客户端（client_token=0 = 广播到所有活跃客户端）
                self.push_english_pair_config(0);
                self.push_pair_via_comp_config(0); // 配对插入提交方式（组合语义 vs 直写，compat 可覆盖）
                self.push_jump_out_keys_config(0); // 配对跳出键同步（英文模式跳出 + 中文转发放行）
                // 密码框抑制策略：内存开关随配置回灌（关掉即解除当前抑制）+ 推给 DLL 吃键门控。
                self.set_password_suppress_enabled(new_cfg.input.password_force_english);
                self.push_custom_en_punct_config(0); // 英半列自定义标点：DLL 据此吃键转发
                self.push_cn_passthrough_punct_config(0); // 中文模式该透传的标点：DLL 据此**不**吃
                self.push_en_passthrough_punct_config(0); // 同上，英文标点态那份（超集）
                self.push_space_custom_cols_config(0); // 空格行自定义列：DLL 据此吃空缓冲空格
                self.push_space_before_digit_config(0); // 中数间补空格：DLL 据此吃汉字后的数字
                self.push_pair_state_ttl_config(0); // 配对状态时效（DLL 侧闸门据此判陈旧）
                self.push_ctrl_space_toggle_config(0); // Ctrl+空格 切换开关：DLL 据此吃不吃该键（GH#172）
                // 诊断采集开关本身与配置文件无关（会话级），这里重推纯属幂等保险——
                // 与 password_suppress 同样处理，让"重载一次"能修好任何 DLL 侧状态漂移。
                self.push_diag_snapshot_config(0);
                // ★★ 热键表重新下发。上面的 `ConfigBundle::build` 已经**重编**过一遍
                // （`Compiler::compile` 读的就是这份新配置），但编出来的表要经 activation
                // push 才到得了 DLL —— 而 TSF 侧的 `RegisterHotKey` 抢占（GLOBAL 位）与
                // 吃键闸门都建立在那张表上。上面那几条 `push_*_config` 各自推的是自己那
                // 一小段配置，没有一条捎带热键表。
                //
                // 不推的表现是「设置页把热键改了，按下去还是老的；切一次焦点或重启才
                // 生效」—— 因为热键表此前**只随 focus_gained 下发**。
                //
                // ⚠️ 定向推给当前活跃客户端，**不广播**：`push_activation_status` 里的
                // `hostRenderAvail` 位是按 token 的 pid 算出来的，广播会把一个进程的值
                // 污染给其它客户端。没有活跃 token 就什么都不做 —— 那种情况下下一次
                // focus_gained 自会带上新表。
                let active_token = self.push_server.active_token();
                if active_token != 0 {
                    self.push_activation_status(active_token);
                }
                // 语法错误时**取代**「设置已更新」，而不是两条都弹：报成功再报失败会让
                // 用户以为是两件事，而这里只有一件——他刚存的设置里有一部分没生效。
                // 此时 `self.rt()` 已是上面换进去的新 bundle，读到的就是本次加载的降级记录。
                if !self.notify_config_syntax_error()
                    && !quiet
                    && !self
                        .quiet_reload_toast
                        .load(std::sync::atomic::Ordering::Relaxed)
                {
                    self.show_toast(
                        "设置已更新",
                        ToastPosition::BottomCenter,
                        ToastKind::Success,
                    );
                }
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

    /// 方案文件/方案覆盖里某些段解析失败、已回落出厂默认值 ⇒ 提示用户。返回是否弹了。
    ///
    /// # 为什么这条提示必须存在
    ///
    /// 段级降级（`EngineManager::salvage_schema`）救回了方案的其余部分，代价是**被降级的
    /// 那一段悄悄变回出厂值**。没有这条提示，用户的处境是「我明明在 `schema_overrides` 里
    /// 写了，重启后没反应，也没有任何报错」——这正是本轮要消灭的失败形态，
    /// 与 [`Self::notify_config_syntax_error`] 同一条纪律。
    ///
    /// ⚠️ 列表限长：项数通常只有一两个，但一份被批量改坏的方案能攒出十几条，全列进 toast
    /// 会撑爆版面，而那时具体是哪几项本就没意义。同 `UnparsableLayer::lines_phrase`。
    /// ★ 数量只由外层的 `{total}` 说一次，括号里超长时只补「等」不再报数——两处都带数字会
    /// 拼出「有 5 项设置无法识别（… 等 5 项）」。同一个语病 `lines_phrase` 已经踩过一次。
    ///
    /// # ⛔ 不要挂到配置热重载上
    ///
    /// 落点刻意只有两个：**服务启动**与**切到该方案**——都是「用户正要用这个方案」的时刻。
    /// 顺手加到 `reload_config` 的 toast 优先级链里看着很自然，但那条链每次保存设置都会走，
    /// 而方案降级与「刚保存了什么设置」没有因果关系，且 `behavior_cache` 未失效时读到的还是
    /// 上一次的记录 ⇒ 用户每点一次保存都被同一条提示打断，把「设置已更新」也挤掉了。
    /// 提示的价值在于出现在困惑的那一刻，不在于出现得多。
    pub fn notify_schema_degradation(&self, schema_id: &str) -> bool {
        let behavior = self.engine_mgr.behavior_for(schema_id);
        if behavior.degraded_items.is_empty() {
            return false;
        }
        let name = self.engine_mgr.schema_name(schema_id);
        let label = if name.is_empty() { schema_id } else { &name };
        const MAX_LISTED: usize = 3;
        let total = behavior.degraded_items.len();
        let listed = behavior
            .degraded_items
            .iter()
            .take(MAX_LISTED)
            .cloned()
            .collect::<Vec<_>>()
            .join("、");
        let items = if total > MAX_LISTED {
            format!("{listed} 等")
        } else {
            listed
        };
        self.show_toast(
            &format!("方案「{label}」有 {total} 项设置无法识别（{items}），已按出厂默认处理"),
            ToastPosition::BottomCenter,
            ToastKind::Error,
        );
        true
    }

    /// 显示一次性通知 toast（约 2.5 秒后自动隐藏）。供配置热重载、词库就绪、错误等一次性事件。
    pub(crate) fn show_toast(&self, text: &str, position: ToastPosition, kind: ToastKind) {
        self.show_toast_ex(text, position, kind, DEFAULT_TOAST_MS, None);
    }

    /// 完整形态的 toast：另给时长与自定义强调色，供 `ui.toast` 的具名参数落地。
    /// `duration_ms` 为 0 时取默认时长——0 在 UI 侧会被 `max(1)` 变成"闪一下就消失"，
    /// 而调用方传 0 的意思一律是"没指定"。
    pub(crate) fn show_toast_ex(
        &self,
        text: &str,
        position: ToastPosition,
        kind: ToastKind,
        duration_ms: u64,
        accent: Option<[u8; 4]>,
    ) {
        self.emit_toast(
            text.to_string(),
            position,
            kind,
            if duration_ms == 0 {
                DEFAULT_TOAST_MS
            } else {
                duration_ms
            },
            accent,
        );
    }

    /// 确保 `schema_id` 的反查索引在**后台**建好；已就绪、或已有线程在建，则什么都不做。
    ///
    /// # 为什么只能派活、不能等
    ///
    /// 唯一调用点在候选刷新链路（`notify_ui_update`）里，那里**正持着 state 锁**，
    /// 且整条链路身处 TSF→服务的**同步 IPC** 中。索引对超大词库是秒级构建，在此等待
    /// 就是让整台机器停住——真机 feihuzj2（253 万条）实测卡死 29.5 秒。
    /// 故本次渲染照常完成，只是暂时没有编码段；建好后由本线程重渲染一次补上。
    ///
    /// # 去重
    ///
    /// 构建期间的**每一次按键**都会走到这里，没有 `is_building_reverse_index` 这道闸，
    /// 就会每按一键 spawn 一个线程去建同一份上百 MB 的东西。
    /// （引擎侧那道闸用 `try_lock`，与 `ensure_loaded` 的 single-flight 同源。）
    /// **阻塞地**建好那些「首次查询才会惰性构建」的索引（反查索引）。
    ///
    /// 生产路径由启动后的预热线程调用（见 `construct.rs`）；测试与移动端 `prepare()`
    /// 也走这里——三方共用一条路径，避免各写一份预热逻辑而互相漂移。
    ///
    /// ⚠️ 会阻塞秒级，**只可在后台线程/测试里调用**，绝不能进按键链路。
    pub fn prewarm_indexes(&self) {
        // 按键路径要用的方案级字段先填缓存（输入流程零读盘）。
        self.engine_mgr.warm_hot_schema_fields();
        // 悬停 [编码] / 编码提示取 code_source_schema，词语联想取 assoc_word_schema。
        // 混输下两者通常都解析到同一个主码表成员，故去重后一般只建一份。
        // 两者相同时不必去重：prewarm_reverse_index 幂等，第二次直接返回。
        //
        // 只在有消费者时建（`DataNeeds::reverse_index`）：主码表反查索引常驻 2~3 MB，
        // 悬停[编码]、`${code_rev}`、联想、辅助码、自动造词都没开的用户不该付。
        // 之后才打开的功能由 `apply_data_needs` 后台补建；运行期格式串（cmdbar `dict.rev`）
        // 里的 `${code_rev*}` 由 `eval_text_var` 首次用到时后台建。
        if self.data_needs().reverse_index {
            let ids = [
                self.engine_mgr.code_source_schema(),
                self.engine_mgr.assoc_word_schema(),
            ];
            for id in ids.iter().filter(|s| !s.is_empty()) {
                let t0 = std::time::Instant::now();
                if self.engine_mgr.prewarm_reverse_index(id) {
                    debug!("预热反查索引 {} 用时 {:?}", id, t0.elapsed());
                }
            }
        }
        // 辅助码引用的码表方案（`schema:<id>`，含临拼目标方案引用的）：进入辅助码的门卫
        // 要求其反查索引已就绪、按键线程绝不现建——不预热的话每次启动后第一次按辅助码键
        // 都静默不进。辅助码关着时集合为空，不白建。
        for id in self.engine_mgr.aux_code_schemas_in_use() {
            let t0 = std::time::Instant::now();
            if self.engine_mgr.prewarm_reverse_index(&id) {
                debug!("预热辅助码来源反查索引 {} 用时 {:?}", id, t0.elapsed());
            }
        }
        // 智能联想档的下一词表：派后台加载，不在这里等（约 2 MB 文本）。
        if self
            .assoc_config()
            .source_enabled(wind_assoc::AssocSource::Bigram)
        {
            self.ensure_assoc_bigram_async();
        }
        // 词语联想的用户词 / 临时词文本索引（t185）：联想开着才建——它要扫整张用户词表，
        // 关着联想的用户不该付这笔。没预热也不致命：首次联想会起后台重建，那一次只出系统词。
        if self.assoc_config().kind != wind_assoc::AssocKind::Off {
            let sid = self.engine_mgr.assoc_word_schema();
            let t0 = std::time::Instant::now();
            if self.engine_mgr.prewarm_user_assoc(&sid) {
                debug!("预热联想用户词索引 {} 用时 {:?}", sid, t0.elapsed());
            }
        }
        // 自动造词开着才预热单字全码表：它是另一次全量扫描（同量级），关着的用户
        // 不该为一个用不到的功能付出启动时间与内存。开着而不预热则第一次上屏必卡，
        // 因为造词跑在上屏线程上。
        if self.auto_phrase_enabled() {
            let sid = self.engine_mgr.code_source_schema();
            if !sid.is_empty() {
                let t0 = std::time::Instant::now();
                if self.engine_mgr.prewarm_single_char_codes(&sid) {
                    debug!("预热单字全码表 {} 用时 {:?}", sid, t0.elapsed());
                }
            }
        }
    }

    /// 后台预热辅助码在用的方案来源（见 [`EngineManager::aux_code_schemas_in_use`]）。
    ///
    /// 切方案、保存 / 重置方案设置（`refresh_schema_derived_config`）之后调：这些操作会换掉
    /// 「在用」集合或让反查索引失效，不补一次预热，下一次按辅助码键就会因索引未就绪而静默
    /// 不进。全局配置重载走 `reload_user_config` 里那次 [`Self::prewarm_indexes`]，已含本集合。
    /// 只派活不等（`spawn_index_warm` 自带去重与延迟提示），可在 RPC / 切方案路径上调。
    ///
    /// [`EngineManager::aux_code_schemas_in_use`]: wind_engine::EngineManager::aux_code_schemas_in_use
    pub(crate) fn warm_aux_code_sources(&self) {
        for id in self.engine_mgr.aux_code_schemas_in_use() {
            self.spawn_index_warm(&id, false);
        }
    }

    fn ensure_reverse_index_async(&self, schema_id: &str) {
        self.spawn_index_warm(schema_id, false);
    }

    /// 同上，另带**单字全码表**（自动造词取码用）。
    ///
    /// 造词路径两样东西都要：全码表用来按 `[[encoder.rules]]` 组码，反查索引用来做
    /// 「系统词库是否已有这个码+词」的查重。两者都是惰性全量构建，而造词跑在上屏
    /// （按键）线程上，故一律后台预热、本次跳过。
    pub(crate) fn ensure_word_encoding_async(&self, schema_id: &str) {
        self.spawn_index_warm(schema_id, true);
    }

    pub(crate) fn spawn_index_warm(&self, schema_id: &str, with_single_char: bool) {
        if schema_id.is_empty() {
            return;
        }
        let index_ready = self.engine_mgr.reverse_index_if_ready(schema_id).is_some();
        let single_char_ready =
            !with_single_char || self.engine_mgr.single_char_codes_ready(schema_id);
        // 已被 build_guard 放弃的方案不再起线程：构建线程结束时会通知重绘，重绘又回到这里，
        // 不挡住就是「每次重绘起一个线程」的无限循环。造词缺了反查索引也做不了查重，
        // 故单字全码表一并不建。
        if (index_ready && single_char_ready)
            || self.engine_mgr.reverse_index_skipped(schema_id)
            || self.engine_mgr.is_building_reverse_index(schema_id)
        {
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        let sid = schema_id.to_string();
        // 提示交给一个计时线程延后弹，见 [`INDEX_TOAST_DELAY`]。
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let done = done.clone();
            let weak = weak.clone();
            let toast = std::thread::Builder::new()
                .name("reverse-index-toast".into())
                .spawn(move || {
                    std::thread::sleep(INDEX_TOAST_DELAY);
                    if done.load(std::sync::atomic::Ordering::Acquire) {
                        return;
                    }
                    let Some(c) = weak.upgrade() else {
                        return;
                    };
                    // upgrade 期间可能刚好干完，再确认一次
                    if done.load(std::sync::atomic::Ordering::Acquire) {
                        return;
                    }
                    // 让用户知道「编码/联想这一会儿是缺的」而不是坏了。
                    // ⚠️ 这条提示**伴随**真实工作、且以下面的重渲染收尾；不要把它改成那种
                    // 「弹个准备中然后什么也不发生」的提示——那种做法在本仓被删过一次
                    // （见 handle_mode.rs 关于 is_loaded 守卫的说明）。延后弹并不违背这条：
                    // 它只在工作**确实还在进行**时才出现。
                    c.show_toast(
                        "正在建立词库索引…",
                        ToastPosition::BottomCenter,
                        ToastKind::Info,
                    );
                });
            if let Err(e) = toast {
                debug!("无法启动索引提示计时线程: {e}（不影响索引构建）");
            }
        }
        let done_if_unspawned = done.clone();
        let spawned = std::thread::Builder::new()
            .name("reverse-index-build".into())
            .spawn(move || {
                let Some(c) = weak.upgrade() else {
                    done.store(true, std::sync::atomic::Ordering::Release);
                    return;
                };
                let t0 = std::time::Instant::now();
                let built_index = c.engine_mgr.prewarm_reverse_index(&sid);
                if with_single_char {
                    c.engine_mgr.prewarm_single_char_codes(&sid);
                }
                // 必须在重渲染**之前**置位：重渲染本身也要花时间，拖在它后面会让
                // 一次快速复用照样弹出提示——那正是本次要消灭的现象。
                done.store(true, std::sync::atomic::Ordering::Release);
                if !built_index && !with_single_char {
                    return; // 等锁期间已被别的线程建好，且无别的活要干
                }
                debug!("后台建成词库索引 {} 用时 {:?}", sid, t0.elapsed());
                // 重渲染当前这屏候选，把编码段补上——否则要等用户下一次按键。
                // 与 reload_user_config / set_filter_mode 的「改完就地重刷」同构。
                let s = c.state.lock().unwrap_or_else(|e| e.into_inner());
                if !s.input_buffer.is_empty() {
                    c.notify_ui_update(&s);
                }
            });
        if let Err(e) = spawned {
            // 构建线程没起来 ⇒ 计时线程手里的 done 永远不会置位，会弹出一条
            // 「正在建立词库索引…」而根本没有人在建。用留下的这份把它按掉。
            done_if_unspawned.store(true, std::sync::atomic::Ordering::Release);
            warn!("无法启动反查索引构建线程: {e}");
        }
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

    /// 配置文件**语法不合法**时弹一次提示，返回是否弹了。
    ///
    /// # 为什么这条提示非有不可
    ///
    /// 语法错误此前只留一行 INFO 日志（默认不打印），而后果是整层配置失效——用户看到的
    /// 是「我的设置怎么全变回默认了」，唯一线索藏在一个他不会去看、也看不到的地方。
    /// 真机上更糟：后台的 `key_actions` 物化拿空表当种子把 config.toml 整个覆盖了。
    /// 写盘那一侧已经在 `wind-config` 里堵死（`ConfigDegradation::unparsable`），
    /// 但**堵住损坏不等于告诉用户**——不提示的话，他的设置依然静默失效，只是这次文件还在。
    ///
    /// 时长给足 8 秒（默认 2.5 秒）：这条提示带着文件路径和行号，是要照着去改的，
    /// 不是「已保存」那种扫一眼即可的回执。
    ///
    /// 只报**第一个**出问题的层：多层同时坏是极罕见的，而堆两条路径会把 toast 撑爆。
    pub fn notify_config_syntax_error(&self) -> bool {
        let rt = self.rt();
        let Some(u) = rt.config.degradation.unparsable.first() else {
            return false;
        };
        // ★ 判据是 `is_salvaged()`，不是 `skipped_lines` 非空——「跳过了几行」不等于
        // 「救回来了」，啃到上限仍失败时两者同时成立。用后者会在一个字都没加载的时候
        // 报「该行设置未生效」，暗示其余还在，而实际整份都没生效。
        // 行号走 `lines_phrase()`：紧凑（啃不动的文件会攒到 32 个行号，全列进 toast
        // 会把版面撑爆）且自带量词——模板里再拼「第 {} 行」会拼出「等 32 处 行」的语病。
        let text = if !u.is_salvaged() {
            format!("配置文件语法错误，本次全部未生效\n{}", u.path.display())
        } else {
            format!(
                "配置文件{}语法错误，相应设置未生效\n{}",
                u.lines_phrase(),
                u.path.display()
            )
        };
        drop(rt);
        self.emit_toast(
            text,
            ToastPosition::BottomCenter,
            ToastKind::Error,
            8000,
            None,
        );
        true
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
    /// 下发候选字号与字体（`ui.candidate.font_size` / `ui.font.*`）。原样下发用户值：
    /// 与主题的合并（0 / 空 = 跟随主题）只在渲染端 `CandidateWindow::refresh_effective_theme`
    /// 一处完成，这里不做判断。
    ///
    /// 启动时须排在首次 `push_theme` **之前**（见 construct.rs）：用户字体先到，主题到达时
    /// 被它覆盖掉的主题字族就不会被当作「将要使用」去查存在性、误报缺字体。
    pub(crate) fn send_candidate_font(&self, config: &wind_config::Config) {
        let _ = self.ui_tx.send(UiCommand::SetCandidateFontSize(
            config.ui.candidate.font_size,
        ));
        // 候选字体（ui.font.family / fallback / scripts；family 空=跟随主题节点字族/内置默认）。
        let font = &config.ui.font;
        let _ = self.ui_tx.send(UiCommand::SetCandidateFont {
            family: font.family.clone(),
            fallback: font.fallback.clone(),
            scripts: font
                .scripts
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            weight: font.weight,
        });
    }

    pub(crate) fn apply_ui_config(&self) {
        let bundle = self.rt();
        let cand = &bundle.config.ui.candidate;
        // 候选排列方向（ui.candidate.layout）：config 是**基线**的持久化真相源，
        // 但实际下发要叠加当前模式的布局意图（见 layout.rs）——热重载不能把模式级覆盖清掉。
        // 此前这里无条件下发 config 值：模式进行中改任意一项设置都会静默取消强制竖排，
        // 且因为不留痕迹而极难复现。
        *self
            .candidate_orientation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Orientation::from_layout_str(&cand.layout);
        {
            // 调用点（启动 / 配置重载）均不持 state 锁；加锁顺序 state → candidate_layout_sent
            // 与 notify_ui_update 一致，不构成环。
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            // 热重载可能换了活跃方案（设置页改 schema.active 即走这里），先同步方案级覆盖。
            self.sync_schema_scope(&mut state);
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
            // 看不见的网格里 ←→ 还会移高亮而不移编码光标：随候选窗一起收起（§5.5）。
            self.collapse_grid(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()));
            self.clear_hover();
            self.emit_hide();
        }
        self.send_candidate_font(&bundle.config);
        // 翻页栏 / 页码显示覆盖（ui.candidate.pager_bar_display / page_number_display）
        let (pager_h, pager_v) = cand.pager_bar_display.both_str();
        let _ = self.ui_tx.send(UiCommand::SetPagerDisplay {
            h: pager_h.to_string(),
            v: pager_v.to_string(),
        });
        let (num_h, num_v) = cand.page_number_display.both_str();
        let _ = self.ui_tx.send(UiCommand::SetPageNumberDisplay {
            h: num_h.to_string(),
            v: num_v.to_string(),
        });
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
        // 用户偏移 / 主题阴影覆盖（ui.candidate.offset_x/y、shadow）
        let _ = self.ui_tx.send(UiCommand::SetCandidateOffset {
            x: cand.offset_x,
            y: cand.offset_y,
        });
        let _ = self
            .ui_tx
            .send(UiCommand::SetCandidateShadow(cand.shadow != "off"));
        // 主题动画开关（ui.theme.animation）
        let _ = self.ui_tx.send(UiCommand::SetThemeAnimation(
            bundle.config.ui.theme.animation,
        ));
        // 候选窗尺寸下限（ui.candidate.min_window_width_* / min_window_height_* / min_rows，抗抖动）
        self.send_candidate_min_size();
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
            hover_reveal: tb.auto_hide_hover_reveal,
        });
        let _ = self.ui_tx.send(UiCommand::SetToolbarVertical(tb.vertical));
        // 工具栏格的显隐与顺序（ui.toolbar.items）。解析（含非法项告警、留空回落全集）
        // 在此侧完成，渲染端只收一份「照这个顺序画」的声明——它读不到配置。
        let _ = self
            .ui_tx
            .send(UiCommand::SetToolbarLayout(parse_toolbar_items(
                &tb.items,
                &tb.buttons,
            )));
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

    /// 展开态专属键（见 `grid_key`）的完整处置：键归网格时返回结局，否则 `None`。
    pub(crate) fn grid_key_action(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        let changed = self.grid_key(state, data)?;
        if changed && let Some(act) = self.after_nav_changed(state) {
            return Some(act);
        }
        Some(KeyAction::Consumed)
    }

    /// 编码区光标移动后的返回：刷新候选窗，并带上组合区新插入符位置（与默认 ←/→ 臂同形）。
    /// 只回 `Consumed` 的话宿主里的插入符不会跟着动。
    /// Ctrl+←/→ 整音节跳是否启用：`session_actions` 里写了 `ctrl+left` / `ctrl+right = "none"` 即关
    /// （同时热键编译器也不再向宿主转发该键）。
    pub(crate) fn ctrl_syllable_enabled(&self, vk: u32) -> bool {
        let name = if vk == keymap::VK_LEFT {
            "ctrl+left"
        } else {
            "ctrl+right"
        };
        !self
            .rt()
            .config
            .keys
            .session_actions
            .get(name)
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
    }

    /// 编码区的音节停靠点（缓冲内字节位置，见 [`preedit_cursor::syllable_stops`]）。
    ///
    /// 拆分串取引擎对**整串缓冲**的切分（`preedit_split_body` / 码表整句的 `preedit_codetable_body`），
    /// 不随高亮候选变——否则高亮一动，光标的停靠点就跟着变。对不上缓冲（双拼等）→ `None`。
    fn caret_syllable_stops(state: &State) -> Option<Vec<usize>> {
        [&state.preedit_split_body, &state.preedit_codetable_body]
            .into_iter()
            .filter(|split| split.contains('\''))
            .find_map(|split| preedit_cursor::syllable_stops(&state.input_buffer, split))
    }

    /// 编码区光标左 / 右移一步（rime 导航器语义）。返回是否真的动了。
    ///
    /// - `smart` = 真（默认 ←/→，rime `Rewind` / `Forward`）：光标在音节边界上整音节跳，
    ///   在音节中间按单字符；
    /// - `smart` = 假（Ctrl+←/→，rime `left_by_syllable` / `right_by_syllable`）：总是整音节跳。
    ///
    /// 不足两个音节或拆分对不上缓冲时都退回单字符。左右两端按 rime 回绕（仅音节跳时）。
    pub(crate) fn move_caret_syllable(state: &mut State, right: bool, smart: bool) -> bool {
        let jump = Self::caret_syllable_stops(state).and_then(|stops| {
            preedit_cursor::syllable_jump(&stops, state.input_cursor_pos, right, smart)
        });
        if let Some(pos) = jump {
            state.input_cursor_pos = pos;
            return true;
        }
        let mut ed = preedit_cursor::BufEdit::new_cased(
            &mut state.input_buffer,
            &mut state.input_cursor_pos,
            &mut state.input_buffer_cased,
        );
        if right {
            ed.move_right()
        } else {
            ed.move_left()
        }
    }

    pub(crate) fn caret_moved_action(&self, state: &mut State) -> KeyAction {
        let text = state.preedit.clone();
        let caret_pos = self.composition_caret(state);
        self.notify_ui_update(state);
        KeyAction::UpdateComposition { caret_pos, text }
    }

    /// 开 / 关高亮项的悬停提示：悬停目标设为高亮项，已是则清掉。
    /// 展开态的高亮项是平铺下标（高亮行之前的整行都算），与 `notify_ui_update` 同口径。
    fn toggle_hover_tip(&self, state: &mut State) -> KeyAction {
        let per_page = self.page_size(state);
        let flat = match state.grid_top {
            Some(top) => state.current_page.saturating_sub(top) * per_page,
            None => 0,
        } + state.selected_index;
        let target = if self.hover_target() == flat as i32 {
            -1
        } else {
            flat as i32
        };
        self.hover_index
            .store(target, std::sync::atomic::Ordering::Relaxed);
        self.notify_ui_update(state);
        KeyAction::Consumed
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
        // 展开态专属键（←→ / Esc）先于绑定表：它们在网格里另有含义，且必须赶在各模式的
        // 光标移动臂与 Esc 臂之前截下——本函数恰好是所有模式都先经过的那一处
        // （快捷输入例外，它的光标与 Esc 臂在本函数之前，自己先调 `grid_key_action`）。
        if let Some(act) = self.grid_key_action(state, data) {
            return Some(act);
        }
        let shift = data.modifiers & MOD_SHIFT != 0;
        let action = self.session_action_for(data.key_code, shift, include_printable)?;
        if action.requires_candidates() && state.candidates.is_empty() {
            return None;
        }
        // 只认纯翻页动词：`AuxCode(PageNext)` 共键降级的那一下不归 `keys.overflow.page_key`。
        // 值是该键翻不动时的策略：翻页动作的后缀，翻屏动词跟随全局档。
        let page_overflow = match action {
            wind_config::SessionAction::PagePrev(o) | wind_config::SessionAction::PageNext(o) => {
                Some(o)
            }
            wind_config::SessionAction::PageScreenPrev
            | wind_config::SessionAction::PageScreenNext => Some(wind_config::PageOverflow::Global),
            _ => None,
        };
        // 「翻屏」动词：展开态翻整屏，未展开时与翻页完全一致（下面落到同一条 nav 执行）。
        let screen_verb = matches!(
            action,
            wind_config::SessionAction::PageScreenPrev | wind_config::SessionAction::PageScreenNext
        );
        let nav = match action {
            wind_config::SessionAction::HighlightUp => keymap::NavAction::HighlightUp,
            wind_config::SessionAction::HighlightDown => keymap::NavAction::HighlightDown,
            wind_config::SessionAction::PagePrev(_)
            | wind_config::SessionAction::PageScreenPrev => keymap::NavAction::PagePrev,
            wind_config::SessionAction::PageNext(_)
            | wind_config::SessionAction::PageScreenNext => keymap::NavAction::PageNext,
            wind_config::SessionAction::Cancel => {
                // 无会话时放行：空闲按 Tab 该是宿主的制表符，不是「取消一个不存在的输入」。
                // 判据与 `cancel_session` 的适用范围一致，见那里。
                if !Self::has_input_session(state) {
                    return None;
                }
                // 展开态下取消键先收起（与 Esc 同一条两段式，见 `grid_key`）。
                if Self::grid_active(state) && self.collapse_grid(state) {
                    self.notify_ui_update(state);
                    return Some(KeyAction::Consumed);
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
            // 的同名分支）。门卫在 `enter_aux_code` 里（未开启 / 无码表 / 该键被音节分隔符占用
            // 都返回 None），此处不重复判断——两处各写一份判据必然漂移。
            //
            // 无候选时走不到这里：上面的 `requires_candidates()` 已经放行了按键，
            // 于是空闲按 Tab 仍是宿主的制表符。
            wind_config::SessionAction::AuxCode(share) => {
                if let Some(act) = self.enter_aux_code(state, data.key_code) {
                    return Some(act);
                }
                // 门卫没过。**顺序即优先级**：专用触发键到此为止（不吞键，落该键原语义）；
                // 共键（`aux_code:page_next`）降级为下翻页，落到下面那段统一的 nav 执行
                // ——不在这里自己写一份 `page_next` + `notify_ui_update`，那是第二份要跟着
                // `flipped` / 混输 preedit 同步一起维护的翻页逻辑。
                //
                // ★ 这条降级同时覆盖三种情形，无需各写一个特判：辅助码态内继续翻页
                // （`active.is_some()` 被拒）、功能未开启 / 方案无码表时退化成纯翻页键。
                match share {
                    wind_config::AuxCodeShare::Solo => return None,
                    wind_config::AuxCodeShare::PageNext => keymap::NavAction::PageNext,
                }
            }
            // 单字输入：原地开关并重建候选，**不顶字、不退模式**——用户切的是「这一码
            // 出什么」，正在打的这串码要留着。
            //
            // ⚠️ **无会话时放行**，判据与 `Cancel` 同侧（有会话即可，不要求有候选）。
            // 本表收的是 Tab / 翻页键那一批**宿主另有原义**的键：空闲时按下必须还给宿主，
            // 否则用户照 `data/config.toml` 里的示例写了 `tab = "single_char"`，Tab
            // 在所有程序里当场失效。「走到这里说明用户明确绑了它，故恒吞键」是错的——
            // `Cancel` 臂那句同款守卫正是本函数在无会话时也会被走到的证据。
            //
            // 判据取「有会话」而非「有候选」：单字档下某个码本来就可能一条候选都不剩，
            // 那时若按「无候选」放行，用户就再也切不回去了（`requires_candidates` 把
            // `SingleChar` 与 `Cancel` 并列，为的就是这个）。
            //
            // 想空闲时也能切 → 绑到 `keys.key_actions`（`BoundAction::SingleChar`）：
            // 那张表收的是符号键 / z / 组合键，没有宿主原义要让，故那边恒吞键。
            //
            // 状态泡走 `show_tip_locked`：本函数持着 state 锁，`show_tip` 会重入取锁。
            // ⚠️ 开启后候选可能整个变空（这个码一个单字都没有），那时**只剩**
            // 状态泡这一个反馈——不能因为「候选窗自己会变，用户看得见」就省掉它。
            wind_config::SessionAction::SingleChar(a) => {
                if !Self::has_input_session(state) {
                    return None;
                }
                if let Some(label) = self.apply_single_char_action(state, a) {
                    self.show_tip_locked(state, label);
                }
                // `Consumed` = 吞键、组合区不变（同翻页与移高亮的结局）：开关只换候选
                // 内容，用户正在打的那串码要原样留着。
                return Some(KeyAction::Consumed);
            }
            // 注释态：判据与 `SingleChar` 同侧——无会话放行（Tab 等键空闲时要还给宿主），
            // 有会话即吞键；`session` 档另由 `requires_candidates` 在上游按有候选放行。
            wind_config::SessionAction::CommentState(a) => {
                if !Self::has_input_session(state) {
                    return None;
                }
                match self.apply_comment_state_action(state, a) {
                    Ok(label) => self.show_tip_locked(state, label),
                    Err(e) => warn!("comment_state: {e}"),
                }
                return Some(KeyAction::Consumed);
            }
            // 上屏高亮：与各模式空格臂**同一个函数**，副作用逐字一致（见 `commit_highlighted`）。
            // 不走空格臂本身——那里有 `space_as_input` 这类**空格键**的变体，本动词不跟；
            // 联想 `space_commits = false` 则跟（判据在 `commit_highlighted` 里）。
            // 无候选已被上面的 `requires_candidates` 放行。
            wind_config::SessionAction::CommitHighlighted => {
                return self.commit_highlighted(state);
            }
            // 悬停提示开关：把悬停目标设为高亮项（再按一次清掉），其余全走鼠标悬停那条通路。
            // 展开态的高亮项是平铺下标（高亮行之前的整行都算），与 `notify_ui_update` 同口径。
            //
            // 绑在 → 上时（t372）：编码区光标不在末尾，→ 照旧右移光标；到了末尾才开关提示。
            // 只在主路判（overlay 各有自己的光标与 → 臂，同 `CaretRight`）。
            wind_config::SessionAction::ToggleTip => {
                if data.key_code == keymap::VK_RIGHT
                    && state.active.is_none()
                    && Self::has_input_session(state)
                {
                    let moved = preedit_cursor::BufEdit::new_cased(
                        &mut state.input_buffer,
                        &mut state.input_cursor_pos,
                        &mut state.input_buffer_cased,
                    )
                    .move_right();
                    if moved {
                        return Some(self.caret_moved_action(state));
                    }
                }
                return Some(self.toggle_hover_tip(state));
            }
            // 编码区光标移动（macOS 式方向键，见 design/macos-style-arrow-keys.md）。
            //
            // 只在主路生效：overlay（临拼 / 快捷输入 / Url / Email / Special）各有自己的
            // 缓冲光标与 ←→ 臂，`state.active` 非 `None` 时放行让位——绑了本动词不能把
            // overlay 的编辑键抢走。展开态（网格）的 ←→ 在本函数最前已被 `grid_key_action`
            // 截走，轮不到这里。无会话放行：空闲方向键是宿主的。
            //
            // `caret_right` 在**末尾**改为上屏高亮候选（macOS「→ 进选词」）；无候选时没有
            // 可上屏的东西，纯吞键——组合进行中把 → 交给宿主会移动宿主光标，与组合区错位。
            // 移动成功只刷新组合区（caret 位置变了，缓冲与候选都没变，无需重查）。
            wind_config::SessionAction::CaretLeft | wind_config::SessionAction::CaretRight(_) => {
                if state.active.is_some() || !Self::has_input_session(state) {
                    return None;
                }
                let end = match action {
                    wind_config::SessionAction::CaretRight(e) => Some(e),
                    _ => None,
                };
                let right = end.is_some();
                let moved = {
                    let mut ed = preedit_cursor::BufEdit::new_cased(
                        &mut state.input_buffer,
                        &mut state.input_cursor_pos,
                        &mut state.input_buffer_cased,
                    );
                    if right {
                        ed.move_right()
                    } else {
                        ed.move_left()
                    }
                };
                // 末尾那一下按 `caret_right[:end]` 的末尾动作办：上屏 / 高亮下移 / 开关提示。
                if let Some(end) = end
                    && !moved
                    && !state.candidates.is_empty()
                {
                    match end {
                        wind_config::CaretEnd::Commit => return self.commit_highlighted(state),
                        wind_config::CaretEnd::Tip => return Some(self.toggle_hover_tip(state)),
                        // 落到下面统一的 nav 执行，与 `highlight_down` 同一条路。
                        wind_config::CaretEnd::Next => keymap::NavAction::HighlightDown,
                    }
                } else if moved {
                    return Some(self.caret_moved_action(state));
                } else {
                    return Some(KeyAction::Consumed);
                }
            }
            // 整音节跳（出厂绑 Ctrl+←/→，rime `left_by_syllable` / `right_by_syllable`）。
            // 门卫同 `CaretLeft`：overlay 让位、无会话放行；无切分时退回单字符。
            wind_config::SessionAction::SyllablePrev | wind_config::SessionAction::SyllableNext => {
                if state.active.is_some() || !Self::has_input_session(state) {
                    return None;
                }
                let right = matches!(action, wind_config::SessionAction::SyllableNext);
                return Some(if Self::move_caret_syllable(state, right, false) {
                    self.caret_moved_action(state)
                } else {
                    KeyAction::Consumed
                });
            }
            // 上屏编码：这里只登记、吞键，持锁的这一层没法重入分派；由出口
            // `redispatch_commit_code` 把这个键当回车再走一遍。
            //
            // 无会话放行，判据与 `Cancel` 同侧。联想态也放行：那里没有编码，回车在联想态
            // 是「收窗并把回车交还宿主」，借过来就成了把 Tab / Ctrl 交还宿主。
            // 绑在回车自己身上没有意义，也会让重新分派再命中一次，一并放行。
            wind_config::SessionAction::CommitCode => {
                if !Self::has_input_session(state)
                    || state.assoc_active()
                    || data.key_code == keymap::VK_RETURN
                {
                    return None;
                }
                self.commit_code_pending
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return Some(KeyAction::Consumed);
            }
            // 命令：执行，**当前组合原样不动**（不清空、不上屏），吞键。
            //
            // 无会话放行，判据与 `Cancel` / `SingleChar` 同侧：本表收的 Tab / 翻页键那一批
            // 宿主另有原义，空闲时必须还给宿主。想空闲时也能按 → 绑到 `keys.key_actions`。
            //
            // 持 state 锁调用是安全的：`run_bound_command` 只起线程，不等它跑完。
            wind_config::SessionAction::Command(expr) => {
                if !Self::has_input_session(state) {
                    return None;
                }
                self.run_bound_command(&expr);
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
        //
        // 展开态（`grid_top` 有值）整组改写：高亮上下 = 换行、翻页 = 滚一行（PageUp/PageDown
        // 可翻一屏），见 `candidate_grid`。
        // 网格只在横排出现，`flipped`（竖排专属）在这里恒为 false，两支不会相遇。
        // 收起态按下翻页先试展开：展开了就只展开、不翻页（设计 D3）。
        let changed = if Self::grid_active(state) {
            match nav {
                keymap::NavAction::HighlightUp => self.grid_move_row(state, false),
                keymap::NavAction::HighlightDown => self.grid_move_row(state, true),
                keymap::NavAction::PagePrev => {
                    self.grid_page_key(state, false, data.key_code, screen_verb)
                }
                keymap::NavAction::PageNext => {
                    self.grid_page_key(state, true, data.key_code, screen_verb)
                }
            }
        } else {
            match nav {
                keymap::NavAction::HighlightUp if flipped => self.move_down(state),
                keymap::NavAction::HighlightDown if flipped => self.move_up(state),
                keymap::NavAction::HighlightUp => self.move_up(state),
                keymap::NavAction::HighlightDown => self.move_down(state),
                keymap::NavAction::PagePrev => self.page_prev(state),
                keymap::NavAction::PageNext => self.try_expand_grid(state) || self.page_next(state),
            }
        };
        if changed {
            if let Some(act) = self.after_nav_changed(state) {
                return Some(act);
            }
        } else if page_overflow.is_some_and(|o| self.page_key_overflow_to_punct(state, data, o)) {
            // 翻不动（`page_next` 的检索范围放宽已在上面先行裁决，放宽了就是 `changed`）：
            // 不消费，让键落到兜底标点臂，由它统一顶高亮候选并出符号（中文标点、全角、
            // 智能符号、自动配对、`punct_commit` 都在那一处）。
            return None;
        }
        Some(KeyAction::Consumed)
    }

    /// 高亮 / 页码变了之后的收尾：混输按新高亮重算组合区形态，再重绘候选窗。
    ///
    /// 返回 `Some` = 组合区形态变了且编码嵌在宿主里，须把新组合串回传给宿主。
    /// 收进一处，是因为网格专属键（←→）与绑定表里的导航动作都要走它——少走一处，
    /// 混输下用 ←→ 移到拼音候选时组合区就不跟着变形。
    fn after_nav_changed(&self, state: &mut State) -> Option<KeyAction> {
        // 混输高亮跟随：普通模式下高亮在五笔↔拼音候选间移动可能切换 preedit 形态
        // （原始码 ↔ 音节拆分）。重算 preedit；若形态变化且嵌入编码（app_inline），须回传
        // 组合串使宿主内联编码同步；候选窗模式仅 notify_ui_update 刷新即可。
        // 拼音候选各带自己的切分（`pinan` 高亮「平安」`pin'an`、高亮「皮」`pi'nan`），纯拼音
        // 下形态同样会变。临拼 / mix 文本透镜与主路同一套跟随（overlay 对齐主路）。
        // 门控：普通模式存在拆分形态或有候选自带切分（混输下首选与原始码同形时前者为空）
        // ——纯五笔不触发，零回归；其余 overlay 各自维护 preedit，不碰。
        let mut composed: Option<KeyAction> = None;
        let follows = match state.active {
            None => {
                !state.preedit_split_body.is_empty()
                    || state
                        .candidates
                        .iter()
                        .any(|c| c.meta.preedit_split.is_some())
            }
            Some(ModeKind::TempPinyin | ModeKind::Mix(_)) => true,
            Some(
                ModeKind::TempEnglish
                | ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Special(_)
                | ModeKind::RareChar
                | ModeKind::Reverse
                | ModeKind::AuxCode,
            ) => false,
        };
        if follows {
            let before = state.preedit.clone();
            self.sync_preedit_to_highlight(state);
            if state.preedit != before {
                // ★ 必须用**有效**归属而不是配置原值：压制态下候选窗不下发，编码只剩组合区
                // 这一条出口，这里不回传就等于让游戏聊天框里的编码停在旧形态。
                if self.preedit_in_app_effective() {
                    let text = state.preedit.clone();
                    let caret_pos = self.reshaped_caret(state);
                    composed = Some(KeyAction::UpdateComposition { text, caret_pos });
                }
            }
        }
        self.notify_ui_update(state);
        composed
    }

    /// 编码栏形态随高亮变了之后回传宿主的光标（TSF 位置），按新形态换算，与按键路径同源：
    /// 缓冲中间编辑时光标不在串尾，分隔符增减会挪动它的显示位置，按串尾给会把宿主光标甩到末尾。
    /// 方向键移高亮（`after_nav_changed`）与整句切换（`try_sentence_cycle_key`）共用。
    fn reshaped_caret(&self, state: &State) -> u32 {
        if state.active.is_some() {
            self.overlay_caret(state)
        } else {
            self.composition_caret(state)
        }
    }

    /// 翻页键翻不动时是否按 `keys.overflow.page_key = "commit_and_input"` 交给标点臂。
    ///
    /// 只在主路（overlay 各有自己的翻页语义）、该键能出符号（PageUp/PageDown/Tab 不算，
    /// 数字另有选词臂）、且本批候选**从未翻过页**时成立——判据取 `state.paged` 而非
    /// `current_page == 0`，理由同 [`Self::symbol_buffer_key_free`]：翻回首页的用户仍在
    /// 翻页，此刻按键突然出字会让人当场打错。
    fn page_key_overflow_to_punct(
        &self,
        state: &State,
        data: &KeyEventData,
        overflow: wind_config::PageOverflow,
    ) -> bool {
        state.active.is_none()
            && !state.paged
            && self.page_key_overflow_commits(overflow)
            && punct_char(data.key_code, data.modifiers & MOD_SHIFT != 0)
                .is_some_and(|c| !c.is_ascii_digit())
    }

    /// 该翻页键「翻不动时是否顶屏出符号」：绑定动作的后缀（自定义按键里的
    /// `page_prev:commit_and_input` / `page_prev:ignore`）优先，无后缀回落全局档 `page_key`。
    fn page_key_overflow_commits(&self, overflow: wind_config::PageOverflow) -> bool {
        match overflow {
            wind_config::PageOverflow::CommitAndInput => true,
            wind_config::PageOverflow::Ignore => false,
            wind_config::PageOverflow::Global => {
                self.rt().config.keys.overflow.page_key == "commit_and_input"
            }
        }
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
        self.note_word_boundary();
        state.clear_comment_flips();
        match state.active {
            Some(ModeKind::TempPinyin) => self.exit_temp_pinyin(state),
            Some(ModeKind::TempEnglish) => self.exit_temp_english(state),
            Some(ModeKind::Url) => self.exit_url_mode(state),
            Some(ModeKind::Email) => self.exit_email_mode(state),
            Some(ModeKind::Unicode) => self.exit_unicode_mode(state),
            Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) | Some(ModeKind::Reverse) => {
                self.exit_special_mode(state)
            }
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
                // `exit_aux_code` 已把 `active` 还原成**来源模式**。取消的语义是「连来源
                // 一起放弃」，故还要把那个模式也退掉——否则从临拼进来的会话按 Esc 后，
                // `active` 停在 `TempPinyin`、`temp_pinyin_buffer` 还留着码，而下面
                // 无条件 `ClearComposition` 已告诉宿主「组合没了」，正是上面那段注释
                // 描述的自相矛盾，只是换了个缓冲。主输入路来源时 `active == None`，
                // 走 `_` 臂 `reset_pinyin_composition`，与改动前逐字等价。
                match state.active {
                    Some(ModeKind::TempPinyin) => self.exit_temp_pinyin(state),
                    None
                    | Some(
                        ModeKind::TempEnglish
                        | ModeKind::Url
                        | ModeKind::Email
                        | ModeKind::Unicode
                        | ModeKind::Special(_)
                        | ModeKind::RareChar
                        | ModeKind::Reverse
                        | ModeKind::Mix(_)
                        | ModeKind::AuxCode,
                    ) => self.reset_pinyin_composition(state),
                }
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
    /// 会话态动词 `commit_code` 的执行：本次按键若命中了它，把触发键当成一次回车重新
    /// 分派，并让这一下回车恒走「上屏编码」那一支。
    ///
    /// ★ 借回车的臂而不是另写上屏：回车在主路与各 overlay 里各有一条臂（引导符归还、
    /// 已转换前缀、全角、英文造词、上屏历史各有讲究），另写一份就得跟着它们逐个维护。
    /// 与回车唯一的不同是不看 `input.enter_behavior`——用户绑这个键要的就是上屏编码，
    /// 「回车清空编码」是回车这个键的配置。
    ///
    /// 不在命中处直接执行：那里持着 `State` 锁，分派要自己取锁。按键出口
    /// （`handle_key_event_policed`、CapsLock 钩子）都在锁外，于是挂在那里。
    pub(crate) fn redispatch_commit_code(
        &self,
        data: &KeyEventData,
        action: KeyAction,
    ) -> KeyAction {
        use std::sync::atomic::Ordering::Relaxed;
        if !self.commit_code_pending.swap(false, Relaxed) {
            return action;
        }
        let enter = KeyEventData {
            key_code: keymap::VK_RETURN,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            ..data.clone()
        };
        self.commit_code_forced.store(true, Relaxed);
        let act = wind_bridge::handler::MessageHandler::handle_key_event(self, &enter);
        self.commit_code_forced.store(false, Relaxed);
        self.commit_code_pending.store(false, Relaxed);
        act
    }

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
            // 通配结果不参与顶码（spec §3.2）：混输下 4 码通配串 +1 键会把整串判成超长字面串，
            // 通配探测放行、拼音也接不了，前缀恰为顶码前缓冲，显示首选就是通配结果——
            // 在此剔除，与通配关闭 / 纯码表的行为一致（该键只判字面串，不上屏通配结果）。
            (c.source == CandidateSource::CodeTable && !c.is_wildcard)
                || ((c.is_phrase || c.is_command) && self.phrase_has_exact_code(state, &pre_buf))
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
        //
        // 通配组码不顶字（spec §3.2）。引擎的 `handle_top_code` 是字面语义（`aaaza` 在它看来
        // 是「超码长 + 无匹配 + 无后继」的典型溢出），故短路必须落在这里。
        let top_code = if self.wildcard_pattern_of(state).is_some() {
            None
        } else {
            self.engine_mgr
                .handle_top_code(&state.input_buffer)
                .filter(|_| !self.phrase_vetoes_top_code(state, &state.input_buffer))
                // 缓冲里混进了入缓冲符号 ⇒ 这串不是本方案的编码，没有「码长溢出」可言。
                // 不否决的话，码表方案下打 `sun-panel` 到第 5 个字符就会把 `sun` 顶上屏、
                // 余码续打，用户正打的标识符当场被拆两半。判据与候选侧同源，见
                // `buffer_has_literal_symbol`。
                .filter(|_| !self.buffer_has_literal_symbol(state))
                // 切点修正：引擎把 prefix 固定切在 `max_code_length`，而**短语码长不受方案满码长
                // 约束**（5 码短语 `zzsfz` 落在 4 码五笔里）。顶码前的缓冲若恰是一条精确码短语，
                // 就以短语码为切点。不修则 `zzsfza` 被切成 `zzsf` + `za`，与 `pre_buf` 对不上而
                // 落进「多级溢出」分支，又因 `zzsf` 在码表无字放弃顶码——表现为「进空码不顶码」。
                // pre_buf 长度恰为满码长时两种切法本就重合（`zzbd` 一类），行为不变。
                .map(|(engine_top, remainder)| {
                    if self.phrase_has_exact_code(state, &pre_buf) {
                        let rem: String = state
                            .input_buffer
                            .chars()
                            .skip(pre_buf.chars().count())
                            .collect();
                        (engine_top, rem)
                    } else {
                        (engine_top, remainder)
                    }
                })
        };
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
                            // 命令文本同样只过 `maybe_convert`）。
                            Some(text) => self.commit_top_text(
                                state,
                                &prefix,
                                text,
                                None,
                                None,
                                &remainder,
                                cand.source,
                                None, // 命令文本不是候选实体，无从是草稿
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
                        let freq_text = cand.freq_text().to_string();
                        // 顶出的首选就是一条草稿候选 ⇒ 这次上屏要按「用过」转正。
                        let draft_boundary = cand.is_draft.then_some(cand.boundary);
                        return self.commit_top_text(
                            state,
                            &prefix,
                            cand.text,
                            Some(&freq_text),
                            s2t_override.as_deref(),
                            &remainder,
                            source,
                            draft_boundary,
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
                    None,
                    None, // 引擎码表纯文本，无候选对象可承载变体覆盖
                    &remainder,
                    CandidateSource::CodeTable,
                    None, // 同上：纯文本没有候选实体，无从是草稿
                );
            }
        }

        // 全码自动上屏 / 满码空码清空（schema.auto_commit_at_full / clear_on_empty_max）。
        match self.update_candidates(state) {
            InputOutcome::AutoCommit(text) => {
                // 自动上屏文本取自首候选（handle_candidate.rs 构造 AutoCommit 时同源）。
                // 记账码同取首候选（按来源分流，见 `freq_code`），无候选时退回输入缓冲。
                // 记账文本同理取首候选的 `freq_text`（模板词是源文本，GH#177）。
                let (source, code, freq_text) = state
                    .candidates
                    .first()
                    .map(|c| {
                        let code = self.freq_code(&state.input_buffer, c);
                        (c.source, code, c.freq_text().to_string())
                    })
                    .unwrap_or_else(|| {
                        let buf = state.input_buffer.clone();
                        (CandidateSource::default(), buf, text.clone())
                    });
                // `6a-draft`：满码自动上屏 / 空码顶出也是「词落了屏」，草稿候选同样算用过。
                // 判据与顶码一致（见 `commit_top_text`）：算数的不是用户按了哪颗键，而是词有
                // 没有真上屏——开着 `auto_commit_at_full` 时候选栏根本没机会让人按选择键。
                self.promote_draft_on_auto_commit(state, &text, &code);
                let out = self.commit_candidate(state, &text, &freq_text, None, source, &code);
                // 满码自动上屏同样要接联想（t185），出口与手动选词一致。
                return self.auto_commit_then_assoc(state, out, &text);
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
        // 符号通配键（spec §3.4）：在标点流水线之前截住，接线点与码元闸门同处。
        // 让位时落回下面的码元判定，与关闭通配时逐键相同。
        if self.wildcard_enters(state, lower) {
            return Some(self.accumulate_code_char(state, lower, ch));
        }
        if !self.can_enter_buffer(state, lower) {
            return None;
        }
        Some(self.accumulate_code_char(state, lower, ch))
    }

    /// 组码中符号入缓冲闸门（`input.buffer_symbol_chars`，出厂 `-`）：打 `sun-panel`。
    ///
    /// # 与 [`Self::try_code_char_gate`] 的分工是「谁让位」
    ///
    /// 那道闸门管的是**真码元**（方案 `input_chars`）：字符进缓冲并参与码长/顶码判定，
    /// 且**无条件夺取**该键——方案作者说了算，撞了什么功能由
    /// [`Self::code_char_conflicts`] 告警而不阻止。
    ///
    /// 本闸门相反，是**让位优先**：只捡该键此刻空着的那一格（见
    /// [`Self::symbol_buffer_key_free`]），任何活身份都能把它挡回去。所以它不需要、也不该
    /// 进那张冲突清单——它本身就不制造冲突。
    ///
    /// # 为什么非要有这一格
    ///
    /// `-` 同时是英文标识符的高频字符和出厂翻页键（`keys.page_keys` 含 `minus_equal`），
    /// 撞车处此前**两边都不通**：配成翻页键时首页按下走
    /// [`Self::apply_session_action`] 的「命中即吞」（`page_prev` 返回 false 也照吞，
    /// 用户观感是按了没反应）；不配翻页键则落兜底标点臂顶码上屏。两条路都打不出
    /// `sun-panel`，而把 `-` 写进 `input_chars` 又会彻底夺走翻页身份——且拼音方案根本
    /// 走不通那条路（`PinyinEngine::input_chars` 完全由双拼布局推导，不读该字段）。
    ///
    /// 复用 [`Self::accumulate_code_char`] 而不另起一条插入路径：顶码、全码自动上屏、
    /// 满码空码清空、影子串、光标中插全在那里，另写一份就是第二套组码语义。
    /// 拼音下顶码对它恒不触发（顶码要求显示首选是码表候选），码表下则与「把该符号配成
    /// 码元」逐字同路——那正是同一件事该有的样子。
    pub(crate) fn try_symbol_buffer_gate(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        // Ctrl/Alt 组合不是字符输入（同 `try_code_char_gate`，纵深防御）。
        if data.modifiers & MOD_SHORTCUT != 0 {
            return None;
        }
        // ★ 只在组码中。空闲时这些键归标点流水线/宿主——否则用户在任何程序里都打不出减号。
        // 判据取 `input_buffer` 而非 `has_input_session`：已上屏前缀（`committed_text`）
        // 非空而缓冲已空时，这一码是新一轮的开头，符号当首码没有意义。
        if state.input_buffer.is_empty() {
            return None;
        }
        let shift = data.modifiers & MOD_SHIFT != 0;
        let ch = printable_char(data.key_code, shift)?;
        // 字母不走这里（同 `try_code_char_gate`）：字母臂上还有 z-fallback 等专属判定，
        // 从这里抄近路会把它们全绕过去。
        if ch.is_ascii_alphabetic() {
            return None;
        }
        if !self.rt().config.input.buffer_symbol_chars.contains(ch) {
            return None;
        }
        if !self.symbol_buffer_key_free(state, data.key_code, shift) {
            return None;
        }
        // 符号无大小写，缓冲形态与影子串原形同值。
        Some(self.accumulate_code_char(state, ch, ch))
    }

    /// 缓冲里有没有经 [`Self::try_symbol_buffer_gate`] 进来的符号 —— 即「这一串还是不是
    /// 本方案的编码」。
    ///
    /// 两个消费点，**必须同一个谓词**：
    /// 1. [`Self::update_candidates`]：是 ⇒ 不问引擎，候选空着（否则拼音引擎按合法前缀
    ///    容错，`sun-panel` 会顶着 `sun` 的候选，空格上屏「孙」）；
    /// 2. [`Self::accumulate_code_char`] 的顶码否决：是 ⇒ 不顶码（否则码表方案下打到第 5
    ///    个字符就把 `sun` 顶上屏、余码 `-p` 续打，用户正打的标识符当场被拆成两半）。
    ///
    /// 两处判据若分家，会分裂成「候选空着但还在顶码」这种自相矛盾的中间态。
    ///
    /// ⚠️ `!active_is_code_char` 这一条不能省：方案作者把某符号配成**真码元**
    /// （`[engine.codetable] input_chars`）时，它是合法编码的一部分，该照常查引擎、
    /// 照常参与顶码。本谓词只管本闸门放进来的那类字面符号。
    ///
    /// ⚠️ 分隔符那一条同样不能省，而且是**反着**踩出来的：`symbol_buffer_key_free` 已经
    /// 让位给音节分隔符 / 英文分词符了，于是 `'` 是经**那条通路**进的缓冲——可本谓词只问
    /// 「这个字符在不在白名单里」，照样把它认成自己放进来的，`xi'an` 的候选于是被清空。
    /// 让位与识别必须用同一套判据，否则「让了位」只是让到一半。
    ///
    /// 判据落在**字符**上（`char_to_main_vk` 反查回键）而不是记一个「这一帧是谁放进来的」
    /// 标志位：缓冲会被退格、光标中插改写，来源标志迟早与内容对不上，而字符本身不会。
    pub(crate) fn buffer_has_literal_symbol(&self, state: &State) -> bool {
        let bundle = self.rt();
        let set = &bundle.config.input.buffer_symbol_chars;
        if set.is_empty() {
            return false;
        }
        state
            .input_buffer
            .chars()
            .any(|c| set.contains(c) && self.char_is_literal_symbol(c))
    }

    /// 缓冲里的这个字符，是[`Self::try_symbol_buffer_gate`] 放进来的那一类吗。
    ///
    /// 两条排除各有来历：
    /// - **真码元**（方案 `input_chars`）是合法编码的一部分，该照常查引擎、照常参与顶码；
    /// - **音节分隔符 / 英文分词符**根本不经本闸门（`symbol_buffer_key_free` 给它们让了位），
    ///   把它们算进来等于把自己让出去的那条路又堵死一次。
    fn char_is_literal_symbol(&self, c: char) -> bool {
        if self.engine_mgr.active_is_code_char(c) {
            return false;
        }
        // 通配键进缓冲走的是通配闸门，不是本闸门；它是查询的一部分，不是字面符号。
        // 同时列在 `buffer_symbol_chars` 里也一样——否则候选被清空、通配形同虚设。
        if self.engine_mgr.active_wildcard_key() == Some(c) {
            return false;
        }
        match char_to_main_vk(c) {
            Some(vk) => !self.manual_separator_key(vk) && !self.english_phrase_separator_key(vk),
            // 反查不到键的字符只可能来自本闸门（它的入口就是 `printable_char`）。
            None => true,
        }
    }

    /// 这个键此刻有没有别的活身份——有就让位，[`Self::try_symbol_buffer_gate`] 的唯一判据。
    ///
    /// 四问覆盖 [`Self::code_char_conflicts`] 的 owners 链，**两处必须同增同减**：
    /// 那边漏一臂是「本该告警却没告警」，这边漏一臂是「本该让位却夺了键」——后者用户
    /// 立刻可见，且会被归因成「翻页键忽然不灵了」。
    ///
    /// 那边的次选键与以词定字键在这里**不单独问**：
    /// [`Self::select_key_offset`] / [`Self::select_char_index`] 都是
    /// [`Self::session_action_for`] 的派生（取 `candidate_ordinal` / `char_ordinal`），
    /// 第一问已经拦下。那边分开列是因为要给用户报出**是哪个功能**占了键，这里只需要
    /// 知道「有没有人占」。⚠️ 哪天它们改成独立取表，这里就得跟着补问。
    ///
    /// # 两张不经会话表的表也要问
    ///
    /// 音节分隔符（`'` / 反引号）与数字选词（硬编码的 `VK_1..=VK_9` 臂）都不走
    /// `session_actions`，上面两问一个都查不到，而它们的消费点全排在本闸门之后。
    /// 不显式问一句，用户照文档的邀请往 `buffer_symbol_chars` 里加 `'` 就会让
    /// `xi'an` 彻底没有候选，加数字就会废掉 1-9 选词，且都**无声无息**。
    ///
    /// 分隔符那条尤其阴：`manual_separator_key` 的 `auto` 档只在「`'` 未被占作选词键
    /// **且**未被 `[key_actions]` 绑定」时才挑中 `'`——那恰好是上面两问的补集，于是
    /// 「它当上了分隔符」与「本闸门判它空闲」是同一个不等式，必撞。
    ///
    /// # 唯一的例外：没翻过页的「上一页」，且**此刻有候选**
    ///
    /// 有候选时那一格确是空转——`page_prev` 在 `current_page == 0` 时返回 false，而
    /// [`Self::apply_session_action`] 末尾无条件 `Consumed`，按下什么都不发生。
    ///
    /// 判据取 `state.paged`（这批候选**翻过页没有**）而不是 `current_page == 0`：
    /// 从第 2 页翻回第 1 页的用户正在翻页，此刻 `-` 突然变回字符会让人当场打错字。
    /// 候选一重装 `paged` 即清零（[`Self::reset_candidate_view`]），所以「打一串码、
    /// 翻过页、再接着打字母」之后的那个 `-` 仍然是字符。
    ///
    /// ★ **「有候选」这个前提不能省**，它是本函数最后一问。无候选时导航类动作被
    /// `SessionAction::requires_candidates` 挡在 `apply_session_action` 门外，**键根本
    /// 不会被吞**，而是继续往下走三条各有行为的通路：`try_z_fallback`（`z_key_action`
    /// 配成临英/mix 时 `z-` 要夺取残余码）、标点臂按 `punct_on_empty_behavior` 甩掉废码
    /// （出厂 `clear`，那是用户打错码时的退出口）、以及标点臂里的全角与自定义标点映射。
    /// 把这一格也夺过来等于一次废掉三条既有行为，而它们都不是「按下没反应」。
    ///
    /// 例外的例外：缓冲里**已经有**本闸门放进来的符号时照收不误。那一串早已不是本方案
    /// 的编码（候选因此恒空，见 [`Self::buffer_has_literal_symbol`]），上面三条通路对它
    /// 要么无从谈起，要么反而有害——标点臂会把用户打了一半的 `e-mail-addr` 整串丢掉。
    ///
    /// # 「下一页」刻意不给这个待遇
    ///
    /// 末页按 `=` **不是**空转：它带 [`Self::try_relax_scope_on_page_end`]（翻到底了还想
    /// 看更多 ⇒ 临时放宽检索范围）。把它判成空转会把那个功能整个砍掉，而 `=` 也不是
    /// 英文标识符字符，换不来什么。
    ///
    /// # shift
    ///
    /// 第一问**带** shift（`SessionKey::matches` 要求精确相等），所以 `;` 作为无 shift 的
    /// 次选键挡不住 `:`；真正保守让位的是 `bound_action_for`，它没有 shift 维度，绑定键的
    /// shift 形态会跟着一起让。这不构成缺口：选词键与以词定字键的两个消费点自带
    /// `!shift` 守卫，shift 形态本就不归它们。
    fn symbol_buffer_key_free(&self, state: &State, key_code: u32, shift: bool) -> bool {
        match self.session_action_for(key_code, shift, true) {
            Some(wind_config::SessionAction::PagePrev(_)) if !state.paged => {}
            Some(_) => return false,
            None => {}
        }
        // 显式 `none` 是「这个键让位」，不是「这个键归我」——它与未配置同义。
        if !matches!(
            self.bound_action_for(key_code),
            None | Some(wind_config::BoundAction::None)
        ) {
            return false;
        }
        // ⚠️ 删掉这一问**不会有用例变红**——`char_is_literal_symbol` 那侧也排除了分隔符，
        // 候选照样出得来。两处不是重复防御，是同一条规则的两面：这里管「`'` 走不走它自己
        // 那条分隔符通路」（那条臂在本闸门之后，抢了就再也到不了），那里管「已经在缓冲里
        // 的 `'` 算不算本闸门放进来的」。只留那一侧的话，`'` 会被当字面符号塞进缓冲，
        // 缓冲内容碰巧一样、候选碰巧也一样，但它再不是分隔符了——引擎侧的硬边界语义
        // （双拼的配对边界、`consumed_length` 的计入）会无声消失。
        if self.manual_separator_key(key_code) || self.english_phrase_separator_key(key_code) {
            return false;
        }
        if (keymap::VK_0..=keymap::VK_9).contains(&key_code) {
            return false;
        }
        // 最后一问：这个键此刻**真的**空转吗（见上方 ★）。
        self.buffer_has_literal_symbol(state) || !state.candidates.is_empty()
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
    ///
    /// ⚠️ **新增引导键类动词时必须同时往下面的 `owners` 链里加一臂**。这张清单是手写的，
    /// 漏加不会有任何报错：撞车检测对那个新动词恒不报，方案作者把它的引导键配成码元时
    /// 一声不吭，直到现场发现「那个模式再也进不去了」。失效的是**保护性功能**，
    /// 没有人会来报这个 bug ——「本该警告却没警告」不构成用户可见的症状。
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
                    wind_config::SessionAction::AuxCode(wind_config::AuxCodeShare::Solo) => {
                        "辅助码键"
                    }
                    // 共键两个身份都要点名：只写「辅助码键」会让用户以为把辅助码关掉就不冲突了。
                    wind_config::SessionAction::AuxCode(wind_config::AuxCodeShare::PageNext) => {
                        "辅助码/翻页键"
                    }
                    wind_config::SessionAction::CommitHighlighted => "上屏键",
                    wind_config::SessionAction::Command(_) => "命令键",
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
                if self.is_reverse_trigger(vk) {
                    owners.push("反查模式触发键");
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
        // 联想态：整个收掉（候选 + 编码栏标识 + 自动隐藏计时），不顶屏，只出该字符。
        // `has_comp` 由调用方按「候选非空」算过，联想态下为真，下面照常收窗。
        self.exit_assoc(state, crate::handle_assoc::AssocExit::TopCommitKey);
        // 顶屏的高亮候选造词 / 推 6b（见 `learn_on_top_commit`），须在 `take_committed` 清段前。
        // 联想态不顶屏，判据同下方（收在 `highlight_for_top_commit`）。
        if let Some((_, cand)) = self.highlight_for_top_commit(state) {
            let buf = state.input_buffer.clone();
            self.learn_on_top_commit(state, &cand, &buf, None);
        }
        let committed = self.take_committed(state);
        let mut out = self.maybe_convert(state, &committed);
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
            // 记账码：码表按输入码（码位独立），拼音/英文按候选码；通配组码记全码。
            // 见 `main_freq_code`。
            let freq_code = self.main_freq_code(&state.input_buffer, &cand);
            self.record_selection_cand(&freq_code, &cand);
            out.push_str(&self.cand_convert_text(state, &cand));
        }
        state.input_buffer.clear();
        state.candidates.clear();
        if has_comp {
            self.notify_ui_hide();
        }
        // 英文补空格（`schema.english.commit_space`）**刻意不接这里**：本函数的用途是
        // 「顶掉高亮候选 + 紧接着上屏这个字符」，补了会得到 `hello ,` 这种断开的标点。
        // 不是漏接。
        // `numpad_raw_output`：小键盘恒半角时不转全角（普通模式 direct 档的落点，
        // 也是这个开关最常被触发的一条路——空缓冲下按小键盘数字就走这里）。
        out.push_str(&if state.full_width && !self.numpad_raw_output(state) {
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

    /// 切换定位调试浮窗。关闭时立即让 UI 撤掉窗口——否则最后一帧会留在屏幕上。
    pub(crate) fn toggle_caret_overlay(&self) {
        let on = !self
            .caret_overlay_enabled
            .load(std::sync::atomic::Ordering::Relaxed);
        self.caret_overlay_enabled
            .store(on, std::sync::atomic::Ordering::Relaxed);
        if !on {
            *self
                .caret_overlay_frame
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            let _ = self.ui_tx.send(wind_ui_types::UiCommand::HideCaretOverlay);
        }
    }

    /// 记录本帧组合矩形判据走了哪条分支（浮窗关闭时是无操作）。
    pub(crate) fn note_overlay_verdict(&self, v: wind_ui_types::diag::RectVerdict) {
        if !self
            .caret_overlay_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let mut f = self
            .caret_overlay_frame
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        f.get_or_insert_with(Default::default).rect_verdict = v;
    }

    /// 暂存本帧宿主上报的几何。锚点此时还不知道——它由 `notify_ui_update` 算出，
    /// 故在那里补齐后才推给浮窗。
    pub(crate) fn stash_overlay_frame(&self, data: &wind_bridge::handler::CaretData) {
        if !self
            .caret_overlay_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let mut f = self
            .caret_overlay_frame
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let view = f.get_or_insert_with(Default::default);
        view.caret = (data.x, data.y, data.height);
        view.comp_start = if data.composition_start_x != 0 || data.composition_start_y != 0 {
            Some((data.composition_start_x, data.composition_start_y))
        } else {
            None
        };
        view.comp_rect = data.composition_rect;
        view.caret_source = wind_ipc::protocol::caret_source::name(data.source);
        if data.composition_rect.is_none() {
            view.rect_verdict = wind_ui_types::diag::RectVerdict::Absent;
        }
        view.process = self.active_process_name();
    }

    /// 候选窗下发时补上「实际用了哪个锚点」并推给浮窗——这一项才让整幅图能回答
    /// 「判据算出来的位置对不对」。
    pub(crate) fn push_overlay_frame(&self, anchor: (i32, i32), source: &'static str) {
        if !self
            .caret_overlay_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let view = {
            let mut f = self
                .caret_overlay_frame
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let v = f.get_or_insert_with(Default::default);
            v.anchor = anchor;
            v.anchor_source = source;
            v.clone()
        };
        let _ = self
            .ui_tx
            .send(wind_ui_types::UiCommand::ShowCaretOverlay(Box::new(view)));
    }

    pub(crate) fn reset_pinyin_composition(&self, state: &mut State) {
        state.committed_text.clear();
        state.committed_segs.clear();
        state.input_buffer.clear();
        state.input_buffer_cased.clear();
        state.english_case_variant = crate::english_candidates::CaseVariant::default();
        state.input_cursor_pos = 0;
        state.preedit.clear();
        state.preedit_split_body.clear();
        state.preedit_fp_body.clear();
        state.preedit_abbrev_body.clear();
        state.direct_aux_body.clear();
        state.direct_aux_prev = None;
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
    /// 「其它输入」= 宿主自行出字 / 重放 / 用户挪光标（`note_host_wrote_text`），其中宿主自行出字
    /// 要等**下一个**送达的 keydown 带透传位才知道——工具栏按钮触发撤销没有 keydown，那一下仍按
    /// 旧计数删（冷门入口，已知限制）；
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
        // 删掉的正是下一词学习的上文
        self.note_word_boundary();
        let encoded = wind_ipc::codec::encode_replace_backward(count, "");
        let _ = self.push_server.push_commit_to_active(&encoded);
    }

    /// 命令栏的数据类开关（`ime.toggle` / `ime.set` 直通，不进菜单与工具栏）：
    /// - `emoji`：emoji 候选总开关（`input.emoji.enabled`）；
    /// - `emoji:<模块id>`：附加模块（`input.emoji.modules.<id>`，含用户放进 `emoji/` 的文件）；
    /// - `dict:<注释库id>`：注释词库（`[[ui.comment_dicts]]` 的 `enabled`，中英释义、拆分等）。
    ///
    /// `want` = 目标状态，`None` = 取反（toggle）。已是目标状态时不写盘、不重载——`ime.set`
    /// 的语义就是「确保是这个状态」——但**照样提示**「…已是开启状态」：一点反应都没有的话，
    /// 用户分不清是状态本就如此还是命令没被调用（维护者实测反馈）。改了则写用户配置后热重载
    /// （emoji 表 / 注释库按新开关重挂），提示「…已开启 / 已关闭」：命令栏与短语没有别的
    /// 反馈渠道，开了没反应和关掉了在界面上无从分辨。
    pub(crate) fn cmd_set_feature(&self, target: &str, want: Option<bool>) -> anyhow::Result<()> {
        let (name, on) = if target == "emoji" {
            let cur = self.rt().config.input.emoji.enabled;
            let on = want.unwrap_or(!cur);
            let name = "emoji 候选".to_string();
            if on == cur {
                return self.notify_feature_unchanged(target, &name, on);
            }
            Config::set_user_value(&["input", "emoji", "enabled"], toml::Value::Boolean(on))?;
            (name, on)
        } else if let Some(id) = target.strip_prefix("emoji:") {
            let module = Config::emoji_modules(Config::data_dir().as_deref())
                .into_iter()
                .find(|m| m.id == id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "没有 id 为 {id:?} 的 emoji 模块（见设置 → Emoji/OpenCC → 附加模块）"
                    )
                })?;
            let cur = self.rt().config.input.emoji.module_enabled(id);
            let on = want.unwrap_or(!cur);
            let name = format!("emoji 模块「{}」", module.label);
            if on == cur {
                return self.notify_feature_unchanged(target, &name, on);
            }
            Config::set_user_value(&["input", "emoji", "modules", id], toml::Value::Boolean(on))?;
            (name, on)
        } else if let Some(id) = target.strip_prefix("dict:") {
            let configured = self.rt().config.ui.comment_dicts.clone();
            let auto: Vec<String> = self
                .auto_comment_sources()
                .into_iter()
                .map(|a| a.id)
                .collect();
            let builtin: Vec<String> = self
                .system_comments
                .dicts
                .iter()
                .map(|b| b.id.clone())
                .collect();
            let name = format!("注释词库「{id}」");
            let Some((list, on)) =
                crate::handle_cmdbar::set_comment_dict(&configured, &auto, &builtin, id, want)?
            else {
                // 只有指定了状态才可能「已一致」（取反恒有变化），故 want 必为 Some
                return self.notify_feature_unchanged(target, &name, want.unwrap_or(true));
            };
            Config::set_user_value(&["ui", "comment_dicts"], toml::Value::try_from(&list)?)?;
            (name, on)
        } else {
            anyhow::bail!("不认识的数据开关 {target:?}");
        };
        self.reload_user_config();
        let text = format!("{name}已{}", if on { "开启" } else { "关闭" });
        if let Err(e) = self.ui_toast(&text, "info", "", "center", 1500) {
            warn!("数据开关的提示弹不出来: {e}");
        }
        info!("数据开关 {target}: {text}");
        Ok(())
    }

    /// `ime.set` 遇到状态本就一致：不写盘不重载，只提示「…已是开启 / 关闭状态」。
    fn notify_feature_unchanged(&self, target: &str, name: &str, on: bool) -> anyhow::Result<()> {
        let text = format!("{name}已是{}状态", if on { "开启" } else { "关闭" });
        self.notify_set_unchanged(target, &text);
        Ok(())
    }

    fn notify_set_unchanged(&self, target: &str, text: &str) {
        if let Err(e) = self.ui_toast(text, "info", "", "center", 1500) {
            warn!("ime.set 的提示弹不出来: {e}");
        }
        info!("ime.set {target}: {text}（未改动）");
    }

    /// `ime.set` 的输入状态开关（`cn-en` / `fullshape` / `s2t` / `t2s` / `toolbar`）：已是目标
    /// 状态只提示（同数据开关）。`cn-en` 是**直达**路径（置到目标态，CapsLock 取消后也按
    /// 目标落地）；其余走 `ime.toggle` 同一条切换路径——那条路径自带状态气泡与工具栏刷新，
    /// 这里不另弹提示，免得一次切换两个浮窗。
    ///
    /// `cn-en` 读的是 `chinese_mode`（用户选的中英），不含 CapsLock 临时英文：`set` 回答的是
    /// 「切到中文」，CapsLock 松开后自然回到中文。
    pub(crate) fn cmd_set_state(&self, target: &str, on: bool) -> anyhow::Result<()> {
        self.ime_state_set(target, on, true).map(|_| ())
    }

    /// 输入状态开关的当前值（`cn-en` 为 true = 中文）。目标只认 cn-en / fullshape / s2t / t2s / toolbar。
    pub fn ime_state_get(&self, target: &str) -> anyhow::Result<bool> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match target {
            "cn-en" => Ok(s.chinese_mode),
            "fullshape" => Ok(s.full_width),
            "s2t" => Ok(s.s2t_enabled),
            "t2s" => Ok(s.t2s_enabled),
            "toolbar" => Ok(s.toolbar_visible),
            _ => anyhow::bail!(
                "ime.set 不支持 {target:?}：输入状态只认 cn-en / fullshape / s2t / t2s / toolbar（layout / candwin / preedit 只能 ime.toggle）"
            ),
        }
    }

    /// 把输入状态开关设为 `on`；返回是否真的改动。
    ///
    /// 已是目标状态时不动作，`notify_unchanged` 决定要不要弹「已是…」——命令栏要（短语没有
    /// 别的反馈渠道），CLI 脚本不要（反复调用会天天弹）。改动时走 `ime.toggle` 同一条路径，
    /// 自带状态气泡与工具栏刷新，不另弹提示。
    pub fn ime_state_set(
        &self,
        target: &str,
        on: bool,
        notify_unchanged: bool,
    ) -> anyhow::Result<bool> {
        if self.ime_state_get(target)? == on {
            if notify_unchanged {
                let text = crate::handle_cmdbar::state_unchanged_text(target, on);
                self.notify_set_unchanged(target, &text);
            }
            return Ok(false);
        }
        if target == "cn-en" {
            // 中英开关走**直达**路径，不吃 ime.toggle 的翻转语义：ime.set 的契约是
            // 「置为目标态」，已在目标态根本到不了这里（上面已 return）。
            // CapsLock 开着时取消锁定并直达目标（Directed，不吃 mode_after_cancel 归位）；
            // 待编码经 push 出口交还宿主，避免丢字（见 [`SwitchCommit`]）。
            let (_, commit) = self.cn_en_set_with_commit(on);
            self.push_switch_commit(&commit);
            return Ok(true);
        }
        self.cmd_ime_toggle(target);
        Ok(true)
    }

    /// 取反输入状态开关，返回切换后的值（RPC `ime.toggle`）。目标须是上述五个之一。
    pub fn ime_state_toggle(&self, target: &str) -> anyhow::Result<bool> {
        let cur = self.ime_state_get(target)?;
        self.cmd_ime_toggle(target);
        Ok(!cur)
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
            // 两个转换方向各一个 target。互斥由 `toggle_conversion_direction` 统一保证，
            // 这里只管把动词转过去。
            "s2t" => {
                self.handle_menu_command("toggle_s2t");
                self.notify_toolbar();
            }
            "t2s" => {
                self.handle_menu_command("toggle_t2s");
                self.notify_toolbar();
            }
            "toolbar" => self.toggle_toolbar(),
            "preedit" => self.cmd_toggle_preedit(),
            "candwin" => self.cmd_toggle_candwin(),
            "layout" => self.cmd_toggle_layout(),
            // 命令栏入口走 `CoordIme::toggle` 直接返回错误；这里是菜单等无处报错的路径。
            "comment" => {
                if let Err(e) = self.cmd_comment_state(None) {
                    warn!("ime.toggle comment: {e}");
                }
            }
            other => {
                warn!(
                    "ime.toggle: 暂不支持 target {:?}（Rust 平台能力待补）",
                    other
                )
            }
        }
    }

    /// 「编码这一刻是不是嵌在宿主组合区里」——`preedit_display` 的**有效**取值，全部消费点共用。
    ///
    /// 配置说 `candidate_top` / `candidate_inline` 时编码归候选窗画；但候选窗被宿主压住
    /// （TSF UI-less 的全屏游戏 / D3D 独占全屏，见 [`Self::ui_suppressed_by_host`]）时它
    /// **根本不画**，此时一律回到嵌入，否则编码两条出口全断、游戏里一个字都看不见。
    /// 完整论证见 `preedit_uses_placeholder` 与 `docs/design/game-compat-tsf-uielement.md` §4.4 末。
    ///
    /// ⛔ **别再在别处写 `preedit_display.lock().map(|m| m.in_app())`**。这个访问器是从四个
    /// 各判各的读点收上来的，其中「混输高亮跟随」那处（`handle_candidate_nav` 里）漏掉压制态
    /// 的后果是可见的：↑↓ 改了编码形态却不回传组合串，游戏聊天框里的编码停在旧形态。
    /// 配置的**写入**点（`apply_ui_config` / [`Self::cmd_toggle_preedit`]）不在此列——它们存的是
    /// 用户选的值，不是这一刻的有效值。
    ///
    /// 锁毒化时回 `true`（嵌入）：与收上来的三处原有兜底逐字一致，也与
    /// `preedit_uses_placeholder` 的 `unwrap_or(false)` 互为反面。
    ///
    /// ⚠️ **加锁顺序**：本函数会在**持有 `state`** 的路径里被调用（混输高亮跟随、
    /// `maybe_enter_assoc`），而它自己要取 `uielement_*_pids` / `active_compat` / `app_compat`。
    /// 全局持有序是 `state → uielement_*_pids → active_compat → app_compat`，它取的这几把
    /// **必须永远排在 `state` 之后**；谁哪天写出一条「持 `active_compat` 再锁 `state`」就成环。
    /// 本仓在同类问题上翻过一次车，见 `apply_compat_for_pid` 上方那条「两把锁在此嵌套会引入
    /// 一个方向相反的持有序」。本函数**没有引入新的锁边**：那两个调用点本来紧接着就要调
    /// `notify_ui_update(state)`，而那里面的压制态守卫早就在做同样的 `state → uielement_*_pids`。
    pub(crate) fn preedit_in_app_effective(&self) -> bool {
        if self.ui_suppressed_by_host().is_some() {
            return true;
        }
        // 反方向的同一个道理：宿主**没有**应用内预编辑（Linux 下不声明 Preedit 能力位的客户端，
        // WPS / QQ 等），配置说 app_inline 也嵌不进去。addon 不能把编码转交 Fcitx5 的输入面板
        // ——那会由 classicui 另起一个小窗，正好被我们的候选窗盖住——所以编码归候选窗顶部栏
        // （`SetPreeditEmbedded` 按配置原值下发，app_inline 时为 false，即顶部栏）。
        if self
            .host_no_inline_preedit
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }
        self.preedit_display
            .lock()
            .map(|m| m.in_app())
            .unwrap_or(true)
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

    /// 用户是否隐藏了候选窗（candwin 切换 / `ui.candidate.hide_window`）。
    pub(crate) fn candidate_window_hidden(&self) -> bool {
        *self
            .hide_candidate_window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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
            // 看不见的网格里 ←→ 还会移高亮而不移编码光标：随候选窗一起收起（§5.5）。
            self.collapse_grid(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()));
            self.clear_hover();
            self.emit_hide();
        }
        self.show_tip(if hidden {
            "候选窗:隐藏"
        } else {
            "候选窗:显示"
        });
    }

    /// 当前悬停目标（测试/诊断用）：鼠标悬停与 `toggle_tip` 共用，`-1` = 无。
    pub fn debug_hover_target(&self) -> i32 {
        self.hover_target()
    }

    /// 候选窗是否被 `ime.toggle("candwin")` 隐藏（测试/诊断用）。
    ///
    /// 住在这里而非 `debug_support.rs`：字段私有，以本模块为界。
    ///
    /// 命令类绑定的端到端测试拿它当探针：这个开关**只改一个内存位**——不写盘、不碰组合区，
    /// 于是「命令执行了」与「当前组合原样不动」可以在同一条用例里同时断言。
    pub fn debug_candidate_window_hidden(&self) -> bool {
        *self
            .hide_candidate_window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// 切换候选布局方向（横排 ↔ 竖排），下发 UI 并持久化。命令栏 ime.toggle("layout")。
    /// 切换时 composition 已清（命令选中即 ClearComposition），下次输入按新方向渲染。
    fn cmd_toggle_layout(&self) {
        // ── 方案声明了方向时：只翻本方案的手动覆盖，不动全局基线、不写盘 ──────────
        //
        // 判据是「用户此刻表达的是什么」：在一个 `[candidate] layout = "vertical"` 的方案里
        // 按切换，他要的是「这个方案里我要横排」，不是「把全局默认改成横排」。后者的效果
        // 要等切到别的方案才显形，用户完全无法把它和刚才那次按键联系起来。
        //
        // 手动值随 `schema_scope_gen` 失效：切走再切回，回到方案意图（见 crate::schema_scope）。
        if self.schema_layout_intent() != wind_config::LayoutIntent::Follow {
            let vertical = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                // 先收敛代际，否则刚切完方案就按切换会基于上个方案的手动值取反。
                self.sync_schema_scope(&mut state);
                // 翻转的基准是**当前实际生效值**，不是基线——基线在方案意图之下，
                // 对它取反在强制竖排的方案里一次都不会改变用户看到的方向。
                let want = !self.desired_orientation(&state).vertical;
                state.layout_manual = Some(want);
                self.sync_candidate_layout(&state);
                want
            };
            self.show_tip(if vertical {
                "候选:竖排"
            } else {
                "候选:横排"
            });
            return;
        }
        // 切换只翻**竖排位**这一根轴，不碰文字排列：后者由 `desired_orientation` 每次从当前
        // 数据方案重新取。故在蒙古文方案里切到竖排（旋转按 `normalized()` 归零——竖排再转
        // 90° 就是横排，没有定义）、再切回横排时旋转原样回来，方案意图不被这个二值开关吃掉。
        let vertical = {
            let mut v = self
                .candidate_orientation
                .lock()
                .unwrap_or_else(|x| x.into_inner());
            let next = !v.vertical;
            *v = if next {
                Orientation::VERTICAL
            } else {
                Orientation::HORIZONTAL
            };
            next
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
    /// ③ 否则回退默认 (i+1)，第 10 个写 `0`，与选它的物理键对应。
    fn resolve_index_label(
        &self,
        cand_cfg: &wind_config::config::UiCandidateConfig,
        i: usize,
    ) -> String {
        pick_index_label(
            cand_cfg,
            &self
                .theme_index_labels
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            i,
        )
    }

    /// 展开态带序号的格子（平铺下标区间 `(起点, 个数)`）：从高亮行行首起，按
    /// `grid_index_fill` 往下顺延，与数字键的选择范围 `select_range` 同源。
    /// 桌面 `UpdateCandidates` 与中立通道的帧共用。
    pub(crate) fn grid_index_of(
        &self,
        state: &State,
        grid_range: Option<(usize, usize)>,
    ) -> Option<(usize, usize)> {
        grid_range.map(|(vis_start, _)| {
            let (sel_start, sel_end) = self.select_range(state);
            (sel_start.saturating_sub(vis_start), sel_end - sel_start)
        })
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
            self.emit_hide();
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
            self.emit_hide();
            self.reset_first_show();
            return;
        }
        // 宿主侧压住：宿主接管了候选绘制（TSF UI-less），或前台是 D3D 独占全屏。
        // 候选状态照常演进（宿主画的正是这份状态、盲打照样上屏），只是本地不弹窗。
        if let Some(reason) = self.ui_suppressed_by_host() {
            debug!("候选窗被压住: {reason}");
            self.clear_hover();
            self.emit_hide();
            self.reset_first_show();
            return;
        }
        // 中立通道（移动端）：到这里三道隐藏判断都过了，只发一帧快照。
        // 下面的候选窗布局 / 字体、首显闸门、注释与悬停提示渲染、锚点与 overlay 帧都是桌面
        // 候选窗专属，移动端没有对应物（见 session_output 模块文档）。
        if self.session_tx.is_some() {
            // 提示卡（长按取 `candidate_tooltip`）要用的两样懒建结构——反查索引、按词查编码的
            // 用户层——都在首次使用时后台构建，建好后各补刷一帧。若等到长按才触发，补刷帧会
            // 把刚弹出的提示卡收掉（宿主在任意候选帧上收卡），用户的下一次点击落到键盘上。
            // 所以在首个候选帧就触发，让补刷帧落在打字这段里。桌面路径每帧组装候选页时
            // 经 `tooltip_eval_ctx` 触发反查索引；这里不组装候选页，结果丢弃，只要副作用。
            let ctx = self.tooltip_eval_ctx(state, &self.rt().tooltip);
            if let Some(sid) = ctx.code_schema.as_deref() {
                let _ = self.engine_mgr.text_codes(sid);
            }
            self.emit_view(state, mode_label);
            return;
        }
        // 模式级候选布局：按当前模式意图叠加全局基线重算方向，与上次下发不同才下发。
        // 必须在下方 UpdateCandidates **之前**——同 channel 按序处理，UI 先改方向再填候选。
        // 这是「强制竖排/横排」的唯一执行点，模式进入/退出各处都不再自己动布局（见 layout.rs）。
        self.sync_candidate_layout(state);
        // 方案级候选字体：归属是**数据方案**，随临英/快符叠加逐次按键变化，故与布局同处重算。
        self.sync_candidate_font(state);
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
        //   ④ 组合前的空闲上报可作锚点：见 `caret_cache_is_fresh_idle_report`。补的是 ③ 在
        //      连打时的死角——③ 的组合起点每次上屏被清、重锁又要等那个等不到的权威坐标，
        //      于是 fast 档连打时两个逃生口全部失效，只能走等待。
        let skip_caret_pending =
            self.effective_first_show_mode() == wind_config::app_compat::FirstShowMode::Instant;
        // ⚠ 焦点后的第一个组合不吃这条：那时的空闲上报最不可信（EverEdit 实测错 1200px）。
        // 它只被挡在「立即显示」之外，仍走 25ms 短兜底——足够 probe 把缓存刷成正确位置。
        // ⚠ 只给 probe 不可信的宿主用：probe 可信时等那 25ms 让它说话更准——它是宿主在本次
        // 组合里现测的，而空闲上报是按键**之前**的。EverEdit 实测空闲上报滞后一拍（点击移
        // 光标后既不发 caret_update 也不发 selection_changed），拿它抢跑 ⇒ 候选窗恒慢一拍，
        // 而 probe 1ms 后就给出了正确位置。见 `probe_is_untrusted`。
        let idle_anchor = self.probe_is_untrusted()
            && self.caret_cache_is_fresh_idle_report()
            && !self
                .awaiting_first_authority_after_focus
                .load(std::sync::atomic::Ordering::Relaxed);
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
        // 上屏后开的新组合（`first_show_after_commit`）：插入点必然已前移，instant 的遗留坐标、
        // idle_anchor 的「按键前」坐标都是上屏前的陈旧值，逃生口一律不放行，老老实实等
        // 权威坐标（兜底超时到期时消费该标记，见 `fire_pending_first_show`）。
        let after_commit = self
            .first_show_after_commit
            .load(std::sync::atomic::Ordering::Relaxed);
        if is_first_frame
            && !caret_free
            && (after_commit || (!skip_caret_pending && !coords_ready && !idle_anchor))
        {
            // 唯一的「等」出口。与下面的放行日志成对，两条合起来即可从服务端日志判定
            // 每一帧走了哪条路、以及是哪个逃生口生效——不必再对着 TSF 日志比时间戳。
            debug!(
                "first_show 闸门 → 等待权威坐标（arm {}ms 兜底）: skip_caret_pending=0 coords_ready=0 idle_anchor=0",
                self.planned_first_show_timeout_ms()
            );
            self.arm_pending_first_show();
            return;
        }
        if is_first_frame {
            // instant 档用的是上一轮遗留的坐标，必然是「非权威」；coords_ready 那条是已锁定
            // 的本轮组合起点，属权威，不置位。
            //
            // idle_anchor 跟 instant 同待遇而非跟 coords_ready：它是**按键前**的光标位置，
            // 组合一开始首字母就落进编辑区，真实光标随即前移一个字符宽（记事本实测 15px）。
            // 不置位则 tol 只有 3px，那一次权威坐标必然触发 reshow——首显是准的，却要当着
            // 用户的面跳一格。置位后走 settle 放宽容差（0.8×行高 ≈ 44px）吸收掉。
            if skip_caret_pending || idle_anchor {
                self.first_show_was_provisional
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            // ★ 空闲上报坐标**就是**组合起点——它是按键前的光标位置。抢在宿主之前锁上。
            //
            // 部分宿主根本没实现组合起点：微信（Qt WebView）报的 compStart 恒等于当前光标 x
            // （实测逐帧 `compStart=(2241,783)` 与 `x=2241` 相等），于是「组合起点」随每个字母
            // 右移一格。首显用的是按键前坐标（对的），reshow 却改用宿主那份偏右一格的 compStart
            // ⇒ 打第二个字母时候选窗自己挪 12px。「同一组合只锁首个 compStart」那条守卫防的是
            // 起点持续漂移，但它锁到的已经是**第一个字母落下之后**的位置，差的就是这一格。
            //
            // ⚠ 只在 idle_anchor 这条路上锁：这份坐标是宿主对当前插入点的直接测量，够格当起点。
            // 其余路径（兜底用的旧缓存、instant 用的上一轮遗留坐标）本身就可能是错的，锁进
            // 组合起点会让 reshow 也救不回来——组合起点一旦锁定，本组合内不再更新。
            debug!(
                "first_show 闸门 → 立即显示（逃生口）: instant={} coords_ready={} idle_anchor={}",
                skip_caret_pending as u8, coords_ready as u8, idle_anchor as u8
            );
        }
        let t_nu = std::time::Instant::now();
        // 仅推送当前页候选（窗口按 1..N 编号，翻页后重新编号）。
        // 展开态推送可见区内的几页、逐行平铺（一行一页），见 `candidate_grid`。
        let grid_range = self.grid_visible_range(state);
        let (start, end) = grid_range.unwrap_or_else(|| self.page_range(state));
        let per_page = self.page_size(state);
        // 展开态一帧有好几页候选，悬停气泡**只为鼠标下的那一格**渲染——每格各渲染一份会把
        // 持锁热路径上的开销放大成行数倍，而 UI 本来就只显示悬停那一格的气泡（悬停一变，
        // `mouse_hover` 就会重新下发）。
        let hover_now = self.hover_target();
        // 展开态带序号的格子（平铺下标区间）：从高亮行行首起，按 `grid_index_fill` 往下顺延，
        // 与数字键的选择范围 `select_range` 同源。
        let grid_index = self.grid_index_of(state, grid_range);
        // 候选序号标签：
        //  - 数字透镜：数字键正在录表达式 → 选词改用字母标签 a/b/c（实例关了字母选词除外）
        //  - 其余（含自由输入）：正常序号
        //
        // ★ 自由输入（Free 透镜）下字母与数字**都是**字面输入，没有键能按序号选——曾经因此
        // 干脆不画序号。维护者 2026-10-07 改定为照常显示：与其它状态的候选窗外观一致，
        // 尤其快捷输入历史在这里置首、候选一多时，序号是用户辨认「第几条」的唯一参照。
        // 选词靠空格（高亮）、方向键与 `keys.quick_input_select`（出厂 Ctrl+序号）。
        let mix_lens = matches!(state.active, Some(ModeKind::Mix(_))).then(|| self.mix_lens(state));
        // 关掉 `numeric_letter_select` 的实例字母是字面输入，标签回到数字序号。
        let alpha =
            mix_lens == Some(MixLens::Numeric) && self.mix_numeric_letter_select(state.mix_id);
        // 悬停提示/候选微调配置（热重载快照）
        let rt = self.rt();
        let cand_cfg = &rt.config.ui.candidate;
        let tip = &rt.tooltip;
        // 命令直通车候选前缀标注（features.cmdbar.candidate_prefix）：仅命令候选(is_command)显示。
        let cmd_prefix = rt.config.input.cmdbar.candidate_prefix.as_str();
        // 检索范围放宽（自动补充）候选的前缀标注，见 docs/design/smart-filter-scope-relax.md
        let scope_prefix = rt.config.input.scope_relax.prefix.as_str();
        // 换位纠错候选的标记（`input.correction.marker`，留空不加）。
        let corrected_prefix = rt.config.input.correction.marker.as_str();
        // 编码提示(反查):拼音来源候选的 `${code_rev}` / `${shuangpin}` 只看候选来源,显不显示
        // 由分到的注释模板决定(设计 §13.6)。码表类候选的剩余编码由码表引擎在 convert 内填。
        // 悬停提示与注释段共用的整页上下文（编码来源方案、调试归属、注释库作用域…），
        // 见 `TooltipEvalCtx`；移动端按下标取提示走同一份。
        let tip_ctx = self.tooltip_eval_ctx(state, tip);
        // 反查表读锁在候选循环外取一次（写方只有拆字段的装卸：sync_chaizi_assets /
        // ensure_chaizi_async）。
        let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
        // 注释段（候选右侧灰字）模板，见 `crate::comment`。横竖各持一份、互不影响：
        // 两种排布的可用横向空间差一个数量级，能放什么本就不是同一个答案。
        // 按 模式 → 方案 → 方案类型 → 全局 取当前态那一格，见 `comment_template_for`。
        // ⚠️ 注释模板按 `vertical` 位取，旋转态因此走**横排**那一份模板。
        // 这是「旋转态的 vertical 恒为 false ⇒ 所有按方向分叉的配置走横排支」这条总规则的
        // 一个实例，不是遗漏。
        let comment_vertical = self.desired_orientation(state).vertical;
        // 解析一次、循环里逐候选只渲染（模板串对整页相同）。
        let comment_tpl = crate::comment::Template::parse(&self.comment_template_for(
            &rt.config,
            state,
            comment_vertical,
        ));
        // 注释段长度预算横竖各一份：横排全部候选共享一行宽度，竖排每行独占。
        let comment_max = cand_cfg.comment_max_chars(comment_vertical);
        // 本页每个候选的气泡原文，右键菜单按段 / 按行复制、上屏时从这里取（见 handle_tooltip）。
        let mut tip_page: Vec<crate::handle_tooltip::TooltipPageEntry> =
            Vec::with_capacity(end.saturating_sub(start));
        // 命令候选加前缀标注（截断后再加,保证前缀不被截掉）。
        // 检索范围放宽补进来的候选同理加标注（`input.scope_relax.prefix`），让用户
        // 一眼看出「这几条是超出当前检索范围补来的」，而非词库里本该有的常用字。
        let prefix_of = |c: &Candidate| {
            if c.is_command && !cmd_prefix.is_empty() {
                cmd_prefix
            } else if c.is_scope_filtered {
                scope_prefix
            } else if c.is_corrected {
                corrected_prefix
            } else {
                ""
            }
        };
        // 显示截断（超长加 …）：短语与普通候选统一按用户可配的 ui.candidate.max_chars。
        // 短语 text 在生成层已存完整原文（仅一行化），此处仅裁显示——上屏仍用完整原文。
        // 展开态按列对齐，一个长词会撑宽整列，另有更紧的格宽上限。
        let truncate = |full: &str| {
            if grid_range.is_some() {
                cand_cfg.truncate_grid_cell(full)
            } else {
                cand_cfg.truncate_display(full)
            }
        };
        // 正确拼音提示（「苹(ping)果」）：按来源过滤、换算到显示文字、过了截断与前缀后的带提示正文。
        let hint_cfg = &rt.config.input.correction.hint;
        let tone_reverse = hint_cfg.tone.then_some(&*reverse);
        let hint_styled = |c: &Candidate, full: &str, disp: &str| {
            self.cand_display_hints(state, c, full, hint_cfg, tone_reverse)
                .map(|h| crate::hint_text::styled_for_shown(full, disp, prefix_of(c), &h))
                .unwrap_or_default()
        };
        // 哪些候选标（`first` / `all`）见 `pinyin_hint_texts`。
        let mut hint_texts =
            self.pinyin_hint_texts(state, &rt.config.input.correction.hint, start, end, |c| {
                let full = self.cand_display_text(state, c);
                hint_styled(c, &full, &truncate(&full))
            });
        let items: Vec<CandidateItem> = state.candidates[start..end]
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let full = self.cand_display_text(state, c);
                let disp = truncate(&full);
                let tip_skipped = grid_range.is_some() && i as i32 != hover_now;
                let rendered = if tip_skipped {
                    Default::default()
                } else {
                    self.render_candidate_tooltip(state, c, &disp, &full, tip, &tip_ctx, &reverse)
                };
                let dict_schema = self.comment_dict_scope(
                    state,
                    c,
                    tip_ctx.mix_comment_scope,
                    &tip_ctx.comment_dict_schema,
                );
                let tooltip = rendered.doc.clone();
                tip_page.push(crate::handle_tooltip::TooltipPageEntry {
                    text: c.text.clone(),
                    rendered,
                });
                // 注释段（候选右侧灰字）：渲染当前排布对应的模板。
                // 与悬停提示无耦合——注释放不下的内容不往气泡里塞，气泡有自己的
                // `ui.tooltip.sections`，塞了会与之重复。
                // 上方注释条开关开：模板字面 `\n` 拆出上段（`comment_above`）。开关关走原路径，
                // 不拆分、字面 `\n` 原样留在 `comment`，`comment_above` 恒空。
                let (comment_above, comment) = if cand_cfg.comment_above {
                    self.comment_parts_for(c, &comment_tpl, comment_max, &reverse, &dict_schema)
                } else {
                    let comment =
                        self.comment_for(c, &comment_tpl, comment_max, &reverse, &dict_schema);
                    (wind_ui_types::StyledText::new(), comment)
                };
                let prefix = prefix_of(c);
                let text_styled = std::mem::take(&mut hint_texts[i]);
                let text = if prefix.is_empty() {
                    disp
                } else {
                    format!("{prefix}{disp}")
                };
                CandidateItem {
                    text,
                    text_styled,
                    code: c.code.clone(),
                    label: if alpha {
                        ((b'a' + i as u8) as char).to_string()
                    } else if let Some((from, count)) = grid_index {
                        // 带序号的格子从高亮行行首起顺延编号；其余格子的序号渲染端画成透明，
                        // 这里照列号给一个，只为占住同样的宽度。
                        if (from..from + count).contains(&i) {
                            self.resolve_index_label(cand_cfg, i - from)
                        } else {
                            self.resolve_index_label(cand_cfg, i % per_page)
                        }
                    } else {
                        self.resolve_index_label(cand_cfg, i)
                    },
                    tooltip,
                    comment,
                    comment_above,
                    no_index: false,
                }
            })
            .collect();
        *self.tooltip_page.lock().unwrap_or_else(|e| e.into_inner()) = tip_page;
        // 翻页信息改为结构化字段传给候选窗（窗口内渲染独立的页码指示）
        let total_pages = self.total_pages(state);
        let grid = state.grid_top.zip(grid_index).map(|(top, (from, count))| {
            wind_ui_types::CandidateGrid {
                cols: per_page as u16,
                rows: self.grid_rows() as u16,
                highlight_row: state.current_page.saturating_sub(top) as u16,
                index_start: from as u16,
                index_count: count as u16,
                tabs: self.grid_tabs(state).iter().fold(0, |m, t| m | t.bit()),
                active_tab: state.grid_tab as u8,
            }
        });
        // 展开态的选中项是平铺下标：高亮行之前的整行都算进去（与中立通道共用）。
        let selected = crate::session_output::frame_selected(
            state,
            per_page,
            grid.map(|g| g.highlight_row as usize),
            items.len(),
        );
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
        // 联想态**不再强制非嵌入**：宿主侧此刻挂着占位组合（见 `assoc_composition`），
        // 归属如实按配置走即可。嵌入模式下 `maybe_enter_assoc` 干脆不给标识
        // （`state.preedit` 为空），候选窗因此没有编码栏、高度不跳。
        let in_app = self.preedit_in_app_effective();
        // 坐标基准：嵌入模式且组合起点已锁定 → 用组合起点（钉在缓冲头部，不随输入移动）；否则当前光标。
        // 组合起点由 handle_caret_update 在本组合首个有效坐标处锁定。候选窗首显已由"延迟首显"门控
        // 保证发生在 reflow 后的权威坐标处。无效坐标回退最近有效坐标，避免跑到屏幕左上角。
        // ★ 首次下发候选窗时，若首显坐标来自「组合前的空闲上报」，那它**就是**组合起点
        // ——空闲上报是按键前的光标位置，组合正是从那里开始的。抢在宿主之前锁上。
        //
        // 部分宿主根本没实现组合起点：微信（Qt WebView）报的 compStart 恒等于当前光标 x
        // （实测逐帧 `compStart=(2241,783)` 与 `x=2241` 相等），于是「组合起点」随每个字母
        // 右移一格。首显用的是按键前坐标（对的），reshow 却改用宿主那份偏右一格的 compStart
        // ⇒ 打第二个字母时候选窗自己挪 12px。「同一组合只锁首个 compStart」那条守卫防的是
        // 起点持续漂移，但它锁到的已经是**第一个字母落下之后**的位置，差的就是这一格。
        //
        // ⚠ 判据用 `!shown`（本轮尚未下发过）而**不是** `is_first_frame`：兜底 timer 到期那条
        // 路径先置了 `show_authorized`，`is_first_frame` 已为 false，绑在它上面会漏掉整条兜底
        // 首显——微信「焦点后第一个组合」走的正是这条，表现为第三个字好了、第二个字仍挪一格。
        //
        // ⚠ 只在坐标确实来自空闲上报时锁：兜底用的旧缓存、`instant` 用的上一轮遗留坐标本身
        // 就可能是错的，锁进组合起点会让 reshow 也救不回来（本组合内不再更新）——WPS 表格的
        // 「恒慢一步」正是宿主自己先报了旧单元格的 compStart 造成的，前车之鉴。
        // ⚠ 同样限 probe 不可信的宿主：probe 可信时宿主自己报的 compStart 就够用，而空闲上报
        // 可能滞后（EverEdit）——把一个陈旧坐标锁成组合起点，后续 reshow 就再也救不回来了
        // （本组合内不再更新）。EverEdit 实测：reshow 正确判出 447px 偏移，位置却纹丝不动。
        if !shown && self.probe_is_untrusted() && self.caret_cache_is_fresh_idle_report() {
            let mut cs_lock = self
                .composition_start
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !cs_lock.2 {
                *cs_lock = (state.caret_x, state.caret_y, true);
            }
        }
        let cs = *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // ★ 非首显、非坐标校正的重绘（悬停 / 翻页 / 候选变化）复用候选窗**当前**的位置基准：
        // `state.caret_x/y` 是缓存，会被 probe 的 `absorb_probe_coords`、以及被 settle 吸收掉的
        // 那一帧权威坐标悄悄改写，而候选窗并不跟着动。此时直接读缓存 ⇒ 鼠标一指候选窗，位置
        // 跳一个字符（微信实测 483→496）。`cs.2` 那条不需要本机制（组合起点本就钉死），但它在
        // 连打时每次上屏都被 `reset_first_show` 清掉，`idle_anchor` 首显时更是还没锁上。
        let anchor = *self.shown_anchor.lock().unwrap_or_else(|e| e.into_inner());
        let hold_anchor = anchor.2 && !is_first_frame && !authorized;
        // ⚠ 锚点必须**压过** `cs.2`，不能排在它后面：组合起点常常在首显**之后**才由宿主报来
        // （微信实测首显 03.980 用 (483,987)，38ms 后才锁定 (496,989)），一旦锁上就接管位置，
        // 把候选窗从首显处挪到组合起点——而上一行 `settle` 刚判定这 13px 不值得校正。排在
        // `cs.2` 后面等于让组合起点从侧面绕过 settle，悬停一次就补上那 13px。
        // 真正该动候选窗的两种情形（首显、坐标校正）都不满足 `hold_anchor`，照常走下面两条。
        // 本轮首次下发、组合起点还没锁：pre_reflow 帧报过组合起点就先用它（见字段注释，A2-42）。
        let pre_cs = *self
            .pre_reflow_comp_start
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (cx, cy, ch, anchor_source) = if hold_anchor {
            (anchor.0, anchor.1, state.caret_height, "shown_anchor")
        } else if in_app && cs.2 {
            (cs.0, cs.1, state.caret_height, "composition_start")
        } else if in_app && !shown && pre_cs.2 {
            (
                pre_cs.0,
                pre_cs.1,
                state.caret_height,
                "pre_reflow_comp_start",
            )
        } else {
            (state.caret_x, state.caret_y, state.caret_height, "caret")
        };
        let (caret_x, caret_y, caret_height, caret_valid) = self.resolve_caret_for_ui(cx, cy, ch);
        let n_items = items.len();
        // 编码区**恒下发**：谁来画由 `preedit_host_owned` 表达。
        // 曾经是「in_app 就发空串」——那等于把渲染策略焊进数据通道，自绘编码栏的宿主
        // 拿不到数据（Android 侧一度只能靠改显示模式配置绕开）。
        let (preedit, preedit_caret) = self.display_preedit(state);
        let (cand_fixed, cand_fixed_x, cand_fixed_y) = self.candidate_fixed_pos();
        // mode_label 已在顶部计算（纳入空则隐藏守卫）：作为候选窗内联标记随候选窗一并显示。
        let _ = self.ui_tx.send(UiCommand::UpdateCandidates {
            preedit,
            preedit_caret,
            preedit_host_owned: in_app,
            composing: crate::session_output::composing_of(state),
            mode_label,
            candidates: items,
            selected,
            hover,
            page: state.current_page + 1,
            total_pages,
            grid,
            caret_x,
            caret_y,
            caret_height,
            caret_valid,
            fixed: cand_fixed,
            fixed_x: cand_fixed_x,
            fixed_y: cand_fixed_y,
        });
        self.push_overlay_frame((caret_x, caret_y), anchor_source);
        // 记下候选窗**实际**用的位置基准，供后续非坐标重绘复用（见上面 `hold_anchor`）。
        // 只在首显和坐标校正时更新：复用锚点的那些重绘按定义不改变位置，回写等于把
        // 「谁有资格移动候选窗」这条判据又散成两处。
        if !hold_anchor {
            *self.shown_anchor.lock().unwrap_or_else(|e| e.into_inner()) = (cx, cy, true);
            // 基准**只在首显时**用显示位置初始化：那一刻还没有「上一次被认可的插入点」。
            // 之后它由 `handle_caret_update` 用宿主报的坐标维护（settle 吸收与 reshow 两处）。
            // ⚠ 若在这里跟着每次下发一起覆盖，reshow 刚写进去的插入点会被显示位置盖回去 ⇒
            // 组合起点钉住位置后，基准与宿主坐标的差值恒定存在，同一个 dx 反复触发 reshow。
            if !shown {
                *self
                    .caret_baseline
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = (cx, cy, true);
            }
        }
        // 候选窗已下发显示：标记本组合已首显，后续刷新（翻页/选字/打字）即可立即下发不再延迟。
        *self
            .candidate_shown
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = true;
        tracing::debug!(
            "notify_ui_update: build+send {:?} (n={n_items}) pos=({caret_x},{caret_y}) h={caret_height} \
             valid={caret_valid} anchor={anchor_source} raw=({cx},{cy}) hold={hold_anchor} \
             first={is_first_frame} authorized={authorized} in_app={in_app} \
             compStart=({},{},valid={}) shownAnchor=({},{},valid={}) fixed={cand_fixed}",
            t_nu.elapsed(),
            cs.0,
            cs.1,
            cs.2,
            anchor.0,
            anchor.1,
            anchor.2,
        );
    }

    pub(crate) fn notify_ui_hide(&self) {
        // 候选窗隐藏即会话终结：无条件收回 CapsLock 拦截。
        //
        // ★ 这里刻意**不查 state** 而是直接归零。闸门的两个方向后果不对称：少吃只是
        // 「CapsLock 绑定这一次没生效」，多吃却是「用户在别的应用里 CapsLock 按不动」。
        // 凡拿不准就归零。
        wind_keys::capslock_hook::set_should_eat(false);
        wind_keys::capslock_hook::set_alt_gate(0);
        wind_keys::capslock_hook::set_alt_pid(0);
        // 悬停归零同理：窗口没了，悬停目标不可能还有意义。UI 侧 `CandidateMouse::reset_hover`
        // 清的只是防抖闸门（决定何时**发**事件），高亮与 tooltip 读的是本值——不清这一句，
        // 特殊模式下窗口再次弹出时会带着上次的悬停高亮，鼠标却从未移动过。
        self.clear_hover();
        self.emit_hide();
        self.reset_first_show();
        // Linux：候选收起 ⇒ 候选右键菜单失去操作对象，一并收掉（主菜单不受影响：它可以在没有
        // 候选时打开，一条无关的 HideCandidates 不该把它收掉——与 Windows 连带收掉任何菜单不同）。
        // ⚠ 本函数常在持 `state` 锁时被调用，这里绝不能取锁，见 `candidate_menu_open`。
        #[cfg(ext_presenter)]
        if self
            .candidate_menu_open
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            let _ = self.ui_tx.send(UiCommand::HideMenu);
        }
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
            UiEvent::ToolbarMoved { right, bottom } => self.save_toolbar_anchor(right, bottom),
            UiEvent::SoftKeyboardMoved { right, bottom } => {
                self.save_softkeyboard_anchor(right, bottom)
            }
            // 只在这里换算平铺下标：`candidate_op` 的另一批调用方（Ctrl+数字热键等）传的
            // 本来就是页内下标。
            UiEvent::CandidateOp { op, page_local } => {
                if let Some(page_local) = self.resolve_event_index(page_local, false) {
                    self.candidate_op(op, page_local)
                }
            }
            UiEvent::RequestCandidateMenu { page_local, x, y } => {
                self.show_candidate_menu(page_local, x, y)
            }
            UiEvent::RequestMainMenu(anchor) => {
                #[cfg(all(target_os = "linux", ext_presenter))]
                self.send_host_work_area();
                self.show_main_menu(anchor)
            }
            UiEvent::RequestToolbarMenu { action, anchor } => {
                #[cfg(all(target_os = "linux", ext_presenter))]
                self.send_host_work_area();
                self.show_toolbar_menu(action, anchor)
            }
            UiEvent::MenuAction(kind) => self.menu_action(kind),
            UiEvent::MenuClose => {
                // ESC / 点击别处关闭：无动作派发，可直接解除 tooltip 隐藏抑制。
                self.menu_close();
                self.clear_tooltip_menu_flag();
            }
            UiEvent::GlobalHotkey(action) => self.handle_global_hotkey(&action),
            UiEvent::SoftKeyboardKey { slot, shift, ctrl } => {
                self.ui_softkeyboard_key(&slot, shift, ctrl)
            }
            UiEvent::SoftKeyboardPage(i) => self.ui_softkeyboard_page(i),
            UiEvent::SoftKeyboardClose => self.ui_softkeyboard_close(),
            UiEvent::SoftKeyboardPin => {
                self.toggle_softkeyboard_pin();
            }
            UiEvent::SoftKeyboardFunctionKey(name) => self.ui_softkeyboard_fn_key(&name),
            UiEvent::StatusTipMoved { x, y } => self.save_status_tip_pos(x, y),
            UiEvent::CandidateWindowMoved { x, y } => self.save_candidate_pos(x, y),
            UiEvent::CandidateDoubleClick => self.on_candidate_double_click(),
            UiEvent::RequestStatusMenu { x, y } => self.show_status_menu(x, y),
            UiEvent::RequestTooltipMenu {
                x,
                y,
                candidate,
                hit,
                doc_fingerprint,
            } => self.show_tooltip_menu(x, y, candidate, hit, doc_fingerprint),
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

    /// 开 / 关**单字输入**，以新状态重过滤并刷新候选。
    ///
    /// # ⚠️ 与 [`Self::set_filter_mode`] 的关键区别：**不写配置**
    ///
    /// 那个切一次就写回 `input.filter_mode`；本项只改内存里的
    /// [`State::single_char_override`]，重启或切方案即回到配置值（2026-09-07 用户拍板）。
    /// 照抄那边加一行 `set_user_string` 是错的——本项是**方案级**配置，写盘要落到
    /// `schema_overrides/{id}.toml`，那是另一条链（须登记 `SideCommitter`）。
    ///
    /// 目标状态与当前**生效**状态相同时直接返回，故「方案本来就开着，用户又按了一次开」
    /// 不会平白留下一个临时态——那会让随后的切方案清空变成用户可感知的跳变。
    pub fn set_single_char(&self, on: bool) {
        let tip = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.set_single_char_in(&mut s, Some(on))
        };
        if let Some(label) = tip {
            self.show_tip(label);
        }
    }

    /// 持 state 锁的调用点用的本体。返回**要显示的状态泡文案**，`None` = 无变化。
    ///
    /// ⚠️ 状态泡刻意由调用方在**锁外**弹（`show_tip` 会走 UI 通道）。这与
    /// `set_filter_mode` 里那句 `drop(s); self.show_tip(label);` 是同一条约束。
    ///
    /// `on` 为 `None` = 清掉临时态、回落配置层（`single_char:follow` 动词）。
    pub(crate) fn set_single_char_in(
        &self,
        state: &mut State,
        on: Option<bool>,
    ) -> Option<&'static str> {
        let before = self.effective_single_char(state);
        match on {
            Some(v) if before == v => return None, // 已是这个状态，不留临时态
            Some(v) => state.single_char_override = Some(v),
            // `take` 一步做两件事：清掉临时态，并在本来就没有时提前返回（无变化、不弹泡）。
            None => {
                state.single_char_override.take()?;
            }
        }
        let after = self.effective_single_char(state);
        if after == before {
            return None; // 清临时态后恰好回到同一状态：什么都没变，不弹泡
        }
        // 组合中：以新状态重建候选并刷新（与 `set_filter_mode` 同）。
        if !state.input_buffer.is_empty() {
            self.update_candidates(state);
            self.notify_ui_update(state);
        }
        Some(if after {
            SINGLE_CHAR_TIPS.0
        } else {
            SINGLE_CHAR_TIPS.1
        })
    }

    /// 在开 / 关之间切换（`single_char:toggle` 与裸 `single_char` 的实现）。
    ///
    /// 从**当前生效**状态取反，而不是从临时态取反：临时态为 `None` 时用户看到的是配置
    /// 值，切换必须从他看见的那个状态出发，否则第一次按热键可能毫无变化。
    pub fn toggle_single_char(&self) {
        let tip = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.toggle_single_char_in(&mut s)
        };
        if let Some(label) = tip {
            self.show_tip(label);
        }
    }

    /// 持 state 锁的切换本体。返回值语义同 [`Self::set_single_char_in`]。
    pub(crate) fn toggle_single_char_in(&self, state: &mut State) -> Option<&'static str> {
        let next = !self.effective_single_char(state);
        self.set_single_char_in(state, Some(next))
    }

    /// 执行一个单字输入按键动词（[`wind_config::SingleCharAction`]）。
    ///
    /// 三个消费点（`key_actions` 空缓冲 / `key_actions` 缓冲非空 / `session_actions`）
    /// 共用这一处，免得三份 `match` 各自漂移——本仓「三处必须一致」的约束已栽过四次。
    pub(crate) fn apply_single_char_action(
        &self,
        state: &mut State,
        action: wind_config::SingleCharAction,
    ) -> Option<&'static str> {
        match action {
            wind_config::SingleCharAction::Toggle => self.toggle_single_char_in(state),
            wind_config::SingleCharAction::Set(v) => self.set_single_char_in(state, Some(v)),
            // 清临时态、回落配置层。
            wind_config::SingleCharAction::Follow => self.set_single_char_in(state, None),
        }
    }

    /// 持久化简繁开关到 `config.input.s2t.enabled`（单一源：与设置页统一，reload 不会覆盖
    /// 菜单/热键选择）。菜单与热键两条切换路径共用，避免只改一处留下不对称。
    pub(crate) fn persist_s2t_enabled(&self, on: bool) {
        if let Err(e) = Config::set_user_bool(&["input", "s2t", "enabled"], on) {
            warn!("toggle_s2t: 持久化 input.s2t.enabled 失败: {}", e);
        }
        self.refresh_config_in_memory(|c| c.input.s2t.enabled = on);
    }

    /// 持久化繁简开关到 `config.input.t2s.enabled`（同 [`Self::persist_s2t_enabled`]）。
    pub(crate) fn persist_t2s_enabled(&self, on: bool) {
        if let Err(e) = Config::set_user_bool(&["input", "t2s", "enabled"], on) {
            warn!("toggle_t2s: 持久化 input.t2s.enabled 失败: {}", e);
        }
        self.refresh_config_in_memory(|c| c.input.t2s.enabled = on);
    }

    /// 两个转换方向都开着时把 `t2s` 关掉并**落盘**，让配置文件回到合法状态。
    ///
    /// # 为什么关的是 t2s 而不是 s2t
    ///
    /// 走到这里说明配置里已经是「两个都 true」，**无从知道用户后开的是哪一个**——
    /// 配置是一份快照，不带顺序。于是取一个可解释的固定优先级：保简入繁出。理由是
    /// 它是既有功能、用户基数大，而繁入简出是后加的小众档；反过来会让一批老用户在
    /// 某次升级后莫名其妙地不出繁体了。
    ///
    /// 真正「后开的赢」那条语义住在 [`Self::toggle_conversion_direction`]（热键/工具栏/
    /// 菜单/命令栏）与设置页的互斥联动里——那两处**知道**用户此刻按的是哪一个。本函数
    /// 是最后一道防线，接的是那两处都够不着的入口。
    /// 启动期归一（见 [`Self::normalize_conversion_exclusivity`]）：读自己的运行时配置，
    /// 不像 reload 那样有一份现成的 `new_cfg`。
    // 唯一调用方是 construct::new（desktop-ui 特性）；wind-webdata 等不带该特性的依赖方编译时它无人调用。
    #[cfg_attr(not(feature = "desktop-ui"), allow(dead_code))]
    pub(crate) fn normalize_conversion_exclusivity_on_start(&self) {
        if self.rt().config.input.has_conversion_conflict() {
            warn!("启动时 input.s2t 与 input.t2s 同时开启，已关闭 input.t2s（两个方向互斥）");
            self.persist_t2s_enabled(false);
        }
    }

    fn normalize_conversion_exclusivity(&self, cfg: &Config) {
        if !cfg.input.has_conversion_conflict() {
            return;
        }
        warn!("input.s2t 与 input.t2s 同时开启，已关闭 input.t2s（两个方向互斥）");
        self.persist_t2s_enabled(false);
    }

    /// 确保 `to_traditional` 方向（true = 简入繁出）的转换器在内存里：缺则**同步**读盘。
    /// 返回该方向是否可用（数据缺失为 false）。
    ///
    /// 只在切换入口与配置生效时调——那是一次性的用户动作，不是逐键路径。release 实测
    /// s2t 1.3–3.4 ms、t2s 0.1–0.3 ms（2026-10-08，Linux 页缓存热，`data_needs` 的
    /// `converter_load_timing`），远低于设计稿的 50 ms 门槛，故同步加载、不弹「正在加载」。
    pub(crate) fn ensure_converter(&self, to_traditional: bool) -> bool {
        let slot = if to_traditional { &self.s2t } else { &self.t2s };
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = load_converter(
                self.data_dir().as_deref(),
                &self.rt().config,
                to_traditional,
            );
        }
        g.is_some()
    }

    /// 切换入口（热键、菜单、工具栏、命令栏）共用：要**打开**的方向先确保数据在内存，
    /// 缺数据就提示「简繁数据缺失」且不切。返回是否切了。
    ///
    /// 两条入口曾各写一份：热键路径查「已加载」、菜单路径不查——数据缺失时菜单照样把开关
    /// 拨上，状态显示「繁」而上屏原样不转。
    pub(crate) fn try_toggle_conversion(&self, to_traditional: bool) -> bool {
        let turning_on = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if to_traditional {
                !s.s2t_enabled
            } else {
                !s.t2s_enabled
            }
        };
        if turning_on && !self.ensure_converter(to_traditional) {
            self.show_toast(
                "简繁数据缺失",
                ToastPosition::BottomCenter,
                ToastKind::Error,
            );
            return false;
        }
        self.toggle_conversion_direction(to_traditional);
        true
    }

    /// 配置生效后按 [`DataNeeds`](crate::data_needs::DataNeeds) 对齐按需数据。
    /// `reload_user_config` 与方案设置变更（`refresh_schema_derived_config`）调。各项方向不同：
    ///
    /// - 拆字表：装 / 卸都做（见 `sync_chaizi_assets`；运行期按需装的那份不卸）。
    /// - 主码表反查索引：只**补建**（后台，`spawn_index_warm` 单飞、已就绪即返回）。消费者
    ///   （联想、`${code_rev}`）只问 `reverse_index_if_ready`，从不自己建——不在这里补，
    ///   设置页刚打开的功能就一直拿不到索引。不需要时**不卸**：它有内存护栏（只留在用的几份），
    ///   失效由 `invalidate_schema` / `reload_from_config` 负责。
    /// - 用户层编码索引：只**清空**（不需要时）；需要时由消费者首次用到时后台建。
    /// - 简繁转换器：只**加载**（开着的方向），从不卸载——切换常来回按；`input.s2t.variant`
    ///   改了要重启才生效（沿用原有行为，已加载的那份不重读）。
    pub(crate) fn apply_data_needs(&self) {
        let needs = self.data_needs();
        self.engine_mgr
            .set_correct_pinyin_in_templates(needs.correct_pinyin);
        self.sync_chaizi_assets_with(&needs);
        if needs.reverse_index {
            self.spawn_index_warm(&self.engine_mgr.code_source_schema(), false);
            self.spawn_index_warm(&self.engine_mgr.assoc_word_schema(), false);
        }
        if !needs.user_text && self.engine_mgr.user_text_loaded() > 0 {
            info!("释放按词查编码用户层（当前配置无消费者）");
            self.engine_mgr.clear_user_text();
        }
        if needs.s2t {
            self.ensure_converter(true);
        }
        if needs.t2s {
            self.ensure_converter(false);
        }
        if needs.assoc_bigram {
            self.ensure_assoc_bigram_async();
        }
        self.engine_mgr.warm_hot_schema_fields();
    }

    /// 切换简繁转换的**一个方向**，返回切换后该方向是否开着。
    ///
    /// `to_traditional=true` 即简入繁出（`s2t`），`false` 即繁入简出（`t2s`）。
    ///
    /// # 为什么两个方向必须互斥
    ///
    /// 内部候选域只有一个。两个方向同时开，出口就是「先按一个方向转、再按另一个转
    /// 回来」——产物是绕了一圈的**近似**原文（一对多的字回不来），用户既得不到繁体也
    /// 得不到简体，还看不出是哪一步造成的。所以开一个即关另一个。
    ///
    /// ★ 被顺手关掉的那个**也要落盘**：只改内存的话，下次配置重载（设置页保存任意一项
    /// 都会触发）就把它从配置文件里读回来，两个方向重新同时开着，且用户什么都没做。
    pub(crate) fn toggle_conversion_direction(&self, to_traditional: bool) -> bool {
        let (on, other_off) = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if to_traditional {
                s.s2t_enabled = !s.s2t_enabled;
                let other_off = s.s2t_enabled && s.t2s_enabled;
                if s.s2t_enabled {
                    s.t2s_enabled = false;
                }
                (s.s2t_enabled, other_off)
            } else {
                s.t2s_enabled = !s.t2s_enabled;
                let other_off = s.t2s_enabled && s.s2t_enabled;
                if s.t2s_enabled {
                    s.s2t_enabled = false;
                }
                (s.t2s_enabled, other_off)
            }
        };
        if to_traditional {
            self.persist_s2t_enabled(on);
            if other_off {
                self.persist_t2s_enabled(false);
            }
        } else {
            self.persist_t2s_enabled(on);
            if other_off {
                self.persist_s2t_enabled(false);
            }
        }
        on
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
    ///
    /// 展开态下滚一行（翻页箭头同此）；收起态往下翻与翻页键同语义——先试展开。
    fn mouse_page(&self, dir: i32) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.candidates.is_empty() {
            return;
        }
        let changed = if Self::grid_active(&state) {
            // 鼠标往上滚到第一行就停住，不像 ↑ 那样收起：滚轮滚过头是常事。
            self.grid_scroll(&mut state, dir > 0)
        } else if dir < 0 {
            self.page_prev(&mut state)
        } else {
            self.try_expand_grid(&mut state) || self.page_next(&mut state)
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
            // 展开态下悬停的是平铺下标，校验范围是整个可见区。
            let (start, end) = self
                .grid_visible_range(&state)
                .unwrap_or_else(|| self.page_range(&state));
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

    /// 当前状态该显示的图标主字。**这是这个标签的唯一产地。**
    ///
    /// 从前 `build_status`（语言栏图标 + IPC）、`handle_menu`（工具栏）、
    /// `status_indicator_text`（状态气泡）各抄了一份
    /// `effective_chinese ? schema : caps ? "A" : "英"`。三份字面量意味着任何一次
    /// "让标签可配"的改动都得记得改三处，漏一处的表现是"设置里改了，某个面还是旧字"。
    ///
    /// ⚠️ 状态气泡**刻意没有**改成调用本函数：它比这里多一个维度——`ui.status.items`
    /// 的内容段过滤（关掉 `caps` 段时大写锁定不顶替、落回中英显示）。硬套进来会静默
    /// 丢掉那个维度。它只共享 `[ui.labels]` 的**取值**，不共享分支结构。
    ///
    /// 中文态回落的「中」不可配：那一档是"方案没配 `icon_label`"的兜底，属于方案侧
    /// 的缺省而非一个用户会想改的状态标签。
    pub(crate) fn mode_icon_label(&self, chinese_mode: bool, caps_lock: bool) -> String {
        // 有效中文：中文模式且大写锁定未开（对齐 Go effectiveChinese = chineseMode && !capsLockOn）。
        if chinese_mode && !caps_lock {
            let id = self.engine_mgr.active_schema_id();
            let lbl = self.engine_mgr.schema_icon_label(&id);
            if lbl.is_empty() {
                "中".to_string()
            } else {
                lbl
            }
        } else if caps_lock {
            self.rt().config.ui.labels.caps_label()
        } else {
            self.rt().config.ui.labels.english_label()
        }
    }

    /// 拼音音节分隔符的显示替换：配置不是默认 `'`、当前是主路（无独占模式）且活跃引擎是
    /// 拼音 / 混输时返回新分隔符，否则 `None`。临时英文等模式里的 `'` 是正文，不能换。
    pub(crate) fn syllable_separator_for(&self, main_path: bool) -> Option<String> {
        if !main_path {
            return None;
        }
        // 应用级覆盖（compat `syllable_separator`）优先，没配跟随全局。
        let app = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lookups
            .syllable_separator;
        let sep = match app {
            Some(s) => s.as_str().to_string(),
            None => self.rt().config.ui.candidate.syllable_separator.clone(),
        };
        if sep == "'" {
            return None;
        }
        matches!(
            self.engine_mgr.current_engine_type(),
            Some(wind_engine::EngineType::Pinyin | wind_engine::EngineType::Mixed)
        )
        .then_some(sep)
    }

    /// 「热键激活的模式活着吗」—— `STATUS_HOTKEY_SESSION` 的判据，**单一真相源**。
    ///
    /// 抽出来是因为它落在**每一次按键**的返回路径上（`push_hotkey_session_if_changed`），
    /// 而走整个 `build_status()` 只为取这一个 bool 太贵：那边还要 `mode_icon_label()`
    /// 造一个 String、`key_down_tsf_hashes()` / `key_up_tsf_hashes()` 各 collect 一个全量
    /// `Vec<u32>`，三次分配白白落在热路径上。
    ///
    /// ⚠️ 判据**刻意不含「且没有 composition」**：C++ 侧 `_HasInputSession()` 是 OR，
    /// 有 composition 时本位多余但无害；加上那个条件反而要在这里复算一份「有没有
    /// composition」，与 DLL 的 `HasActiveComposition()` 成为两个真相源 —— 那正是前一版
    /// （边沿驱动）出问题的形态。宁可多置一位。
    pub(crate) fn hotkey_session_now(&self) -> bool {
        // 无占位的联想会话也算：宿主里没有组合，DLL 只能靠这一位知道该转发按键。
        // 先于取 state 锁（`assoc_bare_session_now` 自己会取），避免重入。
        if self.assoc_bare_session_now() {
            return true;
        }
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.add_word_active
            || matches!(
                s.active,
                Some(
                    ModeKind::TempPinyin
                        | ModeKind::Special(_)
                        | ModeKind::RareChar
                        | ModeKind::Reverse
                )
            )
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
        // 判据收在 `hotkey_session_now()` 一处（它也在每次按键的返回路径上被调）。
        // ⚠️ 必须在上面那个 guard 释放**之后**取：`std::sync::Mutex` 不可重入。
        let hotkey_session = self.hotkey_session_now();
        let soft_keyboard = self
            .softkeyboard_active
            .load(std::sync::atomic::Ordering::Relaxed);
        // 键盘面：按键交还输入法，C++ 不启用软键盘总闸。**每次都从当前面重新读**，
        // 不另存一份 atomic——多一份镜像就多一处会与切面动作漂移的状态。
        let soft_keyboard_keys = soft_keyboard
            && self
                .softkeyboard_table()
                .pages()
                .get(self.softkeyboard_page_idx())
                .is_some_and(|p| p.send_keys);
        let icon_label = self.mode_icon_label(chinese_mode, caps_lock);
        // Linux：托盘图标按主字运行时渲染，文件必须先于带这个标签的状态帧落盘（见子模块头）。
        #[cfg(all(target_os = "linux", ext_presenter, not(test)))]
        self.ensure_tray_icons((chinese_mode && !caps_lock).then_some(icon_label.as_str()));
        self.hook_mode(chinese_mode, full_width);
        StatusUpdateData {
            chinese_mode,
            full_width,
            chinese_punct,
            toolbar_visible,
            caps_lock,
            soft_keyboard,
            soft_keyboard_keys,
            hotkey_session,
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
    /// 对 caret 应用当前宿主兼容变换，并返回与本次变换**同一时刻**的规则快照。
    ///
    /// 高频 caret 路径后续还要判断宿主协议帧对；若变换后再锁一次 `active_compat`，焦点恰在
    /// 两次取锁之间切换时就可能拿到两个宿主的规则拼盘。返回 Copy 快照既少一次锁，也让
    /// 本帧所有兼容判据共享同一来源。
    fn apply_caret_compat(&self, data: &mut CaretData) -> ActiveCompat {
        let active = *self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
        let (use_top, dx, dy) = (
            active.caret_use_top,
            active.caret_offset_x,
            active.caret_offset_y,
        );
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
        active
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
        height > 0 && !(x == 0 && y == 0) && x > -32000 && x < 32000 && y > -32000 && y < 32000
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
        // 宿主接管了 UI（TSF UI-less）或前台是 D3D 独占全屏：气泡也是本进程的一个窗口，
        // 盖到独占全屏的游戏上同样会把它踢出独占态。规范原话是 TIP 的**任何** UI 都得经
        // UIElementMgr 征得宿主同意——气泡没有对应的 UIElement，只能不弹。
        if let Some(reason) = self.ui_suppressed_by_host() {
            debug!("status_tip 被压住: {reason}");
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
        self.show_tip_at(text, raw_x, raw_y, raw_h);
    }

    /// **持 state 锁时**弹状态泡。
    ///
    /// 与 [`Self::show_tip`] 的唯一区别是 caret 从调用方已有的 `state` 里读，而不是自己
    /// 再取一次 state 锁（本仓 state 锁**不可重入**，重入即死锁）。
    ///
    /// ⚠️ 锁序仍是 `state → last_valid_caret`（后者在 `resolve_caret_for_ui` 内取），
    /// 与候选窗路径同向，不构成反序。`show_tip` 那句「先放锁再解析」说的是它自己没必要
    /// 持着锁，不是「持锁调用会反序」。
    pub(crate) fn show_tip_locked(&self, state: &State, text: &str) {
        self.show_tip_at(text, state.caret_x, state.caret_y, state.caret_height);
    }

    /// 状态泡的发送本体（caret 由调用方给出）。**不取 state 锁**。
    fn show_tip_at(&self, text: &str, raw_x: i32, raw_y: i32, raw_h: i32) {
        self.show_tip_with(text, raw_x, raw_y, raw_h, None);
    }

    /// 以指定定位弹状态泡（焦点气泡的锚点超时用），光标仍从 state 里取（选屏参考）。
    pub(crate) fn show_tip_placed(&self, text: &str, placement: wind_ui_types::StatusTipPlacement) {
        let (raw_x, raw_y, raw_h) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.caret_x, s.caret_y, s.caret_height)
        };
        self.show_tip_with(text, raw_x, raw_y, raw_h, Some(placement));
    }

    /// `forced = None` 时按 [`Self::status_position`] + 原始坐标可信度解析定位（见
    /// [`Self::placement_for`]），解析为「不显示」就直接返回。**不取 state 锁**。
    fn show_tip_with(
        &self,
        text: &str,
        raw_x: i32,
        raw_y: i32,
        raw_h: i32,
        forced: Option<wind_ui_types::StatusTipPlacement>,
    ) {
        let bundle = self.rt();
        let si = &bundle.config.ui.status;
        if !si.enabled {
            return;
        }
        if let Some(reason) = self.ui_suppressed_by_host() {
            debug!("status_tip 被压住: {reason}");
            return;
        }
        if text.trim().is_empty() {
            return;
        }
        // 原始坐标的可信度必须在 resolve 之前取：resolve 会回退到最近一次有效坐标、并把
        // valid 报成 true，拿它判就分不出「这次坐标本身不可信」（兜底的触发条件）。
        let raw_valid = Self::caret_is_valid(raw_x, raw_y, raw_h);
        // resolve 照调：它顺带刷新 last_valid_caret，兜底 `last` 依赖这份记录。
        let (x, y, caret_height, _valid) = self.resolve_caret_for_ui(raw_x, raw_y, raw_h);
        // 常驻(always)→ duration_ms=0(UI 不自动隐藏);否则按 duration 自动隐藏。对齐 Go display_mode。
        let duration_ms = if si.display_mode.eq_ignore_ascii_case("always") {
            0
        } else {
            si.duration.max(1) as u64
        };
        // 定位：固定坐标 / 锚点 / 跟随光标（坐标不可信时按兜底）。规则优先回落全局。
        let placement = match forced.or_else(|| {
            Self::placement_for(
                self.status_position(),
                raw_valid,
                si.offset_x,
                si.offset_y,
                si.caret_above(),
            )
        }) {
            Some(p) => p,
            None => {
                debug!("status_tip → 不显示: 光标坐标不可信且兜底为 hide");
                return;
            }
        };
        self.emit_status_tip(text.to_string(), x, y, caret_height, duration_ms, placement);
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
    /// `fixed` 与锚点模式不读 caret，故不受本闸门约束，一律直接显示。
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
        // fixed / 锚点不读 caret，不受可信度闸门约束，直接显示。清掉可能残留的上一次挂起，
        // 免得它的锚点超时在这之后把气泡又摆到别处。
        let pos = self.status_position();
        if pos.mode != wind_config::app_compat::StatusPositionMode::FollowCaret {
            self.pending_focus_tip
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.show_tip(&self.status_indicator_text());
            return;
        }
        let source = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.caret_source
        };
        // 不需要等 TSF 坐标的两种宿主，直接显示：
        // - 宿主自己摆窗口（`caret_independent`，Wayland 的 input popup 由合成器按光标摆）：
        //   坐标根本不用，等它只会永远等不到；
        // - 宿主声明光标可信（`host_caret_trusted`，Linux addon）：来源恒为 UNKNOWN（没有 TSF
        //   语义域），按 TSF 判就永远挂起，「焦点变化时显示」在 Linux 上从不出现。
        let host_caret_trusted = self
            .caret_independent
            .load(std::sync::atomic::Ordering::Relaxed)
            || self
                .host_caret_trusted
                .load(std::sync::atomic::Ordering::Relaxed);
        if host_caret_trusted || wind_ipc::protocol::caret_source::is_tsf(source) {
            self.pending_focus_tip
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.show_tip(&self.status_indicator_text());
        } else {
            // 挂起，等 DLL 补来的 TSF 坐标。挂起在下次焦点事件/失焦时作废。
            //
            // 兜底为 `last` / `hide` 时**不设超时**：超时到期只能拿现有的不可信坐标显示，那正是
            // 本闸门要挡的东西。兜底为**锚点**时设约 150ms 超时——锚点不是光标坐标，不在这条
            // 反对理由之列，到期仍无 TSF 坐标就显示在锚点（C2-33 / GH#148）。
            debug!(
                "focus_tip → 挂起: 坐标来源 {} 非 TSF 域，等待权威坐标",
                wind_ipc::protocol::caret_source::name(source)
            );
            self.park_focus_tip(pos.fallback);
        }
    }

    /// 合成当前 IME 核心状态文本：方案/中英(+大写) · 标点 · [全角] · [繁]。
    /// 默认态省略（半角/简体不显示），减少干扰；标点总显示（。/.）。
    pub(crate) fn status_indicator_text(&self) -> String {
        let (chinese, punct_cn, full, s2t, t2s, caps) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.chinese_mode,
                s.chinese_punct,
                s.full_width,
                s.s2t_enabled,
                s.t2s_enabled,
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
        // 标签取值与语言栏图标 / 工具栏同源（`[ui.labels]`），但**分支结构是本函数独有的**：
        // 下面这三档比 `mode_icon_label` 多了 `show()` 内容段过滤，不能合并过去。
        if caps && show("caps") {
            parts.push(self.rt().config.ui.labels.caps_label());
        } else if !show("schema") {
            // 方案段关闭：首槽整体略过（含英文态标记）
        } else if !chinese {
            parts.push(self.rt().config.ui.labels.english_label());
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
        // 转换方向：简入繁出显示「繁」，繁入简出显示「简」。默认态两个都关，此段整体省略
        // ——于是「简」出现本身就意味着「有转换、出的是简体」。
        //
        // ★ 两个方向共用 `s2t` 这一个内容段（`ui.status.items`），不新开一个：它回答的是
        //   「上屏出什么体」，而两个方向互斥，一个槽位就够。
        // ★ 这一段是繁入简出**唯一**的视觉反馈：它按设计不进右键菜单也不进工具栏，
        //   而 `show_status` 在文本与上次相同时整个跳过 ⇒ 不加这里，按下热键会是彻底的
        //   「按了没反应」。
        if show("s2t") {
            if s2t {
                parts.push("繁".into());
            } else if t2s {
                // ★ 这里带方向而 s2t 那档不带，是**刻意的不对称**：简入繁出是多数人用的
                // 那档，「繁」这一个字已经沿用很久，加成「简→繁」是拿所有人的习惯去换
                // 一个对称性。繁入简出没有历史包袱，而它单显「简」会与默认态（不转换、
                // 本段整体省略）说的是同一件事——只有写出方向才看得出「转过」。
                parts.push("繁→简".into());
            }
        }
        parts.join(" ")
    }

    /// 显示合成的核心状态气泡（中英/标点/全半角/简繁/方案切换共用）。
    ///
    /// 文本与上次显示的完全相同时**整个跳过**，不弹窗——用户通过 `ui.status.items`
    /// 关掉某段后，切换该状态不再改变气泡文本，弹一个和上次一模一样的气泡纯属噪声。
    pub(crate) fn show_status(&self) {
        // 同 push_state_update：状态泡文案含标点态，先让方案级覆盖落地。
        self.sync_schema_scope_locked();
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

    /// `toggle_mode` 的分发内核：切中英，并**回传**切换前那串编码的处置结果。
    ///
    /// 抽出来是因为出口有两个（见 [`SwitchCommit`]）。此前 `dispatch_hotkey` 直接
    /// `let (status, _)` 丢掉 commit_text，于是经热键表/菜单触发的中英切换既不上屏
    /// 也不结束组合——与本次修的方案切换是同一个缺口，只是少了一个出口。
    pub(crate) fn toggle_mode_with_commit(&self) -> (Option<StatusUpdateData>, SwitchCommit) {
        self.toggle_mode_with_commit_cased(false)
    }

    /// 同 [`Self::toggle_mode_with_commit`]，`upper` 见 `handle_toggle_mode_cased`。
    pub(crate) fn toggle_mode_with_commit_cased(
        &self,
        upper: bool,
    ) -> (Option<StatusUpdateData>, SwitchCommit) {
        let had_pending = self.has_pending_composition();
        let (status, text) = self.handle_toggle_mode_cased(upper);
        (status, SwitchCommit { text, had_pending })
    }

    /// `ime.set("cn-en", …)` 的执行体：**置**中英态为目标值（区别于 toggle 的翻转）。
    ///
    /// 调用方保证「已在目标态」根本不会进来（`ime_state_set` 先判，只提示不动作），
    /// 待编码零扰动的问题不存在；走到这里必然真切换，编码按 toggle 同一策略交还宿主。
    /// CapsLock 开着时走 [`CapsCancelSwitch::Directed`]：取消锁定的同时直达目标，
    /// 不吃 `mode_after_cancel` 的归位改写。
    pub(crate) fn cn_en_set_with_commit(
        &self,
        chinese: bool,
    ) -> (Option<StatusUpdateData>, SwitchCommit) {
        let had_pending = self.has_pending_composition();
        let (status, text) = self.handle_switch_mode_cased(Some(chinese), false);
        (status, SwitchCommit { text, had_pending })
    }

    /// **按键上下文**的热键分发：与 [`Self::dispatch_hotkey`] 共用同一张动作表，区别只在
    /// 「切换前未上屏编码」的出口——这里经 `KeyAction` 交还宿主（`CommitText` 会顺带
    /// `EndComposition`），那里只能 push。返回 `None` = 该动词未被接受，调用方不吞键。
    ///
    /// 只特判两个会动输入状态的动词，其余原样转交，故动作表仍然只有一处定义。
    pub(crate) fn dispatch_hotkey_keyed(&self, action: &str) -> Option<KeyAction> {
        match action {
            "toggle_mode" => {
                let (status, commit) = self.toggle_mode_with_commit();
                status
                    .is_some()
                    .then(|| self.schema_switch_key_action(commit))
            }
            "switch_engine" => Some(self.schema_switch_key_action(self.cycle_schema())),
            _ => self
                .dispatch_hotkey(action)
                .then(|| KeyAction::StatusUpdate(self.build_status())),
        }
    }

    /// 分发热键动作；返回是否已处理。
    ///
    /// ⚠ **无按键上下文**的路径（全局热键 `WM_HOTKEY`、托盘/工具栏菜单）用本函数：
    /// 会动输入状态的两个动词在这里把编码经 push 出口交给宿主。按键路径请走
    /// [`Self::dispatch_hotkey_keyed`]，否则空文本清不掉宿主里的组合。
    pub(crate) fn dispatch_hotkey(&self, action: &str) -> bool {
        match action {
            "toggle_mode" => {
                let (status, commit) = self.toggle_mode_with_commit();
                self.push_switch_commit(&commit);
                status.is_some()
            }
            "switch_engine" => {
                let commit = self.cycle_schema();
                self.push_switch_commit(&commit);
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
            "toggle_s2t" | "toggle_t2s" => {
                if !self.try_toggle_conversion(action == "toggle_s2t") {
                    return true;
                }
                self.show_status();
                // 工具栏「繁」格随切即刷（对齐 toggle_full_width 与菜单路径）。缺这步时
                // 只有一闪而过的状态气泡，工具栏状态滞后到下次刷新事件，被误感知为“切换卡”。
                // 关掉 s2t 的那一半同样要刷——互斥让一次按键改动两个方向。
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
            "open_dictionary" => {
                self.open_dictionary();
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
        let supported: [(&str, &str); 9] = [
            ("switch_engine", k.switch_engine.as_str()),
            ("toggle_full_width", k.toggle_full_width.as_str()),
            ("toggle_punct", k.toggle_punct.as_str()),
            ("toggle_toolbar", k.toggle_toolbar.as_str()),
            ("open_settings", k.open_settings.as_str()),
            ("open_dictionary", k.open_dictionary.as_str()),
            ("take_screenshot", k.take_screenshot.as_str()),
            ("toggle_s2t", k.toggle_s2t.as_str()),
            ("toggle_t2s", k.toggle_t2s.as_str()),
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
        // ★ 大小写档位循环**选了 CapsLock 时**同样要求装钩子：CapsLock 的 keydown 压根不
        // 转发给服务端、锁定态又由系统在 TSF 之前维护 —— 没有钩子，功能永不触发且毫无报错。
        // 判据是「配置的触发键恰好是 CapsLock」而不是「功能开着」：选了 tab 的用户（macOS
        // 的推荐值）走的是 keydown 主链路，不该为此白装一个全局钩子。
        rt.english_case_cycle_vk == Some(keymap::VK_CAPITAL)
            || rt
                .session_keys
                .classify(keymap::VK_CAPITAL, false, true)
                .is_some()
            || rt.schema_session_vks.contains(&keymap::VK_CAPITAL)
    }

    /// keyup 到达服务端时，该键的会话动作是否仍应执行。
    ///
    /// ★ 唯一为假的情形：CapsLock **且**钩子装着。keyup 能到服务端，本身就证明钩子那一刻
    /// 没吃它 ⇒ 系统已经翻转了锁定态（不可撤销），此时再执行绑定动作就是「和系统抢」，
    /// 用户看到翻页与大写同时发生。详见 `message_handler` 调用点的长注释。
    ///
    /// 纯函数：真装钩子的分支在 CI 的 Linux 上跑不到，判据必须能脱离平台单独验证。
    pub(crate) fn key_up_session_action_allowed(
        key_code: u32,
        capslock_hook_installed: bool,
    ) -> bool {
        !(key_code == keymap::VK_CAPITAL && capslock_hook_installed)
    }

    /// 钩子此刻是否真的装着。
    ///
    /// ★ 与 [`Self::capslock_bound`] 是**两件事**：后者是「配置想不想要」，本函数是「系统里
    /// 有没有」。安装可能失败（非 Windows、`SetWindowsHookExW` 被拒），也可能事后被系统静默
    /// 移除。凡是要判「这个键归谁管」的地方都必须问本函数——问 `capslock_bound()` 会在钩子
    /// 装不上时把键判给一个不存在的处理者，那正是「按了完全没反应」。
    ///
    /// 只为注释切换键 Alt 装的钩子不算：那时 CapsLock 不归它管。
    pub(crate) fn capslock_hook_installed(&self) -> bool {
        self.hook_caps.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 按配置装/卸全局键盘钩子（启动与配置热重载时调用）：CapsLock 要 ∪ 注释切换键 Alt 要。
    ///
    /// ★ 幂等：已装且仍该装 → 不动（重复 `SetWindowsHookExW` 会留下卸不掉的旧钩子）。
    pub(crate) fn sync_capslock_hook(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let caps = self.capslock_bound() || self.capslock_is_toggle_key();
        let alt = self.comment_alt_gate_mask();
        let want = caps || alt != 0;
        let mut slot = self.capslock_hook.lock().unwrap_or_else(|e| e.into_inner());
        let installed = if want == slot.is_some() {
            // ★ 「没配所以没装」此前是**完全静默**的：装成功打 INFO、卸载打 INFO，唯独这条
            // 最常见的路径一行都没有。排障时日志里搜不到任何 CapsLock 字样，无从区分
            // 「用户没配」「配了但动词/键名写错被剔除」「装失败」——三者的处置完全不同。
            if want {
                debug!("全局键盘钩子保留（CapsLock 要: {caps}，注释切换键 Alt 闸门: {alt:#x}）");
            } else {
                debug!(
                    "CapsLock 未配置会话态绑定、注释切换键未选 Alt（或非 Windows），不装全局钩子"
                );
            }
            want
        } else if !want {
            // Drop 即卸载（内部会先停拦截再停消息泵）。
            *slot = None;
            info!("CapsLock 未配置会话态绑定、注释切换键未选 Alt → 全局钩子已卸载");
            false
        } else {
            // 钩子回调在钩子线程执行，**必须只做非阻塞投递**：它超时会被系统静默移除且无从察觉。
            // 故这里只 send，真正的动作在 new 起的消费线程里做（可安全加锁）。
            let tx = self.capslock_press_tx.clone();
            match wind_keys::capslock_hook::CapsLockHook::install(Box::new(move |ev| {
                let _ = tx.send(ev);
            })) {
                Ok(h) => {
                    *slot = Some(h);
                    info!("全局键盘钩子：CapsLock 要: {caps}，注释切换键 Alt 闸门: {alt:#x}");
                    true
                }
                Err(e) => {
                    // 装不上就退化为「CapsLock 绑定 / Alt 切换注释不生效」，不影响其余功能。绝不回退到
                    // 「翻转再回敲」——那条路已被真机否掉（竞态 + 厂商 OSD 弹窗）。
                    tracing::error!(
                        "全局键盘钩子安装失败，CapsLock 绑定 / Alt 切换注释将不生效: {e}"
                    );
                    false
                }
            }
        };
        self.hook_caps.store(installed && caps, Relaxed);
        self.hook_alt_mask
            .store(if installed { alt } else { 0 }, Relaxed);
        // 立刻按当前会话状态校准一次（配置热重载可能只改了「切换按键」或切换键）。
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.sync_capslock_gate(&s);
        self.sync_capslock_idle_gate(&s);
    }

    /// 「切换按键」勾了 CapsLock（`keys.toggle_mode_keys` 含 `capslock`）。
    ///
    /// 只有装得上钩子时它才真的切中英：锁定态由系统在 TSF 之前翻，没有钩子就压不住，
    /// 那时 CapsLock 仍只是大写键（服务端 CapsLock 分支照旧只同步大写）。
    pub(crate) fn capslock_is_toggle_key(&self) -> bool {
        self.is_toggle_mode_keycode(keymap::VK_CAPITAL)
    }

    /// 同步「空闲时也吃 CapsLock」：CapsLock 是切换键且本输入法激活时，交给钩子当前活动
    /// 客户端的进程 pid（token 高 32 位），钩子只在前台窗口属于它时接管。
    ///
    /// 激活态 `ime_active` 由焦点进出与输入法切入切出维护，凡改它的地方都要紧跟着调本函数
    /// （焦点进入那处要在 `set_active_token` 之后）。宿主崩溃这类清不掉激活态的情形由钩子的
    /// 前台进程比对兜住。
    pub(crate) fn sync_capslock_idle_gate(&self, state: &State) {
        let pid = if self.capslock_is_toggle_key() && state.ime_active {
            (self.push_server.active_token() >> 32) as u32
        } else {
            0
        };
        wind_keys::capslock_hook::set_idle_pid(pid);
    }

    /// 同步「钩子此刻该不该吃 CapsLock」。
    ///
    /// ★★ 这个标志为 true 的时间窗必须尽量短。钩子是**全局**的：标志滞留意味着用户在
    /// **别的应用**里按 CapsLock 也切不动大小写——比功能不生效糟糕得多。故凡是会改变
    /// 「有没有输入会话」的出口都要调它，宁可多调（幂等的原子写，开销可忽略）。
    ///
    /// Alt 闸门同在这里：有候选时下发切换键那几侧，否则 0。
    pub(crate) fn sync_capslock_gate(&self, state: &State) {
        use std::sync::atomic::Ordering::Relaxed;
        // 未装钩子时也照常写：装钩子那一刻会重新校准，这里写了不会有副作用。
        wind_keys::capslock_hook::set_should_eat(
            self.hook_caps.load(Relaxed) && Self::has_input_session(state),
        );
        let alt = if state.candidates.is_empty() {
            0
        } else {
            self.hook_alt_mask.load(Relaxed)
        };
        wind_keys::capslock_hook::set_alt_gate(alt);
        let pid = if alt == 0 {
            0
        } else {
            (self.push_server.active_token() >> 32) as u32
        };
        wind_keys::capslock_hook::set_alt_pid(pid);
    }

    /// 钩子报告「CapsLock 被按下」（在专用消费线程执行，可安全加锁）。
    ///
    /// 走的是与键盘 keyup 路径**同一对函数、同一顺序**，故动词值域、守卫、各模式的选中/
    /// 翻页出口都不会分叉。钩子只负责「这个键被按了」，「按了该干什么」仍归那两张表。
    /// CapsLock 在**英文输入态**临时夺取为「候选大小写档位循环」：
    /// 默认 → 全大写 → 全小写 → 默认。返回 `None` = 本次不夺取，按键归原有语义。
    ///
    /// # 三道守卫，缺一不可
    ///
    /// - `input.capslock.english_case_cycle`：功能开关，**出厂关**；
    /// - [`Self::in_english_input_context`]：英文方案常驻 或 临英 overlay；
    /// - 有候选：空闲时按 CapsLock 仍是系统原生的大写锁定，不能吞掉。
    ///
    /// 这三条与 `apply_session_action` 的「有会话归绑定、无会话归原语义」是同一条纪律，
    /// 只是判据多了「语境是英文」这一维。
    ///
    /// # 为什么重建候选而不是就地改写
    ///
    /// 就地重套档位要先把每条候选还原成词库原文（`case_source`），漏一条就会得到
    /// 「档位切了一半」的列表；更要命的是它与「用户继续打下一个字母」那条路的产出可能
    /// 不同——同一个档位、同一串输入，两条路给出两份候选，这种不一致最难查。重建则天然
    /// 与逐键路径同源：候选表本就是每次按键从零装配的。
    pub(crate) fn try_english_case_cycle(&self) -> Option<KeyAction> {
        // 未配置触发键就没有这条路：`?` 在此只作「无配置即返回 None」的守卫，值本身不用。
        self.rt().english_case_cycle_vk?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 快捷输入单列一问（`mix_highlight_is_english`），**不**并进 `in_english_input_context`：
        // 那是「整个语境在打英文」，mix 的中文候选不该跟着补空格（见 `mix_candidate_is_english`）。
        if state.candidates.is_empty()
            || !(self.in_english_input_context(&state) || self.mix_highlight_is_english(&state))
        {
            return None;
        }
        state.english_case_variant = state.english_case_variant.next();
        if state.active == Some(ModeKind::TempEnglish) {
            self.update_temp_english_candidates(&mut state);
        } else if matches!(state.active, Some(ModeKind::Mix(_))) {
            self.update_mix_candidates(&mut state);
        } else {
            // 英文引擎恒 `should_commit = false`（见 `EnglishEngine::convert`），故这里
            // 不会有自动上屏意向要处置；返回值刻意丢弃。
            let _ = self.update_candidates(&mut state);
        }
        self.notify_ui_update(&state);
        Some(KeyAction::Consumed)
    }

    /// keydown 主链路上的档位循环闸门：配置的触发键**不是** CapsLock 时走这条。
    ///
    /// # 为什么与钩子那条分成两个消费点
    ///
    /// CapsLock 是 keyup-only 键（C++ 压根不转发它的 keydown，锁定态由系统在 TSF 之前
    /// 维护），只能靠全局钩子拦；而 tab / enter 这些是普通键，走的是正常的 keydown 链路。
    /// 两条路的**判据**共用 [`Self::try_english_case_cycle`]，分开的只是「谁把按键送进来」。
    ///
    /// ★ 落点在**单点分派之前**（`handle_candidate_action_hotkey` 旁边）：那里是「候选窗
    /// 显示期间生效的快捷键」的既定位置，五个模式一次接通。挂进各模式处理器要写五遍还会漏
    /// ——`overlay_ctrl_alt_guard` 的文档里点名了这条纪律。
    ///
    /// ⚠️ 只认 keydown：keyup 也放行的话，一次按键会切两档。
    pub(crate) fn try_english_case_cycle_key(&self, data: &KeyEventData) -> Option<KeyAction> {
        if data.event_type != EVENT_KEY_DOWN {
            return None;
        }
        // 带 Ctrl/Alt/Cmd 的组合归宿主快捷键，不是本功能。
        if data.modifiers & MOD_SHORTCUT != 0 {
            return None;
        }
        let vk = self.rt().english_case_cycle_vk?;
        // CapsLock 那份归钩子路径，走不到这里（它的 keydown 不会到达服务端）；显式排除是
        // 为了「同一个键被两条路各处理一次」这种最难查的重复触发从结构上不可能发生。
        if vk == keymap::VK_CAPITAL || data.key_code != vk {
            return None;
        }
        self.try_english_case_cycle()
    }

    /// 启动体检：档位循环的触发键被别的功能占着 → 告警。
    ///
    /// 这不是配置冲突（各自都合法），而是**运行期的优先级夺取**：英文输入期间那些绑定
    /// 按不出来。现场表现是「CapsLock 翻页在中文里好用，一打英文就失灵」「Tab 在英文里
    /// 调不出辅助码」——不告警的话无从知道是谁夺走的。文案照
    /// [`Self::warn_code_char_conflicts`] 的形制，直接给出化解办法而不是只陈述状态。
    ///
    /// ⚠️ 占用方要**逐类点名**：只报「被占用」而不说被谁，用户仍得挨个试。
    pub(crate) fn warn_english_case_cycle_conflict(&self) {
        let rt = self.rt();
        let Some(vk) = rt.english_case_cycle_vk else {
            return;
        };
        let mut owners: Vec<&str> = Vec::new();
        if rt.session_keys.classify(vk, false, true).is_some()
            || rt.schema_session_vks.contains(&vk)
        {
            owners.push("会话动作（keys.session_actions）");
        }
        if rt.jump_out_keys.contains(&vk) {
            owners.push("配对跳出键（input.auto_pair.jump_out_keys）");
        }
        if vk == keymap::VK_CAPITAL {
            owners.push("系统大写锁定");
        }
        if owners.is_empty() {
            return;
        }
        warn!(
            "英文大小写档位循环占用的键（input.english_case_cycle_key = {:?}）同时配作 {}；             英文方案 / 临时英文输入期间、以及快捷输入**高亮英文候选**时，本键归档位循环，那些功能在此期间按不出来。             要保留它们：把 english_case_cycle_key 换成别的键或留空；要保留档位循环：把那些功能改绑到别的键",
            rt.config.input.english_case_cycle_key,
            owners.join(" / ")
        );
    }

    /// 当前方案该用哪把整句切换键：码表方案取 `schema.codetable.sentence_cycle_key`（按方案折叠），
    /// 其余取 `schema.pinyin.sentence_cycle_key`。两边各管各的，见 `CodetableGlobal::sentence_count`。
    pub(crate) fn active_sentence_cycle_key(&self) -> Option<keymap::SessionKey> {
        if self.engine_mgr.current_engine_type() == Some(wind_engine::engine::EngineType::CodeTable)
        {
            // 写错的键名在建配置 / 切方案时已告警过，这里静默按关闭处理，免得每次按键刷日志。
            return self.engine_mgr.with_codetable_settings(|c| {
                let raw = c.sentence_cycle_key.trim();
                (!raw.is_empty())
                    .then(|| keymap::session_key_name_to_vk(raw))
                    .flatten()
            });
        }
        self.rt().sentence_cycle_key
    }

    /// 当前方案的整句块露几条（`place_sentence_block` 用）。规则同 [`Self::active_sentence_cycle_key`]。
    pub(crate) fn active_sentence_count(&self) -> usize {
        if self.engine_mgr.current_engine_type() == Some(wind_engine::engine::EngineType::CodeTable)
        {
            return usize::from(
                self.engine_mgr
                    .with_codetable_settings(|c| c.sentence_count),
            );
        }
        usize::from(self.rt().config.schema.pinyin.sentence_count)
    }

    /// 整句切换键（拼音 / 码表各一把，见 [`Self::active_sentence_cycle_key`]）：在整句池里滚动显示窗口。
    ///
    /// 与英文大小写档位循环同处「候选显示期间生效的快捷键」这一层、同为**临时夺取**：
    /// 守卫任一不成立即返回 `None`，按键原样落回它本来的语义（Tab 出厂是高亮下移键）。
    ///
    /// 守卫：配了键 / 按的就是那个键（含 Shift 位）/ **不是本方案的手动分隔符、也没被本方案
    /// `[key_actions]` 绑定** / 普通拼音输入态（无 overlay 模式）/ 有输入 / **整句池 ≥ 2 条**。
    ///
    /// 分隔符与方案绑定那条是审查查出的：全拼出厂 `separator = "auto"`，`'` 被选词键占着时
    /// 反引号就是音节分隔符；用户在设置页把切换键选成反引号，打到 2 个音节以上（池子才非空）
    /// 分隔符就永远按不进去。这两者在「打拼音中途」有自己的语义，一律让位。最后一条是关键：出厂 `sentence_max_count = 1` 时池子恒空，
    /// 即使用户配了键也不会吃；开了 N-best 但这串输入只解出一种整句时也不吃。
    ///
    /// 切换**不重新转换**：池子是这串输入的完整解码结果，换窗口只是换显示。下一次按键
    /// 触发 `update_candidates` 时池子与窗口一并重置（见 `State::sentence_pool`）。
    pub(crate) fn try_sentence_cycle_key(&self, data: &KeyEventData) -> Option<KeyAction> {
        if data.event_type != EVENT_KEY_DOWN {
            return None;
        }
        // 带 Ctrl/Alt/Cmd 的组合归宿主快捷键，不是本功能。
        if data.modifiers & MOD_SHORTCUT != 0 {
            return None;
        }
        let key = self.active_sentence_cycle_key()?;
        if data.key_code != key.vk || (data.modifiers & MOD_SHIFT != 0) != key.shift {
            return None;
        }
        if !key.shift
            && (self.manual_separator_key(key.vk)
                || self
                    .bound_action_in_schema(key.vk, &self.engine_mgr.active_schema_id())
                    .is_some())
        {
            return None;
        }
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let state: &mut State = &mut guard;
        // 展开态按分类筛选中：`candidates` 是子集，整句窗口按完整列表的位置滚，两者对不上。
        // 筛选时把键让回原语义（多半是单字 / 词组标签里本就没有整句）。
        if state.active.is_some()
            || state.input_buffer.is_empty()
            || state.sentence_pool.len() < 2
            || state.grid_tab != wind_ui_types::GridTab::All
        {
            return None;
        }
        let first = crate::handle_candidate::cycle_sentence_window(
            &mut state.candidates,
            &mut state.sentence_base,
            &state.sentence_pool,
            &mut state.sentence_window,
        )?;
        // 高亮落到窗口首条上：空格上屏的就是用户刚切到的那种解读。
        // 直接写页码而**不经 `turn_page`**：那个会置 `paged`（「这批候选被翻过页」），
        // 而切换整句不是翻页，`-` 键的翻页身份不该因此改变。
        let per_page = self.page_size(state);
        state.current_page = first / per_page;
        state.selected_index = first % per_page;
        // 展开态下页码变了，可见区要跟上，否则高亮行落在可见区外（屏幕上高亮错格）。
        self.grid_follow(state);
        self.clear_hover();
        // 高亮换了 ⇒ 组合区按高亮候选重算（双拼下简拼 / 整句的切法不同，审查查出漏了这步）。
        // 回传写法同方向键移动高亮（`handle_nav_key`）：只有形态真变了、且编码显示在宿主里时
        // 才回传组合串，否则吞键即可。
        let before = state.preedit.clone();
        self.sync_preedit_to_highlight(state);
        let composed = (state.preedit != before && self.preedit_in_app_effective()).then(|| {
            let text = state.preedit.clone();
            let caret_pos = self.reshaped_caret(state);
            KeyAction::UpdateComposition { text, caret_pos }
        });
        self.notify_ui_update(state);
        Some(composed.unwrap_or(KeyAction::Consumed))
    }

    /// 启动体检：整句切换键被别的功能占着 → 告警（文案同 `warn_english_case_cycle_conflict`
    /// 的形制：逐类点名占用方，并给出化解办法）。
    ///
    /// 这不是配置冲突（各自都合法），而是**运行期的优先级夺取**：整句池 ≥ 2 条时那些绑定
    /// 按不出来。不告警的话用户只会看到「Tab 翻页有时好用有时不好用」，无从知道是谁夺走的。
    ///
    /// 拼音与码表两把键各体检一次（码表取全局值；方案里另写的见
    /// [`Self::warn_schema_sentence_cycle_conflict`]）。
    pub(crate) fn warn_sentence_cycle_conflict(&self) {
        let rt = self.rt();
        let p = &rt.config.schema.pinyin;
        let c = &rt.config.schema.codetable;
        let codetable_key = keymap::session_key_name_to_vk(c.sentence_cycle_key.trim());
        for (path, raw, key, counts) in [
            (
                "schema.pinyin.sentence_cycle_key",
                p.sentence_cycle_key.as_str(),
                rt.sentence_cycle_key,
                (p.sentence_count, p.sentence_max_count),
            ),
            (
                "schema.codetable.sentence_cycle_key",
                c.sentence_cycle_key.as_str(),
                codetable_key,
                (c.sentence_count, c.sentence_max_count),
            ),
        ] {
            if let Some(key) = key {
                self.warn_one_sentence_cycle_conflict(path, raw, key, counts);
            }
        }
    }

    /// 码表方案自己写了整句切换键 / 条数（与全局不同）时补一次体检：启动时查活跃方案，
    /// 切方案后查切到的方案。与全局相同的已由 [`Self::warn_sentence_cycle_conflict`] 查过，跳过。
    /// 键名写错也在这里告警（逐键路径静默按关闭处理）。
    pub(crate) fn warn_schema_sentence_cycle_conflict(&self, schema_id: &str) {
        if self.engine_mgr.loaded_engine_type(schema_id)
            != Some(wind_engine::engine::EngineType::CodeTable)
        {
            return;
        }
        let pick = |c: &wind_config::CodetableGlobal| {
            (
                c.sentence_cycle_key.clone(),
                c.sentence_count,
                c.sentence_max_count,
            )
        };
        let own = self.engine_mgr.with_codetable_settings_of(schema_id, pick);
        if own == pick(&self.rt().config.schema.codetable) {
            return;
        }
        let (raw, count, max_count) = own;
        let path = format!("方案 {schema_id} 的 schema.codetable.sentence_cycle_key");
        if let Some(key) = crate::config_bundle::parse_sentence_cycle_key(&path, &raw) {
            self.warn_one_sentence_cycle_conflict(&path, &raw, key, (count, max_count));
        }
    }

    fn warn_one_sentence_cycle_conflict(
        &self,
        path: &str,
        raw: &str,
        key: keymap::SessionKey,
        (count, max_count): (u8, u8),
    ) {
        let rt = self.rt();
        let vk = key.vk;
        // 键配好了、池子却恒空 ⇒ 用户以为开了、实际永远不生效。「键即开关」的初衷就是不要
        // 第二道闸，而算几条整句事实上成了第二道——至少把它说出来（审查查出）。
        if count.max(max_count) < 2 {
            warn!(
                "整句切换键（{path} = {raw:?}）已配置，但同段的 sentence_count 与 \
                 sentence_max_count 都小于 2，整句池恒空、切换键永远不会生效；要用它请把备选条数调到 ≥2"
            );
        }
        let mut owners: Vec<&str> = Vec::new();
        if rt.session_keys.classify(vk, key.shift, true).is_some()
            || (!key.shift && rt.schema_session_vks.contains(&vk))
        {
            owners.push(
                "会话动作 / 高亮 / 翻页（keys.session_actions / highlight_keys / page_keys）",
            );
        }
        if !key.shift && rt.jump_out_keys.contains(&vk) {
            owners.push("配对跳出键（input.auto_pair.jump_out_keys）");
        }
        if !key.shift && rt.english_case_cycle_vk == Some(vk) {
            owners.push("英文大小写档位循环（input.english_case_cycle_key，它先处理、会先截走）");
        }
        // 下两类在夺键时会**让位**（本键不生效），而不是被夺——同样要说出来，否则用户只看到
        // 「配了切换键没反应」。
        if !key.shift && self.manual_separator_key(vk) {
            owners.push("当前方案的音节分隔符（schema.pinyin.separator，本键此时让位给分隔符）");
        }
        if !key.shift
            && self
                .bound_action_in_schema(vk, &self.engine_mgr.active_schema_id())
                .is_some()
        {
            owners.push("当前方案的 [key_actions] 绑定（本键此时让位给方案绑定）");
        }
        if owners.is_empty() {
            return;
        }
        warn!(
            "整句切换键（{path} = {raw:?}）同时配作 {}；\
             整句算出 ≥2 条时本键归整句切换，那些功能在此期间按不出来。\
             要保留它们：把 sentence_cycle_key 换成别的键或留空；要保留整句切换：把那些功能改绑到别的键",
            owners.join(" / ")
        );
    }

    /// 钩子报来的 CapsLock 事件（在专用消费线程执行，可安全加锁）。
    ///
    /// down 照旧走会话动作（含自动重复）；松开时若 CapsLock 是切换键、且这次按下没被会话
    /// 动作用掉，按 [`Self::caps_toggle_key_released`] 处理。
    fn handle_capslock_hook_event(&self, ev: wind_keys::capslock_hook::CapsEvent) {
        use std::sync::atomic::Ordering::Relaxed;
        use wind_keys::capslock_hook::CapsEvent;
        match ev {
            CapsEvent::Down { first } => {
                // 首个 down 先清账：上一次按下的 up 若丢了（钩子被系统摘掉），标志不该带过来。
                if first {
                    self.caps_press_claimed.store(false, Relaxed);
                }
                if self.handle_capslock_hook_press() {
                    self.caps_press_claimed.store(true, Relaxed);
                }
            }
            CapsEvent::Up {
                held_ms,
                interrupted,
            } => {
                let claimed = self.caps_press_claimed.swap(false, Relaxed);
                if claimed || !self.capslock_is_toggle_key() {
                    return;
                }
                // 按住期间按了别的键：同 Shift 的轻敲判定，不算单按，什么都不做。
                if interrupted {
                    debug!("CapsLock 切换键：按住期间按了别的键，不切换");
                    return;
                }
                self.caps_toggle_key_released(held_ms);
            }
        }
    }

    /// CapsLock 作切换键时松开：短按切中英，长按（或大写已开着时）开关大写锁定。
    ///
    /// 开关大写靠注入一次 CapsLock：注入事件钩子放行，系统照常翻锁定态，TSF 收到后走
    /// 服务端 CapsLock 分支的原有处理（正在打的编码按 `commit_on_switch` / `commit_upper`
    /// 上屏），与没勾切换键时按 CapsLock 同一条路。
    ///
    /// 短按也当大写键的两种情形：大写开着（短按是关大写）、焦点在密码框（那里中英切换没有
    /// 意义，用户要的是大写）。
    ///
    /// 注入成功后先乐观翻转大写镜像：TSF 回报要走一个往返，紧接着的短按不能读到旧值。
    fn caps_toggle_key_released(&self, held_ms: u32) {
        let caps_on = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .caps_lock;
        let password = self
            .password_suppress
            .load(std::sync::atomic::Ordering::Relaxed);
        if held_ms >= CAPS_TOGGLE_LONG_PRESS_MS || caps_on || password {
            debug!(
                "CapsLock 切换键：开关大写锁定（按住 {held_ms}ms，大写原为 {caps_on}，密码框 {password}）"
            );
            match wind_keys::key_inject::tap_caps_lock() {
                Ok(()) => {
                    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    s.caps_lock = !caps_on;
                }
                Err(e) => warn!("CapsLock 切换键：注入 CapsLock 失败: {e}"),
            }
        } else {
            debug!("CapsLock 切换键：短按切中英（按住 {held_ms}ms）");
            // 同 `dispatch_hotkey("toggle_mode")`（无按键上下文，编码经 push 上屏），
            // 只多一个 `commit_upper`：CapsLock 切的中英，编码同样转大写上屏。
            let upper = self.rt().config.input.capslock.commit_upper;
            let (_, commit) = self.toggle_mode_with_commit_cased(upper);
            self.push_switch_commit(&commit);
        }
    }

    /// 返回这次按下是否被某个会话动作用掉了。
    fn handle_capslock_hook_press(&self) -> bool {
        // 合成一个 keyup 事件：CapsLock 在键盘路径上本来就只有 keyup 到得了服务端
        // （见 `handle_session_action_key_up`），保持同形以免两条路径的守卫产生差异。
        let data = KeyEventData {
            key_code: keymap::VK_CAPITAL,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_UP,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        };
        // ★ **最先**试大小写档位循环：它是「英文输入态下临时夺取本键」，优先级按定义高于
        // 用户给 CapsLock 配的任何会话动作。守卫（开关 / 英文语境 / 有候选）都在函数内部，
        // 三者有一个不成立就返回 None，键原样落回下面的既有两条路。
        if self.rt().english_case_cycle_vk == Some(keymap::VK_CAPITAL)
            && let Some(act) = self.try_english_case_cycle()
        {
            debug!("CapsLock 钩子：英文候选大小写档位循环");
            let _ = act;
            return true;
        }
        // ★ 必须先试选词出口，顺序与 `message_handler` 的 keyup 分支逐字一致。
        //   `apply_session_action` 对 `select_candidate:N` / `select_char:N` 一律
        //   `return None`（它们带 overflow 语义，要落到各自的既有消费点执行），而键盘路径
        //   上「落到既有消费点」靠的是 keydown 继续往下走——CapsLock **没有可用的 keydown**
        //   （C++ 压根不发），钩子路径更是走完就结束，None 即终点。少了这一行的表现是
        //   `capslock = "select_candidate:3"` 配了完全没反应：既不选词，也因为钩子照吃而
        //   连大写都不翻。2026-08-31 修。
        let action = self
            .handle_select_key_up(&data)
            .or_else(|| self.handle_session_action_key_up(&data))
            .map(|act| self.redispatch_commit_code(&data, act));
        // 候选窗刷新已在各出口内部完成（`notify_ui_update`），此处无须再推。
        match action {
            None => false,
            Some(KeyAction::Consumed) => true,
            Some(act) => {
                // 钩子路径**没有 TSF 按键上下文**可回传——与鼠标点选候选完全同一处境，
                // 故复用它的既有出口 `push_no_key_ctx_action`（`UpdateComposition` / `InsertText`
                // 经 push 通道定向投递，C++ 侧再用合成提交键在按键上下文里落地）。
                //
                // ⚠️ 此处曾只打一行 debug 就把结果**丢弃**，于是选词类动作「引擎状态变了、
                // 词频记了，字却没上屏」——比不生效更难查，因为看起来什么都没发生。
                // 不经按键出口，收尾在这里做（同鼠标点选，见 `crate::session_exit`）。
                let (act, chinese_mode, main_path) = {
                    let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let (chinese_mode, main_path) = (st.chinese_mode, st.active.is_none());
                    let act = self.finish_session_action(
                        st,
                        crate::session_exit::ActionOrigin::CapsLockHook,
                        act,
                    );
                    (act, chinese_mode, main_path)
                };
                debug!("CapsLock 钩子：经 push 通道投递 {:?}", act);
                self.push_no_key_ctx_action(&act, chinese_mode, main_path);
                true
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

    /// 切换前是否有未上屏的编码/候选/独占模式缓冲。
    ///
    /// 判据比中英切换那两处**宽**：多问 `preedit` 与 `active`。独占模式（临拼/混输/
    /// 临英/辅助码）的码不落在 `input_buffer` 里，只看主缓冲会把「临拼态下切换」判成
    /// 无待处理，于是不结束 composition —— 宿主里的引导符和码原样留着。
    pub(crate) fn has_pending_composition(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        !s.input_buffer.is_empty()
            || !s.preedit.is_empty()
            || !s.committed_text.is_empty()
            || !s.candidates.is_empty()
            || s.active.is_some()
    }

    /// 方案切换时对未上屏编码的处置——与中英切换**同一条策略**（`keys.commit_on_switch`：
    /// 开则上屏原码，关则丢弃），这也正是用户要求的「按中英切换那套来」。
    ///
    /// ★ 必须在 `EngineManager` 真正换掉活跃方案**之前**调用：上屏走 `record_commit`，
    /// 而那里的 `schema_id` 取的是**当前**活跃方案。切完再取，这串码会记到新方案头上。
    ///
    /// 传 `chinese = false` 不是「切到英文」的意思——该参数在
    /// [`Self::take_input_on_mode_switch`] 里只参与 `!chinese && commit_on_switch` 这一个
    /// 判断，问的是「这串码还要不要」。切方案与切英文对它的答案相同：编码属于旧方案，
    /// 新方案接不下去。
    ///
    /// ⚠ 取 `State` 锁，必须在**不持锁**时调用（与 `finish_user_schema_switch` 同约束）。
    pub(crate) fn take_input_on_schema_switch(&self) -> SwitchCommit {
        // 切方案 = 一段输入结束，与中英切换同义。须在取 state 锁之前调用（内部走词库 IO）。
        self.terminate_auto_phrase("switch_schema");
        let had_pending = self.has_pending_composition();
        let text = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.take_input_on_mode_switch(&mut state, false)
        };
        SwitchCommit { text, had_pending }
    }

    /// **无按键上下文**的路径（托盘/工具栏菜单、全局热键、命令直通车）把处置结果交给宿主。
    ///
    /// 两条出口对应 `SwitchCommit` 的两个字段：有文本走 `CommitText`（C++ 侧提交时顺带
    /// 结束 composition），无文本但切换前有编码则单独推 `ClearComposition`——push 通道的
    /// `CommitText` 在 C++ `AsyncReader` 里被 `!response.text.empty()` 门控，
    /// **空文本清不掉组合**，与按键路径那条「空文本也能 EndComposition」不同源。
    pub(crate) fn push_switch_commit(&self, commit: &SwitchCommit) {
        if !commit.text.is_empty() {
            self.push_commit_text(&commit.text);
        } else if commit.had_pending {
            self.push_server
                .push_commit_to_active(&wind_ipc::codec::encode_clear_composition());
        }
    }

    /// **按键路径**把处置结果包成本次按键的应答。
    ///
    /// 与中英切换 keyup 分支同构：**空文本也要走 `InsertText`**——C++ 的 `CommitText`
    /// 即便文本为空也会 `EndComposition`，清掉宿主里残留的编码；而 `StatusUpdate` 那条
    /// 路不结束组合，正是「切了方案编码还挂在应用里」的根因。
    /// `mode_changed = true` 让中英图标跟着刷新（切方案会归位中文）。
    pub(crate) fn schema_switch_key_action(&self, commit: SwitchCommit) -> KeyAction {
        if commit.text.is_empty() && !commit.had_pending {
            return KeyAction::StatusUpdate(self.build_status());
        }
        let chinese_mode = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .chinese_mode;
        KeyAction::InsertText {
            text: commit.text,
            new_composition: None,
            mode_changed: true,
            chinese_mode,
            has_new_composition: false,
        }
    }

    /// 切换中英文时取消当前输入：清空缓冲/候选/preedit，并按 `hotkeys.commit_on_switch`
    /// 决定是否把已输入的原始编码上屏（仅在切到英文且有待输入时）。返回待上屏文本。
    ///
    /// ★ **换行改写收口在这里**（A3-3）。本函数产出的文本经 `CMD_TOGGLE_MODE` /
    /// `CMD_SYSTEM_MODE_SWITCH` 的**同步回包**直接上屏（`wind-bridge/src/server.rs` 里
    /// 各自 `encode_commit_text`），既不走 `handle_key_event_policed`，也不在 push 那几条
    /// 路上——三个调用方（切中英 / 系统切换 / CapsLock）都从这里取文本，收在此处一处覆盖。
    /// 漏了它的症状是「同一个词，回车上屏对、切中英上屏不对」。
    ///
    /// ⚠️ 但这一处堵的是**理论缺口，不是能复现的故障**，上面那句是失效形态、不是现场记录，
    /// 别照着它反推「曾经有个 bug」：
    /// 本函数上屏的绝大多数是**原始编码**（普通组合）或英文缓冲原文，两者都不可能带换行。
    /// 唯一带得上换行的是拼音类逐步转换的 `committed_text`（已选中的汉字，见其字段注释）
    /// ——要触发得先在临拼/混输的逐步转换里选中一个带换行的词条，再切中英；而码表（五笔）
    /// 选词消费整串、根本不进那个态。⇒ 修它是为了让四条出口语义一致，不是修现场。
    pub(crate) fn take_input_on_mode_switch(&self, state: &mut State, chinese: bool) -> String {
        self.take_input_on_mode_switch_cased(state, chinese, false)
    }

    /// 同 [`Self::take_input_on_mode_switch`]，`upper` 为真时上屏的原码转大写
    /// （`input.capslock.commit_upper`，打字中按 CapsLock 开大写那一路）。
    /// 只转用户打的码（含字母引导符），已转换前缀 `committed_text` 不动。
    pub(crate) fn take_input_on_mode_switch_cased(
        &self,
        state: &mut State,
        chinese: bool,
        upper: bool,
    ) -> String {
        let text = self.take_input_on_mode_switch_raw(state, chinese, upper);
        self.convert_commit_newline(text)
    }

    /// [`Self::take_input_on_mode_switch_cased`] 的本体，不含换行改写。
    fn take_input_on_mode_switch_raw(
        &self,
        state: &mut State,
        chinese: bool,
        upper: bool,
    ) -> String {
        let cased = |code: String| if upper { code.to_uppercase() } else { code };
        // 快捷加词预览还开着：切中英等于放弃它。不退的话 `add_word_active` 留着，
        // `hotkey_session` 恒为真，DLL 把所有 Ctrl 组合键（Ctrl+-）乃至单键 `-` 都当作
        // 加词会话里的键吞掉，英文模式下宿主快捷键整片失效（GH#254）。
        if state.add_word_active {
            self.exit_add_word_mode(state);
        }
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
                let buf = cased(state.temp_english_buffer.clone());
                let text = if state.full_width {
                    to_full_width(&buf)
                } else {
                    buf
                };
                // 与临拼 / mix / 主路的残留同口径：本次上屏按模式切换来源记统计。
                self.record_commit(
                    &text,
                    state.temp_english_buffer.len() as u32,
                    -1,
                    CommitSource::ModeSwitch,
                );
                text
            } else if let Some((buf, prefix)) = match state.active {
                Some(ModeKind::TempPinyin) => Some((
                    state.temp_pinyin_buffer.clone(),
                    state.temp_pinyin_prefix.clone(),
                )),
                Some(ModeKind::Mix(_)) => {
                    Some((state.mix_buffer.clone(), state.mix_prefix.clone()))
                }
                // 辅助码是唯一**不清空来源缓冲**的独占模式（它只筛候选，拼音码原封不动
                // 留在来源那儿），故上面那句「独占模式下 input_buffer 必为空」对它不成立。
                // 取来源缓冲、按来源给引导前缀——语义与「在来源模式里直接切英文」完全一致，
                // 否则辅助码态下切英文会把待上屏的拼音原码静默丢掉。
                //
                // ⚠️ 「来源」不再恒是主输入路：辅助码现在也能从临拼进入，那时码在
                // `temp_pinyin_buffer`、引导前缀是 `temp_pinyin_prefix`（`z` 要归还）。
                // 判据取 `AuxCodeOverlay::origin`，与 `aux_code_source_buffer` 同源。
                Some(ModeKind::AuxCode) => {
                    Some(match state.aux_code.as_ref().and_then(|o| o.origin) {
                        Some(ModeKind::TempPinyin) => (
                            state.temp_pinyin_buffer.clone(),
                            state.temp_pinyin_prefix.clone(),
                        ),
                        None
                        | Some(
                            ModeKind::TempEnglish
                            | ModeKind::Url
                            | ModeKind::Email
                            | ModeKind::Unicode
                            | ModeKind::Special(_)
                            | ModeKind::RareChar
                            | ModeKind::Reverse
                            | ModeKind::Mix(_)
                            | ModeKind::AuxCode,
                        ) => (
                            preedit_cursor::cased_or_buffer(
                                &state.input_buffer,
                                &state.input_buffer_cased,
                            )
                            .to_string(),
                            String::new(),
                        ),
                    })
                }
                None
                | Some(
                    ModeKind::TempEnglish
                    | ModeKind::Url
                    | ModeKind::Email
                    | ModeKind::Unicode
                    | ModeKind::Special(_)
                    | ModeKind::RareChar
                    | ModeKind::Reverse,
                ) => None,
            } {
                // 临拼 / mix：镜像普通组合的 commit_on_switch，且对齐各自的回车上屏语义。
                let has_pending = !buf.is_empty() || !state.committed_text.is_empty();
                if !chinese && self.rt().config.keys.commit_on_switch {
                    if has_pending {
                        // 有待输入：上屏「引导字母 + 已转换前缀 committed_text + 剩余原码」。
                        // 符号引导符不输出、字母引导符归还，判据见 `guide_to_return`
                        // ——与临拼/mix 的回车上屏共用同一条，三处必须同进同出。
                        // committed 段已在选词时记过，此处只记本次实际上屏的原码（来源模式切换）。
                        let guide = cased(Self::guide_to_return(&prefix, &state.committed_text));
                        let buf = cased(buf);
                        let code = format!("{}{}", guide, buf);
                        self.record_commit(&code, code.len() as u32, -1, CommitSource::ModeSwitch);
                        let raw = format!("{}{}{}", guide, state.committed_text, buf);
                        let out = self.maybe_convert(state, &raw);
                        // 快捷输入含英文成员时对齐临英（A2-3b）：全角态转全角，与 mix 回车
                        // 同一判据（`mix_raw_counts_as_english`，数字透镜除外）。
                        if matches!(state.active, Some(ModeKind::Mix(_)))
                            && self.mix_raw_counts_as_english(state)
                        {
                            Self::mix_english_width(state, &out)
                        } else {
                            out
                        }
                    } else if !prefix.is_empty() && !self.enter_clears_composition() {
                        // 只按了模式进入符（缓冲空）：原样上屏该前缀符号本身，与回车空缓冲上屏一致
                        // （enter_behavior=clear 时回车也不上屏，故一并放弃）。
                        let prefix = cased(prefix);
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
            let raw_code = cased(
                preedit_cursor::cased_or_buffer(&state.input_buffer, &state.input_buffer_cased)
                    .to_string(),
            );
            // 模式切换上屏：committed 段已在选词时记过，此处只记剩余原码（来源模式切换）。
            self.record_commit(
                &raw_code,
                raw_code.len() as u32,
                -1,
                CommitSource::ModeSwitch,
            );
            self.maybe_convert(state, &format!("{}{}", prefix, raw_code))
        } else {
            String::new()
        };
        state.committed_text.clear();
        state.committed_segs.clear();
        state.input_buffer.clear();
        state.input_buffer_cased.clear();
        // 组合随切换结束，检索放宽一并复原（独占分支经 `reset_exclusive_modes` 复原）。托盘 / 热键 /
        // 按应用切方案走推送上屏、不经出口的 `expire_scope_override`，不在这里复原就会带进新方案。
        state.scope_relaxed = false;
        state.english_case_variant = crate::english_candidates::CaseVariant::default();
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
        // 绝大多数路径的击键数就是码长（打 n 键、选一次词）。「一键出一串」的路径
        // 码长恰恰是 0，必须走 `record_commit_ks` 显式传 1，否则速度封顶盖不住它们。
        self.record_commit_ks(text, code_len, code_len, candidate_pos, source);
    }

    /// 同 [`Self::record_commit`]，但显式给出击键数（仅影响速度分子封顶，见
    /// `wind_store::stats::speed_chars_of`）。实际字数统计与本参数无关。
    pub(crate) fn record_commit_ks(
        &self,
        text: &str,
        code_len: u32,
        keystrokes: u32,
        candidate_pos: i32,
        source: CommitSource,
    ) {
        if text.is_empty() {
            return;
        }
        // 下一词学习**默认断开**：任何上屏（标点、原码、顶码、满码自动上屏…）都清上文。
        // 只有 `commit_selected` 记账前先暂存上文、记账后再学习，见那里。
        self.note_word_boundary();
        if let Some(log) = self
            .debug_stat_events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            log.push((source, text.to_string()));
        }
        let collector = match self.stat_collector.as_ref() {
            Some(c) => c,
            None => return,
        };
        if !self.rt().config.stats.enabled {
            return;
        }
        // 暂停学习：逐日字数与速度同样是「这次输入留下的痕迹」。放在断点与调试捕获之后——
        // 断点照打，调试捕获只观测「本该记哪一笔」。`stat_recorded` 照常置位，与正常记账
        // 同一形状，顶层 `record_input_stats` 才不会把这次当成漏记、再走一遍兜底。
        if self.learning_suspended() {
            self.stat_recorded
                .store(true, std::sync::atomic::Ordering::Relaxed);
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
            keystrokes,
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
            // 不进统计但同样动了宿主正文、又没经 `record_commit` 的出口：下一词学习的断点
            KeyAction::ReplaceBackward { .. }
            | KeyAction::HoldComposition { .. }
            | KeyAction::CommitReplacingHeld { .. }
            | KeyAction::CommitAndHoldComposition { .. }
            | KeyAction::CommitThenDeferComposition { .. }
            | KeyAction::MoveCursorRight { .. }
            | KeyAction::DeletePair => {
                self.note_word_boundary();
                return;
            }
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
    /// 用户文本重新载入后，按变了的项重建内存镜像：短语层、快捷格式调整、常用字覆盖、字符集
    /// 注册表。用户词与 Shadow 由引擎 / 按键路径直接读表，不用重建。
    pub(crate) fn apply_user_text_changes(&self, changed: &[String]) {
        let has = |k: &str| changed.iter().any(|c| c == k);
        if has("phrases") {
            self.rebuild_phrases();
        }
        if has("quick_format") {
            self.reload_quick_adjust();
        }
        if has("common_chars") {
            self.reload_common_chars();
        }
        if has("charsets") {
            self.reload_charsets();
        }
        for c in changed {
            if let Some(ds) = c.strip_prefix(wind_store::user_words::USER_DICTS_CHANGED) {
                self.engine_mgr.invalidate_user_dicts(ds);
            }
        }
    }

    pub(crate) fn rebuild_phrases(&self) {
        let recs: Vec<wind_phrase::PhraseSeed> = match self.store.as_ref() {
            Some(store) => store
                .enabled_phrases_for_input()
                .unwrap_or_default()
                .into_iter()
                .map(|p| wind_phrase::PhraseSeed {
                    code: p.code,
                    text: p.text,
                    weight: p.weight,
                    position: p.position,
                    is_system: p.is_system,
                    category: p.category,
                })
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
                category: e.category.clone(),
                enabled: e.enabled,
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
                category: e.category.clone(),
                enabled: e.enabled,
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
        e.enabled.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// 悬停提示（与注释段）渲染的整页上下文：只依赖状态与配置、对一页候选相同的那些量，
/// 候选循环外解析一次，循环里逐候选 [`Coordinator::render_candidate_tooltip`]。
///
/// 桌面候选页组装与移动端按绝对下标取提示（[`Coordinator::candidate_tooltip_at`]）共用这一份——
/// 两边各解析一遍就是两个真相源，漂移后的表现是「同一个候选，电脑上气泡有编码段、手机上没有」。
pub(crate) struct TooltipEvalCtx {
    /// 调试提示上下文：仅有段引用 `${debug}` 时解析（mixed 归属 / 方案 id）。
    dbg_ctx: Option<DebugSchemaCtx>,
    /// `${word_code}` / `${code_source}` 的来源方案；没有段引用它们就是 `None`。
    code_schema: Option<String>,
    code_source_name: Option<String>,
    /// 快捷输入（mix）下注释库归属逐候选取，见 [`Coordinator::comment_dict_scope`]。
    pub(crate) mix_comment_scope: bool,
    /// 注释**库**的 `schemas` 白名单作用域（非 mix 时整页共用）。
    pub(crate) comment_dict_schema: String,
}

impl Coordinator {
    pub(crate) fn tooltip_eval_ctx(
        &self,
        state: &State,
        tip: &crate::tooltip::CompiledTooltip,
    ) -> TooltipEvalCtx {
        let force_hint = Self::forces_code_hint(state);
        let dbg_ctx = if tip.references("debug") {
            // 归属与读写两端同源（`effective_data_schema`）：特殊模式下若这里仍按 active 解析，
            // 调试段显示的计数与排序实际用的不是同一个 key——排查时会被它带偏，
            // 而这正是最难察觉的一种不一致。
            Some(self.build_debug_schema_ctx(self.effective_data_schema(state).as_deref()))
        } else {
            None
        };
        // 注释**库**的 `schemas` 白名单作用域：与词频/短语同源取 `effective_data_schema`
        // （临英归 english 桶）。⚠️ 与上面模板层取 active 刻意不同——库是数据、模板是呈现。
        let comment_dict_schema = self
            .effective_data_schema(state)
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        // ⚠️ 快捷输入（mix）下这一份**不够**：它把多个成员方案的候选合进同一张列表，
        // 而 `effective_data_schema` 对 `Mix(_)` 返回 `None` ⇒ 整列表都按主方案（五笔等）
        // 求白名单，于是英文成员的候选查不到白名单写着 `english` 的注释库——同一个单词，
        // 英文方案/临英下有释义，快捷输入里没有（论坛 #153）。
        //
        // 归属逐候选取 `mix_candidate_owner`，与词频/加词**同一个**函数：注释库是数据类
        // 资源，归属与词频/短语同源（见 `comment.rs` 里 `dict` 变量那段）。另写一份
        // 「哪个候选算哪个成员」的判据就是第二个真相源，漂移后的表现是「词频记进 A 桶、
        // 注释查的是 B 桶」这类只在多成员配置下才现形的错配。
        let mix_comment_scope = matches!(state.active, Some(ModeKind::Mix(_)));
        // `${word_code}` / `${code_source}` 的来源方案（循环外解析一次，没有段引用就不碰）：
        // 码表方案=自身全部编码（码长升序 a/ab/abc）、混输=其主码表成员、拼音=全局主码表。
        // 编码按词查方案词库反查索引（word_codes_display），不按取码规则生成；含用户层：
        // 自己造的词也显示编码（text-code-lookup.md）。候选并非用该编码方案
        // 直接输入时（来源方案≠活跃方案，或处于临时拼音/快捷输入反查模式）`${code_source}`
        // 才有值，出厂段名据此显示为 [编码(五笔)]。
        let code_schema = (tip.references("word_code") || tip.references("code_source"))
            .then(|| self.engine_mgr.code_source_schema())
            .filter(|s| !s.is_empty());
        // 反查索引没就绪就**在后台建**，本次先不显示编码段（绝不在此等）。
        // 索引对超大词库是秒级构建，而这里是按键处理链路、且正持有 state 锁——
        // 真机上曾因此让整机卡死 29.5 秒。
        if let Some(sid) = code_schema.as_deref() {
            self.ensure_reverse_index_async(sid);
        }
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
        TooltipEvalCtx {
            dbg_ctx,
            code_schema,
            code_source_name,
            mix_comment_scope,
            comment_dict_schema,
        }
    }

    /// 渲染一个候选的悬停提示（`disp` / `full` 见 [`crate::tooltip::CompiledTooltip::render`]）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_candidate_tooltip(
        &self,
        state: &State,
        c: &Candidate,
        disp: &str,
        full: &str,
        tip: &crate::tooltip::CompiledTooltip,
        ctx: &TooltipEvalCtx,
        reverse: &wind_reverse::ReverseLookup,
    ) -> crate::tooltip::RenderedTooltip {
        // 悬停提示按段列表渲染（见 `crate::tooltip`）。逐字段遍历**截断后**的显示文本、
        // 且不含截断追加的 `…`：超长候选（如长短语）逐字展开会撑爆气泡，只提示实际
        // 显示出的字；被截掉的部分由「完整原文」段（`${full_text}`）整段给出。
        // `${word_code}` 则按候选**完整原文**查词库（截断/繁化文本词库里没有；
        // 查不到=空，段随之消失）。
        //
        // ⚠️ 曾改成按显示文本（`full`）查编码，动机是「气泡三段应同属一个域」——已回退。
        // 拼音段/拆字段吃显示文本是**它们**的事（拆字库覆盖繁体字，查得到），而编码段
        // 回答的是「这个候选怎么打出来」，用户实际敲的就是内部文本那个码；改成查繁化
        // 文本只会让它查不到而整段消失，是拿一个**已经正确**的段去换取形式上的一致。
        // 想再动它之前，先拿出一个真实的错例（2026-09-08 复核，没有找到）。
        // `word_codes_display` 返回 None＝**系统层反查索引尚未就绪**（区别于「查不到」的
        // Some("")）。此时本段不显示，并已在 [`Self::tooltip_eval_ctx`] 里触发后台构建，建好后自动补上。
        let word_code = ctx
            .code_schema
            .as_deref()
            .and_then(|sid| {
                #[cfg(test)]
                WORD_CODE_LOOKUPS.with(|n| n.set(n.get() + 1));
                self.engine_mgr.word_codes_display(sid, &c.text)
            })
            .unwrap_or_default();
        let dict_schema =
            self.comment_dict_scope(state, c, ctx.mix_comment_scope, &ctx.comment_dict_schema);
        // 调试正文要点查 redb 词频，按需算且一个候选只算一次。
        let debug_body = std::cell::OnceCell::new();
        let cand_eval = |name: &str, arg: Option<&str>| -> Option<VarValue> {
            Some(VarValue::from(match name {
                "word_code" => word_code.clone(),
                "code_source" => ctx.code_source_name.clone().unwrap_or_default(),
                "debug" => ctx
                    .dbg_ctx
                    .as_ref()
                    .map(|dbg| {
                        debug_body
                            .get_or_init(|| self.debug_tooltip_body(c, &state.input_buffer, dbg))
                            .clone()
                    })
                    .unwrap_or_default(),
                _ => {
                    return self.eval_var(name, arg, c, reverse, &dict_schema);
                }
            }))
        };
        let char_eval = |ch: char, name: &str, arg: Option<&str>| {
            crate::tooltip::char_var(name, arg, ch, reverse)
                .map(VarValue::from)
                .or_else(|| self.eval_text_var(name, arg, &ch.to_string(), reverse))
        };
        tip.render(disp, full, &cand_eval, &char_eval)
    }
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
    /// 调试段的「来源」行：词库来源 + **整句标记**。
    ///
    /// 整句标记单列一层、不并进 `match c.source`：整句不是第七种来源，而是正交于来源的
    /// 一个身份——拼音、码表（`codetable/sentence.rs`）两侧都产整句，而同一条整句还可能
    /// 是「词典里本就有的词被补标了整句身份」（同文合并，见 `Candidate::is_synthesized`
    /// 的文档）。把它揉进 `match` 就要在每个分支重复一遍。
    ///
    /// 三个标志各自回答一个不同的问题，调试时都需要：
    /// - `is_sentence` —— 它是不是「对整串输入的最优解读」；
    /// - `is_synthesized` —— 是引擎新拼出来的，还是词库本就有这个词条；
    /// - `is_sentence_demoted` —— 是否已让位于精确整词（还在列表里，只是不占首位）。
    ///
    /// **验证 S2（用户词进整句词图）时这行是主要抓手**：整句只出一条、赢者通吃，
    /// 光看候选文本分不清「用户词赢了」和「系统词本来就长这样」。
    fn debug_source_label(&self, c: &Candidate, ctx: &DebugSchemaCtx) -> String {
        let base = self.debug_source_base(c, ctx);
        // 逆切分候选的 `code` 是整串、来源是码表，不标出来就和词库词条长得一模一样。
        let tag = sentence_debug_tag(c).or_else(|| c.is_split_composed.then(|| "逆切分".into()));
        match tag {
            Some(tag) => format!("{base} · {tag}"),
            None => base,
        }
    }

    /// 词库来源本身（不含整句标记），见 [`Self::debug_source_label`]。
    fn debug_source_base(&self, c: &Candidate, ctx: &DebugSchemaCtx) -> String {
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
            .get_freq(&sid, &code, c.freq_text())
            .ok()
            .flatten()
            .map(|r| r.count)
            .unwrap_or(0)
    }

    /// 候选调试信息（悬停提示 `${debug}` 的值）：来源行 + 合并的（编码/权重/序/词频/标记）行。
    /// 来源区分系统/用户短语、用户/临时词库、码表(方案)、拼音、英文。`[调试]` 标题归段名。
    fn debug_tooltip_body(&self, c: &Candidate, input_code: &str, ctx: &DebugSchemaCtx) -> String {
        let source = self.debug_source_label(c, ctx);
        let count = self.debug_freq_count(c, input_code, ctx);
        let mut parts: Vec<String> = Vec::new();
        if !c.code.is_empty() {
            parts.push(format!("码 {}", c.code));
        }
        // 权重后标出**贡献层**：跨词库同词合并时 weight 取各启用层的最大值，而 code 仍属
        // 首个出现层，光看数字分不清它出自哪本词库。少了这一标注，「扩展词库权重没生效」
        // 与「生效了但被别的排序维度盖过」在界面上长得一模一样。
        parts.push(match &c.meta.weight_layer {
            Some(layer) => format!("权 {} ←{layer}", c.weight),
            None => format!("权 {}", c.weight),
        });
        parts.push(format!("序 {}", c.natural_order));
        parts.push(format!("用 {count}次"));
        if c.has_shadow {
            parts.push("✎已调整".to_string());
        }
        format!("来源: {source}\n{}", parts.join(" · "))
    }
}

/// 候选的整句身份标记，供调试段的「来源」行使用（`None` = 不是整句）。
///
/// 抽成自由函数是为了可单测：整个调试段要一个活的 `Coordinator` 才跑得起来，
/// 而这段判据是纯的——它只问候选身上那三个布尔。
///
/// 三个标志各自回答一个不同的问题：
/// - `is_sentence` —— 它是不是「对整串输入的最优解读」；
/// - `is_synthesized` —— 引擎新拼出来的，还是词库本就有这个词条（同文合并会给后者
///   补上整句身份，见该字段文档）；
/// - `is_sentence_demoted` —— 是否已让位于精确整词（还在列表里，只是不占首位）。
///
/// 另带 N-best 名次 `sentence_rank`：**只在 ≥ 2 时**写成 `整句#2(…)`。最优解（名次 1）与
/// 出厂单条整句不标号，出厂下的标记因此与 N-best 之前一字不差。
fn sentence_debug_tag(c: &Candidate) -> Option<String> {
    if !c.is_sentence {
        return None;
    }
    let kind = if c.is_synthesized { "合成" } else { "词典" };
    let demoted = if c.is_sentence_demoted {
        "·已降位"
    } else {
        ""
    };
    let rank = if c.sentence_rank >= 2 {
        format!("#{}", c.sentence_rank)
    } else {
        String::new()
    };
    Some(format!("整句{rank}({kind}{demoted})"))
}

#[cfg(test)]
mod sentence_debug_tag_tests {
    //! 整句标记是验证 S2（用户词进整句词图）的主要抓手：整句只出一条、赢者通吃，
    //! 光看候选文本分不清「用户词赢了」与「系统词本来就长这样」。

    use super::*;

    fn cand(f: impl FnOnce(&mut Candidate)) -> Candidate {
        let mut c = Candidate {
            text: "有盖伦吗".into(),
            ..Default::default()
        };
        f(&mut c);
        c
    }

    #[test]
    fn non_sentence_has_no_tag() {
        assert_eq!(sentence_debug_tag(&cand(|_| {})), None);
        // 只有 is_synthesized 而没有 is_sentence 不构成整句（理论上不该出现，
        // 但标记判据必须以 is_sentence 为准，不能靠另两个反推）。
        assert_eq!(sentence_debug_tag(&cand(|c| c.is_synthesized = true)), None);
    }

    #[test]
    fn synthesized_and_dictionary_sentences_are_distinguishable() {
        // 引擎新拼出来的：词库里没有「有盖伦吗」这个词条。
        assert_eq!(
            sentence_debug_tag(&cand(|c| {
                c.is_sentence = true;
                c.is_synthesized = true;
            }))
            .as_deref(),
            Some("整句(合成)")
        );
        // 同文合并：词库本就有这个词，被补标了整句身份。
        assert_eq!(
            sentence_debug_tag(&cand(|c| c.is_sentence = true)).as_deref(),
            Some("整句(词典)")
        );
    }

    #[test]
    fn demotion_is_visible() {
        assert_eq!(
            sentence_debug_tag(&cand(|c| {
                c.is_sentence = true;
                c.is_synthesized = true;
                c.is_sentence_demoted = true;
            }))
            .as_deref(),
            Some("整句(合成·已降位)"),
            "降位是排序决策、不清 is_sentence，调试时必须看得见"
        );
    }

    #[test]
    fn nbest_rank_shows_only_for_alternatives() {
        let tag = |rank: u8| {
            sentence_debug_tag(&cand(|c| {
                c.is_sentence = true;
                c.is_synthesized = true;
                c.sentence_rank = rank;
            }))
        };
        // 名次 1 = 最优解，与出厂单条整句同一标记。
        assert_eq!(tag(1).as_deref(), Some("整句(合成)"));
        assert_eq!(tag(3).as_deref(), Some("整句#3(合成)"));
    }
}

#[cfg(test)]
#[path = "coordinator_mode_comment_e2e_tests.rs"]
mod mode_comment_e2e_tests;

#[cfg(test)]
#[path = "coordinator_engine_reload_needed_tests.rs"]
mod engine_reload_needed_tests;

#[cfg(test)]
#[path = "coordinator_caret_compat_tests.rs"]
mod caret_compat_tests;

#[cfg(test)]
#[path = "coordinator_initial_mode_tests.rs"]
mod initial_mode_tests;

#[cfg(test)]
#[path = "coordinator_capslock_tests.rs"]
mod capslock_tests;

#[cfg(test)]
#[path = "coordinator_focus_ownership_tests.rs"]
mod focus_ownership_tests;

#[cfg(test)]
#[path = "coordinator_per_app_compat_tests.rs"]
mod per_app_compat_tests;

#[cfg(test)]
#[path = "coordinator_input_diag_tests.rs"]
mod input_diag_tests;

// ─────────────────────────────────────────────────────────────────────────────
// 注释词库：配置里的挂载表 × 词库自带 comment 列的自动派生项
// ─────────────────────────────────────────────────────────────────────────────

/// `sync_emoji_dict` 的变更判据：`(enabled, categories, 开着的附加模块 id)`。
pub(crate) type EmojiSpec = (bool, bool, Vec<String>);

/// 已挂载的 emoji 表，按查询合并顺序（见 `Coordinator::load_emoji_dict`）。
pub(crate) struct EmojiTables(pub(crate) Vec<std::sync::Arc<wind_dict::emojidict::EmojiReader>>);

impl EmojiTables {
    /// 各表命中的值按表序拼成一串（空格分隔）。表间的重复（同一符号、或带 / 不带 U+FE0F）
    /// 不在这里去：消费端本就要按 `emoji_dedup_key` 去重并截断到 `max_per_word`，在这里
    /// 再去一遍只是多分配。
    pub(crate) fn lookup(&self, text: &str) -> Option<String> {
        let parts: Vec<&str> = self.0.iter().filter_map(|r| r.lookup(text)).collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// 一个自动派生出的注释源：某个方案声明的词库自己带了 `comment` 列。
#[derive(Debug, Clone, PartialEq)]
pub struct AutoCommentSource {
    /// 稳定 id（`auto:<rel>`）。配置里的引用空壳靠它认领本条。
    pub id: String,
    /// 词库在方案里的相对路径，供设置页显示与排错。
    pub rel: String,
    /// 显示名（方案里的 `label`，空则回落 `rel`）。
    pub label: String,
    /// 解析后的绝对路径。
    pub path: std::path::PathBuf,
    /// 声明了这份词库的方案 id —— 直接就是它的适用方案白名单。
    ///
    /// 自动项的白名单**不可编辑**，因为它不是一个偏好而是一个事实：这份注释随词库分发，
    /// 拿到别的方案下去查纯属浪费（还查不到）。用户想跨方案共用，正确做法是把它当独立
    /// 注释库手工挂一条。
    pub schemas: Vec<String>,
}

/// 自动项的稳定 id。带 `auto:` 前缀是为了与用户手写的 id 分处两个命名空间 ——
/// 手工项与自动项同 id 时，认领逻辑靠 `auto` 标志位区分而不靠 id，但日志里两条同名
/// 记录足够让人查半天。
pub(crate) fn auto_comment_id(rel: &str) -> String {
    format!("auto:{}", rel.replace('\\', "/"))
}

/// 合并后的一条挂载项。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EffectiveCommentDict {
    pub id: String,
    pub enabled: bool,
    pub schemas: Vec<String>,
    pub kind: EffectiveCommentKind,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EffectiveCommentKind {
    /// 用户手工挂的库，`path` 相对 `schemas/`，尚待解析。
    Manual { path: String },
    /// 自动派生项，`path` 已是绝对路径。
    Auto { path: std::path::PathBuf },
    /// 随包内置项，`path` 相对 `schemas/`，缺文件时只告警一次。
    Builtin { path: String },
}

impl EffectiveCommentDict {
    /// 供日志用的路径串（两种形态各自的原样）。
    pub fn display_path(&self) -> String {
        match &self.kind {
            EffectiveCommentKind::Manual { path } | EffectiveCommentKind::Builtin { path } => {
                path.clone()
            }
            EffectiveCommentKind::Auto { path } => path.display().to_string(),
        }
    }
}

/// 把最终挂载序列解析成 `(路径, 方案白名单)` 与 `${dict:id}` 用的 id 表；顺序即优先级。
/// 只被内置项指到的库：裸 `${dict}` / `${dict_all}` 不查它们，只能 `${dict:id}` 取。
/// 用户另挂了同一个文件（id 不同）时，那个库就归用户，照常参与裸查。
pub(crate) fn named_only_mounts(
    specs: &[EffectiveCommentDict],
    ids: &[(std::path::PathBuf, Vec<String>)],
) -> Vec<std::path::PathBuf> {
    let is_builtin = |id: &str| {
        specs
            .iter()
            .any(|e| e.id == id && matches!(e.kind, EffectiveCommentKind::Builtin { .. }))
    };
    ids.iter()
        .filter(|(_, v)| !v.is_empty() && v.iter().all(|i| is_builtin(i)))
        .map(|(p, _)| p.clone())
        .collect()
}

/// 词库路径 → 指到它的 id 列表。
type PathIds = Vec<(std::path::PathBuf, Vec<String>)>;

pub(crate) fn resolve_comment_mounts(
    specs: &[EffectiveCommentDict],
    data_dir: Option<&Path>,
) -> (PathIds, PathIds) {
    let mut paths: Vec<(std::path::PathBuf, Vec<String>)> = Vec::new();
    // `${dict:id}` 用：每个库被哪些 id 指到（去重合并的那条的 id 也算它的）。
    let mut ids: Vec<(std::path::PathBuf, Vec<String>)> = Vec::new();
    for eff in specs.iter().filter(|e| e.enabled) {
        // 自动项的路径已经是解析好的绝对路径（检测时就得打开文件），手工项才需要按
        // `schemas/` 逐层解析。两者到这里汇成同一条流水线。
        let resolved = match &eff.kind {
            EffectiveCommentKind::Auto { path } => Some(path.clone()),
            EffectiveCommentKind::Manual { path } if path.is_empty() => continue,
            EffectiveCommentKind::Manual { path } => {
                Config::resolve_schema_resource(data_dir, path)
            }
            EffectiveCommentKind::Builtin { path } => {
                let r = Config::resolve_schema_resource(data_dir, path);
                if r.is_none() {
                    warn_builtin_comment_missing(&eff.id, path);
                }
                r
            }
        };
        let s = eff;
        match resolved {
            // 按**解析后路径**去重：两条 spec 写不同的相对路径却指向同一个文件时
            // （`a.dict.yaml` 与 `./a.dict.yaml`，或用户目录与安装目录同名文件都被
            // 解析到同一处），只加载一次。重复加载除了浪费解析时间，还会让优先级
            // 判定变得依赖「第几次出现」——去重后靠前那条恒胜出。
            Some(p) if paths.iter().any(|(q, _)| *q == p) => {
                info!("注释词库重复挂载，已跳过: {} (id={})", p.display(), s.id);
                if let Some((_, v)) = ids.iter_mut().find(|(q, _)| *q == p) {
                    v.push(s.id.clone());
                }
            }
            Some(p) => {
                ids.push((p.clone(), vec![s.id.clone()]));
                paths.push((p, s.schemas.clone()));
            }
            // 只 warn 不中断：一个库路径写错不该让其余库一起不加载。
            None if matches!(s.kind, EffectiveCommentKind::Builtin { .. }) => {}
            None => warn!(
                "注释词库不存在（用户/安装目录均未找到）: {} (id={})",
                s.display_path(),
                s.id
            ),
        }
    }
    (paths, ids)
}

/// 内置注释词库缺文件：每个 id 进程内只告警一次（sync 在每次配置重载都会跑）。
fn warn_builtin_comment_missing(id: &str, path: &str) {
    static WARNED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut w = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if !w.iter().any(|x| x == id) {
        w.push(id.to_string());
        warn!("内置注释词库缺失，已跳过: {path} (id={id})");
    }
}

fn builtin_effective(
    b: &wind_config::comment_lib::BuiltinCommentDict,
    enabled: bool,
) -> EffectiveCommentDict {
    EffectiveCommentDict {
        id: b.id.clone(),
        enabled,
        schemas: Vec::new(),
        kind: EffectiveCommentKind::Builtin {
            path: b.path.clone(),
        },
    }
}

/// 把配置里的挂载表与自动检测结果合成**最终挂载序列**（顺序即优先级）。
///
/// - 配置里 `auto = true` 的条目是**引用空壳**：按 id 去 `auto` 里认领，认领到就用检测出
///   的 `path`/`schemas`，只保留用户的 `enabled` 与位置；认领不到则**静默丢弃**（词库被删、
///   被停用、或总开关关了——都是常态，不是配置错误）。
/// - `builtin = true` 的条目同理，按 id 认领随包内置库（`builtin`），认领不到静默丢弃。
/// - 其余条目原样保留为手工项。
/// - 没被认领的内置项默认启用，追加在用户项之后；再其后是没被认领的 `auto` 项（同样默认启用）。
///
/// # 为什么新自动项默认排末尾
///
/// 用户显式挂的库表达的是明确意图，自动项是「顺手给的」。装了个新方案就把用户排好的第一
/// 优先级挤到后面去，是这类「自动」功能最招人烦的地方。想让它靠前，拖一下即可 —— 拖过
/// 之后它就有了空壳，位置从此归用户所有。
pub(crate) fn merge_comment_dicts(
    configured: &[wind_config::CommentDictSpec],
    builtin: &[wind_config::comment_lib::BuiltinCommentDict],
    auto: &[AutoCommentSource],
) -> Vec<EffectiveCommentDict> {
    let mut out: Vec<EffectiveCommentDict> = Vec::new();
    let mut claimed: Vec<&str> = Vec::new();
    for spec in configured {
        if spec.builtin {
            let Some(b) = builtin.iter().find(|b| b.id == spec.id) else {
                continue;
            };
            claimed.push(b.id.as_str());
            out.push(builtin_effective(b, spec.enabled));
            continue;
        }
        if spec.auto {
            let Some(a) = auto.iter().find(|a| a.id == spec.id) else {
                continue;
            };
            claimed.push(a.id.as_str());
            out.push(EffectiveCommentDict {
                id: a.id.clone(),
                enabled: spec.enabled,
                schemas: a.schemas.clone(),
                kind: EffectiveCommentKind::Auto {
                    path: a.path.clone(),
                },
            });
            continue;
        }
        // 用户自己写的同 id 条目胜出，内置项不再重复出现。
        claimed.push(spec.id.as_str());
        out.push(EffectiveCommentDict {
            id: spec.id.clone(),
            enabled: spec.enabled,
            schemas: spec.schemas.clone(),
            kind: EffectiveCommentKind::Manual {
                path: spec.path.clone(),
            },
        });
    }
    for b in builtin.iter().filter(|b| !claimed.contains(&b.id.as_str())) {
        out.push(builtin_effective(b, true));
    }
    for a in auto.iter().filter(|a| !claimed.contains(&a.id.as_str())) {
        out.push(EffectiveCommentDict {
            id: a.id.clone(),
            enabled: true,
            schemas: a.schemas.clone(),
            kind: EffectiveCommentKind::Auto {
                path: a.path.clone(),
            },
        });
    }
    out
}

#[cfg(test)]
#[path = "coordinator_comment_dict_merge_tests.rs"]
mod comment_dict_merge_tests;

#[cfg(test)]
#[path = "coordinator_hover_reset_tests.rs"]
mod hover_reset_tests;

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
        assert!(
            !Coordinator::caret_is_valid(i32::MIN, 500, 20),
            "未初始化极值必须安全判无效，不得在 abs() 溢出"
        );
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

/// `[ui.labels]` → 图标主字的产地测试。
///
/// 这些断言覆盖的是 `mode_icon_label` 这个**单点**；语言栏图标、工具栏、状态气泡
/// 三个消费面都从它取值（气泡共享取值、不共享分支结构），故不必逐面重复断言。
#[cfg(test)]
mod mode_icon_label_tests {
    use crate::coordinator::Coordinator;
    use std::sync::Arc;
    use wind_config::Config;

    fn coord_with(english: &str, caps: &str) -> Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.ui.labels.english = english.to_string();
        cfg.ui.labels.caps_lock = caps.to_string();
        Coordinator::new_headless(cfg, None)
    }

    /// 配了就用配的。
    #[test]
    fn configured_labels_take_effect() {
        let c = coord_with("En", "Cp");
        assert_eq!(c.mode_icon_label(false, false), "En", "英文半角");
        assert_eq!(c.mode_icon_label(false, true), "Cp", "大写锁定");
        assert_eq!(
            c.mode_icon_label(true, true),
            "Cp",
            "中文 + 大写锁定：大写锁定优先（对齐 effective_chinese）"
        );
    }

    /// 留空回落内置默认，**不是回落空**——空标签会让语言栏图标画出一个没有主字的
    /// 空白方块，用户既看不出模式也无从理解。
    #[test]
    fn empty_falls_back_to_builtin() {
        let c = coord_with("", "");
        assert_eq!(c.mode_icon_label(false, false), "英");
        assert_eq!(c.mode_icon_label(false, true), "A");
    }

    /// 出厂配置必须与本次改造前的字面量逐字一致：老用户升级后不该看到任何变化。
    #[test]
    fn default_config_preserves_legacy_labels() {
        let c = Coordinator::new_headless(Config::default(), None);
        assert_eq!(c.mode_icon_label(false, false), "英");
        assert_eq!(c.mode_icon_label(false, true), "A");
    }

    /// ★ 反向对照：英文标签**不得**渗进中文态。
    ///
    /// 缺了这条，一个"所有分支都返回 english_label"的错误实现能让上面的断言全绿，
    /// 而它在真机上的表现是打中文时图标也显 `En`。无方案数据时中文态回落「中」，
    /// 正好不需要任何 fixture 就能把这个方向钉死。
    #[test]
    fn english_label_does_not_leak_into_chinese() {
        let c = coord_with("En", "Cp");
        assert_eq!(
            c.mode_icon_label(true, false),
            "中",
            "中文态取方案侧 icon_label，与 [ui.labels] 无关"
        );
    }

    /// 超长配置在这一层就被截断。
    ///
    /// ⚠️ 这是 C++ 侧 `_inputTypeLabel` 缓冲的**第一道**防线（第二道是那边的
    /// `_TRUNCATE`）。这条断言挂了意味着超长标签能一路走到 `wcsncpy_s`，
    /// 别把它当成"显示不好看"的测试。
    #[test]
    fn overlong_label_truncated_before_leaving_coordinator() {
        let c = coord_with("English", "CapsLock");
        assert_eq!(c.mode_icon_label(false, false), "En");
        assert_eq!(c.mode_icon_label(false, true), "Ca");
    }
}

#[cfg(test)]
#[path = "coordinator_toolbar_items_tests.rs"]
mod toolbar_items_tests;

#[cfg(test)]
mod emoji_tables_tests {
    use super::EmojiTables;
    use wind_dict::emojidict::{EmojiReader, write_emoji_wemj};

    fn table(
        dir: &std::path::Path,
        name: &str,
        rows: &[(&str, &[&str])],
    ) -> std::sync::Arc<EmojiReader> {
        let p = dir.join(format!("{name}.wemj"));
        let rows: Vec<(String, Vec<String>)> = rows
            .iter()
            .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect();
        write_emoji_wemj(&p, &rows).unwrap();
        std::sync::Arc::new(EmojiReader::open(&p).unwrap())
    }

    /// 多表按挂载顺序拼接：主表在前，附加模块的同词结果接在后面；只有一张表命中时就是它；
    /// 都没命中为 None。去重留给消费端（`plan_emoji_insertions`）。
    #[test]
    fn lookup_concatenates_in_table_order() {
        let dir = std::env::temp_dir().join(format!("wind-emoji-tables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let t = EmojiTables(vec![
            table(&dir, "word", &[("开心", &["😄"])]),
            table(
                &dir,
                "frost",
                &[("开心", &["😄", "😊"]), ("右箭头", &["➡️"])],
            ),
        ]);
        assert_eq!(t.lookup("开心").as_deref(), Some("😄 😄 😊"));
        assert_eq!(t.lookup("右箭头").as_deref(), Some("➡️"));
        assert_eq!(t.lookup("无此词"), None);
        drop(t);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod exit_flush_tests {
    use super::*;

    /// 退出前落盘连统计一起：`process::exit` 不跑析构，收集器的内存增量否则就丢了。
    #[test]
    fn flush_before_exit_writes_todays_stats() {
        let user = std::env::temp_dir().join(format!("wind_exit_flush-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&user);
        let (c, _rx) = Coordinator::new_headless_with_ui_at(Config::default(), None, Some(&user));
        let sc = c.stat_collector.as_ref().expect("有用户目录就有统计收集器");
        sc.record(StatEvent {
            chinese: 7,
            ..Default::default()
        });
        c.flush_user_data_before_exit();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let store = c.store.as_ref().unwrap();
        assert_eq!(store.get_daily_stat(&today).unwrap().chinese, 7);
        drop(c);
        let _ = std::fs::remove_dir_all(&user);
    }
}

#[cfg(test)]
mod syllable_separator_app_tests {
    //! 应用级 `syllable_separator`：断言落在按键出口（组合串），不是判据返回值。
    use super::*;
    use wind_config::app_compat::{AppCompat, AppCompatRule, SyllableSeparator};

    fn data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
    }

    fn composition_after_typing(c: &Coordinator) -> String {
        let mut last = KeyAction::Consumed;
        for ch in "xiaopeng".chars() {
            last = c.handle_key_event_policed(&KeyEventData {
                key_code: (ch.to_ascii_uppercase() as u32) & 0xFF,
                scan_code: 0,
                modifiers: 0,
                event_type: EVENT_KEY_DOWN,
                toggles: 0,
                event_seq: 0,
                prev_char: 0,
                seq_char: 0,
            });
        }
        match last {
            KeyAction::UpdateComposition { text, .. } => text,
            other => panic!("期望组合区更新，实得 {other:?}"),
        }
    }

    fn focus(c: &Coordinator, pid: u32, name: &str) {
        c.pid_names.lock().unwrap().insert(pid, name.into());
        c.active_compat.lock().unwrap().pid = pid;
        c.refresh_active_compat_lookups();
    }

    #[test]
    fn app_rule_overrides_global_separator() {
        if !data_dir().join("schemas/pinyin.schema.toml").exists() {
            eprintln!("skip: 无 build_dev/data");
            return;
        }
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
        cfg.schema.active = "pinyin".into();
        cfg.input.default.chinese_mode = true;
        let c = Coordinator::new_headless(cfg, Some(&data_dir()));
        c.test_set_app_compat(AppCompat::from_rules(vec![AppCompatRule {
            process: "ue.exe".into(),
            syllable_separator: Some(SyllableSeparator::None),
            ..Default::default()
        }]));

        focus(&c, 7, "ue.exe");
        assert_eq!(
            composition_after_typing(&c),
            "xiaopeng",
            "配了 none 的应用无分隔符"
        );

        c.handle_key_event_policed(&KeyEventData {
            key_code: 0x1B,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
        focus(&c, 8, "other.exe");
        assert!(
            composition_after_typing(&c).contains('\''),
            "未配规则的应用跟随全局默认 '"
        );
    }
}

/// [`Coordinator::resolve_index_label`] 的裁决本体（主题槽位由调用方给）：设置端预览按
/// 被预览主题的槽位取，与实时候选窗同一条规则。
pub(crate) fn pick_index_label(
    cand_cfg: &wind_config::config::UiCandidateConfig,
    theme_labels: &[String],
    i: usize,
) -> String {
    if let Some(s) = cand_cfg.user_index_label(i) {
        return s;
    }
    wind_ui_types::theme_index_label(theme_labels, i)
}
