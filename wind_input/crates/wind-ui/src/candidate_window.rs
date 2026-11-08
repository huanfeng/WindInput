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
/// 它同时是 `TextRenderer::new` 的初始字族与回退链的链首，**必须只有一个来源**——
/// 两处写死同一个字符串，改一处漏一处时的表现是「链首与实际 base family 对不上」，
/// 而那只会让回退链静默不生效，不报任何错。
pub const DEFAULT_FONT_FAMILY: &str = "Microsoft YaHei UI";

/// 空字族回落内置默认。
///
/// ★ 抽成函数是因为 [`build_font_plan`] 必须用同一个答案：两处各写一次
/// `if empty { … }` 的话，回退链的链首会变成空串，而 `AddMapping` 的 `baseFamilyName`
/// 是空串就永远匹配不上任何段——表现是「配了 fallback，但只在显式写了 family 时才生效」，
/// 且没有任何报错。
fn resolve_font_family(family: &str) -> &str {
    let f = family.trim();
    if f.is_empty() { DEFAULT_FONT_FAMILY } else { f }
}

/// 把 `[ui.font]` 的三个键折成渲染层的 [`FontPlan`]。
///
/// 纯函数：它承载了本功能全部的「配置怎么变成方案」的判定（链首归一、未知类名处置），
/// 而这些判定与窗口/COM 都无关，抽出来才测得到（构造 `CandidateWindow` 要真窗口）。
///
/// 未知的脚本类名记一条 warn 后**忽略**：配置文件是用户手写的，拼错一个键不该让整份
/// 字体配置失效，更不该 panic。
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
    // 不靠 data/config.toml 的默认值而在这里注入，是因为配置层写回不剔除「等于默认」的键：
    // 老用户的 config.toml 里已经躺着一句 `scripts = {}`，改出厂值对他们永远不生效。
    // 用户显式写了 `emoji = [...]`（含空表）即按用户的来，空表就是「关掉这条指派」。
    //
    // 为什么必须指派而不是只靠系统回退（`DEFAULT_EMOJI_FAMILY` 的文档有全文）：
    // - 新码位（Emoji 16.0 的 🫜 一类）本机 Segoe UI Emoji 明明有字形，DirectWrite 内置的
    //   回退区间表却没把它映射过去，量出来是缺字宽度；
    // - 键帽序列（1️⃣）的基字「1」基准字体自己就有，回退根本不会触发，只有把整串切进
    //   emoji 段才能连字（见 `script::font_runs` 的回溯改写）。
    //
    // 已知代价：`U+2600..27BF` 里 ★ ☆ ✓ 这些中性符号会跟着搬到 Segoe UI Emoji（分类表
    // `script.rs` 对这段的取舍写明了「声明后一并跟着走」）；`declared()` 非空后 `font_runs`
    // 在每次绘制每个文本叶子上都会跑一遍，实测每叶子几个字、微秒级。
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
/// 查询失败按允许。其它平台本期不播放皮肤动画，只画首帧。
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

#[cfg(not(windows))]
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
    /// **生效**主题：[`Self::theme_source`] 叠上外观覆盖（字体 / 字号）之后的那份。
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
    /// 候选**文字节点**的字族覆盖（方案级 `[candidate] font_family`）；空 = 不覆盖。
    ///
    /// 优先级：方案 > 用户 `ui.font.family`（非空）> 主题节点 `views.text.font_family` >
    /// 内置默认（合并见 [`scene::effective_theme`]）。
    /// ⚠️ 只作用于候选文字：序号/编码栏/注释/翻页栏是拉丁与数字，跟着换蒙文字体反而更差；
    /// 要按脚本换字体用全局 `ui.font.scripts`（按字符分，比按节点分更贴合真实问题）。
    text_family_override: String,
    /// 候选列表顺时针旋转 90° 呈现（蒙古文等纵向书写脚本）。来自 ui.candidate.layout。
    ///
    /// ★ 旋转态下 [`Self::vertical`] 是 **false**——屏幕上候选确是并列的，于是所有按方向
    /// 分叉的既有判据（窗口尺寸下限、注释模板、`flip_when_above`）自动走横排那一支。
    /// 唯一额外判 `rotated` 的地方是**列表怎么构造**：局部空间里它按竖排堆叠，
    /// 转完才成为并列的列（见 `build_tree` 的 `list_vertical`）。
    rotated: bool,
    /// 旋转态下把每个字逆时针扶正、逐字下行（对联式竖排）。来自 ui.candidate.layout。
    ///
    /// ★ 蕴含 [`Self::rotated`]：它与旋转态是**同一种排列**，只有叶子怎么搭不同。
    /// 于是列表构造、逆序、阴影轴、尺寸下限、分隔方向、翻页栏位置全部原样复用，
    /// 本位只在 `build_tree` 拼文字/序号叶子时读一次。
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
    /// 固定位置模式的**内容左上**屏幕坐标；`None`=跟随光标（默认）。
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
    /// # ⚠️ 它是「替换」不是「补充」
    ///
    /// 声明了它，候选文字就**不再用** `ui.font.family`——升级前候选文字一律走全局主字体，
    /// 升级后凡声明本键的方案都改用它。两个字体在同一 `font_size` 下视觉大小本就不同
    /// （字形在 em 框内的占比各家不一），故「换了个字体」在用户眼里等同于「字被缩放了」。
    /// 这一行日志是那件事的唯一线索：换字族不经任何会失败的调用，画面之外无迹可寻。
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
    /// ⚠️ 三位并非正交，合法值只有四个；协调器只会发出其中之一，这里 debug 下断言两条
    /// 非法组合，把「谁手写了字段」挡在开发期。
    pub fn set_orientation(&mut self, vertical: bool, rotated: bool, upright: bool) {
        debug_assert!(!(vertical && rotated), "vertical 与 rotated 不能同时为真");
        // 写成蕴含式（`!upright || rotated`）而不是 `!(upright && !rotated)`：
        // 后者在 clippy 1.96 上会报 nonminimal_bool（1.97 放宽），而 macOS 验证机的
        // Homebrew 工具链就停在 1.96。两种写法等价，取两个版本都干净的那个。
        debug_assert!(!upright || rotated, "upright 蕴含 rotated");
        if (vertical, rotated, upright) != (self.vertical, self.rotated, self.upright) {
            // ⚠️ upright 会把整串按格切开逐格排版，这会**切断连写脚本**（蒙古文、阿拉伯文）
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
    /// 场景输入一律读存下的值：一帧里要借好几次输入，每次都查显示器（失败时每次还刷一条 warn）
    /// 没有必要，同一帧内光标点与固定点也不会变。
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
        self.scene.next_anim_at().map(|d| self.anim_start + d)
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
    /// **所有**字体名入口（`ui.font.family` / `fallback` / `scripts`、方案级
    /// `[candidate] font_family`、主题节点 `font_family`）都走这一个函数，判定顺序见
    /// [`resolve_font_name`]：原串是 family 就原样用；否则剥掉结尾字重词、再否则按 face 全名
    /// 查——存量配置里的旧 GDI face name（「思源宋体 SemiBold」）靠这两步自愈（看板 A2-1）。
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
    /// # ★ 缺失的判据是「解析后仍 `Missing`」，不是 `family_exists != Some(true)`
    ///
    /// `Unknown` 表示**查不了**（mock 后端、或拿不到系统字体集）。把它也当成缺失会让
    /// 非 Windows 平台每次设字体都刷一条假 warn——「查不到」与「确实没有」是两件事。
    ///
    /// `source` 写配置键的**全名**：用户看到日志后要能直接去改那一项，
    /// 而几个来源（全局主字体 / 回退链 / 方案级 / 主题节点）改的是不同的地方。
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
    /// ⚠️ 链首不在这里查存在性：它就是 `family`，已由 [`Self::set_font_family`] 查过，
    /// 同一条命令里查两遍就是同一条 warn 刷两次。
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
    /// ★ **判据的真相源只能在这一侧**：`placed_above` 要窗口尺寸 + 屏幕工作区才算得出，
    /// `vertical` 还会被模式级强制横/竖排改写 —— 协调器读配置推不出来。故复用
    /// [`CandidateScene::above_layout`] 这个既有的单一真相源，把它第一个返回值送过去，
    /// 由协调器把 `highlight_up` / `highlight_down` 的走向翻转（视觉方向优先于候选序）。
    ///
    /// 调用点＝每处「`placed_above` 刚刚定完」之后，以及 [`CandidateWindow::hide`]
    /// 清除上翻粘滞时 —— 后者关掉的是「组合结束后状态残留为 true，下一轮首帧渲染前
    /// 用户就按了方向键」的窗口期。
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

    /// 标记窗口进入可见态；**不可见 → 可见**的转换处重采鼠标防抖基线。
    ///
    /// # ★★★ 基线必须在「窗口即将出现」时采样，不能沿用 `hide()` 那次
    ///
    /// 防抖的门控是「物理光标坐标与基线相同 ⇒ 是内容刷新引起的伪移动 ⇒ 忽略」。基线一旦
    /// 陈旧，判据就整个反转：窗口出现在**新**光标位下方时，Windows 投来的进入消息坐标与
    /// 陈旧基线不同 → 被判成「用户真实移动了鼠标」→ 闸门放行 → 60ms 后自动高亮并弹 tooltip，
    /// 而用户根本没动过鼠标。这正是本防抖要挡的那件事。
    ///
    /// 此前基线只在 [`CandidateWindow::hide`] 里采，于是两种情况必然失效：
    /// - **进程内第一次显示**：此前没有任何 `hide()`，基线是构造初值 `i32::MIN`，必不相等；
    /// - **两次显示之间用户移动过鼠标**：`hide()` 采的是上一次组合结束时的光标位，已经过时。
    ///
    /// 采样点挪到显示转换处后，两者一并消失——「窗口出现瞬间鼠标在哪」才是这个判据要的基准。
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

    /// 设置拆字字根字体（TTF 路径 + DWrite 家族名）。**两个渲染器都要收到**。
    ///
    /// 字根字符落在私用区（PUA），系统字体里没有对应字形，须靠渲染器把 PUA 连续段切到字根
    /// 字体集（`text::dwrite` 的 `pua_runs` + `SetFontCollection`，测量与绘制共用同一函数）。
    /// 那套机制本身与控件无关，但**它只对调用过本方法的那个 `TextRenderer` 实例生效**。
    ///
    /// 本方法此前名为 `set_tooltip_chaizi_font`，只转发给 `self.tooltip` —— 于是候选窗自己的
    /// `self.text_renderer` 从未拿到过字根字体。悬停提示里的字根正常、候选行里的字根显示成
    /// 豆腐块或空白，而配置/文件/家族名/日志四项全对，查不出所以然
    /// （同 `project_chaizi_font_pua_plane_gap` 的形态：**四项全对就该查消费端，不是资源本身**）。
    /// 此前无人发现是因为候选行从来没有渲染过字根——注释段接入拆字来源后才第一次有。
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

    /// 由原始主题 + 外观覆盖算出生效主题（[`Self::theme`]）与生效字号——**唯一**的合并点。
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
        // 存在性告警按**生效**字族查（叠上用户字体之后、方案级字体之前，见
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
    /// 全局 `ui.font.family` 与方案级 `[candidate] font_family` 早有这道查，主题节点一直漏着
    /// ——而主题是最容易写错的那处：作者自己机器上装着那款字体，换台机器就静默回落，
    /// 画面只表现为「字体不对」，日志里一片安静。
    ///
    /// 只在换主题时走（`push_theme` 的那几个低频事件：启动 / 切主题 / 深浅色 / 配置 reload），
    /// 不在渲染热路径——每个字族一次 `FindFamilyName` 是按 COM 调用计费的。
    ///
    /// **按字族去重**：一份主题里十来个节点常配同一个名字，缺失时刷十条一模一样的 warn
    /// 只会把日志淹掉。留第一个声明它的节点路径，用户照着去主题文件里搜那个名字就够了。
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
            // 气泡贴的是**窗口实际的**右缘：渲染入口随后会按屏幕安全宽度收窄窗口，超宽的网格
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
    /// - 展开态：**停靠**在整个窗口外侧的顶端（右侧优先、放不下改左），不随悬停行上下移动，
    ///   只换内容。按单格放的话，横排的「下方」与竖排的「侧边」都会盖住相邻的格子；跟着行
    ///   上下走则是「半跟随」——位置在变却够不着，鼠标斜着过去一路擦过别的格子。
    /// - 竖排 / 旋转态：候选项侧边（旋转态的候选项是又高又窄的一列，放上下会离得很远）。
    /// - 横排：候选项下方（不足时上翻）。
    ///
    /// ★ 三处定位（本地显示 `update_tooltip`、host-render `render_tooltip_frame`、外部宿主
    /// `render_tooltip_overlay`）都从这里取，一处放侧边、一处放上下的话，同一个悬停在
    /// 「实时显示」与「重推帧」之间会跳位置。
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

    /// 当前候选窗**内容左上**屏幕坐标（窗口左上 + 阴影扩边）。
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
        self.split_pass_decor = true;
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
        // ⚠️ 本路径一直按光标点取，不像另两个渲染入口那样在固定位置模式下改按固定点取；
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

        // 定位提前到 paint 前（供 flip_when_above 判定）。对齐 Go：每帧据当前光标 + 内容尺寸重算
        // 位置（place_window 负责工作区钳制、上方底边参考、上翻粘滞），再用 4px 阈值抑制微移——
        // 从而 (1) 输入缓冲变化致尺寸变化时按需重定位、不溢出屏幕；(2) 上方显示以底边贴光标为参考；
        // (3) 已显示后宿主 caret 微抖动(<4px)不跳，位置保护；显著变化(换行/reflow 修正)才跟随；
        // (4) 一旦上翻则粘滞上方。新组合首显已由协调器延迟到 reflow 后的权威坐标，故首帧即落在正确处。
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
        // 基线采样刻意排在 `show` **之前**：窗口出现不会移动鼠标，两处取值物理上相同，
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
        // 固定位置模式按**固定点**取——固定位置可能落在与光标不同缩放的另一块屏上，
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
        let (px, py, absolute) = match self.fixed_pos {
            Some(f) => {
                // 固定位置下窗口不随光标上下移动，"上翻"随之失去意义：placed_above 必须归 false，
                // 否则 flip_when_above / swap_preedit_when_above 会让内容在一个不动的窗口里倒序。
                self.placed_above = false;
                self.last_content_pos = None;
                // 本路径坐标是逻辑点，尺寸与扩边是设备像素：先换成逻辑点再交给 place_fixed。
                // 扩边来自皮肤装饰（macOS 不画软阴影）；place_fixed 返回**窗口**左上，加回扩边
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
                (wx + lg(ml), wy + lg(mt), true)
            }
            None => {
                // place_window 约定 caret_y 为光标【底端】（下方锚点=底端+gap），但 macOS .app 的
                // caretRectToWire 发的是光标行【顶端】(top-left)，故这里把行高补上传光标底端 =
                // self.y + self.caret_height，候选窗才落在光标行下方、不遮挡输入。
                let offset = self.position_offset_px();
                let (px0, py0, above) = Self::place_window(
                    self.x,
                    self.y + self.caret_height,
                    self.caret_height,
                    content_w,
                    content_h,
                    self.placed_above,
                    offset,
                    &self
                        .scene
                        .skin_place_anchor(1.0 / self.scale.max(1.0), self.scale),
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
                (px, py, false)
            }
        };
        self.report_flip_state();
        self.scene
            .relayout_above(&self.scene_input(self.placed_above), &mut frame);

        // 命中矩形（窗口缓冲坐标，内容起点 (ml,mt)），同步给鼠标处理器。
        self.sync_hits(&frame);

        // 绘制到独立 BGRA 缓冲（透明清屏 → 内容；本路径不画软影）。
        let mut buf = vec![0u8; (width * height * 4) as usize];
        self.scene
            .paint(&frame, &mut buf, &self.text_renderer, false);
        let shadow = frame.shadow;

        self.mark_visible();
        // 关键单位换算：ml/mt/blur 都是 device px（含 scale），而 screen_x/y 是 .app 使用的【逻辑点】
        // （图像按 scale 显示）。故补偿前必须 /scale 换回逻辑点，否则 Retina(×2) 下会多减一截 →
        // 候选窗偏左、偏上盖住 caret（正是截图现象）。no-shadow(margin=0)→0，无变化。
        let scale = self.scale.max(1.0);
        let ml_l = (ml as f32 / scale).round() as i32;
        let mt_l = (mt as f32 / scale).round() as i32;
        // 软阴影上边距是完整 3σ 高斯尾(多为透明)；内容紧贴 caret 时上方约一个 blur 高的【可见浓阴影】
        // 会盖住 caret。故内容较锚点再下移「可见浓阴影」高度(≈blur，换算逻辑点)，使浓阴影恰落 caret
        // 下方、透明尾不碍观感。above(上翻)不额外下移；固定位置分支 placed_above 恒为 false，
        // 但那条路上窗口本就不贴 caret，阴影补偿有没有都不影响观感。
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
        })
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

        // DPI 探测点：固定位置模式按**固定点**取缩放，而不是光标点——固定位置可能落在
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
        // 兜底。**只在它真的生效时才记日志**——那意味着预算算漏了行内某个成员（这正是横排内联
        // 编码/徽标/翻页栏曾被漏算的形态），是需要看见的异常，不是每帧噪音。
        // ⚠️ 只有本路径记这条（三份管线合并前就如此，非 Windows 两条路径从来不记），保持原样。
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
        // 穿透装饰：只在本地窗口路径（show 置位 split_pass_decor）拆到独立的穿透窗口；
        // host-render 等路径照旧画进主缓冲（按 block 处理）。穿透窗口建不出来时也不拆。
        let split = self.split_pass_decor && !self.pass_window_failed;
        self.pass_layer = self
            .scene
            .paint(&frame, &mut buf, &self.text_renderer, split);
        let shadow = frame.shadow;

        self.mark_visible();
        // 上方三级定位分支穷尽，此处必为 Some；debug 下断言，release 兜底到光标处而非
        // (0,0)——真出现逻辑漏洞时窗口至少还在光标附近，不会莫名飞到屏幕左上角。
        debug_assert!(screen_xy.is_some(), "定位三分支必有其一赋值 screen_xy");
        let (screen_x, screen_y) = screen_xy.unwrap_or((self.x, self.y));
        // 鼠标层记录阴影扩边，两处要用：
        // 1. 拖动落定时把窗口左上换算回**内容左上**再上报落盘，否则每次「拖动→保存→
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

    /// Linux 外部宿主：渲染当前悬停 tooltip（位图 + 落位规则）。`(wx, wy)` 为候选窗**建议**
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

    /// **内容**左上 → **窗口**左上（减去软阴影扩边）。
    ///
    /// 落盘的 custom_x/y 记的是内容左上（用户视觉上看到的窗口边缘），而 Win32 定位用的是
    /// 窗口左上。本函数与 `window_to_content` 必须严格互逆，否则每轮
    /// 「拖动 → 落盘 → 重新显示」都会多减一次阴影，候选窗逐次向左上漂移。
    ///
    /// 纯算术、无平台依赖，故【不加 cfg 门控】：调用方 `content_origin` 是未门控的 pub fn，
    /// 若把本对函数限定为 windows-only，非 Windows 目标就会 E0599 找不到函数——这类不对称
    /// 在 Windows 本地开发时永远显现不出来，只有 CI 的 macOS 目标才炸。
    /// Linux（既非 windows 也非 macos，仅供跑测试）下 `place_fixed` 无调用者，故显式
    /// allow(dead_code)：它与 `window_to_content` 共用互逆契约和 round-trip 测试，必须成对存在。
    #[cfg_attr(not(any(windows, ext_presenter)), allow(dead_code))]
    fn content_to_window(content: (i32, i32), ml: u32, mt: u32) -> (i32, i32) {
        (content.0 - ml as i32, content.1 - mt as i32)
    }

    /// **窗口**左上 → **内容**左上（加回软阴影扩边）。`content_to_window` 的逆。
    fn window_to_content(window: (i32, i32), ml: i32, mt: i32) -> (i32, i32) {
        (window.0 + ml, window.1 + mt)
    }

    /// 固定位置模式的窗口落点（返回窗口左上屏幕坐标，含阴影扩边）。
    ///
    /// - `fixed` 是**内容**左上坐标，减去阴影扩边 `(ml, mt)` 才是窗口左上。
    /// - `(0, 0)` 视作"已开启固定但尚未设定位置"（用户还没拖过），落到默认锚点。
    /// - 钳制按**内容**矩形、且钳到整块屏幕（含任务栏）：`custom_x/y` 是绝对屏幕坐标，
    ///   用户换分辨率或拔掉副屏后会指向不可见区域，候选窗就此"消失"且无法用鼠标拖回来；
    ///   但按含阴影的窗口矩形去钳会让内容离屏幕边还有一整个阴影宽就被拦下，见
    ///   [`clamp_content_to_monitor`]。
    ///
    /// macOS 同样走这里（`.app` 的 NSPanel 只是照搬算好的坐标）：那边不画软阴影，扩边恒 0，
    /// 且 `clamp_content_to_monitor` 在非 Windows 是恒等函数——真正的屏幕钳制由 `.app` 用
    /// `NSScreen.visibleFrame` 做，服务进程这边查不到 macOS 的显示器几何。
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

    /// 固定模式尚未设定位置时的默认锚点（**内容**左上屏幕坐标）。
    ///
    /// 取**光标所在**显示器的工作区（用户在哪块屏打字就在哪块屏出现），水平居中、
    /// 垂直落在约 3/4 高度处：贴底会和任务栏/其他浮动 UI 挤在一起，居中则挡住正文。
    /// 内容高度不参与——顶端定在 3/4 处即可，超出底部由调用方的钳制拉回。
    ///
    /// 这里用 `rcWork` 而非 `rcMonitor`：自动落位应避开任务栏，只有用户**手动拖动**
    /// 才允许压到任务栏上。
    ///
    /// 非 Windows 退化为「就落在光标处」：服务进程查不到 macOS 的显示器几何（`.app` 才有
    /// `NSScreen`），而这只影响「刚打开固定位置、还没拖过」这一瞬——窗口出现在光标旁，
    /// 用户拖一次就定下来了。与其为此加一轮 IPC 往返，不如让首帧落在最不意外的地方。
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

    /// 据冻结光标锚点与当前内容尺寸计算候选窗位置，钳制在光标所在显示器工作区内。
    /// 规则：
    /// - 默认显示在光标下方（+gap）；下方空间不足则上翻到光标上方。
    /// - 上方显示以"窗口底边贴近光标顶端"为参考：`above_y = caret_top - h - gap`，
    ///   底边 = caret_top - gap 与高度无关 → 候选变少（h 减小）时顶边下移、底边不动，不会离光标变远。
    /// - `sticky_above`=true（当前已在上方）时优先保持上方，仅当上方也放不下才回落下方，
    ///   避免候选数量变化时上下抖动（需求 4）。
    /// - 左右溢出则贴边（横向右方空间不足时允许左移，属位置保护的例外）。
    ///
    /// 返回 (x, y, above)：above=true 表示窗口被上翻到光标上方（供 flip_when_above 判定）。
    ///
    /// 仅「跟随光标」定位方式走这里；固定位置见 [`Self::place_fixed`]。
    // 非 Windows 下窗口钳制为空实现，caret_h/w/h 及 x/y 的可变性仅 Windows 分支需要。
    #[cfg_attr(not(windows), allow(unused_variables, unused_mut))]
    /// 候选窗相对光标的**锚点**计算（纯函数，不含屏幕钳制）：返回 `(x, below_y, above_y)`。
    ///
    /// `caret_y` 为光标**底端**。下方锚点 = 底端 + gap；上方锚点以窗口底边贴光标顶端为参考。
    /// 主题偏移 `off_y` 语义恒为「远离光标」：下方 **+**、上方 **−**。写成同号会让上翻时
    /// 偏移把窗口压向光标，与配置意图相反——这是本函数存在的唯一理由（抽出来才测得到，
    /// `place_window` 余下部分是 `#[cfg(windows)]` 的屏幕钳制，依赖真实显示器）。
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

    /// 按最终方位施加主题 Y 偏移：`off_y` 正值**恒为远离光标**——在下方就向下、
    /// 在上方就向上。
    ///
    /// ⚠️ 必须在**方位已定之后**调用，不能把偏移预先加进锚点：
    /// `below_ok`/`above_ok` 是拿锚点跟工作区边界比出来的，锚点含偏移会让 off_y 越大
    /// 两个条件越难成立 —— 本该上翻的场景被判成「上方也放不下」，落回下方分支再被钳到
    /// `wa.bottom - hi`（贴屏幕底），窗口就压在光标上了。偏移只该改变距离，不该改变
    /// 「往上还是往下」这个决策。
    fn apply_offset_y(y: i32, above: bool, off_y: i32) -> i32 {
        if above { y - off_y } else { y + off_y }
    }

    /// 跟随光标定位。`offset` 为主题 `window.position_offset`（设备像素，已 ×scale）。
    ///
    /// ⚠️ 偏移**必须在此函数内、锚点计算处注入**，不能在调用方给返回值加：
    /// 下方那段 `rcWork` 越界兜底要在偏移之后跑，否则偏移能把窗口推出屏幕且再也钳不回来。
    /// 上方锚点用**减号**——`off_y` 正值语义恒为「远离光标」，上翻时是向上推。
    ///
    /// 翻转/钳制整段在 `cfg(windows)` 内（靠 `GetMonitorInfoW` 取显示器边界），非 Windows
    /// 直接返回下方锚点，于是那几个只服务于翻转的局部量在此平台无人读写。
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
                    // 垂直可用区：前台铺满**caret 所在这块屏**时任务栏不可见，按整屏算；
                    // 否则照旧避开任务栏。全屏游戏下仍按 rcWork 算就白白少掉一条任务栏的高度
                    // （实测 48px）：洛克王国 caret 底端 y=2024 时，rcMonitor 下方尚有 134px、
                    // rcWork 只剩 86px，高度落在其间的候选窗被误判"放不下"而上翻，反过来遮住
                    // 正在输入的那一行。
                    //
                    // ⚠ 判据（含 DWM cloaked / shell 进程两道守卫）收口在 wind-keys，别在此重写：
                    // 几何比较只是它的一半，那两道守卫才是它能用的原因。也**不能**改用
                    // `foreground_fullscreen_kind() == Covering`——那个问的是"前台窗口自己那块屏"，
                    // 而这里必须问 caret 所在那块屏：多屏下 A 屏的全屏游戏不会隐藏 B 屏的任务栏。
                    let wa = if wind_keys::foreground::foreground_covers_monitor(&mi.rcMonitor) {
                        mi.rcMonitor
                    } else {
                        mi.rcWork
                    };
                    // 可行性判定用**净锚点**（不含主题偏移）：偏移只该改变距离，
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
                    // 方位定后再施加偏移：上方向上、下方向下，均为远离光标。
                    y = Self::apply_offset_y(
                        if above { above_y } else { below_y },
                        above,
                        offset.1,
                    );
                    if above {
                        x = ax - anchor.above.0;
                    }
                    let mask = if above {
                        anchor.avoid_above.as_ref()
                    } else {
                        anchor.avoid_below.as_ref()
                    };
                    y = Self::avoid_caret_line(y, above, caret_y, caret_h, mask);
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
    /// 供边缘带装饰的主题拉开候选窗与光标的观感距离。**只喂给 place_window**——
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
    /// **panic 掉整个 UI 线程**。故在此收口：越界截到末尾，落在字符中间则退到前一个边界。
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
    /// 消息循环据此安排唤醒。两个到期源：悬停激活闸门（`engage_at`，用户首次真实移动鼠标到
    /// 候选窗上时武装，到期后悬停才开始响应；激活之后悬停走 `on_message` 即时发出，不再需要
    /// 唤醒），以及右键压制「等菜单」阶段的兜底超时（见 [`TipHold::AwaitMenu`]）；另有隐藏后释放
    /// 像素缓冲与皮肤装饰换帧。
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
            self.anim_due.then(Instant::now),
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
    /// 窗口左上 + (left, top) = **内容**左上，即落盘用的坐标系；四个分量一起用于
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

/// 气泡显示中、光标移到**另一个**候选的切换延迟。气泡在旁侧时，从候选 A 斜着去 A 的气泡
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
/// 不压的话气泡会盖在菜单上：① 右键时气泡已显示，它不会自己消失；② 右键时激活延迟还没
/// 到期，到期后照发 `Hover`，气泡在菜单弹出**之后**冒出来、叠在菜单上面（z 序后来者居上）。
///
/// 做成阶段而不是一个布尔：右键到菜单可见之间，「菜单开着」还是 false，只靠它挡闸门的话，
/// 这段来回里鼠标动 1px 就会重新武装闸门，到期时菜单已弹出，气泡照样叠上去；
/// `ui.tooltip.delay=0` 时气泡甚至抢在菜单之前出来。
///
/// 只压气泡、不清悬停：悬停高亮还指着右键的那个候选，是「菜单作用于谁」的唯一提示。
/// 清了（发 `Hover(-1)`）高亮会跳回键盘选中项，看上去像是菜单对着别的候选。
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
    /// 解除压制时拿它而不是 `last_hover` 比：压制期间候选若重绘过（协调器重置悬停），
    /// `last_hover` 还是旧值，与光标下的目标相等就不发 `Hover`，而 UI 画的已经是 -1，
    /// 气泡便再也不回来。
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

    /// 重置悬停状态：清空闸门与去重值，并**以当前物理光标位重建基线**。
    ///
    /// 两个调用点，职责不同，缺一不可：
    /// - [`CandidateWindow::mark_visible`]（不可见 → 可见）：**基线在这里才有意义**。判据问的是
    ///   「窗口出现之后鼠标动没动」，基准就必须取自窗口出现那一刻，见该函数的说明。
    /// - [`CandidateWindow::hide`]：清掉闸门与残留悬停，使下一轮从未激活态起步。它顺带采的
    ///   那次基线到下次显示时多半已经过时（用户在这期间移动了鼠标），**不能当作基线的来源**。
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
                        // 上报**内容左上**（窗口左上 + 阴影扩边）：固定位置模式下协调器把它
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
                // 按**内容**矩形钳制、且钳到整块屏幕：软阴影可溢出屏幕，候选窗也允许
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

    /// 展开态宽度跟着当前可见内容走：滚到窄的一屏，窗口缩回（GH#256）。
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
    /// 宽高下限只灌进与 `vertical` 相符的那一项（另一方向留 0），顺带白嫖一份「读错方向」的
    /// 回归覆盖：本模块几乎每条测试都会在 `min_window_w_px`/`min_window_h_px` 误读另一方向
    /// 的字段（恒为 0）时失效下限而变红。
    ///
    /// `scale` 钉成 1.0 使 dp == 设备像素——断言里的数值才有确定含义（本机 DPI 会漂）。
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

    // ────────────────── 只有模式徽标时的提示窗宽度（GH#127） ──────────────────

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

    /// ★ GH#127 的**另一半**：那 12px 右留白在**宽度预算**段也得跟着条件化。
    ///
    /// 装配段（`trailing`）无候选时给 0，预算段却一直无条件加 `12 * s`——两边分叉的后果
    /// 不是多出空白（方向反了，是少算可用宽），而是**超长编码提前一截被截断**：实测同一帧
    /// 下窗宽 2988.4 而上限 3000，白白空掉 10.8px。只修装配段就只修好一半。
    ///
    /// 判据取「剩余空隙小于一个字符宽」而不是「宽度 == 上限」：截断按整字符走，量出来
    /// 总比预算少小半个字。带 bug 时空隙 11.6px > 一个字宽（14×0.6=8.4），故这条会红。
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

        // 前置①：这一帧必须**已经被上限夹住**（再加长一倍宽度不变），否则下面的判据恒真。
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

    /// ★ GH#127：提示窗该紧贴文字，除了窗口自己的内边距不该再多出任何固定量。
    ///
    /// 修复前恒多 40.8px：无候选占位行把 `item_pad` 左右（10+8）连同那个撑高用的空格
    /// （约 0.6 字宽）算进了行宽，模式徽标又带着「与后面候选拉开距离」的 12px 右留白 ——
    /// 而这一帧后面根本没有候选。用户原话是「提示框右侧还有好多空白」。
    ///
    /// 判据写成等式而不是「差值一致」：多出来的是**固定量**，比较两个不同长度的徽标
    /// 得到的差值在修复前后完全一样，那样的测试一个字都测不到。
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

    /// 占位行的本职不能丢：提示窗的**高度**仍须与「有一条候选」时一致。
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

    /// 用户显式配了候选窗最小宽度时，仍以配置为准（紧贴文字只是**没配**时的默认）。
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

    /// ★ 核心判据：补出的占位行必须与真实候选行**完全等高**。
    ///
    /// 「补了行、窗口变高了」是自动成立的废话；只有「3 条候选 + 2 空行 ≡ 5 条真实候选」
    /// 才锁得住占位行装配不全（漏序号节点、漏 item.margin）导致的「矮一截」——那种缺陷
    /// 的表现是窗口仍在抖、只是抖得小一点，肉眼很难认定为 bug。
    #[test]
    fn padded_rows_equal_real_rows_in_height() {
        let full = measured(&win(true, 5, FIVE)).1;
        let padded = measured(&win(true, 5, THREE)).1;
        assert_eq!(padded, full, "3 候选补 2 空行后须与 5 条真实候选等高");
    }

    /// 占位行的文本属性必须逐项镜像真实候选行。
    ///
    /// ★ 这条测的是等高性测试**测不到**的那一半：mock 文本后端只按字号估算、不区分字族与
    /// 字重，占位行漏掉 `font_family` 时 `padded_rows_equal_real_rows_in_height` 照样全绿；
    /// 而真实 DirectWrite 的行高取自该字族的 line metrics（宋体约 1.17em、雅黑约 1.33em），
    /// 漏配就会让占位行矮一截。测量断言在此无能为力，只能从结构上比对。
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

    /// ★★ 本次重构的核心判据：下限量的是**窗口**，候选自身的宽度与位置一概不动。
    ///
    /// 旧实现（`min_width_chars_*`）把下限打在每个候选行容器的 `min_w` 上，横排时每格都被
    /// 撑宽、格间距成倍放大——用户要的是「窗口大小不变」，不是「每个候选都变宽」。故这里
    /// 逐条比对候选的命中矩形：配下限前后 x/w 必须逐位相等，多出来的宽度只能留在右侧空着。
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

    /// ★ 竖排例外：候选行本就 `fill_cross`（高亮宽度统一），窗口被撑宽后高亮须跟着铺满，
    /// 否则高亮只占左边一截、右侧空一块，观感比抖动更糟。
    ///
    /// 与上一条不矛盾：铺满的是**行背景**，行内的序号/文字/注释仍按内容左对齐。
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
    /// 真机实测出来的缺口：注释模板含拆字/编码/注音，宽度变化比候选文字本身还大。
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

    /// ★ 横排下限与竖排下限互不影响——两种排布的可用横向空间差一个数量级，共用一个值时
    /// 调宽了竖排就顶得横排太松，调紧了横排就不够竖排用。
    ///
    /// 只灌一侧字段（另一侧留 0）后按**另一个**方向渲染：若 `min_window_w_px()` 读错了方向
    /// （例如恒读 horizontal），这里会把本该跟随内容的渲染错锁成定宽。
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

    /// ★★ 高度下限存在的理由：`min_rows` 只数候选行，**翻页栏**的出现/消失它管不着
    /// （用户真机反馈的正是这一处），窗口总高照样跳。窗口级下限把它一并罩住。
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

    /// ★★ 窗口翻到光标上方时，高度富余必须补在**顶部**。
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
/// 这个开关的诉求只有一句：**竖排时别让翻页栏白占一行**。但编码有两种形态，落点因此有两处：
/// 独立编码栏（`candidate_top`）并进栏行右端，内联编码（`candidate_inline`）并进编码自己那一行。
/// 本组钉的是「两种形态表现一致」，以及三条不该被波及的旁路（横排 / 旋转态 / 开关关闭）。
///
/// 断言一律是相对量（谁比谁矮、谁在谁上面、有没有出右缘），与文本后端量出多宽无关，
/// 故不 gate 平台 —— 与 [`width_budget_tests`] 同样的理由。
#[cfg(test)]
mod pager_inline_tests {
    use super::*;
    use crate::view::Rect;

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

    /// 造一个多页候选窗。`embedded` = 内联编码（`candidate_inline`），false 即独立编码栏。
    ///
    /// `rotated`（蒙文竖排）与 `vertical` 互斥、不能同时为真（见 `set_orientation` 的断言），
    /// 故这里传 `rotated=true` 时 `vertical` 一律置 false —— 两者在布局里殊途同归，
    /// 都让 `list_vertical` 成立。
    fn win(
        vertical: bool,
        rotated: bool,
        embedded: bool,
        on: bool,
        preedit: &str,
    ) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(vertical && !rotated, rotated, false);
        w.set_preedit_embedded(embedded);
        w.set_pager_in_preedit(on);
        let items: Vec<CandidateItem> = ["一", "二", "三"].iter().map(|t| cand(t)).collect();
        // total_pages > 1 才有翻页栏；preedit 非空才谈得上「并入编码行」。
        w.update(preedit, preedit.len(), "", items, 0, -1, 1, 3);
        w
    }

    fn laid(w: &CandidateWindow, above: bool) -> View {
        let mut root = w.scene.build_tree(&w.scene_input(above));
        root.layout(0.0, 0.0, &w.text_renderer);
        root
    }

    fn height(w: &CandidateWindow) -> f32 {
        laid(w, false).measured_size().1
    }

    /// 递归收集树上所有叶子文本。
    fn texts(v: &View) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(t) = &v.text {
            out.push(t.clone());
        }
        for c in &v.children {
            out.extend(texts(c));
        }
        out
    }

    /// 主题写的翻页字符要真的画到树上。
    ///
    /// `arrow_char` 那条纯函数测试挡不住这个 —— 把装配处的 `prev_txt`/`next_txt` 改回字面量
    /// `"‹"`/`"›"`，它照样全绿。而「字段一路解析得好好的、渲染层不消费」正是本次修的 bug
    /// 本体，测试若也绕开消费点，等于把同一个坑再踩一遍。故这条从 `set_theme` 出发，
    /// 一直断言到布局后的树上。
    #[test]
    fn theme_arrow_chars_reach_the_view_tree() {
        let mut w = win(false, false, false, false, "");
        let mut t = wind_theme::Resolved::default();
        t.views.footer_bar.prev_char = "$".into();
        t.views.footer_bar.next_char = ")".into();
        w.set_theme(t);
        let found = texts(&laid(&w, false));
        assert!(
            found.iter().any(|s| s == "$"),
            "上一页应画主题字符, 实得 {found:?}"
        );
        assert!(
            found.iter().any(|s| s == ")"),
            "下一页应画主题字符, 实得 {found:?}"
        );
        assert!(
            !found.iter().any(|s| s == "‹" || s == "›"),
            "内置箭头不该再出现, 实得 {found:?}"
        );
    }

    /// 主题给 footer_bar 配的禁用态文字色，要真的画到禁用那一侧的箭头上。
    ///
    /// 判据取「首页时上一页与下一页**不同色**」这种相对量：绝对色值要跟着回退链（未配时
    /// 回退全局 text_hint）走, 写死就成了另一份需要同步的副本。resolve 那侧另有一条
    /// footer_bar_disabled_state_survives_resolve 钉住「patch 建得出来」, 这条钉住「渲染
    /// 真的读它」—— 两段都断过一次。
    #[test]
    fn theme_disabled_arrow_color_reaches_the_view_tree() {
        let leaf_colors = |ch_prev: &str, ch_next: &str, disabled: Option<[u8; 4]>| {
            let mut w = win(false, false, false, false, "");
            let mut t = wind_theme::Resolved::default();
            t.views.footer_bar.prev_char = ch_prev.into();
            t.views.footer_bar.next_char = ch_next.into();
            if let Some(c) = disabled {
                t.views.footer_bar.disabled = Some(Box::new(wind_theme::RvNode {
                    text_color: Some(c),
                    ..Default::default()
                }));
            }
            w.set_theme(t);
            let mut found = Vec::new();
            collect_colors(&laid(&w, false), &mut found);
            found
        };
        // 首页：上一页禁用、下一页可用 —— 两者取色分属禁用/启用两条路径。
        let red = [255, 0, 0, 255];
        let got = leaf_colors("<", ">", Some(red));
        let prev_c = got.iter().find(|(t, _)| t == "<").map(|(_, c)| *c);
        let next_c = got.iter().find(|(t, _)| t == ">").map(|(_, c)| *c);
        assert_eq!(
            prev_c,
            Some(red),
            "禁用侧应用主题配的禁用态色, 实得 {got:?}"
        );
        assert_ne!(next_c, Some(red), "可用侧不该被禁用色染上");

        // 未配禁用态：回退全局 text_hint，行为与从前一致（内置主题都没配）。
        let plain = leaf_colors("<", ">", None);
        let prev_plain = plain.iter().find(|(t, _)| t == "<").map(|(_, c)| *c);
        assert!(prev_plain.is_some() && prev_plain != Some(red));
    }

    /// 递归收集树上 (文本, 文字色)。
    fn collect_colors(v: &View, out: &mut Vec<(String, [u8; 4])>) {
        if let Some(t) = &v.text {
            out.push((t.clone(), v.text_color));
        }
        for c in &v.children {
            collect_colors(c, out);
        }
    }

    /// 多字翻页符号要把按钮（连同命中区）撑宽，而不是盖到页码上或被裁掉。
    ///
    /// 宽度基线 `arrow_w` 是按一个字算的，schema 却不限 prev_char 的长度。判据用相对量
    /// （多字宽 > 单字宽），与本组头注要求一致 —— 三个平台的文本后端量出的绝对宽度各不相同。
    #[test]
    fn multi_char_arrow_widens_the_hit_box() {
        let hit_w = |ch: &str| {
            let mut w = win(false, false, false, false, "");
            let mut t = wind_theme::Resolved::default();
            t.views.footer_bar.next_char = ch.into();
            w.set_theme(t);
            hit(&laid(&w, false), TAG_PAGE_NEXT)
                .expect("下一页可点（首页 → 有下一页）")
                .w
        };
        let one = hit_w(")");
        let many = hit_w("下一页");
        assert!(
            many > one,
            "多字应撑宽按钮: 单字 {one}, 多字 {many}（fixed_w 下两者相等）"
        );
    }

    /// 多字箭头下，同一行的编码预算要跟着收窄 —— 否则超长编码把翻页栏顶出窗口右缘。
    ///
    /// 命中盒那条测试盖不住这个：按钮撑宽了、预算却按单字算，两者不一致恰恰是「箭头宽度
    /// 公式写了两份」的后果。判据沿用同组既有那条（翻页栏右缘须落在窗口内），因为那正是
    /// 预算算漏时真实会坏掉的地方；`pager_row_w` 的注释也明写着这条因果。
    #[test]
    fn multi_char_arrow_shrinks_the_preedit_budget() {
        let themed = |ch: &str, preedit: &str| {
            let mut w = win(true, false, true, true, preedit);
            let mut t = wind_theme::Resolved::default();
            t.views.footer_bar.next_char = ch.into();
            t.views.footer_bar.prev_char = ch.into();
            w.set_theme(t);
            w
        };
        // 前置：编码必须已被宽度上限夹住, 否则下面的不等式恒真（同组头注反复警告的写法）。
        let long = clamped_preedit(|p| themed("下一页", p));
        let w1 = laid(&themed("下一页", &long), false).measured_size().0;
        let w2 = laid(&themed("下一页", &long.repeat(2)), false)
            .measured_size()
            .0;
        assert_eq!(w1, w2, "前置：超长编码须已被上限夹住（{w1} vs {w2}）");

        let w = themed("下一页", &long);
        let cap = w.screen_safety_max_width_px() as f32;
        let root = laid(&w, false);
        let (win_w, _) = root.measured_size();
        // 判据必须是安全上限而不是「翻页栏落在 win_w 内」：预算算漏时整行照样排得下,
        // 只是 win_w 自己跟着涨 —— 那个不等式恒真, 测了等于没测。真正坏掉的地方是渲染入口
        // 那道 content_w.min(safety_max_w), 它是**裁切**语义, 超出部分在右缘直接被切掉。
        assert!(
            win_w <= cap + 0.5,
            "多字箭头下整窗仍不得越过安全上限（{win_w} vs {cap}）—— 越界即预算按单字算漏了"
        );
        let p = pager(&root);
        assert!(
            p.x + p.w <= win_w + 0.5,
            "翻页栏须完整落在窗口内（右缘 {} vs 窗口宽 {win_w}）",
            p.x + p.w
        );
    }

    /// 未配字符时回退内置 ‹ › —— 与上一条互为反面, 省得「让主题字符生效」被实现成
    /// 「无论如何都不画内置箭头」。
    #[test]
    fn builtin_arrows_remain_when_theme_says_nothing() {
        let w = win(false, false, false, false, "");
        let found = texts(&laid(&w, false));
        assert!(
            found.iter().any(|s| s == "‹"),
            "未配字符应回退内置, 实得 {found:?}"
        );
        assert!(
            found.iter().any(|s| s == "›"),
            "未配字符应回退内置, 实得 {found:?}"
        );
    }

    /// 取某个 tag 的命中矩形；`None` = 该 tag 未参与命中（如翻页箭头被禁用或整个翻页栏没渲染）。
    fn hit(root: &View, tag: i32) -> Option<Rect> {
        let mut hits = Vec::new();
        root.collect_hits(&mut hits);
        hits.iter().find(|(t, _)| *t == tag).map(|(_, r)| *r)
    }

    fn pager(root: &View) -> Rect {
        hit(root, TAG_PAGE_NEXT).expect("翻页栏须存在（多页 + 默认主题下可见）")
    }

    /// 首条候选的矩形（tag = 候选下标）。
    fn first_cand(root: &View) -> Rect {
        hit(root, 0).expect("首条候选须参与命中")
    }

    /// 造一段**确实已被宽度上限夹住**的超长编码：从 200 组 `a'` 起翻倍，直到「再翻一倍
    /// 也不再变宽」为止，返回夹住的那一档。`mk` 是被测那一档的构造器 —— 必须与随后写
    /// 前置断言时用的是同一个，否则夹住的是另一棵树。
    ///
    /// ⚠️ **重复次数不能写死。** 本组头注说「断言一律是相对量，与文本后端量出多宽无关」，
    ///    可「超长编码须已被宽度上限夹住」这条**前置**恰恰是绝对量 —— 它成不成立取决于
    ///    一个字有多宽，而三个平台的后端各不相同（Windows: DirectWrite；macOS: CoreText；
    ///    Linux: 等宽 mock，见 `text/dwrite.rs` 末尾）。实测同样 200 组 `a'`：Windows 下
    ///    已顶到 3000 的上限，macOS 下只有 2713.45，于是这三条**只在 macOS CI 上红**，
    ///    Windows 与 Linux(mock) 全绿 —— 本机怎么跑都发现不了。
    ///    判据要跟着字宽走，而不是把次数往上调一档、等下一个后端再把它顶穿。
    ///
    /// ★ `> floor` 那半条判据不能省。「加长一倍不再变宽」在**两端**都成立：
    ///   一端是这里要的「顶到宽度上限」，另一端是「短得连窗口最小宽度都没撑满」——
    ///   实测 mock 后端下 2 组 `a'` 与 4 组同为 116.4，正是后者。少了这半条，探测会在
    ///   第一档就「收敛」并交回一段根本没被夹住的短编码，于是三条测试里那些
    ///   「不得越过上限」的不等式**全部恒真** —— 绿得毫无意义，正是本组头注反复警告的
    ///   那种写法。用空编码那档的树宽当 floor，两个平台期差着两个数量级，分得很开。
    fn clamped_preedit(mk: impl Fn(&str) -> CandidateWindow) -> String {
        let floor = laid(&mk(""), false).measured_size().0;
        let mut n = 200usize;
        for _ in 0..6 {
            let one = "a'".repeat(n);
            let w1 = laid(&mk(&one), false).measured_size().0;
            let w2 = laid(&mk(&"a'".repeat(n * 2)), false).measured_size().0;
            if w1 == w2 && w1 > floor {
                return one;
            }
            n *= 2;
        }
        panic!("加到 {n} 组 `a'` 仍未被宽度上限夹住 —— 上限逻辑或文本测量变了，先查那边");
    }

    /// ★ 本次修复的核心判据：竖排下**两种编码形态的表现必须一致**。
    ///
    /// 修复前内联编码那档完全不理会开关（翻页栏照旧独占底部一行），用户看到的就是
    /// 「同一个开关，换个编码显示方式就失灵」。
    ///
    /// 前置断言不可省：若独立编码栏那档本身就没省下这一行，后面两个相等就成了恒等式。
    #[test]
    fn inline_preedit_joins_pager_like_top_bar() {
        let top_off = height(&win(true, false, false, false, "nihao"));
        let top_on = height(&win(true, false, false, true, "nihao"));
        let inline_off = height(&win(true, false, true, false, "nihao"));
        let inline_on = height(&win(true, false, true, true, "nihao"));
        assert!(
            top_on < top_off,
            "前置：独立编码栏下开关须真的省掉一行（{top_on} vs {top_off}）"
        );
        assert_eq!(
            inline_off, top_off,
            "开关关闭时两种编码形态本就同高（都是底部独立行）"
        );
        assert_eq!(
            inline_on, top_on,
            "开关开启时内联编码须与独立编码栏省下同样的一行（{inline_on} vs {top_on}）"
        );
    }

    /// 并入后翻页栏须真的落在**编码那一行**里：整个矩形在首条候选之上。
    ///
    /// 只测高度不够——把翻页栏挂到窗口任意别处也能让总高少一行，位置才是这条开关的本意。
    #[test]
    fn inlined_pager_sits_above_first_candidate() {
        let w = win(true, false, true, true, "nihao");
        let root = laid(&w, false);
        let p = pager(&root);
        let c0 = first_cand(&root);
        assert!(
            p.y + p.h <= c0.y + 0.5,
            "翻页栏须整体位于首条候选之上（翻页栏底 {} vs 候选顶 {}）",
            p.y + p.h,
            c0.y
        );
    }

    /// ★ 开关关闭时翻页栏**不得消失**，仍是候选区下方的独立一行。
    ///
    /// 这条钉的是实现里的一个真实陷阱：并入分支若写成 `pager.take().filter(开关)`，
    /// 开关关闭时翻页栏会被取出来直接丢掉，末尾装配段再也拿不到它 —— 症状是「关掉开关翻页栏
    /// 就没了」，而开着的时候一切正常，最容易漏测的那一半。
    #[test]
    fn switch_off_keeps_pager_below_candidates() {
        let w = win(true, false, true, false, "nihao");
        let root = laid(&w, false);
        let p = pager(&root);
        let c0 = first_cand(&root);
        assert!(
            p.y > c0.y,
            "开关关闭时翻页栏须留在候选下方（翻页栏 y={} vs 首候选 y={}）",
            p.y,
            c0.y
        );
    }

    /// 横排不受影响：内联编码与候选本就同属一行，翻页栏本来就在该行末尾，开关无从谈起。
    #[test]
    fn horizontal_inline_is_untouched_by_switch() {
        let off = height(&win(false, false, true, false, "nihao"));
        let on = height(&win(false, false, true, true, "nihao"));
        assert_eq!(off, on, "横排内联编码下开关不得改变布局");
    }

    /// ⚠️ 旋转态（蒙文竖排）刻意排除在外：内联编码在旋转包裹层**内部**，翻页栏跟进去会连
    /// 箭头带页码一起转 90°。那一档维持底部独立行（它挂在 root 上，屏幕上仍是横的一条）。
    #[test]
    fn rotated_inline_keeps_bottom_pager() {
        let off = height(&win(false, true, true, false, "nihao"));
        let on = height(&win(false, true, true, true, "nihao"));
        assert_eq!(off, on, "旋转态下不得并入（否则翻页箭头会跟着转）");
        // 前置：同一组参数在非旋转竖排下开关是**有效**的，否则上面的相等只是「哪儿都没生效」。
        assert!(
            height(&win(true, false, true, true, "nihao"))
                < height(&win(true, false, true, false, "nihao")),
            "前置：非旋转竖排下开关须真的省掉一行"
        );
    }

    /// 编码沉底（`swap_preedit_when_above`，窗口翻到光标上方）时，并入的翻页栏须**跟着编码一起**
    /// 沉到底部 —— 它是编码那一行的子节点，本就该同进同退。
    #[test]
    fn inlined_pager_follows_preedit_to_bottom_on_swap() {
        let mut w = win(true, false, true, true, "nihao");
        w.set_swap_preedit_when_above(true);
        let root = laid(&w, true);
        let p = pager(&root);
        let c0 = first_cand(&root);
        assert!(
            p.y > c0.y,
            "沉底时翻页栏须随编码行落到候选之下（翻页栏 y={} vs 首候选 y={}）",
            p.y,
            c0.y
        );
    }

    /// 造一个**独立编码栏**（`candidate_top`）的窗口，可带模式徽标。
    ///
    /// 与 `win()` 的区别只有两条：`embedded = false`，以及 `mode_label` 可非空。
    /// 单列一个助手而不给 `win()` 再加参数——这两条测的是 band 那条落点，与内联那族无关。
    fn bar_win(label: &str, on: bool, preedit: &str) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(true, false, false);
        w.set_preedit_embedded(false);
        w.set_pager_in_preedit(on);
        let items: Vec<CandidateItem> = ["一", "二", "三"].iter().map(|t| cand(t)).collect();
        w.update(preedit, preedit.len(), label, items, 0, -1, 1, 3);
        w
    }

    /// 独立编码栏必须撑满窗口内容宽，与主题编辑器预览一致。
    ///
    /// 只有编码那么宽时，配了背景色的编码栏后半截露出窗口底（窗口透明就是透明的一段）。
    /// 取无徽标、无并入翻页栏的那档——那两者在时 band 本来就撑满，测不到这条。
    /// 默认主题没配 `separator`，即「第三方主题」那条路径。
    #[test]
    fn own_preedit_bar_spans_window_content_width() {
        let w = bar_win("", false, "a");
        let root = laid(&w, false);
        let band = &root.children[0];
        assert!(
            band.text.is_none() && !band.children.is_empty(),
            "前置：首个子节点应为编码栏"
        );
        let content_w = root.measured_size().0 - root.padding.l - root.padding.r;
        let band_w = band.laid_rect().w;
        assert!(
            band.children[0].measured_size().0 < content_w,
            "前置：编码本身须比窗口内容窄，否则恒真"
        );
        assert_eq!(
            band_w, content_w,
            "编码栏只有 {band_w} 宽，窗口内容宽 {content_w}"
        );
    }

    /// 编码栏下的分隔线：颜色 / 线粗 / 间距走 `views.divider`；`enabled=false` 不画。
    #[test]
    fn divider_node_controls_the_preedit_separator() {
        // 找到编码栏之后紧跟的那条细线（Row、定高、无文字无子节点、背景色 = sep）。
        let line_of = |root: &View, col: [u8; 4]| -> Option<(f32, f32)> {
            fn walk(v: &View, col: [u8; 4], out: &mut Option<(f32, f32)>) {
                if v.text.is_none() && v.children.is_empty() && v.bg == Some(col) {
                    *out = Some((v.laid_rect().h, v.margin.b));
                }
                for c in &v.children {
                    walk(c, col, out);
                }
            }
            let mut out = None;
            walk(root, col, &mut out);
            out
        };
        let col = [10, 20, 30, 255];
        let mut w = bar_win("", false, "a");
        let mut t = wind_theme::Resolved::default();
        t.views.divider.bg_color = Some(col);
        w.set_theme(t.clone());
        assert_eq!(
            line_of(&laid(&w, false), col),
            Some((1.0, 4.0)),
            "缺省 1px、下距 4"
        );

        t.views.divider_height = Some(wind_theme::schema::Dim::Dp(3.0));
        t.views.divider.margin.bottom = Some(wind_theme::schema::Dim::Dp(9.0));
        w.set_theme(t.clone());
        assert_eq!(
            line_of(&laid(&w, false), col),
            Some((3.0, 9.0)),
            "线粗与下距可配"
        );

        t.views.divider_enabled = Some(false);
        w.set_theme(t);
        assert_eq!(line_of(&laid(&w, false), col), None, "enabled=false 不画");
    }

    /// ★ F-17：**独立**编码栏那条落点同样要为并入的翻页栏留出宽度。
    ///
    /// 这是 55032bc0 给内联那条补的同一笔账的另一半，当时漏了。band 排的是
    /// `[编码][spacer][徽标][翻页栏]`，而 `preedit_bar_text_budget_px` 只扣了窗口内边距与
    /// 自身内边距，后两个成员一个都没进预算。
    ///
    /// ⚠️ 症状**不是**「窗口探出屏幕」：渲染入口那道 `content_w.min(safety_max_w)` 是**裁切**
    /// 语义、不回头改 `root`，所以窗口出不去，超出的部分在右缘直接被切掉且没有省略号——
    /// 翻页栏看不见或只剩半截箭头。也正因如此 `width_budget_tests` 那组抓不到它（它断的是
    /// 钳制**之后**的窗口宽，恒满足）。判据必须量 `laid()` 的**树宽**、与安全上限比。
    ///
    /// 实测（scale=1，200 组 `a'`，上限 3000）：修复前树宽 3091.6，修复后 2994.4。
    #[test]
    fn long_preedit_in_own_bar_reserves_room_for_inlined_pager() {
        let long = clamped_preedit(|p| bar_win("", true, p));
        let w = bar_win("", true, &long);
        let cap = w.screen_safety_max_width_px() as f32;
        // 前置①：这一行必须**已经被上限夹住**（再加长一倍不再变宽），否则不等式恒真。
        let root = laid(&w, false);
        let (win_w, _) = root.measured_size();
        assert_eq!(
            win_w,
            laid(&bar_win("", true, &long.repeat(2)), false)
                .measured_size()
                .0,
            "前置：超长编码须已被宽度上限夹住"
        );
        // 前置②：这一档确实并入了编码栏（翻页栏落在编码那一行，不是候选区底部）。
        let p = pager(&root);
        let c0 = first_cand(&root);
        assert!(
            p.y < c0.y,
            "前置：翻页栏须并在编码行（y={} 应在首候选 y={} 之上）",
            p.y,
            c0.y
        );

        assert!(
            win_w <= cap + 0.5,
            "整棵树不得越过安全上限（{win_w} vs {cap}）—— 越过的部分会被右缘裁掉"
        );
        assert!(
            p.x + p.w <= win_w + 0.5,
            "翻页栏须完整落在窗口内（右缘 {} vs 窗口宽 {win_w}）",
            p.x + p.w
        );
    }

    /// ★ F-17 的另一半：模式徽标同样占着 band 的右端，同样得进预算。
    ///
    /// 与上一条分开是因为两者**各自独立**地漏掉了——实测（同参数）只带徽标不开翻页栏时
    /// 树宽 3046.4，仍越上限 46.4；只有两项都扣掉，四种组合才全部落回上限内。
    #[test]
    fn long_preedit_in_own_bar_reserves_room_for_mode_chip() {
        let long = clamped_preedit(|p| bar_win("临时英文", false, p));
        let w = bar_win("临时英文", false, &long);
        let cap = w.screen_safety_max_width_px() as f32;
        let root = laid(&w, false);
        let (win_w, _) = root.measured_size();
        assert_eq!(
            win_w,
            laid(&bar_win("临时英文", false, &long.repeat(2)), false)
                .measured_size()
                .0,
            "前置：超长编码须已被宽度上限夹住"
        );
        // 前置：对照组（无徽标）必须**本来就没越界**，否则这条量到的是别的账。
        let plain = laid(&bar_win("", false, &long), false).measured_size().0;
        assert!(plain <= cap + 0.5, "前置：无徽标时本就不该越界（{plain}）");
        assert!(
            win_w <= cap + 0.5,
            "带模式徽标时整棵树仍不得越过安全上限（{win_w} vs {cap}）"
        );
    }

    /// ★ 超长编码不得把这一行撑出宽度上限：编码的文字预算要先扣掉翻页栏占的那一截。
    ///
    /// 竖排下内联编码原本独占整行预算（与候选互不竞争），并入之后这个前提不再成立。
    ///
    /// ⚠️ 参照物必须是**屏幕安全上限**，不能拿 `measured_size()` 当窗口宽去比翻页栏右缘 ——
    /// 树是自底向上量的，翻页栏当然落在自己撑出来的那个宽度里，那样写恒真（本条最初就是
    /// 这么写的，变异检验里「预算不扣翻页栏」一改，七条测试全绿，才发现它什么都没测）。
    #[test]
    fn long_preedit_does_not_push_row_past_width_budget() {
        let long = clamped_preedit(|p| win(true, false, true, false, p));
        let w_on = win(true, false, true, true, &long);
        let cap = w_on.screen_safety_max_width_px() as f32;
        // 前置：这一行必须**已经被上限夹住**（再加长一倍宽度不变），否则下面的不等式恒真。
        // 判据取「加长不再变宽」而不是「宽度 == 上限」：截断按整字符走，量出来总比预算少小半个字。
        let off_w = laid(&win(true, false, true, false, &long), false)
            .measured_size()
            .0;
        let off_w2 = laid(&win(true, false, true, false, &long.repeat(2)), false)
            .measured_size()
            .0;
        assert_eq!(
            off_w, off_w2,
            "前置：超长编码须已被宽度上限夹住（{off_w} vs {off_w2}）"
        );
        assert!(
            off_w <= cap + 0.5,
            "前置自洽：夹住后不该超过上限（{off_w} vs {cap}）"
        );
        let root = laid(&w_on, false);
        let (win_w, _) = root.measured_size();
        assert!(
            win_w <= cap + 0.5,
            "并入翻页栏后整窗仍不得越过安全上限（{win_w} vs {cap}）"
        );
        let p = pager(&root);
        assert!(
            p.x + p.w <= win_w + 0.5,
            "翻页栏须完整落在窗口内（右缘 {} vs 窗口宽 {win_w}）",
            p.x + p.w
        );
    }
}

/// 宽度上限：**整棵树**都不得超过屏幕安全上限。
///
/// ★ 与 [`min_size_tests`] 不同，本组**不 gate 平台**——断言的是不等式（行宽 ≤ 上限）而非
/// 具体像素值，与文本后端量出多宽无关，故真实 DirectWrite 与 mock 下同样成立。上限也不写死
/// 常量，直接取 `screen_safety_max_width_px()`。本组不走渲染入口，`screen_extent` 恒为 None，
/// 各平台都按兜底常量（3000dp）断言，不涉真实屏幕宽。
#[cfg(test)]
mod width_budget_tests {
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

    /// 造一个候选窗并布局，返回 (窗口, 内容宽)。scale 钉 1.0 使 dp == 设备像素。
    fn build(vertical: bool, texts: &[&str]) -> (CandidateWindow, View) {
        build_o(vertical, false, texts)
    }

    /// 同 [`build`]，可指定旋转位与直立位。
    fn build_o(vertical: bool, rotated: bool, texts: &[&str]) -> (CandidateWindow, View) {
        build_ou(vertical, rotated, false, texts)
    }

    /// 同 [`build_o`]，可指定「字直立」（对联式竖排）。
    fn build_ou(
        vertical: bool,
        rotated: bool,
        upright: bool,
        texts: &[&str],
    ) -> (CandidateWindow, View) {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(vertical, rotated, upright);
        let items: Vec<CandidateItem> = texts.iter().map(|t| cand(t)).collect();
        w.update("", 0, "", items, 0, -1, 1, 1);
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        (w, root)
    }

    /// 取全部候选的命中矩形，按 tag 升序（tag = 原始候选下标）。
    fn hits_by_tag(root: &View) -> Vec<Rect> {
        let mut out = Vec::new();
        root.collect_hits(&mut out);
        out.retain(|(t, _)| *t >= 0);
        out.sort_by_key(|(t, _)| *t);
        out.into_iter().map(|(_, r)| r).collect()
    }

    /// ★★ 横排多候选：**整行**宽度不得超过屏幕安全上限——与候选个数无关。
    ///
    /// 这是「先到先得」贪心分配翻车的直接回归：那种做法下第一个候选吃光预算，其后每个候选
    /// 仍各自加上「固定开销（item 内边距/序号/间隙）+ 文字下限」，累加必然突破上限，真机 9 个
    /// 候选实测溢出约 1100px。★ 逐个候选看都「没超自己的预算」，**只有按整行断言才测得出来**
    /// ——所以这里量的是 `build_tree` 产物的总宽，而不是任何单个候选的宽度。
    #[test]
    fn horizontal_row_never_exceeds_screen_budget_for_any_candidate_count() {
        let long = "超长候选".repeat(200); // 800 字，远超任何屏幕宽度
        for n in 1..=12usize {
            let texts: Vec<&str> = (0..n).map(|_| long.as_str()).collect();
            let (w, root) = build(false, &texts);
            let cap = w.screen_safety_max_width_px() as f32;
            let got = root.measured_size().0;
            assert!(
                got <= cap + 0.5,
                "{n} 个超长候选横排时整行宽 {got} 超出上限 {cap}"
            );
        }
    }

    /// 横排「一条长整句 + 若干短词」——最常见的形态：短词必须**原样显示不被误截**，长句吃掉
    /// 剩余空间。一刀切均分会把长句压到 1/n（明明还有大把空间），water-filling 才有这个性质。
    #[test]
    fn horizontal_short_candidates_survive_next_to_a_long_one() {
        let long = "长".repeat(500);
        let mut texts: Vec<&str> = vec![long.as_str()];
        texts.extend(["你好", "世界", "输入", "法"]);
        let (w, root) = build(false, &texts);
        assert!(
            root.measured_size().0 <= w.screen_safety_max_width_px() as f32 + 0.5,
            "整行仍不得超上限"
        );
        let mut in_tree = Vec::new();
        collect_texts(&root, &mut in_tree);
        for short in ["你好", "世界", "输入", "法"] {
            assert!(
                in_tree.iter().any(|t| t == short),
                "短候选 {short} 不该被截断，树内文本：{in_tree:?}"
            );
        }
    }

    /// 竖排：每候选独占一行、各自用满整行预算，行宽同样不得超上限。
    #[test]
    fn vertical_rows_never_exceed_screen_budget() {
        let long = "超长候选".repeat(200);
        let texts: Vec<&str> = (0..9).map(|_| long.as_str()).collect();
        let (w, root) = build(true, &texts);
        let cap = w.screen_safety_max_width_px() as f32;
        let got = root.measured_size().0;
        assert!(got <= cap + 0.5, "竖排宽 {got} 超出上限 {cap}");
    }

    /// 正常长度的候选完全不受影响：没超预算就不该动它（零回归）。
    #[test]
    fn normal_candidates_are_untouched() {
        for vertical in [false, true] {
            let (_, root) = build(vertical, &["你好", "世界", "输入法"]);
            let mut in_tree = Vec::new();
            collect_texts(&root, &mut in_tree);
            for t in ["你好", "世界", "输入法"] {
                assert!(
                    in_tree.iter().any(|x| x == t),
                    "vertical={vertical} 下正常候选 {t} 被改动了：{in_tree:?}"
                );
            }
        }
    }

    /// 递归收集树内所有文本叶子的内容。
    fn collect_texts(v: &View, out: &mut Vec<String>) {
        if let Some(t) = &v.text {
            out.push(t.clone());
        }
        for c in &v.children {
            collect_texts(c, out);
        }
    }

    /// 手动基准：量截断带来的 build_tree+layout 开销与测量缓存增长。
    /// `cargo test -p wind-ui --release --lib bench_truncation_cost -- --ignored --nocapture`
    ///
    /// 只在 Windows：要量的是**真实 DirectWrite** 的排版/哈希成本，mock 后端（等宽近似）量出来
    /// 的数字没有意义；`measure_cache_len` 也只有真实后端才有。
    #[cfg(windows)]
    #[test]
    #[ignore = "手动基准，不参与常规测试"]
    fn bench_truncation_cost() {
        use std::time::Instant;

        let short: Vec<&str> = vec![
            "你好",
            "世界",
            "输入法",
            "候选",
            "测试",
            "性能",
            "开销",
            "评估",
            "基准",
        ];
        let longs = "超长候选词条".repeat(140); // 840 字
        let long: Vec<&str> = (0..9).map(|_| longs.as_str()).collect();

        for (name, texts) in [("短候选(常态)", &short), ("超长候选(最坏)", &long)] {
            for vertical in [false, true] {
                let (tx, _rx) = std::sync::mpsc::channel();
                let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
                w.scale = 2.0;
                w.set_orientation(vertical, false, false);
                let items: Vec<CandidateItem> = texts.iter().map(|t| cand(t)).collect();
                w.update("ceshipinyin", 0, "", items, 0, -1, 1, 1);
                let cache0 = w.text_renderer.measure_cache_len();

                // 冷：首帧（所有测量都要真打 DirectWrite）
                let t0 = Instant::now();
                let mut root = w.scene.build_tree(&w.scene_input(false));
                root.layout(0.0, 0.0, &w.text_renderer);
                let cold = t0.elapsed();
                let cache1 = w.text_renderer.measure_cache_len();

                // 热：稳态（同样文本，测量走缓存）——真实打字时每帧都是这个路径
                let n = 50;
                let t1 = Instant::now();
                for _ in 0..n {
                    let mut r = w.scene.build_tree(&w.scene_input(false));
                    r.layout(0.0, 0.0, &w.text_renderer);
                }
                let warm = t1.elapsed() / n;
                let cache2 = w.text_renderer.measure_cache_len();

                println!(
                    "{name} {} | 冷={cold:?} 热={warm:?} | 缓存 {cache0}→{cache1}→{cache2}",
                    if vertical { "竖排" } else { "横排" }
                );
            }
        }

        // 拆解：新增开销的两个构成——(a) 预扫阶段的「已缓存」测量，(b) 二分探测串的构造+哈希。
        let (tx, _rx) = std::sync::mpsc::channel();
        let w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        let st = TextStyle::new(36.0);
        for (label, text) in [("短串(4字)", "输入法好"), ("长串(840字)", longs.as_str())]
        {
            w.text_renderer.measure(text, &st); // 预热入缓存
            let n = 2000;
            let t = Instant::now();
            for _ in 0..n {
                std::hint::black_box(w.text_renderer.measure(std::hint::black_box(text), &st));
            }
            println!("  已缓存 measure {label}: {:?}/次", t.elapsed() / n);
        }
        // 单次截断（含二分）的成本：预算取自然宽的一半，强制触发。
        let full = w.text_renderer.measure(longs.as_str(), &st).width;
        let n = 500;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(w.scene_input(false).truncate_text_for_width(
                std::hint::black_box(longs.as_str()),
                &st,
                full * 0.5,
            ));
        }
        println!(
            "  truncate_text_for_width(840字, 热): {:?}/次",
            t.elapsed() / n
        );
    }

    // ───────────────────── 旋转态接线（A：候选项旋转 90°）─────────────────────

    /// 旋转态的窗口尺寸相对**竖排**必须整个翻过来。
    ///
    /// ★ 对照必须选竖排而不是横排：竖排本身就是「窄而高」，拿横排比的话，
    /// 一个「旋转位被当成竖排、根本没套旋转包裹」的实现照样通过——
    /// 这正是本轮变异验证抓出来的第一条假绿。
    #[test]
    fn rotated_window_swaps_dimensions_against_vertical() {
        let texts = ["候选一二三四五六", "候选一二三四五六", "候选一二三四五六"];
        let (_, v_root) = build_o(true, false, &texts);
        let (_, r_root) = build_o(false, true, &texts);
        let (vw, vh) = v_root.measured_size();
        let (rw, rh) = r_root.measured_size();
        assert!(
            rw < vw,
            "旋转态的宽应远小于竖排（宽只由列数决定）: 旋转 {rw} vs 竖排 {vw}"
        );
        assert!(
            rh > vh,
            "旋转态的高应远大于竖排（文字长度落到高度上）: 旋转 {rh} vs 竖排 {vh}"
        );
    }

    /// ★★ 候选 1 必须落在**最左**。
    ///
    /// 局部列首经顺时针 90° 会落到屏幕最右，故列表在局部空间里是**逆序**排的。
    /// 少了那次逆序，候选顺序就整个左右颠倒——而画面看上去仍然「正常」，
    /// 只有对着编号读才发现是反的。
    #[test]
    fn rotated_puts_candidate_one_leftmost() {
        let (_, root) = build_o(false, true, &["甲", "乙", "丙"]);
        let hits = hits_by_tag(&root);
        assert_eq!(hits.len(), 3, "应收集到三个候选的命中矩形");
        assert!(
            hits[0].x < hits[1].x && hits[1].x < hits[2].x,
            "候选应自左向右为 1、2、3，实测 x = {:?}",
            hits.iter().map(|r| r.x).collect::<Vec<_>>()
        );
    }

    // ─────────────── 直立态（B：文字竖排／对联式）───────────────

    /// ★★ 直立态与旋转态的**唯一**几何差别：文字沿列前进的步长。
    ///
    /// 旋转态每个字占它的**前进宽度**，直立态每个字占它的**行高**。
    /// 断言写成「加字带来的高度增量」而不是「总高」，是为了把内外边距、序号、
    /// 固定开销全部约掉——那些两态完全一样，混在总高里会稀释掉真正的差异，
    /// 真实字体下（行高只比汉字前进宽多两成）就可能淹没到测不出。
    ///
    /// 用拉丁文字放大差距：前进宽约 0.5em、行高约 1.2em，两倍以上。
    #[test]
    fn upright_advances_by_line_height_rotated_by_glyph_width() {
        let step = |upright: bool| {
            let (_, one) = build_ou(false, true, upright, &["a"]);
            let (_, four) = build_ou(false, true, upright, &["aaaa"]);
            one.measured_size();
            four.measured_size().1 - one.measured_size().1
        };
        let (rot, upr) = (step(false), step(true));
        assert!(rot > 0.0, "对照失效：旋转态加字也该变高，实测 {rot}");
        assert!(
            upr > rot * 1.5,
            "直立态每字应按行高推进（远大于字宽）：直立 {upr} vs 旋转 {rot}"
        );
    }

    /// 直立态与旋转态**同一种排列**：候选 1 同样在最左。
    ///
    /// 少了这条，一个「直立时忘了跟着逆序」的实现只会让候选左右颠倒——画面照样正常。
    #[test]
    fn upright_puts_candidate_one_leftmost() {
        let (_, root) = build_ou(false, true, true, &["甲", "乙", "丙"]);
        let hits = hits_by_tag(&root);
        assert_eq!(hits.len(), 3, "应收集到三个候选的命中矩形");
        assert!(
            hits[0].x < hits[1].x && hits[1].x < hits[2].x,
            "候选应自左向右为 1、2、3，实测 x = {:?}",
            hits.iter().map(|r| r.x).collect::<Vec<_>>()
        );
    }

    /// ★ 序号**整块**扶正，不逐格切：两位数的序号横着读，占的仍是一格的高度。
    ///
    /// 判据落在**列高**上：整块扶正时序号沿列只占一个行高，与位数无关；
    /// 逐格切则两位数占两个行高，列会变高。宽度测不出来——两种实现下它都可能被
    /// 别的节点顶住。
    ///
    /// 对照必不可少：同一批候选里**多一个字**必须让列变高，否则这条断言恒真。
    #[test]
    fn upright_index_is_one_block_not_stacked_digits() {
        let build = |label: &str, text: &str| {
            let (tx, _rx) = std::sync::mpsc::channel();
            let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
            w.scale = 1.0;
            w.set_orientation(false, true, true);
            let mut item = cand(text);
            item.label = label.to_string();
            w.update("", 0, "", vec![item], 0, -1, 1, 1);
            let mut root = w.scene.build_tree(&w.scene_input(false));
            root.layout(0.0, 0.0, &w.text_renderer);
            root.measured_size().1
        };
        let one = build("1", "字");
        assert!(
            (build("88", "字") - one).abs() < 0.5,
            "两位序号让列变高了 ⇒ 序号被逐格竖着切了：{} vs {one}",
            build("88", "字")
        );
        assert!(
            build("1", "字字") > one + 0.5,
            "对照失效：多一个字也没让列变高，本用例测不出东西"
        );
    }

    /// ★★ 注释在直立态下也**逐格竖排**，拉丁编码不例外。
    ///
    /// 判据落在「加一段注释让列长了多少」上：旋转态注释躺着，每个拉丁字符只吃它的
    /// 前进宽（约 0.5em）；直立态每格站着，吃一个行高（约 1.2em）。两者若增量相同，
    /// 说明注释还躺着——而那正是真机反馈「看不懂」的成因：同一列里两种阅读方向。
    ///
    /// ⚠️ 断言写成**增量之比**而不是总高：内外边距、序号、固定开销两态完全一样，
    /// 混在总高里会稀释掉真正的差异。同 `upright_advances_by_line_height…` 的写法。
    #[test]
    fn upright_stacks_the_comment_too() {
        let col_h = |upright: bool, comment: &str| {
            let (tx, _rx) = std::sync::mpsc::channel();
            let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
            w.scale = 1.0;
            w.set_orientation(false, true, upright);
            let mut item = cand("甲");
            item.comment = comment.into();
            w.update("", 0, "", vec![item], 0, -1, 1, 1);
            let mut root = w.scene.build_tree(&w.scene_input(false));
            root.layout(0.0, 0.0, &w.text_renderer);
            root.measured_size().1
        };
        let rot = col_h(false, "abcd") - col_h(false, "");
        let upr = col_h(true, "abcd") - col_h(true, "");
        assert!(rot > 0.0, "对照失效：旋转态加注释也该变长，实测 {rot}");
        assert!(
            upr > rot * 1.5,
            "直立态的注释应按行高逐格推进：直立 {upr} vs 旋转 {rot}"
        );
    }

    /// 模式徽标与内联编码同样在旋转包裹层里，直立态一并逐格扶正。
    ///
    /// ★ 这两处与注释共用同一个 `upright_text`，但仍各测一次：接线漏一处的表现只是
    /// 「某一段还躺着」，画面正常、别的测试全绿。
    ///
    /// ⚠️ 判据落在**窗口高度**上，且要让被测那一段成为最长的一列（故用长串 + 短候选）：
    /// 旋转态的列表在局部空间是 Column，往里加一个节点长的是局部**高度** ⇒ 屏幕**宽度**；
    /// 只有该节点自身沿列的长度（局部宽度）才映射到屏幕高度。拿「加了它高度涨多少」当
    /// 判据会恒为 0——本轮第一版正是这么写的，对照直接失效。
    ///
    /// 用拉丁串放大差距：前进宽约 0.5em、逐格站着约 1.2em，两倍以上；
    /// 汉字两轴只差两成，测不出分辨力。
    #[test]
    fn upright_stacks_the_inline_preedit_and_mode_chip() {
        const LONG: &str = "abcdefghijkl";
        let h = |upright: bool, preedit: &str, label: &str| {
            let (tx, _rx) = std::sync::mpsc::channel();
            let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
            w.scale = 1.0;
            w.set_orientation(false, true, upright);
            w.set_preedit_embedded(true);
            w.update(preedit, preedit.len(), label, vec![cand("甲")], 0, -1, 1, 1);
            let mut root = w.scene.build_tree(&w.scene_input(false));
            root.layout(0.0, 0.0, &w.text_renderer);
            root.measured_size().1
        };
        let bare = h(false, "", "");
        for (what, preedit, label) in [("内联编码", LONG, ""), ("模式徽标", "", LONG)] {
            let rot = h(false, preedit, label);
            let upr = h(true, preedit, label);
            assert!(
                rot > bare,
                "{what} 对照失效：它没成为最长的一列，这条测不出东西（{rot} vs {bare}）"
            );
            assert!(
                upr > rot * 1.8,
                "{what} 没逐格扶正：直立 {upr} vs 旋转 {rot}"
            );
        }
    }

    /// ★ 截断量的是**沿列方向**的长度，不是文字的横向宽度。
    ///
    /// 同一个预算下，直立态能放的字必须比横向**少**——拉丁字母横向前进约 0.5em、
    /// 竖着占一个行高约 1.2em。两者若一样多，说明截断还在按横向宽度估容量，
    /// 表现是「英文候选在对联模式下戳出屏幕」，而中文候选看着完全正常
    /// （汉字两轴只差两成，肉眼看不出来）。
    #[test]
    fn upright_truncation_measures_the_stacking_axis() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        let style = TextStyle::new(20.0);
        let text = "abcdefghij";
        // 预算取「横向刚好放得下五个字母」，两态用同一个数。
        let budget = w.text_renderer.measure("abcde", &style).width;

        w.set_orientation(false, false, false);
        let flat = w
            .scene_input(false)
            .truncate_candidate_text(text, &style, budget);
        w.set_orientation(false, true, true);
        let stacked = w
            .scene_input(false)
            .truncate_candidate_text(text, &style, budget);

        assert!(
            flat.chars().count() > stacked.chars().count(),
            "直立态该放得更少：横向 {flat:?} vs 直立 {stacked:?}"
        );
        // 再紧的预算也至少留一个字 + 省略号，不能什么都不显示。
        w.set_orientation(false, true, true);
        assert_eq!(
            w.scene_input(false)
                .truncate_candidate_text(text, &style, 0.0),
            "a…"
        );
        assert_eq!(
            w.scene_input(false)
                .truncate_candidate_text("", &style, 0.0),
            ""
        );
    }

    /// ★★ 旋转态走的是**竖排**的列表路径：每个候选在局部空间里独占一行、宽度统一
    /// （高亮宽度一致），转完就是**等宽的列**。
    ///
    /// ⚠️ 断言落在**高度**上：局部宽度经顺时针 90° 变成屏幕高度。
    /// 断言宽度是测不出来的——那对应局部**高度**（行高），无论走哪条路径都恒等，
    /// 本轮变异验证抓出的第二条假绿正是这个。
    ///
    /// 反向对照不可少：同一批长短不一的候选在横排下宽度必须**互不相同**。
    #[test]
    fn rotated_uses_the_vertical_list_path() {
        let texts = ["甲", "乙丙丁", "戊己庚辛壬"];
        let (_, r_root) = build_o(false, true, &texts);
        let r = hits_by_tag(&r_root);
        assert!(
            (r[0].h - r[1].h).abs() < 0.5 && (r[1].h - r[2].h).abs() < 0.5,
            "旋转态各列应等高（＝竖排路径统一的高亮宽度转过来）: {:?}",
            r.iter().map(|x| x.h).collect::<Vec<_>>()
        );
        let (_, h_root) = build_o(false, false, &texts);
        let h = hits_by_tag(&h_root);
        assert!(
            (h[0].w - h[2].w).abs() > 0.5,
            "横排下长短候选宽度应不同，否则本用例的对照失效: {:?}",
            h.iter().map(|x| x.w).collect::<Vec<_>>()
        );
    }
}

/// 竖排「主词条优先」宽度分配（论坛 t271）：注释过长时让步的是注释，不是候选文字。
/// 分配规则见 `build_tree` 里竖排那一支的注释。mock 后端等宽（0.6em），只用几何断言。
#[cfg(test)]
mod vertical_comment_budget_tests {
    use super::*;

    fn cand(text: &str, comment: &str) -> CandidateItem {
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

    /// 竖排、scale=1、主题竖排最大宽度 `vmax_dp`（0=只受屏幕安全宽约束）。
    fn build(vertical: bool, vmax_dp: i32, items: Vec<CandidateItem>) -> (CandidateWindow, View) {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(vertical, false, false);
        w.theme.behavior.vertical_max_width = vmax_dp;
        w.update("", 0, "", items, 0, -1, 1, 1);
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        (w, root)
    }

    fn find_list(v: &View) -> Option<&View> {
        if v.children.iter().any(|c| c.tag >= 0) {
            return Some(v);
        }
        v.children.iter().find_map(find_list)
    }

    /// 候选 item 的 (文字, 注释) 文本（item = Row[序号, 文字, 注释?]）。
    fn texts(root: &View) -> Vec<(String, Option<String>)> {
        let list = find_list(root).expect("未找到候选列表容器");
        let mut its: Vec<&View> = list.children.iter().filter(|c| c.tag >= 0).collect();
        its.sort_by_key(|c| c.tag);
        its.iter()
            .map(|it| {
                let t = it.children[1].text.clone().unwrap_or_default();
                let c = it.children.get(2).and_then(|c| c.text.clone());
                (t, c)
            })
            .collect()
    }

    /// ★ t271 主症状：注释极长时主词条完整显示，注释截断加 `…`，窗口不超上限。
    #[test]
    fn long_comment_yields_to_candidate_text() {
        let long = "注".repeat(300);
        for vmax in [0, 300] {
            let (w, root) = build(true, vmax, vec![cand("你好", &long), cand("世界", "sj")]);
            let t = texts(&root);
            assert_eq!(t[0].0, "你好", "vmax={vmax}：主词条不得被注释挤截");
            let c0 = t[0].1.as_deref().unwrap();
            assert!(c0.ends_with('…'), "vmax={vmax}：超长注释应截断加省略号");
            assert!(c0.chars().count() < long.chars().count());
            assert_eq!(t[1], ("世界".into(), Some("sj".into())), "短注释不受影响");
            let cap = if vmax > 0 {
                vmax as f32
            } else {
                w.screen_safety_max_width_px() as f32
            };
            assert!(
                root.measured_size().0 <= cap + 0.5,
                "vmax={vmax}：窗口宽 {} 超出上限 {cap}",
                root.measured_size().0
            );
        }
    }

    /// 放得下就一个字都不动（零回归）。
    #[test]
    fn fitting_text_and_comment_are_untouched() {
        let (_, root) = build(true, 300, vec![cand("你好", "ni hao"), cand("世界", "")]);
        let t = texts(&root);
        assert_eq!(t[0], ("你好".into(), Some("ni hao".into())));
        assert_eq!(t[1], ("世界".into(), None));
    }

    /// 主词条自身就超长：主词条截断，但仍给注释留出保底（看得见一截 + `…`），行宽不超上限。
    #[test]
    fn overlong_text_still_leaves_room_for_comment() {
        let long_text = "长".repeat(100);
        let (_, root) = build(true, 300, vec![cand(&long_text, "chang")]);
        let t = texts(&root);
        assert!(t[0].0.ends_with('…'), "主词条超出行宽时照常截断");
        let c = t[0].1.as_deref().expect("注释节点仍在");
        assert!(!c.is_empty());
        assert!(
            root.measured_size().0 <= 300.5,
            "行宽 {}",
            root.measured_size().0
        );
    }

    /// 多行注释（竖排模板里写 `\n`）逐行截断：首行超长不会把后面几行整个吃掉。
    #[test]
    fn multiline_comment_truncates_per_line() {
        let long = "注".repeat(300);
        let comment = format!("{long}\nshort");
        let (_, root) = build(true, 300, vec![cand("你好", &comment)]);
        let t = texts(&root);
        assert_eq!(t[0].0, "你好");
        let c = t[0].1.as_deref().unwrap();
        let lines: Vec<&str> = c.split('\n').collect();
        assert_eq!(lines.len(), 2, "行数不变：{c:?}");
        assert!(lines[0].ends_with('…'));
        assert_eq!(lines[1], "short");
    }
}

/// 方案级候选字体（`[candidate] font_family`）的接线。
///
/// ⚠️ **必须 gate 到 Windows**：断言的是「换了字族宽度就变」，而非 Windows 的 mock
/// 后端明写「字重/字体族不影响等宽近似测量，只取字号」（`text/dwrite.rs` 的 mock `measure`）
/// ⇒ Linux CI 上 `assert_ne!` 两边相等直接红，而本机 Windows 全绿看不见。
/// 同组的 `assert_eq!` 那条在 Linux 上更糟——它**恒真**、是条假绿。
/// 这正是本仓「test 跑 Linux、clippy 交叉编 Win」那条分工的典型踩法，
/// 先例见 `text/dwrite.rs` 的 `font_plan_tests`。
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

    /// ★ 覆盖真的作用到排版上；空串 = 不覆盖。
    ///
    /// 反向对照不可少：只断言「变了」的话，一个「无条件套用覆盖」的实现在空串下也会改字体。
    #[test]
    fn override_changes_layout_and_empty_means_no_override() {
        let texts = ["illill", "候选"];
        let base = build("", "", &texts);
        let overridden = build("Consolas", "", &texts);
        assert_ne!(base, overridden, "方案级字族没作用到排版上");
    }

    /// ★★ 覆盖只作用于**候选文字**：编码栏由 preedit 决定，宽度不得受影响。
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

    /// ★ 预算被行内其它成员吃光（≤0）时语义是**截到最短**，不是「不截断」。
    ///
    /// 反向写法（早退返回原文）会让「预算耗尽」变成「完全放开」，把溢出放大而不是收敛——
    /// 正是它此前被 `.max(20*s)` 地板掩盖着没暴露。
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

    /// ★ 测量样式必须与渲染叶子同构：字重/字族归一化规则照抄 `View::font_weight`（>0 才生效）
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
mod water_fill_tests {
    use super::CandidateScene;

    /// 总额够用：人人原样满足，不做无谓截断。
    #[test]
    fn everyone_fits_when_total_is_enough() {
        let got = CandidateScene::water_fill(&[100.0, 200.0, 50.0], 1000.0, 0.0);
        assert_eq!(got, vec![100.0, 200.0, 50.0]);
    }

    /// ★ 核心性质：短需求原样保留，**把没用完的份额让给长需求**，而不是一刀切均分。
    ///
    /// 均分会给长的 600/3=200，water-filling 给到 400——这正是「1 条长整句 + 8 个短词」
    /// 这个最常见场景里两者的观感差别。
    #[test]
    fn spare_share_flows_to_the_long_one() {
        let got = CandidateScene::water_fill(&[100.0, 100.0, 1000.0], 600.0, 0.0);
        assert_eq!(got, vec![100.0, 100.0, 400.0]);
        assert_eq!(got.iter().sum::<f32>(), 600.0);
    }

    /// 顺序无关：同一组需求换个排列，各自拿到的份额不变（内部按需求升序处理，结果按原序返回）。
    #[test]
    fn allocation_is_order_independent() {
        let a = CandidateScene::water_fill(&[1000.0, 100.0, 100.0], 600.0, 0.0);
        assert_eq!(a, vec![400.0, 100.0, 100.0]);
    }

    /// ★ 不溢出保证：只要 total ≥ 0 且不触发 floor，`Σ result ≤ total` **与项数无关**。
    /// 这正是贪心（先到先得 + 下限）给不了的性质——那种做法下第一项吃光预算，
    /// 后面每项仍各自加上「固定开销 + 文字下限」，累加必然突破上限。
    #[test]
    fn sum_never_exceeds_total_regardless_of_count() {
        for n in 1..=20usize {
            let demands: Vec<f32> = (0..n).map(|i| (i as f32 + 1.0) * 500.0).collect();
            let total = 1000.0;
            let got = CandidateScene::water_fill(&demands, total, 0.0);
            let sum: f32 = got.iter().sum();
            assert!(sum <= total + 0.01, "n={n} 时 Σ={sum} 超出 total={total}");
        }
    }

    /// 全员都超份额：平分总额。
    #[test]
    fn all_oversized_split_evenly() {
        let got = CandidateScene::water_fill(&[900.0, 900.0, 900.0], 300.0, 0.0);
        assert_eq!(got, vec![100.0, 100.0, 100.0]);
    }

    /// floor 是「无解时的保底」：固定开销已超预算（total ≤ 0）时每项仍拿到 floor，
    /// 总和因此可能超出 total——这是刻意取舍，宁可溢出被裁，也不少显示候选
    /// （选中键位与候选列表绑定，少一个会让用户按 8 选到错的词）。
    #[test]
    fn floor_guarantees_minimum_even_when_insolvent() {
        let got = CandidateScene::water_fill(&[900.0, 900.0], 0.0, 20.0);
        assert_eq!(got, vec![20.0, 20.0]);
        let got = CandidateScene::water_fill(&[900.0, 900.0], -500.0, 20.0);
        assert_eq!(got, vec![20.0, 20.0]);
    }

    /// 空输入不 panic（无候选时的退化情形）。
    #[test]
    fn empty_demands_yield_empty_result() {
        assert!(CandidateScene::water_fill(&[], 100.0, 0.0).is_empty());
    }
}

/// `[ui.font]` → [`FontPlan`] 的折叠规则。平台无关（不碰窗口/COM），故随 Linux CI 跑。
#[cfg(test)]
mod font_plan_build_tests {
    // DEFAULT_EMOJI_FAMILY 只被下面 `#[cfg(windows)]` 的出厂指派用例引用，
    // 导入不带 cfg 的话 macOS/Linux 上就是 unused_imports（CI 的 -D warnings 会红）。
    #[cfg(windows)]
    use super::DEFAULT_EMOJI_FAMILY;
    use super::{DEFAULT_FONT_FAMILY, build_font_plan};
    use crate::text::script::ScriptClass;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// ★ 空字族的链首必须是内置默认，不能是空串。
    ///
    /// 空串链首的表现最阴：`AddMapping` 的 `baseFamilyName` 是空串就永远匹配不上任何段
    /// ⇒「用户没显式写 family 时，配了 fallback 也不生效」，而配置、日志、界面全都正常。
    #[test]
    fn empty_family_becomes_the_builtin_default_at_the_chain_head() {
        let p = build_font_plan("", &v(&["Noto Sans Mongolian"]), &[]);
        assert_eq!(p.base_family(), Some(DEFAULT_FONT_FAMILY));
        assert_eq!(
            p.chain_for(None),
            [DEFAULT_FONT_FAMILY, "Noto Sans Mongolian"]
        );
        // 只有空白也算空。
        assert_eq!(
            build_font_plan("   ", &[], &[]).base_family(),
            Some(DEFAULT_FONT_FAMILY)
        );
    }

    /// 默认链 = `[family] + fallback`，顺序不能颠倒（颠倒后主字体成了回退项）。
    #[test]
    fn default_chain_is_family_then_fallback() {
        let p = build_font_plan(
            "Mongolian Baiti",
            &v(&["Noto Sans Mongolian", "Segoe UI"]),
            &[],
        );
        assert_eq!(
            p.chain_for(None),
            ["Mongolian Baiti", "Noto Sans Mongolian", "Segoe UI"]
        );
    }

    /// 已知脚本类名进方案，未知的**只忽略这一条**——不得连累同一份配置里其余的类。
    /// 用户拼错一个键就让整份字体配置失效是不能接受的。
    #[test]
    fn unknown_script_key_is_dropped_without_affecting_the_others() {
        let p = build_font_plan(
            "Mongolian Baiti",
            &[],
            &[
                ("latin".to_string(), v(&["Segoe UI"])),
                ("mongolian".to_string(), v(&["Whatever"])), // 未知：不是具名类
                ("CJK".to_string(), v(&["宋体"])),           // 大小写不敏感
            ],
        );
        // 顺序是 `ScriptClass` 的**声明序**（`FontPlan::new` 按 `Ord` 排），不是字母序。
        // Windows 上出厂会再注入 emoji 一项，这里只看用户声明的那两条。
        let user_declared: Vec<ScriptClass> = p
            .declared()
            .iter()
            .copied()
            .filter(|c| *c != ScriptClass::Emoji)
            .collect();
        assert_eq!(user_declared, [ScriptClass::Latin, ScriptClass::Cjk]);
        assert_eq!(p.chain_for(Some(ScriptClass::Latin)), ["Segoe UI"]);
        assert_eq!(p.chain_for(Some(ScriptClass::Cjk)), ["宋体"]);
    }

    /// 零配置（出厂）必须折成平凡方案——调用方据此完全走旧路径，一次 COM 调用都不多做。
    ///
    /// Windows 除外：出厂会注入 emoji 类的指派（见 `build_font_plan`），零配置不再平凡，
    /// 由 `factory_config_assigns_emoji_on_windows` 守。
    #[cfg(not(windows))]
    #[test]
    fn factory_config_folds_to_a_trivial_plan() {
        assert!(build_font_plan("", &[], &[]).is_trivial());
        assert!(build_font_plan("宋体", &[], &[]).is_trivial());
    }

    /// 反向对照（各平台）：只要有一项非零配置就不再平凡（否则「平凡」判据形同虚设）。
    #[test]
    fn any_non_factory_item_makes_the_plan_non_trivial() {
        assert!(!build_font_plan("宋体", &v(&["Arial"]), &[]).is_trivial());
        assert!(
            !build_font_plan("宋体", &[], &[("latin".to_string(), v(&["Arial"]))]).is_trivial()
        );
    }

    /// ★ Windows 出厂：emoji 类恒有指派（Segoe UI Emoji），否则 1️⃣ 与 Emoji 16.0 的新码位
    /// 渲染不出来；用户显式声明 emoji（含空表）时按用户的来，空表即关掉这条指派。
    #[cfg(windows)]
    #[test]
    fn factory_config_assigns_emoji_on_windows() {
        let p = build_font_plan("", &[], &[]);
        assert_eq!(p.declared(), &[ScriptClass::Emoji]);
        assert_eq!(
            p.chain_for(Some(ScriptClass::Emoji)),
            [DEFAULT_EMOJI_FAMILY]
        );
        // 其余零配置语义不变：默认链仍只有一项。
        assert_eq!(p.chain_for(None), [DEFAULT_FONT_FAMILY]);

        // 用户自己指派 emoji：不覆盖。
        let p = build_font_plan("", &[], &[("emoji".to_string(), v(&["Twemoji"]))]);
        assert_eq!(p.chain_for(Some(ScriptClass::Emoji)), ["Twemoji"]);

        // 逃生口：`emoji = []` 关掉出厂指派，回到纯系统回退。
        let p = build_font_plan("", &[], &[("emoji".to_string(), vec![])]);
        assert!(
            p.is_trivial(),
            "emoji = [] 应关掉出厂指派：{:?}",
            p.declared()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{CandidateScene, CandidateWindow, plan_present};

    #[test]
    fn present_full_when_not_on_screen() {
        assert_eq!(plan_present(None, (10, 20, 300, 100), true), (true, true));
    }

    #[test]
    fn present_skips_identical_frame() {
        let g = (10, 20, 300, 100);
        assert_eq!(plan_present(Some(g), g, true), (false, false));
    }

    #[test]
    fn present_moves_without_repaint_when_only_position_changed() {
        assert_eq!(
            plan_present(Some((10, 20, 300, 100)), (40, 20, 300, 100), true),
            (false, true)
        );
    }

    #[test]
    fn present_repaints_in_place_when_pixels_changed() {
        let g = (10, 20, 300, 100);
        assert_eq!(plan_present(Some(g), g, false), (true, false));
    }

    /// 像素字节数相同但宽高互换（缓冲长度一样）也必须重推。
    #[test]
    fn present_repaints_when_size_changed_with_same_byte_count() {
        assert_eq!(
            plan_present(Some((10, 20, 300, 100)), (10, 20, 100, 300), true),
            (true, true)
        );
    }

    /// 主题位置偏移的方向语义：正值恒为「远离光标」——下方向下、上方向上。
    ///
    /// 上方若照抄下方的加号，上翻时偏移会把窗口**压向**光标，与配置意图相反。
    /// 这类符号错误从现象很难反推（只有触发上翻的场景才露馅），故直接锁死。
    #[test]
    fn position_offset_always_pushes_away_from_caret() {
        // 下方：正值下移
        assert_eq!(CandidateWindow::apply_offset_y(300, false, 12), 312);
        // 上方：正值上移（同样是远离，不是压向光标）
        assert_eq!(CandidateWindow::apply_offset_y(300, true, 12), 288);
        // 零偏移两侧都是恒等
        assert_eq!(CandidateWindow::apply_offset_y(300, false, 0), 300);
        assert_eq!(CandidateWindow::apply_offset_y(300, true, 0), 300);
        // 负偏移语义对称（把窗口拉近光标）
        assert_eq!(CandidateWindow::apply_offset_y(300, false, -6), 294);
        assert_eq!(CandidateWindow::apply_offset_y(300, true, -6), 306);
    }

    /// **净锚点不含 Y 偏移**——这是「偏移不左右上下翻转决策」的地基。
    ///
    /// `below_ok`/`above_ok` 拿锚点跟工作区边界比：锚点一旦含偏移，off_y 越大两个条件
    /// 越难成立，本该上翻的场景会被判成「上方也放不下」，落回下方再被钳到屏幕底、
    /// 压住光标——表现就是「下方正常、上方遮盖」。故锁死锚点与 off_y 无关。
    #[test]
    fn caret_anchors_are_independent_of_y_offset() {
        let (caret_x, caret_y, caret_h, win_h) = (100, 200, 20, 80);
        let a = CandidateWindow::caret_anchors(caret_x, caret_y, caret_h, win_h, 0);
        // 净锚点即旧行为：下方=光标底端+gap，上方=底边贴光标顶端
        assert_eq!(a.1, caret_y + 2, "下方 = 光标底端 + gap");
        assert_eq!(a.2, caret_y - caret_h - win_h - 2, "上方 = 底边贴光标顶端");
        // x 偏移仍在锚点里（水平方向没有翻转决策，不受影响）
        let b = CandidateWindow::caret_anchors(caret_x, caret_y, caret_h, win_h, 5);
        assert_eq!(b.0 - a.0, 5, "x 偏移右移");
        assert_eq!((b.1, b.2), (a.1, a.2), "x 偏移不该动垂直锚点");
    }

    /// 「上方反转候选」只对竖排成立，横排必须原样保持 1..n。
    ///
    /// 横排候选是左右并列，"反转"跟窗口在光标上方还是下方没有任何关系，只会把读序
    /// 倒过来（1 跑到最右）。两个开关正交，故一并锁死 swap 不受排列方向影响。
    #[test]
    fn flip_when_above_applies_to_vertical_only() {
        // (above, vertical, flip, swap) -> (flip_cands, swap_bands)
        let f = CandidateScene::above_layout;
        assert_eq!(
            f(true, true, true, false),
            (true, false),
            "竖排上方：反转生效"
        );
        assert_eq!(
            f(true, false, true, false),
            (false, false),
            "横排上方：反转不得生效"
        );
        assert_eq!(
            f(false, true, true, false),
            (false, false),
            "下方：一律不生效"
        );
        // swap 交换的是上下两条带，横竖排都成立
        assert_eq!(
            f(true, false, false, true),
            (false, true),
            "横排上方：交换仍生效"
        );
        assert_eq!(f(true, true, false, true), (false, true));
        // 叠加：竖排两个都开 → 两个都生效；横排只剩 swap
        assert_eq!(f(true, true, true, true), (true, true));
        assert_eq!(f(true, false, true, true), (false, true));
    }

    /// 插入符位置一律夹到合法字符边界——`split_at` 落在字符中间会 panic 掉 UI 线程。
    #[test]
    fn clamp_caret_never_splits_a_char() {
        assert_eq!(CandidateWindow::clamp_caret("nihao", 3), 3);
        assert_eq!(CandidateWindow::clamp_caret("nihao", 5), 5, "末尾合法");
        assert_eq!(CandidateWindow::clamp_caret("nihao", 99), 5, "越界截到末尾");
        assert_eq!(CandidateWindow::clamp_caret("", 4), 0, "空串恒 0");
        // 「你」占 3 字节：落在 1/2 时退回 0，落在 3 是边界
        assert_eq!(CandidateWindow::clamp_caret("你hao", 1), 0);
        assert_eq!(CandidateWindow::clamp_caret("你hao", 2), 0);
        assert_eq!(CandidateWindow::clamp_caret("你hao", 3), 3);
        assert_eq!(CandidateWindow::clamp_caret("你hao", 4), 4);
    }

    /// clamp 后切分恒安全（本测试的意义在于：若 clamp 失效，split_at 会 panic）。
    #[test]
    fn clamped_caret_is_safe_to_split() {
        for text in ["", "nihao", "你好hao", "·ni'hao"] {
            for caret in 0..=text.len() + 2 {
                let c = CandidateWindow::clamp_caret(text, caret);
                let (head, tail) = text.split_at(c);
                assert_eq!(format!("{head}{tail}"), text);
            }
        }
    }

    /// 固定位置的两个坐标系必须严格互逆。
    ///
    /// 落盘的 custom_x/y 是**内容**左上，Win32 定位用的是**窗口**左上（含软阴影扩边）：
    /// 固定定位走 content→window，拖动落定上报走 window→content。若两者不互逆，
    /// 每轮「拖动 → 落盘 → 重新显示」都会多减一次阴影，候选窗每次重显都往左上爬一点——
    /// 这种漂移从现象极难反推，故在此直接锁死。
    #[cfg(windows)]
    #[test]
    fn content_window_coord_roundtrip_is_lossless() {
        // 覆盖有/无阴影、负坐标（副屏在主屏左侧时屏幕坐标为负）
        for (content, ml, mt) in [
            ((100, 200), 12u32, 10u32),
            ((0, 0), 0, 0),
            ((-1920, 40), 8, 8),
            ((3, 7), 1, 0),
        ] {
            let w = CandidateWindow::content_to_window(content, ml, mt);
            let back = CandidateWindow::window_to_content(w, ml as i32, mt as i32);
            assert_eq!(back, content, "content={content:?} margin=({ml},{mt})");
        }
    }

    /// 阴影扩边确实被减掉了——若 content_to_window 退化成恒等函数，
    /// 上面的 round-trip 仍会通过，但固定位置会整体偏移一个阴影的量。
    #[cfg(windows)]
    #[test]
    fn content_to_window_subtracts_the_shadow_margin() {
        assert_eq!(
            CandidateWindow::content_to_window((100, 200), 12, 10),
            (88, 190)
        );
    }
}

/// 主题节点字族的存在性检查接线。
///
/// ⚠️ **只在 mock 后端下编译**：`family_exists` 在 Windows 上要问系统字体集、在 CoreText 上
/// 尚未实现，两边都没有 `asked_families()` 这个读回口。而「有没有真的去问」正是这条链唯一
/// 钉得住的一半——它此前整个不存在，主题里把字族名写错只表现为「字体不对」，日志里一个字
/// 都没有。
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

/// 字体名解析的接线（看板 A2-1）：旧 GDI face name 在**每个**字体入口都换成 family + 字重。
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

    /// 靶机复现（维护者实测「字重无效」）：主题「Switch风格」给 `[text]` 写了 500、
    /// `[text.selected]` 写了 700，用户 `ui.font.weight = 900` 对候选文字一个字都不生效。
    /// 用户字重直接覆盖主题里所有节点（含选中等状态）写了的字重。
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

    /// ★ 用户字体非空：全部节点（不只候选文字）的主题字族都让位，叶子回落全局字族（=用户字体）。
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
mod tip_hold_tests {
    use super::*;
    use crate::popup_menu::set_menu_visible;
    use std::sync::mpsc::{Receiver, channel};

    const DELAY: u64 = 60;

    fn mouse(delay_ms: u64) -> (CandidateMouse, Receiver<UiEvent>) {
        set_menu_visible(false);
        let (tx, rx) = channel();
        let r = |x| Rect {
            x,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        let m = CandidateMouse {
            hit_rects: vec![(0, r(0.0)), (1, r(20.0))],
            skin_frame: None,
            skin_decor: Vec::new(),
            skin_click_rects: Vec::new(),
            decor_press: None,
            skin_click: None,
            pass_hwnd: None,
            events: tx,
            last_hover: -1,
            last_cursor: (i32::MIN, i32::MIN),
            engaged: false,
            engage_at: None,
            pending_raw: -1,
            engage_delay_ms: delay_ms,
            hwnd: HWND::default(),
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
            tip_for: Rc::new(Cell::new(-1)),
            deferred: None,
            tip_rect: None,
            tip_visible: None,
            tip_best: None,
            last_pos: None,
        };
        (m, rx)
    }

    fn lp(x: i32, y: i32) -> LPARAM {
        LPARAM(((y << 16) | x) as isize)
    }

    fn real_move(m: &mut CandidateMouse, x: i32) {
        m.last_cursor = (i32::MIN, i32::MIN);
        m.on_message(HWND::default(), WM_MOUSEMOVE, WPARAM(0), lp(x, 5));
    }

    fn right_click(m: &mut CandidateMouse, x: i32) {
        m.on_message(HWND::default(), WM_RBUTTONDOWN, WPARAM(0), lp(x, 5));
    }

    /// 远在任何延迟之后（但早于「等菜单」的兜底超时）。
    fn later() -> Instant {
        Instant::now() + Duration::from_millis(1000)
    }

    fn hovers(rx: &Receiver<UiEvent>) -> Vec<i32> {
        rx.try_iter()
            .filter_map(|e| match e {
                UiEvent::Hover(i) => Some(i),
                _ => None,
            })
            .collect()
    }

    fn menu_requests(rx: &Receiver<UiEvent>) -> usize {
        rx.try_iter()
            .filter(|e| {
                matches!(
                    e,
                    UiEvent::RequestCandidateMenu { .. } | UiEvent::RequestMainMenu(_)
                )
            })
            .count()
    }

    fn selects(rx: &Receiver<UiEvent>) -> Vec<usize> {
        rx.try_iter()
            .filter_map(|e| match e {
                UiEvent::CandidateSelect(i) => Some(i),
                _ => None,
            })
            .collect()
    }

    /// 点选在松开时生效（同 macOS / Linux）：按下不选，松开仍在同一项才选，且只选一次。
    #[test]
    fn candidate_select_fires_on_release_over_same_item() {
        let (mut m, rx) = mouse(DELAY);
        m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(0), lp(5, 5));
        assert!(selects(&rx).is_empty(), "按下就选了");
        m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(6, 5));
        assert_eq!(selects(&rx), vec![0]);
        m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(6, 5));
        assert!(selects(&rx).is_empty(), "一次按下只选一次");
    }

    /// 按下后移到别的候选 / 空白处松开 = 放弃。
    #[test]
    fn candidate_select_abandoned_when_released_elsewhere() {
        let (mut m, rx) = mouse(DELAY);
        m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(0), lp(5, 5));
        m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(25, 5));
        m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(0), lp(5, 5));
        m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(500, 5));
        assert!(selects(&rx).is_empty());
        assert!(!m.dragging, "按在候选上不起拖");
    }

    /// 按住期间候选窗收起（上屏 / Esc / 失焦）：之后的松开不选。
    #[test]
    fn candidate_select_dropped_when_hidden_while_pressed() {
        let (mut m, rx) = mouse(DELAY);
        m.on_message(HWND::default(), WM_LBUTTONDOWN, WPARAM(0), lp(5, 5));
        m.reset_drag();
        m.on_message(HWND::default(), WM_LBUTTONUP, WPARAM(0), lp(5, 5));
        assert!(selects(&rx).is_empty());
    }

    /// 已激活、悬停在候选 0 上（UI 画着的悬停也是 0）。
    fn engaged_on_first(delay_ms: u64) -> (CandidateMouse, Receiver<UiEvent>) {
        let (mut m, rx) = mouse(delay_ms);
        real_move(&mut m, 5);
        m.flush(later(), -1);
        assert_eq!(hovers(&rx), vec![0]);
        (m, rx)
    }

    /// 右键时激活延迟还没到期：到期后不能再发 Hover——那正是「气泡在菜单弹出后冒出来、
    /// 叠在菜单上」的来路。
    #[test]
    fn right_click_cancels_pending_engage() {
        let (mut m, rx) = mouse(DELAY);
        real_move(&mut m, 5);
        assert!(m.engage_at.is_some(), "首次真实移动武装闸门");
        right_click(&mut m, 5);
        m.flush(later(), -1);
        assert!(m.tip_hold.active());
        assert!(m.engage_at.is_none());
        assert!(hovers(&rx).is_empty(), "闸门已撤，不应再发悬停");
    }

    /// 审查探针（H1）：右键 → 菜单可见之前鼠标动了 → 菜单可见 → 到期。整段都不得发 Hover。
    /// 右键到菜单可见之间「菜单开着」还是 false，只靠它挡闸门时这里会漏。
    fn probe_right_move_menu_flush(delay_ms: u64) {
        let (mut m, rx) = engaged_on_first(delay_ms);
        right_click(&mut m, 5);
        real_move(&mut m, 25);
        m.flush(later(), 0);
        assert!(hovers(&rx).is_empty(), "等菜单阶段不得发悬停");
        set_menu_visible(true);
        m.flush(later(), 0);
        real_move(&mut m, 6);
        m.flush(later(), 0);
        assert_eq!(m.tip_hold, TipHold::InMenu);
        assert!(m.engage_at.is_none(), "菜单中不武装");
        assert!(hovers(&rx).is_empty(), "菜单中不得发悬停");
    }

    #[test]
    fn move_before_menu_shows_does_not_rearm() {
        probe_right_move_menu_flush(DELAY);
    }

    /// `ui.tooltip.delay=0`：闸门一武装就到期，没有「等菜单」阶段时气泡会抢在菜单之前出来。
    #[test]
    fn move_before_menu_shows_does_not_rearm_with_zero_delay() {
        probe_right_move_menu_flush(0);
    }

    /// 菜单关闭后要重新移动、再走一次延迟才解除；UI 画着的悬停就是光标下的目标时由窗口
    /// 本地补显示（不发 Hover，协调器不会因同值重绘）。
    #[test]
    fn release_after_fresh_engage_same_target() {
        let (mut m, rx) = engaged_on_first(DELAY);
        right_click(&mut m, 5);
        set_menu_visible(true);
        m.flush(Instant::now(), 0);
        set_menu_visible(false);
        m.flush(later(), 0);
        assert_eq!(m.tip_hold, TipHold::AwaitMove, "菜单一关、鼠标没动：仍压着");
        real_move(&mut m, 6);
        let armed = m.engage_at.expect("真实移动武装闸门");
        m.flush(armed - Duration::from_millis(1), 0);
        assert!(m.tip_hold.active(), "延迟没走完不解除");
        m.flush(armed, 0);
        assert_eq!(m.tip_hold, TipHold::Off);
        assert!(m.tip_released, "UI 画着的就是它，须本地补显示");
        assert!(hovers(&rx).is_empty());
    }

    /// 解除时目标变了：发 Hover，由协调器重绘带出气泡。
    #[test]
    fn release_after_fresh_engage_new_target() {
        let (mut m, rx) = engaged_on_first(DELAY);
        right_click(&mut m, 5);
        m.flush(later(), 0);
        real_move(&mut m, 25);
        // 没有菜单出现：等兜底超时后才算关闭。
        m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 2, 0);
        real_move(&mut m, 26);
        m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 3, 0);
        assert_eq!(m.tip_hold, TipHold::Off);
        assert!(!m.tip_released);
        assert_eq!(hovers(&rx), vec![1]);
    }

    /// M1：压制期间候选重绘过、协调器把悬停清成 -1（UI 画的是 -1），而 `last_hover` 还是 0。
    /// 解除时回到同一候选，必须发 Hover(0)——按 `last_hover` 判会以为「没变」，气泡不再回来。
    #[test]
    fn release_after_repaint_resends_hover() {
        let (mut m, rx) = engaged_on_first(DELAY);
        right_click(&mut m, 5);
        set_menu_visible(true);
        m.flush(Instant::now(), 0);
        set_menu_visible(false);
        real_move(&mut m, 6);
        m.flush(later(), -1);
        assert_eq!(m.tip_hold, TipHold::Off);
        assert!(!m.tip_released);
        assert_eq!(hovers(&rx), vec![0]);
    }

    /// 兜底超时：请求被丢、菜单始终没出现，压制不能永久持续。
    #[test]
    fn await_menu_times_out_to_await_move() {
        let (mut m, _rx) = mouse(DELAY);
        right_click(&mut m, 5);
        let TipHold::AwaitMenu { until } = m.tip_hold else {
            panic!("右键后应在等菜单：{:?}", m.tip_hold);
        };
        m.flush(until - Duration::from_millis(1), -1);
        assert!(m.tip_hold.blocks_gate());
        m.flush(until, -1);
        assert_eq!(m.tip_hold, TipHold::AwaitMove);
    }

    /// 迟到的菜单（超时之后才出现）照样压住，已武装的闸门作废。
    #[test]
    fn late_menu_reenters_in_menu() {
        let (mut m, rx) = mouse(DELAY);
        right_click(&mut m, 5);
        m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 2, -1);
        real_move(&mut m, 6);
        assert!(m.engage_at.is_some());
        set_menu_visible(true);
        m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 3, -1);
        assert_eq!(m.tip_hold, TipHold::InMenu);
        assert!(m.engage_at.is_none());
        assert!(hovers(&rx).is_empty());
    }

    /// 菜单开着时候选窗自己收到的右键不理：这次按下由菜单轮询合成，两路都认会请求两遍。
    #[test]
    fn own_right_click_ignored_while_menu_visible() {
        let (mut m, rx) = mouse(DELAY);
        set_menu_visible(true);
        right_click(&mut m, 5);
        assert_eq!(menu_requests(&rx), 0);
        assert!(!m.tip_hold.active());
    }

    /// 「等菜单」阶段自己的右键不理：合成那一路已发出请求、菜单还没出现（轮询已把旧菜单
    /// 收起，看不出「开着」），同一次右键的真实消息随后才到——不挡就请求两遍、菜单重弹。
    /// 超时转入「等重新移动」后恢复。
    #[test]
    fn own_right_click_ignored_while_awaiting_menu() {
        let (mut m, rx) = mouse(DELAY);
        m.right_click(5.0, 5.0, (5, 5));
        assert_eq!(menu_requests(&rx), 1);
        right_click(&mut m, 5);
        assert_eq!(menu_requests(&rx), 0, "请求在途不得重发");
        m.flush(Instant::now() + HOLD_MENU_TIMEOUT * 2, -1);
        right_click(&mut m, 5);
        assert_eq!(menu_requests(&rx), 1, "超时后恢复");
    }

    /// 组合结束 / 窗口重新出现：压制随悬停状态一并清掉，不带进下一轮。
    #[test]
    fn reset_hover_clears_hold() {
        let (mut m, _rx) = mouse(DELAY);
        right_click(&mut m, 5);
        m.reset_hover();
        assert_eq!(m.tip_hold, TipHold::Off);
        assert!(!m.tip_released);
    }

    // ---- 窗口层：气泡真的被收起 / 补回（M2），菜单外右键合成（L1） ----

    fn doc() -> wind_ui_types::TooltipDoc {
        wind_ui_types::TooltipDoc {
            sections: vec![wind_ui_types::TooltipSection {
                title: None,
                inline: false,
                lines: vec![wind_ui_types::TooltipLine {
                    text: "nǐ".into(),
                    raw: 0,
                }],
            }],
        }
    }

    /// 候选窗（mock）：两个带气泡的候选，UI 悬停在候选 0 上，气泡已显示。
    fn window() -> (CandidateWindow, Receiver<UiEvent>) {
        set_menu_visible(false);
        let (tx, rx) = channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        let item = |t: &str| CandidateItem {
            text_styled: Default::default(),
            text: t.into(),
            code: String::new(),
            label: String::new(),
            tooltip: std::sync::Arc::new(doc()),
            comment: Default::default(),
            comment_above: Default::default(),
            no_index: false,
        };
        w.candidates = vec![item("你"), item("拟")];
        let r = |x| Rect {
            x,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        w.hit_rects = vec![(0, r(0.0)), (1, r(20.0))];
        w.mouse.borrow_mut().hit_rects = w.hit_rects.clone();
        w.hover = 0;
        w.visible = true;
        w.update_tooltip(0, 0);
        assert!(tip_shown(&w), "前提：气泡已显示");
        (w, rx)
    }

    fn tip_shown(w: &CandidateWindow) -> bool {
        w.tooltip.as_ref().is_some_and(|t| t.shown())
    }

    /// 右键当轮 tick 就收起已显示的气泡；压制期间重绘不再弹出；解除后补回。
    #[test]
    fn hold_hides_tip_and_repaint_keeps_it_hidden() {
        let (mut w, _rx) = window();
        w.mouse.borrow_mut().right_click(5.0, 5.0, (5, 5));
        w.tick();
        assert!(!tip_shown(&w), "右键后气泡须收起");
        w.update_tooltip(0, 0);
        assert!(!tip_shown(&w), "压制期间重绘不得弹出气泡");
        // 菜单出现又关闭，随后重新移动、走完延迟。
        set_menu_visible(true);
        w.tick();
        set_menu_visible(false);
        w.tick();
        {
            let mut m = w.mouse.borrow_mut();
            m.engage_delay_ms = 0;
            real_move(&mut m, 6);
        }
        w.tick();
        assert!(tip_shown(&w), "解除后同一候选的气泡须补回");
    }

    /// L1：菜单开着时在候选窗上右键（候选窗收不到，由菜单轮询转来）→ 关旧开新。
    #[test]
    fn outside_right_press_on_candidate_window_reopens_menu() {
        let (mut w, rx) = window();
        w.window_at = |_, _, _| true;
        w.menu_outside_press(25, 5, true);
        let evs: Vec<UiEvent> = rx.try_iter().collect();
        assert!(
            evs.iter()
                .any(|e| matches!(e, UiEvent::RequestCandidateMenu { page_local: 1, .. })),
            "命中候选 1 应请求其词条菜单：{evs:?}"
        );
        assert!(w.mouse.borrow().tip_hold.active(), "合成右键同样压住气泡");
        // 左键：只关菜单。
        w.menu_outside_press(25, 5, false);
        assert_eq!(menu_requests(&rx), 0);
    }

    /// 右键落在候选窗外（且不在气泡上）：不合成。
    #[test]
    fn outside_right_press_elsewhere_is_ignored() {
        let (mut w, rx) = window();
        w.window_at = |_, _, _| false;
        w.menu_outside_press(25, 5, true);
        assert_eq!(menu_requests(&rx), 0);
    }

    // ---- 悬停宽限与强制隐藏（设计 §7.6 C）----

    #[test]
    fn hover_move_table() {
        use HoverMove::*;
        // 气泡跟着候选 0：去往「无」宽限，去往候选 1 切换延迟。
        assert_eq!(hover_move(0, -1, 0), Defer(TIP_GRACE));
        assert_eq!(
            hover_move(0, TAG_PAGE_PREV, 0),
            Defer(TIP_GRACE),
            "翻页器按「无」"
        );
        assert_eq!(hover_move(0, 1, 0), Defer(TIP_SWITCH));
        // 气泡不跟着移开的那个候选（没显示 / 跟着别的）：即时。
        assert_eq!(hover_move(0, -1, -1), Now);
        assert_eq!(hover_move(0, 1, 1), Now);
        assert_eq!(hover_move(-1, 0, -1), Now);
        assert_eq!(
            hover_move(TAG_PAGE_PREV, 0, TAG_PAGE_PREV),
            Now,
            "翻页器没有气泡"
        );
        assert!(TIP_SWITCH < TIP_GRACE);
    }

    #[test]
    fn resolve_deferred_table() {
        let t0 = Instant::now();
        let until = t0 + TIP_GRACE;
        assert_eq!(resolve_deferred(t0, until, false), Deferred::Wait);
        assert_eq!(resolve_deferred(until, until, false), Deferred::Fire);
        assert_eq!(
            resolve_deferred(t0, until, true),
            Deferred::Cancel,
            "进了气泡就撤"
        );
        assert_eq!(resolve_deferred(until, until, true), Deferred::Cancel);
    }

    /// 已激活、悬停在候选 0 上且气泡跟着它。
    fn engaged_on_0() -> (CandidateMouse, Receiver<UiEvent>) {
        let (mut m, rx) = mouse(0);
        m.engaged = true;
        m.last_hover = 0;
        m.tip_for.set(0);
        (m, rx)
    }

    /// 候选 → 空隙 → 气泡：穿过空隙时不发 `Hover(-1)`；进了气泡则整个撤掉，悬停留在 0。
    #[test]
    fn gap_on_the_way_to_tip_keeps_hover() {
        let (mut m, rx) = engaged_on_0();
        real_move(&mut m, 15);
        assert!(hovers(&rx).is_empty(), "空隙里不得立刻清悬停");
        assert!(m.deferred.is_some());
        m.resolve_deferred(later(), true);
        assert!(hovers(&rx).is_empty(), "进了气泡：撤掉");
        assert!(m.deferred.is_none());
        assert_eq!(m.last_hover, 0);
    }

    /// 朝向判据：气泡在右侧顶端时，水平右移、斜着往上右移都算；往左、往下偏离得比靠近快、
    /// 原地不动都不算。气泡在下方时（收起态横排）同理换一条轴。
    #[test]
    fn heading_to_tip_table() {
        let right_top = (100, 0, 200, 40);
        assert!(
            heading_to_tip((10, 120), (14, 120), right_top),
            "沿行水平右移"
        );
        assert!(heading_to_tip((10, 120), (14, 116), right_top), "斜着往上");
        assert!(
            !heading_to_tip((10, 120), (14, 126), right_top),
            "往下偏得比靠近快"
        );
        assert!(!heading_to_tip((10, 120), (6, 120), right_top), "往回走");
        assert!(!heading_to_tip((10, 120), (10, 120), right_top), "没动");
        assert!(
            heading_to_tip((10, 20), (12, 26), right_top),
            "跨度内上下随意"
        );
        let left = (-200, 0, -100, 40);
        assert!(heading_to_tip((10, 20), (6, 20), left), "气泡在左侧：左移");
        let below = (0, 50, 80, 120);
        assert!(
            heading_to_tip((40, 10), (40, 14), below),
            "气泡在下方：下移"
        );
        assert!(!heading_to_tip((40, 10), (40, 6), below));
        assert!(
            !heading_to_tip((150, 60), (151, 60), (100, 0, 200, 100)),
            "已在气泡上"
        );
    }

    /// 宽限里朝气泡继续移动：截止顺延；背离气泡：照旧取较早者、不顺延。
    #[test]
    fn moving_toward_tip_extends_the_grace() {
        let run = |tip: (i32, i32, i32, i32)| {
            let (mut m, _rx) = engaged_on_0();
            m.tip_rect = Some(tip);
            m.tip_visible = Some(Rc::new(Cell::new(true)));
            real_move(&mut m, 5);
            real_move(&mut m, 22);
            let first = m.deferred.expect("移开气泡所属候选应延后");
            std::thread::sleep(Duration::from_millis(2));
            real_move(&mut m, 25);
            (first, m.deferred.unwrap())
        };
        let (first, after) = run((100, 0, 200, 40));
        assert!(after > first, "朝右侧气泡移动：顺延");
        let (first, after) = run((-200, 0, -100, 40));
        assert_eq!(after, first, "背离气泡：不顺延");
    }

    /// 指针停在格子上左右抖动：一来一回刷不出离气泡的新低，截止不再往后推。
    #[test]
    fn jitter_does_not_keep_extending() {
        let (mut m, _rx) = engaged_on_0();
        m.tip_rect = Some((100, 0, 200, 40));
        m.tip_visible = Some(Rc::new(Cell::new(true)));
        real_move(&mut m, 5);
        real_move(&mut m, 25);
        real_move(&mut m, 26);
        let settled = m.deferred.unwrap();
        for x in [25, 26, 25, 26] {
            std::thread::sleep(Duration::from_millis(2));
            real_move(&mut m, x);
        }
        assert_eq!(m.deferred.unwrap(), settled, "抖动不顺延");
    }

    /// 气泡已自己收起（`tip_rect` 还是旧值）：不按它判朝向。
    #[test]
    fn hidden_tip_does_not_extend() {
        let (mut m, _rx) = engaged_on_0();
        m.tip_rect = Some((100, 0, 200, 40));
        m.tip_visible = Some(Rc::new(Cell::new(false)));
        real_move(&mut m, 5);
        real_move(&mut m, 22);
        let first = m.deferred.unwrap();
        std::thread::sleep(Duration::from_millis(2));
        real_move(&mut m, 25);
        assert_eq!(m.deferred.unwrap(), first);
    }

    /// 宽限到期仍不在气泡上（也没回来）：照发。
    #[test]
    fn gap_fires_after_grace() {
        let (mut m, rx) = engaged_on_0();
        real_move(&mut m, 15);
        m.resolve_deferred(Instant::now(), false);
        assert!(hovers(&rx).is_empty(), "未到期不发");
        m.resolve_deferred(later(), false);
        assert_eq!(hovers(&rx), vec![-1]);
        assert_eq!(m.last_hover, -1);
    }

    /// 宽限期内回到原候选：待发作废，到期也不发。
    #[test]
    fn return_to_same_candidate_cancels() {
        let (mut m, rx) = engaged_on_0();
        real_move(&mut m, 15);
        real_move(&mut m, 5);
        assert!(m.deferred.is_none());
        m.resolve_deferred(later(), false);
        assert!(hovers(&rx).is_empty());
    }

    /// 擦过相邻候选：不立刻切换，切换延迟到期才发；期间进了气泡则不切。
    #[test]
    fn crossing_neighbour_defers_switch() {
        let (mut m, rx) = engaged_on_0();
        real_move(&mut m, 25);
        assert!(hovers(&rx).is_empty(), "擦过候选 1 不立刻切");
        let until = m.deferred.expect("延后");
        assert!(until <= Instant::now() + TIP_SWITCH);
        m.resolve_deferred(until, false);
        assert_eq!(hovers(&rx), vec![1]);
        // 进了气泡的那种。
        let (mut m, rx) = engaged_on_0();
        real_move(&mut m, 25);
        m.resolve_deferred(later(), true);
        assert!(hovers(&rx).is_empty());
    }

    /// 先进空隙（宽限）、再到另一个候选：按较早的切换延迟收紧，不顺延。
    #[test]
    fn switch_after_gap_tightens_deadline() {
        let (mut m, _rx) = engaged_on_0();
        real_move(&mut m, 15);
        let grace = m.deferred.unwrap();
        real_move(&mut m, 25);
        assert!(m.deferred.unwrap() < grace);
    }

    /// 气泡没跟着移开的候选：悬停照旧即时。
    #[test]
    fn no_tip_no_grace() {
        let (mut m, rx) = engaged_on_0();
        m.tip_for.set(-1);
        real_move(&mut m, 15);
        assert_eq!(hovers(&rx), vec![-1]);
        assert!(m.deferred.is_none());
    }

    /// 窗口层：tick 在光标进了气泡时撤掉待发；到期时间进入 next_deadline。
    #[test]
    fn window_tick_cancels_when_cursor_on_tip() {
        let (mut w, rx) = window();
        assert_eq!(w.tip_for.get(), 0, "显示气泡即记下它跟着谁");
        {
            let mut m = w.mouse.borrow_mut();
            m.engaged = true;
            m.last_hover = 0;
            real_move(&mut m, 15);
        }
        let _ = hovers(&rx);
        let until = w.mouse.borrow().deferred.expect("延后");
        assert_eq!(w.next_deadline(), Some(until));
        w.tooltip_mut().unwrap().stub_on_tip(true);
        w.tick();
        assert!(w.mouse.borrow().deferred.is_none());
        assert!(hovers(&rx).is_empty());
    }

    /// Esc / 上屏 / 失焦（候选窗收起）：光标就停在气泡上、跟踪挂着，气泡也得收；待发的
    /// 悬停与「气泡跟着谁」一并清掉。
    #[test]
    fn candidate_hide_force_hides_tip_under_cursor() {
        let (mut w, _rx) = window();
        {
            let t = w.tooltip_mut().unwrap();
            t.stub_on_tip(true);
            t.simulate_move();
        }
        w.mouse.borrow_mut().deferred = Some(later());
        w.hide();
        assert!(!tip_shown(&w), "候选窗收起，气泡不得残留");
        assert_eq!(w.tip_for.get(), -1);
        assert!(w.mouse.borrow().deferred.is_none());
    }

    /// 悬停变成「无」（协调器重绘）：`tip_for` 随之清掉，不再对后续移动延后。
    #[test]
    fn tip_for_clears_when_tip_goes_away() {
        let (mut w, _rx) = window();
        w.hover = -1;
        w.update_tooltip(0, 0);
        assert_eq!(w.tip_for.get(), -1);
    }
}

// 直立态逐格切叶子时，分段颜色跟着格走（设计 text-span-colors.md §7.1）。
#[cfg(test)]
mod upright_color_run_tests {
    use super::*;
    use crate::text::dwrite::ColorRun;

    fn window(upright: bool) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.set_orientation(false, upright, upright);
        w
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    /// 「a你b」：红 `a你`（0..4）、蓝 `b`（4..5）。
    fn runs() -> Vec<ColorRun> {
        vec![
            ColorRun {
                start: 0,
                end: 4,
                rgba: RED,
            },
            ColorRun {
                start: 4,
                end: 5,
                rgba: BLUE,
            },
        ]
    }

    fn build(w: &CandidateWindow) -> View {
        let runs = runs();
        w.scene_input(false)
            .upright_text("a你b", None, |seg, _, off| {
                View::leaf(seg, [0, 0, 0, 255]).color_runs(ColorRun::slice(
                    &runs,
                    off,
                    off + seg.len(),
                ))
            })
    }

    /// 不切格：偏移为 0，一个叶子拿全部区间，原样。
    #[test]
    fn unsplit_leaf_keeps_all_runs() {
        let v = build(&window(false));
        assert_eq!(v.text.as_deref(), Some("a你b"));
        assert_eq!(v.color_runs, runs());
    }

    /// 直立态逐格切：每格拿到自己范围内的那段、平移到格内偏移。
    #[test]
    fn upright_cells_get_their_own_slice() {
        let v = build(&window(true));
        // 每格是扶正包裹层（旋转节点）里的一个叶子。
        let cells: Vec<(&str, &[ColorRun])> = v
            .children
            .iter()
            .map(|c| {
                let leaf = &c.children[0];
                (leaf.text.as_deref().unwrap(), leaf.color_runs.as_slice())
            })
            .collect();
        let one = |end, rgba| {
            vec![ColorRun {
                start: 0,
                end,
                rgba,
            }]
        };
        assert_eq!(
            cells,
            vec![
                ("a", one(1, RED).as_slice()),
                ("你", one(3, RED).as_slice()),
                ("b", one(1, BLUE).as_slice()),
            ]
        );
    }
}

// 注释的分段颜色：按主题与候选状态求色，接到注释叶子的 color_runs（设计 text-span-colors.md §6）。
#[cfg(test)]
mod comment_color_tests {
    use super::*;
    use wind_ui_types::{SpanStyle, StyledText};

    const RED: [u8; 4] = [0xC0, 0, 0, 255];

    /// 「wq(nǐ)」：`wq` 带内联色 `#C00000`（选中态改用 `selected=` 给的色），其余无色。
    fn comment(spec: &str) -> StyledText {
        let mut t = StyledText::new();
        t.push(
            "wq",
            &SpanStyle {
                role: Some("code_rev"),
                color: Some(std::sync::Arc::new(wind_theme::InlineColor::parse(spec))),
                ..Default::default()
            },
        );
        t.push(
            "(nǐ)",
            &SpanStyle {
                role: Some("literal"),
                ..Default::default()
            },
        );
        t
    }

    fn window(theme: wind_theme::Resolved, upright: bool) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_theme(theme);
        w.set_orientation(false, upright, upright);
        w
    }

    fn cand(c: StyledText) -> CandidateItem {
        CandidateItem {
            text_styled: Default::default(),
            text: "你".into(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: c,
            comment_above: Default::default(),
            no_index: false,
        }
    }

    /// 树里所有带颜色区间的叶子：`(文字, 区间)`。
    fn colored(v: &View, out: &mut Vec<(String, Vec<ColorRun>)>) {
        if let Some(t) = &v.text
            && !v.color_runs.is_empty()
        {
            out.push((t.clone(), v.color_runs.clone()));
        }
        for c in &v.children {
            colored(c, out);
        }
    }

    fn runs_of(
        w: &mut CandidateWindow,
        comment_spec: &str,
        selected: usize,
    ) -> Vec<(String, Vec<ColorRun>)> {
        w.update(
            "ni",
            2,
            "",
            vec![cand(comment(comment_spec))],
            selected,
            -1,
            1,
            1,
        );
        let mut out = Vec::new();
        colored(&w.scene.build_tree(&w.scene_input(false)), &mut out);
        out
    }

    fn run(start: u32, end: u32, rgba: [u8; 4]) -> ColorRun {
        ColorRun { start, end, rgba }
    }

    #[test]
    fn inline_color_reaches_the_comment_leaf() {
        let mut w = window(wind_theme::Resolved::default(), false);
        // 候选 0 不是选中（selected 越界）⇒ 常态。
        assert_eq!(
            runs_of(&mut w, "#C00000", 9),
            vec![("wq(nǐ)".to_string(), vec![run(0, 2, RED)])]
        );
    }

    /// 规则 3：主题把选中态注释正文色改了 ⇒ 内联色回落该态正文色（区间被丢）；
    /// `selected=` 单列的照用。
    #[test]
    fn selected_state_falls_back_unless_inline_says_selected() {
        let mut theme = wind_theme::Resolved::default();
        theme.views.comment.selected = Some(Box::new(wind_theme::RvNode {
            text_color: Some([255, 255, 255, 255]),
            ..Default::default()
        }));
        let mut w = window(theme, false);
        assert_eq!(runs_of(&mut w, "#C00000", 0), vec![], "选中态回落正文色");
        assert_eq!(
            runs_of(&mut w, "#C00000,selected=#00FF00", 0),
            vec![("wq(nǐ)".to_string(), vec![run(0, 2, [0, 255, 0, 255])])]
        );
    }

    /// 非出厂测试主题 `span-roles`（wind-theme testdata）：主题角色色真的接到注释叶子，
    /// 且选中 / 悬停态按 §6.3 回落。
    fn roles_theme() -> wind_theme::Resolved {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        wind_theme::load_resolved_dirs(
            &[
                root.join("../wind-theme/testdata/themes"),
                root.join("../../../data/themes"),
            ],
            "span-roles",
            false,
        )
        .unwrap()
    }

    /// 「wq nǐ」：`wq` 角色 code_rev、` ` 字面、`nǐ` 角色 pinyin。
    fn roles_comment() -> StyledText {
        let mut t = StyledText::new();
        t.push(
            "wq",
            &SpanStyle {
                role: Some("code_rev"),
                ..Default::default()
            },
        );
        t.push(
            " ",
            &SpanStyle {
                role: Some("literal"),
                ..Default::default()
            },
        );
        t.push(
            "nǐ",
            &SpanStyle {
                role: Some("pinyin"),
                ..Default::default()
            },
        );
        t
    }

    fn roles_runs(w: &mut CandidateWindow, selected: usize, hover: i32) -> Vec<ColorRun> {
        w.update(
            "ni",
            2,
            "",
            vec![cand(roles_comment())],
            selected,
            hover,
            1,
            1,
        );
        let mut out = Vec::new();
        colored(&w.scene.build_tree(&w.scene_input(false)), &mut out);
        out.into_iter().flat_map(|(_, r)| r).collect()
    }

    #[test]
    fn theme_roles_color_the_comment_per_state() {
        let mut w = window(roles_theme(), false);
        const CODE: [u8; 4] = [0xC0, 0, 0, 255];
        const LIT: [u8; 4] = [0xB0, 0xB0, 0xB0, 255];
        const PY: [u8; 4] = [0, 0x80, 0, 255];
        // 常态：三个角色各自的色。
        assert_eq!(
            roles_runs(&mut w, 9, -1),
            vec![run(0, 2, CODE), run(2, 3, LIT), run(3, 6, PY)]
        );
        // 选中：主题改了选中态正文色 ⇒ 未单列的回落正文色（区间被丢）；code_rev 单列保留。
        assert_eq!(
            roles_runs(&mut w, 0, -1),
            vec![run(0, 2, [0xFF, 0xE0, 0x8A, 255])]
        );
        // 悬停：正文色没改 ⇒ 常态角色色照用；pinyin 在悬停态单列。
        assert_eq!(
            roles_runs(&mut w, 9, 0),
            vec![
                run(0, 2, CODE),
                run(2, 3, LIT),
                run(3, 6, [0, 0, 0xFF, 255])
            ]
        );
    }

    /// 直立态逐格切：`w`、`q` 两格各得一段，其余格没有区间。
    #[test]
    fn upright_cells_get_their_slices() {
        let mut w = window(wind_theme::Resolved::default(), true);
        assert_eq!(
            runs_of(&mut w, "#C00000", 9),
            vec![
                ("w".to_string(), vec![run(0, 1, RED)]),
                ("q".to_string(), vec![run(0, 1, RED)]),
            ]
        );
    }
}

// 上方注释条（`CandidateItem.comment_above`，设计 candidate-comment-above-line.md §3.3）。
//
// ⚠️ host 上文本测量是 mock 近似（字宽 0.6em、行高只看字号），下面凡涉及尺寸的断言都写成
// 相对关系（等高、同一 x、差值 == 某叶子外框），不钉像素——它们测的是**盒模型结构**，
// 真实后端下结构不变、关系照样成立。唯一只在 mock 下成立的前提见各测试注释。
#[cfg(test)]
mod comment_above_tests {
    use super::*;
    use wind_ui_types::{SpanStyle, StyledText};

    fn cand_above(text: &str, above: &str, comment: &str) -> CandidateItem {
        CandidateItem {
            text_styled: Default::default(),
            text: text.to_string(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: comment.into(),
            comment_above: above.into(),
            no_index: false,
        }
    }

    fn mk(vertical: bool, rotated: bool, upright: bool, rows: u32) -> CandidateWindow {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(vertical, rotated, upright);
        w.set_min_size(0, 0, 0, 0, rows);
        w
    }

    fn win(vertical: bool, items: Vec<CandidateItem>) -> CandidateWindow {
        let mut w = mk(vertical, false, false, 0);
        w.update("", 0, "", items, 0, -1, 1, 1);
        w
    }

    fn laid(w: &CandidateWindow) -> View {
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        root
    }

    fn find_list(v: &View) -> Option<&View> {
        if v.children.iter().any(|c| c.tag >= 0) {
            return Some(v);
        }
        v.children.iter().find_map(find_list)
    }

    /// 候选 item（按 tag 升序）。
    fn items(root: &View) -> Vec<&View> {
        let list = find_list(root).expect("未找到候选列表容器");
        let mut out: Vec<&View> = list.children.iter().filter(|c| c.tag >= 0).collect();
        out.sort_by_key(|c| c.tag);
        out
    }

    fn all_leaves<'a>(v: &'a View, out: &mut Vec<&'a View>) {
        if v.text.is_some() {
            out.push(v);
        }
        for c in &v.children {
            all_leaves(c, out);
        }
    }

    /// 带上方条的 item：`(上方条叶子, 主行)`。
    fn split_item(item: &View) -> (&View, &View) {
        assert!(
            matches!(item.layout, Layout::Column),
            "带上方条的 item 应是 Column"
        );
        assert_eq!(item.children.len(), 2, "Column[上方条, 主行]");
        (&item.children[0], &item.children[1])
    }

    /// 有上段 vs 无上段：item 恰好高出上方条叶子的外框高度（主行不变）。
    #[test]
    fn above_line_adds_one_row_height() {
        for vertical in [true, false] {
            let plain = laid(&win(vertical, vec![cand_above("你好", "", "wq")]));
            let with = laid(&win(vertical, vec![cand_above("你好", "ni hao", "wq")]));
            let h0 = items(&plain)[0].laid_rect().h;
            let item = items(&with)[0];
            let (above, _) = split_item(item);
            let ar = above.laid_rect();
            let above_box = ar.h + above.margin.t + above.margin.b;
            assert!(above_box > 0.0);
            assert!(
                (item.laid_rect().h - h0 - above_box).abs() < 0.5,
                "vertical={vertical}：item 应恰好高出一条上方条（{} vs {h0} + {above_box}）",
                item.laid_rect().h
            );
        }
    }

    /// 按页预留：3 条里只有第 2 条有上段，三个 item 等高，且都高于「整页无上段」。
    #[test]
    fn above_line_reserves_height_page_wide() {
        for vertical in [true, false] {
            let root = laid(&win(
                vertical,
                vec![
                    cand_above("一", "", "a"),
                    cand_above("二", "er", "b"),
                    cand_above("三", "", ""),
                ],
            ));
            let hs: Vec<f32> = items(&root).iter().map(|i| i.laid_rect().h).collect();
            assert_eq!(hs.len(), 3);
            assert!(
                hs.iter().all(|h| (h - hs[1]).abs() < 0.01),
                "vertical={vertical}：整页 item 须等高（{hs:?}）"
            );
            let plain = laid(&win(vertical, vec![cand_above("一", "", "a")]));
            assert!(
                hs[0] > items(&plain)[0].laid_rect().h,
                "无上段的候选也要预留"
            );
            // 无上段的候选放的是透明占位叶子，看不见。
            let (ghost, _) = split_item(items(&root)[0]);
            assert_eq!(ghost.text_color[3], 0, "占位叶子须透明");
        }
    }

    /// 上方条字形起点 == 候选文字字形起点（≥ 序号右缘），含圆圈序号与无序号行。
    #[test]
    fn above_line_starts_at_text_origin() {
        for circle in [false, true] {
            for vertical in [true, false] {
                let mut w = mk(vertical, false, false, 0);
                if circle {
                    w.theme.views.index.bg_shape = "circle".to_string();
                }
                let mut no_idx = cand_above("丙", "bing", "");
                no_idx.no_index = true;
                w.update(
                    "",
                    0,
                    "",
                    vec![
                        cand_above("你好", "ni hao", "wq"),
                        cand_above("甲", "jia", ""),
                        no_idx,
                    ],
                    0,
                    -1,
                    1,
                    1,
                );
                let root = laid(&w);
                for item in items(&root) {
                    let (above, main) = split_item(item);
                    let text = if item.tag == 2 {
                        &main.children[0]
                    } else {
                        let idx = &main.children[0];
                        let text = &main.children[1];
                        let ir = idx.laid_rect();
                        assert!(
                            above.laid_rect().x + 0.01 >= ir.x + ir.w,
                            "上方条不得压到序号上（circle={circle}, vertical={vertical}）"
                        );
                        text
                    };
                    let a0 = above.laid_rect().x + above.padding.l;
                    let t0 = text.laid_rect().x + text.padding.l;
                    assert!(
                        (a0 - t0).abs() < 1.0,
                        "上方条起点 {a0} 应对齐文字起点 {t0}（tag={}, circle={circle}, vertical={vertical}）",
                        item.tag
                    );
                }
            }
        }
    }

    /// 整页无上段：item 保持原来的 Row[序号, 文字, 注释]，没有任何 Column / 占位叶子。
    /// （逐字节不变由 `render_golden` 的出厂对拍钉死，这条是本模块内的结构兜底。）
    #[test]
    fn no_above_keeps_tree_identical() {
        for vertical in [true, false] {
            let root = laid(&win(
                vertical,
                vec![cand_above("一", "", "a"), cand_above("二", "", "")],
            ));
            let its = items(&root);
            assert!(its.iter().all(|i| matches!(i.layout, Layout::Row)));
            assert_eq!(its[0].children.len(), 3, "序号 + 文字 + 注释");
            assert_eq!(its[1].children.len(), 2, "序号 + 文字");
        }
    }

    /// 上段极长：以 `…` 结尾，窗口不超过屏幕安全宽；文字本身不被上段挤截。
    #[test]
    fn above_line_overlong_is_ellipsized() {
        let long = "x".repeat(2000);
        for vertical in [true, false] {
            let w = win(
                vertical,
                vec![
                    cand_above("你好", &long, "wq"),
                    cand_above("世界", &long, ""),
                ],
            );
            let root = laid(&w);
            let cap = w.screen_safety_max_width_px() as f32;
            assert!(
                root.measured_size().0 <= cap + 0.5,
                "vertical={vertical}：窗口宽 {} 超出上限 {cap}",
                root.measured_size().0
            );
            for item in items(&root) {
                let (above, main) = split_item(item);
                let t = above.text.as_deref().unwrap();
                assert!(t.ends_with('…'), "上段应被截断加省略号");
                assert!(t.len() < long.len());
                assert_eq!(
                    main.children[1].text.as_deref(),
                    Some(if item.tag == 0 { "你好" } else { "世界" }),
                    "候选文字不因上段而被截"
                );
            }
        }
    }

    /// 旋转 / 直立态不支持上方条：无 Column，上段以 `\n` 合回右侧注释。
    #[test]
    fn upright_merges_above_into_comment() {
        for upright in [false, true] {
            let mut w = mk(false, true, upright, 0);
            w.update(
                "",
                0,
                "",
                vec![cand_above("你", "ni", "亻尔"), cand_above("好", "hao", "")],
                0,
                -1,
                1,
                1,
            );
            let root = laid(&w);
            for item in items(&root) {
                assert!(
                    matches!(item.layout, Layout::Row),
                    "upright={upright}：不应出现 Column"
                );
            }
            let mut leaves = Vec::new();
            all_leaves(&root, &mut leaves);
            let texts: Vec<&str> = leaves.iter().filter_map(|l| l.text.as_deref()).collect();
            if upright {
                // 直立态逐格切：上段的每个字都在。
                for ch in ["n", "i", "亻", "尔", "h", "a", "o"] {
                    assert!(texts.contains(&ch), "直立态缺 {ch}：{texts:?}");
                }
            } else {
                assert!(
                    texts.contains(&"ni\n亻尔"),
                    "上下段应以 \\n 合并：{texts:?}"
                );
                assert!(texts.contains(&"hao"), "下段空时只剩上段：{texts:?}");
            }
        }
    }

    /// `min_rows` 补的占位行与带上方条的真实行同构、等高。
    #[test]
    fn placeholder_rows_match_real_rows_when_above() {
        let five = |n: usize| {
            let mut w = mk(true, false, false, 5);
            w.theme.views.index.bg_shape = "circle".to_string();
            let texts = ["一", "二", "三", "四", "五"];
            let items: Vec<CandidateItem> = texts[..n]
                .iter()
                .enumerate()
                .map(|(i, t)| cand_above(t, if i == 1 { "er" } else { "" }, ""))
                .collect();
            w.update("", 0, "", items, 0, -1, 1, 1);
            w
        };
        let full = laid(&five(5)).measured_size().1;
        let padded_w = five(3);
        let padded = laid(&padded_w);
        assert_eq!(
            padded.measured_size().1,
            full,
            "3 候选补 2 空行须与 5 条等高"
        );
        let list = find_list(&padded).unwrap();
        let ph = list.children.iter().find(|c| c.tag < 0).expect("占位行");
        let real = list.children.iter().find(|c| c.tag == 1).unwrap();
        let (ph_above, ph_main) = split_item(ph);
        let (real_above, real_main) = split_item(real);
        assert_eq!(ph_above.font_size, real_above.font_size);
        assert_eq!(ph_above.font_family, real_above.font_family);
        assert_eq!(ph_above.font_weight, real_above.font_weight);
        let tb = |e: Edges| (e.t, e.b);
        assert_eq!(tb(ph_above.padding), tb(real_above.padding));
        assert_eq!(tb(ph_above.margin), tb(real_above.margin));
        assert_eq!(ph_main.children.len(), real_main.children.len());
        assert!((ph.laid_rect().h - real.laid_rect().h).abs() < 0.01);
    }

    /// `[comment_above.roles]` 只作用于上方条，右侧注释不受影响。
    #[test]
    fn above_line_colors_follow_comment_above_theme() {
        const GREEN: [u8; 4] = [0, 0x80, 0, 255];
        let py = |s: &str| {
            let mut t = StyledText::new();
            t.push(
                s,
                &SpanStyle {
                    role: Some("pinyin"),
                    ..Default::default()
                },
            );
            t
        };
        let mut w = mk(true, false, false, 0);
        w.theme
            .views
            .comment_above
            .roles
            .insert("pinyin".to_string(), GREEN);
        let mut c = cand_above("你", "", "");
        c.comment_above = py("ni");
        c.comment = py("nn");
        w.update("", 0, "", vec![c], 9, -1, 1, 1);
        let root = laid(&w);
        let (above, main) = split_item(items(&root)[0]);
        assert_eq!(
            above.color_runs,
            vec![ColorRun {
                start: 0,
                end: 2,
                rgba: GREEN
            }]
        );
        let cmt = &main.children[2];
        assert_eq!(cmt.text.as_deref(), Some("nn"));
        assert!(cmt.color_runs.is_empty(), "右侧注释不该吃到上方条的角色色");
    }

    /// 纵向中线。
    fn mid_y(v: &View) -> f32 {
        let r = v.laid_rect();
        r.y + r.h * 0.5
    }

    /// 横排：翻页栏、模式徽标、内联编码这些一行高的非 item 成员与候选**主行**对齐，而不是
    /// 与两行高的整个 item 居中（那样会比主行高出半个上方条）。
    #[test]
    fn horizontal_single_line_members_align_with_main_row() {
        // (preedit, 模式标记, 内联编码)：分别让徽标 / 内联编码落进候选行。
        for (preedit, mode, embedded) in [("", "拼", false), ("nihao", "", true)] {
            let mut w = mk(false, false, false, 0);
            w.set_preedit_embedded(embedded);
            w.update(
                preedit,
                preedit.len(),
                mode,
                vec![cand_above("好", "hǎo", "vb"), cand_above("浩", "", "")],
                0,
                -1,
                1,
                2,
            );
            let root = laid(&w);
            let list = find_list(&root).unwrap();
            let (_, main) = split_item(items(&root)[0]);
            let want = mid_y(main);
            let members: Vec<&View> = list.children.iter().filter(|c| c.tag < 0).collect();
            assert!(
                members.len() >= 2,
                "应有翻页栏 + 徽标/内联编码：{}",
                members.len()
            );
            for m in members {
                assert!(
                    (mid_y(m) - want).abs() <= 1.0,
                    "非 item 成员中线 {} 应与主行中线 {want} 对齐（preedit={preedit:?} mode={mode:?}）：{:?}",
                    mid_y(m),
                    m.text
                );
            }
        }
    }

    /// 点击上方条所在的区域命中该候选（上方条在 item 容器之内，命中区是整个 item）。
    #[test]
    fn click_on_above_line_hits_its_candidate() {
        for vertical in [true, false] {
            let root = laid(&win(
                vertical,
                vec![cand_above("好", "hǎo", ""), cand_above("号", "hào", "")],
            ));
            let mut hits = Vec::new();
            root.collect_hits(&mut hits);
            for item in items(&root) {
                let (above, _) = split_item(item);
                let r = above.laid_rect();
                let (x, y) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
                let tag = hits
                    .iter()
                    .find(|(_, hr)| hr.contains(x, y))
                    .map_or(-1, |(t, _)| *t);
                assert_eq!(tag, item.tag, "vertical={vertical}：上方条应命中本候选");
            }
        }
    }

    /// `[comment_above.hover.roles]`：悬停态的角色色接到上方条。
    #[test]
    fn above_line_hover_role_color() {
        const GREEN: [u8; 4] = [0, 0x80, 0, 255];
        const BLUE: [u8; 4] = [0, 0, 0xFF, 255];
        let mut w = mk(true, false, false, 0);
        let ca = &mut w.theme.views.comment_above;
        ca.roles.insert("pinyin".to_string(), GREEN);
        let mut hov = wind_theme::RvNode::default();
        hov.roles.insert("pinyin".to_string(), BLUE);
        ca.hover = Some(Box::new(hov));
        let mut c = cand_above("你", "", "");
        let mut t = StyledText::new();
        t.push(
            "ni",
            &SpanStyle {
                role: Some("pinyin"),
                ..Default::default()
            },
        );
        c.comment_above = t;
        let runs = |w: &mut CandidateWindow, hover: i32| {
            w.update("", 0, "", vec![c.clone()], 9, hover, 1, 1);
            let root = laid(w);
            let (above, _) = split_item(items(&root)[0]);
            above.color_runs.clone()
        };
        let rgba = |r: Vec<ColorRun>| r.iter().map(|r| r.rgba).collect::<Vec<_>>();
        assert_eq!(rgba(runs(&mut w, -1)), vec![GREEN], "常态");
        assert_eq!(rgba(runs(&mut w, 0)), vec![BLUE], "悬停");
    }

    /// `[comment_above]` 的背景 / 边框接到上方条叶子（含选中态），右侧注释不受影响；
    /// 占位叶子不画装饰。
    #[test]
    fn above_line_consumes_background_and_border() {
        const BG: [u8; 4] = [1, 2, 3, 255];
        const SEL_BG: [u8; 4] = [4, 5, 6, 255];
        const BORDER: [u8; 4] = [7, 8, 9, 255];
        let mut w = mk(true, false, false, 0);
        let ca = &mut w.theme.views.comment_above;
        ca.bg_color = Some(BG);
        ca.border_color = Some(BORDER);
        ca.selected = Some(Box::new(wind_theme::RvNode {
            bg_color: Some(SEL_BG),
            ..Default::default()
        }));
        w.update(
            "",
            0,
            "",
            vec![
                cand_above("好", "hǎo", "vb"),
                cand_above("号", "hào", "kg"),
                cand_above("浩", "", "hj"),
            ],
            0,
            -1,
            1,
            1,
        );
        let root = laid(&w);
        let its = items(&root);
        let (sel_above, sel_main) = split_item(its[0]);
        let (above, _) = split_item(its[1]);
        let (ghost, _) = split_item(its[2]);
        assert_eq!(sel_above.bg, Some(SEL_BG), "选中态取 selected patch 的底色");
        assert_eq!(above.bg, Some(BG));
        assert_eq!(above.border.map(|b| b.0), Some(BORDER));
        assert_eq!(sel_main.children[2].bg, None, "右侧注释不吃上方条的底色");
        assert_eq!(sel_main.children[2].border, None);
        assert_eq!(ghost.bg, None, "占位叶子不画底色");
        assert_eq!(ghost.border, None);
    }
}

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
