//! 文字公共类型与文字后端抽象。
//!
//! 平台后端（DirectWrite / CoreText / Linux / mock）在 wind-ui 实现 [`TextBackend`]；
//! 本模块只放它们与 View 共用的平台无关部分。

// 按脚本切字体段的纯逻辑（不依赖任何平台 API）。
pub mod script;

/// 文本度量信息
#[derive(Debug, Clone)]
pub struct TextMetrics {
    pub width: f32,
    pub height: f32,
}

/// 一次文字排版所需的全部字体属性。
///
/// 收成结构体是为了穿过三个后端（DirectWrite / CoreText / mock）与全部测量、绘制调用点时，
/// 新增属性只加字段、不改签名；也避免了改走线程局部注入那种隐式全局状态（忘了复位就串到
/// 后续无关文字上）。
///
/// 本仓暂不设 `line_height`：View 引擎还没有行高概念，高度直接取自后端度量。
/// 加字段前先让它在渲染路径里真正生效，否则就是「声明未实现」。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle<'a> {
    /// 字族名。`None` = 用渲染器的全局字体族。
    pub family: Option<&'a str>,
    /// 字号（设备像素，调用方已按 DPI 缩放）。
    pub size: f32,
    /// 字重（400=常规、700=粗）。`0` = 继承渲染器默认，沿用既有约定。
    pub weight: i32,
}

impl<'a> TextStyle<'a> {
    /// 只指定字号，字重与字体族取默认。
    pub fn new(size: f32) -> Self {
        Self {
            family: None,
            size,
            weight: 0,
        }
    }

    /// 换字重（`0` = 继承默认）。
    pub fn with_weight(self, weight: i32) -> Self {
        Self { weight, ..self }
    }

    /// 换字体族（`None`/空串 = 用全局字体族）。
    pub fn with_family(self, family: Option<&'a str>) -> Self {
        Self {
            family: family.filter(|s| !s.trim().is_empty()),
            ..self
        }
    }
}

/// 分段颜色的一段：文本里 `[start, end)` 这段字节区间改用 `rgba`（`[R, G, B, A]`）画。
///
/// 只作用于绘制，不参与测量——整串仍一次整形，理由同 `View::caret_at`：拆开各自排版会让
/// 宽度随着色边界抖动。三个后端都按这条实现：DirectWrite 用 drawing effect（格式属性，
/// 不参与整形），CoreText 整形一次后按字形子区间换填充色。
///
/// 区间须落在字符边界上、互不重叠；不满足的区间被后端丢弃（见 [`utf16_runs`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorRun {
    pub start: u32,
    pub end: u32,
    pub rgba: [u8; 4],
}

impl ColorRun {
    /// 取 `runs` 落在 `[start, end)` 内的部分，并平移成以 `start` 为 0 的偏移。
    ///
    /// 直立态把一段文字逐格切成多个叶子（`candidate_window::upright_text`），每格只拿
    /// 属于自己的那几段颜色。
    pub fn slice(runs: &[ColorRun], start: usize, end: usize) -> Vec<ColorRun> {
        let (s, e) = (start as u32, end as u32);
        runs.iter()
            .filter_map(|r| {
                let (a, b) = (r.start.max(s), r.end.min(e));
                (a < b).then(|| ColorRun {
                    start: a - s,
                    end: b - s,
                    rgba: r.rgba,
                })
            })
            .collect()
    }
}

/// 把字节区间的 [`ColorRun`] 换算成 UTF-16 码元区间 `(起点, 长度, 颜色)`——DirectWrite 的
/// `DWRITE_TEXT_RANGE` 与 CoreText 的 string index 都以 UTF-16 计（同 wind-ui `pua_runs` 口径）。
///
/// 丢弃空区间、越界区间与不在字符边界上的区间：颜色只是装饰，坏区间让那段回落基色，
/// 不该让整段文字画不出来。
pub fn utf16_runs(text: &str, runs: &[ColorRun]) -> Vec<(u32, u32, [u8; 4])> {
    let u16_at = |byte: usize| text[..byte].encode_utf16().count() as u32;
    runs.iter()
        .filter(|r| {
            let (s, e) = (r.start as usize, r.end as usize);
            s < e && e <= text.len() && text.is_char_boundary(s) && text.is_char_boundary(e)
        })
        .map(|r| {
            let s = u16_at(r.start as usize);
            (s, u16_at(r.end as usize) - s, r.rgba)
        })
        .collect()
}

/// 测量缓存键：`(文本, 字号, 字重, 字体族)` 的 64 位哈希。
///
/// 存哈希而非完整键，是为了免掉每次查询都克隆 `String`——测量在热路径上，一帧数十次。
/// 64 位下 4096 条目的碰撞概率约 4.5e-13，可忽略；真碰撞的后果是某段文本用了另一段的
/// 宽度（布局错位），故键必须覆盖所有影响测量的输入。
///
/// 字号用 `to_bits()` 而非 `as u32`：字号是 DPI 缩放后的浮点（如 14.4/16.8），
/// 取整会让相邻字号撞进同一个键。
///
/// 给 [`TextStyle`] 加字段时必须同步加进这里，否则某段文本会静默套用另一段的宽度
/// （按整个 `TextStyle` 取参正是为此）。
pub fn measure_key(text: &str, ts: &TextStyle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    ts.size.to_bits().hash(&mut h);
    ts.weight.hash(&mut h);
    ts.family.hash(&mut h);
    h.finish()
}

/// View 树排版与绘制所需的文字能力。各平台后端在 wind-ui 实现，View 只经它取度量、画字。
pub trait TextBackend {
    /// 未指定字号的文字所用的基准字号（设备像素）。
    fn base_size(&self) -> f32;

    /// 按样式测量整串文字。
    fn measure(&self, text: &str, ts: &TextStyle) -> TextMetrics;

    /// 以 `(x, y)` 为左上角，把文字画进 BGRA 预乘缓冲。
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        color: [u8; 4],
    ) -> Result<(), String>;

    /// 同 [`Self::draw`]，`runs` 覆盖的字节区间改用各自的颜色（只作用于绘制，不参与测量）。
    #[allow(clippy::too_many_arguments)]
    fn draw_runs(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        color: [u8; 4],
        runs: &[ColorRun],
    ) -> Result<(), String>;
}

/// 把另一个后端的 [`TextBackend::base_size`] 钉成固定值，其余原样转发。
///
/// 一次出图（[`crate::candidate::render_candidate`]）用它让基准字号只由场景输入决定
/// （生效字号 × 缩放），不依赖调用方后端里存着的旧值。
pub struct SizedText<'a> {
    pub inner: &'a dyn TextBackend,
    pub base_size: f32,
}

impl TextBackend for SizedText<'_> {
    fn base_size(&self) -> f32 {
        self.base_size
    }

    fn measure(&self, text: &str, ts: &TextStyle) -> TextMetrics {
        self.inner.measure(text, ts)
    }

    fn draw(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        color: [u8; 4],
    ) -> Result<(), String> {
        self.inner
            .draw(buf, buf_width, buf_height, x, y, text, ts, color)
    }

    fn draw_runs(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        color: [u8; 4],
        runs: &[ColorRun],
    ) -> Result<(), String> {
        self.inner
            .draw_runs(buf, buf_width, buf_height, x, y, text, ts, color, runs)
    }
}

/// 本 crate 测试用的文字后端：照抄 wind-ui mock 后端的公式（宽 = 字符数 × 字号 × 0.6，
/// 高 = 字号 × 1.2），不出像素，只记绘制调用（逐字同 wind-ui mock 的 `draw_log`）。
#[cfg(test)]
pub(crate) struct MockText {
    font_size: f32,
    draw_log: std::cell::RefCell<Vec<String>>,
}

#[cfg(test)]
impl MockText {
    pub(crate) fn new(_font_family: &str, font_size: f32) -> Result<Self, String> {
        Ok(Self {
            font_size,
            draw_log: std::cell::RefCell::new(Vec::new()),
        })
    }

    /// 取走至今的绘制调用记录（取后清空）。
    pub(crate) fn take_draw_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.draw_log.borrow_mut())
    }
}

#[cfg(test)]
impl TextBackend for MockText {
    fn base_size(&self) -> f32 {
        self.font_size
    }

    fn measure(&self, text: &str, ts: &TextStyle) -> TextMetrics {
        if text.is_empty() {
            return TextMetrics {
                width: 0.0,
                height: ts.size * 1.2,
            };
        }
        TextMetrics {
            width: text.chars().count() as f32 * ts.size * 0.6,
            height: ts.size * 1.2,
        }
    }

    fn draw(
        &self,
        _buf: &mut [u8],
        _buf_width: u32,
        _buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        color: [u8; 4],
    ) -> Result<(), String> {
        self.draw_log.borrow_mut().push(format!(
            "draw text={text:?} at=({x:?},{y:?}) size={:?} weight={} family={:?} \
             color={color:?}",
            ts.size, ts.weight, ts.family,
        ));
        Ok(())
    }

    fn draw_runs(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        ts: &TextStyle,
        color: [u8; 4],
        runs: &[ColorRun],
    ) -> Result<(), String> {
        if utf16_runs(text, runs).is_empty() {
            return self.draw(buf, buf_width, buf_height, x, y, text, ts, color);
        }
        self.draw_log.borrow_mut().push(format!(
            "draw_runs text={text:?} at=({x:?},{y:?}) size={:?} weight={} family={:?} \
             color={color:?} runs={runs:?}",
            ts.size, ts.weight, ts.family,
        ));
        Ok(())
    }
}

// 测量缓存键的跨平台测试（`measure_key` 不依赖 DirectWrite，不限平台）。
//
// 这里测的是**键的区分度**而非缓存命中：键漏掉任何一项影响测量的输入，后果都是某段文本
// 静默套用另一段的宽度——布局错位，且因为是缓存命中路径，重现条件依赖于测量顺序，极难定位。
#[cfg(test)]
#[path = "mod_measure_key_tests.rs"]
mod measure_key_tests;

// 分段颜色区间的纯逻辑（不依赖任何后端），不限平台。
#[cfg(test)]
mod color_run_tests {
    use super::{ColorRun, utf16_runs};

    fn run(start: u32, end: u32, tag: u8) -> ColorRun {
        ColorRun {
            start,
            end,
            rgba: [tag, 0, 0, 255],
        }
    }

    /// 字节区间换 UTF-16：`你`（3 字节 / 1 码元）、`😀`（4 字节 / 2 码元，代理对）各占多少都要算对。
    #[test]
    fn utf16_ranges_count_code_units() {
        let text = "你😀ab";
        let got = utf16_runs(text, &[run(0, 3, 1), run(3, 7, 2), run(7, 9, 3)]);
        assert_eq!(
            got,
            vec![
                (0, 1, [1, 0, 0, 255]),
                (1, 2, [2, 0, 0, 255]),
                (3, 2, [3, 0, 0, 255])
            ]
        );
    }

    /// 坏区间丢弃、好区间照留：空、越界、切在字中间。
    #[test]
    fn invalid_ranges_are_dropped() {
        let text = "你好";
        let got = utf16_runs(
            text,
            &[run(0, 0, 1), run(3, 99, 2), run(1, 3, 3), run(3, 6, 4)],
        );
        assert_eq!(got, vec![(1, 1, [4, 0, 0, 255])]);
    }

    /// 直立态逐格切：每格只拿自己范围内那部分，并平移到格内偏移；跨格的区间两边各得一截。
    #[test]
    fn slice_clips_and_shifts() {
        let runs = [run(0, 2, 1), run(2, 7, 2)];
        assert_eq!(
            ColorRun::slice(&runs, 0, 3),
            vec![run(0, 2, 1), run(2, 3, 2)]
        );
        assert_eq!(ColorRun::slice(&runs, 3, 6), vec![run(0, 3, 2)]);
        assert_eq!(ColorRun::slice(&runs, 7, 9), vec![]);
    }
}
