//! 语言栏图标离屏渲染（Windows TSF 输入指示器那个 16×16 图标）
//!
//! 服务端把当前状态渲染成多档位图写进共享内存，`wind_tsf.dll` 的 `GetIcon` 直接取用。
//! 设计与取舍见 `docs/design/langbar-icon-shared-render.md`。
//!
//! [`crate::text::dwrite`] 后端假设目标缓冲区是已含背景的预乘 alpha，在全透明缓冲上画字会得到全透明，
//! 故分层：黑底画白字取亮度当主字蒙版，角标另行几何绘制，各自着色后合成（也因此主字与角标可各自取色）。
//!
//! 输出非预乘 BGRA，即 `CreateIconIndirect` 的 32bpp `hbmColor` 约定。

use crate::text::dwrite::TextRenderer;

/// 标点角标要表达的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunctBadge {
    /// 不画角标（功能关闭，或英文模式下标点不可切换）
    None,
    /// 中文标点
    Chinese,
    /// 英文标点
    English,
}

/// 角标总开关：只有「不画」与「角标」两档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BadgeStyle {
    /// 不画任何角标，图标只有主字。
    /// 做成枚举一员而非另开 `enabled` 布尔，类型上排除「关着却配了规则」的矛盾态；它也是用户可见的总开关。
    /// 默认值：不替所有人改变任务栏图标的样子。
    #[default]
    None,
    /// 按规则表在四角画直角三角（16px 下比圆点醒目，斜边朝内与主字接触面小）。
    Corner,
}

impl BadgeStyle {
    /// 全部档位，顺序即菜单里的顺序，也是 [`Self::index`] 的编号依据。
    /// 菜单项、勾选态还原、`MenuCmd` 的 u8 参数都从这里取。
    pub const ALL: [BadgeStyle; 2] = [BadgeStyle::None, BadgeStyle::Corner];

    /// 菜单文案。
    pub fn label(self) -> &'static str {
        match self {
            BadgeStyle::None => "不显示",
            BadgeStyle::Corner => "角标",
        }
    }

    /// 落盘用的稳定 id。不存 [`Self::index`]：下标绑死声明顺序。
    pub fn as_id(self) -> &'static str {
        match self {
            BadgeStyle::None => "none",
            BadgeStyle::Corner => "corner",
        }
    }

    /// 由稳定 id 还原；未知（含空串）回落到默认「不显示」。
    /// 上一版实验性的形状 id（`corner_triangle` / `outer_ring` …）不做兼容别名。
    pub fn from_id(id: &str) -> BadgeStyle {
        Self::ALL
            .iter()
            .find(|s| s.as_id() == id)
            .copied()
            .unwrap_or_default()
    }

    /// 在 [`Self::ALL`] 中的下标，用作菜单命令参数（仅进程内有效，勿落盘）。
    pub fn index(self) -> u8 {
        Self::ALL.iter().position(|&s| s == self).unwrap_or(0) as u8
    }

    /// 由下标还原；越界回落默认（菜单 id 来自另一个进程，不能假定合法）。
    pub fn from_index(i: u8) -> BadgeStyle {
        Self::ALL.get(i as usize).copied().unwrap_or_default()
    }
}

/// 一条角标规则所绑定的状态。
/// 半角与英文态不在其中：没有信息量的状态不占像素（半角是常态，英文态主字已是「英」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeState {
    /// 中文标点态。
    PunctCn,
    /// 英文标点态。
    PunctEn,
    /// 全角态。
    FullWidth,
}

impl BadgeState {
    /// 全部状态，顺序即设置页下拉里的顺序。
    pub const ALL: [BadgeState; 3] = [
        BadgeState::PunctCn,
        BadgeState::PunctEn,
        BadgeState::FullWidth,
    ];

    /// 面向用户的名称。
    pub fn label(self) -> &'static str {
        match self {
            BadgeState::PunctCn => "中文标点",
            BadgeState::PunctEn => "英文标点",
            BadgeState::FullWidth => "全角",
        }
    }

    /// 落盘用的稳定 id。
    pub fn as_id(self) -> &'static str {
        match self {
            BadgeState::PunctCn => "punct_cn",
            BadgeState::PunctEn => "punct_en",
            BadgeState::FullWidth => "full_width",
        }
    }

    /// 由稳定 id 还原；未知返回 `None`。
    /// 与 [`BadgeStyle::from_id`] / [`Corner::from_id`] 不同，状态没有合理默认，回落任何一个都是瞎猜，
    /// 故整条规则丢弃（调用方负责记警告）。
    pub fn from_id(id: &str) -> Option<BadgeState> {
        Self::ALL.iter().find(|s| s.as_id() == id).copied()
    }

    /// 当前状态是否命中本条规则。
    fn matches(self, spec: &IconSpec) -> bool {
        match self {
            BadgeState::PunctCn => spec.punct == PunctBadge::Chinese,
            BadgeState::PunctEn => spec.punct == PunctBadge::English,
            BadgeState::FullWidth => spec.full_width,
        }
    }
}

/// 角标所在的角落。
///
/// 四个角落形状同构，只差坐标折叠——见 [`draw_corner_triangle`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Corner {
    /// 左上角。
    TopLeft,
    /// 右上角（出厂全角规则的落点）。
    TopRight,
    /// 右下角（出厂两条标点规则的落点，也是未知值的回落目标）。
    #[default]
    BottomRight,
    /// 左下角。
    BottomLeft,
}

impl Corner {
    /// 全部角落，顺序即设置页下拉里的顺序（顺时针从左上起）。
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomRight,
        Corner::BottomLeft,
    ];

    /// 面向用户的名称。
    pub fn label(self) -> &'static str {
        match self {
            Corner::TopLeft => "左上角",
            Corner::TopRight => "右上角",
            Corner::BottomRight => "右下角",
            Corner::BottomLeft => "左下角",
        }
    }

    /// 落盘用的稳定 id。
    pub fn as_id(self) -> &'static str {
        match self {
            Corner::TopLeft => "top_left",
            Corner::TopRight => "top_right",
            Corner::BottomRight => "bottom_right",
            Corner::BottomLeft => "bottom_left",
        }
    }

    /// 由稳定 id 还原；未知回落右下角（最不挡主字、也是出厂标点规则的落点），写错一个词不至于丢整条规则。
    pub fn from_id(id: &str) -> Corner {
        Self::ALL
            .iter()
            .find(|c| c.as_id() == id)
            .copied()
            .unwrap_or_default()
    }

    /// 水平方向是否靠右。
    fn is_right(self) -> bool {
        matches!(self, Corner::TopRight | Corner::BottomRight)
    }

    /// 垂直方向是否靠下。
    fn is_bottom(self) -> bool {
        matches!(self, Corner::BottomLeft | Corner::BottomRight)
    }
}

/// 一条规则在某个主题下的着色：色相 + 不透明度，两者都可以「不指定」。
/// 配置侧是 `color_x` + `alpha_x` 两键，由协调器的 `badge_color_of` 合成本结构。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BadgeColor {
    /// 色相（BGR）。`None` = 与主字同色并跟随明暗主题（配置里的 `""`，旧写法 `auto`）。
    pub rgb: Option<[u8; 3]>,
    /// 本条自己的不透明度（0~1）。`None` = 用全局 [`IconRenderer::badge_alpha`]。
    /// `Some(1.0)` 与 `None` 不同：前者会把这一条切到挖空档，见 [`IconRenderer::badge_alpha`]。
    pub alpha: Option<f32>,
}

impl BadgeColor {
    /// 与主字同色、不透明度跟随全局。
    pub const AUTO: BadgeColor = BadgeColor {
        rgb: None,
        alpha: None,
    };

    /// 指定色相、不透明度跟随全局。
    pub fn rgb(c: [u8; 3]) -> Self {
        Self {
            rgb: Some(c),
            alpha: None,
        }
    }
}

/// 一条角标规则：某状态成立时，在某角落用某色画一个角标。
/// 关掉的规则（配置侧 `enabled = false`）在转换时就滤掉，不进这张表。
#[derive(Debug, Clone, PartialEq)]
pub struct BadgeRule {
    /// 什么时候画。
    pub state: BadgeState,
    /// 画在哪个角。
    pub corner: Corner,
    /// 浅色任务栏上的着色。
    pub color_light: BadgeColor,
    /// 深色任务栏上的着色。同一色在深浅任务栏上的可辨度可能差很远，故分两份。
    pub color_dark: BadgeColor,
    /// 相对全局倍率的额外倍率。`<= 0` 视作这一条不画。
    /// 全局那一级管角标整体大小，这一级管这一条要不要比别人大或小。
    pub scale: f32,
}

impl BadgeRule {
    /// 亮暗同色、不透明度跟随全局的规则（出厂三条都是这个形态）。
    pub fn solid(state: BadgeState, corner: Corner, color: [u8; 3], scale: f32) -> Self {
        Self {
            state,
            corner,
            color_light: BadgeColor::rgb(color),
            color_dark: BadgeColor::rgb(color),
            scale,
        }
    }
}

/// 一次图标渲染的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconSpec {
    /// 主字，如「中」「英」「拼」「五」。
    pub label: String,
    /// 标点角标状态。
    pub punct: PunctBadge,
    /// 是否全角。命中 [`BadgeState::FullWidth`] 规则时画角标（出厂在右上角）；半角不画。
    pub full_width: bool,
    /// 整体变淡：只给线程级 KEYBOARD_DISABLED（输入法整个被禁用）。
    /// 别把「焦点不在可编辑控件里」并进来：那是日常状态，并入会让图标频繁变灰，该情形与密码框一样显「英」。
    pub dimmed: bool,
    /// 动画相位，仅在演示模式下递增（见 [`IconRenderer::demo_animation`]）。
    /// 放进 spec 是为了让发布器的「状态未变则跳过」自动把它算进去。
    pub frame: u32,
    /// 主字显示的是否为英文态标签（非中文态 / 不可输入态 / CapsLock 都算）。只用来挑主字色（见 [`TextColors`]）。
    pub english: bool,
}

impl Default for IconSpec {
    fn default() -> Self {
        Self {
            label: "中".to_string(),
            punct: PunctBadge::None,
            full_width: false,
            dimmed: false,
            frame: 0,
            english: false,
        }
    }
}

/// 主字色，按「中/英 × 任务栏浅/深」四格，每格 RGBA（非预乘）。
/// 这里是合并后的最终值（用户 > 主题 > 内置，在协调器合并），渲染器不再回落；默认浅色任务栏黑、深色任务栏白。
/// alpha 只作用于主字字形；`auto` 角标与演示跑马灯只取 RGB。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextColors {
    pub cn_light: [u8; 4],
    pub cn_dark: [u8; 4],
    pub en_light: [u8; 4],
    pub en_dark: [u8; 4],
}

impl TextColors {
    /// 内置浅色任务栏主字色（黑）。
    pub const BUILTIN_LIGHT: [u8; 4] = [0, 0, 0, 255];
    /// 内置深色任务栏主字色（白）。
    pub const BUILTIN_DARK: [u8; 4] = [255, 255, 255, 255];

    /// 按（是否英文态，任务栏是否深色）取一格。
    pub fn pick(&self, english: bool, dark_theme: bool) -> [u8; 4] {
        match (english, dark_theme) {
            (false, false) => self.cn_light,
            (false, true) => self.cn_dark,
            (true, false) => self.en_light,
            (true, true) => self.en_dark,
        }
    }
}

impl Default for TextColors {
    fn default() -> Self {
        Self {
            cn_light: Self::BUILTIN_LIGHT,
            cn_dark: Self::BUILTIN_DARK,
            en_light: Self::BUILTIN_LIGHT,
            en_dark: Self::BUILTIN_DARK,
        }
    }
}

/// 单通道覆盖度蒙版（0.0~1.0），最终当 alpha 用。
#[derive(Clone)]
struct Mask {
    n: usize,
    v: Vec<f32>,
}

impl Mask {
    fn new(n: usize) -> Self {
        Self {
            n,
            v: vec![0.0; n * n],
        }
    }

    /// source-over 累加：已有覆盖度不会被后画的削减。
    fn blend(&mut self, x: i32, y: i32, cov: f32) {
        if x < 0 || y < 0 || x as usize >= self.n || y as usize >= self.n {
            return;
        }
        let d = &mut self.v[y as usize * self.n + x as usize];
        *d += cov * (1.0 - *d);
    }

    fn get(&self, i: usize) -> f32 {
        self.v[i].min(1.0)
    }

    /// 并入另一张蒙版（source-over，逐像素）。
    /// 多处挖空要先合成一张再挖一次；分别挖两遍会让交叠处被削得更狠。
    fn union(&mut self, other: &Mask) {
        for (d, s) in self.v.iter_mut().zip(other.v.iter()) {
            *d += s * (1.0 - *d);
        }
    }
}

/// 4×4 超采样画圆/环。`r_in > 0` 即为环。图形只有圆和矩形，自己积分覆盖度即可。
fn draw_disc(m: &mut Mask, cx: f32, cy: f32, r_out: f32, r_in: f32) {
    const SS: i32 = 4;
    let x0 = (cx - r_out - 1.0).floor() as i32;
    let x1 = (cx + r_out + 1.0).ceil() as i32;
    let y0 = (cy - r_out - 1.0).floor() as i32;
    let y1 = (cy + r_out + 1.0).ceil() as i32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let mut hit = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                    if d <= r_out && d >= r_in {
                        hit += 1;
                    }
                }
            }
            if hit > 0 {
                m.blend(x, y, hit as f32 / (SS * SS) as f32);
            }
        }
    }
}

/// 4×4 超采样画直角三角，直角顶点在 `corner` 指定的角上、直角边长 `leg`。
fn draw_corner_triangle(m: &mut Mask, s: f32, leg: f32, corner: Corner) {
    const SS: i32 = 4;
    // 四个角落同构：坐标折到右下角后只需一份判据。
    let fold_x = |px: f32| -> f32 { if corner.is_right() { px } else { s - px } };
    let fold_y = |py: f32| -> f32 { if corner.is_bottom() { py } else { s - py } };
    // 判据一律在**折叠后**的坐标里做。
    let inside = |px: f32, qy: f32, l: f32| -> bool {
        px >= s - l && qy >= s - l && (px + qy) >= (2.0 * s - l)
    };
    // 扫描范围同样按角落取：折叠只作用于判据，循环边界仍在原坐标里。
    let (x0, x1) = if corner.is_right() {
        ((s - leg - 1.0).floor() as i32, s.ceil() as i32)
    } else {
        (0i32, (leg + 1.0).ceil() as i32)
    };
    let (y0, y1) = if corner.is_bottom() {
        ((s - leg - 1.0).floor() as i32, s.ceil() as i32)
    } else {
        (0i32, (leg + 1.0).ceil() as i32)
    };
    for y in y0..=y1 {
        for x in x0..=x1 {
            let mut hit = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = fold_x(x as f32 + (sx as f32 + 0.5) / SS as f32);
                    let qy = fold_y(y as f32 + (sy as f32 + 0.5) / SS as f32);
                    if inside(px, qy, leg) {
                        hit += 1;
                    }
                }
            }
            if hit > 0 {
                m.blend(x, y, hit as f32 / (SS * SS) as f32);
            }
        }
    }
}

/// 按「离哪条边最近」分区（沿对角线切四块），转角处像素归属明确，跑马灯不断不重。
fn perimeter_t(px: f32, py: f32, s: f32) -> f32 {
    let per = 4.0 * s;
    let (d_top, d_bottom, d_left, d_right) = (py, s - py, px, s - px);
    let min = d_top.min(d_bottom).min(d_left).min(d_right);
    if min == d_top {
        px / per
    } else if min == d_right {
        (s + py) / per
    } else if min == d_bottom {
        (2.0 * s + (s - px)) / per
    } else {
        (3.0 * s + (s - py)) / per
    }
}

/// 外圈跑马灯：只点亮周长上 `[phase, phase + len)` 的一段（相位与长度均归一化）。
///
/// 纯演示用，不表达任何状态。
fn draw_ring_marquee(m: &mut Mask, s: f32, th: f32, phase: f32, len: f32) {
    const SS: i32 = 4;
    let n = m.n as i32;
    let phase = phase.rem_euclid(1.0);
    for y in 0..n {
        for x in 0..n {
            let mut hit = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let edge = px.min(py).min(s - px).min(s - py);
                    if edge > th {
                        continue;
                    }
                    // 相对相位落在 [0, len) 内即点亮；用 rem_euclid 让区间跨越 0 点时仍连续
                    let rel = (perimeter_t(px, py, s) - phase).rem_euclid(1.0);
                    if rel < len {
                        hit += 1;
                    }
                }
            }
            if hit > 0 {
                m.blend(x, y, hit as f32 / (SS * SS) as f32);
            }
        }
    }
}

/// 一层待合成的角标：形状、配套的挖空形状、以及这一层的颜色（绑在一起，免得平行数组错位）。
struct BadgeLayer {
    /// 角标本身。
    mask: Mask,
    /// 同一形状外扩 [`IconRenderer::BADGE_GAP`] 后的版本，用来从主字里挖出间隙。
    /// 只在本层不透明（`alpha >= 1.0`）时才挖，见 [`IconRenderer::badge_alpha`]。
    clear: Mask,
    /// 本层颜色（BGR），已按当前明暗主题选定。
    color: [u8; 3],
    /// 本层不透明度（0~1），已把「条目覆盖」与全局默认合并完。
    alpha: f32,
}

/// 图标渲染器。持有 [`TextRenderer`]（内含 DirectWrite 工厂与测量缓存），故应长期复用。
pub struct IconRenderer {
    text: TextRenderer,
    /// 调试用：在左上角画 N 个点标出这是第几个尺寸档。
    /// `GetIcon` 没有尺寸参数，开它能看出系统挑了哪档、有没有二次缩放。
    pub size_marks: bool,
    /// 角标总开关，见 [`BadgeStyle`]。**默认关**。
    pub style: BadgeStyle,
    /// 角标规则表，顺序即优先级。
    /// 同一角落只画最靠前命中的那一条（16px 上叠两个三角会糊成一片）。
    /// 不想被某状态打扰就关掉那条规则（不进本表），而不是给透明色。
    pub rules: Vec<BadgeRule>,
    /// 角标不透明度的全局默认（0~1），单条可用 `alpha_light/dark` 覆盖（见 [`BadgeColor::alpha`]）。
    /// 逐层分档：`= 1.0` 走实心 + 挖空（底下笔画被切掉），`< 1.0` 走半透明 + 不挖空（字完整）。
    /// 两者不能叠加，否则调不透明度看起来毫无效果，见设计文档「挖空与透明是互斥的两种分离手段」。
    pub badge_alpha: f32,
    /// 角标全局大小倍率。1.0 = [`Self::CORNER_LEG`] 那个基准；与 [`BadgeRule::scale`] 相乘。
    pub badge_scale: f32,
    /// 演示模式：外圈跑马灯，不表达状态。
    /// 渲染端只按相位画，需有人推进 [`IconSpec::frame`] 并重新发布。
    pub demo_animation: bool,
    /// 主字色（最终值），见 [`TextColors`]。
    pub text_colors: TextColors,
}

impl IconRenderer {
    /// 字体族与旧 C++ 实现保持一致，避免换渲染端时字形跟着变。
    const FONT_FAMILY: &'static str = "Microsoft YaHei UI";

    /// 主字字重，对齐旧 C++ 实现的 `DWRITE_FONT_WEIGHT_LIGHT`。16px 下常规字重的汉字笔画会挤在一起。
    const FONT_WEIGHT: i32 = 300;

    /// 被回缩过的拉丁标签的字重（如 `[ui.labels]` 把英文态配成 `"En"`），300 配回缩后的小字号会发虚。
    /// 判据是「有没有真被缩小」而非「是不是字母」：单字符 `"A"` 不进回缩，仍走 300。
    /// 分档依据是标签自身而非运行时状态，同一标签字重稳定不跳。
    const FONT_WEIGHT_LATIN: i32 = 400;

    /// 纯拉丁标签的边缘留白（像素）。拉丁字母的 advance 自带左右边距，套汉字那档会被白缩两三个像素。
    const LATIN_EDGE_INSET: f32 = 1.0;

    /// 主字字号 = 图标边长 − 本值，与旧 C++ 实现的 `fontSizeDIP = iconSize - 2` 一致。
    /// 这是基线：角标靠挖空间隙让位，不缩小主字。
    const FONT_SIZE_INSET: f32 = 2.0;

    /// 宽标签回缩字号时的下限（相对基线字号的比例）。
    /// 防御性下限，只兜 measure 返回异常大宽度。最宽的合法标签（两个全角字）自然缩到 0.5 左右，
    /// 下限定在那之上会反过来把它顶出画布。
    const MIN_FONT_SCALE: f32 = 0.4;

    /// 宽标签回缩后占可用宽度的比例。行盒不含字形 overhang 与抗锯齿外溢，取满 1.0 仍会触边。
    const WIDE_LABEL_SAFETY: f32 = 0.94;

    /// 角标周围挖空的间隙，按图标边长取比例（16px 下约 1.1px）。没有它角标与主字笔画会糊成一团。
    const BADGE_GAP: f32 = 0.07;

    /// 外圈厚度（按边长比例）。16px 下约 1.1px——再细就被抗锯齿吃没了。
    const RING_TH: f32 = 0.07;

    /// 跑马灯亮段占周长的比例。
    const MARQUEE_LEN: f32 = 0.28;

    /// 演示动画一圈多少帧。帧率由驱动方决定，这里只定义"转一圈需要几帧"。
    pub const DEMO_FRAMES_PER_CYCLE: u32 = 40;

    /// 中文标点角标默认色 `#2288E0`（蓝，BGR 存储）。
    /// 与英文标点的橙成对挑选：深浅任务栏上都立得住、色相相距远；16px 下靠色相区分比靠形状（实心/空心）可靠。
    pub const DEFAULT_PUNCT_CN_COLOR: [u8; 3] = [0xE0, 0x88, 0x22];

    /// 英文标点角标默认色 `#EE9922`（橙，BGR 存储），见 [`Self::DEFAULT_PUNCT_CN_COLOR`]。
    pub const DEFAULT_PUNCT_EN_COLOR: [u8; 3] = [0x22, 0x99, 0xEE];

    /// 全角标记默认色 `#E0447A`（玫红，BGR 存储）。绿感知亮度太高，浅色任务栏上拉不开对比。
    /// 别与英文标点共用橙：用户把两者配到同一角落时就无法区分。
    pub const DEFAULT_FULL_WIDTH_COLOR: [u8; 3] = [0x7A, 0x44, 0xE0];

    /// 角标默认不透明度，见 [`Self::badge_alpha`]。须严格小于 1，等于 1 会切到挖空档。
    pub const DEFAULT_BADGE_ALPHA: f32 = 0.88;

    /// 角标三角的基准直角边长（按图标边长取比例），对应倍率 1.0。
    /// 更大会抢主字视觉重心，小到 0.28 左右就糊成色块看不出三角。
    const CORNER_LEG: f32 = 0.34;

    /// 墨迹居中的收敛阈值（像素）。`IDWriteBitmapRenderTarget` 把基线吸附到整像素，0.5px 是物理下限。
    const CENTER_TOL: f32 = 0.5;

    /// 居中最多重画几遍。基线吸附使原点位移与墨迹位移不是 1:1，单步必然欠冲，故迭代。
    const CENTER_MAX_PASSES: usize = 3;

    /// 量墨迹包围盒时的覆盖度门槛。抗锯齿边缘会向外洇出很淡的一圈，
    /// 全算进包围盒会让"边缘更虚的那一侧"显得更宽，反而把中心算偏。
    const INK_THRESHOLD: f32 = 0.10;

    pub fn new(style: BadgeStyle) -> Result<Self, String> {
        // 基准字号仅用于构造，实际每次渲染都按图标尺寸显式指定。
        let text = TextRenderer::new(Self::FONT_FAMILY, 16.0)?;
        Ok(Self {
            text,
            size_marks: false,
            style,
            rules: Self::default_rules(),
            badge_alpha: Self::DEFAULT_BADGE_ALPHA,
            badge_scale: 1.0,
            demo_animation: false,
            text_colors: TextColors::default(),
        })
    }

    /// 出厂规则表：中文标点右下蓝、英文标点右下橙、全角右上玫红，亮暗同色、倍率都是 1.0。
    /// 两条标点规则同占右下角不冲突：状态互斥，同一时刻只会命中一条。
    pub fn default_rules() -> Vec<BadgeRule> {
        vec![
            BadgeRule::solid(
                BadgeState::PunctCn,
                Corner::BottomRight,
                Self::DEFAULT_PUNCT_CN_COLOR,
                1.0,
            ),
            BadgeRule::solid(
                BadgeState::PunctEn,
                Corner::BottomRight,
                Self::DEFAULT_PUNCT_EN_COLOR,
                1.0,
            ),
            BadgeRule::solid(
                BadgeState::FullWidth,
                Corner::TopRight,
                Self::DEFAULT_FULL_WIDTH_COLOR,
                1.0,
            ),
        ]
    }

    /// 渲染一个变体，返回 `size_px × size_px` 的**非预乘** BGRA。
    ///
    /// `dark_theme` = 任务栏是暗色（图标应画成浅色）。
    pub fn render(&self, size_px: u16, dark_theme: bool, spec: &IconSpec) -> Vec<u8> {
        let n = size_px as usize;
        let s = size_px as f32;
        // 主字色按（中/英，任务栏明暗）取；输出是 BGR 顺序，这里从 RGBA 换过去。
        // fg3 同时是 `auto` 角标与演示跑马灯的颜色（与主字同色）。
        let [fr, fg, fb, fa] = self.text_colors.pick(spec.english, dark_theme);
        let fg3 = [fb, fg, fr];
        let text_alpha = fa as f32 / 255.0;

        let glyph = self.render_glyph_mask(size_px, spec);
        // 总开关关掉时 active_layers 返回空表，像素上与「此刻无状态可显示」完全一致。
        let layers = self.active_layers(size_px, dark_theme, spec, fg3);

        // clear 是角标外扩一圈后的形状，用来在主字上挖出间隙。
        // 挖空与半透明互斥（见 [`Self::badge_alpha`]），故只有不透明的层参与挖空；
        // 全部并进同一张 clear，主字只挖一次。
        let mut clear = Mask::new(n);
        for l in layers.iter().filter(|l| l.alpha >= 1.0) {
            clear.union(&l.clear);
        }

        // 演示动画独立成层：不参与挖空，叠在最上面。
        let marquee = if self.demo_animation {
            let mut m = Mask::new(n);
            let phase = spec.frame as f32 / Self::DEMO_FRAMES_PER_CYCLE as f32;
            draw_ring_marquee(&mut m, s, s * Self::RING_TH, phase, Self::MARQUEE_LEN);
            m
        } else {
            Mask::new(n)
        };

        let mut out = vec![0u8; n * n * 4];
        for i in 0..n * n {
            // 自下而上 source-over：主字（已挖空）→ 各角标（按规则序）→ 演示动画。
            // 色值按预乘累加，最后除以合成 alpha 还原成非预乘（`hbmColor` 要求）。
            let g_a = glyph.get(i) * (1.0 - clear.get(i)) * text_alpha;
            let mut a = g_a;
            let mut col = [
                fg3[0] as f32 * g_a,
                fg3[1] as f32 * g_a,
                fg3[2] as f32 * g_a,
            ];
            for l in &layers {
                let la = l.mask.get(i) * l.alpha;
                for (c, v) in col.iter_mut().enumerate() {
                    *v = l.color[c] as f32 * la + *v * (1.0 - la);
                }
                a = la + a * (1.0 - la);
            }
            let m_a = marquee.get(i);
            for (c, v) in col.iter_mut().enumerate() {
                *v = fg3[c] as f32 * m_a + *v * (1.0 - m_a);
            }
            a = m_a + a * (1.0 - m_a);

            let mut alpha = (a * 255.0).round().clamp(0.0, 255.0) as u8;
            if spec.dimmed {
                alpha = ((alpha as u32 * 90) / 255) as u8;
            }

            if a > 0.0 {
                for c in 0..3 {
                    out[i * 4 + c] = (col[c] / a).round().clamp(0.0, 255.0) as u8;
                }
            }
            out[i * 4 + 3] = alpha; // A（非预乘）
        }
        out
    }

    /// 主字蒙版：黑底画白字，取 max(R,G,B) 当覆盖度（透明缓冲上画不出字，见模块文档）。
    ///
    /// `measure` 给的是行盒而非墨迹盒，按行盒居中字会偏上偏左。故先画一遍、从蒙版量真实墨迹包围盒、
    /// 再按差量重画，见设计文档「主字定位：按墨迹居中，不是按行盒」。代价是每个变体多画几次，只在状态变化时发生。
    fn render_glyph_mask(&self, size_px: u16, spec: &IconSpec) -> Mask {
        let s = size_px as f32;

        // 字号与角标有无无关：按运行时状态分档会让同一个字随中英切换忽大忽小。
        let font_size = s - Self::FONT_SIZE_INSET;
        let mut fs = font_size;
        let mut style =
            crate::text::dwrite::TextStyle::new(font_size).with_weight(Self::FONT_WEIGHT);

        // 第一遍按行盒粗定位。测量必须与绘制用同一个 TextStyle（字重影响字宽）。
        let mut m = self.text.measure(&spec.label, &style);

        // 装不下（如 `[ui.labels]` 配的 `"En"`、全角「Ｅｎ」）就按 measure 实测宽度回缩字号，不能按字符数判。
        // 依据是标签自身宽度，各标签字号稳定，与上面说的按运行时状态分档不是一回事。
        // 可用宽度按是否含汉字分两档：汉字墨迹几乎填满 advance 要留足，拉丁字母自带边距可占满。
        let avail = if Self::has_cjk(&spec.label) {
            s - Self::FONT_SIZE_INSET
        } else {
            s - Self::LATIN_EDGE_INSET
        };
        // 目标宽度再收一点：行盒不含 overhang 与抗锯齿外溢。
        let target = avail * Self::WIDE_LABEL_SAFETY;
        let min_size = font_size * Self::MIN_FONT_SCALE;

        // 字重补偿只给真被缩小的拉丁标签（见 [`Self::FONT_WEIGHT_LATIN`]），装得下的标签路径不变。
        let weight = Self::label_weight(&spec.label, m.width, target);
        // 字重影响字宽，换了就必须重测——否则下面的收敛判据用的是另一种字重的宽度。
        if weight != Self::FONT_WEIGHT {
            style = crate::text::dwrite::TextStyle::new(font_size).with_weight(weight);
            m = self.text.measure(&spec.label, &style);
        }

        // 须循环收敛：字号会被吸附到整像素，比例算法会在吸附值上原地打转，每轮至少降 1px。
        // `fs` 每轮严格减小且以 `min_size` 收底，必然终止。
        while m.width > target && m.width > 0.0 && fs > min_size {
            fs = (fs * target / m.width).min(fs - 1.0).max(min_size);
            style = crate::text::dwrite::TextStyle::new(fs).with_weight(weight);
            m = self.text.measure(&spec.label, &style);
        }
        let x0 = ((s - m.width) * 0.5).max(0.0);
        let y0 = ((s - m.height) * 0.5).max(0.0);
        let mut mask = self.draw_glyph_at(size_px, &style, &spec.label, x0, y0);

        // 逐次按墨迹残差校正。无墨迹（非 Windows 的 mock 后端）时 delta 为 None，直接跳过。
        // 让墨迹盒正中的位移同时也让溢出最小，不会把字挤出画布。
        let (mut ox, mut oy) = (0.0f32, 0.0f32);
        let mut err = Self::center_err(&mask, s);
        for _ in 0..Self::CENTER_MAX_PASSES {
            let Some((dx, dy)) = Self::ink_center_delta(&mask, s) else {
                break;
            };
            if dx.abs() <= Self::CENTER_TOL && dy.abs() <= Self::CENTER_TOL {
                break;
            }
            let (nx, ny) = (ox + dx, oy + dy);
            let next = self.draw_glyph_at(size_px, &style, &spec.label, x0 + nx, y0 + ny);
            let next_err = Self::center_err(&next, s);
            // 不再改善就收手：吸附使残差呈阶梯状，硬追会在相邻整像素间来回跳。
            if next_err >= err {
                break;
            }
            mask = next;
            err = next_err;
            (ox, oy) = (nx, ny);
        }

        if self.size_marks {
            Self::draw_size_marks(&mut mask, size_px);
        }
        mask
    }

    /// 该给这个标签用哪个字重，见 [`Self::FONT_WEIGHT_LATIN`]。
    /// `base_width` 是基线字号与字重下的行盒宽，与 `target` 比即知会不会被缩小。抽成纯函数便于直接断言判据。
    fn label_weight(label: &str, base_width: f32, target: f32) -> i32 {
        if base_width > target && !Self::has_cjk(label) {
            Self::FONT_WEIGHT_LATIN
        } else {
            Self::FONT_WEIGHT
        }
    }

    /// 标签里是否含汉字。字重与可用宽度共用这一判据，免得出现半档状态。
    /// 只判 CJK 表意文字三段：全角「Ｅｎ」不是 ASCII 却是字母，按 ASCII 判会错分到汉字档。
    fn has_cjk(label: &str) -> bool {
        label.chars().any(|c| {
            matches!(c as u32,
                0x4E00..=0x9FFF   // CJK 统一表意文字
                | 0x3400..=0x4DBF // 扩展 A
                | 0xF900..=0xFAFF // 兼容表意文字
            )
        })
    }

    /// 在指定原点画一次主字，返回覆盖度蒙版。
    fn draw_glyph_at(
        &self,
        size_px: u16,
        style: &crate::text::dwrite::TextStyle,
        label: &str,
        x: f32,
        y: f32,
    ) -> Mask {
        let n = size_px as usize;
        // 黑色不透明底：GDI 需要不透明背景才能正确抗锯齿混合
        let mut buf = vec![0u8; n * n * 4];
        for px in buf.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        let _ = self.text.draw(
            &mut buf,
            size_px as u32,
            size_px as u32,
            x,
            y,
            label,
            style,
            [255, 255, 255, 255], // 白字（BGRA）
        );

        let mut mask = Mask::new(n);
        for i in 0..n * n {
            let b = buf[i * 4];
            let g = buf[i * 4 + 1];
            let r = buf[i * 4 + 2];
            // max 而非平均：保留抗锯齿边缘的过渡，与旧 C++ 实现同口径
            mask.v[i] = r.max(g).max(b) as f32 / 255.0;
        }
        mask
    }

    /// 偏心程度，用于比较两次尝试谁更居中。无墨迹时视为无穷差。
    fn center_err(m: &Mask, s: f32) -> f32 {
        Self::ink_center_delta(m, s).map_or(f32::INFINITY, |(dx, dy)| dx.abs().max(dy.abs()))
    }

    /// 求「把墨迹包围盒摆到 `s×s` 正中」所需的位移。无墨迹时返回 `None`。
    fn ink_center_delta(m: &Mask, s: f32) -> Option<(f32, f32)> {
        let n = m.n;
        let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0usize, usize::MAX, 0usize);
        for y in 0..n {
            for x in 0..n {
                if m.v[y * n + x] < Self::INK_THRESHOLD {
                    continue;
                }
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
        if x0 == usize::MAX {
            return None;
        }
        // 包围盒按**像素边界**取值：第 x1 个像素的右边界是 x1+1。
        let cx = (x0 + x1 + 1) as f32 * 0.5;
        let cy = (y0 + y1 + 1) as f32 * 0.5;
        Some((s * 0.5 - cx, s * 0.5 - cy))
    }

    /// 当前状态下要画的全部角标层，按规则表顺序：按状态过滤 → 同角落只留最靠前的 → 按明暗取色。
    /// 总开关关掉时返回空表。
    fn active_layers(
        &self,
        size_px: u16,
        dark_theme: bool,
        spec: &IconSpec,
        fg3: [u8; 3],
    ) -> Vec<BadgeLayer> {
        if self.style == BadgeStyle::None {
            return Vec::new();
        }
        let gap = size_px as f32 * Self::BADGE_GAP;
        let mut used: Vec<Corner> = Vec::new();
        let mut layers = Vec::new();
        for r in &self.rules {
            if !r.state.matches(spec) {
                continue;
            }
            // 一个角落只画最靠前命中的那一条。
            if used.contains(&r.corner) {
                continue;
            }
            let k = self.badge_scale.max(0.0) * r.scale;
            // 倍率归零视作不画，连挖空一起短路：挖空版边长是 leg + gap，leg 为 0 仍会在主字上挖出空洞。
            if k <= 0.0 {
                continue;
            }
            used.push(r.corner);
            let c = if dark_theme {
                r.color_dark
            } else {
                r.color_light
            };
            layers.push(BadgeLayer {
                mask: self.draw_corner_badge(size_px, r.corner, k, 0.0),
                clear: self.draw_corner_badge(size_px, r.corner, k, gap),
                color: c.rgb.unwrap_or(fg3),
                // 在这里合并全局默认，让「挖不挖空」与「画多淡」读同一个值。
                alpha: c.alpha.unwrap_or(self.badge_alpha).clamp(0.0, 1.0),
            });
        }
        layers
    }

    /// 画一个角标；`expand > 0` 时整体外扩，用于生成挖空蒙版。
    fn draw_corner_badge(&self, size_px: u16, corner: Corner, k: f32, expand: f32) -> Mask {
        let n = size_px as usize;
        let s = size_px as f32;
        let mut m = Mask::new(n);
        // 倍率只缩放形状，不缩放 expand（间隙）：间隙由「多宽才不糊」决定，跟着缩会在角标调小时把它收掉。
        draw_corner_triangle(&mut m, s, s * Self::CORNER_LEG * k + expand, corner);
        m
    }

    /// 在左上角画 N 个点标出尺寸档下标（调试用，见 [`Self::size_marks`]）。
    fn draw_size_marks(m: &mut Mask, size_px: u16) {
        let idx = wind_ipc::protocol::ICON_SIZES
            .iter()
            .position(|&s| s == size_px)
            .unwrap_or(0);
        let r = (size_px as f32 * 0.05).max(0.6);
        for k in 0..=idx {
            let cx = r + 0.5 + k as f32 * (r * 2.0 + 1.0);
            draw_disc(m, cx, r + 0.5, r, 0.0);
        }
    }
}

/// 把当前状态渲染成全部变体并投送到共享内存。
///
/// 服务进程持有一个，状态变化时调 [`Self::publish`]。DLL 侧的通知走既有 push 通道
/// （`push_state_update` → `OnUpdate(TF_LBI_ICON)`），本类不负责通知。
#[cfg(windows)]
pub struct LangBarIconPublisher {
    renderer: IconRenderer,
    shm: wind_bridge::icon_shm_windows::IconShm,
    /// 上次发布的状态。状态推送（焦点切换等）比图标变化频繁得多，未变就不重渲。
    last: Option<IconSpec>,
    /// 演示动画的当前相位。归发布器所有，状态推送那条路不必知道动画转到哪了，相位自然延续。
    demo_frame: u32,
}

#[cfg(windows)]
impl LangBarIconPublisher {
    /// `suffix` 取 `wind_config::variant::pipe_suffix()`（`""` / `"_dev"`）。
    pub fn new(suffix: &str, style: BadgeStyle) -> Result<Self, String> {
        let renderer = IconRenderer::new(style)?;
        let shm = wind_bridge::icon_shm_windows::IconShm::create(suffix)
            .map_err(|e| format!("创建图标共享内存失败: {e}"))?;
        Ok(Self {
            renderer,
            shm,
            last: None,
            demo_frame: 0,
        })
    }

    /// 演示动画（外圈跑马灯）开关。关掉时相位归零。
    /// 画面要动还需有人按帧调 [`Self::advance_demo_frame`] 并重新发布。
    pub fn set_demo_animation(&mut self, on: bool) {
        if self.renderer.demo_animation != on {
            self.renderer.demo_animation = on;
            self.demo_frame = 0;
            self.last = None; // 呈现变了，下次必须重发
        }
    }

    pub fn demo_animation(&self) -> bool {
        self.renderer.demo_animation
    }

    /// 当前相位。
    pub fn demo_frame(&self) -> u32 {
        self.demo_frame
    }

    /// 推进一帧并返回新相位。按周期取模，避免长时间运行后溢出。
    pub fn advance_demo_frame(&mut self) -> u32 {
        self.demo_frame = (self.demo_frame + 1) % IconRenderer::DEMO_FRAMES_PER_CYCLE;
        self.demo_frame
    }

    /// 一次性套用全部呈现参数（配置侧的落地入口）。`None` = 该项不动。
    /// 收成一个函数，「改了参数必须清 `last` 才会重发」只需在一处记得。返回是否确实有改动。
    pub fn apply_appearance(
        &mut self,
        style: Option<BadgeStyle>,
        badge_scale: Option<f32>,
        badge_alpha: Option<f32>,
        rules: Option<Vec<BadgeRule>>,
        text_colors: Option<TextColors>,
    ) -> bool {
        let r = &mut self.renderer;
        let mut changed = false;
        let mut set = |cond: bool| changed |= cond;

        if let Some(v) = style {
            set(r.style != v);
            r.style = v;
        }
        if let Some(v) = badge_scale {
            set(r.badge_scale != v);
            r.badge_scale = v;
        }
        if let Some(v) = badge_alpha {
            set(r.badge_alpha != v);
            r.badge_alpha = v;
        }
        if let Some(v) = rules {
            set(r.rules != v);
            r.rules = v;
        }
        if let Some(v) = text_colors {
            set(r.text_colors != v);
            r.text_colors = v;
        }

        if changed {
            self.last = None; // 呈现变了，下次必须重发
        }
        changed
    }

    /// 调试开关：在各档位图上烧尺寸标记，用于真机确认系统实际取用了哪一档。
    pub fn set_size_marks(&mut self, on: bool) {
        if self.renderer.size_marks != on {
            self.renderer.size_marks = on;
            self.last = None; // 呈现变了，下次必须重发
        }
    }

    pub fn size_marks(&self) -> bool {
        self.renderer.size_marks
    }

    /// 换角标总开关档位。改这个不需要重新分发 DLL——这正是把渲染搬到服务端的收益。
    pub fn set_style(&mut self, style: BadgeStyle) {
        if self.renderer.style != style {
            self.renderer.style = style;
            self.last = None;
        }
    }

    pub fn style(&self) -> BadgeStyle {
        self.renderer.style
    }

    /// 渲染并发布。返回新的发布序号；`None` 表示状态未变、已跳过。
    /// 序号即 SHM header 的 `sequence`（读端 seqlock 判据），是服务端与 DLL 日志唯一能对上号的量。
    pub fn publish(&mut self, spec: &IconSpec) -> Result<Option<u32>, String> {
        if self.last.as_ref() == Some(spec) {
            return Ok(None);
        }

        // 用变体表驱动渲染，顺序与档位才不会与 DLL 读端错位。
        let table = wind_ipc::protocol::icon_variant_table();
        let mut bitmaps = Vec::with_capacity(table.len());
        for v in &table {
            let dark = v.theme == wind_ipc::protocol::ICON_THEME_DARK;
            bitmaps.push(self.renderer.render(v.size_px, dark, spec));
        }

        let seq = self
            .shm
            .publish(&bitmaps)
            .map_err(|e| format!("发布图标共享内存失败: {e}"))?;
        self.last = Some(spec.clone());
        Ok(Some(seq))
    }

    /// SHM 名（日志与排查用）。
    pub fn shm_name(&self) -> &str {
        self.shm.name()
    }
}

#[cfg(test)]
#[path = "langbar_icon_tests.rs"]
mod tests;
