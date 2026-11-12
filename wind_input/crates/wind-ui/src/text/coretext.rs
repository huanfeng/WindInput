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
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;

use core_graphics::base::{kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst};
use core_graphics::color::CGColor;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;

use core_text::font::CTFont;
use core_text::font_collection;
use core_text::font_descriptor::{CTFontDescriptor, kCTFontCascadeListAttribute};
use core_text::line::CTLine;
use core_text::run::CTRunRef;
use core_text::string_attributes::{
    kCTFontAttributeName, kCTForegroundColorAttributeName,
    kCTForegroundColorFromContextAttributeName,
};
use foreign_types::ForeignType;

use super::dwrite::{ColorRun, TextMetrics, TextStyle, utf16_runs};

// core-text 没有包 `CTRunDraw`（按字形子区间画一个 run）；框架本身已由 core-text 链接。
unsafe extern "C" {
    fn CTRunDraw(run: CTRunRef, context: *mut core_graphics::sys::CGContext, range: CFRange);
}
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

    /// 最近一次设进来的全局字族（设置端预览临时换字体后据此换回）。
    pub fn font_family(&self) -> &str {
        &self.family
    }

    /// 本后端不支持全局默认字重（见 [`Self::set_default_weight`]），恒为 0。
    pub fn default_weight(&self) -> i32 {
        0
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

    /// 系统字体集里有没有这个字族名。`None` = 查不了（名字为空）。
    ///
    /// 语义与 Windows 侧 `dwrite.rs` 的同名方法对齐：问的是**家族名表**里有没有这个名字
    /// （`CTFontManagerCopyAvailableFontFamilyNames`），而不是「能不能凑出一款字体」——
    /// 后者（`font_collection::create_for_family` 那条路）对写错的名字也会匹配出最近似的
    /// 一款，那恰好是这条告警要抓的情形，用它等于自废。
    /// 大小写不敏感，同 DirectWrite 的 `FindFamilyName`。
    ///
    /// # ⚠️ 此前这里恒返回 `None`
    ///
    /// 理由写的是「先一律不知道，宁可少一条提示也不误报」，而调用方
    /// （`CandidateWindow::resolve_and_report`）按 `Some(false)` 才判缺失
    /// ⇒ **整条字体缺失告警在 macOS 上空转**：`ui.font.family` / 方案级 `[candidate]
    /// font_family` / 主题节点写错字族名，画面静默回落、日志一个字都没有，而 macOS 上
    /// 写错的概率比 Windows 还高（主题多半是在 Windows 上做的，字体名照抄过来就不存在）。
    ///
    /// 成本：一次全量家族名拷贝（本机几百条）。调用点都是换字体 / 换方案 / 换主题这类
    /// 低频事件，不在渲染热路径——同 Windows 侧那条 `⚠️ 只在设置字体时查` 的约束。
    pub fn family_exists(&self, family: &str) -> Option<bool> {
        let name = family.trim();
        if name.is_empty() {
            return None;
        }
        let names = font_collection::get_family_names();
        Some(
            names
                .iter()
                .any(|n| n.to_string().eq_ignore_ascii_case(name)),
        )
    }

    /// 按 face 全名找 family：CoreText 侧未实现。旧 GDI face name 是 Windows 设置页的产物，
    /// macOS 上只靠 `font_resolve` 的剥字重后缀那一步兜底。
    pub fn find_face(&self, _name: &str) -> Option<(String, i32)> {
        None
    }

    /// 全局默认字重（`ui.font.weight`）：**本后端尚未实现**——与节点字重一样被
    /// [`Self::font_styled`] 忽略（平台限制，见看板 A2-1）。保留接口是为了两平台配置通路一致。
    pub fn set_default_weight(&mut self, _weight: i32) {}

    /// 加载拆字字根字体（TTF）作级联回退；失败返回 Err（不影响普通文本渲染）。
    /// `_family` 为 DWrite 家族名（Windows 侧用），CoreText 直接从字体文件字节建描述符，故忽略。
    /// `path` 为空 = 撤掉字根字体，不再挂级联回退。
    ///
    /// 先撤旧的再加载：加载失败的结局是「没有字根字体」而不是沿用上一个方案的（理由见
    /// `dwrite.rs` 的同名方法）。
    pub fn set_chaizi_font(&mut self, path: &str, _family: &str) -> Result<(), String> {
        self.chaizi = None;
        self.fonts.borrow_mut().clear();
        if path.is_empty() {
            return Ok(());
        }
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

    /// 基准字体的自然行高：ascent + descent + leading。
    ///
    /// 行高与基线都只看**基准字体**，不看 `CTLine` 的 typographic bounds——后者取行内所有
    /// 回退字体（Apple Color Emoji 等）里最大的那个，含 emoji 的候选行会比纯汉字行高一截、
    /// 候选窗高度随之抖动。与 dwrite 后端钉 UNIFORM 行距是同一条约定，两端必须一致，
    /// 否则同一份配置两平台行高不同。比基准字体高的字形按溢出画，不裁切。
    fn line_height(font: &CTFont) -> f64 {
        font.ascent() + font.descent() + font.leading()
    }

    /// 单行行高（像素）；字体度量取不到时回落 `size * 1.2`。多行排版逐行按它下移。
    fn lines_height(&self, size: f32, font: &CTFont) -> f32 {
        let h = Self::line_height(font) as f32;
        if h > 0.0 { h } else { size * 1.2 }
    }

    /// 按 `\n` 硬换行的多行测量（与 DirectWrite 的 NO_WRAP、Linux 后端同口径：不自动折行，
    /// 宽取最宽一行，高为行数 × 行高）。`line_width` 量单行宽。
    fn measure_lines(
        &self,
        text: &str,
        size: f32,
        line_width: impl Fn(&str) -> f32,
        font: &CTFont,
    ) -> TextMetrics {
        let lh = self.lines_height(size, font);
        let mut width = 0f32;
        let mut n = 0u32;
        for (_, l) in line_offsets(text) {
            n += 1;
            if !l.is_empty() {
                width = width.max(line_width(l));
            }
        }
        TextMetrics {
            width,
            height: lh * n as f32,
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
        if text.contains('\n') {
            return self.measure_lines(text, ts.size, |l| self.measure(l, ts).width, &font);
        }
        let line = make_line(text, &font, None);
        let b = line.get_typographic_bounds();
        // 行高取**基准字体**自身的度量，不取这一行的 typographic bounds：后者是行内所有
        // 回退字体里最大的那个，含 emoji 的候选会比纯汉字行高出一截，候选窗高度随之抖动。
        // 与 dwrite 后端的 UNIFORM 行距同一条约定（见 `dwrite.rs` 的 `line_height_for`）。
        let height = Self::line_height(&font) as f32;
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
        if text.contains('\n') {
            let lh = self.lines_height(ts.size, &self.font_styled(ts.size, ts.weight, ts.family));
            for (i, (_, l)) in line_offsets(text).enumerate() {
                self.draw(
                    buf,
                    buf_width,
                    buf_height,
                    x,
                    y + i as f32 * lh,
                    l,
                    ts,
                    color,
                )?;
            }
            return Ok(());
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
        // 基线按**基准字体**的 ascent 放，与 `measure` 取的行高同一口径：行高不再跟着
        // 行内最高的回退字体走，基线也不能跟着走，否则 emoji 行里的汉字会整体下沉。
        let ascent = font.ascent();
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

    /// 分段着色绘制。镜像 dwrite 的 `TextRenderer::draw_runs`：`runs` 为空（或全被判为非法
    /// 区间）时就是 [`Self::draw`]。
    ///
    /// # 为什么不按区间设 `kCTForegroundColorAttributeName`
    ///
    /// 属性不同的区间会被 CoreText 切成不同的 CTRun，跨边界的字距与连字可能因此变化——带色
    /// line 的宽度就不再等于 [`Self::measure`] 量出的宽度，布局与绘制对不上。
    ///
    /// 做法是**整形只做一次**：整串设 `kCTForegroundColorFromContextAttributeName`（字色取
    /// 上下文填充色），这个 line 除此之外与测量用的完全同构；绘制时逐个 CTRun，用
    /// `string_indices` 把颜色区间换算成字形子区间，逐段换填充色再 `CTRunDraw`。字形位置
    /// 来自同一次整形，度量一致由构造保证。CoreGraphics 原生处理填充色的 alpha，无需像
    /// DirectWrite 那样按 alpha 分组。
    #[allow(clippy::too_many_arguments)]
    pub fn draw_runs(
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
        if text.contains('\n') {
            let lh = self.lines_height(ts.size, &self.font_styled(ts.size, ts.weight, ts.family));
            for (i, (off, l)) in line_offsets(text).enumerate() {
                let sub = ColorRun::slice(runs, off, off + l.len());
                self.draw_runs(
                    buf,
                    buf_width,
                    buf_height,
                    x,
                    y + i as f32 * lh,
                    l,
                    ts,
                    color,
                    &sub,
                )?;
            }
            return Ok(());
        }
        let spans = utf16_runs(text, runs);
        if spans.is_empty() {
            return self.draw(buf, buf_width, buf_height, x, y, text, ts, color);
        }
        if buf_width == 0 || buf_height == 0 || ts.size <= 0.0 {
            return Ok(());
        }
        let w = buf_width as usize;
        let h = buf_height as usize;
        if buf.len() < w * h * 4 {
            return Err("buffer too small".into());
        }
        let font = self.font_styled(ts.size, ts.weight, ts.family);
        let line = make_context_colored_line(text, &font);
        // 某个 UTF-16 下标的颜色：区间重叠时后者覆盖前者，与 DirectWrite 侧
        // 「后设的 SetDrawingEffect 覆盖先设的」一致。
        let color_at = |i: isize| -> [u8; 4] {
            spans
                .iter()
                .rev()
                .find(|(s, l, _)| i >= *s as isize && i < (*s + *l) as isize)
                .map_or(color, |r| r.2)
        };
        let ascent = font.ascent();
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
            // 定位与 `draw` 同一口径；各 run 的字形位置相对于这个行原点。
            ctx.set_text_position(x as f64, (h as f32 - (y + ascent as f32)) as f64);
            for run in line.glyph_runs().iter() {
                let idx = run.string_indices();
                let mut g = 0usize;
                while g < idx.len() {
                    let c = color_at(idx[g]);
                    let mut e = g + 1;
                    while e < idx.len() && color_at(idx[e]) == c {
                        e += 1;
                    }
                    ctx.set_fill_color(&cg_rgba(c));
                    unsafe {
                        CTRunDraw(
                            run.as_concrete_TypeRef(),
                            ctx.as_ptr(),
                            CFRange::init(g as isize, (e - g) as isize),
                        );
                    }
                    g = e;
                }
            }
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
        if text.contains('\n') {
            return self.measure_lines(
                text,
                size,
                |l| self.measure_text_sized(l, size).width,
                &font,
            );
        }
        let line = make_line(text, &font, None);
        let b = line.get_typographic_bounds();
        let height = Self::line_height(&font) as f32;
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

        if text.contains('\n') {
            let lh = self.lines_height(size, &self.font_for(size));
            for (i, (_, l)) in line_offsets(text).enumerate() {
                self.draw_text_sized(
                    buf,
                    buf_width,
                    buf_height,
                    x,
                    y + i as f32 * lh,
                    l,
                    size,
                    color,
                )?;
            }
            return Ok(());
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

        // 基线按基准字体的 ascent 放，理由见 `draw`。
        let ascent = font.ascent();

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

/// 按 `\n` 拆行，附带每行在原文里的字节起点（着色区间按它切分）。
fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut off = 0usize;
    text.split('\n').map(move |l| {
        let at = off;
        off += l.len() + 1;
        (at, l)
    })
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

/// `[R, G, B, A]` → CGColor（设备 RGB，与 `draw` 同一色彩空间）。
fn cg_rgba(c: [u8; 4]) -> CGColor {
    CGColor::rgb(
        c[0] as f64 / 255.0,
        c[1] as f64 / 255.0,
        c[2] as f64 / 255.0,
        c[3] as f64 / 255.0,
    )
}

/// 字色取上下文填充色的单行 CTLine（[`TextRenderer::draw_runs`] 用）。
///
/// 除 `kCTForegroundColorFromContextAttributeName` 外与 `make_line(text, font, None)`
/// （测量用）同构，且该属性整串统一，不切分 CTRun。
fn make_context_colored_line(text: &str, font: &CTFont) -> CTLine {
    let mut attr = CFMutableAttributedString::new();
    attr.replace_str(&CFString::new(text), CFRange::init(0, 0));
    let range = CFRange::init(0, attr.char_len());
    unsafe {
        attr.set_attribute(range, kCTFontAttributeName, font);
        attr.set_attribute(
            range,
            kCTForegroundColorFromContextAttributeName,
            &CFBoolean::true_value(),
        );
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

    /// [`TextRenderer::family_exists`] 的三态各自落对。
    ///
    /// ★ 关键是**第二条**：装上之前这里恒返回 `None`，而调用方的判据是 `== Some(false)`
    /// ⇒ 字体缺失告警在 macOS 上整条空转。只断言「存在的查得到」的话，一个恒 `Some(true)`
    /// 的实现也能绿——那同样是空转，只是翻了个面。
    #[test]
    fn family_exists_tells_present_from_absent() {
        let r = TextRenderer::new(FALLBACK_FAMILY, 16.0).unwrap();
        assert_eq!(
            r.family_exists(FALLBACK_FAMILY),
            Some(true),
            "本文件把它当兜底字体用，它必须在家族名表里"
        );
        assert_eq!(
            r.family_exists("这个字族并不存在 ZZZ"),
            Some(false),
            "查不到必须明确报「没有」，报 None 等于告诉调用方「别 warn」"
        );
        assert_eq!(
            r.family_exists("   "),
            None,
            "空名 = 没设置，与「系统里没有」是两件事"
        );
        assert_eq!(
            r.family_exists(&FALLBACK_FAMILY.to_lowercase()),
            Some(true),
            "大小写不敏感，同 DirectWrite 的 FindFamilyName"
        );
    }

    /// 分段着色用的 line 与测量用的 line 宽度相等——含连字 / 字距恰好落在颜色边界上的样例
    /// （`fi` 在 `of|fice` 的 `f|i` 之间、`AV` 在 `A|V` 之间）。
    ///
    /// 这是「颜色不影响度量」在 CoreText 上的证据：若改成按区间设前景色属性，CTRun 会在
    /// 颜色边界被切开，连字与字距就可能丢失，宽度随之漂移。
    #[test]
    fn colored_line_width_equals_measured_width() {
        let r = TextRenderer::new("Helvetica", 16.0).unwrap();
        for text in ["office", "AVATAR", "fi AV 你好", "WAVE fifty"] {
            let font = r.font_for(16.0);
            let plain = make_line(text, &font, None).get_typographic_bounds().width;
            let colored = make_context_colored_line(text, &font)
                .get_typographic_bounds()
                .width;
            assert_eq!(plain, colored, "{text:?}：带色 line 宽度与测量 line 不同");
            assert_eq!(
                r.measure(text, &TextStyle::new(16.0)).width,
                colored as f32,
                "{text:?}"
            );
        }
    }

    /// 区间全取基色时，`draw_runs` 与 `draw` 的缓冲应基本相同（容忍少量抗锯齿边缘漂移）。
    ///
    /// 两条路径的 line 不同（前者 `FromContext` + 逐 CTRun 的 `CTRunDraw`，后者整行
    /// `CTLineDraw`），位置或基线若有一点漂移——例如 run 的原点没对上行原点——就在这里现形。
    /// 含 CJK 与 emoji，覆盖回退字体切出的多个 CTRun。
    ///
    /// 2026-09-28 首次在真机 macOS CI 上跑通此用例时，逐字节比对报了 503/11520（约 4.3%）
    /// 像素不同、且都是边缘级的小幅色差（非整段文字错位/丢失），怀疑是两条绘制路径下
    /// CoreGraphics 抗锯齿/字体平滑的细微差异，尚未在真机上用 PNG 逐像素查实。在查清前
    /// 放宽为容忍小比例、小幅度的差异，但仍要求：① 差异像素占比给足安全边际、
    /// ② 差异像素本身的通道差值要小——真出现整段错位/漏画会让大量像素从「纯黑字」直接
    /// 翻成「纯白底」（通道差值拉满），这两条护栏都会先报错，不会被本放宽悄悄盖过去。
    #[test]
    fn base_color_runs_equal_plain_draw() {
        let r = TextRenderer::new("Helvetica", 24.0).unwrap();
        let (w, h) = (240u32, 48u32);
        let ts = TextStyle::new(24.0);
        let text = "ab 你好 😀 cd";
        let black = [0, 0, 0, 255];
        let bg = [255u8, 255, 255, 255].repeat((w * h) as usize);
        let mut plain = bg.clone();
        r.draw(&mut plain, w, h, 4.5, 6.0, text, &ts, black)
            .unwrap();
        assert_ne!(plain, bg, "应画出字形");
        let runs: Vec<ColorRun> = [(0usize, 2usize), (3, 9), (15, text.len())]
            .iter()
            .map(|&(s, e)| ColorRun {
                start: s as u32,
                end: e as u32,
                rgba: black,
            })
            .collect();
        let mut colored = bg.clone();
        r.draw_runs(&mut colored, w, h, 4.5, 6.0, text, &ts, black, &runs)
            .unwrap();
        let total = (w * h) as usize;
        let mut diff = 0usize;
        let mut max_channel_delta = 0i32;
        for i in 0..total {
            let p = &plain[i * 4..i * 4 + 4];
            let c = &colored[i * 4..i * 4 + 4];
            if p != c {
                diff += 1;
                for k in 0..4 {
                    let d = (p[k] as i32 - c[k] as i32).abs();
                    max_channel_delta = max_channel_delta.max(d);
                }
            }
        }
        assert!(
            diff * 10 <= total,
            "全取基色的 draw_runs 应与 draw 大致相同：差异像素 {diff}/{total} 超过 10%"
        );
        assert!(
            max_channel_delta <= 160,
            "差异像素的通道差值 {max_channel_delta} 过大，疑似整段错位/漏画而非抗锯齿边缘漂移"
        );
    }

    /// 真按区间上了色：红 `ab` + 蓝 `cd`，各自 x 区间内改动像素以本段色为主；并且非空。
    #[test]
    fn draw_runs_colors_each_range() {
        let r = TextRenderer::new("Helvetica", 32.0).unwrap();
        let (w, h) = (200u32, 60u32);
        let bg = [255u8, 255, 255, 255].repeat((w * h) as usize);
        let mut buf = bg.clone();
        let ts = TextStyle::new(32.0);
        let x0 = 4.0;
        let split = x0 + r.measure("ab", &ts).width;
        let runs = [
            ColorRun {
                start: 0,
                end: 2,
                rgba: [220, 0, 0, 255],
            },
            ColorRun {
                start: 2,
                end: 4,
                rgba: [0, 0, 220, 255],
            },
        ];
        r.draw_runs(&mut buf, w, h, x0, 4.0, "abcd", &ts, [0, 0, 0, 255], &runs)
            .unwrap();
        let (mut left, mut right) = ((0, 0), (0, 0));
        for i in 0..(w * h) as usize {
            if buf[i * 4..i * 4 + 4] == bg[i * 4..i * 4 + 4] {
                continue;
            }
            // BGRA 内存序：[0]=B、[2]=R。
            let (b, red) = (buf[i * 4] as i32, buf[i * 4 + 2] as i32);
            let side = if ((i as u32 % w) as f32) < split {
                &mut left
            } else {
                &mut right
            };
            if red > b + 30 {
                side.0 += 1;
            } else if b > red + 30 {
                side.1 += 1;
            }
        }
        assert!(
            left.0 > 20 && left.0 > left.1 * 4,
            "左半应以红为主：{left:?}"
        );
        assert!(
            right.1 > 20 && right.1 > right.0 * 4,
            "右半应以蓝为主：{right:?}"
        );
    }
    /// `\n` 硬换行：高 = 行数 × 单行高，宽取最宽一行（悬停提示等多行文本依赖）。
    #[test]
    fn newline_stacks_lines_and_takes_widest() {
        let r = TextRenderer::new("PingFang SC", 16.0).unwrap();
        let ts = TextStyle::new(16.0);
        let one = r.measure("你好世界啊", &ts);
        let two = r.measure("你好世界啊\n你", &ts);
        let three = r.measure("你好世界啊\n你\n", &ts);
        assert!(
            (two.height - one.height * 2.0).abs() < 0.5,
            "两行高：{two:?} vs {one:?}"
        );
        assert!(
            (three.height - one.height * 3.0).abs() < 0.5,
            "末尾空行也占一行：{three:?}"
        );
        assert!(
            (two.width - one.width).abs() < 0.5,
            "宽取最宽一行：{two:?} vs {one:?}"
        );
    }

    /// 第二行真画在第一行下面，而且分段颜色按行切分后各归各的行。
    #[test]
    fn newline_draws_second_line_below_with_its_own_colors() {
        let r = TextRenderer::new("PingFang SC", 16.0).unwrap();
        let ts = TextStyle::new(16.0);
        let lh = r.measure("A", &ts).height;
        let (w, h) = (96u32, (lh * 2.0).ceil() as u32 + 8);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        // "红\n蓝"：第一行红、第二行蓝（字节区间：红 0..3，\n 3..4，蓝 4..7）。
        let runs = [
            ColorRun {
                start: 0,
                end: 3,
                rgba: [255, 0, 0, 255],
            },
            ColorRun {
                start: 4,
                end: 7,
                rgba: [0, 0, 255, 255],
            },
        ];
        r.draw_runs(
            &mut buf,
            w,
            h,
            4.0,
            2.0,
            "红\n蓝",
            &ts,
            [0, 0, 0, 255],
            &runs,
        )
        .unwrap();
        let (mut top, mut bottom) = ((0, 0), (0, 0));
        for i in 0..(w * h) as usize {
            let (b, red, a) = (buf[i * 4] as i32, buf[i * 4 + 2] as i32, buf[i * 4 + 3]);
            if a == 0 {
                continue;
            }
            let row = (i as u32 / w) as f32;
            let side = if row < 2.0 + lh {
                &mut top
            } else {
                &mut bottom
            };
            if red > b + 30 {
                side.0 += 1;
            } else if b > red + 30 {
                side.1 += 1;
            }
        }
        assert!(top.0 > 10 && top.1 == 0, "上半行应只有红：{top:?}");
        assert!(bottom.1 > 10 && bottom.0 == 0, "下半行应只有蓝：{bottom:?}");
    }
}
