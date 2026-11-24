//! 异形皮肤（`[skin]`）：schema 与求值。设计见 `docs/design/theme-skin-animation.md`。
//!
//! 皮肤是窗口级的整体设计：横排 / 竖排 / 嵌入变体各是一整套，互不深合并；base 链上子主题
//! 写了某个变体就整表替换 base 的同名变体（见 [`replace_inherited_variants`]）。
//!
//! 坐标一律是**主图源像素**；渲染期乘倍率 `k`（见 [`RvSkin::factor`]）换算成设备像素。

use serde::Deserialize;
use toml::Value;

use crate::rvnode::Flip;

/// 皮肤变体键。顺序无意义；嵌入变体缺省回退对应的非嵌入变体。
pub const SKIN_VARIANTS: [&str; 4] = [
    "horizontal",
    "vertical",
    "horizontal_embedded",
    "vertical_embedded",
];

// ── schema（serde 目标） ──

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Skin {
    /// dpi（默认）| font。
    pub scale_with: Option<String>,
    /// 源像素 / dp。缺省 1。
    pub density: Option<f32>,
    /// `scale_with = "font"` 时的基准字号（逻辑 pt）。缺省 16。
    pub font_base: Option<f32>,
    pub horizontal: Option<SkinVariant>,
    pub vertical: Option<SkinVariant>,
    pub horizontal_embedded: Option<SkinVariant>,
    pub vertical_embedded: Option<SkinVariant>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct SkinVariant {
    pub frame: Option<SkinFrame>,
    #[serde(default)]
    pub content: SkinInsets,
    #[serde(default)]
    pub decor: Vec<SkinDecor>,
    #[serde(default)]
    pub placement: SkinPlacement,
    /// 分槽位（`free`）布局：写了 `slots.candidates` 即启用，编码 / 候选 / 翻页各自锚定。
    #[serde(default)]
    pub slots: SkinSlots,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct SkinSlots {
    pub preedit: Option<SlotSpec>,
    pub candidates: Option<SlotSpec>,
    pub footer: Option<SlotSpec>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct SlotSpec {
    /// 缺省 `content.top_left`。
    #[serde(default)]
    pub anchor: String,
    /// 槽位自身对齐点；缺省同锚点方位。
    pub pivot: Option<String>,
    /// 源像素。
    #[serde(default)]
    pub offset: [f32; 2],
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct SkinFrame {
    #[serde(default)]
    pub image: String,
    /// 覆盖 `skin.density`。
    pub density: Option<f32>,
    pub stretch_x: Option<Vec<BandSpec>>,
    pub stretch_y: Option<Vec<BandSpec>>,
    /// 填法：stretch | tile | round。写了拉伸带时它是各带的缺省（不写为 round：带里的花纹按整数次
    /// 重复，节奏与两侧固定段一致；拉伸会把针脚、纸纹拉长，和固定段对不上）；没写拉伸带、整轴
    /// 缩放时它是整轴的填法（不写为 stretch）。
    pub fill: Option<String>,
    #[serde(default)]
    pub srcset: Vec<SrcSetEntry>,
    /// 窗体**不支持** `flip`：拉伸带与 content 按源图写，翻了像素就和它们对不上。只为认出这个
    /// 写错的键、告警后忽略（serde 对未知键静默，作者会以为生效了）。
    pub flip: Option<Value>,
}

/// 拉伸带：`[from, to]` 或 `{ from, to, fill }`。
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum BandSpec {
    Pair([f32; 2]),
    Table {
        from: f32,
        to: f32,
        fill: Option<String>,
    },
}

#[derive(Deserialize, Debug, Clone)]
pub struct SrcSetEntry {
    pub image: String,
    pub density: f32,
}

#[derive(Deserialize, Debug, Default, Clone, Copy, PartialEq)]
pub struct SkinInsets {
    #[serde(default)]
    pub left: f32,
    #[serde(default)]
    pub top: f32,
    #[serde(default)]
    pub right: f32,
    #[serde(default)]
    pub bottom: f32,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct SkinDecor {
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub anchor: String,
    pub pivot: Option<String>,
    #[serde(default)]
    pub offset: [f32; 2],
    /// 目标尺寸（源像素）；缺省取图片自身尺寸（按其 density 换算到主图坐标）。
    pub size: Option<[f32; 2]>,
    /// 覆盖 `skin.density`。
    pub density: Option<f32>,
    #[serde(default)]
    pub z: i32,
    /// drag（缺省，不透明处按下拖动候选窗）| block（接住鼠标不响应）| pass（点击穿透到底下的应用）。
    /// 点击互动（如点一下换表情）留给动画事件（设计 §6），届时另加字段，不复用 `hit`。
    pub hit: Option<String>,
    pub opacity: Option<f32>,
    /// 动画装饰的帧率，覆盖文件自带帧时长；非正数忽略。
    pub fps: Option<f32>,
    /// 镜像：x（左右）| y（上下）| xy；缺省不翻。只翻像素：anchor / pivot / offset / size /
    /// 命中矩形按源图写、不变。作用于本装饰的全部帧，含 `states` / `on` 引用的附加文件。
    pub flip: Option<String>,
    /// 状态片段：名 → `{ frames = "a-b" }` 或 `{ image = "x" }`（设计 §16）。逐条宽松解析：
    /// 写错的条目求值时 warn 并丢弃，不连累整份皮肤。
    #[serde(default)]
    pub states: toml::Table,
    /// 事件 → `{ state = "名" }` 或 `{ play = 片段, then = "名" }`（`then` 缺省 `@prev`）。
    #[serde(default)]
    pub on: toml::Table,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct SkinPlacement {
    pub caret: Option<CaretSpec>,
    pub caret_above: Option<CaretSpec>,
    /// 窗体与装饰的不透明像素不得压住光标行上的保护带（缺省开）。`true` / `false`，或
    /// `{ before = 200, after = 40 }`：保护带从光标向左 / 向右的宽度（dp）。
    pub avoid_caret: Option<AvoidSpec>,
    /// frame（缺省）：只保证窗体在屏内 | all：连装饰一起。
    pub clamp: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum AvoidSpec {
    On(bool),
    Strip {
        before: Option<f32>,
        after: Option<f32>,
    },
}

/// 光标行保护带：从光标向左 / 向右的宽度（dp）。左边是刚输入的文字，默认留得宽。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AvoidStrip {
    pub before: f32,
    pub after: f32,
}

impl Default for AvoidStrip {
    fn default() -> Self {
        Self {
            before: 200.0,
            after: 40.0,
        }
    }
}

/// 对齐点：`"frame.top_left"` 或 `{ anchor = "frame.top_left", offset = [180, 260] }`（源像素）。
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum CaretSpec {
    Name(String),
    Table {
        #[serde(default)]
        anchor: String,
        #[serde(default)]
        offset: [f32; 2],
    },
}

// ── 求值形态 ──

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum BandFill {
    #[default]
    Stretch,
    Tile,
    Round,
}

impl BandFill {
    fn parse(s: Option<&str>) -> Option<Self> {
        match s? {
            "stretch" => Some(Self::Stretch),
            "tile" => Some(Self::Tile),
            "round" => Some(Self::Round),
            other => {
                tracing::warn!("skin: 未知拉伸带填法 {other:?}，按 stretch");
                Some(Self::Stretch)
            }
        }
    }
}

/// 一条拉伸带：源像素半开区间 `[from, to)`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub from: f32,
    pub to: f32,
    pub fill: BandFill,
}

/// 某轴的拉伸声明。
#[derive(Clone, Debug, PartialEq)]
pub enum Stretch {
    /// 未写：整轴拉伸。
    Whole(BandFill),
    /// 写了空数组：该轴不可伸缩。
    Fixed,
    Bands(Vec<Band>),
}

/// 九方位。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Pos9 {
    /// 0 = 左 / 上，1 = 中，2 = 右 / 下。
    pub h: u8,
    pub v: u8,
}

impl Pos9 {
    pub const TOP_LEFT: Self = Self { h: 0, v: 0 };
    pub const BOTTOM_LEFT: Self = Self { h: 0, v: 2 };

    /// `top_left` / `top-left` / `top` / `center` / `bottom_right` …；无法识别返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.replace('-', "_");
        let (mut h, mut v) = (1u8, 1u8);
        for part in s.split('_').filter(|p| !p.is_empty()) {
            match part {
                "left" => h = 0,
                "right" => h = 2,
                "top" => v = 0,
                "bottom" => v = 2,
                "center" | "middle" => {}
                _ => return None,
            }
        }
        Some(Self { h, v })
    }

    /// 盒 `(x, y, w, h)` 上该方位点的坐标。
    pub fn point(self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
        (x + w * self.h as f32 / 2.0, y + h * self.v as f32 / 2.0)
    }
}

/// 锚点所在的盒。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AnchorBox {
    #[default]
    Frame,
    Content,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Anchor {
    pub on: AnchorBox,
    pub pos: Pos9,
}

impl Anchor {
    /// `frame.top_right` / `content.bottom_left`；省略前缀视为 `frame`。
    pub fn parse(s: &str) -> Option<Self> {
        let (on, pos) = match s.split_once('.') {
            Some(("frame", p)) => (AnchorBox::Frame, p),
            Some(("content", p)) => (AnchorBox::Content, p),
            Some((other, _)) => {
                tracing::warn!("skin: 未知锚点盒 {other:?}（P1 只支持 frame / content）");
                return None;
            }
            None => (AnchorBox::Frame, s),
        };
        Some(Self {
            on,
            pos: Pos9::parse(pos)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DecorHit {
    /// 不透明处接住鼠标，按下拖动候选窗（与拖窗体一致）。
    #[default]
    Drag,
    /// 接住鼠标但不响应（贴纸）。
    Block,
    /// 整块点击穿透到底下的应用。
    Pass,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RvSkinFrame {
    pub image: String,
    pub density: f32,
    pub stretch_x: Stretch,
    pub stretch_y: Stretch,
    /// 已按 density 升序。
    pub srcset: Vec<(String, f32)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RvSkinDecor {
    /// 在 TOML `[[skin.<变体>.decor]]` 里的原始下标（从 0 起，含被丢弃的项）；求值后按 z 排序，
    /// 本表下标不再等于它。指定义这张变体表的那份 TOML（变体继承自 base 时是 base 的）。
    pub src_index: usize,
    pub image: String,
    pub density: f32,
    pub anchor: Anchor,
    pub pivot: Pos9,
    pub offset: [f32; 2],
    pub size: Option<[f32; 2]>,
    pub z: i32,
    pub hit: DecorHit,
    pub opacity: f32,
    /// 覆盖动画文件自带帧时长的帧率；None = 用文件自带的。
    pub fps: Option<f32>,
    /// 镜像（各源合成到目标尺寸后再翻）。
    pub flip: Flip,
    /// 事件状态机；没写 `states` / `on` 时为空（整个文件循环，同 P4）。
    pub anim: RvDecorAnim,
}

/// 装饰的一段片段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecorClip {
    /// 本装饰文件的帧区间（含两端，文件原始帧号）。
    Frames { first: u32, last: u32 },
    /// 另一个文件，整段。
    Image(String),
}

/// 装饰状态机认的事件（设计 §16）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorEvent {
    Show,
    Hide,
    Typing,
    Backspace,
    Page,
    Empty,
    Commit,
    ModeCn,
    ModeEn,
    /// 显示中、距最后一个事件这么多秒。
    Idle(u32),
    Click,
}

impl DecorEvent {
    /// `show` / `typing` / `idle_5s` …；无法识别返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "show" => Self::Show,
            "hide" => Self::Hide,
            "typing" => Self::Typing,
            "backspace" => Self::Backspace,
            "page" => Self::Page,
            "empty" => Self::Empty,
            "commit" => Self::Commit,
            "mode_cn" => Self::ModeCn,
            "mode_en" => Self::ModeEn,
            "click" => Self::Click,
            _ => {
                let n: u32 = s.strip_prefix("idle_")?.strip_suffix('s')?.parse().ok()?;
                return (n > 0).then_some(Self::Idle(n));
            }
        })
    }
}

/// 事件触发的动作。状态以 [`RvDecorAnim::states`] 的下标引用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecorAction {
    /// 切换常驻状态。
    State(usize),
    /// 一次性片段，播完转 `then`；None = `@prev`（回到当前常驻状态）。
    Play {
        clip: DecorClip,
        then: Option<usize>,
        /// 播放期间不被其它事件打断：`state` 事件只记下、播完才生效，别的一次性片段丢弃。
        lock: bool,
    },
}

/// 一块装饰的事件状态机声明（求值后）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RvDecorAnim {
    /// 按名字排序（toml 表不保留书写顺序）。
    pub states: Vec<(String, DecorClip)>,
    /// 初始状态：`idle`，没有则取 `states` 的第一个；没有状态为 None。
    pub initial: Option<usize>,
    pub on: Vec<(DecorEvent, DecorAction)>,
}

impl RvDecorAnim {
    /// 事件对应的动作（同一事件有多条时取第一条，即主条；`alt` 展开的备选排在它后面）。
    pub fn action(&self, ev: DecorEvent) -> Option<&DecorAction> {
        self.on.iter().find(|(e, _)| *e == ev).map(|(_, a)| a)
    }
}

/// `"a-b"`（含两端）/ `"a"` / 整数 → 帧区间。
fn parse_frames(v: &Value) -> Option<DecorClip> {
    let (first, last) = match v {
        Value::Integer(n) => (u32::try_from(*n).ok()?, u32::try_from(*n).ok()?),
        Value::String(s) => match s.split_once('-') {
            Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
            None => {
                let n = s.trim().parse().ok()?;
                (n, n)
            }
        },
        _ => return None,
    };
    (first <= last).then_some(DecorClip::Frames { first, last })
}

/// 片段：帧区间字符串（`play = "36-47"`），或 `{ frames = .. }` / `{ image = .. }`。
fn parse_clip(v: &Value) -> Option<DecorClip> {
    let Some(t) = v.as_table() else {
        return parse_frames(v);
    };
    if let Some(f) = t.get("frames") {
        return parse_frames(f);
    }
    t.get("image")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| DecorClip::Image(s.to_string()))
}

fn resolve_decor_anim(states: &toml::Table, on: &toml::Table) -> RvDecorAnim {
    let states: Vec<(String, DecorClip)> = states
        .iter()
        .filter_map(|(name, v)| match parse_clip(v) {
            Some(c) => Some((name.clone(), c)),
            None => {
                tracing::warn!("skin: decor.states.{name} 不是合法片段，忽略");
                None
            }
        })
        .collect();
    let find = |name: &str| states.iter().position(|(n, _)| n == name);
    // `parent` 是 `alt` 备选所属的主条：备选没写 `then` / `lock` 时沿用主条的。
    let action = |v: &Value, parent: Option<&toml::Table>| -> Option<DecorAction> {
        let t = v.as_table()?;
        if let Some(s) = t.get("state") {
            return Some(DecorAction::State(s.as_str().and_then(find)?));
        }
        let clip = parse_clip(t.get("play")?)?;
        let then = match t.get("then").or_else(|| parent?.get("then")) {
            None => None,
            Some(Value::String(s)) if s == "@prev" => None,
            Some(s) => Some(s.as_str().and_then(find)?),
        };
        let lock = t
            .get("lock")
            .or_else(|| parent?.get("lock"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Some(DecorAction::Play { clip, then, lock })
    };
    let mut out: Vec<(DecorEvent, DecorAction)> = Vec::new();
    for (name, v) in on {
        let Some(ev) = DecorEvent::parse(name) else {
            tracing::warn!("skin: 未知装饰事件 {name:?}，忽略");
            continue;
        };
        let Some(a) = action(v, None) else {
            tracing::warn!("skin: decor.on.{name} 写法不对或引用了不存在的状态，忽略");
            continue;
        };
        out.push((ev, a));
        // `alt = [ { play = .. }, .. ]`：同一事件的备选动作，触发时与主条等概率随机挑一条。
        // 旧版本不认 `alt`，只会按主条播，所以主条必须是一份自足的写法。
        if let Some(alts) = v.as_table().and_then(|t| t.get("alt")) {
            let Some(alts) = alts.as_array() else {
                tracing::warn!("skin: decor.on.{name}.alt 不是数组，忽略");
                continue;
            };
            for (i, alt) in alts.iter().enumerate() {
                match action(alt, v.as_table()) {
                    Some(a) => out.push((ev, a)),
                    None => tracing::warn!(
                        "skin: decor.on.{name}.alt[{i}] 写法不对或引用了不存在的状态，忽略"
                    ),
                }
            }
        }
    }
    let on = out;
    RvDecorAnim {
        initial: find("idle").or((!states.is_empty()).then_some(0)),
        states,
        on,
    }
}

/// 对齐点：锚点 + 偏移（主坐标源像素）。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct CaretPoint {
    pub anchor: Anchor,
    pub offset: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Clamp {
    #[default]
    Frame,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RvPlacement {
    pub caret: CaretPoint,
    pub caret_above: CaretPoint,
    /// None = 不避让。
    pub avoid_caret: Option<AvoidStrip>,
    pub clamp: Clamp,
}

impl Default for RvPlacement {
    /// 默认对齐窗体角：窗体本身不会压住光标所在行。
    fn default() -> Self {
        Self {
            caret: CaretPoint {
                anchor: Anchor {
                    on: AnchorBox::Frame,
                    pos: Pos9::TOP_LEFT,
                },
                offset: [0.0; 2],
            },
            caret_above: CaretPoint {
                anchor: Anchor {
                    on: AnchorBox::Frame,
                    pos: Pos9::BOTTOM_LEFT,
                },
                offset: [0.0; 2],
            },
            avoid_caret: Some(AvoidStrip::default()),
            clamp: Clamp::Frame,
        }
    }
}

fn resolve_caret(spec: Option<&CaretSpec>, dflt: CaretPoint) -> CaretPoint {
    let (name, offset) = match spec {
        None => return dflt,
        Some(CaretSpec::Name(n)) => (n.as_str(), [0.0; 2]),
        Some(CaretSpec::Table { anchor, offset }) => (anchor.as_str(), *offset),
    };
    let anchor = if name.is_empty() {
        dflt.anchor
    } else {
        Anchor::parse(name).unwrap_or(dflt.anchor)
    };
    CaretPoint { anchor, offset }
}

/// 一个槽位：内容按自然尺寸摆放，`pivot` 点落在 `anchor` + `offset` 上。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RvSlot {
    pub anchor: Anchor,
    pub pivot: Pos9,
    pub offset: [f32; 2],
}

/// 分槽位布局。候选槽必有；编码槽缺省时编码栏叠在候选槽上方一起摆，翻页槽缺省时翻页栏按原规则
/// 留在候选区里。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RvSlots {
    pub preedit: Option<RvSlot>,
    pub candidates: RvSlot,
    pub footer: Option<RvSlot>,
}

fn resolve_slot(s: &SlotSpec) -> Option<RvSlot> {
    let anchor = Anchor::parse(if s.anchor.is_empty() {
        "content.top_left"
    } else {
        &s.anchor
    })?;
    let pivot = match s.pivot.as_deref() {
        Some(p) => Pos9::parse(p)?,
        None => anchor.pos,
    };
    Some(RvSlot {
        anchor,
        pivot,
        offset: s.offset,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct RvSkinVariant {
    pub frame: RvSkinFrame,
    pub content: SkinInsets,
    /// 已按 z 升序（稳定排序，同 z 保持书写顺序）。
    pub decor: Vec<RvSkinDecor>,
    pub placement: RvPlacement,
    /// None = `stack` 布局（编码 / 候选 / 翻页照旧纵向堆叠进内容框）。
    pub slots: Option<RvSlots>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ScaleWith {
    #[default]
    Dpi,
    Font,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RvSkin {
    pub scale_with: ScaleWith,
    /// 主图密度（源像素 / dp），各变体的坐标都按它写。
    pub density: f32,
    pub font_base: f32,
    pub horizontal: Option<RvSkinVariant>,
    pub vertical: Option<RvSkinVariant>,
    pub horizontal_embedded: Option<RvSkinVariant>,
    pub vertical_embedded: Option<RvSkinVariant>,
}

impl RvSkin {
    /// 选变体：嵌入变体缺省回退非嵌入；竖排不回退横排（异形图兼容不了两种形态）。
    pub fn variant(&self, vertical: bool, embedded: bool) -> Option<&RvSkinVariant> {
        let (plain, emb) = if vertical {
            (&self.vertical, &self.vertical_embedded)
        } else {
            (&self.horizontal, &self.horizontal_embedded)
        };
        if embedded {
            emb.as_ref().or(plain.as_ref())
        } else {
            plain.as_ref()
        }
    }

    /// 源像素 → 设备像素的倍率。`dpi_scale` 为显示器缩放，`font_size` 为生效基准字号（逻辑）。
    pub fn factor(&self, dpi_scale: f32, font_size: f32) -> f32 {
        let s = match self.scale_with {
            ScaleWith::Dpi => dpi_scale,
            ScaleWith::Font => dpi_scale * font_size / self.font_base,
        };
        s / self.density
    }
}

const DEFAULT_FONT_BASE: f32 = 16.0;

fn positive(v: Option<f32>) -> Option<f32> {
    v.filter(|d| d.is_finite() && *d > 0.0)
}

fn resolve_stretch(bands: Option<&Vec<BandSpec>>, fill: Option<BandFill>) -> Stretch {
    let Some(bands) = bands else {
        return Stretch::Whole(fill.unwrap_or(BandFill::Stretch));
    };
    let default_fill = fill.unwrap_or(BandFill::Round);
    if bands.is_empty() {
        return Stretch::Fixed;
    }
    let mut out: Vec<Band> = bands
        .iter()
        .filter_map(|b| {
            let (from, to, fill) = match b {
                BandSpec::Pair([a, b]) => (*a, *b, None),
                BandSpec::Table { from, to, fill } => (*from, *to, fill.as_deref()),
            };
            let band = Band {
                from: from.min(to).max(0.0),
                to: from.max(to),
                fill: BandFill::parse(fill).unwrap_or(default_fill),
            };
            (band.to > band.from).then_some(band)
        })
        .collect();
    out.sort_by(|a, b| a.from.total_cmp(&b.from));
    // 重叠的带合并进前一条（取并集，填法沿用前一条）。
    let mut merged: Vec<Band> = Vec::with_capacity(out.len());
    for b in out {
        match merged.last_mut() {
            Some(last) if b.from < last.to => last.to = last.to.max(b.to),
            _ => merged.push(b),
        }
    }
    if merged.is_empty() {
        Stretch::Fixed
    } else {
        Stretch::Bands(merged)
    }
}

fn resolve_variant(v: &SkinVariant, density: f32) -> Option<RvSkinVariant> {
    let Some(f) = v.frame.as_ref().filter(|f| !f.image.is_empty()) else {
        tracing::warn!("skin: 变体缺 frame.image，忽略该变体");
        return None;
    };
    let fill = BandFill::parse(f.fill.as_deref());
    let mut srcset: Vec<(String, f32)> = f
        .srcset
        .iter()
        .filter(|e| !e.image.is_empty() && e.density.is_finite() && e.density > 0.0)
        .map(|e| (e.image.clone(), e.density))
        .collect();
    srcset.sort_by(|a, b| a.1.total_cmp(&b.1));
    if f.flip.is_some() {
        tracing::warn!("skin: frame 不支持 flip（拉伸带与 content 按源图写），忽略");
    }
    let frame_density = positive(f.density).unwrap_or(density);
    let frame = RvSkinFrame {
        image: f.image.clone(),
        density: frame_density,
        stretch_x: resolve_stretch(f.stretch_x.as_ref(), fill),
        stretch_y: resolve_stretch(f.stretch_y.as_ref(), fill),
        srcset,
    };
    let mut decor: Vec<RvSkinDecor> = v
        .decor
        .iter()
        .enumerate()
        .filter(|(_, d)| !d.image.is_empty())
        .filter_map(|(src_index, d)| {
            let anchor = Anchor::parse(if d.anchor.is_empty() {
                "frame.top_left"
            } else {
                &d.anchor
            })?;
            let pivot = match d.pivot.as_deref() {
                Some(p) => Pos9::parse(p)?,
                None => anchor.pos,
            };
            let hit = match d.hit.as_deref() {
                None | Some("drag") => DecorHit::Drag,
                Some("block") => DecorHit::Block,
                Some("pass") => DecorHit::Pass,
                Some(other) => {
                    tracing::warn!("skin: 未知 decor.hit {other:?}，按 drag");
                    DecorHit::Drag
                }
            };
            let flip = match d.flip.as_deref() {
                None => Flip::NONE,
                Some(s) => Flip::parse(s).unwrap_or_else(|| {
                    tracing::warn!("skin: 未知 decor.flip {s:?}（只认 x / y / xy），按不翻");
                    Flip::NONE
                }),
            };
            Some(RvSkinDecor {
                src_index,
                image: d.image.clone(),
                density: positive(d.density).unwrap_or(density),
                anchor,
                pivot,
                offset: d.offset,
                size: d.size.filter(|[w, h]| *w > 0.0 && *h > 0.0),
                z: d.z,
                hit,
                opacity: d.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
                fps: positive(d.fps),
                flip,
                anim: resolve_decor_anim(&d.states, &d.on),
            })
        })
        .collect();
    decor.sort_by_key(|d| d.z);
    let p = &v.placement;
    let dflt = RvPlacement::default();
    let placement = RvPlacement {
        caret: resolve_caret(p.caret.as_ref(), dflt.caret),
        caret_above: resolve_caret(p.caret_above.as_ref(), dflt.caret_above),
        avoid_caret: match &p.avoid_caret {
            None | Some(AvoidSpec::On(true)) => dflt.avoid_caret,
            Some(AvoidSpec::On(false)) => None,
            Some(AvoidSpec::Strip { before, after }) => {
                let d = AvoidStrip::default();
                let w =
                    |v: Option<f32>, d: f32| v.filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(d);
                Some(AvoidStrip {
                    before: w(*before, d.before),
                    after: w(*after, d.after),
                })
            }
        },
        clamp: match p.clamp.as_deref() {
            None | Some("frame") => Clamp::Frame,
            Some("all") => Clamp::All,
            Some(other) => {
                tracing::warn!("skin: 未知 placement.clamp {other:?}，按 frame");
                Clamp::Frame
            }
        },
    };
    let slots = v
        .slots
        .candidates
        .as_ref()
        .and_then(resolve_slot)
        .map(|candidates| RvSlots {
            preedit: v.slots.preedit.as_ref().and_then(resolve_slot),
            candidates,
            footer: v.slots.footer.as_ref().and_then(resolve_slot),
        });
    if slots.is_none() && (v.slots.preedit.is_some() || v.slots.footer.is_some()) {
        tracing::warn!("skin: 写了 slots 却没写 slots.candidates，按 stack 布局");
    }
    Some(RvSkinVariant {
        frame,
        content: v.content,
        decor,
        placement,
        slots,
    })
}

/// typed `Skin` → `RvSkin`。没有任何可用变体时返回 None（等同未配皮肤）。
pub fn resolve_skin(s: &Skin) -> Option<RvSkin> {
    let density = positive(s.density).unwrap_or(1.0);
    let rv = |v: &Option<SkinVariant>| v.as_ref().and_then(|v| resolve_variant(v, density));
    let out = RvSkin {
        scale_with: match s.scale_with.as_deref() {
            Some("font") => ScaleWith::Font,
            _ => ScaleWith::Dpi,
        },
        density,
        font_base: positive(s.font_base).unwrap_or(DEFAULT_FONT_BASE),
        horizontal: rv(&s.horizontal),
        vertical: rv(&s.vertical),
        horizontal_embedded: rv(&s.horizontal_embedded),
        vertical_embedded: rv(&s.vertical_embedded),
    };
    let any = out.horizontal.is_some()
        || out.vertical.is_some()
        || out.horizontal_embedded.is_some()
        || out.vertical_embedded.is_some();
    any.then_some(out)
}

/// base 链合并前：子主题写了的皮肤变体，从 base 里整表拿掉（皮肤是整体设计，逐字段合并没有意义）。
pub(crate) fn replace_inherited_variants(base: &mut Value, over: &Value) {
    let Some(over_skin) = over.get("skin").and_then(Value::as_table) else {
        return;
    };
    let Some(base_skin) = base.get_mut("skin").and_then(Value::as_table_mut) else {
        return;
    };
    for key in SKIN_VARIANTS {
        if over_skin.contains_key(key) {
            base_skin.remove(key);
        }
    }
}

/// 图片文件头是否动画格式（读文件头前 64 KiB 足够）：GIF 一律算（帧数另判）；APNG 看 IDAT 之前有无 acTL；WebP 看 VP8X 的动画位。
pub fn sniff_animated(b: &[u8]) -> bool {
    if b.starts_with(b"GIF8") {
        return true;
    }
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut at = 8;
        while let Some(head) = b.get(at..at + 8) {
            let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
            match &head[4..8] {
                b"acTL" => return true,
                b"IDAT" => return false,
                _ => at = at.saturating_add(len).saturating_add(12),
            }
        }
        return false;
    }
    b.len() > 20
        && &b[0..4] == b"RIFF"
        && &b[8..12] == b"WEBP"
        && &b[12..16] == b"VP8X"
        && b[20] & 0x02 != 0
}

#[cfg(test)]
#[path = "skin_tests.rs"]
mod tests;
