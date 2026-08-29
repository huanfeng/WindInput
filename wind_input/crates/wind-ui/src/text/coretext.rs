//! 文本渲染后端（CoreText 实现，macOS）。
//!
//! 与 Windows 版 `text/dwrite.rs` 对外契约逐方法对齐：
//! 颜色 `[u8;4]` 是 `[R, G, B, A]`（与 palette/View 叶子色一致）；`buf` 是预乘 BGRA、
//! 已含背景、原地叠加绘制（CGBitmapContext 声明 BGRA 内存序，CoreGraphics 自行落位）。
//!
//! 管线：CTFont（按取整 px 缓存，可级联拆字字根字体）
//!      → 测量：CFAttributedString + CTLine::get_typographic_bounds
//!      → 绘制：CGBitmapContext 直接绑定调用方 BGRA 缓冲区（不拷贝），翻转 CTM 后 CTLine::draw。

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use core_foundation::array::CFArray;
use core_foundation::attributed_string::CFMutableAttributedString;
use core_foundation::base::{CFRange, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;

use core_graphics::base::{kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst};
use core_graphics::color::CGColor;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;

use core_text::font::CTFont;
use core_text::font_descriptor::{CTFontDescriptor, kCTFontCascadeListAttribute};
use core_text::line::CTLine;
use core_text::string_attributes::{kCTFontAttributeName, kCTForegroundColorAttributeName};

use super::dwrite::{TextMetrics, TextStyle};
use super::script::FontPlan;

/// 主字体载入失败时的系统回退字体族。
const FALLBACK_FAMILY: &str = "Helvetica";

/// CJK 字体回退链：用户/主题指定的字体在 macOS 常不可解析（如 Windows 字体名
/// "Microsoft YaHei UI"），逐一尝试本机常见含 CJK 字形的字体，绝不退到纯拉丁字体，
/// 否则汉字渲染成方框 □。与 Go `forwarder_darwin.go` 的候选链一致。
const CJK_FALLBACK: &[&str] = &["PingFang SC", "Hiragino Sans GB", "STHeiti", "Songti SC"];

/// 字形级级联回退族链：base 字体缺字时按序解析。覆盖 UI 标记符号、CJK 备选、
/// 彩色 emoji（sbix）。拆字字根字体（PUA）若已加载则置于本链之前。
/// 与 Go `font_fallback_darwin.go` 的候选链一致——尤其 Apple Color Emoji 提供 emoji
/// 词库层候选的彩色位图字形（缺它则 emoji 渲染成方框/单色）。
const CASCADE_FALLBACK: &[&str] = &[
    "Apple Symbols",     // ✓ ▸ ◂ 等 UI 标记符号（换主字体后回归的符号靠它兜底）
    "PingFang SC",       // CJK 主力（base 已是它时 CoreText 自动去重）
    "Hiragino Sans GB",  // 冬青黑简
    "STHeiti",           // 华文黑体
    "Songti SC",         // 宋体
    "Apple Color Emoji", // emoji 彩色字形（sbix）
];

/// 文本渲染器（CoreText）。
pub struct TextRenderer {
    /// 字体族名
    family: String,
    /// 基准字号（family 固定）；可按调用传不同字号。
    font_size: f32,
    /// 拆字字根字体描述符（可选）：设置后构造级联回退，PUA 码位回退到它。
    chaizi: Option<CTFontDescriptor>,
    /// 全局字体方案。**本后端只实现「默认回退链」这一半**：链尾各项进 cascade list，
    /// 语义与 DirectWrite 侧的自定义 fallback 一致。
    ///
    /// ⚠️ **按脚本的字体指派（`FontPlan::declared`）在 macOS 上尚未实现**——它要按
    /// `font_runs` 的分段构造 attributed string 的分段字体属性，而本后端目前是整行
    /// 单字体（`make_line`）。蒙古文用户在 Windows 上，故先补 Windows 侧；这里如实标注，
    /// 不做「声明未实现」的假接线。
    plan: FontPlan,
    /// CTFont 缓存：按取整 px keyed，避免每次测量/绘制重建。
    fonts: RefCell<HashMap<u32, CTFont>>,
}

impl TextRenderer {
    /// 创建文本渲染器。
    pub fn new(font_family: &str, font_size: f32) -> Result<Self, String> {
        Ok(Self {
            family: font_family.to_string(),
            font_size,
            chaizi: None,
            plan: FontPlan::default(),
            fonts: RefCell::new(HashMap::new()),
        })
    }

    /// 基准字号（View 叶子未显式指定字号时回退）。
    pub fn base_size(&self) -> f32 {
        self.font_size
    }

    /// 更新基准字号（DPI 动态变化时调用）。
    pub fn set_base_size(&mut self, size: f32) {
        self.font_size = size;
    }

    /// 切换字体族（ui.font.family 变更时调用）。清空字号缓存使新字体生效。
    pub fn set_font_family(&mut self, font_family: &str) {
        self.family = font_family.to_string();
        self.fonts.borrow_mut().clear();
    }

    /// 换字体方案。字号缓存必须清：cascade list 由方案参与构建，缓存里的 CTFont
    /// 带的是旧链。见结构体字段上关于本后端只实现了哪一半的说明。
    pub fn set_font_plan(&mut self, plan: FontPlan) {
        if self.plan == plan {
            return;
        }
        // 脚本指派在本后端是**静默无效**的。不吭一声的话，用户看到的是「配了没反应」，
        // 要靠一轮误诊才查到平台差异——一行日志省掉那一轮。
        if !plan.declared().is_empty() {
            tracing::warn!(
                "ui.font.scripts 的按脚本字体指派在 macOS 上尚未实现，本次声明的 {} 个脚本类不生效（回退链仍生效）",
                plan.declared().len()
            );
        }
        self.plan = plan;
        self.fonts.borrow_mut().clear();
    }

    /// 当前字体方案。
    pub fn font_plan(&self) -> &FontPlan {
        &self.plan
    }

    /// 加载拆字字根字体（TTF）作级联回退；失败返回 Err（不影响普通文本渲染）。
    /// `_family` 为 DWrite 家族名（Windows 侧用），CoreText 直接从字体文件字节建描述符，故忽略。
    pub fn set_chaizi_font(&mut self, path: &str, _family: &str) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read chaizi font {path}: {e}"))?;
        let desc = core_text::font_manager::create_font_descriptor(&bytes)
            .map_err(|_| format!("create_font_descriptor failed for {path}"))?;
        self.chaizi = Some(desc);
        self.fonts.borrow_mut().clear();
        Ok(())
    }

    /// 解析字体族为 CTFont（base 字体，未含拆字级联）：依次尝试
    /// 指定族 → CJK 回退链 → Helvetica → Menlo，确保汉字有字形。
    fn resolve_base(family: &str, pt: f64) -> CTFont {
        if let Ok(f) = core_text::font::new_from_name(family, pt) {
            // 含 CJK 字形校验：若解析到的是纯拉丁字体（如把未知名 fallback 成 Helvetica），
            // CoreText 仍会返回成功，但渲染汉字时该字体无字形 → 由 CTLine 级联兜底。
            // 这里直接返回；CJK 级联由系统 cascade 处理，PingFang 优先仅为求稳。
            return f;
        }
        for cand in CJK_FALLBACK {
            if let Ok(f) = core_text::font::new_from_name(cand, pt) {
                return f;
            }
        }
        core_text::font::new_from_name(FALLBACK_FAMILY, pt)
            .unwrap_or_else(|_| core_text::font::new_from_name("Menlo", pt).unwrap())
    }

    /// 给 base 字体注入字形级级联回退（kCTFontCascadeListAttribute）：
    /// 拆字字根（PUA，若已加载）→ 符号 → CJK 备选 → 彩色 emoji。
    ///
    /// 必须显式构链而非依赖系统默认级联：一旦设过拆字字体，若只放拆字 descriptor 会
    /// **覆盖**系统默认级联，导致 emoji/符号丢失（渲染成方框）。故这里把符号 / CJK /
    /// Apple Color Emoji 一并纳入 cascade，使这些码位都能解析到对应字形。
    fn apply_cascade(&self, base: CTFont, pt: f64) -> CTFont {
        let mut descs: Vec<CTFontDescriptor> = Vec::new();
        // 拆字字根优先（PUA 私用区码位专属）。
        if let Some(chaizi_desc) = &self.chaizi {
            descs.push(chaizi_desc.clone());
        }
        // 用户声明的回退链（默认链去掉链首——链首是 base 字体本身，见 `font_for`）
        // 排在内置兜底之前：用户显式写下的顺序必须赢过我们猜的那条链。
        for fam in self.plan.chain_for(None).iter().skip(1) {
            if let Ok(f) = core_text::font::new_from_name(fam, pt) {
                descs.push(f.copy_descriptor());
            }
        }
        // 符号 / CJK / 彩色 emoji 回退族。
        for fam in CASCADE_FALLBACK {
            if let Ok(f) = core_text::font::new_from_name(fam, pt) {
                descs.push(f.copy_descriptor());
            }
        }
        if descs.is_empty() {
            return base;
        }
        let cascade = CFArray::from_CFTypes(&descs).into_untyped();
        let key_attr = unsafe { CFString::wrap_under_get_rule(kCTFontCascadeListAttribute) };
        let attrs = CFDictionary::from_CFType_pairs(&[(key_attr.as_CFType(), cascade.as_CFType())]);
        match base
            .copy_descriptor()
            .create_copy_with_attributes(attrs.into_untyped())
        {
            Ok(merged) => core_text::font::new_from_descriptor(&merged, pt),
            Err(_) => base,
        }
    }

    /// 取得（或创建）给定字号的 CTFont（按取整 px 缓存，含拆字级联）。
    fn font_for(&self, size: f32) -> CTFont {
        let key = size.max(1.0).round() as u32;
        if let Some(f) = self.fonts.borrow().get(&key) {
            return f.clone();
        }
        let pt = key as f64;
        // 方案默认链的链首优先于全局字族——与 dwrite 侧 `create_layout` 的三层合成同序。
        let base_family = self.plan.base_family().unwrap_or(&self.family);
        let base = Self::resolve_base(base_family, pt);
        let font = self.apply_cascade(base, pt);
        self.fonts.borrow_mut().insert(key, font.clone());
        font
    }

    /// 带字体族覆盖的 CTFont 解析（styled 路径）。family=None 时走缓存的基准族；
    /// family=Some 时按覆盖族即时构建（不入缓存，覆盖族通常少量出现）。
    ///
    /// ⚠️ 「通常少量出现」这个前提在**方案级候选字体**下不再成立：方案一旦声明
    /// `[candidate] font_family`，每个候选文字叶子的每次测量与每次绘制都走这条不缓存的路
    /// （`resolve_base` + `apply_cascade`，后者内含一串 `new_from_name`）。
    /// 蒙文用户在 Windows，故暂不改；macOS 上真用起这个功能时应加一个按 (族, 字号) 的缓存。
    ///
    /// weight 当前
    /// 不合成粗体（CoreText 合成粗体涉及 symbolic traits，纯视觉差异，留待后续）。
    fn font_styled(&self, size: f32, _weight: i32, family: Option<&str>) -> CTFont {
        match family {
            None => self.font_for(size),
            Some(fam) => {
                let pt = size.max(1.0).round() as f64;
                let base = Self::resolve_base(fam, pt);
                self.apply_cascade(base, pt)
            }
        }
    }

    /// 测量文本。镜像 dwrite 的 `TextRenderer::measure`。
    pub fn measure(&self, text: &str, ts: &TextStyle) -> TextMetrics {
        if text.is_empty() {
            return TextMetrics {
                width: 0.0,
                height: ts.size * 1.2,
            };
        }
        let font = self.font_styled(ts.size, ts.weight, ts.family);
        let line = make_line(text, &font, None);
        let b = line.get_typographic_bounds();
        let height = (b.ascent + b.descent + b.leading) as f32;
        TextMetrics {
            width: b.width as f32,
            height: if height > 0.0 { height } else { ts.size * 1.2 },
        }
    }

    /// 绘制文本。镜像 dwrite 的 `TextRenderer::draw`。
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
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
        if text.is_empty() || buf_width == 0 || buf_height == 0 || ts.size <= 0.0 {
            return Ok(());
        }
        let w = buf_width as usize;
        let h = buf_height as usize;
        if buf.len() < w * h * 4 {
            return Err("buffer too small".into());
        }
        let font = self.font_styled(ts.size, ts.weight, ts.family);
        // color 为 [R,G,B,A]（与 palette/View 叶子色约定一致）；给 CGColor 真 R/G/B。
        let cg_color = CGColor::rgb(
            color[0] as f64 / 255.0,
            color[1] as f64 / 255.0,
            color[2] as f64 / 255.0,
            color[3] as f64 / 255.0,
        );
        let line = make_line(text, &font, Some(&cg_color));
        let ascent = line.get_typographic_bounds().ascent;
        let space = CGColorSpace::create_device_rgb();
        let bitmap_info = kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little;
        {
            let ctx = CGContext::create_bitmap_context(
                Some(buf.as_mut_ptr() as *mut c_void),
                w,
                h,
                8,
                w * 4,
                &space,
                bitmap_info,
            );
            // 不翻转上下文：CGBitmapContext 默认 bottom-left 原点、CTLine 字形自然朝上；
            // translate(0,h)+scale(1,-1) 会让字形上下颠倒（实测候选字倒置）。缓冲是 top-down
            // （row0=顶，与 tiny-skia 框同向），故把基线的 top-down 坐标 y+ascent 换成 CG 的
            // bottom-left 坐标 h-(y+ascent)，文字即正立且落在正确行。
            ctx.set_text_position(x as f64, (h as f32 - (y + ascent as f32)) as f64);
            line.draw(&ctx);
            ctx.flush();
        }
        Ok(())
    }

    /// 测量文本尺寸（用基准字号）。
    pub fn measure_text(&self, text: &str) -> TextMetrics {
        self.measure_text_sized(text, self.font_size)
    }

    /// 测量文本尺寸（指定字号；宽为排版宽度，高为 ascent+descent+leading）。
    pub fn measure_text_sized(&self, text: &str, size: f32) -> TextMetrics {
        if text.is_empty() {
            return TextMetrics {
                width: 0.0,
                height: size * 1.2,
            };
        }
        let font = self.font_for(size);
        let line = make_line(text, &font, None);
        let b = line.get_typographic_bounds();
        let height = (b.ascent + b.descent + b.leading) as f32;
        TextMetrics {
            width: b.width as f32,
            height: if height > 0.0 { height } else { size * 1.2 },
        }
    }

    /// 绘制文本（用基准字号）。
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        color: [u8; 4],
    ) -> Result<(), String> {
        self.draw_text_sized(
            buf,
            buf_width,
            buf_height,
            x,
            y,
            text,
            self.font_size,
            color,
        )
    }

    /// 绘制文本（指定字号）到预乘 BGRA 缓冲区（已含背景，原地叠加）。
    ///
    /// - `buf`: 目标 BGRA 缓冲区（已含背景，预乘 alpha）
    /// - `x`/`y`: 文本左上角（像素坐标，顶端向下）
    /// - `color`: 文本颜色 `[B, G, R, A]`
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_sized(
        &self,
        buf: &mut [u8],
        buf_width: u32,
        buf_height: u32,
        x: f32,
        y: f32,
        text: &str,
        size: f32,
        color: [u8; 4],
    ) -> Result<(), String> {
        if text.is_empty() || buf_width == 0 || buf_height == 0 || size <= 0.0 {
            return Ok(());
        }
        let w = buf_width as usize;
        let h = buf_height as usize;
        if buf.len() < w * h * 4 {
            return Err("buffer too small".into());
        }

        let font = self.font_for(size);
        // 文本前景色：入参 color 为 [R,G,B,A]（与 palette/View 叶子色约定一致，非 BGRA）。
        // CGBitmapContext 声明为 BGRA 内存序，CoreGraphics 据 bitmapInfo 自行落位，故这里给真 R/G/B。
        let cg_color = CGColor::rgb(
            color[0] as f64 / 255.0,
            color[1] as f64 / 255.0,
            color[2] as f64 / 255.0,
            color[3] as f64 / 255.0,
        );
        let line = make_line(text, &font, Some(&cg_color));

        let ascent = line.get_typographic_bounds().ascent;

        // CGBitmapContext 直接绑定调用方 buf（不拷贝）。
        // kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little ⇒ 内存序 BGRA、预乘。
        let space = CGColorSpace::create_device_rgb();
        let bitmap_info = kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little;
        {
            let ctx = CGContext::create_bitmap_context(
                Some(buf.as_mut_ptr() as *mut c_void),
                w,
                h,
                8,
                w * 4,
                &space,
                bitmap_info,
            );
            // CoreText 坐标系原点在左下、Y 向上；翻转使其与顶端向下的像素缓冲一致。
            ctx.translate(0.0, h as f64);
            ctx.scale(1.0, -1.0);
            // 基线 Y：文本顶部在 y，基线下移 ascent；再换算到翻转后坐标系。
            ctx.set_text_position(x as f64, (h as f32 - (y + ascent as f32)) as f64);
            line.draw(&ctx);
            ctx.flush();
            // ctx 在此 drop，释放对 buf 的裸指针借用。
        }
        Ok(())
    }
}

/// 用给定字体（可选前景色）构造单行 CTLine。
fn make_line(text: &str, font: &CTFont, color: Option<&CGColor>) -> CTLine {
    let mut attr = CFMutableAttributedString::new();
    let cf_text = CFString::new(text);
    let full = CFRange::init(0, 0);
    attr.replace_str(&cf_text, full);
    let range = CFRange::init(0, attr.char_len());
    unsafe {
        attr.set_attribute(range, kCTFontAttributeName, font);
        if let Some(c) = color {
            attr.set_attribute(range, kCTForegroundColorAttributeName, c);
        }
    }
    CTLine::new_with_attributed_string(attr.as_concrete_TypeRef())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_nonempty_positive_and_scales() {
        let r = TextRenderer::new("PingFang SC", 16.0).unwrap();
        let m1 = r.measure_text("中文");
        assert!(m1.width > 0.0 && m1.height > 0.0);
        let m2 = r.measure_text_sized("中文", 32.0);
        assert!(m2.width > m1.width);
    }

    #[test]
    fn measure_empty_is_zero_width() {
        let r = TextRenderer::new("PingFang SC", 16.0).unwrap();
        let m = r.measure_text_sized("", 20.0);
        assert_eq!(m.width, 0.0);
        assert!((m.height - 24.0).abs() < 0.01); // 20*1.2
    }

    #[test]
    fn draw_writes_nonbackground_pixels() {
        let r = TextRenderer::new("PingFang SC", 16.0).unwrap();
        let (w, h) = (64u32, 32u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        r.draw_text(&mut buf, w, h, 2.0, 2.0, "中", [0, 0, 0, 255])
            .unwrap();
        assert!(buf.iter().any(|&b| b != 0));
    }
}
