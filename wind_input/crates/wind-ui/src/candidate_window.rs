//! 候选窗口：View 盒模型布局 + DirectWrite 文本 + Win32 Layered Window
//!
//! 与 Go 版本 `wind_input/internal/ui/manager_candidate.go` + `viewbox_build.go` 对齐。
//! 用 `crate::view` 的盒模型构建候选树（预编辑行 + 候选行[序号|文本] + 翻页指示），
//! measure/arrange 算出尺寸与每候选的绝对矩形（供鼠标命中），再 paint 到 BGRA 缓冲区。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use crate::manager::{
    HOVER_PAGE_NEXT as TAG_PAGE_NEXT, HOVER_PAGE_PREV as TAG_PAGE_PREV, MenuAnchor, UiEvent,
};
use crate::sys::{
    GetCursorPos, GetWindowRect, HWND, HWND_TOPMOST, IDC_ARROW, IDC_SIZEALL, LPARAM, LRESULT,
    LoadCursorW, POINT, RECT, ReleaseCapture, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetCapture,
    SetCursor, SetWindowPos, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_SETCURSOR, WPARAM, clamp_content_to_monitor,
};
use crate::text::dwrite::TextRenderer;
use crate::text::font_resolve::{FontNameSource, ResolvedFont, resolve_font_name};
use crate::text::script::{FontPlan, ScriptClass};
use crate::view::Rect;
// 下面几组只给本文件的各测试模块（`use super::*`）用：搭树代码迁到 wind-scene 后窗口本身不再用到。
#[cfg(test)]
use crate::text::dwrite::{ColorRun, TextStyle};
#[cfg(test)]
use crate::view::{Edges, Layout, View};

/// 内置默认字族：`ui.font.family` 为空时用它。
///
/// 它同时是 `TextRenderer::new` 的初始字族与回退链的链首，必须只有一个来源，否则链首与
/// 实际 base family 对不上，回退链静默不生效。
pub const DEFAULT_FONT_FAMILY: &str = "Microsoft YaHei UI";

/// 空字族回落内置默认。
///
/// [`build_font_plan`] 必须用同一个答案：链首是空串时 `AddMapping` 的 `baseFamilyName` 永远
/// 匹配不上，fallback 只在显式写了 family 时才生效。
fn resolve_font_family(family: &str) -> &str {
    let f = family.trim();
    if f.is_empty() { DEFAULT_FONT_FAMILY } else { f }
}

/// 把 `[ui.font]` 的三个键折成渲染层的 [`FontPlan`]。
///
/// 纯函数，承载「配置怎么变成方案」的全部判定（链首归一、未知类名处置），与窗口/COM 无关，
/// 故可单测。未知的脚本类名记 warn 后忽略：拼错一个键不该让整份字体配置失效。
pub(crate) fn build_font_plan(
    family: &str,
    fallback: &[String],
    scripts: &[(String, Vec<String>)],
) -> FontPlan {
    let default_chain: Vec<String> = std::iter::once(resolve_font_family(family).to_string())
        .chain(fallback.iter().cloned())
        .collect();
    let mut assigned: Vec<(ScriptClass, Vec<String>)> = Vec::new();
    for (key, chain) in scripts {
        match ScriptClass::from_key(key) {
            Some(c) => assigned.push((c, chain.clone())),
            None => tracing::warn!("ui.font.scripts 里的未知脚本类名「{key}」已忽略"),
        }
    }
    // emoji 类的出厂指派（仅 Windows；macOS 的脚本指派尚未实现，见 `coretext.rs`）。
    //
    // 在这里注入而不靠 data/config.toml 的默认值：老用户的 config.toml 里已有 `scripts = {}`，
    // 改出厂值对他们不生效。用户显式写了 `emoji = [...]`（含空表 = 关掉）即按用户的来。
    //
    // 只靠系统回退不够：DirectWrite 的回退区间表漏映射新 emoji 码位；键帽序列的基字基准字体
    // 自己就有，回退不触发（见 `script::font_runs` 的回溯改写）。背景见
    // `docs/design/emoji-suggestion.md`。代价：`U+2600..27BF` 的中性符号（☆ ✓）也跟着走，
    // 且 `font_runs` 在每个文本叶子上都要跑一遍（微秒级）。
    if cfg!(windows) && !assigned.iter().any(|(c, _)| *c == ScriptClass::Emoji) {
        assigned.push((ScriptClass::Emoji, vec![DEFAULT_EMOJI_FAMILY.to_string()]));
    }
    FontPlan::new(default_chain, assigned)
}

/// Windows 出厂的 emoji 字族。只在 [`build_font_plan`] 未见用户声明 emoji 类时注入。
pub const DEFAULT_EMOJI_FAMILY: &str = "Segoe UI Emoji";

/// 本帧上屏要做什么：返回 `(推像素, 摆位)`。`prev` 为窗口当前在屏上的几何，None = 不在屏上。
///
/// 光标在组合内右移会触发协调器 reshow，同一键常连发 2–3 帧内容与位置都不变的刷新；远程桌面下
/// 每次 `UpdateLayeredWindow` 都要把整窗位图传给客户端，故像素没变就不推。摆位会
/// `SetWindowPos(HWND_TOPMOST)` 重新置顶，几何没变也省掉（同 `popup_menu::plan_render`）。
#[cfg_attr(not(windows), allow(dead_code))]
fn plan_present(
    prev: Option<(i32, i32, u32, u32)>,
    want: (i32, i32, u32, u32),
    same_pixels: bool,
) -> (bool, bool) {
    match prev {
        None => (true, true),
        Some(p) => (!same_pixels || (p.2, p.3) != (want.2, want.3), p != want),
    }
}

use crate::window::{LayeredWindow, WindowMouse};
use scene::{CandidateScene, CandidateSceneInput, CandidateSceneOptions};
use std::time::{Duration, Instant};

/// macOS host-render：`render_frame()` 产出的离屏帧（forwarder 写 SHM + push 用）。
#[cfg(ext_presenter)]
pub struct RenderedFrame {
    /// 图像屏幕左上角 X（已含软影 margin 回移）
    pub screen_x: i32,
    /// 图像屏幕左上角 Y
    pub screen_y: i32,
    /// 图像宽（设备像素，已乘 scale）
    pub width: u32,
    /// 图像高（设备像素）
    pub height: u32,
    /// 渲染缩放（Retina=2）；.app 按 logical=像素/scale 显示，根治模糊
    pub scale: f32,
    /// 是否含软件高斯阴影（影响 .app 落位/裁切）
    pub software_shadow: bool,
    /// `screen_x/screen_y` 是否为用户固定位置的绝对坐标（而非按光标推算的落点）。
    /// 置位时 `.app` 只做边界钳制，不再套用「下方放不下翻到光标上方」的兜底。
    pub absolute_pos: bool,
    /// 候选命中矩形（窗口缓冲坐标，内容起点 (ml,mt)）：(index, Rect)
    pub hit_rects: Vec<(i32, crate::view::Rect)>,
    /// 预乘 BGRA 像素缓冲（width×height×4）
    pub buf: Vec<u8>,
    /// 皮肤区域（`CMD_CANDIDATE_SKIN`，设计 §17.2）。没启用皮肤为 `Default`（`flags = 0`）。
    pub skin: wind_ipc::codec::CandidateSkinWire,
    /// macOS：拆出来的穿透装饰层（与 `buf` 同尺寸）+ 是否垫在主层下面。主层里没有它们。
    pub pass_layer: Option<(Vec<u8>, bool)>,
    /// 窗口左上 → 内容左上（`ui.candidate.custom_x/y` 同义）的偏移，单位同 `screen_x/y`。宿主本地拖动后
    /// 按它把窗口落点换回内容左上报 `pos.candidate`（皮肤装饰外扩时非零，设计 §17.7）。
    pub content_offset: (i32, i32),
    /// 跟随光标时翻到光标上方的窗口左上（单位同 `screen_x/y`）。服务端拿不到屏幕几何、不翻转，
    /// 上下由宿主按可见区定；上方点按 Windows `place_window` 的上翻规则算（皮肤 `caret_above`
    /// 对齐点、上方避让、偏移远离光标）。固定位置时同 `screen_x/y`。
    pub above_pos: (i32, i32),
    /// 宿主判翻转、夹边用的框（相对窗口左上的 x, y, w, h，单位同上）：内容框，皮肤 `clamp = "all"`
    /// 时连装饰外扩。与 Windows `place_window` 的 `hi + cb` / `- ct` 同口径。
    pub place_box: (i32, i32, i32, i32),
}

#[cfg(ext_presenter)]
impl RenderedFrame {
    /// 主层与穿透装饰层合成后的整张位图（截图用）。没拆层时就是 `buf`。
    pub fn composed(&self) -> Vec<u8> {
        let mut out = self.buf.clone();
        if let Some((pass, below)) = &self.pass_layer {
            if *below {
                scene::composite_under(&mut out, pass);
            } else {
                scene::composite_over(&mut out, pass);
            }
        }
        out
    }
}

/// Windows host-render：`render_frame()` 产出的离屏帧（写 SHM + Event 触发用）。
#[cfg(windows)]
pub struct RenderedFrame {
    /// 图像屏幕左上角 X（已含软影 margin 回移）
    pub screen_x: i32,
    /// 图像屏幕左上角 Y
    pub screen_y: i32,
    /// 图像宽（设备像素）
    pub width: u32,
    /// 图像高（设备像素）
    pub height: u32,
    /// 渲染缩放（DPI/96.0）
    pub scale: f32,
    /// 是否含软件高斯阴影
    pub software_shadow: bool,
    /// 候选命中矩形（窗口缓冲坐标，内容起点 (ml,mt)）：(index, Rect)
    pub hit_rects: Vec<(i32, crate::view::Rect)>,
    /// 预乘 BGRA 像素缓冲（width×height×4）
    pub buf: Vec<u8>,
    /// 拆出来的穿透装饰层（与 `buf` 同尺寸）+ 是否垫在主层下面。只 [`CandidateWindow::render_host_frame`]
    /// 按要求拆，其余路径为 `None`（穿透装饰在 `buf` 里）。
    pub pass_layer: Option<(Vec<u8>, bool)>,
}

/// 候选词数据类型已下沉至 wind-ui-types；再导出保持 `wind_ui::candidate_window::CandidateItem`
/// 原路径成立。
pub use wind_ui_types::CandidateItem;

/// 候选窗口配置
pub struct CandidateWindowConfig {
    pub font_size: f32,
    pub per_page: usize,
    pub bg_color: [u8; 4],
    pub text_color: [u8; 4],
    pub highlight_color: [u8; 4],
    pub border_color: [u8; 4],
    pub selected_bg: [u8; 4],
    /// 鼠标悬停底色（比选中底色更淡，区分两种状态）
    pub hover_bg: [u8; 4],
    pub padding_x: f32,
    pub padding_y: f32,
    pub item_spacing: f32,
}

impl Default for CandidateWindowConfig {
    fn default() -> Self {
        let dpi_scale = Self::get_dpi_scale();
        let base_font_size = 24.0;
        let font_size = base_font_size * dpi_scale;

        Self {
            font_size,
            per_page: 5,
            bg_color: [255, 255, 255, 245],
            text_color: [51, 51, 51, 255],
            highlight_color: [0, 120, 215, 255],
            border_color: [200, 200, 200, 200],
            selected_bg: [230, 240, 255, 255],
            hover_bg: [238, 242, 247, 255],
            padding_x: 12.0 * dpi_scale,
            padding_y: 8.0 * dpi_scale,
            item_spacing: 4.0 * dpi_scale,
        }
    }
}

impl CandidateWindowConfig {
    /// 序号标签颜色（比正文淡）
    #[allow(dead_code)]
    fn marker_color(&self) -> [u8; 4] {
        [140, 140, 145, 255]
    }

    pub(crate) fn get_dpi_scale() -> f32 {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Graphics::Gdi::*;
            unsafe {
                let hdc = GetDC(HWND::default());
                let dpi = GetDeviceCaps(hdc, LOGPIXELSY);
                ReleaseDC(HWND::default(), hdc);
                dpi as f32 / 96.0
            }
        }
        #[cfg(not(windows))]
        {
            1.0
        }
    }
}

/// 系统是否允许界面动画：Windows 读「显示 Windows 中的动画」（`SPI_GETCLIENTAREAANIMATION`），
/// 查询失败按允许。macOS / Linux 外部宿主见下面两个同名函数（设计 §17.1）；其余形态（开发机上的
/// 非 Windows 本地窗口，只供测试与出图）不播放，只画首帧。
#[cfg(windows)]
fn system_allows_animation() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    let mut on: i32 = 1;
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut i32 as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    !ok || on != 0
}

/// 宿主（macOS `.app` / Linux addon）报来的「减少动态效果」：0 = 没报过，1 = 关，2 = 开。见
/// [`set_host_reduce_motion`]。
#[cfg(ext_presenter)]
static HOST_REDUCE_MOTION: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// 记下宿主报来的「减少动态效果」（扩展信封 `host.reduce_motion`）。下一次显示候选窗时生效。
#[cfg(ext_presenter)]
pub fn set_host_reduce_motion(reduce: bool) {
    HOST_REDUCE_MOTION.store(
        if reduce { 2 } else { 1 },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// macOS「减少动态效果」：以 `.app` 报来的为准（它能用 `accessibilityDisplayShouldReduceMotion`，服务
/// 进程不链 AppKit）。没报过就读 `com.apple.universalaccess` 的 `reduceMotion`：先同步偏好域免得读到
/// 进程启动时的旧值；读不到（键不存在、该偏好域不让读）按允许。
#[cfg(target_os = "macos")]
fn system_allows_animation() -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::preferences::{
        CFPreferencesAppSynchronize, CFPreferencesGetAppBooleanValue,
    };
    match HOST_REDUCE_MOTION.load(std::sync::atomic::Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let key = CFString::from_static_string("reduceMotion");
    let app = CFString::from_static_string("com.apple.universalaccess");
    let mut valid: u8 = 0;
    // SAFETY: 两个 CFString 在调用期间存活；`valid` 是本地变量。
    let reduce = unsafe {
        CFPreferencesAppSynchronize(app.as_concrete_TypeRef());
        CFPreferencesGetAppBooleanValue(
            key.as_concrete_TypeRef(),
            app.as_concrete_TypeRef(),
            &mut valid,
        )
    };
    // valid == 0：键不存在或不是布尔，未知按允许。
    valid == 0 || reduce == 0
}

/// Linux 外部宿主：以 addon 报来的为准（它读设置门户与 kdeglobals，设计 §17.1）；没报过（读不到、
/// 旧 addon）按允许，只受 `ui.theme.animation` 控制。
#[cfg(all(target_os = "linux", ext_presenter))]
fn system_allows_animation() -> bool {
    host_motion_allowed(HOST_REDUCE_MOTION.load(std::sync::atomic::Ordering::Relaxed))
}

/// [`HOST_REDUCE_MOTION`] 的取值 → 允不允许动画：只有报过「减少」才不允许。
#[cfg(all(target_os = "linux", ext_presenter))]
fn host_motion_allowed(host: u8) -> bool {
    host != 2
}

#[cfg(not(any(windows, target_os = "macos", all(target_os = "linux", ext_presenter))))]
fn system_allows_animation() -> bool {
    false
}

/// 候选窗口
pub struct CandidateWindow {
    window: LayeredWindow,
    #[allow(dead_code)]
    config: CandidateWindowConfig,
    candidates: Vec<CandidateItem>,
    preedit: String,
    /// 编码区插入符位置：`preedit` 内字节偏移（恒在字符边界）。== len 表示光标在末尾。
    preedit_caret: usize,
    /// 模式指示文本（拼/双/快/英/符 或全称）；空=不渲染。
    mode_label: String,
    selected: usize,
    /// 鼠标悬停项（页内下标），-1 表示无；与 selected 独立渲染
    hover: i32,
    page: usize,
    total_pages: usize,
    visible: bool,
    /// 隐藏后释放像素缓冲的计时（设计 §7，见 [`Self::release_idle_pixels_at`]）。
    pixel_release: crate::window::HiddenRelease,
    x: i32,
    y: i32,
    /// 光标高度（上翻定位用）
    caret_height: i32,
    /// 当前光标坐标是否有效
    caret_valid: bool,
    /// 上次内容锚点屏幕坐标 (px, py)：每帧 place_window 算出新位置后与之比较，微移(<4px*scale)则
    /// 保持原位以抑制宿主 caret 抖动（位置保护）。隐藏时清空，下次组合重新落位。
    last_content_pos: Option<(i32, i32)>,
    /// 最近一帧内容盒的屏幕矩形 `(left, top, right, bottom)`（不含阴影扩边，宿主坐标单位），
    /// 隐藏时清空。状态提示泡跟随光标时避让它，见 [`Self::screen_rect`]。
    content_rect: Option<(i32, i32, i32, i32)>,
    /// 本地窗口当前在屏上的几何 (x, y, w, h)，None = 不在屏上。上屏去重的基线，见 [`plan_present`]。
    #[cfg(windows)]
    presented: Option<(i32, i32, u32, u32)>,
    text_renderer: TextRenderer,
    /// arrange 后收集的候选命中矩形：(候选页内下标, 矩形)，供鼠标层使用
    hit_rects: Vec<(i32, Rect)>,
    /// 鼠标处理器（与 window 共享，wnd_proc 经注册表回调）
    mouse: Rc<RefCell<CandidateMouse>>,
    /// 悬停编码反查气泡
    tooltip: Option<crate::tooltip::Tooltip>,
    /// 气泡此刻跟着哪个候选（页内下标；-1 = 无），与 [`CandidateMouse`] 共享，悬停宽限据此
    /// 判「移开的正是气泡所属的候选」。本地显示与宿主渲染两路都在这里记。
    tip_for: Rc<Cell<i32>>,
    /// 「屏幕点处最上层是不是这个窗口」，默认 [`crate::window::window_at`]；测试换桩。
    window_at: fn(HWND, i32, i32) -> bool,
    /// 生效主题：[`Self::theme_source`] 叠上外观覆盖（字体 / 字号）之后的那份。
    /// 渲染与测量一律只读它，由 [`Self::refresh_effective_theme`] 单点产出。
    theme: wind_theme::Resolved,
    /// 协调器下发的原始主题（未叠外观覆盖）。只供 [`Self::refresh_effective_theme`] 读。
    theme_source: wind_theme::Resolved,
    /// 本帧候选窗所在显示器的工作区，渲染入口开头查一次，见 [`Self::query_screen_extent`]。
    screen_extent: Option<(u32, u32)>,
    /// 候选树的构建与跨帧状态（皮肤几何、分槽位各块），见 [`scene`]。
    scene: CandidateScene,
    /// 低频选项（上翻、翻页栏、尺寸下限），按引用交给场景。
    scene_opts: CandidateSceneOptions,
    /// 设置端预览专用的场景（[`Self::render_preview`]），与实时窗口的 [`Self::scene`] 分开。
    preview_scene: CandidateScene,
    /// 本帧是否把穿透装饰拆到独立窗口：只有本地窗口路径（Windows `show`）置位。
    #[cfg_attr(not(windows), allow(dead_code))]
    split_pass_decor: bool,
    /// 拆出来的穿透装饰像素（与主窗口同尺寸同位置）+ 是否放在主窗口下面。
    #[cfg_attr(not(windows), allow(dead_code))]
    pass_layer: Option<(Vec<u8>, bool)>,
    /// 穿透窗口（`WS_EX_TRANSPARENT`，点击直达底下的应用）。首次用到才建。
    #[cfg(windows)]
    pass_window: Option<LayeredWindow>,
    /// 穿透窗口当前的屏上几何 (x, y, w, h)；None = 未显示。
    #[cfg(windows)]
    pass_presented: Option<(i32, i32, u32, u32)>,
    /// 穿透窗口建不出来：之后不再拆，穿透装饰回落到主窗口（按 block）。
    #[cfg_attr(not(windows), allow(dead_code))]
    pass_window_failed: bool,
    /// 系统是否允许界面动画，默认 [`system_allows_animation`]；测试换桩。
    motion_allowed: fn() -> bool,
    /// `ui.theme.animation`（缺省开）。
    theme_animation: bool,
    /// 播不播动画：[`Self::theme_animation`] 且 [`Self::motion_allowed`]，换主题与每次显示时
    /// 刷新。关着时装饰停在初始状态首帧，事件不切换。
    anim_allowed: bool,
    /// 上一次候选更新的（编码串, 页码, 空码）：比出 typing / backspace / page / empty。隐藏时清。
    skin_input_prev: Option<(String, usize, bool)>,
    /// 上一次工具栏的中英状态：比出 mode_cn / mode_en。
    skin_mode_prev: Option<bool>,
    /// 上次记日志时的（配置开关, 系统动画效果）：只在变化时记。
    anim_reported: Option<(bool, bool)>,
    /// 装饰动画的时间轴起点：由隐藏变可见时记。
    anim_start: Instant,
    /// 换帧到期，待 UI 循环取走后重绘一帧。
    anim_due: bool,
    /// 正在为换帧重绘：不重置悬停、不重摆气泡。
    anim_repaint: bool,
    /// 限帧闸门（外部宿主推帧用，见 [`Self::set_anim_gate`]）：换帧到期不早于它。None = 不限。
    anim_gate: Option<Instant>,
    /// 用户外观字体（`ui.font.family`，已 trim）；空 = 跟随主题节点字族。
    user_font_family: String,
    /// 用户全局字重（`ui.font.weight`）；0 = 不指定（取 [`Self::family_weight`]，再无则常规）。
    user_font_weight: i32,
    /// `ui.font.family` 是旧 GDI face name（「思源宋体 SemiBold」）时从名字里解析出的字重；
    /// 0 = 名字里没带。见 [`Self::apply_default_weight`]。
    family_weight: i32,
    /// 字体名解析缓存（配置名 → 渲染端 family + 字重）。解析可能要全量扫系统字体集，
    /// 而主题刷新会把同一批名字反复过一遍。每次 `set_font_family`（即配置重载）清空，
    /// 让新装的字体在下次保存设置后就能被认出来。
    font_names: RefCell<std::collections::HashMap<String, ResolvedFont>>,
    /// 生效的候选基准字号（逻辑像素）：用户 `ui.candidate.font_size` > 0 时取它，否则主题
    /// `behavior.font_size`。由 [`Self::refresh_effective_theme`] 算出，渲染只读它。
    base_font_logical: f32,
    /// 上次做过存在性检查的生效主题字族，未变就不再问（每个字族一次 COM 调用）。
    checked_theme_families: Vec<(String, String)>,
    /// DPI 缩放（主题几何为逻辑像素，渲染时乘此）
    scale: f32,
    /// 竖排布局（候选纵向堆叠）；默认横排。来自 ui.candidate.layout。
    vertical: bool,
    /// 候选文字节点的字族覆盖（方案级 `[candidate] font_family`）；空 = 不覆盖。
    ///
    /// 优先级：方案 > 用户 `ui.font.family`（非空）> 主题节点 `views.text.font_family` >
    /// 内置默认（合并见 [`scene::effective_theme`]）。
    /// 只作用于候选文字：序号/编码栏/注释/翻页栏多为拉丁与数字，跟着换反而更差；按脚本换字体
    /// 用全局 `ui.font.scripts`。
    text_family_override: String,
    /// 候选列表顺时针旋转 90° 呈现（蒙古文等纵向书写脚本）。来自 ui.candidate.layout。
    ///
    /// 旋转态下 [`Self::vertical`] 是 false（屏幕上候选是并列的），按方向分叉的既有判据自动走
    /// 横排那一支。唯一额外判 `rotated` 的是列表怎么构造（见 `build_tree` 的 `list_vertical`）。
    rotated: bool,
    /// 旋转态下把每个字逆时针扶正、逐字下行（对联式竖排）。来自 ui.candidate.layout。
    ///
    /// 蕴含 [`Self::rotated`]：同一种排列，只有叶子怎么搭不同，本位只在 `build_tree` 拼文字/
    /// 序号叶子时读。
    upright: bool,
    /// 预编辑嵌入模式（编码嵌入候选行首，不显示独立 preedit 条）。
    /// 来自 ui.candidate.preedit_display == "candidate_inline"。
    preedit_embedded: bool,
    /// 候选字号覆盖（>0 时取代主题 behavior.font_size）。来自 ui.candidate.font_size。
    font_size_override: f32,
    /// 当前锚点是否落在光标上方（定位时计算，随锚点锁定保持）。供 flip 判定。
    placed_above: bool,
    /// 反向事件通道（与 `mouse` / `tooltip` 同源）。窗口自身也要上报状态，见 [`CandidateWindow::report_flip_state`]。
    events: Sender<UiEvent>,
    /// 上一次上报给协调器的「候选是否反转」，用于只在变化时发事件（渲染每帧都会走判定）。
    reported_flip: bool,
    /// 用户对候选窗位置的偏移（dp）。与主题 `window.position_offset` 相加，见 `position_offset_px`。
    /// 来自 ui.candidate.offset_x / offset_y。
    user_offset: (i32, i32),
    /// 是否采用主题阴影（false = 用户覆盖为无阴影）。来自 ui.candidate.shadow。
    theme_shadow: bool,
    /// 固定位置模式的内容左上屏幕坐标；`None`=跟随光标（默认）。
    /// 来自 ui.candidate.position_mode + custom_x/custom_y，每次 UpdateCandidates 同步。
    /// `Some((0, 0))` 是"已开启固定但尚未设定位置"，定位时落到屏幕默认锚点。
    fixed_pos: Option<(i32, i32)>,
    /// 展开态（多行网格）的版式；`None` = 普通单行 / 单列。每次 UpdateCandidates 同步。
    /// 设计见 `docs/design/candidate-grid-expand.md`。
    grid: Option<wind_ui_types::CandidateGrid>,
    /// 展开态这一帧内容区的 `(左, 右, 顶)`（窗口内坐标，与命中矩形同一坐标系）。
    /// 悬停气泡据此停靠在整个窗口外侧的顶端，见 [`Self::tip_anchor`]。
    grid_box: Cell<(f32, f32, f32)>,
}

impl CandidateWindow {
    pub fn new(config: CandidateWindowConfig, events: Sender<UiEvent>) -> Result<Self, String> {
        let window = LayeredWindow::create(None, 400, 200, "WindInputCandidate")?;
        let text_renderer = TextRenderer::new(DEFAULT_FONT_FAMILY, config.font_size)?;
        let tooltip_events = events.clone();
        let self_events = events.clone();
        let tip_for = Rc::new(Cell::new(-1));
        let mouse = Rc::new(RefCell::new(CandidateMouse {
            hit_rects: Vec::new(),
            skin_frame: None,
            skin_decor: Vec::new(),
            skin_click_rects: Vec::new(),
            decor_press: None,
            skin_click: None,
            pass_hwnd: None,
            events,
            last_hover: -1,
            last_cursor: (i32::MIN, i32::MIN),
            engaged: false,
            engage_at: None,
            pending_raw: -1,
            engage_delay_ms: 60,
            hwnd: window.hwnd(),
            dragging: false,
            pressed: None,
            drag_anchor: (0, 0),
            drag_origin: (0, 0),
            drag_pin: None,
            drag_pin_margin: (0, 0),
            margin: (0, 0, 0, 0),
            double_click: crate::double_click::DoubleClick::new(),
            tip_hold: TipHold::Off,
            tip_released: false,
            tip_for: tip_for.clone(),
            deferred: None,
            tip_rect: None,
            tip_visible: None,
            tip_best: None,
            last_pos: None,
        }));
        window.register_mouse(mouse.clone());
        let tooltip = crate::tooltip::Tooltip::new(tooltip_events).ok();
        mouse.borrow_mut().tip_visible = tooltip.as_ref().map(|t| t.visible_flag());
        Ok(Self {
            window,
            config,
            candidates: Vec::new(),
            preedit: String::new(),
            preedit_caret: 0,
            mode_label: String::new(),
            selected: 0,
            hover: -1,
            page: 1,
            total_pages: 1,
            visible: false,
            pixel_release: Default::default(),
            x: 0,
            y: 0,
            caret_height: 0,
            caret_valid: false,
            last_content_pos: None,
            content_rect: None,
            #[cfg(windows)]
            presented: None,
            text_renderer,
            hit_rects: Vec::new(),
            mouse,
            tip_for,
            tooltip,
            window_at: crate::window::window_at,
            theme: wind_theme::Resolved::default(),
            theme_source: wind_theme::Resolved::default(),
            screen_extent: None,
            scene: CandidateScene::default(),
            scene_opts: CandidateSceneOptions::default(),
            preview_scene: CandidateScene::default(),
            split_pass_decor: false,
            pass_layer: None,
            #[cfg(windows)]
            pass_window: None,
            #[cfg(windows)]
            pass_presented: None,
            pass_window_failed: false,
            motion_allowed: system_allows_animation,
            theme_animation: true,
            anim_allowed: system_allows_animation(),
            skin_input_prev: None,
            skin_mode_prev: None,
            anim_reported: None,
            anim_start: Instant::now(),
            anim_due: false,
            anim_repaint: false,
            anim_gate: None,
            user_font_family: String::new(),
            user_font_weight: 0,
            family_weight: 0,
            font_names: RefCell::new(std::collections::HashMap::new()),
            base_font_logical: scene::effective_base_font_size(
                0.0,
                wind_theme::Resolved::default().behavior.font_size,
            ),
            checked_theme_families: Vec::new(),
            scale: CandidateWindowConfig::get_dpi_scale(),
            vertical: false,
            rotated: false,
            upright: false,
            text_family_override: String::new(),
            preedit_embedded: false,
            font_size_override: 0.0,
            placed_above: false,
            events: self_events,
            reported_flip: false,
            user_offset: (0, 0),
            theme_shadow: true,
            fixed_pos: None,
            grid: None,
            grid_box: Cell::new((0.0, 0.0, 0.0)),
        })
    }

    /// 设置候选文字的字族覆盖（方案级 `[candidate] font_family`）；空串 = 取消覆盖。
    /// 语义与优先级见 [`Self::text_family_override`]。
    ///
    /// 它是「替换」不是「补充」：声明了就不再用 `ui.font.family`。不同字体同字号下视觉大小
    /// 不同，用户会以为字被缩放了，这一行日志是唯一线索。
    pub fn set_text_family_override(&mut self, family: &str) {
        let want = family.trim().to_string();
        if want != self.text_family_override {
            tracing::info!(
                "候选文字字族：「{}」→「{}」（方案级 [candidate] font_family；空 = 回落 ui.font.family 或主题节点）",
                self.text_family_override,
                want
            );
            self.resolve_and_report(&want, "方案级 [candidate] font_family");
        }
        self.text_family_override = want;
        self.refresh_effective_theme();
    }

    /// 设置候选布局方向。三位的语义见 [`Self::rotated`] / [`Self::upright`] 与
    /// `UiCommand::SetCandidateLayout`。
    ///
    /// 三位并非正交，合法值只有四个；协调器只会发出其中之一，这里 debug 下断言两条
    /// 非法组合，把「谁手写了字段」挡在开发期。
    pub fn set_orientation(&mut self, vertical: bool, rotated: bool, upright: bool) {
        debug_assert!(!(vertical && rotated), "vertical 与 rotated 不能同时为真");
        // 写成蕴含式（`!upright || rotated`）而不是 `!(upright && !rotated)`：
        // 后者在 clippy 1.96 上会报 nonminimal_bool（1.97 放宽），而 macOS 验证机的
        // Homebrew 工具链就停在 1.96。两种写法等价，取两个版本都干净的那个。
        debug_assert!(!upright || rotated, "upright 蕴含 rotated");
        if (vertical, rotated, upright) != (self.vertical, self.rotated, self.upright) {
            // upright 会把整串按格切开逐格排版，这会切断连写脚本（蒙古文、阿拉伯文）
            // 的字形连接，每格退回孤立形——孤立形比连写形窄小，画面上就是「字变小变散」。
            // 那类脚本要的是 rotated。日志里带上这一位，才分得开「字体不对」与「排列不对」。
            tracing::info!("候选排列：vertical={vertical} rotated={rotated} upright={upright}");
        }
        let was_vertical = self.vertical || self.rotated;
        self.vertical = vertical;
        self.rotated = rotated;
        self.upright = upright;
        // 竖排变体随方向切换：只在「是否竖排」变了且主题有变体时重算生效主题。
        if was_vertical != (vertical || rotated) && self.theme_source.views_vertical.is_some() {
            self.refresh_effective_theme();
            if let Some(tip) = self.tooltip.as_mut() {
                tip.set_theme(&self.theme);
            }
        }
    }

    /// 设置预编辑嵌入模式（true=编码嵌入候选行首，不显示独立 preedit 条）。
    pub fn set_preedit_embedded(&mut self, embedded: bool) {
        self.preedit_embedded = embedded;
    }

    /// 设置候选字号覆盖（0=跟随主题）。来自 ui.candidate.font_size。
    pub fn set_font_size_override(&mut self, font_size: f32) {
        self.font_size_override = font_size.max(0.0);
        self.refresh_effective_theme();
    }

    /// 设置候选窗尺寸下限（抗抖动）。来自 ui.candidate.min_window_width_horizontal /
    /// min_window_width_vertical / min_window_height_horizontal /
    /// min_window_height_vertical / min_rows。
    pub fn set_min_size(
        &mut self,
        width_horizontal: u32,
        width_vertical: u32,
        height_horizontal: u32,
        height_vertical: u32,
        rows: u32,
    ) {
        self.scene_opts.min_window_width_horizontal = width_horizontal;
        self.scene_opts.min_window_width_vertical = width_vertical;
        self.scene_opts.min_window_height_horizontal = height_horizontal;
        self.scene_opts.min_window_height_vertical = height_vertical;
        self.scene_opts.min_rows = rows;
    }

    /// 内容宽度的屏幕安全上限（设备 px），算法见 [`CandidateSceneInput::screen_safety_max_width_px`]。
    fn screen_safety_max_width_px(&self) -> u32 {
        self.scene_input(false).screen_safety_max_width_px()
    }

    /// 候选窗所在显示器的工作区 `(宽, 高)`（设备像素），交给场景算文字预算与屏幕安全上限。
    ///
    /// Windows 按固定点（固定位置模式）或光标点查——固定位置可能落在与光标不同缩放/不同显示器
    /// 的另一块屏上，取点与 DPI 探测一致。其它平台本地服务进程查不到显示器几何，给 None，
    /// 场景退化为保守的兜底常量（见 [`CandidateSceneInput::screen_safety_max_width_px`]）。
    ///
    /// 每帧只在渲染入口开头查一次，存进 [`Self::screen_extent`]（见 [`Self::layout_frame`]），
    /// 场景输入一律读存下的值。
    fn query_screen_extent(&self) -> Option<(u32, u32)> {
        #[cfg(windows)]
        {
            let (px, py) = match self.fixed_pos {
                Some(f) if f != (0, 0) => f,
                _ => (self.x, self.y),
            };
            let extent = crate::sys::monitor_work_area_width_at(px, py)
                .zip(crate::sys::monitor_work_area_height_at(px, py));
            if extent.is_none() {
                tracing::warn!(
                    "screen_extent: monitor query failed at ({px},{py}), 退化到兜底常量"
                );
            }
            extent
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// 借出场景与本帧输入（`above = false`）给需要改场景状态的调用：场景先取出、用完放回，
    /// 输入只借窗口其余字段。
    fn with_scene<R>(
        &mut self,
        f: impl FnOnce(&mut CandidateScene, &CandidateSceneInput) -> R,
    ) -> R {
        let mut scene = std::mem::take(&mut self.scene);
        let r = f(&mut scene, &self.scene_input(false));
        self.scene = scene;
        r
    }

    /// 一帧排版：查一次屏幕工作区、做窗口侧动画簿记，再交场景 [`CandidateScene::layout`]。
    /// 网格展开态顺带记下内容盒（悬停气泡停靠用）。三个渲染入口共用。
    fn layout_frame(&mut self, now: Instant, soft_shadow: bool) -> scene::SceneFrame {
        self.screen_extent = self.query_screen_extent();
        let clock = self.begin_anim_frame(now);
        let frame = self.with_scene(|scene, inp| scene.layout(inp, clock, soft_shadow));
        let (ml, mt, _, _) = frame.margins;
        self.note_grid_box((ml as f32, mt as f32), frame.natural_w);
        frame
    }

    /// 本帧命中矩形存下并同步给鼠标处理器（须在上翻重排之后）。
    fn sync_hits(&mut self, frame: &scene::SceneFrame) {
        self.hit_rects = self.scene.hits(frame);
        let mut m = self.mouse.borrow_mut();
        m.hit_rects = self.hit_rects.clone();
        m.skin_frame = self.scene.skin_geom.as_ref().map(|g| g.frame_rect());
        m.skin_decor = self
            .scene
            .skin_geom
            .as_ref()
            .map_or_else(Vec::new, |g| g.no_drag_rects());
        m.skin_click_rects = self
            .scene
            .skin_geom
            .as_ref()
            .map_or_else(Vec::new, |g| g.click_rects());
        // 换帧重绘时内容没变，悬停去重基线保持原样。
        if !self.anim_repaint {
            m.last_hover = -1;
        }
    }

    /// 下一次装饰换帧的时刻：场景给的是距动画时间轴起点的时长，起点在窗口侧。
    fn anim_next_at(&self) -> Option<Instant> {
        let at = self.scene.next_anim_at().map(|d| self.anim_start + d)?;
        Some(self.anim_gate.map_or(at, |g| at.max(g)))
    }

    /// 设限帧闸门：此后换帧到期不早于 `at`，且跳过画面不会变的边界（设计 §17.1）。在推出一帧
    /// 换帧之后调，「画面会不会变」按刚推出去的那帧比。只管换帧：悬停、提示保持、释放像素照旧到期。
    pub fn set_anim_gate(&mut self, at: Instant) {
        self.anim_gate = Some(at);
        if self.scene.next_anim_at().is_none() {
            return;
        }
        let after = at.saturating_duration_since(self.anim_start);
        let next = self.scene.next_anim_change(&self.scene_input(false), after);
        self.scene.anim_next = next;
    }

    /// 限帧闸门（[`Self::set_anim_gate`]）；None = 不限。
    pub fn anim_gate(&self) -> Option<Instant> {
        self.anim_gate
    }

    /// 本帧场景输入：内容、排布与生效主题都按引用借出，`above` 决定是否按上方专属布局搭。
    fn scene_input(&self, above: bool) -> CandidateSceneInput<'_> {
        CandidateSceneInput {
            candidates: &self.candidates,
            preedit: &self.preedit,
            preedit_caret: self.preedit_caret,
            mode_label: &self.mode_label,
            selected: self.selected,
            hover: self.hover,
            page: self.page,
            total_pages: self.total_pages,
            grid: self.grid,
            vertical: self.vertical,
            rotated: self.rotated,
            upright: self.upright,
            preedit_embedded: self.preedit_embedded,
            above,
            theme: &self.theme,
            base_font_logical: self.base_font_logical,
            scale: self.scale,
            screen_extent_px: self.screen_extent,
            text_renderer: &self.text_renderer,
            opts: &self.scene_opts,
        }
    }

    /// 把配置里的字体名解析成渲染端认得的 family（+ 名字里带的字重），按名字缓存。
    ///
    /// 所有字体名入口（`ui.font.family` / `fallback` / `scripts`、方案级 `[candidate] font_family`、
    /// 主题节点 `font_family`）都走这一个函数，判定顺序见 [`resolve_font_name`]：存量配置里的
    /// 旧 GDI face name（「思源宋体 SemiBold」）靠剥字重词、按 face 全名查自愈。
    fn resolve_font(&self, name: &str) -> ResolvedFont {
        let name = name.trim();
        if let Some(r) = self.font_names.borrow().get(name) {
            return r.clone();
        }
        let r = resolve_font_name(
            name,
            |n| self.text_renderer.family_exists(n),
            |n| self.text_renderer.find_face(n),
        );
        self.font_names
            .borrow_mut()
            .insert(name.to_string(), r.clone());
        r
    }

    /// [`Self::resolve_font`] + 按结果记日志。空名字返回 `None`。
    ///
    /// 缺失的判据是「解析后仍 `Missing`」：`Unknown` 表示查不了（mock 后端等），当成缺失会刷
    /// 假 warn。`source` 写配置键的全名，用户看到日志能直接去改那一项。
    fn resolve_and_report(&self, family: &str, source: &str) -> Option<ResolvedFont> {
        let name = family.trim();
        if name.is_empty() {
            return None;
        }
        let r = self.resolve_font(name);
        match r.source {
            FontNameSource::Missing => tracing::warn!(
                "{source} 指定的字体「{name}」不在系统字体集里，已被静默回落到默认字体——\
                 请确认填的是字体的**家族名**（如 Mongolian Baiti），DirectWrite 不认文件名"
            ),
            FontNameSource::WeightSuffix | FontNameSource::FaceName => tracing::info!(
                "{source} 的字体名「{name}」是旧式（GDI）字体名，已按家族「{}」+ 字重 {} 解析——\
                 在设置页重新选一次字体与字重即可换成新写法",
                r.family,
                r.weight
            ),
            FontNameSource::Exact | FontNameSource::Unknown => {}
        }
        Some(r)
    }

    /// 设置候选字体族（来自 ui.font.family；空=默认 [`DEFAULT_FONT_FAMILY`]）。
    pub fn set_font_family(&mut self, family: &str) {
        // 配置重载的入口：清掉解析缓存，新装 / 卸掉的字体在这之后才会被重新判定。
        self.font_names.borrow_mut().clear();
        let name = resolve_font_family(family);
        let r = self
            .resolve_and_report(name, "ui.font.family")
            .unwrap_or_else(|| ResolvedFont {
                family: name.to_string(),
                weight: 0,
                source: FontNameSource::Unknown,
            });
        self.text_renderer.set_font_family(&r.family);
        self.family_weight = r.weight;
        self.apply_default_weight();
        self.user_font_family = family.trim().to_string();
        self.refresh_effective_theme();
    }

    /// 设置全局字重（来自 `ui.font.weight`；0 = 不指定）。
    ///
    /// `<= 0` 视为不指定；其余钳到 DirectWrite 接受的 1..=950，越界记 warn。
    pub fn set_font_weight(&mut self, weight: i32) {
        self.user_font_weight = if weight <= 0 { 0 } else { weight.clamp(1, 950) };
        if weight > 950 {
            tracing::warn!("ui.font.weight = {weight} 超出 1..=950，已按 950 处理");
        }
        self.apply_default_weight();
        // 主题节点里旧字体名拆出的字重让位于用户字重，需重算生效主题。
        self.refresh_effective_theme();
    }

    /// 渲染器默认字重：用户 `ui.font.weight` > 旧字体名里带的字重 > 常规。
    /// 主题节点显式的 `font_weight` 在 layout 层胜过它；用户字重非 0 时主题节点的普通字重
    /// 已在 [`Self::refresh_effective_theme`] 里改写成用户字重，强调字重取 max（见 `apply_user_font_weight`）。
    fn apply_default_weight(&mut self) {
        let w = if self.user_font_weight > 0 {
            self.user_font_weight
        } else {
            self.family_weight
        };
        self.text_renderer.set_default_weight(w);
    }

    /// 设置候选字体的回退链与按脚本的字体指派（来自 `ui.font.fallback` / `ui.font.scripts`）。
    ///
    /// `family` 必须与最近一次 [`Self::set_font_family`] 同源——它是默认链的链首。
    /// 二者由同一条 `UiCommand::SetCandidateFont` 携带，故不存在到达顺序问题。
    ///
    /// 链首不在这里查存在性：已由 [`Self::set_font_family`] 查过。
    pub fn set_font_plan(
        &mut self,
        family: &str,
        fallback: &[String],
        scripts: &[(String, Vec<String>)],
    ) {
        // 链里每一项都换成解析后的 family：链首必须与 `set_font_family` 设进 TextFormat 的
        // 那个名字逐字相同（`AddMapping` 靠 baseFamilyName 认段），故链首也走同一个解析。
        // 回退链与脚本指派表达不了字重，名字里带的字重在这里丢弃（日志里已说明是旧名）。
        let family = self.resolve_font(resolve_font_family(family)).family;
        let fallback: Vec<String> = fallback
            .iter()
            .map(|f| {
                self.resolve_and_report(f, "ui.font.fallback")
                    .map_or_else(|| f.clone(), |r| r.family)
            })
            .collect();
        let scripts: Vec<(String, Vec<String>)> = scripts
            .iter()
            .map(|(key, chain)| {
                let chain = chain
                    .iter()
                    .map(|f| {
                        self.resolve_and_report(f, &format!("ui.font.scripts.{key}"))
                            .map_or_else(|| f.clone(), |r| r.family)
                    })
                    .collect();
                (key.clone(), chain)
            })
            .collect();
        let plan = build_font_plan(&family, &fallback, &scripts);
        // 出厂注入的 emoji 指派（见 `build_font_plan`）不在用户的 `scripts` 里，上面的循环
        // 查不到它；精简过的 Windows 镜像缺 Segoe UI Emoji 时，用户得知道是哪一项在回落。
        if !scripts
            .iter()
            .any(|(k, _)| ScriptClass::from_key(k) == Some(ScriptClass::Emoji))
            && plan.chain_for(Some(ScriptClass::Emoji)) == [DEFAULT_EMOJI_FAMILY]
        {
            self.resolve_and_report(DEFAULT_EMOJI_FAMILY, "ui.font.scripts.emoji（出厂默认）");
        }
        self.text_renderer.set_font_plan(plan);
    }

    /// 设置"上方时反转候选顺序"。来自 ui.candidate.flip_when_above。
    pub fn set_flip_when_above(&mut self, flip: bool) {
        self.scene_opts.flip_when_above = flip;
    }

    /// 设置用户位置偏移（dp）。只影响下一次定位，不需重算主题。
    pub fn set_user_offset(&mut self, x: i32, y: i32) {
        self.user_offset = (x, y);
    }

    /// 设置是否采用主题阴影。生效主题由 [`Self::refresh_effective_theme`] 重算，
    /// 窗口扩边、内容锚点随之自动变化。
    pub fn set_theme_shadow(&mut self, on: bool) {
        if self.theme_shadow != on {
            self.theme_shadow = on;
            self.refresh_effective_theme();
        }
    }

    /// 设置"上方时交换编码栏与候选栏位置"。来自 ui.candidate.swap_preedit_when_above。
    pub fn set_swap_preedit_when_above(&mut self, swap: bool) {
        self.scene_opts.swap_preedit_when_above = swap;
    }

    /// 把「候选项当前是否被反转」上报给协调器（仅在取值变化时发事件）。
    ///
    /// 判据的真相源只能在这一侧（`placed_above` 要窗口尺寸 + 屏幕工作区，协调器推不出来）：
    /// 送 [`CandidateScene::above_layout`] 的第一个返回值过去，由协调器翻转
    /// `highlight_up` / `highlight_down` 的走向。
    ///
    /// 调用点＝每处 `placed_above` 刚定完之后，以及 [`CandidateWindow::hide`] 清除上翻粘滞时
    /// （防下一轮首帧前按方向键读到残留的 true）。
    fn report_flip_state(&mut self) {
        let (flip, _) = CandidateScene::above_layout(
            self.placed_above,
            self.vertical,
            self.scene_opts.flip_when_above,
            self.scene_opts.swap_preedit_when_above,
        );
        if flip != self.reported_flip {
            self.reported_flip = flip;
            let _ = self.events.send(UiEvent::CandidateFlipped(flip));
        }
    }

    /// 标记窗口进入可见态；不可见 → 可见的转换处重采鼠标防抖基线。
    ///
    /// 基线必须在窗口即将出现时采样，不能沿用 `hide()` 那次：防抖判据是「物理光标与基线相同
    /// ⇒ 伪移动 ⇒ 忽略」，基线陈旧（进程内首次显示、或两次显示间鼠标动过）会把窗口出现时的
    /// 进入消息误判成真实移动，自动高亮并弹 tooltip。
    fn mark_visible(&mut self) {
        if !self.visible {
            self.mouse.borrow_mut().reset_hover();
        }
        self.visible = true;
        self.pixel_release.shown();
    }

    /// 设置"翻页栏并入编码栏行"。来自 ui.candidate.pager_in_preedit。
    pub fn set_pager_in_preedit(&mut self, on: bool) {
        self.scene_opts.pager_in_preedit = on;
    }

    /// 设置翻页栏显示覆盖（横排档、竖排档）。来自 ui.candidate.pager_bar_display。
    pub fn set_pager_display(&mut self, h: String, v: String) {
        self.scene_opts.pager_display = (h, v);
    }

    /// 设置页码文字显示覆盖（横排档、竖排档）。来自 ui.candidate.page_number_display。
    pub fn set_page_number_display(&mut self, h: String, v: String) {
        self.scene_opts.page_number_display = (h, v);
    }

    /// 设置悬停提示激活延迟（毫秒）。来自 ui.tooltip.delay。
    pub fn set_tooltip_delay(&mut self, delay_ms: i32) {
        self.mouse.borrow_mut().engage_delay_ms = delay_ms.max(0) as u64;
    }

    /// 设置拆字字根字体（TTF 路径 + DWrite 家族名）。两个渲染器都要收到。
    ///
    /// 字根字符落在私用区（PUA），靠渲染器把 PUA 连续段切到字根字体集（`text::dwrite` 的
    /// `pua_runs` + `SetFontCollection`）。这套机制只对调用过本方法的那个 `TextRenderer` 实例
    /// 生效，漏转发给候选窗自己的渲染器，候选行里的字根就显示成豆腐块。
    pub fn set_chaizi_font(&mut self, path: &str, family: &str) {
        if let Err(e) = self.text_renderer.set_chaizi_font(path, family) {
            tracing::warn!("候选窗字根字体加载失败: {e}");
        }
        if let Some(t) = self.tooltip.as_mut() {
            t.set_chaizi_font(path, family);
        }
    }

    /// 将悬停提示窗口当前渲染帧保存为 PNG 文件（截图用）。
    pub fn tooltip_capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        match self.tooltip.as_ref() {
            Some(t) => t.capture_to_file(path),
            None => Err("tooltip 未初始化".to_string()),
        }
    }

    /// 将悬停 tooltip 当前渲染帧复制到剪贴板。
    pub fn tooltip_capture_to_clipboard(&self) -> Result<(), String> {
        match self.tooltip.as_ref() {
            Some(t) => t.capture_to_clipboard(),
            None => Err("tooltip 未初始化".to_string()),
        }
    }

    /// 悬停提示窗口当前是否可见。
    pub fn tooltip_is_visible(&self) -> bool {
        self.tooltip
            .as_ref()
            .map(|t| t.is_visible())
            .unwrap_or(false)
    }

    /// 设置悬停提示右键菜单打开状态（转发给 Tooltip，见其 set_menu_open 说明）。
    pub fn tooltip_set_menu_open(&mut self, open: bool) {
        if let Some(t) = self.tooltip.as_mut() {
            t.set_menu_open(open);
        }
    }

    /// 关掉菜单的那次菜单外按下（菜单轮询所见，见 `popup_menu::OutsidePress`）。
    ///
    /// 菜单开着时同线程的候选窗、气泡都收不到鼠标消息，这次按下只有轮询看得见。右键落在
    /// 气泡上交给气泡（重开气泡菜单，见其 `on_menu_outside_press`）；落在候选窗上则合成一次
    /// 候选窗右键——「关旧开新」，与在没有菜单时右键同一效果。左键只关菜单。
    pub fn menu_outside_press(&mut self, x: i32, y: i32, right: bool) {
        if let Some(t) = self.tooltip.as_mut()
            && t.on_menu_outside_press(x, y, right)
        {
            return;
        }
        if !right || !self.visible || !(self.window_at)(self.window.hwnd(), x, y) {
            return;
        }
        let origin = self.mouse.borrow().window_origin();
        let (ox, oy) = origin.unwrap_or((0, 0));
        self.mouse
            .borrow_mut()
            .right_click((x - ox) as f32, (y - oy) as f32, (x, y));
    }

    /// 悬停气泡（测试用）。
    #[cfg(test)]
    pub(crate) fn tooltip_mut(&mut self) -> Option<&mut crate::tooltip::Tooltip> {
        self.tooltip.as_mut()
    }

    /// UI 循环收掉了一个可见菜单（协调器 `HideMenu` / `HideCandidates`），转给气泡。
    pub fn tooltip_menu_dismissed(&mut self) {
        if let Some(t) = self.tooltip.as_mut() {
            t.on_menu_dismissed();
        }
    }

    /// 应用主题（协调器下发）。同步更新悬停 tooltip 配色。
    pub fn set_theme(&mut self, theme: wind_theme::Resolved) {
        self.refresh_anim_allowed();
        if self.theme_source.skin != theme.skin {
            crate::view::clear_anim_frames();
            self.scene.decor_anims_key = None;
            self.scene.skin_warned.borrow_mut().clear();
        }
        self.theme_source = theme;
        self.refresh_effective_theme();
        if let Some(tip) = self.tooltip.as_mut() {
            tip.set_theme(&self.theme);
        }
    }

    /// 由原始主题 + 外观覆盖算出生效主题（[`Self::theme`]）与生效字号——唯一的合并点。
    ///
    /// 在四个低频入口（换主题 / 用户字体 / 方案级字体 / 用户字号）调用，不在渲染热路径。
    /// 渲染与测量（`build_tree` 里的 `measure_style` 与各叶子）都只读结果，
    /// 两条路径因此天然同源，不在节点构建处散写「用户有值就用用户的」。
    fn refresh_effective_theme(&mut self) {
        let o = scene::ThemeOverrides {
            vertical: self.vertical || self.rotated,
            user_font_family: &self.user_font_family,
            user_font_weight: self.user_font_weight,
            scheme_text_family: &self.text_family_override,
            shadow: self.theme_shadow,
            font_size: self.font_size_override,
        };
        // 字体名解析走本窗口的缓存（见 [`Self::resolve_font`]），旧 GDI face name → family + 字重。
        let eff = scene::effective_theme(&self.theme_source, &o, |f| {
            let r = self.resolve_font(f);
            (r.family, r.weight)
        });
        // 存在性告警按生效字族查（叠上用户字体之后、方案级字体之前，见
        // [`scene::EffectiveTheme::declared_families`]）。未变就不再问。
        if eff.declared_families != self.checked_theme_families {
            self.warn_missing_theme_families(&eff.declared_families);
            self.checked_theme_families = eff.declared_families;
        }
        let t = eff.theme;
        self.base_font_logical = eff.base_font_logical;
        // 渲染器的基准字号也要跟上：没显式带字号的节点回落到它（`View` 取 `tr.base_size()`），
        // 只在换 DPI 时更新的话，改字号/换主题后这些节点会停在旧值直到下次换显示器。
        self.text_renderer
            .set_base_size(self.base_font_logical * self.scale);
        self.theme = t;
        if let Some(tip) = self.tooltip.as_mut() {
            tip.set_candidate_font(self.base_font_logical);
        }
    }

    /// 主题各节点声明的字族逐个查一次存在性，缺的记 warn。见 [`Self::resolve_and_report`]。
    ///
    /// 主题作者机器上装着的字体换台机器就静默回落，这里给出日志线索。只在换主题时走
    /// （`push_theme` 的低频事件），不在渲染热路径。按字族去重，留第一个声明它的节点路径。
    fn warn_missing_theme_families(&self, declared: &[(String, String)]) {
        let mut seen = std::collections::HashSet::new();
        for (path, family) in declared {
            if seen.insert(family.clone()) {
                self.resolve_and_report(family, &format!("主题节点 {path}.font_family"));
            }
        }
    }

    /// 一帧候选窗的完整状态。参数即协调器下发的字段本身，包成结构体只会在 IPC 解包与
    /// 本调用之间多一次搬运。
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        preedit: &str,
        preedit_caret: usize,
        mode_label: &str,
        candidates: Vec<CandidateItem>,
        selected: usize,
        hover: i32,
        page: usize,
        total_pages: usize,
    ) {
        self.preedit = preedit.to_string();
        self.preedit_caret = Self::clamp_caret(preedit, preedit_caret);
        self.mode_label = mode_label.to_string();
        self.candidates = candidates;
        self.selected = selected;
        self.hover = hover;
        self.page = page.max(1);
        self.total_pages = total_pages.max(1);
    }

    /// 展开态版式（`None` = 收起）。
    pub fn set_grid(&mut self, grid: Option<wind_ui_types::CandidateGrid>) {
        // 刚展开：窗口原地往下长，新长出来的格子可能正好落在静止不动的鼠标底下，系统随之补发
        // 一次移动消息 ⇒ 用户什么都没做，悬停高亮与提示就冒出来了。与窗口刚显示同理
        // （`mark_visible`），从这一刻重新起算「鼠标动过没有」。
        if self.grid.is_none() && grid.is_some() && self.visible {
            self.mouse.borrow_mut().reset_hover();
        }
        self.grid = grid;
    }

    /// 记下展开态这一帧的内容盒，供悬停气泡停靠（见 [`Self::grid_box`]）。`(left, top)` 是
    /// 内容区在窗口里的左上（软阴影扩边）。
    fn note_grid_box(&self, (left, top): (f32, f32), w: f32) {
        if self.grid.is_some() {
            // 气泡贴的是窗口实际的右缘：渲染入口随后会按屏幕安全宽度收窄窗口，超宽的网格
            // 按测得宽度记会让气泡离窗口右缘空出一截。与入口那道钳制同一个上限。
            let shown = w.min(self.screen_safety_max_width_px() as f32);
            self.grid_box.set((left, left + shown, top));
        }
    }

    /// 此刻是否按展开态（网格）画。判据与 `build_tree` 里的 `grid` 同源。
    fn grid_shown(&self) -> bool {
        self.grid.is_some_and(|g| g.cols > 0)
            && !self.vertical
            && !self.rotated
            && !self.candidates.is_empty()
    }

    /// 悬停气泡的锚点：`(放侧边?, 左缘, 右缘, 顶, 底)`，窗口内坐标。
    ///
    /// - 展开态：停靠在整个窗口外侧的顶端（右侧优先、放不下改左），不随悬停行移动，只换内容
    ///   （按单格放会盖住相邻的格子）。
    /// - 竖排 / 旋转态：候选项侧边（旋转态的候选项是又高又窄的一列，放上下会离得很远）。
    /// - 横排：候选项下方（不足时上翻）。
    ///
    /// 三处定位（`update_tooltip`、`render_tooltip_frame`、`render_tooltip_overlay`）都从这里取，
    /// 否则同一个悬停在实时显示与重推帧之间会跳位置。
    fn tip_anchor(&self, r: &Rect) -> (bool, f32, f32, f32, f32) {
        if self.grid_shown() {
            let (l, rr, top) = self.grid_box.get();
            return (true, l, rr, top, top + r.h);
        }
        (
            self.vertical || self.rotated,
            r.x,
            r.x + r.w,
            r.y,
            r.y + r.h,
        )
    }

    pub fn set_position(&mut self, x: i32, y: i32, caret_height: i32, caret_valid: bool) {
        self.x = x;
        self.y = y;
        self.caret_height = caret_height;
        self.caret_valid = caret_valid;
    }

    /// 设置固定位置模式：`Some((x, y))`=固定在该内容左上屏幕坐标，`None`=跟随光标。
    pub fn set_fixed_position(&mut self, pos: Option<(i32, i32)>) {
        self.fixed_pos = pos;
    }

    /// 当前候选窗内容左上屏幕坐标（窗口左上 + 阴影扩边）。
    /// 供「定位方式」切到 fixed 时把当前实际位置落盘成 custom_x/custom_y。
    pub fn content_origin(&self) -> (i32, i32) {
        let m = self.mouse.borrow();
        let w = m.window_origin().unwrap_or((0, 0));
        Self::window_to_content(w, m.margin.0, m.margin.1)
    }

    /// 候选页内命中矩形（绝对坐标，相对窗口左上角）
    pub fn hit_rects(&self) -> &[(i32, Rect)] {
        &self.hit_rects
    }

    /// Windows：show 直接复用 render_frame() 渲染结果，blit 到本地 LayeredWindow。
    /// 与 host-render 路径共用单一渲染逻辑，确保几何完全一致。
    #[cfg(windows)]
    pub fn show(&mut self) {
        let t_start = Instant::now();
        self.split_pass_decor = !self.pass_window_failed;
        let frame = self.render_frame();
        self.split_pass_decor = false;
        let t_render = t_start.elapsed();
        match frame {
            None => {
                self.hide();
            }
            Some(frame) => {
                let len = (frame.width * frame.height * 4) as usize;
                let want = (frame.screen_x, frame.screen_y, frame.width, frame.height);
                let same_pixels = self.window.buffer() == &frame.buf[..len];
                let (repaint, reposition) = plan_present(self.presented, want, same_pixels);
                let t_blit0 = Instant::now();
                let mut ok = true;
                if repaint {
                    self.window.resize(frame.width, frame.height);
                    self.window.buffer_mut()[..len].copy_from_slice(&frame.buf[..len]);
                    if let Err(e) = self.window.update() {
                        tracing::warn!("CandidateWindow update failed: {}", e);
                        ok = false;
                    }
                }
                let t_blit = t_blit0.elapsed();
                // render_frame() 已设 visible=true；screen_x/y 为窗口左上（含阴影偏移）。
                let t_pos0 = Instant::now();
                if reposition {
                    self.window.show(frame.screen_x, frame.screen_y);
                }
                self.present_pass_layer(frame.screen_x, frame.screen_y, frame.width, frame.height);
                let t_pos = t_pos0.elapsed();
                self.presented = ok.then_some(want);
                if self.anim_repaint {
                    return;
                }
                self.update_tooltip(frame.screen_x, frame.screen_y);
                tracing::debug!(
                    "render[{}x{} n={}] pos=({},{}) repaint={} reposition={} rdp={}: render={:?} blit={:?} setpos={:?} | total={:?}",
                    frame.width,
                    frame.height,
                    self.candidates.len(),
                    frame.screen_x,
                    frame.screen_y,
                    repaint,
                    reposition,
                    unsafe {
                        windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                            windows::Win32::UI::WindowsAndMessaging::SM_REMOTESESSION,
                        ) != 0
                    },
                    t_render,
                    t_blit,
                    t_pos,
                    t_start.elapsed()
                );
            }
        }
    }

    #[cfg(not(windows))]
    pub fn show(&mut self) {
        // DPI 动态化：按光标所在显示器实时取缩放，换显示器后自动按新 DPI 渲染。
        // 本路径一直按光标点取，不像另两个渲染入口那样在固定位置模式下改按固定点取；
        // 三份管线合并时保持原样，没有顺手统一。
        let new_scale = crate::dpi::scale_for_point(self.x, self.y);
        self.show_at(new_scale, Instant::now());
    }

    /// 同 [`Self::show`]，DPI 缩放与动画时刻由调用方给（金样注入用）。
    #[cfg(not(windows))]
    fn show_at(&mut self, new_scale: f32, now: Instant) {
        // mode_label 非空表示已进入临时模式：即使暂无候选/preedit 也要弹窗显示模式标记。
        if self.candidates.is_empty() && self.preedit.is_empty() && self.mode_label.is_empty() {
            self.hide();
            return;
        }

        // 几何/字号全部由 self.scale 派生（build_tree 中现算），更新此值即生效。
        if (new_scale - self.scale).abs() > 0.01 {
            self.scale = new_scale;
            self.text_renderer
                .set_base_size(self.base_font_logical * new_scale);
        }

        // ── 渲染计时（定位长按翻页卡顿耗时段）──
        let t_start = Instant::now();

        // 排版（皮肤几何 → 搭树 → 布局 → 宽度钳制），本路径画软件阴影。
        let mut frame = self.layout_frame(now, true);
        let (ml, mt, _, _) = frame.margins;
        let (content_w, content_h) = (frame.content_w, frame.content_h);
        let (width, height) = frame.size();
        let t_layout = t_start.elapsed();

        // 定位提前到 paint 前（供 flip_when_above 判定）：每帧据当前光标 + 内容尺寸重算位置
        // （place_window 负责工作区钳制、上方底边参考、上翻粘滞），再用 4px 阈值抑制宿主 caret
        // 微抖动。新组合首显已由协调器延迟到 reflow 后的权威坐标。
        let offset = self.position_offset_px();
        let (px0, py0, above) = Self::place_window(
            self.x,
            self.y,
            self.caret_height,
            content_w,
            content_h,
            self.placed_above,
            offset,
            &self.scene.skin_place_anchor(1.0, self.scale),
        );
        self.placed_above = above;
        // 位置稳定性：窗口已显示且新位置与上次内容锚点微移(<4px*scale)→保持原位，吞掉 caret 微抖。
        let (px, py) = match self.last_content_pos {
            Some((lx, ly)) if self.visible => {
                let thr = (4.0 * self.scale).round().max(1.0) as i32;
                if (px0 - lx).abs() < thr && (py0 - ly).abs() < thr {
                    (lx, ly)
                } else {
                    (px0, py0)
                }
            }
            _ => (px0, py0),
        };
        self.last_content_pos = Some((px, py));
        // 上方显示且有开关真正生效 → 按上方布局重建候选树（项数/尺寸/位置不变，仅排列翻转）
        self.report_flip_state();
        self.scene
            .relayout_above(&self.scene_input(self.placed_above), &mut frame);

        // 收集候选命中矩形并同步给鼠标处理器（须在 flip 重建后）
        self.sync_hits(&frame);

        self.window.resize(width, height);

        // 透明清屏 + 绘制
        let t_paint0 = Instant::now();
        {
            let buf = self.window.buffer_mut();
            let buf_size = (width * height * 4) as usize;
            buf[..buf_size].fill(0);
            self.scene.paint(&frame, buf, &self.text_renderer, false);
        }
        let t_paint = t_paint0.elapsed();

        let t_blit0 = Instant::now();
        if let Err(e) = self.window.update() {
            tracing::warn!("CandidateWindow update failed: {}", e);
        }
        let t_blit = t_blit0.elapsed();

        // 搭树与布局已并进场景的一次 `layout`，这里只报两者合计。
        tracing::debug!(
            "render[{}x{} n={}]: layout={:?} paint={:?} blit={:?} | total={:?}",
            width,
            height,
            self.candidates.len(),
            t_layout,
            t_paint,
            t_blit,
            t_start.elapsed()
        );

        // 位置 (px, py) 已在 paint 前计算并锚定（组合期锁定后复用，避免漂移）。
        // 窗口实际左上 = 内容锚点 − 左/上 margin，使内容仍落在锚点处，阴影向四周溢出。
        // 基线采样刻意排在 `show` 之前：窗口出现不会移动鼠标，两处取值物理上相同，
        // 但排在前面就完全不依赖「`show` 内部不会泵到 WM_MOUSEMOVE」这个前提。
        self.mark_visible();
        self.window.show(px - ml as i32, py - mt as i32);
        let t_tip0 = Instant::now();
        // 命中矩形是窗口缓冲坐标（内容起点在 (ml,mt)）；tooltip 定位须用窗口屏幕原点。
        if !self.anim_repaint {
            self.update_tooltip(px - ml as i32, py - mt as i32);
        }
        let t_tip = t_tip0.elapsed();
        if t_tip.as_micros() > 200 {
            tracing::debug!("render tooltip={:?}", t_tip);
        }
    }

    /// macOS host-render：把当前候选状态渲染到一块独立 BGRA 缓冲区并返回，
    /// 不依赖 Win32 Layered Window（forwarder 取此 buffer 写 POSIX SHM + push 给 .app）。
    /// 镜像 `show()` 的 build/layout/place/flip/paint 流程，但 paint 到自有 Vec。
    /// 返回 None 表示应隐藏候选窗（无候选 / 无 preedit / 无模式标记）。
    #[cfg(ext_presenter)]
    pub fn render_frame(&mut self) -> Option<RenderedFrame> {
        if self.candidates.is_empty() && self.preedit.is_empty() && self.mode_label.is_empty() {
            return None;
        }

        // DPI：按光标所在屏取缩放（Retina=2），几何/字号由 self.scale 派生。
        // 固定位置模式按固定点取——固定位置可能落在与光标不同缩放的另一块屏上，
        // 按光标算会让窗口用错误的 DPI 渲染（字号忽大忽小）。与 Windows 分支同一判据。
        let (dpi_x, dpi_y) = match self.fixed_pos {
            Some(f) if f != (0, 0) => f,
            _ => (self.x, self.y),
        };
        let new_scale = crate::dpi::scale_for_point(dpi_x, dpi_y);
        if (new_scale - self.scale).abs() > 0.01 {
            self.scale = new_scale;
            self.text_renderer
                .set_base_size(self.base_font_logical * new_scale);
        }

        // macOS 改用系统原生 NSPanel 阴影（.app 端 panel.hasShadow + invalidateShadow，自动沿
        // 圆角内容形状投影），位图内【不画软件阴影】：margin 归零 → 定位精确贴 caret、无偏移，
        // 观感也更 native。software_shadow=false → Swift 侧启用系统窗口阴影。
        let mut frame = self.layout_frame(Instant::now(), false);
        let (ml, mt, mr, mb) = frame.margins;
        let (content_w, content_h) = (frame.content_w, frame.content_h);
        let (width, height) = frame.size();

        // ── 定位：fixed_pos（用户固定位置）> place_window（跟随光标）──
        //
        // Windows 那边还有第三级 `drag_pin`（本次组合内冻结拖动落位）。macOS 的拖动发生在
        // `.app` 侧的 NSPanel 上，服务进程这边的 mouse handler 是不产生事件的 mock，
        // `drag_pin` 恒为 None——会话内的落位冻结由 `.app` 自己记（见 CandidatePanel.dragPin）。
        let (px, py, absolute, above_xy, clamp) = match self.fixed_pos {
            Some(f) => {
                // 固定位置下窗口不随光标上下移动，"上翻"随之失去意义：placed_above 必须归 false，
                // 否则 flip_when_above / swap_preedit_when_above 会让内容在一个不动的窗口里倒序。
                self.placed_above = false;
                self.last_content_pos = None;
                // 本路径坐标是逻辑点，尺寸与扩边是设备像素：先换成逻辑点再交给 place_fixed。
                // 扩边来自皮肤装饰（macOS 不画软阴影）；place_fixed 返回窗口左上，加回扩边
                // 才是内容左上，与下面跟随光标分支的 (px, py) 同义（出口处再统一减扩边）。
                let s = self.scale.max(1.0);
                let lg = |v: u32| (v as f32 / s).round() as i32;
                let (wx, wy) = Self::place_fixed(
                    f,
                    self.x,
                    self.y,
                    lg(width) as u32,
                    lg(height) as u32,
                    (lg(ml), lg(mt), lg(mr), lg(mb)),
                );
                let (cx, cy) = (wx + lg(ml), wy + lg(mt));
                (cx, cy, true, (cx, cy), (0, 0, 0, 0))
            }
            None => {
                // place_window 约定 caret_y 为光标【底端】（下方锚点=底端+gap），但 macOS .app 的
                // caretRectToWire 发的是光标行【顶端】(top-left)，故这里把行高补上传光标底端 =
                // self.y + self.caret_height，候选窗才落在光标行下方、不遮挡输入。
                let offset = self.position_offset_px();
                let anchor = self
                    .scene
                    .skin_place_anchor(1.0 / self.scale.max(1.0), self.scale);
                let (px0, py0, above) = Self::place_window(
                    self.x,
                    self.y + self.caret_height,
                    self.caret_height,
                    content_w,
                    content_h,
                    self.placed_above,
                    offset,
                    &anchor,
                );
                self.placed_above = above;
                let (px, py) = match self.last_content_pos {
                    Some((lx, ly)) if self.visible => {
                        let thr = (4.0 * self.scale).round().max(1.0) as i32;
                        if (px0 - lx).abs() < thr && (py0 - ly).abs() < thr {
                            (lx, ly)
                        } else {
                            (px0, py0)
                        }
                    }
                    _ => (px0, py0),
                };
                self.last_content_pos = Some((px, py));
                // 上方点交给宿主（它才知道下方放不放得下）。微移抑制按下方点的取舍同样平移。
                let s = self.scale.max(1.0);
                let (ax, ay) = Self::place_above(
                    self.x,
                    self.y + self.caret_height,
                    self.caret_height,
                    (content_h as f32 / s).round() as u32,
                    offset,
                    &anchor,
                );
                (px, py, false, (ax + px - px0, ay + py - py0), anchor.clamp)
            }
        };
        self.report_flip_state();
        self.scene
            .relayout_above(&self.scene_input(self.placed_above), &mut frame);

        // 命中矩形（窗口缓冲坐标，内容起点 (ml,mt)），同步给鼠标处理器。
        self.sync_hits(&frame);

        // 绘制到独立 BGRA 缓冲（透明清屏 → 内容；本路径不画软影）。穿透装饰先拆出来（§17.2）：
        // Linux 按主层算输入区再合回去，macOS 另走子 panel。
        let mut buf = vec![0u8; (width * height * 4) as usize];
        let pass = self
            .scene
            .paint(&frame, &mut buf, &self.text_renderer, true);
        let (skin, pass_layer) = self.skin_regions(&mut buf, width, height, pass);
        let shadow = frame.shadow;

        self.mark_visible();
        // 单位换算：ml/mt/blur 是设备像素，screen_x/y 是 .app 的逻辑点，补偿前必须 /scale，
        // 否则 Retina 下候选窗偏左上盖住 caret。
        let scale = self.scale.max(1.0);
        let ml_l = (ml as f32 / scale).round() as i32;
        let mt_l = (mt as f32 / scale).round() as i32;
        // 软阴影上边距是完整 3σ 高斯尾，内容紧贴 caret 时上方约一个 blur 高的浓阴影会盖住 caret，
        // 故内容再下移约 blur 高度。上翻不下移（固定位置分支本就不贴 caret）。
        let clear_l = if self.placed_above {
            0
        } else {
            (shadow.as_ref().map(|s| s.blur).unwrap_or(0.0) / scale).round() as i32
        };
        let content_top = py + clear_l;
        self.content_rect = Some((
            px,
            content_top,
            px + ((width - ml - mr) as f32 / scale).round() as i32,
            content_top + ((height - mt - mb) as f32 / scale).round() as i32,
        ));
        let content_offset = (ml_l, mt_l - clear_l);
        let (cl, ct, cr, cb) = clamp;
        let place_box = (
            content_offset.0 - cl,
            content_offset.1 - ct,
            ((width - ml - mr) as f32 / scale).round() as i32 + cl + cr,
            ((height - mt - mb) as f32 / scale).round() as i32 + ct + cb,
        );
        Some(RenderedFrame {
            // 图像含四向 margin(device)，其屏幕左上(逻辑点) = 内容锚点 − 左/上 margin/scale
            //（纵向再 +可见浓阴影下移，让阴影落在 caret 下方）。
            screen_x: px - ml_l,
            screen_y: py - mt_l + clear_l,
            width,
            height,
            scale: self.scale,
            software_shadow: shadow.is_some(),
            absolute_pos: absolute,
            hit_rects: self.hit_rects.clone(),
            buf,
            skin,
            pass_layer,
            content_offset,
            above_pos: (above_xy.0 - content_offset.0, above_xy.1 - content_offset.1),
            place_box,
        })
    }

    /// 外部宿主的皮肤区域（设计 §17.2）。`buf` 是不含穿透装饰的主层，`pass` 是拆出来的穿透装饰层。
    ///
    /// Linux：输入区按主层的 alpha 算，然后把穿透装饰合回 `buf`（同一张位图，只是不进输入区）。
    /// macOS：不发输入区（非不透明窗口按 alpha 自动穿透），穿透装饰层原样交出去给子 panel。
    #[cfg(ext_presenter)]
    fn skin_regions(
        &self,
        buf: &mut [u8],
        width: u32,
        height: u32,
        pass: Option<(Vec<u8>, bool)>,
    ) -> (wind_ipc::codec::CandidateSkinWire, Option<(Vec<u8>, bool)>) {
        use wind_ipc::codec::{CandidateSkinWire, SkinDecorWire};
        use wind_ipc::protocol::skin::*;
        use wind_theme::skin::DecorHit;
        let Some(g) = self.scene.skin_geom.as_ref() else {
            return (CandidateSkinWire::default(), pass);
        };
        let split = cfg!(target_os = "macos");
        let decor = g
            .host_decor()
            .into_iter()
            .map(|d| {
                let hit = match d.hit {
                    DecorHit::Drag => SKIN_HIT_DRAG,
                    DecorHit::Block => SKIN_HIT_BLOCK,
                    DecorHit::Pass => SKIN_HIT_PASS,
                };
                let mut flags = 0;
                if d.under {
                    flags |= SKIN_DECOR_UNDER;
                }
                if split && d.hit == DecorHit::Pass {
                    flags |= SKIN_DECOR_SPLIT;
                }
                SkinDecorWire {
                    index: d.index as u32,
                    hit,
                    flags,
                    rect: d.rect,
                }
            })
            .collect();
        let f = g.frame_rect();
        let mut wire = CandidateSkinWire {
            flags: SKIN_FLAG_ACTIVE,
            frame: [f.x as i32, f.y as i32, f.w as i32, f.h as i32],
            decor,
            mask: Vec::new(),
        };
        if split {
            return (wire, pass);
        }
        wire.flags |= SKIN_FLAG_MASK;
        wire.mask =
            scene::opaque_rects(buf, width, height, scene::MAX_INPUT_RECTS).unwrap_or_else(|| {
                tracing::debug!("skin: 输入区矩形超过上限，退化为窗体与装饰矩形");
                g.coarse_input_rects()
            });
        if let Some((p, below)) = pass {
            if below {
                scene::composite_under(buf, &p);
            } else {
                scene::composite_over(buf, &p);
            }
        }
        (wire, None)
    }

    /// Windows host-render：把当前候选状态渲染到一块独立 BGRA 缓冲区并返回，
    /// 供 host-render 管理器写 SHM + 触发 Event，DLL 侧 HostWindow 读取并显示。
    /// 镜像 `show()` 的 build/layout/place/collect_hits/paint 流程，但 paint 到自有 Vec，
    /// 不调 Win32 LayeredWindow API（写 SHM 路径不需要本地窗口）。
    /// 返回 None 表示应隐藏候选窗（无候选 / 无 preedit / 无模式标记）。
    #[cfg(windows)]
    pub fn render_frame(&mut self) -> Option<RenderedFrame> {
        if self.candidates.is_empty() && self.preedit.is_empty() && self.mode_label.is_empty() {
            return None;
        }

        // DPI 探测点：固定位置模式按固定点取缩放，而不是光标点——固定位置可能落在
        // 与光标不同缩放的另一块屏上，按光标算会让窗口用错误的 DPI 渲染（字号忽大忽小）。
        let (dpi_x, dpi_y) = match self.fixed_pos {
            Some(f) if f != (0, 0) => f,
            _ => (self.x, self.y),
        };
        let new_scale = crate::dpi::scale_for_point(dpi_x, dpi_y);
        if (new_scale - self.scale).abs() > 0.01 {
            self.scale = new_scale;
            self.text_renderer
                .set_base_size(self.base_font_logical * new_scale);
        }

        let mut frame = self.layout_frame(Instant::now(), true);
        let (ml, mt, mr, mb) = frame.margins;
        let (content_w, content_h) = (frame.content_w, frame.content_h);
        let (width, height) = frame.size();
        // 正常情况下 `build_tree` 已按同一套预算把文字裁到位，屏幕安全上限的钳制是恒不生效的
        // 兜底。只在它真的生效时才记日志——那意味着预算算漏了行内某个成员（这正是横排内联
        // 编码/徽标/翻页栏曾被漏算的形态），是需要看见的异常，不是每帧噪音。
        // 只有本路径记这条（三份管线合并前就如此，非 Windows 两条路径从来不记），保持原样。
        if let Some((natural, safety_max_w)) = frame.safety_clamped {
            tracing::debug!(
                "candidate width safety-clamp 生效（预算漏算？）: natural={} safety_max_content={} clamped={} margin_lr={} scale={:.2} vertical={}",
                natural,
                safety_max_w,
                content_w,
                ml + mr,
                self.scale,
                self.vertical
            );
        }

        // ── 定位：三级优先级 drag_pin > fixed_pos > place_window(跟随光标) ──
        // 用户已手动拖动过本次组合的候选窗 → 冻结落位：位置固定、上/下方排列也不再翻转，
        // 否则窗口停在原处但内容突然倒序，视觉上会"自己变了个样"。
        // 落位按写入时的扩边记；换算到本帧扩边（无皮肤时两者相等，原样不变）。
        let drag_pin = {
            let m = self.mouse.borrow();
            m.drag_pin.map(|(x, y)| {
                (
                    x + m.drag_pin_margin.0 - ml as i32,
                    y + m.drag_pin_margin.1 - mt as i32,
                )
            })
        };
        let mut screen_xy: Option<(i32, i32)> = match (drag_pin, self.fixed_pos) {
            // 拖动落定：placed_above 保持不变（同上，避免窗口不动而内容倒序）。
            (Some(p), _) => Some(p),
            // 固定位置模式：窗口不再随光标上下移动，"上翻"随之失去意义 —— placed_above
            // 必须归 false，否则 flip_when_above / swap_preedit_when_above 会让内容
            // 在一个位置固定的窗口里莫名倒序。
            (None, Some(f)) => {
                self.placed_above = false;
                self.last_content_pos = None;
                Some(Self::place_fixed(
                    f,
                    self.x,
                    self.y,
                    width,
                    height,
                    (ml as i32, mt as i32, mr as i32, mb as i32),
                ))
            }
            // 换帧重绘：内容没变，沿用上一帧落位（拖动中走上面的 drag_pin 分支）。
            (None, None) if self.anim_repaint && self.visible => self
                .last_content_pos
                .map(|(px, py)| (px - ml as i32, py - mt as i32)),
            (None, None) => None,
        };
        if screen_xy.is_none() {
            // Windows 的 self.y 已是光标底端（与 show() 语义一致），直接传入。
            // 主题位置偏移只在这条「跟随光标」分支叠加——上面两个分支是用户显式定位。
            let offset = self.position_offset_px();
            let (px0, py0, above) = Self::place_window(
                self.x,
                self.y,
                self.caret_height,
                content_w,
                content_h,
                self.placed_above,
                offset,
                &self.scene.skin_place_anchor(1.0, self.scale),
            );
            self.placed_above = above;
            let (px, py) = match self.last_content_pos {
                Some((lx, ly)) if self.visible => {
                    let thr = (4.0 * self.scale).round().max(1.0) as i32;
                    if (px0 - lx).abs() < thr && (py0 - ly).abs() < thr {
                        (lx, ly)
                    } else {
                        (px0, py0)
                    }
                }
                _ => (px0, py0),
            };
            self.last_content_pos = Some((px, py));
            screen_xy = Some((px - ml as i32, py - mt as i32));
        }
        self.report_flip_state();
        self.scene
            .relayout_above(&self.scene_input(self.placed_above), &mut frame);

        self.sync_hits(&frame);

        let mut buf = vec![0u8; (width * height * 4) as usize];
        // 穿透装饰：本地窗口路径（show，穿透窗口建得出来时）与能画穿透层的 DLL host-render
        // （render_host_frame）拆到独立的穿透窗口；其余照旧画进主缓冲（按 block 处理）。
        self.pass_layer =
            self.scene
                .paint(&frame, &mut buf, &self.text_renderer, self.split_pass_decor);
        let shadow = frame.shadow;

        self.mark_visible();
        // 上方三级定位分支穷尽，此处必为 Some；debug 下断言，release 兜底到光标处而非
        // (0,0)——真出现逻辑漏洞时窗口至少还在光标附近，不会莫名飞到屏幕左上角。
        debug_assert!(screen_xy.is_some(), "定位三分支必有其一赋值 screen_xy");
        let (screen_x, screen_y) = screen_xy.unwrap_or((self.x, self.y));
        // 鼠标层记录阴影扩边，两处要用：
        // 1. 拖动落定时把窗口左上换算回内容左上再上报落盘，否则每次「拖动→保存→
        //    重显」都会多减一次阴影，窗口逐次漂移；
        // 2. 拖动中按内容矩形钳制，让可见内容能真正贴到屏幕边缘（含任务栏）。
        self.mouse.borrow_mut().margin = (ml as i32, mt as i32, mr as i32, mb as i32);
        self.content_rect = Some((
            screen_x + ml as i32,
            screen_y + mt as i32,
            screen_x + width as i32 - mr as i32,
            screen_y + height as i32 - mb as i32,
        ));
        Some(RenderedFrame {
            screen_x,
            screen_y,
            width,
            height,
            scale: self.scale,
            software_shadow: shadow.is_some(),
            hit_rects: self.hit_rects.clone(),
            buf,
            pass_layer: None,
        })
    }

    /// DLL host-render 的出帧口：`split_pass` 时把穿透装饰拆进 [`RenderedFrame::pass_layer`]（DLL 声明
    /// 了 `HOST_RENDER_CAP_PASS_LAYER`），否则合在主层里。
    #[cfg(windows)]
    pub fn render_host_frame(&mut self, split_pass: bool) -> Option<RenderedFrame> {
        self.split_pass_decor = split_pass;
        let frame = self.render_frame();
        self.split_pass_decor = false;
        let pass = self.pass_layer.take();
        frame.map(|f| RenderedFrame {
            pass_layer: pass,
            ..f
        })
    }

    /// 气泡该跟哪个悬停目标：候选右键压制期间一律「无」（高亮照旧，只是不弹气泡）。
    fn tooltip_hover(&self) -> i32 {
        if self.mouse.borrow().tip_hold.active() {
            -1
        } else {
            self.hover
        }
    }

    /// 悬停时在该候选下方显示其编码（反查）；无悬停或无编码则隐藏。
    /// `(wx, wy)` 为候选窗口屏幕原点（命中矩形坐标的基准）。
    /// 横排：tooltip 在候选行下方（不足时上翻）。
    /// 竖排：tooltip 在候选窗右侧（不足时左侧），纵向对齐悬停候选行，避免遮挡下方候选。
    fn update_tooltip(&mut self, wx: i32, wy: i32) {
        let hover = self.tooltip_hover();
        // 仅候选项（非翻页器 tag）显示反查提示
        let info = if (0..TAG_PAGE_PREV).contains(&hover) {
            let code = self
                .candidates
                .get(hover as usize)
                .map(|c| c.tooltip.clone())
                .unwrap_or_default();
            self.hit_rects
                .iter()
                .find(|(t, _)| *t == hover)
                .map(|(_, r)| *r)
                .filter(|_| !code.is_empty())
                .map(|r| (code, r))
        } else {
            None
        };
        // 锚点先算好：下面要可变借用气泡，`tip_anchor` 读的是窗口自身的状态。
        let anchor = info.as_ref().map(|(_, r)| self.tip_anchor(r));
        if let Some(tip) = self.tooltip.as_mut() {
            match info {
                Some((code, r)) => {
                    // 旋转态一并走「侧边」：它的候选项是又高又窄的一列，
                    // 按横排的「上方/下方」放会离得很远。
                    self.tip_for.set(hover);
                    tip.set_anchor((
                        wx + r.x as i32,
                        wy + r.y as i32,
                        wx + (r.x + r.w) as i32,
                        wy + (r.y + r.h) as i32,
                    ));
                    let (beside, x0, x1, y0, y1) =
                        anchor.unwrap_or((false, r.x, r.x + r.w, r.y, r.y + r.h));
                    if beside {
                        // 侧边：锚点见 `tip_anchor`（hit rect 已含阴影偏移，wx+x 即屏幕
                        // 坐标）。显示在右侧，空间不足时改左侧，不遮挡下方候选。
                        tip.show_beside(
                            &code,
                            hover,
                            wx + x0 as i32,
                            wx + x1 as i32,
                            wy + y0 as i32,
                            wy + y1 as i32,
                        );
                    } else {
                        tip.show(
                            &code,
                            hover,
                            wx + r.x as i32,
                            wy + r.y as i32,
                            wy + (r.y + r.h) as i32,
                        );
                    }
                    self.mouse.borrow_mut().tip_rect = tip
                        .content_rect()
                        .map(|(l, t, r, b)| (l - wx, t - wy, r - wx, b - wy));
                }
                None => {
                    self.tip_for.set(-1);
                    tip.hide();
                }
            }
        }
    }

    /// 当前悬停 tooltip 的目标：`(气泡内容, 候选行命中矩形, 悬停下标)`；无悬停 / 无内容为 None。
    /// 顺带记下 `tip_for`（气泡归属哪个候选）。host-render 两条渲染路径共用。
    #[cfg(any(windows, ext_presenter))]
    fn tooltip_target(&mut self) -> Option<(std::sync::Arc<wind_ui_types::TooltipDoc>, Rect, i32)> {
        let hover = self.tooltip_hover();
        let info = if (0..TAG_PAGE_PREV).contains(&hover) {
            let code = self
                .candidates
                .get(hover as usize)
                .map(|c| c.tooltip.clone())
                .unwrap_or_default();
            self.hit_rects
                .iter()
                .find(|(t, _)| *t == hover)
                .map(|(_, r)| *r)
                .filter(|_| !code.is_empty())
                .map(|r| (code, r, hover))
        } else {
            None
        };
        self.tip_for.set(if info.is_some() { hover } else { -1 });
        info
    }

    /// host-render 专用：渲染当前悬停 tooltip 到 BGRA buffer。
    /// `(wx, wy)` 为候选窗口屏幕原点（与 render_frame 的 screen_x/y 一致）。
    /// 返回 `(bgra, w, h, screen_x, screen_y, software_shadow)`；无悬停/无文本返回 None。
    #[cfg(windows)]
    pub fn render_tooltip_frame(
        &mut self,
        wx: i32,
        wy: i32,
    ) -> Option<(Vec<u8>, u32, u32, i32, i32, bool)> {
        let info = self.tooltip_target();
        // 判据见 `tip_anchor`（与 `update_tooltip` 同源）；先于可变借用气泡算好。
        let anchor = info.as_ref().map(|(_, r, _)| self.tip_anchor(r));
        let tip = self.tooltip.as_mut()?;
        match info {
            Some((code, r, hover)) => {
                let (beside, x0, x1, y0, y1) =
                    anchor.unwrap_or((false, r.x, r.x + r.w, r.y, r.y + r.h));
                if beside {
                    tip.render_frame_beside(
                        &code,
                        hover,
                        wx + x0 as i32,
                        wx + x1 as i32,
                        wy + y0 as i32,
                        wy + y1 as i32,
                    )
                } else {
                    tip.render_frame(
                        &code,
                        hover,
                        wx + r.x as i32,
                        wy + r.y as i32,
                        wy + (r.y + r.h) as i32,
                    )
                }
            }
            None => None,
        }
    }

    /// Linux 外部宿主：渲染当前悬停 tooltip（位图 + 落位规则）。`(wx, wy)` 为候选窗建议
    /// 落点（`render_frame` 的 screen_x/y），addon 再平移到候选窗实际位置。侧边 / 上下的判据
    /// 同 [`Self::render_tooltip_frame`]。
    #[cfg(ext_presenter)]
    pub(crate) fn render_tooltip_overlay(
        &mut self,
        wx: i32,
        wy: i32,
    ) -> Option<crate::overlay_host::Overlay> {
        let (code, r, hover) = self.tooltip_target()?;
        let (beside, x0, x1, y0, y1) = self.tip_anchor(&r);
        // 命中矩形是位图像素，`(wx, wy)` 是宿主坐标单位（macOS 为点），先折成同一单位。
        let u = |px: f32| crate::overlay_host::to_host(px, self.scale);
        let row = (wx + u(x0), wy + u(y0), wx + u(x1), wy + u(y1));
        self.tooltip
            .as_mut()?
            .render_overlay(&code, hover, row, beside)
    }

    /// Linux 外部宿主：右键点在提示位图内 `(cx, cy)` → `(候选页内下标, 命中, 指纹)`，
    /// 见 [`Tooltip::menu_request_at`]。气泡渲染器没建起来时为 `None`。
    #[cfg(ext_presenter)]
    pub(crate) fn tooltip_menu_request_at(
        &self,
        cx: i32,
        cy: i32,
    ) -> Option<(i32, Option<wind_ui_types::TooltipHit>, u64)> {
        Some(self.tooltip.as_ref()?.menu_request_at(cx, cy))
    }

    /// 内容左上 → 窗口左上（减去软阴影扩边）。
    ///
    /// 落盘的 custom_x/y 记的是内容左上，Win32 定位用窗口左上。本函数与 `window_to_content`
    /// 必须严格互逆，否则每轮「拖动 → 落盘 → 重新显示」候选窗逐次向左上漂移。
    ///
    /// 纯算术，不加 cfg 门控（未门控的 `content_origin` 要调它）；Linux 下无调用者故
    /// allow(dead_code)，与 `window_to_content` 成对存在。
    #[cfg_attr(not(any(windows, ext_presenter)), allow(dead_code))]
    fn content_to_window(content: (i32, i32), ml: u32, mt: u32) -> (i32, i32) {
        (content.0 - ml as i32, content.1 - mt as i32)
    }

    /// 窗口左上 → 内容左上（加回软阴影扩边）。`content_to_window` 的逆。
    fn window_to_content(window: (i32, i32), ml: i32, mt: i32) -> (i32, i32) {
        (window.0 + ml, window.1 + mt)
    }

    /// 固定位置模式的窗口落点（返回窗口左上屏幕坐标，含阴影扩边）。
    ///
    /// - `fixed` 是内容左上坐标，减去阴影扩边 `(ml, mt)` 才是窗口左上。
    /// - `(0, 0)` 视作「已开启固定但尚未设定位置」，落到默认锚点。
    /// - 钳制按内容矩形、钳到整块屏幕（含任务栏）：换分辨率或拔掉副屏后 `custom_x/y` 可能指向
    ///   不可见区域；按含阴影的窗口矩形去钳会差一个阴影宽，见 [`clamp_content_to_monitor`]。
    ///
    /// macOS 同样走这里：不画软阴影、扩边恒 0，`clamp_content_to_monitor` 在非 Windows 是恒等
    /// 函数，真正的屏幕钳制由 `.app` 用 `NSScreen.visibleFrame` 做。
    #[cfg_attr(not(any(windows, ext_presenter)), allow(dead_code))]
    fn place_fixed(
        fixed: (i32, i32),
        caret_x: i32,
        caret_y: i32,
        width: u32,
        height: u32,
        margin: (i32, i32, i32, i32),
    ) -> (i32, i32) {
        let (ml, mt, _, _) = margin;
        let (wx, wy) = if fixed == (0, 0) {
            let content_w = (width as i32 - ml - margin.2).max(1) as u32;
            let (cx, cy) = Self::default_fixed_anchor(caret_x, caret_y, content_w);
            (cx - ml, cy - mt)
        } else {
            Self::content_to_window(fixed, ml as u32, mt as u32)
        };
        clamp_content_to_monitor(wx, wy, width, height, margin)
    }

    /// 固定模式尚未设定位置时的默认锚点（内容左上屏幕坐标）。
    ///
    /// 取光标所在显示器的工作区（`rcWork`，自动落位避开任务栏），水平居中、顶端落在约 3/4
    /// 高度处（贴底会与任务栏挤、居中会挡正文），超出底部由调用方的钳制拉回。
    ///
    /// 非 Windows 退化为落在光标处：服务进程查不到 macOS 的显示器几何，只影响还没拖过的那一瞬。
    #[cfg_attr(not(windows), allow(unused_variables))]
    fn default_fixed_anchor(caret_x: i32, caret_y: i32, width: u32) -> (i32, i32) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::POINT;
            use windows::Win32::Graphics::Gdi::{
                GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
            };
            unsafe {
                let pt = POINT {
                    x: caret_x,
                    y: caret_y,
                };
                let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(mon, &mut mi).as_bool() {
                    let wa = mi.rcWork;
                    let x = wa.left + ((wa.right - wa.left) - width as i32) / 2;
                    let y = wa.top + ((wa.bottom - wa.top) * 3) / 4;
                    return (x, y);
                }
            }
        }
        (caret_x, caret_y)
    }

    #[cfg_attr(not(windows), allow(unused_variables, unused_mut))]
    /// 候选窗相对光标的锚点计算（纯函数，不含屏幕钳制）：返回 `(x, below_y, above_y)`。
    ///
    /// `caret_y` 为光标底端。下方锚点 = 底端 + gap；上方锚点以窗口底边贴光标顶端为参考。
    /// 主题偏移 `off_y` 语义恒为「远离光标」：下方 +、上方 −。抽成纯函数为了可测
    /// （`place_window` 其余部分依赖真实显示器）。
    fn caret_anchors(
        caret_x: i32,
        caret_y: i32,
        caret_h: i32,
        window_h: i32,
        off_x: i32,
    ) -> (i32, i32, i32) {
        let gap = 2;
        let below_y = caret_y + gap;
        let above_y = caret_y - caret_h.max(0) - window_h - gap;
        (caret_x + off_x, below_y, above_y)
    }

    /// 按最终方位施加主题 Y 偏移：`off_y` 正值恒为远离光标（在下方向下、在上方向上）。
    ///
    /// 必须在方位已定之后调用，不能预先加进锚点：锚点含偏移会让 `below_ok`/`above_ok` 越难
    /// 成立，本该上翻的窗口落回下方被钳到屏幕底、压在光标上。偏移只改距离，不改方位决策。
    fn apply_offset_y(y: i32, above: bool, off_y: i32) -> i32 {
        if above { y - off_y } else { y + off_y }
    }

    /// 皮肤避让：上下方位与偏移都定了之后，若窗体或装饰的不透明像素落进光标行保护带
    /// （`caret_y - caret_h .. caret_y`，上下各留 gap；横向范围已在 `mask` 里取好），把窗口沿
    /// 远离光标的方向逐像素推开到保护带清空为止。透明区越过光标行不算遮挡。
    fn avoid_caret_line(
        y: i32,
        above: bool,
        caret_y: i32,
        caret_h: i32,
        mask: Option<&scene::AvoidMask>,
    ) -> i32 {
        let Some(m) = mask else {
            return y;
        };
        let gap = 2;
        let (line_top, line_bottom) = (caret_y - caret_h.max(0) - gap, caret_y + gap);
        let step = if above { -1 } else { 1 };
        let mut y = y;
        // 推够 mask 的总行数 + 保护带高度，必然清空；上限防坏数据死循环。
        for _ in 0..=(m.rows() + (line_bottom - line_top)).max(0) {
            if !m.any(line_top - y, line_bottom - y) {
                break;
            }
            y += step;
        }
        y
    }

    /// 翻到光标上方时的内容左上（钳制之前）：皮肤上方对齐点（`caret_above`）落在光标顶端 − gap，
    /// 偏移向上（远离光标），再按上方保护带避让。Windows 在 `place_window` 里判定上翻后走这里；
    /// 外部宿主拿不到屏幕几何，服务端把这个点连同下方点一起交给宿主选（`RenderedFrame::above_pos`）。
    #[cfg_attr(not(any(windows, ext_presenter)), allow(dead_code))]
    fn place_above(
        caret_x: i32,
        caret_y: i32,
        caret_h: i32,
        h: u32,
        offset: (i32, i32),
        anchor: &scene::PlaceAnchor,
    ) -> (i32, i32) {
        let (ax, _, above_y) = Self::caret_anchors(caret_x, caret_y, caret_h, h as i32, offset.0);
        let y = Self::apply_offset_y(above_y + anchor.above.1, true, offset.1);
        let y = Self::avoid_caret_line(y, true, caret_y, caret_h, anchor.avoid_above.as_ref());
        (ax - anchor.above.0, y)
    }

    /// 跟随光标定位：据冻结光标锚点与当前内容尺寸计算候选窗位置，钳制在光标所在显示器工作区内。
    /// `offset` 为主题 `window.position_offset`（设备像素，已 ×scale）。
    /// - 默认显示在光标下方（+gap）；下方空间不足则上翻到光标上方。
    /// - 上方以「窗口底边贴近光标顶端」为参考，底边与高度无关，候选变少时不会离光标变远。
    /// - `sticky_above`=true 时优先保持上方，仅当上方也放不下才回落下方，避免上下抖动。
    /// - 左右溢出则贴边。
    ///
    /// 返回 (x, y, above)：above=true 表示窗口被上翻到光标上方（供 flip_when_above 判定）。
    /// 固定位置见 [`Self::place_fixed`]。
    ///
    /// 偏移必须在此函数内注入，不能在调用方给返回值加：`rcWork` 越界兜底要在偏移之后跑。
    /// 翻转/钳制整段在 `cfg(windows)` 内，非 Windows 直接返回下方锚点（故有下面的 allow）。
    #[cfg_attr(not(windows), allow(unused_variables, unused_mut))]
    #[allow(clippy::too_many_arguments)]
    fn place_window(
        caret_x: i32,
        caret_y: i32,
        caret_h: i32,
        w: u32,
        h: u32,
        sticky_above: bool,
        offset: (i32, i32),
        anchor: &scene::PlaceAnchor,
    ) -> (i32, i32, bool) {
        let (wi, hi) = (w as i32, h as i32);
        // 净锚点（不含 Y 偏移）：方位决策必须基于它，见 apply_offset_y 的说明。
        // 皮肤对齐点在这里折进锚点：下方让对齐点落在光标底端 + gap，上方让它落在光标顶端 − gap。
        let (ax, below_y, above_y) = Self::caret_anchors(caret_x, caret_y, caret_h, hi, offset.0);
        let (below_y, above_y) = (below_y - anchor.below.1, above_y + anchor.above.1);
        let (cl, ct, cr, cb) = anchor.clamp;
        let (mut x, mut y) = (
            ax - anchor.below.0,
            Self::apply_offset_y(below_y, false, offset.1),
        );
        let mut above = false;
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::POINT;
            use windows::Win32::Graphics::Gdi::{
                GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
            };
            unsafe {
                let pt = POINT {
                    x: caret_x,
                    y: caret_y,
                };
                let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(mon, &mut mi).as_bool() {
                    // 垂直可用区：前台铺满 caret 所在这块屏时任务栏不可见，按整屏算，否则全屏
                    // 游戏下会白少一条任务栏高度、误判放不下而上翻遮住输入行。
                    //
                    // 判据（含 DWM cloaked / shell 进程两道守卫）收口在 wind-keys，别在此重写；
                    // 也不能改用 `foreground_fullscreen_kind()`：那个问的是前台窗口自己那块屏，
                    // 这里必须问 caret 所在那块屏。
                    let wa = if wind_keys::foreground::foreground_covers_monitor(&mi.rcMonitor) {
                        mi.rcMonitor
                    } else {
                        mi.rcWork
                    };
                    // 可行性判定用净锚点（不含主题偏移）：偏移只该改变距离，
                    // 不该左右「往上还是往下」——含偏移会让 off_y 越大越容易判成两边都
                    // 放不下，落回下方再被钳到屏幕底、压住光标。
                    let below_ok = below_y + hi + cb <= wa.bottom;
                    let above_ok = above_y - ct >= wa.top;
                    // 垂直方位决策：
                    // - 已在上方（sticky）→ 只要上方放得下就保持上方；上方放不下才回落下方（需求 4）。
                    // - 否则默认下方，仅当下方放不下且上方放得下才上翻。
                    above = if sticky_above {
                        above_ok
                    } else {
                        !below_ok && above_ok
                    };
                    // 方位定后再施加偏移：上方向上、下方向下，均为远离光标；再按该方位的保护带避让。
                    if above {
                        (x, y) = Self::place_above(caret_x, caret_y, caret_h, h, offset, anchor);
                    } else {
                        y = Self::avoid_caret_line(
                            y,
                            false,
                            caret_y,
                            caret_h,
                            anchor.avoid_below.as_ref(),
                        );
                    }
                    // 方位定后的越界兜底：贴住对应屏幕边。
                    // 偏移可能把窗口顶出工作区，这里兜回来——所以偏移必须在钳制之前施加。
                    // 皮肤 clamp = "all" 时 (cl, ct, cr, cb) 是装饰外扩，连装饰一起留在屏内；否则全 0。
                    if above {
                        if y - ct < wa.top {
                            y = wa.top + ct;
                        }
                    } else if y + hi + cb > wa.bottom {
                        y = wa.bottom - hi - cb;
                        if y - ct < wa.top {
                            y = wa.top + ct;
                        }
                    }
                    // 左右钳制（右溢出左移；左溢出贴左）。
                    if x + wi + cr > wa.right {
                        x = wa.right - wi - cr;
                    }
                    if x - cl < wa.left {
                        x = wa.left + cl;
                    }
                }
            }
        }
        // 非 Windows 不做翻转，恒在下方；避让照做（它不依赖显示器几何）。
        #[cfg(not(windows))]
        {
            y = Self::avoid_caret_line(y, false, caret_y, caret_h, anchor.avoid_below.as_ref());
        }
        (x, y, above)
    }

    /// 主题 `window.position_offset` + 用户偏移 → 设备像素 (x, y)。都未配=(0,0)，与旧行为一致。
    ///
    /// 供边缘带装饰的主题拉开候选窗与光标的观感距离。只喂给 place_window——
    /// 固定位置与用户拖动是显式意图，再叠加会让窗口莫名偏离用户放的地方，
    /// 且会破坏拖动落盘的 window↔content 换算互逆性（见 place_fixed 一侧的坐标约定）。
    fn position_offset_px(&self) -> (i32, i32) {
        let v = &self.theme.views;
        let px = |d: Option<wind_theme::schema::Dim>| {
            d.map(|x| x.resolve(self.scale, 0.0)).unwrap_or(0.0).round() as i32
        };
        let (ux, uy) = self.user_offset;
        let user = |d: i32| (d as f32 * self.scale).round() as i32;
        (
            px(v.window_offset_x) + user(ux),
            px(v.window_offset_y) + user(uy),
        )
    }

    /// 把插入符位置夹到合法字符边界。协调器发来的偏移与它自己的 preedit 同源、本应恒合法，
    /// 但 UI 与协调器是跨线程的两份状态（命令队列还会合并/丢弃），一旦错位 `split_at` 会
    /// panic 掉整个 UI 线程。故在此收口：越界截到末尾，落在字符中间则退到前一个边界。
    fn clamp_caret(text: &str, caret: usize) -> usize {
        let mut c = caret.min(text.len());
        while c > 0 && !text.is_char_boundary(c) {
            c -= 1;
        }
        c
    }

    /// 把本帧拆出的穿透装饰推到穿透窗口：与主窗口同位置同尺寸；装饰全在窗体下时插到主窗口
    /// 下面，否则置于其上。本帧没有穿透装饰则收起。
    #[cfg(windows)]
    fn present_pass_layer(&mut self, x: i32, y: i32, w: u32, h: u32) {
        use windows::Win32::UI::WindowsAndMessaging::{
            HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos,
        };
        let Some((pixels, below)) = self.pass_layer.take() else {
            self.hide_pass_window();
            return;
        };
        if self.pass_window.is_none() {
            match LayeredWindow::create(None, w.max(1), h.max(1), "WindInputCandidateDecor") {
                Ok(pw) => {
                    // 鼠标穿透：同 caret_overlay，LayeredWindow 默认没有这一位，创建后补上。
                    unsafe {
                        use windows::Win32::UI::WindowsAndMessaging::{
                            GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_TRANSPARENT,
                        };
                        let ex = GetWindowLongPtrW(pw.hwnd(), GWL_EXSTYLE);
                        SetWindowLongPtrW(
                            pw.hwnd(),
                            GWL_EXSTYLE,
                            ex | WS_EX_TRANSPARENT.0 as isize,
                        );
                    }
                    self.pass_window = Some(pw);
                }
                Err(e) => {
                    tracing::warn!("skin: 穿透窗口创建失败，穿透装饰改画进候选窗: {e}");
                    self.pass_window_failed = true;
                    return;
                }
            }
        }
        let main = self.window.hwnd();
        let Some(pw) = self.pass_window.as_mut() else {
            return;
        };
        let len = (w * h * 4) as usize;
        pw.resize(w, h);
        if pw.buffer()[..len] != pixels[..len] {
            pw.buffer_mut()[..len].copy_from_slice(&pixels[..len]);
            if let Err(e) = pw.update() {
                tracing::warn!("skin: 穿透窗口 update 失败: {e}");
            }
        }
        let want = (x, y, w, h);
        // 换帧重绘不动 z 序（否则每帧置顶会压住气泡 / 菜单）；位置尺寸没变就不调。
        match (self.anim_repaint, self.pass_presented) {
            (true, Some(cur)) if cur == want => {}
            (true, Some(_)) => unsafe {
                use windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER;
                let _ = SetWindowPos(
                    pw.hwnd(),
                    HWND_TOPMOST,
                    x,
                    y,
                    w as i32,
                    h as i32,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
            },
            _ => unsafe {
                let insert_after = if below { main } else { HWND_TOPMOST };
                let _ = SetWindowPos(
                    pw.hwnd(),
                    insert_after,
                    x,
                    y,
                    w as i32,
                    h as i32,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            },
        }
        self.pass_presented = Some(want);
        self.mouse.borrow_mut().pass_hwnd = Some(pw.hwnd());
    }

    fn hide_pass_window(&mut self) {
        #[cfg(windows)]
        {
            if let Some(pw) = &self.pass_window {
                pw.hide();
            }
            self.pass_presented = None;
        }
        self.mouse.borrow_mut().pass_hwnd = None;
    }

    pub fn hide(&mut self) {
        self.window.hide();
        self.hide_pass_window();
        #[cfg(windows)]
        {
            self.presented = None;
        }
        if self.visible {
            self.pixel_release.hidden(Instant::now());
            self.skin_event(wind_theme::skin::DecorEvent::Hide, None);
        }
        self.skin_input_prev = None;
        self.visible = false;
        self.scene.anim_next = None;
        self.anim_due = false;
        self.content_rect = None;
        self.last_content_pos = None; // 组合结束，下次显示重新落位
        self.placed_above = false; // 清除上方粘滞，下次组合按下方默认重新判定
        self.report_flip_state(); // 粘滞一清，反转随之失效，须立刻让协调器跟上
        {
            let mut m = self.mouse.borrow_mut();
            m.reset_hover();
            // 拖动位置只在"本次组合"内有效：组合结束即失效，下次输入恢复跟随光标。
            // 注意 hide_local_window_only()（host-render 分流）刻意不走这里，落位状态须保留。
            m.reset_drag();
        }
        // 候选窗收起：气泡失去依附对象，光标停在它上面也得收（Esc / 上屏 / 失焦 ...）。
        self.tip_for.set(-1);
        if let Some(t) = self.tooltip.as_mut() {
            t.hide_force();
        }
    }

    /// host-render 分流专用：仅隐藏本地 Win32 窗口与 tooltip 窗口，
    /// 不清除 visible / last_content_pos / placed_above 落位状态。
    /// render_frame() 已维护这些状态，host 模式内容确实可见，保持 visible=true 更正确。
    #[cfg(windows)]
    pub fn hide_local_window_only(&mut self) {
        self.window.hide();
        self.hide_pass_window();
        self.presented = None;
        // 宿主接管绘制：本地气泡无条件收（光标在上面也一样），宽限只按候选悬停走。
        if let Some(t) = self.tooltip.as_mut() {
            t.hide_force();
        }
    }

    /// UI 循环每轮调用：推进悬停防抖（稳定后才发出 Hover 事件），并推进候选右键对气泡的
    /// 压制（见 [`TipHold`]）。
    pub fn tick(&mut self) {
        let now = Instant::now();
        if self.release_idle_pixels_at(now) {
            tracing::debug!("UI: 候选窗隐藏已久，释放像素缓冲");
        }
        // 悬停宽限：光标已进入气泡就撤掉待发的悬停变化，到期未进则照发（见 [`hover_move`]）。
        if self.mouse.borrow().deferred.is_some() {
            let on_tip = self.tooltip.as_ref().is_some_and(|t| t.cursor_on_tip_now());
            self.mouse.borrow_mut().resolve_deferred(now, on_tip);
        }
        let (hold, released) = {
            let mut m = self.mouse.borrow_mut();
            m.flush(now, self.hover);
            (m.tip_hold.active(), std::mem::take(&mut m.tip_released))
        };
        // 右键那一刻已显示的气泡就在这里收掉：消息泵刚派发完 WM_RBUTTONDOWN，本轮即生效，
        // 赶在菜单弹出之前。hide 幂等，压制期间每轮调用也只是一次判断。
        if hold && let Some(t) = self.tooltip.as_mut() {
            t.hide();
        }
        // 压制解除时 UI 画着的悬停就是光标下的目标 → 协调器不会重绘，气泡得在这里自己补显示。
        if released && self.visible {
            let origin = self.mouse.borrow().window_origin();
            if let Some((wx, wy)) = origin {
                self.update_tooltip(wx, wy);
            }
        }
        let click = self.mouse.borrow_mut().skin_click.take();
        if let Some(i) = click {
            self.skin_event(wind_theme::skin::DecorEvent::Click, Some(i));
        }
        if self.visible && self.anim_next_at().is_some_and(|t| t <= now) {
            self.scene.anim_next = None;
            self.anim_due = true;
        }
    }

    /// 取走「装饰换帧到期」标记：为 true 时 UI 循环经 [`Self::repaint_anim_frame`] 重绘一帧。
    pub fn take_anim_due(&mut self) -> bool {
        std::mem::take(&mut self.anim_due)
    }

    /// 是否正在为换帧重绘（host-render 据此跳过气泡帧）。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn is_anim_repaint(&self) -> bool {
        self.anim_repaint
    }

    /// 换帧重绘：`paint` 走当前路径（host-render 或本地 `show`）重画一帧。期间不重置悬停去重、
    /// 不重摆气泡；时间轴起点只在由隐藏变可见时记，这里不动。
    pub fn repaint_anim_frame(&mut self, paint: impl FnOnce(&mut Self)) {
        self.anim_repaint = true;
        paint(self);
        self.anim_repaint = false;
    }

    /// 隐藏满 [`crate::window::HIDDEN_RELEASE_AFTER`] 就释放像素缓冲与（Windows）离屏表面，
    /// 返回是否真的放了。两者都按历史最大尺寸驻留（推断：出厂主题 1x 横排约 0.3 MB、1.5x 竖排
    /// 约 1 MB 的像素缓冲，离屏表面按「最大宽 × 最大高」再一份），下一次 `show` 按当时尺寸
    /// 重建。[`Self::tick`] 每轮调，到期时刻经 [`Self::next_deadline`] 登记。
    pub(crate) fn release_idle_pixels_at(&mut self, now: Instant) -> bool {
        if !self.pixel_release.take_due(now) {
            return false;
        }
        self.window.release_buffer();
        #[cfg(windows)]
        self.text_renderer.release_surface();
        true
    }

    /// 下一次需要 [`Self::tick`] 的时刻；`None` = 无待到期者。
    ///
    /// 消息循环据此安排唤醒。到期源：悬停激活闸门（`engage_at`）、右键压制「等菜单」阶段的
    /// 兜底超时（[`TipHold::AwaitMenu`]）、隐藏后释放像素缓冲、皮肤装饰换帧。
    pub fn next_deadline(&self) -> Option<std::time::Instant> {
        let m = self.mouse.borrow();
        let hold = match m.tip_hold {
            TipHold::AwaitMenu { until } => Some(until),
            _ => None,
        };
        [
            m.engage_deadline(),
            hold,
            m.deferred,
            self.pixel_release.deadline(),
            // 装饰换帧：只在可见时登记；事件切了画面待重绘时立即醒。
            self.anim_next_at().filter(|_| self.visible),
            // 闸门没到不醒：外部宿主到闸门才取走这一帧。
            self.anim_due.then(|| {
                self.anim_gate
                    .map_or_else(Instant::now, |g| g.max(Instant::now()))
            }),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 显示中的候选窗内容盒屏幕矩形 `(left, top, right, bottom)`；没显示为 `None`。
    pub fn screen_rect(&self) -> Option<(i32, i32, i32, i32)> {
        self.content_rect.filter(|_| self.visible)
    }

    /// 当前鼠标悬停项（页内下标；翻页器 tag 或 -1=无）。host 分流写帧时作 rendered_hover_index。
    #[cfg(windows)]
    pub fn hover(&self) -> i32 {
        self.hover
    }

    pub fn candidates(&self) -> &[CandidateItem] {
        &self.candidates
    }

    pub fn hwnd(&self) -> HWND {
        self.window.hwnd()
    }

    /// 将当前渲染帧保存为 PNG 文件（截图用）。
    pub fn capture_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        self.window.capture_to_file(path)
    }

    /// 将当前渲染帧复制到剪贴板（截图用）。
    pub fn capture_to_clipboard(&self) -> Result<(), String> {
        self.window.capture_to_clipboard()
    }
}

/// 候选窗鼠标处理器：命中候选→选词，悬停→高亮，滚轮→翻页。
/// 命中矩形为窗口本地坐标（绘制于 0,0），与 WM_* 的 client 坐标一致。
pub struct CandidateMouse {
    hit_rects: Vec<(i32, Rect)>,
    /// 皮肤：窗体矩形与装饰矩形（窗口坐标）。落在装饰上、又不在窗体内的点不起拖。
    skin_frame: Option<Rect>,
    skin_decor: Vec<Rect>,
    /// 能点的皮肤装饰（`drag` / `block`）：(变体下标, 矩形, 在窗体下面)，按 z 升序。
    skin_click_rects: Vec<(usize, Rect, bool)>,
    /// 左键按下时落在哪块装饰上，与当时的光标屏幕坐标。
    decor_press: Option<(usize, (i32, i32))>,
    /// 一次装饰点击（按下后没挪过拖动阈值就松开），待 [`CandidateWindow::tick`] 取走。
    skin_click: Option<usize>,
    /// 皮肤穿透窗口：拖动时跟着主窗口一起挪。
    pass_hwnd: Option<HWND>,
    events: Sender<UiEvent>,
    /// 已生效（已发出）的悬停目标，去重用
    last_hover: i32,
    /// 上次物理光标屏幕坐标——过滤内容变化引起的伪 WM_MOUSEMOVE
    last_cursor: (i32, i32),
    /// 窗口级闸门：是否已"激活"（用户已真实移动鼠标过本窗口）。
    /// 激活后每次 hover 立即响应，不再有逐项延迟。
    engaged: bool,
    /// 首次真实移动后的激活时刻；到期即激活（窗口级一次性防抖）。
    engage_at: Option<Instant>,
    /// 最近一次命中目标（激活瞬间据此发出首个悬停）。
    pending_raw: i32,
    /// 悬停激活延迟（毫秒）。来自 ui.tooltip.delay；默认 60。
    engage_delay_ms: u64,
    /// 本窗口句柄（拖动时 SetCapture / SetWindowPos 用）
    hwnd: HWND,
    /// 是否正在拖动（左键按在空白区并保持）
    dragging: bool,
    /// 左键按下时命中的候选 / 翻页键（松开时仍在同一项才动作，见 [`click_on_release`]）。
    pressed: Option<i32>,
    /// 拖动起点：光标屏幕坐标
    drag_anchor: (i32, i32),
    /// 拖动起点：窗口左上屏幕坐标
    drag_origin: (i32, i32),
    /// 拖动落定位置（窗口左上屏幕坐标）。`Some` 即"本次组合已被用户手动摆放"，
    /// 此后该组合内的每帧渲染都固定用它，不再跟随光标；`hide()`（组合结束）清空。
    drag_pin: Option<(i32, i32)>,
    /// 写 `drag_pin` 时的扩边（左, 上）。皮肤装饰的扩边会随窗体尺寸变，渲染时据此把落位
    /// 换算到当前扩边下，窗体才不随内容变化跳动。
    drag_pin_margin: (i32, i32),
    /// 当前阴影扩边 (left, top, right, bottom)，每帧渲染后由 `render_frame` 同步。
    /// 窗口左上 + (left, top) = 内容左上，即落盘用的坐标系；四个分量一起用于
    /// 按内容矩形做拖动钳制。
    margin: (i32, i32, i32, i32),
    /// 空白处双击判定（截图用，见 `UiEvent::CandidateDoubleClick`）。
    double_click: crate::double_click::DoubleClick,
    /// 候选窗右键对悬停气泡的压制阶段，见 [`TipHold`]。
    tip_hold: TipHold,
    /// 压制刚解除且 UI 画着的悬停正是光标下的目标：待 [`CandidateWindow::tick`] 取走并
    /// 本地补显示气泡。
    tip_released: bool,
    /// 气泡此刻跟着哪个候选（与 [`CandidateWindow`] 共享，见其同名字段）。
    tip_for: Rc<Cell<i32>>,
    /// 悬停宽限：待发的悬停变化（`pending_raw`）到期时刻，见 [`hover_move`]。
    deferred: Option<Instant>,
    /// 气泡内容盒（窗口内坐标 `(left, top, right, bottom)`），显示气泡时由候选窗写入。
    /// 只在 `tip_for` 指着当前悬停时才有人读，见 [`heading_to_tip`]。
    tip_rect: Option<(i32, i32, i32, i32)>,
    /// 气泡此刻是否显示（与气泡共享）。气泡自己收起时 `tip_rect` 不会跟着清，读它前先问这个。
    tip_visible: Option<Rc<Cell<bool>>>,
    /// 本次宽限里离气泡最近的距离（[`tip_gap`]）。只有刷新到新的最小值才顺延：指针停着抖动时
    /// 一来一回刷不出新低，不会把切换无限拖住。
    tip_best: Option<i32>,
    /// 上一次真实移动的窗口内坐标，判这一步的方向用。
    last_pos: Option<(i32, i32)>,
}

/// 光标离开气泡所属候选、去往「无」（空隙 / 内边距 / 翻页器）的宽限：与气泡那侧的
/// [`crate::tooltip::LEAVE_GRACE_MS`] 同值——从候选行穿过空隙进入气泡要这么久。
const TIP_GRACE: Duration = Duration::from_millis(crate::tooltip::LEAVE_GRACE_MS as u64);

/// 气泡显示中、光标移到另一个候选的切换延迟。气泡在旁侧时，从候选 A 斜着去 A 的气泡
/// 会擦过相邻行 B；立刻切到 B，A 的气泡就被换掉、永远够不着。150ms 够手速正常地穿过一行
/// （三四十像素），而在候选间正常移动时高亮只慢这么一点，不显得迟钝。比宽限短：去往另一个
/// 候选多半是真想看它。
const TIP_SWITCH: Duration = Duration::from_millis(150);

/// 光标从 `prev` 移到 `now` 这一步是不是朝着气泡 `tip`（`(left, top, right, bottom)`）去。
///
/// 是的话悬停宽限按 [`TIP_SWITCH`] 往后顺延：展开态的气泡停靠在窗口外侧，过去要横穿同一行
/// 好几格，固定的 150ms 只够穿过一格。停下来、往回走或偏离气泡，都照常在 150ms 内切换。
///
/// 不用严格的「安全三角」：气泡停在顶端时，从靠下几行水平滑向它并不指向气泡本身。判据是
/// 沿气泡所在那一侧的位移在靠近，且横向偏离气泡跨度的位移不比靠近的快。
fn heading_to_tip(prev: (i32, i32), now: (i32, i32), tip: (i32, i32, i32, i32)) -> bool {
    let (l, t, r, b) = tip;
    let (dx, dy) = (now.0 - prev.0, now.1 - prev.1);
    let (toward, lateral, lo, hi, pos) = if l >= prev.0 {
        (dx, dy, t, b, now.1)
    } else if r <= prev.0 {
        (-dx, dy, t, b, now.1)
    } else if t >= prev.1 {
        (dy, dx, l, r, now.0)
    } else if b <= prev.1 {
        (-dy, dx, l, r, now.0)
    } else {
        return false;
    };
    let away = if pos > hi {
        lateral
    } else if pos < lo {
        -lateral
    } else {
        0
    };
    toward > 0 && away <= toward
}

/// 点 `p` 到气泡 `tip` 的距离（沿气泡所在那一侧的轴；在气泡上为 0）。
fn tip_gap(p: (i32, i32), (l, t, r, b): (i32, i32, i32, i32)) -> i32 {
    if l >= p.0 {
        l - p.0
    } else if r <= p.0 {
        p.0 - r
    } else if t >= p.1 {
        t - p.1
    } else if b <= p.1 {
        p.1 - b
    } else {
        0
    }
}

/// 悬停从 `last` 变到 `raw` 时是即时发出还是延后（设计 §7.6 C）。只有 `last` 的气泡正显示
/// （`tip_for == last`）时才延后：去往「无」按 [`TIP_GRACE`]，去往另一个候选按 [`TIP_SWITCH`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoverMove {
    Now,
    Defer(Duration),
}

fn hover_move(last: i32, raw: i32, tip_for: i32) -> HoverMove {
    let tip_up = (0..TAG_PAGE_PREV).contains(&last) && tip_for == last;
    if !tip_up {
        HoverMove::Now
    } else if (0..TAG_PAGE_PREV).contains(&raw) {
        HoverMove::Defer(TIP_SWITCH)
    } else {
        HoverMove::Defer(TIP_GRACE)
    }
}

/// 待发的悬停变化此刻该怎么办：光标进了气泡 → 撤掉（悬停留在原候选，气泡自己的离开跟踪
/// 接手）；到期 → 发出；否则再等。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deferred {
    Wait,
    Cancel,
    Fire,
}

fn resolve_deferred(now: Instant, until: Instant, on_tip: bool) -> Deferred {
    if on_tip {
        Deferred::Cancel
    } else if now >= until {
        Deferred::Fire
    } else {
        Deferred::Wait
    }
}

/// 「等菜单」阶段的兜底超时（取值理由见 `popup_menu::MENU_REPLY_TIMEOUT`）：超时按「菜单已
/// 关闭」处理，不让气泡一直压着。万一菜单来得比这还晚，出现时照样进 [`TipHold::InMenu`]。
const HOLD_MENU_TIMEOUT: Duration = crate::popup_menu::MENU_REPLY_TIMEOUT;

/// 候选窗右键对悬停气泡的压制（设计 §7.6 A）。
///
/// 不压的话气泡会盖在菜单上（已显示的不会自己消失；激活延迟到期后在菜单之后冒出来）。
///
/// 做成阶段而不是一个布尔：右键到菜单可见之间「菜单开着」还是 false，鼠标一动就会重新武装
/// 闸门。只压气泡、不清悬停：悬停高亮是「菜单作用于谁」的唯一提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TipHold {
    /// 不压。
    Off,
    /// 已右键、菜单还没出现。闸门既不武装也不到期。`until` 为兜底超时时刻。
    AwaitMenu { until: Instant },
    /// 菜单开着。闸门既不武装也不到期。
    InMenu,
    /// 菜单已关：等一次真实移动武装闸门、再走完一次延迟才解除。
    AwaitMove,
}

impl TipHold {
    /// 按「菜单此刻是否可见」与时间推进阶段。
    fn advance(self, now: Instant, menu_visible: bool) -> Self {
        match self {
            TipHold::Off => TipHold::Off,
            TipHold::AwaitMenu { .. } if menu_visible => TipHold::InMenu,
            TipHold::AwaitMenu { until } if now >= until => TipHold::AwaitMove,
            TipHold::InMenu if !menu_visible => TipHold::AwaitMove,
            // 迟到的菜单（超时之后才出现）同样要压住。
            TipHold::AwaitMove if menu_visible => TipHold::InMenu,
            other => other,
        }
    }

    /// 本阶段是否挡住激活闸门（不武装、不到期）。
    fn blocks_gate(self) -> bool {
        matches!(self, TipHold::AwaitMenu { .. } | TipHold::InMenu)
    }

    fn active(self) -> bool {
        self != TipHold::Off
    }
}

impl CandidateMouse {
    /// 激活闸门的到期时刻；已激活或未武装时为 `None`。
    ///
    /// 已激活后返回 `None` 与 [`Self::flush`] 的首行早退同源：那之后悬停由 `on_message`
    /// 即时发出，没有任何东西等着到期。
    fn engage_deadline(&self) -> Option<Instant> {
        if self.engaged {
            return None;
        }
        self.engage_at
    }

    /// UI 循环每轮调用：推进右键压制阶段；悬停激活闸门到期则激活并补发当前悬停。
    ///
    /// `drawn_hover` 是 UI 此刻画着的悬停（`CandidateWindow::hover`，协调器最近一次下发的值）。
    /// 解除压制时拿它而不是 `last_hover` 比：压制期间候选若重绘过，`last_hover` 是旧值，
    /// 会漏发 `Hover`，气泡便再也不回来。
    fn flush(&mut self, now: Instant, drawn_hover: i32) {
        self.tip_hold = self
            .tip_hold
            .advance(now, crate::popup_menu::menu_visible());
        if self.tip_hold.blocks_gate() {
            // 进入「等菜单 / 菜单中」前武装的闸门一律作废（迟到的菜单从 AwaitMove 进来时
            // 闸门可能已在走）。
            self.engage_at = None;
            return;
        }
        if self.engaged {
            return; // 已激活：悬停在 on_message 内即时发出
        }
        if let Some(at) = self.engage_at
            && now >= at
        {
            self.engaged = true;
            self.engage_at = None;
            if self.tip_hold == TipHold::AwaitMove {
                self.tip_hold = TipHold::Off;
                self.last_hover = self.pending_raw;
                if self.pending_raw == drawn_hover {
                    // UI 画着的就是它：协调器不会重绘，由窗口本地补显示气泡。
                    self.tip_released = true;
                } else {
                    let _ = self.events.send(UiEvent::Hover(self.pending_raw));
                }
            } else if self.pending_raw != self.last_hover {
                self.last_hover = self.pending_raw;
                let _ = self.events.send(UiEvent::Hover(self.pending_raw));
            }
        }
    }

    /// 推进悬停宽限（见 [`resolve_deferred`]）。`on_tip` = 光标此刻在气泡上。
    fn resolve_deferred(&mut self, now: Instant, on_tip: bool) {
        let Some(until) = self.deferred else {
            return;
        };
        match resolve_deferred(now, until, on_tip) {
            Deferred::Wait => {}
            Deferred::Cancel => self.deferred = None,
            Deferred::Fire => {
                self.deferred = None;
                if self.pending_raw != self.last_hover {
                    self.last_hover = self.pending_raw;
                    let _ = self.events.send(UiEvent::Hover(self.pending_raw));
                }
            }
        }
    }

    /// 候选窗上右键（候选菜单 / 空白处主菜单）：进入压制「等菜单」阶段，并把激活闸门打回
    /// 未激活，见 [`TipHold`]。
    fn hold_tooltip(&mut self, now: Instant) {
        self.deferred = None;
        self.tip_hold = TipHold::AwaitMenu {
            until: now + HOLD_MENU_TIMEOUT,
        };
        self.tip_released = false;
        self.engaged = false;
        self.engage_at = None;
    }

    /// 候选窗右键：客户区 `(x, y)` 定命中，屏幕 `screen` 定菜单位置。命中候选 → 词条菜单，
    /// 空白处 → 功能主菜单（光标处向下弹）。`WM_RBUTTONDOWN` 与菜单外右键合成共用。
    fn right_click(&mut self, x: f32, y: f32, (sx, sy): (i32, i32)) {
        let i = self.hit(x, y);
        self.hold_tooltip(Instant::now());
        if i >= 0 {
            let _ = self.events.send(UiEvent::RequestCandidateMenu {
                page_local: i as usize,
                x: sx,
                y: sy,
            });
        } else {
            let _ = self
                .events
                .send(UiEvent::RequestMainMenu(MenuAnchor::at_point(sx, sy)));
        }
    }

    /// 重置悬停状态：清空闸门与去重值，并以当前物理光标位重建基线。
    ///
    /// 两个调用点：[`CandidateWindow::mark_visible`]（基线在这里才有意义，见该函数）；
    /// [`CandidateWindow::hide`]（清掉闸门与残留悬停，它顺带采的基线不能当作来源）。
    fn reset_hover(&mut self) {
        self.deferred = None;
        self.last_pos = None;
        self.last_hover = -1;
        self.engaged = false;
        self.engage_at = None;
        self.pending_raw = -1;
        self.tip_hold = TipHold::Off;
        self.tip_released = false;
        let (sx, sy) = unsafe {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            (p.x, p.y)
        };
        self.last_cursor = (sx, sy);
    }
}

impl CandidateMouse {
    /// 当前窗口左上屏幕坐标；取不到时 None（非 Windows mock 恒 None 语义下的兜底由调用点给）。
    fn window_origin(&self) -> Option<(i32, i32)> {
        let mut r = RECT::default();
        unsafe {
            if GetWindowRect(self.hwnd, &mut r).is_ok() {
                return Some((r.left, r.top));
            }
        }
        None
    }

    /// 当前窗口尺寸（含阴影扩边）；用于拖动时的工作区钳制。
    fn window_size(&self) -> (u32, u32) {
        let mut r = RECT::default();
        unsafe {
            if GetWindowRect(self.hwnd, &mut r).is_ok() {
                return ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
            }
        }
        (0, 0)
    }

    /// 清除拖动落定位置：下次显示恢复跟随光标自动定位。
    fn reset_drag(&mut self) {
        self.dragging = false;
        self.drag_pin = None;
        self.pressed = None;
        self.decor_press = None;
    }

    fn hit(&self, x: f32, y: f32) -> i32 {
        for (tag, r) in &self.hit_rects {
            if r.contains(x, y) {
                return *tag;
            }
        }
        -1
    }

    /// 点中的装饰（变体下标）：最上层的 `drag` / `block` 装饰；窗体下面的装饰被窗体盖住处不算。
    fn decor_at(&self, x: f32, y: f32) -> Option<usize> {
        let in_frame = self.skin_frame.is_some_and(|f| f.contains(x, y));
        self.skin_click_rects
            .iter()
            .rev()
            .find(|(_, r, under)| r.contains(x, y) && !(*under && in_frame))
            .map(|(i, _, _)| *i)
    }

    /// 左键按下：记下按在哪块装饰上。
    fn press_decor(&mut self, x: f32, y: f32, screen: (i32, i32)) {
        self.decor_press = self.decor_at(x, y).map(|i| (i, screen));
    }

    /// 左键松开：按下与松开在同一块装饰上、光标没挪过拖动阈值，算一次装饰点击。
    fn release_decor(&mut self, x: f32, y: f32, screen: (i32, i32)) {
        if let Some((i, p0)) = self.decor_press.take()
            && within_drag_threshold(p0, screen, crate::double_click::drag_threshold())
            && self.decor_at(x, y) == Some(i)
        {
            self.skin_click = Some(i);
        }
    }

    /// 点落在 `block` 装饰上、且不在窗体内：贴纸式装饰，不拖动窗口（设计 §4）。
    fn on_decor_only(&self, x: f32, y: f32) -> bool {
        let in_frame = self.skin_frame.is_some_and(|f| f.contains(x, y));
        !in_frame && self.skin_decor.iter().any(|r| r.contains(x, y))
    }
}

/// 光标从 `a` 到 `b` 没超出拖动阈值 `(tx, ty)`（系统 `SM_CXDRAG` / `SM_CYDRAG` 口径）。
fn within_drag_threshold(a: (i32, i32), b: (i32, i32), (tx, ty): (i32, i32)) -> bool {
    (b.0 - a.0).abs() <= tx && (b.1 - a.1).abs() <= ty
}

/// 光标屏幕坐标。
fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// 候选窗左键点选：按下记的命中 `pressed` 与松开处的命中 `hit` 是同一项才动作。
fn click_on_release(pressed: Option<i32>, hit: i32) -> Option<i32> {
    pressed.filter(|&p| p == hit)
}

/// 从 lParam 解出 client 坐标（低/高 16 位有符号）
fn mouse_pos(lparam: LPARAM) -> (f32, f32) {
    let v = lparam.0 as u32;
    let x = (v & 0xFFFF) as i16 as f32;
    let y = ((v >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

impl WindowMouse for CandidateMouse {
    fn on_message(
        &mut self,
        _hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match msg {
            WM_LBUTTONDOWN => {
                let (x, y) = mouse_pos(lparam);
                let i = self.hit(x, y);
                match i {
                    // 候选 / 翻页键：按下只记命中，松开仍在同一项上才动作（见 WM_LBUTTONUP）。
                    // SetCapture：移出窗口后松开也要收到，才能判「放弃」并放掉按下态。
                    i if i >= 0 || i == TAG_PAGE_PREV || i == TAG_PAGE_NEXT => {
                        self.double_click.reset();
                        self.pressed = Some(i);
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                    }
                    // 贴纸式装饰不拖窗口；按下只记下来，松开时判是不是一次点击。
                    _ if self.on_decor_only(x, y) => {
                        self.press_decor(x, y, cursor_pos());
                        if self.decor_press.is_some() {
                            unsafe {
                                SetCapture(self.hwnd);
                            }
                        }
                    }
                    _ => {
                        // 空白区（编码栏/内边距，非候选非翻页键）→ 起拖，整窗跟随光标。
                        // 与工具栏同构：记录光标与窗口起点，SetCapture 保证移出窗口后仍收到消息。
                        let mut p = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut p);
                        }
                        self.drag_anchor = (p.x, p.y);
                        self.drag_origin = self.window_origin().unwrap_or((p.x, p.y));
                        self.dragging = true;
                        // 按在 drag 装饰上：没挪过拖动阈值就松开算点它一下。
                        self.press_decor(x, y, (p.x, p.y));
                        unsafe {
                            SetCapture(self.hwnd);
                        }
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.dragging {
                    self.dragging = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    // 按在 drag 装饰上的不进双击判定：连点人物是在点它，不是要截图。
                    let on_decor = self.decor_press.is_some();
                    let (x, y) = mouse_pos(lparam);
                    self.release_decor(x, y, cursor_pos());
                    // 双击判定：只收「按下到松开窗口没挪」的空白处单击——拖过一下的不算，
                    // 否则拖完紧跟一次点击也会截图。候选项 / 翻页键走下面的 else，不经这里。
                    let (interval, slop) = crate::double_click::system_thresholds();
                    if !on_decor && self.window_origin() == Some(self.drag_origin) {
                        let mut p = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut p);
                        }
                        let now = Instant::now();
                        if self.double_click.on_click(now, p.x, p.y, interval, slop) {
                            let _ = self.events.send(UiEvent::CandidateDoubleClick);
                        }
                    } else {
                        self.double_click.reset();
                    }
                    // 以真实窗口位置落定，避免累积误差
                    if let Some(pos) = self.window_origin() {
                        self.drag_pin = Some(pos);
                        self.drag_pin_margin = (self.margin.0, self.margin.1);
                        // 上报内容左上（窗口左上 + 阴影扩边）：固定位置模式下协调器把它
                        // 落盘成 custom_x/custom_y；跟随模式下协调器直接忽略——那里的拖动
                        // 只是"临时挪开"，下次组合仍回到光标旁。
                        let (cx, cy) =
                            CandidateWindow::window_to_content(pos, self.margin.0, self.margin.1);
                        let _ = self
                            .events
                            .send(UiEvent::CandidateWindowMoved { x: cx, y: cy });
                    }
                } else {
                    // 候选 / 翻页键的点击在松开时生效（同 macOS `CandidateContentView.mouseUp`、
                    // Linux `ClickGate`）：按下就选的话，上屏、收起候选窗在松开前就做完了。
                    // 按下后移出该项再松开 = 放弃。抓取无条件放：按住期间候选窗收起时
                    // `pressed` 已被 `reset_drag` 清掉，抓取却还在，不放的话下一次点击会落到
                    // 隐藏的候选窗上。
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    let (x, y) = mouse_pos(lparam);
                    self.release_decor(x, y, cursor_pos());
                    if let Some(p) = click_on_release(self.pressed.take(), self.hit(x, y)) {
                        let ev = match p {
                            TAG_PAGE_PREV => UiEvent::Page(-1),
                            TAG_PAGE_NEXT => UiEvent::Page(1),
                            i => UiEvent::CandidateSelect(i as usize),
                        };
                        let _ = self.events.send(ev);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.dragging => {
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                }
                let nx = self.drag_origin.0 + (p.x - self.drag_anchor.0);
                let ny = self.drag_origin.1 + (p.y - self.drag_anchor.1);
                let (w, h) = self.window_size();
                // 按内容矩形钳制、且钳到整块屏幕：软阴影可溢出屏幕，候选窗也允许
                // 摆到任务栏上方。用 clamp_to_work_area 会让内容离屏幕边还差一个阴影
                // 宽（blur=8 的主题约 29px）就拖不动了。
                let (cx, cy) = clamp_content_to_monitor(nx, ny, w, h, self.margin);
                // 拖动中即写 drag_pin：候选内容若在拖动期间刷新，渲染路径才不会把窗口拽回光标处。
                self.drag_pin = Some((cx, cy));
                self.drag_pin_margin = (self.margin.0, self.margin.1);
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        HWND_TOPMOST,
                        cx,
                        cy,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                    if let Some(p) = self.pass_hwnd {
                        let _ = SetWindowPos(
                            p,
                            HWND_TOPMOST,
                            cx,
                            cy,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                // 物理移动门控：内容变化（打字换候选/窗口刷新）也会产生 WM_MOUSEMOVE，
                // 但此时物理光标屏幕坐标不变 → 忽略，避免静止鼠标下方候选变化引起闪烁。
                let (sx, sy) = unsafe {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    (p.x, p.y)
                };
                if (sx, sy) == self.last_cursor {
                    return Some(LRESULT(0));
                }
                self.last_cursor = (sx, sy);
                let (x, y) = mouse_pos(lparam);
                let raw = self.hit(x, y);
                self.pending_raw = raw;
                let pos = (x as i32, y as i32);
                let tip = self
                    .tip_rect
                    .filter(|_| self.tip_visible.as_ref().is_some_and(|v| v.get()));
                let prev = self.last_pos.replace(pos);
                let gap = tip.map(|t| tip_gap(pos, t));
                let toward = prev
                    .zip(tip)
                    .is_some_and(|(prev, tip)| heading_to_tip(prev, pos, tip))
                    && gap.zip(self.tip_best).is_some_and(|(g, best)| g < best);
                // 先按此刻的菜单可见性推进压制阶段：菜单在上一次 tick 之后才收起时，关后的
                // 第一次移动就该能武装闸门，不必再等一轮。
                self.tip_hold = self
                    .tip_hold
                    .advance(Instant::now(), crate::popup_menu::menu_visible());
                if self.engaged {
                    // 已激活：即时高亮/显示 tooltip，无逐项延迟——除非正移开气泡所属的
                    // 候选（悬停宽限，见 `hover_move`）。
                    if raw == self.last_hover {
                        // 回到原候选：待发的变化作废。
                        self.deferred = None;
                    } else {
                        match hover_move(self.last_hover, raw, self.tip_for.get()) {
                            HoverMove::Now => {
                                self.deferred = None;
                                self.last_hover = raw;
                                let _ = self.events.send(UiEvent::Hover(raw));
                            }
                            HoverMove::Defer(d) => {
                                // 取较早者：宽限里又移到另一个候选时按切换延迟收紧，不顺延；
                                // 正朝气泡去则顺延，见 `heading_to_tip`。
                                let now = Instant::now();
                                let at = now + d;
                                self.deferred = Some(match self.deferred {
                                    Some(u) if toward => {
                                        self.tip_best = gap;
                                        u.max(now + TIP_SWITCH)
                                    }
                                    Some(u) => u.min(at),
                                    None => {
                                        self.tip_best = gap;
                                        at
                                    }
                                });
                            }
                        }
                    }
                } else if self.engage_at.is_none() && !self.tip_hold.blocks_gate() {
                    // 首次真实移动：启动窗口级激活闸门（仅一次，~60ms）。
                    // 右键压制「等菜单 / 菜单中」不武装，见 TipHold。
                    self.engage_at =
                        Some(Instant::now() + Duration::from_millis(self.engage_delay_ms));
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN => {
                // 菜单开着时这次右键由菜单轮询那一路合成（`CandidateWindow::menu_outside_press`），
                // 两路都认会请求两遍菜单。「等菜单」阶段同理：已有一个请求在路上——合成那一路
                // 先走、这条真实消息后到时，菜单已被轮询收起、还看不出「开着」，只能靠它挡。
                if crate::popup_menu::menu_visible()
                    || matches!(self.tip_hold, TipHold::AwaitMenu { .. })
                {
                    return Some(LRESULT(0));
                }
                let (x, y) = mouse_pos(lparam);
                // 用屏幕光标坐标定位菜单
                let screen = unsafe {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    (p.x, p.y)
                };
                self.right_click(x, y, screen);
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                // 高 16 位为有符号滚动量：上滚(>0)→上一页，下滚(<0)→下一页
                let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
                let dir = if delta > 0 { -1 } else { 1 };
                let _ = self.events.send(UiEvent::Page(dir));
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                unsafe {
                    let cur = if self.dragging {
                        IDC_SIZEALL
                    } else {
                        IDC_ARROW
                    };
                    if let Ok(c) = LoadCursorW(None, cur) {
                        SetCursor(c);
                    }
                }
                Some(LRESULT(1))
            }
            _ => None,
        }
    }
}

#[cfg(all(test, target_os = "linux", ext_presenter))]
mod host_motion_tests {
    /// Linux addon 报的「减少动态效果」：没报过、报了「不减少」都允许，只有报了「减少」才冻结。
    #[test]
    fn only_reported_reduce_freezes() {
        assert!(super::host_motion_allowed(0));
        assert!(super::host_motion_allowed(1));
        assert!(!super::host_motion_allowed(2));
    }
}

/// 设计 §7：候选窗隐藏 30 秒后释放像素缓冲（Windows 另释放 DirectWrite 离屏表面），
/// 下次显示按当时尺寸重建、画出来逐字节一致。
#[cfg(all(test, mock_text))]
mod pixel_release_tests {
    use super::*;
    use crate::window::HIDDEN_RELEASE_AFTER;

    fn shown() -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        let items = ["一", "二", "三"]
            .iter()
            .map(|t| CandidateItem {
                text_styled: Default::default(),
                text: t.to_string(),
                code: String::new(),
                label: String::new(),
                tooltip: Default::default(),
                comment: Default::default(),
                comment_above: Default::default(),
                no_index: false,
            })
            .collect();
        w.update("abc", 3, "", items, 0, -1, 1, 1);
        w.show();
        assert!(!w.window.buffer().is_empty(), "前提：画出来了");
        w
    }

    #[test]
    fn hidden_window_releases_pixels_and_redraws_identically() {
        let mut w = shown();
        let before = w.window.buffer().to_vec();
        let size = w.window.size();
        let t0 = Instant::now();
        w.hide();
        assert!(w.next_deadline().is_some(), "隐藏后要求在到期时刻唤醒");
        assert!(!w.release_idle_pixels_at(t0 + HIDDEN_RELEASE_AFTER - Duration::from_secs(1)));
        assert!(!w.window.buffer().is_empty(), "未到期不放");
        assert!(w.release_idle_pixels_at(Instant::now() + HIDDEN_RELEASE_AFTER));
        assert!(w.window.buffer().is_empty(), "到期释放像素缓冲");
        assert_eq!(w.next_deadline(), None, "放过之后不再要求唤醒");
        w.show();
        assert_eq!(w.window.size(), size);
        assert_eq!(w.window.buffer(), &before[..], "按需重建，画出来一致");
    }

    #[test]
    fn shown_again_within_window_keeps_pixels() {
        let mut w = shown();
        w.hide();
        w.show();
        assert!(!w.release_idle_pixels_at(Instant::now() + HIDDEN_RELEASE_AFTER * 2));
        assert!(!w.window.buffer().is_empty(), "期间又显示过 ⇒ 不放");
    }
}

/// 展开态（多行网格）的版式：一行一页、列对齐、只有高亮行显示序号、末屏补行、宽度只增不减。
///
/// 依赖 mock 文本测量，gate 同 `min_size_tests`。
#[cfg(all(test, mock_text))]
mod grid_tests {
    use super::*;
    use crate::view::Rect;
    use wind_ui_types::CandidateGrid;

    fn cand(text: &str) -> CandidateItem {
        CandidateItem {
            text_styled: Default::default(),
            text: text.to_string(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: Default::default(),
            comment_above: Default::default(),
            no_index: false,
        }
    }

    /// 造一个展开态候选窗：`texts` 逐行平铺，每行 `cols` 个，可见 `rows` 行，高亮第 `hl` 行。
    fn grid_win(cols: u16, rows: u16, hl: u16, texts: &[&str]) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(false, false, false);
        refill(&mut w, cols, rows, hl, texts);
        w
    }

    fn refill(w: &mut CandidateWindow, cols: u16, rows: u16, hl: u16, texts: &[&str]) {
        w.set_grid(Some(CandidateGrid {
            cols,
            rows,
            highlight_row: hl,
            index_start: hl * cols,
            index_count: cols,
            tabs: 0,
            active_tab: 0,
        }));
        // 序号与协调器一致：每行都是 1..cols。
        let items = texts
            .iter()
            .enumerate()
            .map(|(i, t)| CandidateItem {
                text_styled: Default::default(),
                label: (i % cols as usize + 1).to_string(),
                ..cand(t)
            })
            .collect();
        w.update("", 0, "", items, hl as usize * cols as usize, -1, 1, 3);
    }

    fn laid(w: &CandidateWindow) -> View {
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        root
    }

    fn hit(root: &View, tag: i32) -> Rect {
        let mut hits = Vec::new();
        root.collect_hits(&mut hits);
        hits.iter()
            .find(|(t, _)| *t == tag)
            .unwrap_or_else(|| panic!("未找到 tag={tag} 的格子"))
            .1
    }

    /// 序号叶子：(文字, 不透明度)。按树序收集。
    fn index_leaves(v: &View, out: &mut Vec<(String, u8)>) {
        if let Some(t) = &v.text
            && t.chars().all(|c| c.is_ascii_digit())
            && !t.is_empty()
        {
            out.push((t.clone(), v.text_color[3]));
        }
        for c in &v.children {
            index_leaves(c, out);
        }
    }

    const SIX: &[&str] = &["一", "一二三", "一", "一二", "一", "一二"];

    /// 第二行落在第一行下方，同列的格子左缘与宽度都对齐。
    #[test]
    fn cells_align_by_column() {
        let w = grid_win(3, 2, 0, SIX);
        let root = laid(&w);
        for c in 0..3 {
            let (a, b) = (hit(&root, c), hit(&root, c + 3));
            assert!(b.y > a.y, "第 {c} 列：第二行在下方");
            assert_eq!((a.x, a.w), (b.x, b.w), "第 {c} 列：上下对齐");
        }
        // 列宽取该列最宽的格子：第 1 列的「一二三」比第 0 列的「一二」宽。
        assert!(hit(&root, 1).w > hit(&root, 0).w);
    }

    /// 只有高亮行显示序号，其余行的序号是透明的占位（宽度照占，文字才对得齐）。
    #[test]
    fn only_the_highlighted_row_shows_indices() {
        let w = grid_win(3, 2, 1, SIX);
        let mut leaves = Vec::new();
        index_leaves(&laid(&w), &mut leaves);
        let alphas: Vec<u8> = leaves.iter().map(|(_, a)| *a).collect();
        assert_eq!(leaves.len(), 6, "每格都有序号节点");
        assert!(
            alphas[..3].iter().all(|a| *a == 0),
            "非高亮行透明: {alphas:?}"
        );
        assert!(alphas[3..].iter().all(|a| *a > 0), "高亮行可见: {alphas:?}");
    }

    /// 悬停气泡的锚点：展开态贴整个内容区外侧（不是单格侧边），收起态横排放候选下方。
    #[test]
    fn tooltip_anchor_hugs_the_window_when_expanded() {
        let w = grid_win(3, 2, 0, SIX);
        let root = laid(&w);
        let width = root.measured_size().0;
        w.note_grid_box((6.0, 5.0), width);
        let cell = hit(&root, 4);
        assert_eq!(
            w.tip_anchor(&cell),
            (true, 6.0, 6.0 + width, 5.0, 5.0 + cell.h),
            "展开态：停靠在窗口外侧顶端，不随行走"
        );
        let mut flat = grid_win(3, 2, 0, SIX);
        flat.set_grid(None);
        let cell = hit(&laid(&flat), 1);
        assert_eq!(
            flat.tip_anchor(&cell),
            (false, cell.x, cell.x + cell.w, cell.y, cell.y + cell.h),
            "收起态横排：放下方"
        );
    }

    /// 刚展开时重新起算「鼠标动过没有」：窗口往下长到静止的鼠标底下，不该凭空冒出悬停。
    /// 已在展开态时（滚屏、切分类）不动闸门，否则鼠标停在格子上每滚一下悬停就掉一次。
    #[test]
    fn expanding_resets_the_hover_gate_once() {
        let mut w = grid_win(3, 2, 0, SIX);
        w.set_grid(None);
        w.visible = true;
        w.mouse.borrow_mut().engaged = true;
        refill(&mut w, 3, 2, 0, SIX);
        assert!(!w.mouse.borrow().engaged, "收起 → 展开：闸门重新起算");
        w.mouse.borrow_mut().engaged = true;
        refill(&mut w, 3, 2, 1, SIX);
        assert!(w.mouse.borrow().engaged, "展开态里再收到网格帧：不动闸门");
    }

    /// 分类标签栏：只画位集里的标签，各带 `GRID_TAB_TAG_BASE + 编号` 的命中 tag，当前分类画
    /// 选中底色；标签栏在表格下方、与翻页箭头同一行，首行位置与收起态一致。
    #[test]
    fn tab_bar_lists_the_offered_tabs() {
        use wind_ui_types::{GRID_TAB_TAG_BASE as B, GridTab};
        let mut w = grid_win(3, 2, 0, SIX);
        w.set_grid(Some(CandidateGrid {
            cols: 3,
            rows: 2,
            highlight_row: 0,
            index_start: 0,
            index_count: 3,
            tabs: GridTab::All.bit() | GridTab::Single.bit() | GridTab::Rare.bit(),
            active_tab: GridTab::Single as u8,
        }));
        let root = laid(&w);
        let mut hits = Vec::new();
        root.collect_hits(&mut hits);
        let tab_tags: Vec<i32> = hits
            .iter()
            .map(|(t, _)| *t)
            .filter(|t| (B..B + 8).contains(t))
            .collect();
        assert_eq!(tab_tags, vec![B, B + 1, B + 3], "只有给出的三个标签");
        let last_row = hit(&root, 3);
        let tab = hit(&root, B);
        assert!(tab.y >= last_row.y + last_row.h, "标签栏在表格下方");
        // 与翻页箭头同一行：两者纵向区间有重叠。
        let next = hit(&root, TAG_PAGE_NEXT);
        assert!(
            tab.y < next.y + next.h && next.y < tab.y + tab.h,
            "标签与翻页箭头同一行：tab={tab:?} next={next:?}"
        );
        assert!(next.x > tab.x, "翻页箭头在标签右边");
        // 首行的位置与收起态的单行一致：展开是原地往下长出来。
        let mut flat = grid_win(3, 2, 0, SIX);
        flat.set_grid(None);
        assert_eq!(
            hit(&laid(&flat), 0).y,
            hit(&root, 0).y,
            "首行不被标签栏推下去"
        );
        let sel = Some(wind_theme::fallback::SELECTED_BG);
        assert_eq!(bg_of(&root, B + 1).unwrap(), sel, "当前分类画选中底色");
        assert_ne!(bg_of(&root, B).unwrap(), sel);
    }

    /// 末屏只有两行时补占位行，窗口与满屏等高；占位行不是命中目标。
    #[test]
    fn short_screen_is_padded_to_full_height() {
        let full = grid_win(3, 3, 0, &["一"; 9]);
        let short = grid_win(3, 3, 0, &["一"; 6]);
        let (h_full, h_short) = (
            laid(&full).measured_size().1,
            laid(&short).measured_size().1,
        );
        assert_eq!(h_full, h_short);
        let mut hits = Vec::new();
        laid(&short).collect_hits(&mut hits);
        assert_eq!(hits.iter().filter(|(t, _)| *t >= 0 && *t < 100).count(), 6);
    }

    /// 展开态宽度跟着当前可见内容走：滚到窄的一屏，窗口缩回。
    #[test]
    fn width_follows_visible_content_while_expanded() {
        let mut w = grid_win(3, 2, 0, &["一二三四五", "一", "一", "一", "一", "一"]);
        let wide = laid(&w).measured_size().0;
        refill(&mut w, 3, 2, 0, &["一"; 6]);
        assert!(laid(&w).measured_size().0 < wide, "滚到窄的一屏宽度缩回");
    }

    fn bg_of(v: &View, tag: i32) -> Option<Option<[u8; 4]>> {
        if v.tag == tag {
            return Some(v.bg);
        }
        v.children.iter().find_map(|c| bg_of(c, tag))
    }

    /// 选中项是平铺下标：`selected = 3` ⇒ 第二行第一格画选中底色，其余格都不画。
    #[test]
    fn selected_cell_is_addressed_by_flat_index() {
        let w = grid_win(3, 2, 1, SIX);
        let root = laid(&w);
        let sel_bg = Some(wind_theme::fallback::SELECTED_BG);
        assert!(hit(&root, 3).y > hit(&root, 0).y, "第 3 格在第二行");
        for tag in 0..6 {
            let bg = bg_of(&root, tag).expect("每格都在树里");
            assert_eq!(bg == sel_bg, tag == 3, "格 {tag} 的底色 {bg:?}");
        }
    }
}

/// 候选窗尺寸下限（`ui.candidate.min_window_width_*` / `min_window_height_*` / `min_rows`，
/// 抗抖动）。
///
/// 断言含文本尺寸，依赖 mock 文本测量（字符数 × 字号 × 0.6），故与 `view.rs` 的布局测试
/// 一样 gate 掉真实文本后端。
#[cfg(all(test, mock_text))]
mod min_size_tests {
    use super::*;
    use crate::view::Rect;

    fn cand(text: &str) -> CandidateItem {
        cand_c(text, "")
    }

    fn cand_c(text: &str, comment: &str) -> CandidateItem {
        CandidateItem {
            text_styled: Default::default(),
            text: text.to_string(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: comment.into(),
            comment_above: Default::default(),
            no_index: false,
        }
    }

    /// 造一个候选窗：`rows` 是行数下限，`w_dp`/`h_dp` 是窗口宽/高下限（dp）。
    ///
    /// 宽高下限只灌进与 `vertical` 相符的那一项（另一方向留 0），顺带覆盖「读错方向」的回归。
    /// `scale` 钉成 1.0 使 dp == 设备像素。
    fn mk(vertical: bool, rows: u32, w_dp: u32, h_dp: u32, texts: &[&str]) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(vertical, false, false);
        let (wh, wv) = if vertical { (0, w_dp) } else { (w_dp, 0) };
        let (hh, hv) = if vertical { (0, h_dp) } else { (h_dp, 0) };
        w.set_min_size(wh, wv, hh, hv, rows);
        // 序号画成圆圈，使序号节点成为行高的决定项（直径 = index_fs × 1.5，高过文本行高）。
        // 不这样设，占位行即使漏掉序号节点也照样等高 —— 等高性测试就测不出东西来了。
        w.theme.views.index.bg_shape = "circle".to_string();
        let items: Vec<CandidateItem> = texts.iter().map(|t| cand(t)).collect();
        w.update("", 0, "", items, 0, -1, 1, 1);
        w
    }

    /// 只配行数下限的候选窗（`min_rows` 那一族测试用）。
    fn win(vertical: bool, rows: u32, texts: &[&str]) -> CandidateWindow {
        mk(vertical, rows, 0, 0, texts)
    }

    /// 同 `mk`，但候选带注释段。
    fn win_c(vertical: bool, w_dp: u32, items: &[(&str, &str)]) -> CandidateWindow {
        let mut w = mk(vertical, 0, w_dp, 0, &[]);
        let items: Vec<CandidateItem> = items.iter().map(|(t, c)| cand_c(t, c)).collect();
        w.update("", 0, "", items, 0, -1, 1, 1);
        w
    }

    fn measured(w: &CandidateWindow) -> (f32, f32) {
        laid(w, false).measured_size()
    }

    /// 构建 + 布局，返回根节点（`above`=窗口翻到光标上方）。
    fn laid(w: &CandidateWindow, above: bool) -> View {
        let mut root = w.scene.build_tree(&w.scene_input(above));
        root.layout(0.0, 0.0, &w.text_renderer);
        root
    }

    /// 取某条候选行的绝对矩形。`rect` 是 `View` 的私有字段，只能经 `collect_hits` 读到——
    /// 这也正好保证测的是「真正参与命中的那个矩形」。
    fn hit(root: &View, tag: i32) -> Rect {
        let mut hits = Vec::new();
        root.collect_hits(&mut hits);
        hits.iter()
            .find(|(t, _)| *t == tag)
            .unwrap_or_else(|| panic!("未找到 tag={tag} 的候选行"))
            .1
    }

    const FIVE: &[&str] = &["一", "二", "三", "四", "五"];
    const THREE: &[&str] = &["一", "二", "三"];

    // ────────────────── 只有模式徽标时的提示窗宽度 ──────────────────

    /// 造一个「只有模式徽标、没有候选」的提示窗（刚进临时英文/临时拼音的那一帧）。
    fn hint(label: &str, min_w_dp: u32) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(false, false, false);
        w.set_min_size(min_w_dp, 0, 0, 0, 0);
        w.update("", 0, label, vec![], 0, -1, 1, 1);
        w
    }

    /// mock 文本测量的宽度（字符数 × 字号 × 0.6，见模块头注释）。
    fn mock_text_w(w: &CandidateWindow, s: &str) -> f32 {
        s.chars().count() as f32 * w.theme.behavior.font_size as f32 * 0.6
    }

    /// 徽标的 12px 右留白在宽度预算段也须与装配段同条件（无候选时为 0），否则超长编码会提前
    /// 一截被截断。判据取「剩余空隙小于一个字符宽」：截断按整字符走。
    #[test]
    fn hint_window_spends_its_whole_width_budget_on_a_long_preedit() {
        let long = "a'".repeat(200);
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(false, false, false);
        w.set_preedit_embedded(true);
        w.update(&long, long.len(), "临时英文", vec![], 0, -1, 1, 1);
        let width_of = |w: &CandidateWindow| {
            let mut root = w.scene.build_tree(&w.scene_input(false));
            root.layout(0.0, 0.0, &w.text_renderer);
            root.measured_size().0
        };
        let win_w = width_of(&w);
        let cap = w.screen_safety_max_width_px() as f32;

        // 前置①：这一帧必须已经被上限夹住（再加长一倍宽度不变），否则下面的判据恒真。
        let mut w2 = CandidateWindow::new(CandidateWindowConfig::default(), {
            let (tx2, _rx2) = std::sync::mpsc::channel();
            tx2
        })
        .unwrap();
        w2.scale = 1.0;
        w2.set_orientation(false, false, false);
        w2.set_preedit_embedded(true);
        let longer = "a'".repeat(400);
        w2.update(&longer, longer.len(), "临时英文", vec![], 0, -1, 1, 1);
        assert_eq!(
            win_w,
            width_of(&w2),
            "前置：超长编码须已被宽度上限夹住（加长一倍不该再变宽）"
        );
        // 前置②：夹住后不得越界。
        assert!(win_w <= cap + 0.5, "夹住后不该超过上限（{win_w} vs {cap}）");

        let one_char = mock_text_w(&w, "a");
        assert!(
            cap - win_w < one_char,
            "预算该被用满，剩余空隙 {} 不得超过一个字符宽 {}（预算段与装配段的右留白分叉了）",
            cap - win_w,
            one_char
        );
    }

    /// 提示窗该紧贴文字，除了窗口内边距不该再多出固定量（占位行不计宽、无候选时徽标无右留白）。
    /// 判据写成等式而不是「差值一致」：多出来的是固定量，差值比较测不到。
    #[test]
    fn mode_label_hint_window_hugs_its_text() {
        // 16 = 主题默认窗口内边距左右各 8（`window.padding` 的兜底值，scale=1）
        const WINDOW_PAD_LR: f32 = 16.0;
        for label in ["英", "临时英文", "临时英文超长模式名称"] {
            let w = hint(label, 0);
            let got = measured(&w).0;
            let want = mock_text_w(&w, label) + WINDOW_PAD_LR;
            assert!(
                (got - want).abs() < 0.5,
                "徽标「{label}」的提示窗应宽 {want}（文字 + 窗口内边距），实际 {got}"
            );
        }
    }

    /// 占位行的本职不能丢：提示窗的高度仍须与「有一条候选」时一致。
    ///
    /// 它存在的唯一理由就是这个（刚进临英时窗口不要比随后出候选时矮一截）。
    /// 只盯着宽度改，很容易顺手把整行删掉 —— 那样宽度是对了，窗口却开始忽高忽低。
    #[test]
    fn mode_label_hint_window_keeps_candidate_row_height() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut with_cand = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        with_cand.scale = 1.0;
        with_cand.set_orientation(false, false, false);
        with_cand.update("", 0, "临时英文", vec![cand("一")], 0, -1, 1, 1);
        assert_eq!(
            measured(&hint("临时英文", 0)).1,
            measured(&with_cand).1,
            "只有徽标时的窗口高度须与有候选时一致"
        );
    }

    /// 用户显式配了候选窗最小宽度时，仍以配置为准（紧贴文字只是没配时的默认）。
    #[test]
    fn configured_min_width_still_wins_on_hint_window() {
        let w = hint("英", 300);
        assert_eq!(measured(&w).0, 300.0, "配了最小宽度就该撑到 300dp");
    }

    /// 反向对照：有候选时，徽标与候选之间那 12px 留白必须还在 —— 上面那条修的是
    /// 「后面没有候选」，不是把留白一删了事。
    #[test]
    fn mode_label_keeps_its_gap_when_candidates_follow() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut with_label = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        with_label.scale = 1.0;
        with_label.set_orientation(false, false, false);
        with_label.update("", 0, "临时英文", vec![cand("一")], 0, -1, 1, 1);
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let mut no_label = CandidateWindow::new(CandidateWindowConfig::default(), tx2).unwrap();
        no_label.scale = 1.0;
        no_label.set_orientation(false, false, false);
        no_label.update("", 0, "", vec![cand("一")], 0, -1, 1, 1);
        let delta = measured(&with_label).0 - measured(&no_label).0;
        let want = mock_text_w(&with_label, "临时英文") + 12.0; // 徽标文字 + 右留白
        assert!(
            (delta - want).abs() < 0.5,
            "有候选时徽标应占「文字 + 12px 留白」= {want}，实际 {delta}"
        );
    }

    // ─────────────────────────── 行数下限（min_rows） ───────────────────────────

    /// 补出的占位行必须与真实候选行完全等高：「3 条候选 + 2 空行 ≡ 5 条真实候选」才锁得住
    /// 占位行装配不全（漏序号节点、漏 item.margin）。
    #[test]
    fn padded_rows_equal_real_rows_in_height() {
        let full = measured(&win(true, 5, FIVE)).1;
        let padded = measured(&win(true, 5, THREE)).1;
        assert_eq!(padded, full, "3 候选补 2 空行后须与 5 条真实候选等高");
    }

    /// 占位行的文本属性必须逐项镜像真实候选行。
    ///
    /// 补等高性测试测不到的那一半：mock 后端不区分字族与字重，而真实后端行高取自字族
    /// line metrics，只能从结构上比对。
    #[test]
    fn placeholder_row_mirrors_real_row_text_attrs() {
        let mut w = win(true, 5, THREE);
        w.theme.views.text.font_family = Some("宋体".to_string());
        w.theme.views.index.font_family = Some("楷体".to_string());
        let root = w.scene.build_tree(&w.scene_input(false));

        fn find_list(v: &View) -> Option<&View> {
            if v.children.iter().any(|c| c.tag >= 0) {
                return Some(v);
            }
            v.children.iter().find_map(find_list)
        }
        let list = find_list(&root).expect("未找到候选列表容器");
        // 取第 2 条真实候选：首条是选中态，字重可能被 [text.selected] 改写，
        // 而占位行按非选中态构造，拿首条比会误报。
        let real = list
            .children
            .iter()
            .find(|c| c.tag == 1)
            .expect("未找到非选中的真实候选行");
        let ph = list
            .children
            .iter()
            .find(|c| c.tag < 0)
            .expect("未找到占位行");

        assert_eq!(
            ph.children.len(),
            real.children.len(),
            "占位行子节点数须与真实行一致"
        );
        for (i, (p, r)) in ph.children.iter().zip(real.children.iter()).enumerate() {
            assert_eq!(p.font_family, r.font_family, "第 {i} 个子节点字族不一致");
            assert_eq!(p.font_size, r.font_size, "第 {i} 个子节点字号不一致");
            assert_eq!(p.font_weight, r.font_weight, "第 {i} 个子节点字重不一致");
            assert_eq!(p.fixed_h, r.fixed_h, "第 {i} 个子节点固定高不一致");
        }
    }

    /// 候选文字只配底色与圆角、不配边框时，非选中行也要带圆角。
    #[test]
    fn text_bg_radius_applies_without_border() {
        let mut w = win(true, 0, THREE);
        let bg = [1, 2, 3, 255];
        w.theme.views.text.bg_color = Some(bg);
        w.theme.views.text.border_radius = Some(wind_theme::schema::Dim::Dp(6.0));
        let root = w.scene.build_tree(&w.scene_input(false));

        fn find_bg(v: &View, bg: [u8; 4]) -> Option<&View> {
            if v.bg == Some(bg) {
                return Some(v);
            }
            v.children.iter().find_map(|c| find_bg(c, bg))
        }
        fn find_row(v: &View, tag: i32) -> Option<&View> {
            if v.tag == tag {
                return Some(v);
            }
            v.children.iter().find_map(|c| find_row(c, tag))
        }
        let row = find_row(&root, 1).expect("未找到非选中候选行");
        let leaf = find_bg(row, bg).expect("非选中行的文字叶子应带底色");
        assert_eq!(leaf.corner_radius, 6.0, "无边框时圆角也须生效");
    }

    /// 占位行不得参与命中收集：鼠标划过或点击空行都不该有任何反应。
    #[test]
    fn placeholder_rows_are_not_hit_targets() {
        let root = laid(&win(true, 5, THREE), false);
        let mut hits = Vec::new();
        root.collect_hits(&mut hits);
        let cand_hits = hits.iter().filter(|(t, _)| (0..3).contains(t)).count();
        assert_eq!(cand_hits, 3, "只有 3 条真实候选可命中");
        assert!(
            hits.iter().all(|(t, _)| *t < 3),
            "补出的空行不得出现在命中矩形里"
        );
    }

    /// 反向对照：不配 min_rows 时高度必须随候选数变化，否则上一条测的是恒等式。
    #[test]
    fn without_min_rows_height_follows_candidate_count() {
        let h3 = measured(&win(true, 0, THREE)).1;
        let h5 = measured(&win(true, 0, FIVE)).1;
        assert!(h3 < h5, "未配下限时 3 条应矮于 5 条（实测 {h3} vs {h5}）");
    }

    /// 下限不是定值：候选数超过 min_rows 时照常展开，不得反过来把窗口压回去。
    #[test]
    fn min_rows_does_not_shrink_larger_pages() {
        let clamped = measured(&win(true, 3, FIVE)).1;
        let plain = measured(&win(true, 0, FIVE)).1;
        assert_eq!(clamped, plain);
    }

    /// 补行只发生在竖排：横排候选并列于一行，高度本就恒定。
    #[test]
    fn min_rows_is_vertical_only() {
        let padded = measured(&win(false, 5, THREE)).1;
        let plain = measured(&win(false, 0, THREE)).1;
        assert_eq!(padded, plain, "横排不得补行");
    }

    // ─────────────────────── 窗口宽度下限（min_window_width_*） ───────────────────────

    /// 最小宽度：单字候选与三字候选的窗口宽度须相同 —— 用户报的「宽度一直在变」。
    #[test]
    fn min_window_width_stabilizes_width() {
        let one = measured(&mk(true, 0, 300, 0, &["字"])).0;
        let three = measured(&mk(true, 0, 300, 0, &["候选词"])).0;
        assert_eq!(one, three, "配了宽度下限后，1 字候选须与 3 字候选等宽");
        assert_eq!(one, 300.0, "窗口宽度须正好落在下限上（scale=1 ⇒ dp==px）");
    }

    /// 反向对照：不配下限时宽度随内容伸缩（即改动前的行为）。
    #[test]
    fn without_min_window_width_follows_content() {
        let one = measured(&mk(true, 0, 0, 0, &["字"])).0;
        let three = measured(&mk(true, 0, 0, 0, &["候选词"])).0;
        assert!(
            one < three,
            "未配下限时 1 字应窄于 3 字（实测 {one} vs {three}）"
        );
    }

    /// 超过下限的内容照常撑宽：下限是「不得窄于」，不是固定宽度。
    #[test]
    fn min_window_width_does_not_shrink_wider_content() {
        let wide = measured(&mk(true, 0, 60, 0, &["六个汉字的候选"])).0;
        let plain = measured(&mk(true, 0, 0, 0, &["六个汉字的候选"])).0;
        assert_eq!(wide, plain);
    }

    /// 下限量的是窗口，候选自身的宽度与位置一概不动：配下限前后候选命中矩形的 x/w 逐位相等，
    /// 多出来的宽度只能留在右侧空着。
    #[test]
    fn min_window_width_keeps_candidates_in_place() {
        let plain = laid(&mk(false, 0, 0, 0, &["字", "词"]), false);
        let wide = laid(&mk(false, 0, 400, 0, &["字", "词"]), false);
        assert!(
            wide.measured_size().0 > plain.measured_size().0,
            "前置：下限须真的撑宽了窗口，否则下面比的是同一棵树"
        );
        for tag in 0..2 {
            let p = hit(&plain, tag);
            let q = hit(&wide, tag);
            assert_eq!(p.x, q.x, "候选 {tag} 的位置不得因窗口下限改变");
            assert_eq!(p.w, q.w, "候选 {tag} 的宽度不得因窗口下限改变");
        }
    }

    /// 竖排例外：候选行本就 `fill_cross`（高亮宽度统一），窗口被撑宽后高亮须跟着铺满，
    /// 否则高亮只占左边一截、右侧空一块，观感比抖动更糟。
    ///
    /// 与上一条不矛盾：铺满的是行背景，行内的序号/文字/注释仍按内容左对齐。
    #[test]
    fn vertical_highlight_fills_widened_window() {
        let root = laid(&mk(true, 0, 400, 0, &["字"]), false);
        let row = hit(&root, 0);
        let plain = hit(&laid(&mk(true, 0, 0, 0, &["字"]), false), 0);
        assert!(
            plain.w < 200.0,
            "前置：不配下限时行宽应远小于 400（实测 {}）",
            plain.w
        );
        // 400 − 窗口左右内边距（主题默认各 8）= 384；留 24px 余量兜住主题默认值的变化。
        assert!(
            row.w >= 400.0 - 24.0,
            "竖排高亮须随窗口撑满（实测行宽 {}）",
            row.w
        );
    }

    /// 下限量的是整行内容（序号 + 文字 + 注释），注释在下限内时不得改变窗口宽度。
    ///
    /// 注释模板含拆字/编码/注音时，宽度变化比候选文字本身还大。
    #[test]
    fn min_window_width_covers_comment_segment() {
        let plain = measured(&win_c(true, 300, &[("字", "")])).0;
        let commented = measured(&win_c(true, 300, &[("字", "abc")])).0;
        assert_eq!(plain, commented, "注释在下限内时不得改变窗口宽度");
    }

    /// 反向对照：不配下限时，注释会把窗口撑宽 —— 上一条测的不是恒等式。
    #[test]
    fn without_min_window_width_comment_widens_window() {
        let plain = measured(&win_c(true, 0, &[("字", "")])).0;
        let commented = measured(&win_c(true, 0, &[("字", "abc")])).0;
        assert!(
            plain < commented,
            "未配下限时注释应撑宽（{plain} vs {commented}）"
        );
    }

    /// 横排同样按窗口施加下限。
    #[test]
    fn min_window_width_applies_to_horizontal() {
        let one = measured(&mk(false, 0, 400, 0, &["字", "词"])).0;
        let three = measured(&mk(false, 0, 400, 0, &["候选词", "输入法"])).0;
        assert_eq!(one, three);
    }

    /// 横排下限与竖排下限互不影响。只灌一侧字段后按另一个方向渲染：`min_window_w_px()` 读错
    /// 方向时会把本该跟随内容的渲染错锁成定宽。
    #[test]
    fn horizontal_and_vertical_min_width_are_independent() {
        // 只配横排下限，竖排渲染时该下限不得生效。
        let (tx1, _rx1) = std::sync::mpsc::channel();
        let mut v_win = CandidateWindow::new(CandidateWindowConfig::default(), tx1).unwrap();
        v_win.scale = 1.0;
        v_win.set_orientation(true, false, false);
        v_win.set_min_size(400, 0, 0, 0, 0);
        v_win.update("", 0, "", vec![cand("字")], 0, -1, 1, 1);
        let v_one = measured(&v_win).0;
        v_win.update("", 0, "", vec![cand("候选词")], 0, -1, 1, 1);
        let v_three = measured(&v_win).0;
        assert!(
            v_one < v_three,
            "只配横排下限时竖排应照常跟随内容（{v_one} vs {v_three}）"
        );

        // 只配竖排下限，横排渲染时该下限不得生效。
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let mut h_win = CandidateWindow::new(CandidateWindowConfig::default(), tx2).unwrap();
        h_win.scale = 1.0;
        h_win.set_orientation(false, false, false);
        h_win.set_min_size(0, 400, 0, 0, 0);
        h_win.update("", 0, "", vec![cand("字"), cand("词")], 0, -1, 1, 1);
        let h_one = measured(&h_win).0;
        h_win.update("", 0, "", vec![cand("候选词"), cand("输入法")], 0, -1, 1, 1);
        let h_three = measured(&h_win).0;
        assert!(
            h_one < h_three,
            "只配竖排下限时横排应照常跟随内容（{h_one} vs {h_three}）"
        );
    }

    // ─────────────────────── 窗口高度下限（min_window_height_*） ───────────────────────

    /// 最小高度：候选数不同的两页须等高（与 min_rows 殊途同归的那一半）。
    #[test]
    fn min_window_height_stabilizes_height() {
        let three = measured(&mk(true, 0, 0, 400, THREE)).1;
        let five = measured(&mk(true, 0, 0, 400, FIVE)).1;
        assert_eq!(three, five, "配了高度下限后 3 条须与 5 条等高");
        assert_eq!(three, 400.0, "窗口高度须正好落在下限上");
    }

    /// 超过下限的内容照常展开，下限不封顶。
    #[test]
    fn min_window_height_does_not_shrink_taller_content() {
        let clamped = measured(&mk(true, 0, 0, 40, FIVE)).1;
        let plain = measured(&mk(true, 0, 0, 0, FIVE)).1;
        assert_eq!(clamped, plain);
    }

    /// 高度下限存在的理由：`min_rows` 只数候选行，翻页栏的出现/消失它管不着，窗口总高照样跳。
    /// 窗口级下限把它一并罩住。
    ///
    /// 前置断言不可省：若默认主题下翻页栏根本不显示，后半段就成了恒等式（假绿）。
    #[test]
    fn min_window_height_covers_pager_bar() {
        let pages = |total: usize, h_dp: u32| {
            let mut w = mk(true, 0, 0, h_dp, &[]);
            let items: Vec<CandidateItem> = THREE.iter().map(|t| cand(t)).collect();
            w.update("", 0, "", items, 0, -1, 1, total);
            w
        };
        let single = measured(&pages(1, 0)).1;
        let multi = measured(&pages(3, 0)).1;
        assert!(
            single < multi,
            "前置：翻页栏须真的改变窗口高度，否则下面测的是恒等式（{single} vs {multi}）"
        );
        assert_eq!(
            measured(&pages(1, 400)).1,
            measured(&pages(3, 400)).1,
            "高度下限须把翻页栏的出现/消失一并吸收"
        );
    }

    /// 窗口翻到光标上方时，高度富余必须补在顶部。
    ///
    /// 上方显示时底边贴光标，空白压在下面会把候选整体顶离光标，位置反而随内容抖动——
    /// 正是本功能要消除的东西。与 min_rows 的占位行「反转时补顶部」同一判据。
    #[test]
    fn min_window_height_pads_top_when_above() {
        let w = mk(true, 0, 0, 400, &["字"]);
        let below = laid(&w, false);
        let above = laid(&w, true);
        let h = below.measured_size().1;
        assert_eq!(h, above.measured_size().1, "上下方窗口总高须一致");

        let b = hit(&below, 0);
        let a = hit(&above, 0);
        assert!(
            a.y > b.y,
            "上方显示时候选须被推到窗口下部（{} vs {}）",
            a.y,
            b.y
        );
        // 对称判据：上方显示时「候选底边→窗口底边」的空隙，等于下方显示时
        // 「窗口顶边→候选顶边」的空隙（都只剩窗口内边距）。
        let gap_below_top = b.y;
        let gap_above_bottom = h - (a.y + a.h);
        assert!(
            (gap_below_top - gap_above_bottom).abs() < 0.5,
            "上翻时富余须全部落在顶部（顶隙 {gap_below_top} vs 底隙 {gap_above_bottom}）"
        );
    }

    /// 反向对照：未配高度下限时不得平白引入偏移（`main_align` 只在有富余时才有意义）。
    #[test]
    fn without_min_height_above_does_not_shift_content() {
        let w = mk(true, 0, 0, 0, &["字"]);
        assert_eq!(hit(&laid(&w, false), 0).y, hit(&laid(&w, true), 0).y);
    }

    /// 高度下限对横排同样生效（`min_rows` 是竖排专属，这一项不是）。
    #[test]
    fn min_window_height_applies_to_horizontal() {
        let plain = measured(&mk(false, 0, 0, 0, THREE)).1;
        let tall = measured(&mk(false, 0, 0, 200, THREE)).1;
        assert!(plain < 200.0, "前置：横排自然高度须低于下限");
        assert_eq!(tall, 200.0);
    }

    /// 主题的竖排变体：竖排时生效视图换成变体，切回横排恢复。
    #[test]
    fn vertical_theme_variant_follows_orientation() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        let mut theme = wind_theme::Resolved::default();
        theme.views.window.border_radius = Some(wind_theme::schema::Dim::Dp(20.0));
        let mut vv = theme.views.clone();
        vv.window.border_radius = Some(wind_theme::schema::Dim::Dp(6.0));
        theme.views_vertical = Some(vv);
        w.set_theme(theme);
        let r = |w: &CandidateWindow| w.theme.views.window.border_radius;
        assert_eq!(r(&w), Some(wind_theme::schema::Dim::Dp(20.0)));
        w.set_orientation(true, false, false);
        assert_eq!(r(&w), Some(wind_theme::schema::Dim::Dp(6.0)), "竖排取变体");
        w.set_orientation(false, false, false);
        assert_eq!(r(&w), Some(wind_theme::schema::Dim::Dp(20.0)), "切回横排");
    }

    /// 横排/竖排的高度下限互不影响（同宽度那条的判据）。
    #[test]
    fn horizontal_and_vertical_min_height_are_independent() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(true, false, false);
        // 只配横排高度下限，竖排渲染时不得生效。
        w.set_min_size(0, 0, 400, 0, 0);
        w.update("", 0, "", vec![cand("字")], 0, -1, 1, 1);
        assert!(
            measured(&w).1 < 400.0,
            "只配横排下限时竖排高度应照常跟随内容"
        );
    }

    // ─────────────────────────────── dp → 设备像素 ───────────────────────────────

    /// dp 随 DPI 缩放：同一配置在 2x 屏上撑出双倍设备像素。
    ///
    /// 这正是单位从「字符数」改成 dp 后仍能跨 DPI 成立的原因；0 是「不限」而非「0 像素」，
    /// 缩放后仍须是 0，否则未配下限的窗口会被 `max(1)` 抬出一条恒定下限。
    #[test]
    fn min_size_scales_with_dpi() {
        let mut w = mk(true, 0, 200, 150, &["字"]);
        assert_eq!(
            (
                w.scene_input(false).min_window_w_px(),
                w.scene_input(false).min_window_h_px()
            ),
            (200, 150)
        );
        w.scale = 2.0;
        assert_eq!(
            (
                w.scene_input(false).min_window_w_px(),
                w.scene_input(false).min_window_h_px()
            ),
            (400, 300)
        );
        assert_eq!(CandidateScene::dp_to_px(0, 2.0), 0, "0 恒为「不限」");
    }
}

/// 翻页栏并入「编码所在行」（`ui.candidate.pager_in_preedit`）。
///
/// 这个开关的诉求只有一句：竖排时别让翻页栏白占一行。但编码有两种形态，落点因此有两处：
/// 独立编码栏（`candidate_top`）并进栏行右端，内联编码（`candidate_inline`）并进编码自己那一行。
/// 本组钉的是「两种形态表现一致」，以及三条不该被波及的旁路（横排 / 旋转态 / 开关关闭）。
///
/// 断言一律是相对量（谁比谁矮、谁在谁上面、有没有出右缘），与文本后端量出多宽无关，
/// 故不 gate 平台 —— 与 [`width_budget_tests`] 同样的理由。
#[cfg(test)]
#[path = "candidate_window_pager_inline_tests.rs"]
mod pager_inline_tests;

/// 宽度上限：整棵树都不得超过屏幕安全上限。
///
/// 不 gate 平台：断言的是不等式而非具体像素值。上限取 `screen_safety_max_width_px()`；本组
/// 不走渲染入口，`screen_extent` 恒为 None，按兜底常量断言。
#[cfg(test)]
#[path = "candidate_window_width_budget_tests.rs"]
mod width_budget_tests;

/// 竖排「主词条优先」宽度分配：注释过长时让步的是注释，不是候选文字。
/// 分配规则见 `build_tree` 里竖排那一支的注释。mock 后端等宽（0.6em），只用几何断言。
#[cfg(test)]
#[path = "candidate_window_vertical_comment_budget_tests.rs"]
mod vertical_comment_budget_tests;

/// 方案级候选字体（`[candidate] font_family`）的接线。
///
/// 必须 gate 到 Windows：断言的是「换了字族宽度就变」，而 mock 后端测量不看字族（`assert_ne!`
/// 会红、`assert_eq!` 恒真）。先例见 `text/dwrite.rs` 的 `font_plan_tests`。
///
/// 旋转那三条留在 `width_budget_tests` 里不动：它们只用几何、mock 下同样成立。
#[cfg(all(test, windows))]
mod schema_font_tests {
    use super::*;

    fn cand(text: &str) -> CandidateItem {
        CandidateItem {
            text_styled: Default::default(),
            text: text.to_string(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: Default::default(),
            comment_above: Default::default(),
            no_index: false,
        }
    }

    /// 造窗并布局；`preedit` 为空时窗口宽度由候选决定。
    fn build(family: &str, preedit: &str, texts: &[&str]) -> f32 {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_text_family_override(family);
        let items: Vec<CandidateItem> = texts.iter().map(|t| cand(t)).collect();
        w.update(preedit, 0, "", items, 0, -1, 1, 1);
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        root.measured_size().0
    }

    /// 覆盖真的作用到排版上；空串 = 不覆盖。
    ///
    /// 反向对照不可少：只断言「变了」的话，一个「无条件套用覆盖」的实现在空串下也会改字体。
    #[test]
    fn override_changes_layout_and_empty_means_no_override() {
        let texts = ["illill", "候选"];
        let base = build("", "", &texts);
        let overridden = build("Consolas", "", &texts);
        assert_ne!(base, overridden, "方案级字族没作用到排版上");
    }

    /// 覆盖只作用于候选文字：编码栏由 preedit 决定，宽度不得受影响。
    #[test]
    fn override_does_not_touch_the_preedit_bar() {
        // 长编码 + 短候选：窗口宽度由编码栏决定。
        let plain = build("", "iiiiiiiiiiiiiiii", &["一"]);
        let overridden = build("Consolas", "iiiiiiiiiiiiiiii", &["一"]);
        assert_eq!(
            plain, overridden,
            "编码栏宽度受方案级候选字族影响了——覆盖越界到了 text 之外的节点"
        );
    }
}

/// `truncate_text_for_width` 像素级截断——依赖 mock 文本测量（字符数 × 字号 × 0.6），
/// 同 [`min_size_tests`] 一样 gate 掉真实文本后端。
#[cfg(all(test, mock_text))]
mod truncate_text_tests {
    use super::*;

    fn win() -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap()
    }

    /// mock 后端按「字符数 × 字号 × 0.6」估宽，字号 10 → 每字 6.0px。
    fn st() -> TextStyle<'static> {
        TextStyle::new(10.0)
    }

    /// 没超预算：原样返回，不加省略号（没必要动的东西不要动）。
    #[test]
    fn fits_within_budget_returns_unchanged() {
        let w = win();
        // 5 字 × 6.0 = 30，预算 30 恰好放得下。
        assert_eq!(
            w.scene_input(false)
                .truncate_text_for_width("abcde", &st(), 30.0),
            "abcde"
        );
    }

    /// 超预算：二分裁到刚好放得下「前缀+…」的最长前缀，结果宽度必须 ≤ 预算。
    /// "abcdefghij" 10 字，每字宽 6.0；预算 25 时 mid=3 是最大可行值
    /// （(3+1)*6=24≤25，(4+1)*6=30>25），故应得 "abc…"。
    #[test]
    fn exceeds_budget_truncates_with_ellipsis() {
        let w = win();
        let out = w
            .scene_input(false)
            .truncate_text_for_width("abcdefghij", &st(), 25.0);
        assert_eq!(out, "abc…");
        assert!(w.text_renderer.measure(&out, &st()).width <= 25.0);
    }

    /// 预算小到连 1 字+省略号都放不下：至少保留 1 字，不能返回空串（空串等于什么都没显示）。
    #[test]
    fn tiny_budget_keeps_at_least_one_char() {
        let w = win();
        assert_eq!(
            w.scene_input(false)
                .truncate_text_for_width("abcdefghij", &st(), 1.0),
            "a…"
        );
    }

    /// 预算被行内其它成员吃光（≤0）时语义是截到最短，不是「不截断」。
    ///
    /// 反向写法（早退返回原文）会让「预算耗尽」变成「完全放开」，把溢出放大而不是收敛。
    #[test]
    fn non_positive_budget_truncates_to_minimum() {
        let w = win();
        assert_eq!(
            w.scene_input(false)
                .truncate_text_for_width("abcdefghij", &st(), 0.0),
            "a…"
        );
        assert_eq!(
            w.scene_input(false)
                .truncate_text_for_width("abcdefghij", &st(), -50.0),
            "a…"
        );
    }

    /// 空文本：原样返回（不误加省略号）。
    #[test]
    fn empty_text_returns_unchanged() {
        let w = win();
        assert_eq!(
            w.scene_input(false)
                .truncate_text_for_width("", &st(), 25.0),
            ""
        );
        assert_eq!(
            w.scene_input(false).truncate_text_for_width("", &st(), 0.0),
            ""
        );
    }

    /// 用中文候选验证二分逻辑本身不依赖字符集，只依赖 measure() 返回的宽度。
    #[test]
    fn works_with_cjk_text() {
        let w = win();
        // 6 字 × 6.0 = 36；预算 25 时 mid=3（(3+1)*6=24≤25）。
        assert_eq!(
            w.scene_input(false)
                .truncate_text_for_width("一二三四五六", &st(), 25.0),
            "一二三…"
        );
    }

    /// 恰好等于预算的边界：允许（`<=`，不是 `<`），不该多切一个字符。
    #[test]
    fn exact_fit_boundary_is_not_truncated() {
        let w = win();
        // 4 字 × 6.0 = 24，预算恰好 24。
        assert_eq!(
            w.scene_input(false)
                .truncate_text_for_width("abcd", &st(), 24.0),
            "abcd"
        );
    }

    /// 测量样式必须与渲染叶子同构：字重/字族归一化规则照抄 `View::font_weight`（>0 才生效）
    /// 与 `View::font_family`（非空才生效）。少一条就是两套样式，预算按一种字体算、排版按另一
    /// 种走，差值累积成窗口右侧留白或右缘溢出。
    #[test]
    fn measure_style_mirrors_view_leaf_normalization() {
        // 字重 0/负数 → 继承默认（0），正数才生效
        assert_eq!(CandidateScene::measure_style(10.0, 0, None).weight, 0);
        assert_eq!(CandidateScene::measure_style(10.0, -1, None).weight, 0);
        assert_eq!(CandidateScene::measure_style(10.0, 700, None).weight, 700);
        // 空/纯空白字族 → None（用渲染器全局字族）
        assert_eq!(
            CandidateScene::measure_style(10.0, 0, Some("")).family,
            None
        );
        assert_eq!(
            CandidateScene::measure_style(10.0, 0, Some("   ")).family,
            None
        );
        assert_eq!(
            CandidateScene::measure_style(10.0, 0, Some("宋体")).family,
            Some("宋体")
        );
    }
}

/// 最大最小公平分配（water-filling）——纯算术，不依赖文本后端，故不 gate 平台。
#[cfg(test)]
#[path = "candidate_window_water_fill_tests.rs"]
mod water_fill_tests;

/// `[ui.font]` → [`FontPlan`] 的折叠规则。平台无关（不碰窗口/COM），故随 Linux CI 跑。
#[cfg(test)]
#[path = "candidate_window_font_plan_build_tests.rs"]
mod font_plan_build_tests;

#[cfg(test)]
#[path = "candidate_window_tests.rs"]
mod tests;

/// 主题节点字族的存在性检查接线。
///
/// 只在 mock 后端下编译：只有 mock 有 `asked_families()` 读回口，而「有没有真的去问」正是这条
/// 链钉得住的一半。
#[cfg(all(test, mock_text))]
mod theme_font_check_tests {
    use super::*;
    use wind_theme::RvNode;

    fn node(family: &str) -> RvNode {
        RvNode {
            font_family: Some(family.to_string()),
            ..Default::default()
        }
    }

    fn win() -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap()
    }

    #[test]
    fn switching_theme_asks_once_per_distinct_declared_family() {
        let mut w = win();
        let before = w.text_renderer.asked_families().len();

        let mut theme = wind_theme::Resolved::default();
        theme.views.item = node("Consolas");
        theme.views.item.selected = Some(Box::new(node("Consolas Bold")));
        theme.views.text = node("思源宋体");
        // 同一个字族在两个节点上声明：缺失时刷两条一模一样的 warn 没有意义，只问一次。
        theme.views.index = node("思源宋体");
        w.set_theme(theme);

        assert_eq!(
            &w.text_renderer.asked_families()[before..],
            ["Consolas", "Consolas Bold", "思源宋体"],
            "每个不同的声明字族问且只问一次，含状态 patch"
        );
    }

    /// 反证这条接线真的在钉东西：没声明字族的主题一个都不该问——否则上面那条断言在
    /// 「把整份节点表都问一遍」这种实现下也照样绿。
    #[test]
    fn a_theme_without_font_family_asks_nothing() {
        let mut w = win();
        let before = w.text_renderer.asked_families().len();
        w.set_theme(wind_theme::Resolved::default());
        assert_eq!(w.text_renderer.asked_families().len(), before);
    }
}

/// 字体名解析的接线：旧 GDI face name 在每个字体入口都换成 family + 字重。
///
/// 只在 mock 后端下编译，理由同上：它靠 `set_mock_families` 注入「系统字体集」。
#[cfg(all(test, mock_text))]
mod font_name_resolve_tests {
    use super::*;

    fn win() -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.text_renderer.set_mock_families(&[
            "思源宋体",
            "霞鹜文楷",
            "Consolas",
            DEFAULT_FONT_FAMILY,
        ]);
        w
    }

    #[test]
    fn legacy_global_family_becomes_family_plus_default_weight() {
        let mut w = win();
        w.set_font_family("思源宋体 SemiBold");
        assert_eq!(w.text_renderer.font_family(), "思源宋体");
        assert_eq!(w.text_renderer.default_weight(), 600);
        // 回退链的链首必须与 TextFormat 的字族逐字相同，否则 AddMapping 认不出段。
        w.set_font_plan("思源宋体 SemiBold", &["霞鹜文楷 Medium".to_string()], &[]);
        assert_eq!(
            w.text_renderer.font_plan().chain_for(None),
            ["思源宋体".to_string(), "霞鹜文楷".to_string()]
        );
    }

    #[test]
    fn font_weight_is_clamped_to_dwrite_range() {
        let mut w = win();
        w.set_font_weight(5000);
        assert_eq!(w.text_renderer.default_weight(), 950);
        w.set_font_weight(-3);
        assert_eq!(w.text_renderer.default_weight(), 0, "负数 = 不指定");
        w.set_font_weight(1);
        assert_eq!(w.text_renderer.default_weight(), 1);
    }

    #[test]
    fn explicit_weight_beats_the_weight_in_the_name() {
        let mut w = win();
        w.set_font_family("思源宋体 SemiBold");
        w.set_font_weight(300);
        assert_eq!(w.text_renderer.default_weight(), 300);
        w.set_font_weight(0);
        assert_eq!(
            w.text_renderer.default_weight(),
            600,
            "0 = 回到名字里的字重"
        );
        w.set_font_family("思源宋体");
        assert_eq!(w.text_renderer.default_weight(), 0, "新写法不带字重");
    }

    #[test]
    fn a_correct_family_name_is_passed_through_untouched() {
        let mut w = win();
        w.set_font_family("Consolas");
        assert_eq!(w.text_renderer.font_family(), "Consolas");
        assert_eq!(w.text_renderer.default_weight(), 0);
    }

    #[test]
    fn theme_and_scheme_families_go_through_the_same_resolution() {
        let mut w = win();
        let mut theme = wind_theme::Resolved::default();
        theme.views.comment.font_family = Some("霞鹜文楷 Medium".to_string());
        w.set_theme(theme);
        w.set_text_family_override("思源宋体 SemiBold");
        let v = &w.theme.views;
        assert_eq!(v.comment.font_family.as_deref(), Some("霞鹜文楷"));
        assert_eq!(v.comment.font_weight, 500);
        assert_eq!(v.text.font_family.as_deref(), Some("思源宋体"));
        assert_eq!(v.text.font_weight, 600);
    }

    /// 用户字重直接覆盖主题里所有节点（含选中等状态）写了的字重：主题给 `[text]` 写 500、
    /// `[text.selected]` 写 700 时，用户 `ui.font.weight = 900` 也必须生效。
    #[test]
    fn user_weight_overrides_every_theme_weight() {
        let mut w = win();
        let mut theme = wind_theme::Resolved::default();
        theme.views.text.font_weight = 500;
        theme.views.text.selected = Some(Box::new(wind_theme::RvNode {
            font_weight: 700,
            ..Default::default()
        }));
        w.set_theme(theme);
        assert_eq!(w.theme.views.text.font_weight, 500, "无用户字重：主题原样");
        w.set_font_weight(900);
        let v = &w.theme.views;
        assert_eq!(v.text.font_weight, 900, "普通字重让位给用户字重");
        assert_eq!(w.text_renderer.default_weight(), 900);
        assert_eq!(
            v.text.selected.as_ref().unwrap().font_weight,
            900,
            "选中态同样跟随用户字重"
        );
        w.set_font_weight(300);
        let v = &w.theme.views;
        assert_eq!(v.text.font_weight, 300);
        assert_eq!(
            v.text.selected.as_ref().unwrap().font_weight,
            300,
            "用户字重直接覆盖，选中态也跟随"
        );
        w.set_font_weight(0);
        assert_eq!(w.theme.views.text.font_weight, 500, "清掉用户字重后原样");
    }

    /// 用户 `ui.font.weight` 压过主题旧字体名拆出的字重，也压过节点显式写的加粗字重（800）。
    #[test]
    fn user_weight_beats_name_weight_and_explicit_node_weight() {
        let mut w = win();
        let mut theme = wind_theme::Resolved::default();
        theme.views.comment.font_family = Some("霞鹜文楷 Medium".to_string());
        theme.views.text.font_family = Some("思源宋体 SemiBold".to_string());
        theme.views.text.font_weight = 800;
        w.set_theme(theme);
        assert_eq!(
            w.theme.views.comment.font_weight, 500,
            "无用户字重：名字字重生效"
        );
        w.set_font_weight(300);
        let v = &w.theme.views;
        assert_eq!(v.comment.font_weight, 0, "用户字重生效：节点继承渲染器默认");
        assert_eq!(w.text_renderer.default_weight(), 300);
        assert_eq!(v.text.font_weight, 300, "节点显式写的加粗也让位于用户字重");
        w.set_font_weight(0);
        assert_eq!(
            w.theme.views.comment.font_weight, 500,
            "清掉用户字重后回到名字字重"
        );
    }
}

/// 外观覆盖主题（R3）：候选窗字体 / 字号的生效优先级。
///
/// 断言落在 `build_tree` 产出的叶子上——渲染与测量（`measure_style`）读的是同一份生效主题，
/// 叶子字族/字号就是两条路径共同的输入。mock 后端测量不看字族，故字族只做结构断言，
/// 字号额外做一条测量断言。
#[cfg(all(test, mock_text))]
mod font_precedence_tests {
    use super::*;
    use wind_theme::RvNode;

    fn node(family: &str) -> RvNode {
        RvNode {
            font_family: Some(family.to_string()),
            ..Default::default()
        }
    }

    /// 主题给候选窗全部相关节点都配了字族（编码栏、序号、文字、注释、翻页栏、模式徽标）。
    fn themed() -> wind_theme::Resolved {
        let mut t = wind_theme::Resolved::default();
        t.views.text = node("主题宋体");
        t.views.index = node("主题楷体");
        t.views.comment = node("主题楷体");
        t.views.preedit_bar = node("主题黑体");
        t.views.footer_bar = node("主题黑体");
        t.views.mode_label = node("主题黑体");
        t
    }

    fn win(theme: wind_theme::Resolved) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_theme(theme);
        w
    }

    fn fill(w: &mut CandidateWindow) {
        let items = ["候选", "词"]
            .iter()
            .map(|t| CandidateItem {
                text_styled: Default::default(),
                text: t.to_string(),
                code: String::new(),
                label: String::new(),
                tooltip: Default::default(),
                comment: "注".into(),
                comment_above: Default::default(),
                no_index: false,
            })
            .collect();
        w.update("abc", 3, "中", items, 0, -1, 1, 2);
    }

    /// 整棵树所有叶子声明的字族（去重），以及候选文字叶子的字族。
    fn families(w: &mut CandidateWindow) -> (Vec<String>, Option<String>) {
        fill(w);
        let root = w.scene.build_tree(&w.scene_input(false));
        fn walk(v: &View, all: &mut Vec<String>, text: &mut Option<String>) {
            if let Some(f) = &v.font_family
                && !all.contains(f)
            {
                all.push(f.clone());
            }
            if v.text.as_deref() == Some("候选") {
                *text = v.font_family.clone();
            }
            for c in &v.children {
                walk(c, all, text);
            }
        }
        let (mut all, mut text) = (Vec::new(), None);
        walk(&root, &mut all, &mut text);
        (all, text)
    }

    #[test]
    fn default_tier_no_family_anywhere() {
        let mut w = win(wind_theme::Resolved::default());
        let (all, text) = families(&mut w);
        assert!(
            all.is_empty(),
            "无主题/用户/方案字族时叶子应全部走内置默认：{all:?}"
        );
        assert_eq!(text, None);
    }

    #[test]
    fn theme_tier_applies_when_user_family_empty() {
        let mut w = win(themed());
        w.set_font_family("");
        let (all, text) = families(&mut w);
        assert_eq!(text.as_deref(), Some("主题宋体"));
        assert!(
            all.contains(&"主题黑体".to_string()),
            "非文字节点仍跟主题：{all:?}"
        );
    }

    /// 用户字体非空：全部节点（不只候选文字）的主题字族都让位，叶子回落全局字族（=用户字体）。
    #[test]
    fn user_tier_beats_theme_on_every_node() {
        let mut w = win(themed());
        w.set_font_family("用户字体");
        let (all, text) = families(&mut w);
        assert_eq!(text, None, "候选文字应回落全局字族（用户字体）");
        assert!(all.is_empty(), "仍有节点带着主题字族：{all:?}");
    }

    /// 顺序无关：先设用户字体、后换主题，结果相同（启动时两条命令的到达序不承重）。
    #[test]
    fn user_tier_survives_a_later_theme_switch() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.set_font_family("用户字体");
        w.set_theme(themed());
        let (all, _) = families(&mut w);
        assert!(all.is_empty(), "{all:?}");
    }

    #[test]
    fn scheme_tier_beats_user_on_text_only() {
        let mut w = win(themed());
        w.set_font_family("用户字体");
        w.set_text_family_override("方案字体");
        let (all, text) = families(&mut w);
        assert_eq!(text.as_deref(), Some("方案字体"));
        assert_eq!(all, vec!["方案字体".to_string()], "方案级只作用于候选文字");
    }

    #[test]
    fn clearing_user_family_falls_back_to_theme() {
        let mut w = win(themed());
        w.set_font_family("用户字体");
        w.set_font_family("  ");
        let (_, text) = families(&mut w);
        assert_eq!(text.as_deref(), Some("主题宋体"));
    }

    /// 被用户字体盖掉的主题字族不该被问存在性（否则对用不到的字体报缺失）。
    #[test]
    fn overridden_theme_families_are_not_checked() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.set_font_family("用户字体");
        let before = w.text_renderer.asked_families().len();
        w.set_theme(themed());
        assert_eq!(&w.text_renderer.asked_families()[before..], [] as [&str; 0]);
    }

    /// 字号：用户值 > 0 胜过主题，0 跟随主题；测量与叶子同源（窗口宽度随之变）。
    #[test]
    fn font_size_user_beats_theme_and_zero_follows() {
        let mut t = wind_theme::Resolved::default();
        t.behavior.font_size = 18;
        let mut w = win(t);
        fill(&mut w);
        let width = |w: &CandidateWindow| {
            let mut root = w.scene.build_tree(&w.scene_input(false));
            root.layout(0.0, 0.0, &w.text_renderer);
            root.measured_size().0
        };
        let theme_w = width(&w);
        assert_eq!(w.base_font_logical, 18.0);
        w.set_font_size_override(30.0);
        assert_eq!(w.base_font_logical, 30.0);
        let user_w = width(&w);
        assert!(user_w > theme_w, "用户字号没作用到测量上");
        w.set_font_size_override(0.0);
        assert_eq!(w.base_font_logical, 18.0, "0 = 跟随主题");
    }

    /// 渲染器基准字号随生效字号走，不只在换 DPI 时更新。
    #[test]
    fn renderer_base_size_follows_effective_font_size() {
        let mut t = wind_theme::Resolved::default();
        t.behavior.font_size = 18;
        let mut w = win(t);
        w.set_font_size_override(30.0);
        assert_eq!(w.text_renderer.base_size(), 30.0, "改字号后基准字号没跟上");
        let mut t2 = wind_theme::Resolved::default();
        t2.behavior.font_size = 22;
        w.set_font_size_override(0.0);
        w.set_theme(t2);
        assert_eq!(w.text_renderer.base_size(), 22.0, "换主题后基准字号没跟上");
    }
}

/// 候选右键对悬停气泡的压制（设计 `candidate-tooltip-sections.md` §7.6 规则 A）。
///
/// 非 Windows 下 `GetCursorPos` 是 mock（恒 (0,0)），物理移动门控靠每次移动前把
/// `last_cursor` 拨开来模拟「鼠标真动了」。时间一律显式传入 `flush`，不依赖真实流逝；
/// 「菜单是否可见」是线程局部，测试线程各自一份。
#[cfg(test)]
#[path = "candidate_window_tip_hold_tests.rs"]
mod tip_hold_tests;

// 直立态逐格切叶子时，分段颜色跟着格走（设计 text-span-colors.md §7.1）。
#[cfg(test)]
#[path = "candidate_window_upright_color_run_tests.rs"]
mod upright_color_run_tests;

// 注释的分段颜色：按主题与候选状态求色，接到注释叶子的 color_runs（设计 text-span-colors.md §6）。
#[cfg(test)]
#[path = "candidate_window_comment_color_tests.rs"]
mod comment_color_tests;

// 上方注释条（`CandidateItem.comment_above`，设计 candidate-comment-above-line.md §3.3）。
//
// host 上文本测量是 mock 近似，下面凡涉及尺寸的断言都写成相对关系（等高、同一 x、差值 ==
// 某叶子外框），测的是盒模型结构。唯一只在 mock 下成立的前提见各测试注释。
#[cfg(test)]
#[path = "candidate_window_comment_above_tests.rs"]
mod comment_above_tests;

// 渲染 golden 对拍（Linux mock 后端才有绘制调用记录）。见模块文档。
#[cfg(all(test, mock_text))]
mod render_golden;

/// 场景已迁入 wind-scene；保留 `scene::` 这个路径名给本模块与子模块用。
use wind_scene::candidate as scene;
mod preview;
pub use preview::PreviewQueue;
mod skin_frame;

/// 用户外观覆盖：阴影开关与位置偏移（ui.candidate.shadow / offset_x / offset_y）。
///
/// 只在 mock 后端下编译（要在无显示器的 host 上造出 `CandidateWindow`）。
#[cfg(all(test, mock_text))]
mod user_shadow_offset_tests {
    use super::*;
    use wind_theme::schema::Dim;

    fn win_with_shadow_theme() -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        let mut theme = wind_theme::Resolved::default();
        theme.views.shadow_blur = Some(Dim::Dp(8.0));
        theme.views.shadow_offset_y = Some(Dim::Dp(4.0));
        theme.views.shadow_color = Some([0, 0, 0, 96]);
        theme.views.window_offset_x = Some(Dim::Dp(2.0));
        theme.views.window_offset_y = Some(Dim::Dp(3.0));
        w.set_theme(theme);
        w
    }

    #[test]
    fn shadow_follows_theme_by_default() {
        let w = win_with_shadow_theme();
        assert!(w.scene.shadow_params(&w.scene_input(false)).is_some());
    }

    /// 关掉阴影后扩边归零——窗口与内容等大，`show(px - ml, py - mt)` 的补偿随之为 0。
    #[test]
    fn turning_shadow_off_removes_the_margins() {
        let mut w = win_with_shadow_theme();
        assert_ne!(
            w.scene
                .shadow_params(&w.scene_input(false))
                .unwrap()
                .margins(),
            (0, 0, 0, 0)
        );
        w.set_theme_shadow(false);
        assert!(w.scene.shadow_params(&w.scene_input(false)).is_none());
    }

    #[test]
    fn turning_shadow_back_on_restores_it_without_a_theme_reload() {
        let mut w = win_with_shadow_theme();
        w.set_theme_shadow(false);
        w.set_theme_shadow(true);
        assert!(w.scene.shadow_params(&w.scene_input(false)).is_some());
    }

    /// 主题的 position_offset 补偿的是发光边等装饰，与阴影无关，关阴影时必须保留。
    #[test]
    fn theme_position_offset_survives_shadow_off() {
        let mut w = win_with_shadow_theme();
        let before = w.position_offset_px();
        w.set_theme_shadow(false);
        assert_eq!(w.position_offset_px(), before);
        assert_ne!(before, (0, 0));
    }

    #[test]
    fn user_offset_adds_to_theme_offset() {
        let mut w = win_with_shadow_theme();
        let (tx, ty) = w.position_offset_px();
        w.set_user_offset(5, -7);
        let s = w.scale;
        assert_eq!(
            w.position_offset_px(),
            (
                tx + (5.0 * s).round() as i32,
                ty + (-7.0 * s).round() as i32
            )
        );
    }
}
