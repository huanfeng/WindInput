//! 语言栏图标离屏渲染（Windows TSF 输入指示器那个 16×16 图标）
//!
//! 服务端把当前状态渲染成多档位图写进共享内存，`wind_tsf.dll` 的 `GetIcon` 直接取用。
//! 设计与取舍见 `docs/design/langbar-icon-shared-render.md`。
//!
//! ## 为什么必须分层渲染
//!
//! [`crate::text::dwrite`] 后端假设目标缓冲区是**已含背景的预乘 alpha**：渲染后逐像素
//! 对比，RGB 未变的算背景保留原 alpha，RGB 变了的按缓冲区原 alpha 预乘。
//!
//! 直接后果：**给一个全透明（alpha=0）缓冲画字，文字像素会被按 alpha=0 预乘，
//! 结果全黑透明，什么都看不到。** 所以这里走「黑底画白字 → 取 luminance 当覆盖度」
//! 拿到主字蒙版，角标另行几何绘制拿到第二张蒙版，两张蒙版各自着色后再合成。
//!
//! 顺带的好处是**摆脱了单色限制**：旧的 C++ 实现对整张图共用一个 luminance→alpha，
//! 所以整个图标只能一种颜色；分层之后主字与角标可以各自取色。
//!
//! ## 像素格式
//!
//! 输出 **非预乘** BGRA——Windows 图标的 32bpp DIB 就是这个约定
//! （`CreateIconIndirect` 的 `hbmColor` 里 RGB 不乘 alpha）。

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

/// 角标总开关。
///
/// 只有「不画」与「角标」两档。早期版本这里是一组形状编码（最外圈边框、底部横条、
/// 圆/方、环/点），在 16px 上逐个真机比选，最终只有角落三角立得住；而**位置成为
/// 单条规则的属性**之后，非角落锚定的那几种在模型里也无处安放——留着它们等于留下
/// 一组会静默忽略 `corner` 配置的取值，正是「配了没反应」那类最难自查的毛病。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BadgeStyle {
    /// 不画任何角标，图标退回「只有主字」的旧样子。
    ///
    /// 做成枚举的一员而非另开一个 `enabled` 布尔：两者并存时可以摆出「关着 + 配了
    /// 三条规则」这种自相矛盾的状态，而单选组从类型上就排除了它。它同时是用户可见的
    /// **总开关**——关掉即全部规则都不画，不必逐条去关。
    ///
    /// **它是默认值**：角标是加在一个所有 Windows 用户都会看到的系统图标上的新东西，
    /// 默认改变所有人的任务栏是过界的。想要的人去开，这样默认体验与装之前一致。
    #[default]
    None,
    /// 按规则表在四角画直角三角。
    ///
    /// 三角填满角落，同样面积下比圆形更"占地方"，因而在 16px 下比小圆点更醒目；
    /// 且直角边贴着图标边界，斜边朝向中心，与主字的接触面比同样占地的方块更小。
    Corner,
}

impl BadgeStyle {
    /// 全部档位，顺序即菜单里的顺序，也是 [`Self::index`] 的编号依据。
    ///
    /// 单一真相源：菜单项、勾选态还原、`MenuCmd` 的 u8 参数三处都从这里取，
    /// 各写一份的话，加一档时漏改任意一处都表现为「点了另一档」。
    pub const ALL: [BadgeStyle; 2] = [BadgeStyle::None, BadgeStyle::Corner];

    /// 菜单文案。
    pub fn label(self) -> &'static str {
        match self {
            BadgeStyle::None => "不显示",
            BadgeStyle::Corner => "角标",
        }
    }

    /// 落盘用的**稳定 id**。
    ///
    /// ⚠ 刻意不存 [`Self::index`]：下标是「在 ALL 里排第几」这个位置身份，把它写进
    /// 配置文件等于让格式绑死声明顺序。凡是活得比进程久的标识都要用名字。
    pub fn as_id(self) -> &'static str {
        match self {
            BadgeStyle::None => "none",
            BadgeStyle::Corner => "corner",
        }
    }

    /// 由稳定 id 还原；未知（含空串）回落到默认。
    ///
    /// 上一版的形状 id（`corner_triangle` / `outer_ring` …）**刻意不做兼容别名**：
    /// 那一版是标着实验性发出去的，让用户重新配一次即可，为此长期背一张别名表不值。
    /// 未知值回落「不显示」＝与装之前一致，不会画出个莫名其妙的东西。
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
///
/// 只开放这三个：它们都已经在 [`IconSpec`] 里有对应数据，接线为零。半角与英文态
/// 刻意不在其中——**没有信息量的状态不占像素**：半角是常态，给它画一个标记等于在
/// 16×16 上常驻一个永远不变的点；英文态下主字本身已经是「英」，角标只是重复。
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
    ///
    /// ⚠ 与 [`BadgeStyle::from_id`] / [`Corner::from_id`] 的「回落默认」**刻意不同**：
    /// 状态没有合理的默认值——一条不知道该在什么时候画的规则，回落到任何一个状态都是
    /// 在替用户瞎猜，画出来的东西他对不上因果。故整条规则丢弃（调用方负责记警告）。
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

    /// 由稳定 id 还原；未知回落右下角。
    ///
    /// 位置有合理默认（右下是最不挡主字的角，也是出厂标点规则的落点），故与
    /// [`BadgeState::from_id`] 不同，写错一个词不至于让整条规则消失。
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
///
/// 之所以是一个结构体而不是两个平行字段：两者都描述**同一侧**（light/dark）的着色，
/// 拆开存就得靠约定保证它们同进同出。配置侧是 `color_x` + `alpha_x` 两键，
/// 由协调器的 `badge_color_of` 合成本结构。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BadgeColor {
    /// 色相（BGR）。`None` = 与主字同色并跟随明暗主题（配置里的 `""`，旧写法 `auto`）。
    pub rgb: Option<[u8; 3]>,
    /// 本条自己的不透明度（0~1）。`None` = 用全局 [`IconRenderer::badge_alpha`]。
    ///
    /// 对应配置里的 `alpha_light` / `alpha_dark`（未设置 = `None`）。`Some(1.0)` 与「不指定」
    /// 含义完全不同：前者会把这一条切到挖空档，见 [`IconRenderer::badge_alpha`]。
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
///
/// 关掉的规则**不进这张表**（配置侧的 `enabled = false` 在转换时就被滤掉），
/// 所以渲染器只需要回答"画哪些"，不必再处理"配了但不画"。
#[derive(Debug, Clone, PartialEq)]
pub struct BadgeRule {
    /// 什么时候画。
    pub state: BadgeState,
    /// 画在哪个角。
    pub corner: Corner,
    /// 浅色任务栏上的着色。
    pub color_light: BadgeColor,
    /// 深色任务栏上的着色。
    ///
    /// 亮暗两份而不是一份：渲染本来就按「尺寸档 × 明暗两档」出全部变体
    /// （见 [`crate::langbar_icon::LangBarIconPublisher::publish`]），按主题取色是白拿的；
    /// 而同一个色在浅色与深色任务栏上的可辨度可以差很远。
    pub color_dark: BadgeColor,
    /// 相对全局倍率的**额外**倍率。`<= 0` 视作这一条不画。
    ///
    /// 两级而不是一级：全局那一级答「角标整体要多大」，这一级答「这一条要不要比别人
    /// 大或小」。出厂三条都取 1.0——都是同等重要的状态标记，没有理由分档。
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
    /// 是否全角。为真时在**右上角**画一个小方点。
    ///
    /// 与标点角标分处两角，是因为它们是两个正交的状态：挤在同一角就得设计一套组合
    /// 编码（四种搭配各长什么样），而 16px 上根本放不下那么多可辨的差异。
    ///
    /// 半角不画——与英文模式不画标点角标同一条判据：**没有信息量的状态不占像素**。
    /// 半角是常态，若给它也画一个标记，图标上就常驻一个永远不变的点，既没告诉用户
    /// 任何事，又挤占了本就稀缺的 16×16。
    pub full_width: bool,
    /// 整体变淡：**只给线程级 KEYBOARD_DISABLED**（输入法整个被禁用，罕见且严重）。
    ///
    /// ⚠ 不要把「焦点不在可编辑控件里」并进来——那是日常状态（点按钮/列表/桌面都会进），
    /// 旧实现试过并入，实测图标频繁变灰、用户无从理解，已改为与密码框一样显「英」。
    pub dimmed: bool,
    /// 动画相位，仅在演示模式下递增（见 [`IconRenderer::demo_animation`]）。
    ///
    /// 放进 spec 而非单独传参，是为了让发布器的"状态未变则跳过"判据自动把它算进去：
    /// 相位一变就是新内容，该重发；相位不变就该跳过。
    pub frame: u32,
    /// 主字显示的是否为英文态标签（非中文态 / 不可输入态 / CapsLock 都算）。
    ///
    /// 只用来挑主字色（见 [`TextColors`]），不影响字形——字形由 `label` 决定。
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
///
/// 这里拿的是**合并后的最终值**（用户 > 主题 > 内置，合并在协调器一处做完），
/// 渲染器不再回落。默认 = 改动前写死的颜色：浅色任务栏黑、深色任务栏白。
///
/// alpha 只作用于主字字形（字形覆盖度 × alpha）；`auto` 角标与演示跑马灯只取 RGB，
/// 它们的不透明度各有自己的来源（`badge_alpha` / 条目色值）。
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
    ///
    /// 用于把来自多处的挖空合成一张：主字只该被挖**一次**，分别挖两遍等于
    /// 让第二遍在第一遍的结果上再算一次覆盖度，交叠处会被削得比任何一处都狠。
    fn union(&mut self, other: &Mask) {
        for (d, s) in self.v.iter_mut().zip(other.v.iter()) {
            *d += s * (1.0 - *d);
        }
    }
}

/// 4×4 超采样画圆/环。`r_in > 0` 即为环。
///
/// 自己做超采样而不用现成的矢量库，是因为这里的图形只有圆和矩形两种，
/// 而在 5px 直径上，抗锯齿质量直接决定两态能否分辨——用得起精确的覆盖度积分。
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
///
/// 三角能把角落填满，同面积下比圆更醒目；而斜边朝向图标中心，与主字的接触面
/// 又比同样占地的方块小——这是它被选作角标唯一形状的原因。
fn draw_corner_triangle(m: &mut Mask, s: f32, leg: f32, corner: Corner) {
    const SS: i32 = 4;
    // 四个角落的判据同构：把坐标折到右下角，其余三个角就都变成同一个问题。
    // 与其写四份几乎相同的不等式（改一处忘另一处，且形状差异细到肉眼几乎看不出），
    // 不如只做一次坐标变换。
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

/// 把边界点映射到外圈周长上的归一化位置 `[0,1)`，顺时针：上 → 右 → 下 → 左。
///
/// 用「离哪条边最近」分区（等价于沿对角线切成四块），这样四个角上的像素归属明确，
/// 跑马灯扫过转角时不会出现断点或重叠。
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

/// 一层待合成的角标：形状、配套的挖空形状、以及这一层的颜色。
///
/// 把三者绑在一起而不是三个平行 `Vec`：层数由规则表决定，平行数组一旦长度对不齐
/// 就是"某个角标用了另一个角标的颜色"，而这种错在 16px 上肉眼几乎发现不了。
struct BadgeLayer {
    /// 角标本身。
    mask: Mask,
    /// 同一形状外扩 [`IconRenderer::BADGE_GAP`] 后的版本，用来从主字里挖出间隙。
    ///
    /// 只在本层**不透明**（`alpha >= 1.0`）时才真的拿去挖——挖空与半遮是互斥的两种
    /// 分离手段，见 [`IconRenderer::badge_alpha`]。
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
    ///
    /// `GetIcon` **没有尺寸参数**——图标多大由我们创建位图时决定，系统拿去后是否二次缩放
    /// 从接口上完全看不出来。开启本项部署一次，就能同时回答「系统挑了哪档」和
    /// 「有没有被缩放」两个问题，这是读代码推不出来的。
    pub size_marks: bool,
    /// 角标总开关，见 [`BadgeStyle`]。**默认关**。
    pub style: BadgeStyle,
    /// 角标规则表，**顺序即优先级**。
    ///
    /// 一条规则说的是「某状态成立时，在某角落用某色画一个角标」。位置进了规则之后，
    /// 「中文标点」与「全角」可以被配到同一个角落——16px 上叠两个三角只会糊成一片，
    /// 故**一个角落只画最靠前命中的那一条**。顺序即优先级是列表模型天然带的语义，
    /// 与 `ui.toolbar.items` 同一条规矩，不必另发明一个 priority 字段。
    ///
    /// 「某个状态不想被打扰」的表达是**把那条规则关掉**（配置侧 `enabled = false`，
    /// 转换时整条不进这张表），而不是给它一个透明色：透明与颜色是两件事，挤进一个
    /// 字段还会与 [`Self::badge_alpha`] 那条档位逻辑打架。
    pub rules: Vec<BadgeRule>,
    /// 角标不透明度的**全局默认**（0~1）。单条可用配置里的 `alpha_light/dark` 覆盖，
    /// 见 [`BadgeColor::alpha`]。
    ///
    /// **小于 1 时会同时关掉挖空**，得到"半遮"的效果。这条档位判据是**逐层**生效的
    /// （用该层的有效不透明度判），因为挖空在几何上本就是 per-badge 的操作——挖的是
    /// 这一个角标周围那圈主字。出厂让全部角标共用同一个值是审美上的统一
    /// （一个半遮一个实心会让人以为二者层级不同），不是技术限制。
    ///
    /// 这两件事是互斥的，不是叠加的：
    /// - `= 1.0`：实心角标 + 挖空。靠周围那圈留白与主字分离，代价是底下的笔画被切掉，
    ///   右下有笔画的「五」「双」「拼」看起来像缺了一角。
    /// - `< 1.0`：半透明角标 + 不挖空。笔画从角标里透出来，靠色差分离，字是完整的。
    ///
    /// 若两者同用（第一版就是），主字先被挖掉一圈、没有笔画可透，角标又被调淡，
    /// 于是调这个值**看起来毫无效果**——淡的只是角标自己，底下本来就是空的。
    ///
    /// 不要调到很低：角标要在 16px 的任务栏上一眼可辨，太透就等于没画。
    pub badge_alpha: f32,
    /// 角标全局大小倍率。1.0 = [`Self::CORNER_LEG`] 那个基准。
    ///
    /// 与 [`BadgeRule::scale`] 分两级：这一级答「角标整体要多大」，那一级答「这一条
    /// 要不要比别人小」。改一条规则的位置或颜色不该连带把调好的整体大小丢掉。
    pub badge_scale: f32,
    /// 演示模式：外圈跑马灯。纯粹展示"服务端渲染 + 定时重发"能做到什么，不表达状态。
    ///
    /// 开启后需要有人按帧推进 [`IconSpec::frame`] 并重新发布，否则画面是静止的——
    /// 渲染端只负责按相位画，不负责驱动时间。
    pub demo_animation: bool,
    /// 主字色（最终值），见 [`TextColors`]。
    pub text_colors: TextColors,
}

impl IconRenderer {
    /// 字体族与旧 C++ 实现保持一致，避免换渲染端时字形跟着变。
    const FONT_FAMILY: &'static str = "Microsoft YaHei UI";

    /// 主字字重，对齐旧 C++ 实现的 `DWRITE_FONT_WEIGHT_LIGHT`。
    ///
    /// 别用渲染器默认（400）：16px 下常规字重的汉字笔画会挤在一起，真机实测明显偏粗。
    /// 这里既是"看着更好"，也是"与用户习惯的旧图标一致"。
    const FONT_WEIGHT: i32 = 300;

    /// **被回缩过的**拉丁标签的字重（如 `[ui.labels]` 把英文态配成 `"En"`）。
    ///
    /// [`Self::FONT_WEIGHT`] 那个 300 是**为汉字定的**（见其文档：常规字重下 16px
    /// 的汉字笔画会挤在一起）。字母笔画少、没有那个问题，300 叠加回缩后的小字号
    /// 只剩发虚——真机实测 `"En"` 的 E 明显比周围文字细。
    ///
    /// ⚠️ **判据是"有没有真的被缩小"，不是"是不是字母"。** 这两条判据只在多字符标签上
    /// 一致，单字符会分道扬镳：`"A"` 行盒不到 8px、根本不进回缩，字号仍是满格的
    /// `s - FONT_SIZE_INSET`，给它加粗只会比改动前突兀（首版按字符集分档，真机实测被否）。
    ///
    /// ⚠️ 分档依据是标签自身，不是运行时状态：`"A"` 恒走 300、`"En"` 恒走 400，各自
    /// 稳定不跳。与本文件翻过两次车的"按 has_badge 分档"不是一回事。
    const FONT_WEIGHT_LATIN: i32 = 400;

    /// 纯拉丁标签的边缘留白（像素）。
    ///
    /// 汉字那档用 [`Self::FONT_SIZE_INSET`]，因为汉字墨迹几乎填满 advance；拉丁字母的
    /// advance 里天然含左右边距，只需留住描边与抗锯齿。用汉字那档去套字母，`"En"`
    /// （行盒 15.65，16px 画布本就差不多装得下）会被白缩掉两三个像素。
    const LATIN_EDGE_INSET: f32 = 1.0;

    /// 主字字号 = 图标边长 − 本值，与旧 C++ 实现的 `fontSizeDIP = iconSize - 2` 一致。
    ///
    /// **这是基线，不为新表现让步。** 角标是新增的东西，它的代价由角标自己承担
    /// （靠挖空间隙叠加），而不是把主字整体缩小——早期版本为让位缩到 78%，
    /// 真机对比比旧图标明显小一圈。
    const FONT_SIZE_INSET: f32 = 2.0;

    /// 宽标签回缩字号时的下限（相对基线字号的比例）。
    ///
    /// ⚠️ 这是**防御性下限，不是设计档位**：正常路径上 `avail / m.width` 这个比例
    /// 本身就保证装得下，用不着它。它只兜 measure 返回异常大宽度的情形（字体缺失、
    /// 后端异常），避免把字号算成 0 而画出一片空白。
    ///
    /// 定 0.4 是因为最宽的合法标签——两个全角字符（「符号」/「Ｅｎ」）——自然缩到
    /// 0.5 左右。下限若定在那之上就会**反过来生效**，把本可以装下的两字标签顶出
    /// 画布右缘，那正是这段代码要修的毛病。
    const MIN_FONT_SCALE: f32 = 0.4;

    /// 宽标签回缩后占可用宽度的比例。
    ///
    /// 不取满 1.0 的原因是 measure 给的是**行盒**：它既不含字形的 overhang，也不含
    /// 抗锯齿向外糊出的那半个像素。按行盒缩到"恰好等于可用宽"，实测「符号」在 16px
    /// 下仍会点亮最右一列。0.94 是留给这两项的余量。
    const WIDE_LABEL_SAFETY: f32 = 0.94;

    /// 角标周围挖空的间隙，按图标边长取比例（16px 下约 1.1px）。
    ///
    /// 没有它，角标与主字笔画会糊成一团——第一轮原型的「满格主字 + 角标直接叠加」
    /// 就是这么废掉的。间隙让两者在视觉上分离，主字因而不必缩小。
    const BADGE_GAP: f32 = 0.07;

    /// 外圈厚度（按边长比例）。16px 下约 1.1px——再细就被抗锯齿吃没了。
    const RING_TH: f32 = 0.07;

    /// 跑马灯亮段占周长的比例。
    const MARQUEE_LEN: f32 = 0.28;

    /// 演示动画一圈多少帧。帧率由驱动方决定，这里只定义"转一圈需要几帧"。
    pub const DEMO_FRAMES_PER_CYCLE: u32 = 40;

    /// 中文标点角标默认色 `#2288E0`（蓝，BGR 存储）。
    ///
    /// 与英文标点的橙 `#EE9922` 成对挑选：两者在浅色与深色任务栏上都够亮也够暗，
    /// 且色相相距足够远——16px 下角标只有几个像素，靠色相区分远比靠形状可靠。
    ///
    /// 这也是**形状不再兼任状态编码**的前提：早期版本靠「中实心 / 英空心」区分两态，
    /// 而 16px 下空心三角只剩一条 1px 的细边，英文态几乎看不见。
    pub const DEFAULT_PUNCT_CN_COLOR: [u8; 3] = [0xE0, 0x88, 0x22];

    /// 英文标点角标默认色 `#EE9922`（橙，BGR 存储），见 [`Self::DEFAULT_PUNCT_CN_COLOR`]。
    pub const DEFAULT_PUNCT_EN_COLOR: [u8; 3] = [0x22, 0x99, 0xEE];

    /// 全角标记默认色 `#E0447A`（玫红，BGR 存储）。
    ///
    /// 先试过绿 `#33BB55`，真机否决：**不够清晰**。原因是绿的感知亮度天生偏高，
    /// 在浅色任务栏上与白底拉不开，而 16px 上只有几个像素、没有面积去弥补对比。
    /// 玫红的感知亮度低得多，浅底上压得住；饱和度又足够，深底上也不会糊成一团。
    /// 与标点那两色（蓝 `#2288E0` / 橙 `#EE9922`）的色相距离同样够远，三者同屏不串。
    ///
    /// ⚠ 不要图省事让它与英文标点共用橙：两个不相干的状态用同一色，在出厂那种
    /// 一上一下的位置还能靠角落分辨，一旦用户把它们配到同一个角落就完全区分不出。
    pub const DEFAULT_FULL_WIDTH_COLOR: [u8; 3] = [0x7A, 0x44, 0xE0];

    /// 角标默认不透明度，见 [`Self::badge_alpha`]。
    ///
    /// 0.88：真机上 0.72 太淡，16px 下的标记本就只有几个像素，透得太狠就认不出颜色了。
    ///
    /// 仍要严格小于 1——等于 1 会切到挖空那一档（见 [`Self::badge_alpha`]），主字被
    /// 削掉一角，而半遮的全部意义就是不削字。这个上限不是审美取舍，是档位边界。
    pub const DEFAULT_BADGE_ALPHA: f32 = 0.88;

    /// 角标三角的基准直角边长（按图标边长取比例），对应倍率 1.0。
    ///
    /// 0.34 是真机调过的：0.42 时三角在 16px 上压得太重，抢了主字的视觉重心
    /// （用户原话「需要改小一点」）。再往下到 0.28 就开始糊成一个色块，直角三角的
    /// 形状特征消失、与圆点无异。
    const CORNER_LEG: f32 = 0.34;

    /// 墨迹居中的收敛阈值（像素）。
    ///
    /// 取 0.5 而不是更小，是因为 **0.5px 就是这条渲染管线的物理下限**：
    /// `IDWriteBitmapRenderTarget` 走 GDI 兼容渲染，基线被吸附到整像素，
    /// 亚像素的原点差异根本画不出区别（实测 y=0 与 y=-0.5 输出完全相同）。
    /// 阈值定得比这更小只会让迭代白跑满次数。
    const CENTER_TOL: f32 = 0.5;

    /// 居中最多重画几遍。
    ///
    /// 为什么不能一步到位：原点位移与墨迹位移**不是 1:1**（同上，基线吸附 + 包围盒按
    /// 整像素量），实测原点挪 3px 墨迹只挪 2px。单步牛顿必然欠冲——这正是第一版
    /// 只画两遍却仍偏 1.5px 的原因。三次足够收敛到 ±0.5px。
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

    /// 出厂规则表：中文标点右下蓝、英文标点右下橙、全角右上玫红，亮暗同色、大小相同。
    ///
    /// **三条的倍率都是 1.0**：旧版曾让全角标记小一档（0.28 对 0.34），那是实验期的
    /// 试探值，不是定论——三个状态标记同等重要，出厂就不该替用户分主次。要它小的人
    /// 自己改那一条的 `scale`。
    ///
    /// 亮暗两档写成同一个色是刻意的：这三色本就是按"深浅两种任务栏上都立得住"挑的
    /// （见各自的常量文档），分开配只会让出厂多出三个必须同步维护的值。按主题分开
    /// 配色是留给用户的自由度，不是出厂的负担。
    ///
    /// 两条标点规则同占右下角**不冲突**：它们的状态互斥（同一时刻只可能命中一条），
    /// 优先级规则处理的是「同时命中」，不是「配在同一角」。
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
        // 总开关关掉时 active_layers 直接返回空表，于是「关掉」在像素上必然与
        // 「此刻没有任何状态可显示」一字不差，不会留下什么残迹。
        let layers = self.active_layers(size_px, dark_theme, spec, fg3);

        // clear 是角标外扩一圈后的形状，用来在主字上"挖"出间隙。
        // 没有它，角标会与主字笔画糊成一团（第一轮原型的方案 C 就是这么废掉的）。
        // ★ **挖空与透明是互斥的两种分离手段，不能叠加。**
        //
        // - 挖空：在角标周围切掉一圈主字，靠"留白"把两者分开。角标是实心的，
        //   它底下的笔画被切掉了，看不见。
        // - 透明：让笔画从角标里透出来，靠"色差"把两者分开。这才是**半遮**。
        //
        // 同时用是最差的组合：主字先被挖掉一圈（没有笔画可透），角标又被调淡
        // （不够醒目）——于是调低不透明度"看起来完全没有效果"，因为角标底下
        // 本来就是空的，淡的只是它自己。这正是第一版的实际表现。
        //
        // 全部角标并进**同一张** clear：主字只该被挖一次，每条各挖各的会让交叠处
        // 被削得更狠。
        //
        // 档位判据是**逐层**的：只有不透明的那些层参与挖空，半透明的层保留主字。
        // 挖空在几何上本就是 per-badge 的（挖的是这一个角标周围那圈），条目能各自
        // 指定不透明度之后，这条判据自然跟着降到层上。
        let mut clear = Mask::new(n);
        for l in layers.iter().filter(|l| l.alpha >= 1.0) {
            clear.union(&l.clear);
        }

        // 演示动画独立成层：它不表达状态，也不参与挖空，纯粹叠在最上面。
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
            // 挖空必须发生在叠加之前，否则会把角标自己也挖掉。
            //
            // 色值按**预乘**累加，最后除以合成 alpha 还原成非预乘——输出给
            // `CreateIconIndirect` 的 `hbmColor` 必须是非预乘的。
            //
            // 写成循环而不是把每层权重展开成一条乘法链：层数现在由规则表决定，
            // 展开式每加一层就要给之前每一项补一个 `(1 - a)` 因子，漏一个不报错，
            // 只让某一层的颜色偏一点——16px 上根本看不出来。
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

    /// 主字蒙版：黑底画白字，取 max(R,G,B) 当覆盖度。
    ///
    /// 见模块文档——不能直接在透明缓冲上画字，那样文字会被按 alpha=0 预乘成全透明。
    ///
    /// ## 为什么要画两遍
    ///
    /// **版面盒 ≠ 墨迹盒。** `measure` 返回的是 DirectWrite 的行盒：宽含字符的左右边距、
    /// 高是 ascent + descent + lineGap。把行盒摆正中，字形在盒内本就不居中（CJK 字面
    /// 在 em 框里偏上，行间距又只加在下方），于是整个字肉眼可见地偏上偏左——加了最外圈
    /// 边框作参照后这一点特别明显，这正是本次要修的。
    ///
    /// 修法是先照旧画一遍，**从画出来的蒙版量真实墨迹的包围盒**，再按差量重画一遍。
    /// 不用别的办法的理由：DirectWrite 虽有 `GetOverhangMetrics`，但它给的是相对行盒的
    /// 溢出量、仍受行盒定义影响；而"字形实际点亮了哪些像素"才是我们要对齐的东西，
    /// 直接量输出是唯一不依赖任何度量约定的口径，换字体换字号都不会失准。
    ///
    /// 代价是每个变体多画一次（十个变体 ≈ 20 次小面积排版），只在状态变化时发生，可忽略。
    fn render_glyph_mask(&self, size_px: u16, spec: &IconSpec) -> Mask {
        let s = size_px as f32;

        // 字号**与角标有无完全无关**，恒等于旧 C++ 实现的取值。
        //
        // 两次踩坑都在这一行：先是为给角标让位把字缩到 78%（比旧图标明显小一圈），
        // 又因为按 has_badge 分档，导致英文态（无角标）走满格、中文态走 78%，
        // 每次中英切换字号肉眼可见地跳。图标统共一个字，它的尺寸就是基线本身。
        let font_size = s - Self::FONT_SIZE_INSET;
        let mut fs = font_size;
        let mut style =
            crate::text::dwrite::TextStyle::new(font_size).with_weight(Self::FONT_WEIGHT);

        // 第一遍按行盒粗定位。测量必须与绘制同一个 TextStyle——字重影响字宽，
        // 用 measure_text_sized（不带字重）测出来的宽度会与实际绘制不符。
        let mut m = self.text.measure(&spec.label, &style);

        // 装不下就按**实测宽度**回缩字号。标签自 `[ui.labels]` 起可配成两个字符
        // （英文态 "En"），而画布只有 16px、字号写死 `s - INSET`：`"En"` 在该字号下
        // 宽约 15.7px，下面 x0 的 `.max(0.0)` 会把它钳到左对齐，右半个字母直接画到
        // 画布外被裁掉——用户看到的是 `E` 加半个 `n`。
        //
        // ★ 判据必须是 measure 的实测宽度，**不能按字符数分档**：用户可以配全角
        // 「Ｅｎ」，那是 2 个 char 却有 2 个汉字宽，字符数判断兜不住，measure 兜得住。
        //
        // ⚠️ 这**不是**上面注释里翻过两次车的那种"字号分档"。那次的依据是运行时状态
        // （有无角标），于是同一个「中」字会随中英切换忽大忽小；这里的依据是标签自身
        // 的宽度，「五」恒 1 字符、"En" 恒 2 字符，各自字号稳定不跳，切换时大小不同
        // 是因为**字数本来就不同**。别照着那条注释把这段删掉。
        //
        // ★ 可用宽度按**是否含汉字**分两档，同一个数值套不了两者：
        //
        // 汉字的墨迹几乎填满 advance，行盒宽等于可用宽时就已经漏出边缘，必须留足；
        // 拉丁字母的 advance 里天然含左右边距，墨迹远窄于行盒，占满画布也不会触边。
        // 拿汉字那档去套字母，`"En"`（行盒 15.65，本就差不多装得下）会被缩到 11px
        // 上下——真机实测「E 发虚」，正是这么来的。
        let avail = if Self::has_cjk(&spec.label) {
            s - Self::FONT_SIZE_INSET
        } else {
            s - Self::LATIN_EDGE_INSET
        };
        // 目标宽度比可用宽再收一点（`WIDE_LABEL_SAFETY`）：缩到"行盒宽恰好等于可用宽"
        // 实测仍会触边——**行盒不含墨迹的 overhang，抗锯齿也会向外糊出半个像素**。
        let target = avail * Self::WIDE_LABEL_SAFETY;
        let min_size = font_size * Self::MIN_FONT_SCALE;

        // ★ 字重补偿**只给真的被缩小了的拉丁标签**，判据是 `m.width > target`
        // （下面那个循环的进入条件）。
        //
        // 补偿的理由是**字号回缩带来的变细**，不是"字母天生该更粗"：`"En"` 缩到
        // 12px 上下，配 300（Light）就发虚。而单字符的 `"A"` 行盒不到 8px、根本不进
        // 回缩，字号仍是满格的 `s - INSET`——给它加粗只会比改动前突兀，真机实测被否。
        //
        // ⇒ 装得下的标签（「英」「五」`"A"`）走的是与本功能上线前**逐像素相同**的路径。
        let weight = Self::label_weight(&spec.label, m.width, target);
        // 字重影响字宽，换了就必须重测——否则下面的收敛判据用的是另一种字重的宽度。
        if weight != Self::FONT_WEIGHT {
            style = crate::text::dwrite::TextStyle::new(font_size).with_weight(weight);
            m = self.text.measure(&spec.label, &style);
        }

        // ★ 必须**循环收敛**，一次比例换算不够：排版引擎把字号吸附到整像素，算出的
        // 6.58px 实际按 7px 排版，两个汉字的宽度仍是满格的 14 —— 比例算法在吸附值上
        // 原地打转，画出来还是被裁掉右缘。每轮至少降 1px 才能真正走出那一档。
        //
        // 循环必然终止：`fs` 每轮严格减小（`min(by_ratio, fs - 1.0)`），且以 `min_size`
        // 收底。实测最多两轮（「符号」14 → 7 → 6）。
        while m.width > target && m.width > 0.0 && fs > min_size {
            fs = (fs * target / m.width).min(fs - 1.0).max(min_size);
            style = crate::text::dwrite::TextStyle::new(fs).with_weight(weight);
            m = self.text.measure(&spec.label, &style);
        }
        let x0 = ((s - m.width) * 0.5).max(0.0);
        let y0 = ((s - m.height) * 0.5).max(0.0);
        let mut mask = self.draw_glyph_at(size_px, &style, &spec.label, x0, y0);

        // 逐次按墨迹残差校正。无墨迹（非 Windows 的 mock 后端）时 delta 为 None，直接跳过。
        //
        // 不必担心把字挤出画布：这里求的是"让墨迹盒正中"的位移，它同时也是让溢出最小的
        // 位移——原本装得下的必然仍装得下，原本装不下的也只会更好。
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
            // 不再改善就收手并保留上一版。吸附使残差呈阶梯状，硬追下去会在两个
            // 相邻整像素位置之间来回跳，跑满次数还回到更差的那一边。
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

    /// 该给这个标签用哪个字重。
    ///
    /// `base_width` 是基线字号 + 基线字重下的行盒宽，`target` 是回缩目标宽度——
    /// 两者一比就知道这个标签**会不会被缩小**，而那正是要不要补偿字重的唯一依据。
    /// 详见 [`Self::FONT_WEIGHT_LATIN`]。
    ///
    /// 抽成纯函数是为了能直接断言判据本身：走渲染结果去反推字重，会被居中校正的
    /// 亚像素位移搅进来，判据变成在测抗锯齿噪声。
    fn label_weight(label: &str, base_width: f32, target: f32) -> i32 {
        if base_width > target && !Self::has_cjk(label) {
            Self::FONT_WEIGHT_LATIN
        } else {
            Self::FONT_WEIGHT
        }
    }

    /// 标签里是否含汉字。字重与可用宽度都按它分档——两处必须用**同一个**判据，
    /// 否则会出现"按字母留白、却按汉字上字重"这种半档状态。
    ///
    /// 只判 CJK 表意文字三段，不判"是否 ASCII"：全角「Ｅｎ」不是 ASCII 却是字母，
    /// 用 ASCII 判据会把它错分到汉字档、白缩一截。私用区（拆字字根）也不在此列，
    /// 它们不会出现在模式主字里。
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

    /// 当前状态下要画的全部角标层，按规则表顺序。
    ///
    /// 三件事在这里一次做完，因为它们共享同一次遍历、也共享同一条优先级判据：
    /// 按状态过滤 → 同角落去重（只留最靠前的）→ 按明暗主题取色。
    ///
    /// 总开关关掉时返回空表，调用方无需另加分支。
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
            // 一个角落只画最靠前命中的那一条：16px 上叠两个三角只会糊成一片，
            // 而"哪个在上"若不定死，表现会随规则表的遍历顺序漂移。
            if used.contains(&r.corner) {
                continue;
            }
            let k = self.badge_scale.max(0.0) * r.scale;
            // 倍率归零视作这一条不画，**连挖空一起**短路。只短路形状那一张是不够的：
            // 挖空版的边长是 leg + gap，leg 为 0 时它仍有 gap 那么大，结果是主字被
            // 挖掉一块、却没有任何东西补上去。
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
                // 条目没指定就用全局。在这里合并而不是留到合成循环里，是为了让
                // 「挖不挖空」与「画多淡」读的是同一个值——两处各算一遍就会出现
                // 「挖了空却又是半透明的」那种自相矛盾的组合。
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
        // 倍率只缩放形状本身，**不缩放 expand**：expand 是主字与角标之间的间隙，
        // 由"多宽才不糊在一起"决定，与角标多大无关。跟着一起缩会让角标调小时
        // 间隙也变窄，恰好在最需要间隙的时候把它收掉。
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
    /// 上次发布的状态。图标更新是用户操作级频率，但状态推送比它频繁得多
    /// （焦点切换等也会推），没必要每次都重渲十张位图。
    last: Option<IconSpec>,
    /// 演示动画的当前相位。
    ///
    /// 归发布器所有而不是由调用方每次传入：普通状态推送与动画定时器都会走到 `publish`，
    /// 若相位由调用方给，状态推送那条路必须知道「现在动画转到哪了」才能不打断它——
    /// 那等于把动画状态复制到每个调用点。放在这里，状态推送只管状态，相位自然延续。
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

    /// 演示动画（外圈跑马灯）开关。关掉时相位归零，下次开启从起点转起。
    ///
    /// 只切开关不会让画面动起来——还需要有人按帧调 [`Self::advance_demo_frame`] 并重新
    /// 发布。渲染端只按相位画，不持有时间。
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

    /// 一次性套用全部呈现参数（配置侧的落地入口）。
    ///
    /// 每项都是 `Option`，`None` = 该项不动（保留渲染器自带的默认）。收成一个函数而不是
    /// 让调用方逐字段赋值，是为了让「改了参数必须清 `last` 才会重发」这件事只有一处
    /// 需要记得——漏清的症状是「配置改了、日志说读到了、图标纹丝不动」。
    ///
    /// 返回是否**确实有改动**，调用方据此决定要不要重新发布。
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
    ///
    /// 返回序号而非布尔，是为了让服务端日志记下「这是第几版位图」。排查「图标落后一帧」
    /// 一类问题时，服务端只能看到自己发布了什么、DLL 只能看到自己读到了什么，两边日志
    /// 唯一能对上号的量就是这个序号——它同时是读端 seqlock 的判据（SHM header 的
    /// `sequence`），不是为日志另造的计数器。
    pub fn publish(&mut self, spec: &IconSpec) -> Result<Option<u32>, String> {
        if self.last.as_ref() == Some(spec) {
            return Ok(None);
        }

        // 用变体表驱动渲染，而不是另写一遍嵌套循环——两处循环各写一遍时，
        // 一旦变体表的顺序或档位变了而这里没跟上，图标就会张冠李戴
        // （某个尺寸档显示另一档的内容），且不会有任何报错。
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
