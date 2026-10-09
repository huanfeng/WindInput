//! 文本渲染后端（DirectWrite 实现）
//!
//! 与 Go 版本 `wind_input/internal/ui/dwrite_text.go` 对齐。
//!
//! 管线：IDWriteFactory → IDWriteTextFormat/IDWriteTextLayout（测量）
//!      → IDWriteGdiInterop::CreateBitmapRenderTarget（内存 DC 上的 32bpp 顶端向下 DIB）
//!      → 自定义 IDWriteTextRenderer 回调里调 IDWriteBitmapRenderTarget::DrawGlyphRun
//!      → 预乘 alpha 选择性回写到调用方 BGRA 缓冲区。
//!
//! 透明度正确性（修复 GDI 旧实现"黑字被当背景吞掉、抗锯齿丢失"）：
//! 先把目标缓冲区按"不透明"复制进 DIB（GDI 对不透明背景做抗锯齿混合），渲染后
//! 逐像素对比——RGB 未变 = 背景，保留原 alpha；RGB 变了 = 文字像素，按窗口原
//! alpha 预乘（R' = R×A/255），使其成为合法预乘像素，与背景共享同一透明度。

/// 文本度量信息
#[derive(Debug, Clone)]
pub struct TextMetrics {
    pub width: f32,
    pub height: f32,
}

/// 一次文字排版所需的全部字体属性。
///
/// ## 为什么收成结构体
///
/// 这些属性要穿过三个后端（DirectWrite / CoreText / mock）和全部测量与绘制调用点。
/// 散开成位置参数时，每加一项都要改所有签名——重构前的 `draw_text_styled` 已是 11 个参数，
/// 再加行高、斜体就到 13 个，而参数越多，传错顺序时编译器越抓不到（`size`/`weight`
/// 都是数值，换个位置照样编译）。
///
/// 隔壁 wind-ui-rust 走过这条路并留下了教训：字重就是因为"每加一项都要改所有签名"
/// 而**没有进签名**，改走线程局部注入——于是字重成了隐式全局状态，某条路径忘了复位，
/// 后续无关文字就跟着变粗，且只在特定绘制顺序下显形。收成结构体后新增属性只是加一个
/// 字段，签名不动、调用点不动。
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
/// **只作用于绘制，不参与测量**——整串仍一次整形，理由同 `View::caret_at`：拆开各自排版会让
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
/// `DWRITE_TEXT_RANGE` 与 CoreText 的 string index 都以 UTF-16 计（同 [`pua_runs`] 口径）。
///
/// 丢弃空区间、越界区间与不在字符边界上的区间：颜色只是装饰，坏区间让那段回落基色，
/// 不该让整段文字画不出来。
pub(crate) fn utf16_runs(text: &str, runs: &[ColorRun]) -> Vec<(u32, u32, [u8; 4])> {
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

/// 测量缓存容量上限；超过即整体清空。
///
/// 不做 LRU：候选窗每帧的文本集合高度重复（同一批候选、序号、注释反复测量），
/// 命中率本就极高，淘汰策略的簿记开销换不回收益。整体清空的最坏情况是一帧全 miss，
/// 等价于没有缓存时的行为。
#[cfg_attr(not(windows), allow(dead_code))]
const MEASURE_CACHE_CAP: usize = 4096;

/// 测量缓存键：`(文本, 字号, 字重, 字体族)` 的 64 位哈希。
///
/// 存哈希而非完整键，是为了免掉每次查询都克隆 `String`——测量在热路径上，一帧数十次。
/// 64 位下 4096 条目的碰撞概率约 4.5e-13，可忽略；真碰撞的后果是某段文本用了另一段的
/// 宽度（布局错位），故键必须**覆盖所有影响测量的输入**，漏一项就是系统性错位而非偶发。
///
/// 字号用 `to_bits()` 而非 `as u32`：字号是 DPI 缩放后的浮点（如 14.4/16.8），
/// 取整会让相邻字号撞进同一个键。
///
/// ⚠️ 给 [`TextStyle`] 加字段时**必须同步加进这里**——漏一项就是某段文本静默套用另一段
/// 的宽度。这正是它按整个 `TextStyle` 取参、而非重新罗列各项的原因：字段列表只有一处。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn measure_key(text: &str, ts: &TextStyle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    ts.size.to_bits().hash(&mut h);
    ts.weight.hash(&mut h);
    ts.family.hash(&mut h);
    h.finish()
}

/// 扫描 UTF-16 序列，返回私用区（PUA）字符的连续段 `[(起始下标, 码元长度)]`，
/// 下标/长度均以 **UTF-16 码元** 计，可直接用作 `DWRITE_TEXT_RANGE`。
///
/// 三段私用区缺一不可——不同拆字库用的区不同：内置 wubi86 字根在 BMP 私用区
/// （U+E0E1 等），而 986 等第三方码表的字根在补充私用区 A（U+F00FD 等）。
/// 早期只判 BMP 一段，导致后者从不切字体、渲染成方框。
///
/// - BMP 私用区 `U+E000..=U+F8FF`：单码元，`u16` 值即码位。
/// - 补充私用区 A/B `U+F0000..=U+10FFFD`：UTF-16 下是代理对。高位代理恰好占满
///   `0xDB80..=0xDBFF`（`0xDB80..=0xDBBF` → 第 15 平面，`0xDBC0..=0xDBFF` → 第 16
///   平面），不多不少，故判「高位代理落在该段 + 后随合法低位代理」即可，无需还原码位。
///
/// 相邻的 BMP 与补充私用区字符合并进同一段——它们目标字体族相同，合并只减少
/// `SetFontFamilyName` 调用次数，不改变渲染结果。
#[cfg_attr(not(windows), allow(dead_code))]
fn pua_runs(wide: &[u16]) -> Vec<(usize, usize)> {
    /// 单码元即为私用区码位（BMP PUA）。
    fn is_bmp_pua(u: u16) -> bool {
        (0xE000..=0xF8FF).contains(&u)
    }
    /// 补充私用区 A/B 的高位代理段。
    fn is_spua_lead(u: u16) -> bool {
        (0xDB80..=0xDBFF).contains(&u)
    }
    /// 任意低位代理（配对合法性；具体码位无需还原）。
    fn is_trail(u: u16) -> bool {
        (0xDC00..=0xDFFF).contains(&u)
    }

    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0usize;
    while i < wide.len() {
        // 命中长度：1=BMP 私用区，2=补充私用区代理对，0=非私用区。
        let step = if is_bmp_pua(wide[i]) {
            1
        } else if is_spua_lead(wide[i]) && wide.get(i + 1).is_some_and(|&t| is_trail(t)) {
            2
        } else {
            0
        };
        if step == 0 {
            if let Some(s) = start.take() {
                runs.push((s, i - s));
            }
            i += 1;
        } else {
            start.get_or_insert(i);
            i += step;
        }
    }
    if let Some(s) = start {
        runs.push((s, wide.len() - s));
    }
    runs
}

#[cfg(windows)]
pub use imp::{GlyphRunFont, TextRenderer};

/// Windows 实现（DirectWrite）。非 Windows 平台见文件末尾的 mock。
#[cfg(windows)]
mod imp {
    use super::{ColorRun, MEASURE_CACHE_CAP, TextMetrics, TextStyle, measure_key};
    use crate::text::script::{FontPlan, font_runs};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::c_void;

    use windows::Win32::Foundation::{BOOL, COLORREF, DWRITE_E_NOCOLOR, FALSE};
    use windows::Win32::Graphics::DirectWrite::*;
    use windows::Win32::Graphics::Gdi::{DIBSECTION, GetCurrentObject, GetObjectW, OBJ_BITMAP};
    use windows::core::{Interface, PCWSTR, implement};

    /// 五笔字根字体的 DirectWrite 家族名（HeiTiZiGen.ttf 的 name 表家族名）。
    const CHAIZI_FAMILY: &str = "黑体字根";

    /// 拆字字根字体（自定义字体集 + 家族名），用于 PUA 字根字符的级联回退渲染。
    struct ChaiziFont {
        collection: IDWriteFontCollection1,
        family: Vec<u16>,
    }

    /// 渲染表面：尺寸绑定的位图渲染目标 + 其专属字形渲染器回调对象。
    struct Surface {
        target: IDWriteBitmapRenderTarget,
        renderer: IDWriteTextRenderer,
        width: u32,
        height: u32,
    }

    /// 读一个 `IDWriteLocalizedStrings` 的全部 `(locale, 名字)`。
    fn localized(strings: &IDWriteLocalizedStrings) -> Vec<(String, String)> {
        let mut out = Vec::new();
        unsafe {
            for i in 0..strings.GetCount() {
                let loc = strings.GetLocaleNameLength(i).ok().and_then(|len| {
                    let mut buf = vec![0u16; len as usize + 1];
                    strings
                        .GetLocaleName(i, &mut buf)
                        .ok()
                        .map(|_| String::from_utf16_lossy(&buf[..len as usize]))
                });
                let name = strings.GetStringLength(i).ok().and_then(|len| {
                    let mut buf = vec![0u16; len as usize + 1];
                    strings
                        .GetString(i, &mut buf)
                        .ok()
                        .map(|_| String::from_utf16_lossy(&buf[..len as usize]))
                });
                if let (Some(l), Some(n)) = (loc, name) {
                    out.push((l, n));
                }
            }
        }
        out
    }

    /// 读字体的某项信息串（全名 / GDI family 名等），没有就是空表。
    fn informational(
        font: &IDWriteFont,
        id: DWRITE_INFORMATIONAL_STRING_ID,
    ) -> Vec<(String, String)> {
        unsafe {
            let mut strings: Option<IDWriteLocalizedStrings> = None;
            let mut exists = BOOL(0);
            if font
                .GetInformationalStrings(id, &mut strings, &mut exists)
                .is_err()
                || !exists.as_bool()
            {
                return Vec::new();
            }
            strings.map(|s| localized(&s)).unwrap_or_default()
        }
    }

    /// 文本渲染器
    pub struct TextRenderer {
        /// 字体族（宽字符，含结尾 0）
        family: Vec<u16>,
        /// 语言区域（宽字符，含结尾 0）
        locale: Vec<u16>,
        /// 基准字号（family 固定）；可按调用传不同字号（序号/注释相对偏移）。
        font_size: f32,
        factory: IDWriteFactory,
        /// 彩色字形拆层接口（Win8.1+）；取不到则退化为单色渲染。
        factory2: Option<IDWriteFactory2>,
        gdi_interop: IDWriteGdiInterop,
        params: IDWriteRenderingParams,
        /// 文本格式缓存：按字号（取整 px）keyed，避免每帧重建 COM 对象。
        formats: RefCell<HashMap<u32, IDWriteTextFormat>>,
        /// 文本测量缓存（键见 `measure_key`）：避免每帧重建 `IDWriteTextLayout`。
        ///
        /// 盒模型对同一段文本会测两到三次（measure 阶段一次、paint 阶段为算对齐再一次、
        /// 有 caret 时再测前半段），上翻布局生效时整棵树还会重建重测。没有这层缓存时，
        /// 每一次都是一个新的 COM 对象 + 一次完整排版。
        measure_cache: RefCell<HashMap<u64, TextMetrics>>,
        /// 当前位图渲染表面（按需重建）
        surface: RefCell<Option<Surface>>,
        /// 拆字字根字体（可选）：设置后对 PUA 码位字符级联回退到该字体渲染。
        chaizi: Option<ChaiziFont>,
        /// 全局字体方案（来自 `ui.font`）：默认链 + 按脚本的字体指派。
        ///
        /// 零配置时两处短路各管一半、与升级前逐位等价：`declared().is_empty()` 跳过切段，
        /// `needs_fallback()` 跳过自定义回退对象。
        plan: FontPlan,
        /// 由 [`Self::plan`] 构建的自定义回退对象；`None` = 尚未构建或本方案不需要。
        /// 换方案时必须 take 掉（见 [`TextRenderer::set_font_plan`]）。
        fallback: RefCell<Option<IDWriteFontFallback>>,
        /// 回退对象构建失败过。失败是持久性的（工厂拿不到 `IDWriteFactory2` 等），
        /// 不记就会每次绘制重建一遍 builder 与全部映射。换方案时随 `fallback` 一起复位。
        fallback_failed: std::cell::Cell<bool>,
        /// 基准字族在各字号/字重下的自然 (行高, 基线)，供 [`Self::line_height_for`] 钉 UNIFORM
        /// 行距用。键 = (字号取整, 字重, base family)；与 `measure_cache` 同步清空
        /// （回退链换了度量口径就变）。
        line_heights: RefCell<HashMap<LineProbeKey, (f32, f32)>>,
        /// 全局默认字重（`ui.font.weight`，或旧 GDI 字体名里带的字重）；0 = 常规 400。
        /// 承载在 TextFormat 上，叶子字重（主题节点 `font_weight`）非 0 时在 layout 层覆盖它。
        default_weight: i32,
        /// 测试开关：让 [`Self::effect_key`] 恒错位（见其文档）。
        #[cfg(test)]
        pub(crate) break_effect_lookup: std::cell::Cell<bool>,
    }

    /// [`TextRenderer::line_heights`] 的键：(字号取整, 字重, base family)。
    type LineProbeKey = (u32, i32, String);

    impl TextRenderer {
        /// 创建文本渲染器
        pub fn new(font_family: &str, font_size: f32) -> Result<Self, String> {
            unsafe {
                let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                    .map_err(|e| format!("DWriteCreateFactory: {e}"))?;
                let gdi_interop = factory
                    .GetGdiInterop()
                    .map_err(|e| format!("GetGdiInterop: {e}"))?;
                // 默认渲染参数（系统 ClearType 设置）
                let params = factory
                    .CreateRenderingParams()
                    .map_err(|e| format!("CreateRenderingParams: {e}"))?;
                // IDWriteFactory2（Win8.1+）提供彩色字形拆层；取不到则退化为单色。
                let factory2: Option<IDWriteFactory2> = factory.cast().ok();

                let family: Vec<u16> = font_family
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                let locale: Vec<u16> = "zh-cn".encode_utf16().chain(std::iter::once(0)).collect();

                Ok(Self {
                    family,
                    locale,
                    font_size,
                    factory,
                    factory2,
                    gdi_interop,
                    params,
                    formats: RefCell::new(HashMap::new()),
                    measure_cache: RefCell::new(HashMap::new()),
                    line_heights: RefCell::new(HashMap::new()),
                    surface: RefCell::new(None),
                    chaizi: None,
                    plan: FontPlan::default(),
                    fallback: RefCell::new(None),
                    fallback_failed: std::cell::Cell::new(false),
                    default_weight: 0,
                    #[cfg(test)]
                    break_effect_lookup: std::cell::Cell::new(false),
                })
            }
        }

        /// 加载拆字字根字体（TTF）建自定义字体集，后续渲染中 PUA 码位字符回退到它。
        /// `family` 为方案配置的 DWrite 家族名（空则回退默认 `CHAIZI_FAMILY`）。
        /// 失败返回 Err（不影响普通文本渲染）。
        /// 把「配置里声明的家族名」解析成**这个自定义字体集里真实存在**的家族名。
        ///
        /// # ★★ 为什么必须解析，不能直接用声明值
        ///
        /// `create_layout` 里给 PUA 段做的是 `SetFontCollection(自定义集)` +
        /// `SetFontFamilyName(声明名)`。后者在集里匹配不上时**不报错**——DirectWrite 静默
        /// 回落，于是「词库自带字体」整个没生效。这个失败有两副完全不像的面孔：
        ///
        /// - 机器上**装了**同一款字体：系统回退把字形兜住，画面上「有字」，但字形来自另一个
        ///   字体，与上屏后宿主挑的不一致 ⇒ 用户报「候选和实际有差异」；
        /// - 机器上**没装**：PUA 码位无处可取 ⇒ 那一段直接是**空白**。
        ///
        /// 实测（Toli 蒙古文方案，声明 `"Menk字体"`、字体自报 `"Menk Qagan StdEx Tig"`）：
        /// 声明名与「压根不设词库字体」的测量宽度和着墨像素**逐位相同**（110.03×22.86 / 424），
        /// 而真实家族名是 110.67×22.86 / 413 ⇒ 声明名一路没生效。
        ///
        /// ⚠️ 找不到时退回集里的**第一个**家族：字体文件是方案自带的、通常只含一个家族，
        /// 「作者把名字写成了别名或文件名」远比「集里真有好几个家族而他指了个不存在的」常见。
        /// 退回的同时 warn，否则修好了显示、却把「配置写错了」这件事一并藏起来。
        fn resolve_family_in(collection: &IDWriteFontCollection1, declared: &str) -> String {
            unsafe {
                let w: Vec<u16> = declared.encode_utf16().chain(std::iter::once(0)).collect();
                let mut index = 0u32;
                let mut exists = BOOL(0);
                if collection
                    .FindFamilyName(PCWSTR(w.as_ptr()), &mut index, &mut exists)
                    .is_ok()
                    && exists.as_bool()
                {
                    return declared.to_string();
                }
                let fallback = |what: &str| {
                    tracing::warn!(
                        "词库字体的家族名「{declared}」在该字体文件里不存在，且{what}，\
                         字体将不生效——请把方案 [engine.chaizi] 的 font_family 改成字体自报的家族名"
                    );
                    declared.to_string()
                };
                let Ok(first) = collection.GetFontFamily(0) else {
                    return fallback("取不到集里的第一个字族");
                };
                let Ok(names) = first.GetFamilyNames() else {
                    return fallback("取不到该字族的名字表");
                };
                let Ok(len) = names.GetStringLength(0) else {
                    return fallback("取不到该字族的名字长度");
                };
                let mut buf = vec![0u16; len as usize + 1];
                if names.GetString(0, &mut buf).is_err() {
                    return fallback("读不出该字族的名字");
                }
                let real = String::from_utf16_lossy(&buf[..len as usize]);
                tracing::warn!(
                    "词库字体的家族名「{declared}」在该字体文件里不存在，已改用它自报的「{real}」——\
                     请把方案 [engine.chaizi] 的 font_family 改成后者"
                );
                real
            }
        }

        /// `path` 为空 = 撤掉字根字体：私用区段不再被切到任何自定义字体集。
        ///
        /// ★ 先撤旧的再加载新的：加载失败时的结局必须是「没有字根字体」，而不是「沿用上一个
        /// 方案的」——协调器已把新路径记为已下发、不会重发，旧字体会一直接管新方案的私用区。
        /// 字根字体决定 PUA 字符的字形来源 → 测量宽度随之变，缓存一并清掉（不清的话切换拆字
        /// 方案后字根仍按旧字体的宽度布局，表现为字根格错位/重叠）。
        pub fn set_chaizi_font(&mut self, path: &str, family: &str) -> Result<(), String> {
            self.chaizi = None;
            self.measure_cache.borrow_mut().clear();
            self.line_heights.borrow_mut().clear();
            if path.is_empty() {
                return Ok(());
            }
            unsafe {
                let f3: IDWriteFactory3 = self
                    .factory
                    .cast()
                    .map_err(|e| format!("cast IDWriteFactory3: {e}"))?;
                let path_w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
                let file = f3
                    .CreateFontFileReference(PCWSTR(path_w.as_ptr()), None)
                    .map_err(|e| format!("CreateFontFileReference: {e}"))?;
                let builder: IDWriteFontSetBuilder1 = f3
                    .CreateFontSetBuilder()
                    .map_err(|e| format!("CreateFontSetBuilder: {e}"))?
                    .cast()
                    .map_err(|e| format!("cast IDWriteFontSetBuilder1: {e}"))?;
                builder
                    .AddFontFile(&file)
                    .map_err(|e| format!("AddFontFile: {e}"))?;
                let set = builder
                    .CreateFontSet()
                    .map_err(|e| format!("CreateFontSet: {e}"))?;
                let collection = f3
                    .CreateFontCollectionFromFontSet(&set)
                    .map_err(|e| format!("CreateFontCollectionFromFontSet: {e}"))?;
                let declared = if family.is_empty() {
                    CHAIZI_FAMILY
                } else {
                    family
                };
                let family_name = Self::resolve_family_in(&collection, declared);
                let family: Vec<u16> = family_name
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                self.chaizi = Some(ChaiziFont { collection, family });
                Ok(())
            }
        }

        /// 基准字号（View 叶子未显式指定字号时回退）。
        pub fn base_size(&self) -> f32 {
            self.font_size
        }

        /// 仅测试可见：当前是否挂着字根字体。
        #[cfg(test)]
        pub fn has_chaizi_font(&self) -> bool {
            self.chaizi.is_some()
        }

        /// 仅测试可见：当前测量缓存条目数。
        #[cfg(test)]
        pub fn measure_cache_len(&self) -> usize {
            self.measure_cache.borrow().len()
        }

        /// 更新基准字号（DPI 动态变化时调用）。格式按 px 缓存，无需重建 COM 对象，
        /// 仅改变未显式指定字号的叶子的回退字号。
        pub fn set_base_size(&mut self, size: f32) {
            self.font_size = size;
        }

        /// 切换字体族（ui.font.family 变更时调用）。清空按字号缓存的 TextFormat，使新字体生效。
        ///
        /// 测量缓存同样要清：其键里的字体族为 `None` 时表示"用全局 family"，全局一换，
        /// 这些条目记录的就是旧字体的宽度。
        pub fn set_font_family(&mut self, font_family: &str) {
            self.family = font_family
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            self.formats.borrow_mut().clear();
            self.measure_cache.borrow_mut().clear();
            self.line_heights.borrow_mut().clear();
        }

        /// 换字体方案（`ui.font` 的默认链与脚本指派变更时调用）。
        ///
        /// ⚠️ 两处缓存必须一起失效，各自的漏法不同：
        /// - **测量缓存**：方案不在 [`measure_key`] 里（它是渲染器级状态，与 `family` 同理），
        ///   不清就会拿旧方案的宽度布局新方案的文字——表现是候选框宽度对不上文字。
        /// - **回退对象**：`IDWriteFontFallback` 是按旧链构建的不可变 COM 对象，
        ///   不 take 掉就永远用旧链。
        ///
        /// `formats` **不需要**清：TextFormat 只承载字号与全局字族，而方案的 base family
        /// 是在 layout 层用 `SetFontFamilyName` 覆盖的，不经过 format。
        pub fn set_font_plan(&mut self, plan: FontPlan) {
            if self.plan == plan {
                return;
            }
            self.plan = plan;
            self.fallback.borrow_mut().take();
            self.fallback_failed.set(false);
            self.measure_cache.borrow_mut().clear();
            self.line_heights.borrow_mut().clear();
        }

        /// 当前字体方案。
        pub fn font_plan(&self) -> &FontPlan {
            &self.plan
        }

        /// 系统字体集里有没有这个字族名。`None` = 查不了（拿不到字体集），
        /// `Some(false)` = 系统里确实没有。
        ///
        /// # ★ 它补的是一处**没有任何信号**的失败
        ///
        /// [`Self::create_layout`] 里的 `SetFontFamilyName` 对不存在的字族**不报错**——
        /// DirectWrite 静默回落到默认字体，调用点连返回值都无从判断（那里本就写作
        /// `let _ = …`，因为有返回值也不知道该不该当失败）。用户把字体名写错、或填成了
        /// 字体的**全名/文件名**而不是家族名时，画面上只表现为「字体不对、字看着小了一圈」，
        /// 日志里一片安静。
        ///
        /// ⚠️ 只在**设置字体时**查（每次配置变更一次），不在热路径：`create_layout` 是
        /// 每帧每个文本叶子各走一遍的，那里加一次 COM 查询是按帧计费的。
        pub fn family_exists(&self, family: &str) -> Option<bool> {
            let name = family.trim();
            if name.is_empty() {
                return None;
            }
            unsafe {
                let mut collection: Option<IDWriteFontCollection> = None;
                self.factory
                    .GetSystemFontCollection(&mut collection, false)
                    .ok()?;
                let collection = collection?;
                let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                let mut index = 0u32;
                let mut exists = BOOL(0);
                collection
                    .FindFamilyName(PCWSTR(w.as_ptr()), &mut index, &mut exists)
                    .ok()?;
                Some(exists.as_bool())
            }
        }

        /// 按字体 **face** 的名字（GDI family 名 / 全名）在系统字体集里找，返回它所属的
        /// DirectWrite family 名（本地化名，zh-cn 优先）与该 face 的字重。
        ///
        /// 给存量配置里的旧 GDI face name 兜底（看板 A2-1）：「思源宋体 SemiBold」这类
        /// 剥字重词就能解析的走不到这里（见 `font_resolve::resolve_font_name` 的顺序），
        /// 这里接的是字重词表之外的写法、以及 family 名与 GDI 名根本不同的字体。
        ///
        /// 比对的是字体**自报**的名字，不用 `IDWriteGdiInterop::CreateFontFromLOGFONT`：
        /// 后者对查不到的名字做 GDI 模糊匹配、返回「最近似」的字体，正是要避免的静默纠错；
        /// 且 `LOGFONTW.lfFaceName` 只有 31 个字符，长名字会被截断。
        ///
        /// ⚠️ 全量遍历系统字体集（几千个 face），只在名字**已确认**不是 family 名时才调，
        /// 且调用方按名字缓存结果——不在渲染热路径。
        pub fn find_face(&self, name: &str) -> Option<(String, i32)> {
            use wind_config::font_name::{font_name_eq, pick_localized_name};
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            unsafe {
                let mut collection: Option<IDWriteFontCollection> = None;
                self.factory
                    .GetSystemFontCollection(&mut collection, false)
                    .ok()?;
                let collection = collection?;
                // (family 名, 字重, 是否斜体)。同一个 GDI 名常对应一组 face（常规/粗/斜），
                // 取非斜体里最接近常规的那个——用户选的是「这个字体」而不是它的粗体变体。
                let mut best: Option<(String, i32, bool)> = None;
                for fi in 0..collection.GetFontFamilyCount() {
                    let Ok(family) = collection.GetFontFamily(fi) else {
                        continue;
                    };
                    let family_name = || {
                        family
                            .GetFamilyNames()
                            .ok()
                            .and_then(|n| pick_localized_name(&localized(&n)))
                    };
                    for i in 0..family.GetFontCount() {
                        let Ok(font) = family.GetFont(i) else {
                            continue;
                        };
                        if font.GetSimulations() != DWRITE_FONT_SIMULATIONS_NONE {
                            continue; // 模拟粗/斜是算出来的，不是一个真实 face
                        }
                        let weight = font.GetWeight().0;
                        let full = informational(&font, DWRITE_INFORMATIONAL_STRING_FULL_NAME);
                        if full.iter().any(|(_, n)| font_name_eq(n, name)) {
                            // 全名唯一指向一个 face，直接用。
                            return family_name().map(|f| (f, weight));
                        }
                        let gdi =
                            informational(&font, DWRITE_INFORMATIONAL_STRING_WIN32_FAMILY_NAMES);
                        if gdi.iter().any(|(_, n)| font_name_eq(n, name)) {
                            let italic = font.GetStyle() != DWRITE_FONT_STYLE_NORMAL;
                            let better = best.as_ref().is_none_or(|(_, w, it)| {
                                (italic, (weight - 400).abs()) < (*it, (*w - 400).abs())
                            });
                            if better && let Some(f) = family_name() {
                                best = Some((f, weight, italic));
                            }
                        }
                    }
                }
                best.map(|(f, w, _)| (f, w))
            }
        }

        /// 按与 [`Self::draw`] 相同的 layout 排一遍 `text`，报出每段字形**实际**用到的字体。
        ///
        /// 诊断用（`examples/font_weight_probe.rs`）：「字重设了没效果」到底是配置没传到、
        /// 还是字体本身没有那个字重、或被系统回退换成了别的字体，只有渲染端自己的说法算数。
        pub fn probe_glyph_runs(
            &self,
            text: &str,
            ts: &TextStyle,
        ) -> Result<Vec<GlyphRunFont>, String> {
            let layout = self.create_layout(text, ts, f32::MAX, f32::MAX)?;
            let rec = RunRecorder::default();
            let runs = rec.runs.clone();
            let renderer: IDWriteTextRenderer = rec.into();
            unsafe {
                layout
                    .Draw(None, &renderer, 0.0, 0.0)
                    .map_err(|e| format!("TextLayout::Draw: {e}"))?;
            }
            Ok(runs.take())
        }

        /// 设全局默认字重（`ui.font.weight` 或旧字体名里带的字重；0 = 常规）。
        ///
        /// 三处缓存都要清：TextFormat 按字号缓存、字重就烧在它里面；测量与行高缓存的键里
        /// 字重 0 表示「继承默认」，默认一换这些条目记的就是旧字重的宽高。
        pub fn set_default_weight(&mut self, weight: i32) {
            if self.default_weight == weight {
                return;
            }
            self.default_weight = weight;
            self.formats.borrow_mut().clear();
            self.measure_cache.borrow_mut().clear();
            self.line_heights.borrow_mut().clear();
        }

        /// TextFormat 实际承载的字重。
        fn format_weight(&self) -> i32 {
            // 上游已钳过；此处再钳一次，越界值传给 CreateTextFormat 会直接失败。
            if self.default_weight > 0 {
                self.default_weight.clamp(1, 950)
            } else {
                400
            }
        }

        /// 取（或构建）本方案的自定义字体回退对象。方案不需要回退时返回 `None`——
        /// 不设自定义回退 = 走系统默认回退 = 与升级前逐位等价，故这条路径零回归。
        ///
        /// # ★★ 实测得到的 `AddMapping` 语义（推翻过一版猜测，别再按猜测改）
        ///
        /// **base font 先被问；映射的 target 列表是「base 缺这个字时」的接续。**
        /// 三条实测互相印证：
        /// - `fallback_chain_is_applied`：base 缺字时链尾决定字形 ⇒ target 列表确实生效；
        /// - `default_chain_fallback_does_not_swallow_a_script_assignment`：
        ///   拉丁段（base=Consolas）在默认链兜底映射存在时仍是 Consolas ⇒ base 优先；
        /// - `leaf_family_survives_a_non_empty_default_chain`：叶子级字族同理不被换掉。
        ///
        /// ⚠️ 曾经写成「它不问 base font、每个字符都会被映射到 target」——**那是错的**，
        /// 并且按那条错误前提推导会得出「兜底映射会把脚本指派和方案级字体整个吃掉」
        /// 这个吓人但不存在的结论。改这段前先跑上面三条测量。
        ///
        /// target 列表**含链首自己**是刻意的：base 已被优先问过，重复一次是无害的冗余，
        /// 但它让「这条链完整地写在一处」，读代码时不必再去脑补 base 是谁。
        ///
        /// # ⚠️ 依赖 `baseFamilyName` 作为筛选条件
        ///
        /// 多条链（默认链 + 各脚本指派链）共存时，靠 `baseFamilyName` 区分「这一段该走哪条
        /// 链」——而段的 base family 正是由 `SetFontFamilyName` 按 [`font_runs`] 的切段设定的，
        /// 两者天然对齐。这条假设由 `fallback_chain_picks_the_chain_of_its_own_base`
        /// 直接验证（**单渲染器、双链、链尾不同**）；`fallback_chain_is_applied` 只证明
        /// 「自定义回退挂上了」，证不了筛选——两条缺一不可。
        ///
        /// # 映射的添加顺序承重（`AddMapping` 取首个匹配的映射）
        ///
        /// 实际顺序由 [`FontPlan::chains`] 决定：**默认链在前、各脚本指派链其次**，
        /// 然后才是下面单独追加的两条。四段的相对位置都不能随手调：
        ///
        /// 1. 默认链（`baseFamilyName` = 它的链首）；
        /// 2. 各脚本指派链（`baseFamilyName` = 各自链首）；
        /// 3. 默认链的**兜底**映射（`baseFamilyName` = None，匹配任意 base）——必须在
        ///    全部具名映射之后，否则它会先匹配上、让具名链永远轮不到；
        /// 4. 系统表。
        ///
        /// ⚠️ 第 1、2 段的先后是 `heads` 去重「第一条胜出」语义的依据（同链首的后来者被
        /// warn 并丢弃）。照「最具体的最先」去重排会改掉那条语义。
        ///
        /// 第 3 段是为**叶子级字族**存在的：主题给节点配了 `font_family`（或方案级
        /// `[candidate] font_family`）时，该段的 base 与任何链首都不匹配。没有它，
        /// 用户配的 `ui.font.fallback` 对那些节点整条失效——而 macOS 侧
        /// （cascade list 是字体级属性）在同样情形下链照常生效，同一份配置两平台结果相反。
        ///
        /// 第 2 条是为了**主题节点字族**：主题给某个节点配了 `font_family`（如宋体）时，
        /// 该段的 base 是宋体，与任何一条链的链首都不匹配。没有这条兜底的话，用户配的
        /// `ui.font.fallback` 会**整条静默失效**——而 macOS 侧（CoreText 的 cascade list
        /// 是字体级属性）在同样情形下链照常生效，同一份配置两平台结果相反且都不吭声。
        fn ensure_fallback(&self) -> Option<IDWriteFontFallback> {
            if !self.plan.needs_fallback() {
                return None;
            }
            if let Some(fb) = self.fallback.borrow().as_ref() {
                return Some(fb.clone());
            }
            // 构建失败是持久性的（工厂能力问题），不记哨兵就会每次绘制重建一遍 builder
            // 与全部映射——一次失败变成每帧的开销。
            if self.fallback_failed.get() {
                return None;
            }
            let Some(f2) = self.factory2.as_ref() else {
                self.fallback_failed.set(true);
                return None;
            };
            unsafe {
                let Ok(builder) = f2.CreateFontFallbackBuilder() else {
                    self.fallback_failed.set(true);
                    return None;
                };
                const ALL: [DWRITE_UNICODE_RANGE; 1] = [DWRITE_UNICODE_RANGE {
                    first: 0,
                    last: 0x10FFFF,
                }];
                // `base = None` 表示不按 base family 筛选，匹配任意段。
                let add = |chain: &[String], base: Option<&str>| {
                    // 宽字符缓冲必须活到 AddMapping 返回之后——targets 里存的是裸指针。
                    let targets_w: Vec<Vec<u16>> = chain
                        .iter()
                        .map(|s| s.encode_utf16().chain(std::iter::once(0)).collect())
                        .collect();
                    let targets: Vec<*const u16> = targets_w.iter().map(|v| v.as_ptr()).collect();
                    let base_w: Option<Vec<u16>> =
                        base.map(|b| b.encode_utf16().chain(std::iter::once(0)).collect());
                    let base_ptr = base_w
                        .as_ref()
                        .map(|v| PCWSTR(v.as_ptr()))
                        .unwrap_or(PCWSTR::null());
                    let _ = builder.AddMapping(
                        &ALL,
                        &targets,
                        None,
                        PCWSTR(self.locale.as_ptr()),
                        base_ptr,
                        1.0,
                    );
                };

                let mut heads: Vec<&str> = Vec::new();
                for chain in self.plan.chains() {
                    if chain.len() < 2 {
                        continue; // 单项链无回退可言，建映射只会拖慢查表
                    }
                    // 两条链共用同一个链首时，靠 baseFamilyName 分不开——后加的那条永远
                    // 匹配不到（取首个匹配）。这是配置层的问题，静默丢掉会让用户查不出来。
                    if heads.contains(&chain[0].as_str()) {
                        tracing::warn!(
                            "字体方案里有多条链以「{}」开头，只有第一条的回退顺序会生效",
                            chain[0]
                        );
                        continue;
                    }
                    heads.push(chain[0].as_str());
                    add(chain, Some(&chain[0]));
                }
                // 默认链的兜底映射：放在全部具名映射之后，见函数文档「添加顺序承重」。
                let default_chain = self.plan.chain_for(None);
                if default_chain.len() > 1 {
                    add(default_chain, None);
                }
                // 系统表兜底放最后：不接的话，凡是自定义映射没覆盖到的字符（emoji、少数民族
                // 文字、符号）全部退化成缺字方框——自定义回退是**整体替换**系统回退，不是叠加。
                if let Ok(sys) = f2.GetSystemFontFallback() {
                    let _ = builder.AddMappings(&sys);
                }
                let Ok(fb) = builder.CreateFontFallback() else {
                    self.fallback_failed.set(true);
                    return None;
                };
                *self.fallback.borrow_mut() = Some(fb.clone());
                Some(fb)
            }
        }

        /// 取得（或创建）给定字号的文本格式（按取整 px 缓存）。
        fn ensure_format(&self, size: f32) -> Result<IDWriteTextFormat, String> {
            let key = size.max(1.0).round() as u32;
            if let Some(f) = self.formats.borrow().get(&key) {
                return Ok(f.clone());
            }
            unsafe {
                let fmt = self
                    .factory
                    .CreateTextFormat(
                        PCWSTR(self.family.as_ptr()),
                        None,
                        DWRITE_FONT_WEIGHT(self.format_weight()),
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        key as f32,
                        PCWSTR(self.locale.as_ptr()),
                    )
                    .map_err(|e| format!("CreateTextFormat: {e}"))?;
                self.formats.borrow_mut().insert(key, fmt.clone());
                Ok(fmt)
            }
        }

        /// 基准字族在该字号/字重下的**自然 (行高, 基线)**（按 (字号, 字重, 字族) 缓存）。
        ///
        /// 供 [`Self::create_layout`] 把每个 layout 的行距钉成 UNIFORM：不钉的话 DirectWrite 取
        /// 该行所有字体里最大的 line metrics，含 emoji 的候选行（回退到 Segoe UI Emoji）比
        /// 纯汉字行高 5%（雅黑 25.40 → 26.67）到 17%（宋体 22.81 → 26.60），基线也跟着挪，
        /// 候选窗高度随 emoji 出没而抖。
        ///
        /// 探针文本取「中a」：与今天纯汉字行的自然高度逐位相同（零回归）；带一个拉丁字母，
        /// 是让「基准字族缺汉字、汉字回退到别家」的方案也量到与汉字行一致的值。探针本身
        /// **不**钉行距，但走同一套回退对象，度量口径与正式 layout 一致。
        ///
        /// 缓存键里的字族是 layout 层实际设的 base（叶子字族 / 方案链首），与 `ensure_format`
        /// 只按字号缓存的 TextFormat 不同——同一字号下换字族行高就不同，所以钉在 layout 上。
        /// 字重同理：探针与正式 layout 一样 `SetFontWeight`，粗体字面纵向指标不同的字族
        /// 才不会被钉上常规字面的行框（键与 [`measure_key`] 覆盖同一组影响高度的输入）。
        ///
        /// 基线**必须**从探针的 line metrics 里读，不能写成行高的固定比例：各字族的
        /// ascent/(ascent+descent+lineGap) 从等线 0.78、雅黑 0.80、Segoe UI 0.81 到宋体
        /// （lineGap 非零，取决于 DirectWrite 把 lineGap 放在基线上方还是均分）不等。
        /// 写死一个比例，换基准字族后行高不变、字却整体上下漂，行底/行顶多出一条空白。
        fn line_height_for(
            &self,
            fmt: &IDWriteTextFormat,
            family: Option<&str>,
            weight: i32,
            size_key: u32,
        ) -> Option<(f32, f32)> {
            let key = (size_key, weight, family.unwrap_or("").to_string());
            if let Some(v) = self.line_heights.borrow().get(&key) {
                return Some(*v);
            }
            let probe: Vec<u16> = "中a".encode_utf16().collect();
            let (h, baseline) = unsafe {
                let layout = self
                    .factory
                    .CreateTextLayout(&probe, fmt, f32::MAX / 2.0, f32::MAX / 2.0)
                    .ok()?;
                let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let full = DWRITE_TEXT_RANGE {
                    startPosition: 0,
                    length: probe.len() as u32,
                };
                // 与 TextFormat 自带的默认字重比，而不是与 400 比：全局设了 600 时，
                // 节点显式写的 400 也得真的落下去。
                if weight > 0 && weight != self.format_weight() {
                    let _ = layout.SetFontWeight(DWRITE_FONT_WEIGHT(weight), full);
                }
                if let Some(fam) = family {
                    let famw: Vec<u16> = fam.encode_utf16().chain(std::iter::once(0)).collect();
                    let _ = layout.SetFontFamilyName(PCWSTR(famw.as_ptr()), full);
                }
                if let Some(fb) = self.ensure_fallback() {
                    let _ = layout
                        .cast::<IDWriteTextLayout2>()
                        .and_then(|l2| l2.SetFontFallback(&fb));
                }
                // 探针是单行（NO_WRAP、无 `\n`），一条 line metrics 就是整个行框。
                let mut lm = [DWRITE_LINE_METRICS::default()];
                let mut n = 0u32;
                layout.GetLineMetrics(Some(&mut lm), &mut n).ok()?;
                (lm[0].height, lm[0].baseline)
            };
            if h <= 0.0 || baseline <= 0.0 || baseline > h {
                return None;
            }
            self.line_heights.borrow_mut().insert(key, (h, baseline));
            Some((h, baseline))
        }

        /// 为给定文本/样式创建布局对象。
        ///
        /// 字体的三层合成，**从弱到强**依次覆盖（后者赢过前者）：
        /// 1. TextFormat 自带的全局字族（`ui.font.family`）；
        /// 2. 方案默认链的链首 → 叶子级 `ts.family`（主题节点的 `font_family`），作用于全文；
        /// 3. 按脚本的指派（[`font_runs`] 切段）→ 最后是拆字字根的私用区段。
        ///
        /// ★ 第 3 层赢过第 2 层是刻意的：脚本指派回答的是「**哪些字符**用什么字体」，
        /// 比「这个节点用什么字体」更具体。主题给候选文字配了宋体、全局又指派
        /// 「拉丁用 Segoe UI」时，结果是汉字宋体 + 拉丁 Segoe UI——这正是想要的。
        ///
        /// 参数收成 `&TextStyle` 而非散开：本文件开头已经为同一个理由写过一次
        /// （散开时每加一项都要改所有签名，而参数越多传错顺序编译器越抓不到）。
        fn create_layout(
            &self,
            text: &str,
            ts: &TextStyle,
            max_w: f32,
            max_h: f32,
        ) -> Result<IDWriteTextLayout, String> {
            self.create_layout_with(text, ts, max_w, max_h, true)
        }

        /// [`Self::create_layout`] 的全参版本。`uniform_spacing = false` 只给测试用：
        /// 回退链的那几条测试靠「哪款字体渲染了『中』」的**自然行高**做判据，钉了行距就
        /// 看不出来了；生产路径恒为 `true`。
        ///
        /// ⚠️ layout 目前**不缓存**：测量未命中与每次绘制各建一个，[`Self::draw_runs`] 的
        /// drawing effect 只设在绘制那个一次性 layout 上、用完即弃。将来若给绘制加 layout
        /// 缓存，effect 必须每次取出后重设、用后清掉，或带 effect 的 layout 不入缓存——否则
        /// 上一帧的颜色会残留到下一帧同一段文字上。
        fn create_layout_with(
            &self,
            text: &str,
            ts: &TextStyle,
            max_w: f32,
            max_h: f32,
            uniform_spacing: bool,
        ) -> Result<IDWriteTextLayout, String> {
            let (size, weight, family) = (ts.size, ts.weight, ts.family);
            let fmt = self.ensure_format(size)?;
            let wide: Vec<u16> = text.encode_utf16().collect();
            unsafe {
                let layout = self
                    .factory
                    .CreateTextLayout(&wide, &fmt, max_w.max(1.0), max_h.max(1.0))
                    .map_err(|e| format!("CreateTextLayout: {e}"))?;
                // 关闭自动换行：本 View 引擎是单行盒模型，容不下 DirectWrite 自作主张的折行。
                //
                // 测量与绘制传的 max_w 本就不同——测量传 f32::MAX（不换行），绘制传缓冲宽度。
                // 于是文本一旦宽过缓冲，布局按单行高度排、绘制却折成多行，多出来的行直接画到
                // 节点框外，盖住相邻候选。竖排的 behavior.vertical_max_width（出厂默认 0=不限，
                // 用户/主题可显式配正值）或渲染层恒生效的屏幕安全钳制都会把窗口宽度钳掉，
                // 触发这条路径。
                //
                // NO_WRAP 只关**自动**换行，`\n` 硬换行照旧生效（实测：含 \n 的文本仍返回
                // 2 倍行高）——candidate_window 依赖后者做多行候选，不能一起关掉。
                let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                // 节点级字重/字体族覆盖（作用于全文；下方 chaizi PUA 段会再覆盖字体族）。
                let full = DWRITE_TEXT_RANGE {
                    startPosition: 0,
                    length: wide.len() as u32,
                };
                // 与 TextFormat 自带的默认字重比，而不是与 400 比：全局设了 600 时，
                // 节点显式写的 400 也得真的落下去。
                if weight > 0 && weight != self.format_weight() {
                    let _ = layout.SetFontWeight(DWRITE_FONT_WEIGHT(weight), full);
                }
                // 叶子级字族优先，其次方案默认链的链首；都没有就沿用 TextFormat 的全局字族。
                let base = family
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| self.plan.base_family());
                if let Some(fam) = base {
                    let famw: Vec<u16> = fam.encode_utf16().chain(std::iter::once(0)).collect();
                    let _ = layout.SetFontFamilyName(PCWSTR(famw.as_ptr()), full);
                }
                // 按脚本指派：只有被显式声明了字体的类才会切出独立段（见 `font_runs`）。
                //
                // 外层的 `is_empty` 判定挡的是「没有任何声明」的方案：`font_runs` 在无声明时
                // 也会**分配**一个单段 Vec，而本函数是每次绘制每个文本叶子各走一遍的热路径。
                // Windows 出厂会注入 emoji 类的指派（`candidate_window::build_font_plan`），
                // 这条短路在 Windows 上通常走不到——每叶子每帧跑一遍 `font_runs`（几个字、
                // 微秒级）是为了 1️⃣ 与 Emoji 16.0 新码位付的账；用户写 `emoji = []` 关掉
                // 指派、以及 macOS，仍从这里短路。
                if !self.plan.declared().is_empty() {
                    for run in font_runs(&wide, self.plan.declared()) {
                        let Some(fam) =
                            run.class.and_then(|c| self.plan.chain_for(Some(c)).first())
                        else {
                            continue; // 默认链的段：base family 已在上面设过
                        };
                        let famw: Vec<u16> = fam.encode_utf16().chain(std::iter::once(0)).collect();
                        let range = DWRITE_TEXT_RANGE {
                            startPosition: run.start as u32,
                            length: run.len as u32,
                        };
                        let _ = layout.SetFontFamilyName(PCWSTR(famw.as_ptr()), range);
                    }
                }
                // 回退链：让每一段在自己的 base family 缺字时按用户声明的顺序找下一个。
                // 方案没有任何多项链时 `ensure_fallback` 返回 None，走系统默认回退。
                // 短路顺序承重：`ensure_fallback` 先判、`cast` 后做——没有方案时连一次
                // QueryInterface 都不该付（这是每次绘制都会走到的热路径）。
                if let Some(fb) = self.ensure_fallback() {
                    let _ = layout
                        .cast::<IDWriteTextLayout2>()
                        .and_then(|l2| l2.SetFontFallback(&fb));
                }
                // 行距钉成 UNIFORM：行高与基线都取基准字族自己的自然值，不随行内回退字体
                // 起伏（理由与数据见 `line_height_for`）。DirectWrite 的 UNIFORM 模式要求显式
                // 给基线，没有按比例自动分配的选项，所以基线也由探针实测而非写死比例。
                // 只影响高度：宽度、连字、彩色层都不经过这里；`\n` 硬换行仍按行数翻倍
                // （candidate_window 的多行候选依赖）。比基准字族高的字形（emoji、字根字体）
                // 按 overhang 画出行框，不裁切。
                let size_key = size.max(1.0).round() as u32;
                if uniform_spacing
                    && let Some((line, baseline)) =
                        self.line_height_for(&fmt, base, weight, size_key)
                {
                    let _ =
                        layout.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, baseline);
                }
                // 拆字字根：把私用区（BMP PUA + 补充私用区 A/B）的连续段切到字根字体集，
                // 级联回退渲染字根字符。段划分见 `super::pua_runs`——测量与绘制共用本函数，
                // 故字根段的字体在两条路径上必然一致（否则宽度按主字体缺字宽算，布局出错）。
                if let Some(cf) = &self.chaizi {
                    for (start, len) in super::pua_runs(&wide) {
                        let range = DWRITE_TEXT_RANGE {
                            startPosition: start as u32,
                            length: len as u32,
                        };
                        let _ = layout.SetFontCollection(&cf.collection, range);
                        let _ = layout.SetFontFamilyName(PCWSTR(cf.family.as_ptr()), range);
                    }
                }
                Ok(layout)
            }
        }

        /// 测量文本尺寸（用基准字号）。
        pub fn measure_text(&self, text: &str) -> TextMetrics {
            self.measure_text_sized(text, self.font_size)
        }

        /// 测量文本尺寸（指定字号，其余取默认；宽含尾随空白，高为行高）。
        pub fn measure_text_sized(&self, text: &str, size: f32) -> TextMetrics {
            self.measure(text, &TextStyle::new(size))
        }

        /// 测量文本尺寸。结果按 `measure_key` 缓存。
        pub fn measure(&self, text: &str, ts: &TextStyle) -> TextMetrics {
            if text.is_empty() {
                return TextMetrics {
                    width: 0.0,
                    height: ts.size * 1.2,
                };
            }
            let key = measure_key(text, ts);
            if let Some(m) = self.measure_cache.borrow().get(&key) {
                return m.clone();
            }
            // 排版失败走等宽近似回退，且**不入缓存**：失败多是暂时性的（资源紧张、
            // 字体集正在切换），一旦把回退值固化，这段文本就会一直按错误宽度布局
            // 直到下次整体清空——而清空只在换字体/换字根时发生，可能永远等不到。
            let Some(m) = self.measure_layout(text, ts) else {
                return TextMetrics {
                    width: text.chars().count() as f32 * ts.size * 0.6,
                    height: ts.size * 1.2,
                };
            };
            let mut c = self.measure_cache.borrow_mut();
            if c.len() >= MEASURE_CACHE_CAP {
                c.clear();
            }
            c.insert(key, m.clone());
            m
        }

        /// 走一次 DirectWrite 排版取度量。任一 COM 环节失败返回 `None`
        /// （由 [`TextRenderer::measure`] 决定回退值，并跳过缓存）。
        fn measure_layout(&self, text: &str, ts: &TextStyle) -> Option<TextMetrics> {
            let layout = self
                .create_layout(text, ts, f32::MAX / 2.0, f32::MAX / 2.0)
                .ok()?;
            unsafe {
                let mut m = DWRITE_TEXT_METRICS::default();
                layout.GetMetrics(&mut m).ok()?;
                let height = if m.height > 0.0 {
                    m.height
                } else {
                    ts.size * 1.2
                };
                Some(TextMetrics {
                    width: m.widthIncludingTrailingWhitespace,
                    height,
                })
            }
        }

        /// 不钉行距的度量（测试专用，不走缓存）：高度是 DirectWrite 的自然行高，即行内
        /// 实际参与渲染的字体里最大的那个——回退链测试靠它分辨「中」落到了哪款字体。
        #[cfg(test)]
        pub(crate) fn measure_natural(&self, text: &str, ts: &TextStyle) -> TextMetrics {
            let layout = self
                .create_layout_with(text, ts, f32::MAX / 2.0, f32::MAX / 2.0, false)
                .expect("create_layout");
            let mut m = DWRITE_TEXT_METRICS::default();
            unsafe { layout.GetMetrics(&mut m).expect("GetMetrics") };
            TextMetrics {
                width: m.widthIncludingTrailingWhitespace,
                height: m.height,
            }
        }

        /// 释放离屏表面（窗口隐藏够久时，设计 §7）；下一次绘制经 [`Self::ensure_surface`]
        /// 按当时尺寸重建。
        pub fn release_surface(&self) {
            *self.surface.borrow_mut() = None;
        }

        /// 确保位图渲染表面至少为给定尺寸（只增长不重建：翻页时窗口宽度抖动，
        /// 复用最大表面可避免每帧重建 COM 渲染目标）。DIB 实际可比窗口大，
        /// draw_text 用窗口尺寸裁剪、用 DIBSECTION 的真实 stride 索引，故安全。
        fn ensure_surface(&self, w: u32, h: u32) -> Result<(), String> {
            let (cur_w, cur_h) = self
                .surface
                .borrow()
                .as_ref()
                .map_or((0, 0), |s| (s.width, s.height));
            if cur_w >= w && cur_h >= h {
                return Ok(());
            }
            let nw = w.max(cur_w);
            let nh = h.max(cur_h);
            unsafe {
                let target = self
                    .gdi_interop
                    .CreateBitmapRenderTarget(None, nw, nh)
                    .map_err(|e| format!("CreateBitmapRenderTarget: {e}"))?;
                target
                    .SetPixelsPerDip(1.0)
                    .map_err(|e| format!("SetPixelsPerDip: {e}"))?;
                let renderer: IDWriteTextRenderer = GlyphRenderer {
                    target: target.clone(),
                    params: self.params.clone(),
                    factory2: self.factory2.clone(),
                }
                .into();
                *self.surface.borrow_mut() = Some(Surface {
                    target,
                    renderer,
                    width: nw,
                    height: nh,
                });
            }
            Ok(())
        }

        /// 渲染文本到 BGRA 缓冲区（用基准字号）。
        ///
        /// - `buf`: 目标 BGRA 缓冲区（已含背景，预乘 alpha）
        /// - `buf_width`/`buf_height`: 缓冲区尺寸
        /// - `x`/`y`: 文本左上角（像素坐标）
        /// - `color`: 文本颜色 [R, G, B, A]（`A` 为文字自身不透明度，见
        ///   [`TextRenderer::draw`] 步骤 3 的二次混合）
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

        /// 绘制文本（指定字号，其余取默认）。
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
            self.draw(
                buf,
                buf_width,
                buf_height,
                x,
                y,
                text,
                &TextStyle::new(size),
                color,
            )
        }

        /// 绘制文本。
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
            if text.is_empty() || buf_width == 0 || buf_height == 0 {
                return Ok(());
            }
            let w = buf_width as usize;
            let h = buf_height as usize;
            if buf.len() < w * h * 4 {
                return Err("buffer too small".into());
            }

            self.ensure_surface(buf_width, buf_height)?;
            let surface = self.surface.borrow();
            let surface = surface.as_ref().ok_or("no surface")?;

            unsafe {
                // 取内存 DC 中 DIB 的像素指针与行距。
                let memdc = surface.target.GetMemoryDC();
                let hbmp = GetCurrentObject(memdc, OBJ_BITMAP);
                let mut ds = DIBSECTION::default();
                let n = GetObjectW(
                    hbmp,
                    std::mem::size_of::<DIBSECTION>() as i32,
                    Some(&mut ds as *mut _ as *mut c_void),
                );
                if n == 0 || ds.dsBm.bmBits.is_null() {
                    return Err("GetObjectW(DIBSECTION) failed".into());
                }
                let stride = ds.dsBm.bmWidthBytes as usize; // 32bpp 顶端向下，bmBits 指向首（顶）行
                let bits = ds.dsBm.bmBits as *mut u8;
                let dib = std::slice::from_raw_parts_mut(bits, stride * h);

                // 颜色经 clientDrawingContext 透传给字形回调（整段一色：无 effect 表）。
                let dc = DrawCtx::solid(colorref(color));
                let layout = self.create_layout(text, ts, buf_width as f32, buf_height as f32)?;

                // 关键优化：用文本度量算出包围盒，后续两遍逐像素操作只在盒内进行
                // （原实现每次绘制都遍历整窗，单帧十余次 × 整窗 → paint 高达 ~100ms）。
                // ClearType/抗锯齿可能轻微外溢，留 2px 余量。
                let mut tm = DWRITE_TEXT_METRICS::default();
                let _ = layout.GetMetrics(&mut tm);
                const MARGIN: f32 = 2.0;
                let cx0 = (x + tm.left - MARGIN).floor().max(0.0) as usize;
                let cy0 = (y + tm.top - MARGIN).floor().max(0.0) as usize;
                let cx1 = (((x + tm.left + tm.widthIncludingTrailingWhitespace + MARGIN).ceil())
                    .max(0.0) as usize)
                    .min(w);
                let cy1 = (((y + tm.top + tm.height + MARGIN).ceil()).max(0.0) as usize).min(h);
                if cx0 >= cx1 || cy0 >= cy1 {
                    return Ok(());
                }

                // 1) 背景按不透明复制进 DIB（仅包围盒；盒外 DIB 残留不会被读取）。
                for row in cy0..cy1 {
                    let src = row * w * 4;
                    let dst = row * stride;
                    for col in cx0..cx1 {
                        let s = src + col * 4;
                        let d = dst + col * 4;
                        dib[d] = buf[s];
                        dib[d + 1] = buf[s + 1];
                        dib[d + 2] = buf[s + 2];
                        dib[d + 3] = 255;
                    }
                }

                // 2) 渲染文本（绝对坐标 x,y，不受 DIB 实际尺寸影响）。
                layout
                    .Draw(
                        Some(&dc as *const DrawCtx as *const c_void),
                        &surface.renderer,
                        x,
                        y,
                    )
                    .map_err(|e| format!("TextLayout::Draw: {e}"))?;

                // 3) 选择性预乘回写：RGB 变动的像素视为文字，按窗口原 alpha 预乘（仅包围盒）。
                //
                // 文字自身的 alpha（`color[3]`）在这一步才混进来，而非交给 DirectWrite：
                // `BitmapRenderTarget::DrawGlyphRun` 只接受不含 alpha 的 COLORREF，半透明
                // 文字色根本传不进去。DirectWrite 已把**字形覆盖率**（含抗锯齿/ClearType）
                // 算进 (nr,ng,nb)——那是"文字色完全不透明"时的合成结果；此处再按 fa 与原
                // 背景混一次，等效于把 fa 乘进有效覆盖率。
                //
                // fa=255 时 mix 退化为 n 本身，逐像素等同旧逻辑 → 不透明文字零回归。
                let fa = color[3] as u32;
                // 背景侧取 buf 的现有预乘值当直通用——与步骤 1 拷进 DIB 的口径一致，
                // 两处必须同源，否则半透明背景上的文字会与 DirectWrite 的混合基准错位。
                let mix = |n: u8, b: u8| ((n as u32 * fa + b as u32 * (255 - fa)) / 255) as u8;
                for row in cy0..cy1 {
                    let sbase = row * w * 4;
                    let dbase = row * stride;
                    for col in cx0..cx1 {
                        let s = sbase + col * 4;
                        let d = dbase + col * 4;
                        let nb = dib[d];
                        let ng = dib[d + 1];
                        let nr = dib[d + 2];
                        if nb == buf[s] && ng == buf[s + 1] && nr == buf[s + 2] {
                            continue; // 背景未变
                        }
                        let a = buf[s + 3] as u32;
                        // 先按 fa 混合（读原 buf 值），再按窗口 alpha 预乘写回——顺序承重：
                        // mix 的背景侧必须是尚未被本像素写覆盖的原值。
                        let fb = mix(nb, buf[s]);
                        let fg = mix(ng, buf[s + 1]);
                        let fr = mix(nr, buf[s + 2]);
                        buf[s] = (fb as u32 * a / 255) as u8;
                        buf[s + 1] = (fg as u32 * a / 255) as u8;
                        buf[s + 2] = (fr as u32 * a / 255) as u8;
                        // alpha 保持窗口原值
                    }
                }
            }
            Ok(())
        }

        /// effect 在 [`DrawCtx`] 反查表里的键：它的 `IUnknown` 指针。测试可让它恒错位，
        /// 模拟「运行时换了 effect 指针、反查全部失败」。
        fn effect_key(&self, e: &windows::core::IUnknown) -> usize {
            let p = e.as_raw() as usize;
            #[cfg(test)]
            if self.break_effect_lookup.get() {
                return p ^ 1;
            }
            p
        }

        /// 分段着色绘制：`runs` 覆盖的区间改用各自的颜色，其余用 `color`。`runs` 为空（或全被
        /// 判为非法区间）时就是 [`Self::draw`]——出厂路径一个字节都不变。
        ///
        /// # 按区间上色：drawing effect
        ///
        /// 每种颜色建一个 effect 对象、`SetDrawingEffect` 到它的区间；[`GlyphRenderer`] 的
        /// `DrawGlyphRun` 拿到 effect 后按**指针身份**在 [`DrawCtx`] 的表里反查颜色。effect 是
        /// 格式属性，不参与整形——带 effect 的 layout 与测量用的 layout 字形位置相同（像素测试
        /// `span_color_tests` 钉住）。不必自定义 COM 接口：effect 只要是个活着的 `IUnknown`，
        /// 这里借 `CreateTypography` 造一个最轻的对象，只活到本次绘制结束。
        ///
        /// # 按 alpha 分组、多遍绘制
        ///
        /// 文字 alpha 不交给 DirectWrite（`DrawGlyphRun` 只收不含 alpha 的 COLORREF），而是在
        /// 回写时按 `fa` 与底色再混一次（见 [`Self::draw`] 步骤 3）；那一步已分不出哪个像素属于
        /// 哪段。故按 alpha 分组，每组一遍 `Draw`（不属本组的字形在回调里跳过）。不透明色与
        /// 基色同组，常见情形仍是一遍。
        ///
        /// ⚠️ 多遍时**不能**每遍都「拷底 → 预乘回写缓冲」：回写的是按窗口 alpha 预乘后的值，
        /// 下一遍又把它当直通值拷进 DIB，半透明底（`_base` 气泡 `F0`）上已画的字会被二次预乘、
        /// 发暗。做法：包围盒内的底先拷进一块直通值草稿区，各遍都在草稿区上「画 → 按本组 `fa`
        /// 混合」累积，最后统一按窗口 alpha 预乘、回写缓冲**一次**。被任一遍改动过的像素才回写
        /// ——单遍时与 [`Self::draw`] 的判据、算术逐字节相同。
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
            let spans = super::utf16_runs(text, runs);
            if spans.is_empty() {
                return self.draw(buf, buf_width, buf_height, x, y, text, ts, color);
            }
            if buf_width == 0 || buf_height == 0 {
                return Ok(());
            }
            let w = buf_width as usize;
            let h = buf_height as usize;
            if buf.len() < w * h * 4 {
                return Err("buffer too small".into());
            }

            self.ensure_surface(buf_width, buf_height)?;
            let surface = self.surface.borrow();
            let surface = surface.as_ref().ok_or("no surface")?;

            unsafe {
                let memdc = surface.target.GetMemoryDC();
                let hbmp = GetCurrentObject(memdc, OBJ_BITMAP);
                let mut ds = DIBSECTION::default();
                let n = GetObjectW(
                    hbmp,
                    std::mem::size_of::<DIBSECTION>() as i32,
                    Some(&mut ds as *mut _ as *mut c_void),
                );
                if n == 0 || ds.dsBm.bmBits.is_null() {
                    return Err("GetObjectW(DIBSECTION) failed".into());
                }
                let stride = ds.dsBm.bmWidthBytes as usize;
                let dib = std::slice::from_raw_parts_mut(ds.dsBm.bmBits as *mut u8, stride * h);

                let layout = self.create_layout(text, ts, buf_width as f32, buf_height as f32)?;
                // 每种颜色一个 effect；同色区间共用，分组与反查都按颜色而非区间。
                let mut colors: Vec<[u8; 4]> = Vec::new();
                let mut effects: Vec<windows::core::IUnknown> = Vec::new();
                let total = text.encode_utf16().count();
                let mut covered = vec![false; total];
                for &(start, len, rgba) in &spans {
                    let i = match colors.iter().position(|c| *c == rgba) {
                        Some(i) => i,
                        None => {
                            let e = self
                                .factory
                                .CreateTypography()
                                .and_then(|t| t.cast::<windows::core::IUnknown>())
                                .map_err(|e| format!("CreateTypography: {e}"))?;
                            colors.push(rgba);
                            effects.push(e);
                            colors.len() - 1
                        }
                    };
                    let range = DWRITE_TEXT_RANGE {
                        startPosition: start,
                        length: len,
                    };
                    layout
                        .SetDrawingEffect(&effects[i], range)
                        .map_err(|e| format!("SetDrawingEffect: {e}"))?;
                    covered[start as usize..(start + len) as usize].fill(true);
                }
                // 没被任何区间覆盖的字形用基色；全覆盖时基色那组根本没有字形，不为它多画一遍。
                let base_used = covered.iter().any(|c| !c);
                let mut alphas: Vec<u8> = Vec::new();
                for a in base_used
                    .then_some(color[3])
                    .into_iter()
                    .chain(colors.iter().map(|c| c[3]))
                {
                    // alpha 0 的字画了也看不见：跳过，连「改动过」都不算，免得底色被预乘一次。
                    if a > 0 && !alphas.contains(&a) {
                        alphas.push(a);
                    }
                }

                let mut tm = DWRITE_TEXT_METRICS::default();
                let _ = layout.GetMetrics(&mut tm);
                const MARGIN: f32 = 2.0;
                let cx0 = (x + tm.left - MARGIN).floor().max(0.0) as usize;
                let cy0 = (y + tm.top - MARGIN).floor().max(0.0) as usize;
                let cx1 = (((x + tm.left + tm.widthIncludingTrailingWhitespace + MARGIN).ceil())
                    .max(0.0) as usize)
                    .min(w);
                let cy1 = (((y + tm.top + tm.height + MARGIN).ceil()).max(0.0) as usize).min(h);
                if cx0 >= cx1 || cy0 >= cy1 {
                    return Ok(());
                }
                let bw = cx1 - cx0;

                // 直通值草稿区（BGR）：初值取 buf 现值，与 [`Self::draw`] 步骤 1 同一口径。
                let mut scratch = vec![0u8; bw * (cy1 - cy0) * 3];
                let mut touched = vec![false; bw * (cy1 - cy0)];
                for row in cy0..cy1 {
                    for col in cx0..cx1 {
                        let s = (row * w + col) * 4;
                        let k = ((row - cy0) * bw + col - cx0) * 3;
                        scratch[k..k + 3].copy_from_slice(&buf[s..s + 3]);
                    }
                }

                for (pass, &a) in alphas.iter().enumerate() {
                    // 1) 草稿区按不透明拷进 DIB。
                    for row in cy0..cy1 {
                        for col in cx0..cx1 {
                            let d = row * stride + col * 4;
                            let k = ((row - cy0) * bw + col - cx0) * 3;
                            dib[d..d + 3].copy_from_slice(&scratch[k..k + 3]);
                            dib[d + 3] = 255;
                        }
                    }
                    // 2) 只画本组的字形。
                    let dc = DrawCtx {
                        base: colorref(color),
                        base_on: base_used && color[3] == a,
                        fallback_on: pass == 0,
                        effects: colors
                            .iter()
                            .zip(&effects)
                            .map(|(c, e)| (self.effect_key(e), colorref(*c), c[3] == a))
                            .collect(),
                    };
                    layout
                        .Draw(
                            Some(&dc as *const DrawCtx as *const c_void),
                            &surface.renderer,
                            x,
                            y,
                        )
                        .map_err(|e| format!("TextLayout::Draw: {e}"))?;
                    // 3) 按本组 fa 混进草稿区（仍是直通值），记下改动过的像素。
                    let fa = a as u32;
                    let mix = |n: u8, b: u8| ((n as u32 * fa + b as u32 * (255 - fa)) / 255) as u8;
                    for row in cy0..cy1 {
                        for col in cx0..cx1 {
                            let d = row * stride + col * 4;
                            let i = (row - cy0) * bw + col - cx0;
                            let k = i * 3;
                            if dib[d..d + 3] == scratch[k..k + 3] {
                                continue;
                            }
                            for c in 0..3 {
                                scratch[k + c] = mix(dib[d + c], scratch[k + c]);
                            }
                            touched[i] = true;
                        }
                    }
                }

                // 4) 统一按窗口 alpha 预乘、回写一次。
                for row in cy0..cy1 {
                    for col in cx0..cx1 {
                        let i = (row - cy0) * bw + col - cx0;
                        if !touched[i] {
                            continue;
                        }
                        let s = (row * w + col) * 4;
                        let a = buf[s + 3] as u32;
                        for c in 0..3 {
                            buf[s + c] = (scratch[i * 3 + c] as u32 * a / 255) as u8;
                        }
                    }
                }
            }
            Ok(())
        }
    }

    /// 入参色 `[R,G,B,A]` → GDI COLORREF（0x00BBGGRR，丢 alpha：alpha 由回写时混合处理）。
    fn colorref(c: [u8; 4]) -> u32 {
        (c[0] as u32) | ((c[1] as u32) << 8) | ((c[2] as u32) << 16)
    }

    /// 一次 `Draw` 的颜色表，经 `clientDrawingContext` 交给 [`GlyphRenderer`]。
    ///
    /// 颜色不存于渲染器对象内（它按表面尺寸缓存、跨绘制复用），而是每次绘制现传，避免可变状态。
    struct DrawCtx {
        /// 没有 drawing effect 的字形用的颜色（COLORREF）。
        base: u32,
        /// 本遍画不画基色字形（多遍绘制按 alpha 分组，见 [`TextRenderer::draw_runs`]）。
        base_on: bool,
        /// 反查不到的 effect 本遍画不画（按基色）。只在第一遍为真：区间全覆盖时基色那组
        /// 根本不存在，`base_on` 恒假，没有这一位的话反查失败的字形哪一遍都不画。
        fallback_on: bool,
        /// `(effect 的 IUnknown 指针, COLORREF, 本遍画不画)`，按指针身份反查。
        effects: Vec<(usize, u32, bool)>,
    }

    impl DrawCtx {
        /// 整段一色。
        fn solid(colorref: u32) -> Self {
            Self {
                base: colorref,
                base_on: true,
                fallback_on: true,
                effects: Vec::new(),
            }
        }

        /// 某个字形段该用的颜色；`None` = 本遍不画它。
        ///
        /// 表里查不到的 effect（理论上不会出现：effect 只有 `draw_runs` 设；真出现了多半是
        /// 运行时把 effect 包了一层、指针变了）按基色、在第一遍画，宁可颜色不对也不丢字。
        ///
        /// ⚠️ 第一遍是按**首个 alpha 组**混合的：基色的 alpha 与首组不同时（例如基色半透明、
        /// 区间全覆盖且首组不透明），兜底画出的字用的是首组的 alpha，而不是基色自己的。
        /// 这只影响「反查失败」这个本不该发生的兜底，不为它多画一遍。
        fn pick(&self, effect: Option<&windows::core::IUnknown>) -> Option<u32> {
            if let Some(e) = effect {
                let p = e.as_raw() as usize;
                return match self.effects.iter().find(|x| x.0 == p) {
                    Some(&(_, c, on)) => on.then_some(c),
                    None => self.fallback_on.then_some(self.base),
                };
            }
            self.base_on.then_some(self.base)
        }
    }

    /// DWRITE_COLOR_F（0..1 各通道）→ GDI COLORREF（0x00BBGGRR）。
    /// BitmapRenderTarget.DrawGlyphRun 只接受不含 alpha 的 COLORREF；彩色 emoji 层通常 a=1.0，可接受。
    fn color_f_to_colorref(c: DWRITE_COLOR_F) -> COLORREF {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        COLORREF((q(c.b) << 16) | (q(c.g) << 8) | q(c.r))
    }

    /// 自定义字形渲染器：优先把字形拆成彩色层逐层着色（emoji），否则以文字色单色绘制。
    /// 颜色不存于对象内，而是每次 Draw 经 clientDrawingContext 透传 [`DrawCtx`]，避免可变状态；
    /// 彩色 emoji 的前景哨兵层同样取该字形段的颜色。
    #[implement(IDWriteTextRenderer)]
    struct GlyphRenderer {
        target: IDWriteBitmapRenderTarget,
        params: IDWriteRenderingParams,
        /// 彩色字形拆层接口（Win8.1+）；None 时仅单色绘制。
        factory2: Option<IDWriteFactory2>,
    }

    #[allow(non_snake_case)]
    impl IDWritePixelSnapping_Impl for GlyphRenderer_Impl {
        fn IsPixelSnappingDisabled(&self, _ctx: *const c_void) -> windows::core::Result<BOOL> {
            Ok(FALSE)
        }

        fn GetCurrentTransform(
            &self,
            _ctx: *const c_void,
            transform: *mut DWRITE_MATRIX,
        ) -> windows::core::Result<()> {
            // 单位矩阵
            unsafe {
                if !transform.is_null() {
                    *transform = DWRITE_MATRIX {
                        m11: 1.0,
                        m12: 0.0,
                        m21: 0.0,
                        m22: 1.0,
                        dx: 0.0,
                        dy: 0.0,
                    };
                }
            }
            Ok(())
        }

        fn GetPixelsPerDip(&self, _ctx: *const c_void) -> windows::core::Result<f32> {
            Ok(1.0)
        }
    }

    #[allow(non_snake_case)]
    impl IDWriteTextRenderer_Impl for GlyphRenderer_Impl {
        fn DrawGlyphRun(
            &self,
            ctx: *const c_void,
            baseline_x: f32,
            baseline_y: f32,
            measuring_mode: DWRITE_MEASURING_MODE,
            glyph_run: *const DWRITE_GLYPH_RUN,
            desc: *const DWRITE_GLYPH_RUN_DESCRIPTION,
            effect: Option<&windows::core::IUnknown>,
        ) -> windows::core::Result<()> {
            let colorref = if ctx.is_null() {
                0u32
            } else {
                // 不属于本遍的字形段直接跳过（多遍按 alpha 分组，见 `TextRenderer::draw_runs`）。
                match unsafe { &*(ctx as *const DrawCtx) }.pick(effect) {
                    Some(c) => c,
                    None => return Ok(()),
                }
            };

            // 优先：把字形拆成彩色层（COLR/CPAL，如 emoji）逐层着色叠加。
            // 字体无彩色数据时 TranslateColorGlyphRun 返回 DWRITE_E_NOCOLOR，落到下方单色路径。
            if let Some(f2) = &self.factory2 {
                let desc_opt = if desc.is_null() { None } else { Some(desc) };
                let enumr = unsafe {
                    f2.TranslateColorGlyphRun(
                        baseline_x,
                        baseline_y,
                        glyph_run,
                        desc_opt,
                        measuring_mode,
                        None, // 无世界变换（位图已按物理像素 1:1）
                        0,    // 默认调色板
                    )
                };
                match enumr {
                    Ok(en) => {
                        unsafe {
                            // 逐层绘制；枚举出错则中止彩色路径（已绘层保留）。
                            while let Ok(more) = en.MoveNext() {
                                if !more.as_bool() {
                                    break;
                                }
                                let Ok(run_ptr) = en.GetCurrentRun() else {
                                    break;
                                };
                                if run_ptr.is_null() {
                                    break;
                                }
                                let run = &*run_ptr;
                                // paletteIndex == 0xFFFF 为规范哨兵：该层用文字前景色。
                                let color = if run.paletteIndex == 0xFFFF {
                                    COLORREF(colorref)
                                } else {
                                    color_f_to_colorref(run.runColor)
                                };
                                let _ = self.target.DrawGlyphRun(
                                    run.baselineOriginX,
                                    run.baselineOriginY,
                                    measuring_mode,
                                    &run.glyphRun,
                                    &self.params,
                                    color,
                                    None,
                                );
                            }
                        }
                        return Ok(());
                    }
                    Err(e) if e.code() == DWRITE_E_NOCOLOR => {} // 无彩色数据：走单色
                    Err(_) => {}                                 // 其它失败：保守走单色
                }
            }

            // 单色：用文字颜色直接在已拷入真实背景的位图上抗锯齿混合。
            unsafe {
                self.target.DrawGlyphRun(
                    baseline_x,
                    baseline_y,
                    measuring_mode,
                    glyph_run,
                    &self.params,
                    COLORREF(colorref),
                    None,
                )?;
            }
            Ok(())
        }

        fn DrawUnderline(
            &self,
            _ctx: *const c_void,
            _x: f32,
            _y: f32,
            _underline: *const DWRITE_UNDERLINE,
            _effect: Option<&windows::core::IUnknown>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn DrawStrikethrough(
            &self,
            _ctx: *const c_void,
            _x: f32,
            _y: f32,
            _strikethrough: *const DWRITE_STRIKETHROUGH,
            _effect: Option<&windows::core::IUnknown>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn DrawInlineObject(
            &self,
            _ctx: *const c_void,
            _x: f32,
            _y: f32,
            _obj: Option<&IDWriteInlineObject>,
            _sideways: BOOL,
            _rtl: BOOL,
            _effect: Option<&windows::core::IUnknown>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
    }
    /// 一段字形实际用到的字体（[`TextRenderer::probe_glyph_runs`] 的结果）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct GlyphRunFont {
        /// 这段的字形数。
        pub glyphs: u32,
        /// face 所属 family 名（本地化名，zh-cn 优先）。
        pub family: String,
        /// face 名（Regular / Bold / SemiBold …，英文名优先）。
        pub face: String,
        /// face 自报的字重（不含模拟）。
        pub weight: i32,
        /// DirectWrite 是否在该 face 上做了粗体模拟。
        pub bold_sim: bool,
        /// 是否做了斜体模拟。
        pub oblique_sim: bool,
    }

    /// 只记录不绘制的字形渲染器：把每个 glyph run 的 font face 记下来。
    #[implement(IDWriteTextRenderer)]
    #[derive(Default)]
    struct RunRecorder {
        runs: std::rc::Rc<RefCell<Vec<GlyphRunFont>>>,
    }

    #[allow(non_snake_case)]
    impl IDWritePixelSnapping_Impl for RunRecorder_Impl {
        fn IsPixelSnappingDisabled(&self, _ctx: *const c_void) -> windows::core::Result<BOOL> {
            Ok(FALSE)
        }

        fn GetCurrentTransform(
            &self,
            _ctx: *const c_void,
            transform: *mut DWRITE_MATRIX,
        ) -> windows::core::Result<()> {
            unsafe {
                if !transform.is_null() {
                    *transform = DWRITE_MATRIX {
                        m11: 1.0,
                        m22: 1.0,
                        ..Default::default()
                    };
                }
            }
            Ok(())
        }

        fn GetPixelsPerDip(&self, _ctx: *const c_void) -> windows::core::Result<f32> {
            Ok(1.0)
        }
    }

    #[allow(non_snake_case)]
    impl IDWriteTextRenderer_Impl for RunRecorder_Impl {
        fn DrawGlyphRun(
            &self,
            _ctx: *const c_void,
            _x: f32,
            _y: f32,
            _mode: DWRITE_MEASURING_MODE,
            glyph_run: *const DWRITE_GLYPH_RUN,
            _desc: *const DWRITE_GLYPH_RUN_DESCRIPTION,
            _effect: Option<&windows::core::IUnknown>,
        ) -> windows::core::Result<()> {
            use wind_config::font_name::pick_localized_name;
            let Some(run) = (unsafe { glyph_run.as_ref() }) else {
                return Ok(());
            };
            let Some(face) = run.fontFace.as_ref() else {
                return Ok(());
            };
            let sims = unsafe { face.GetSimulations() };
            let face3: IDWriteFontFace3 = face.cast()?;
            let (family, face_name, weight) = unsafe {
                let family = face3
                    .GetFamilyNames()
                    .ok()
                    .and_then(|n| pick_localized_name(&localized(&n)))
                    .unwrap_or_default();
                let names = face3
                    .GetFaceNames()
                    .map(|n| localized(&n))
                    .unwrap_or_default();
                let face_name = names
                    .iter()
                    .find(|(l, _)| l.eq_ignore_ascii_case("en-us"))
                    .or(names.first())
                    .map(|(_, n)| n.clone())
                    .unwrap_or_default();
                (family, face_name, face3.GetWeight().0)
            };
            self.runs.borrow_mut().push(GlyphRunFont {
                glyphs: run.glyphCount,
                family,
                face: face_name,
                weight,
                bold_sim: (sims.0 & DWRITE_FONT_SIMULATIONS_BOLD.0) != 0,
                oblique_sim: (sims.0 & DWRITE_FONT_SIMULATIONS_OBLIQUE.0) != 0,
            });
            Ok(())
        }

        fn DrawUnderline(
            &self,
            _ctx: *const c_void,
            _x: f32,
            _y: f32,
            _underline: *const DWRITE_UNDERLINE,
            _effect: Option<&windows::core::IUnknown>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn DrawStrikethrough(
            &self,
            _ctx: *const c_void,
            _x: f32,
            _y: f32,
            _strikethrough: *const DWRITE_STRIKETHROUGH,
            _effect: Option<&windows::core::IUnknown>,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn DrawInlineObject(
            &self,
            _ctx: *const c_void,
            _x: f32,
            _y: f32,
            _obj: Option<&IDWriteInlineObject>,
            _sideways: BOOL,
            _rtl: BOOL,
            _effect: Option<&windows::core::IUnknown>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
    }

    /// [`TextRenderer::resolve_family_in`] 的两条语义。用 Windows 自带字体建集，
    /// 不依赖任何外部资源，故是常规用例而非 `#[ignore]` 探针。
    ///
    /// # ⚠️ 判据必须落在**返回值**上，不能落在测量宽度上
    ///
    /// 系统里往往也装着同一款字体：解析一旦失效，系统回退会挑中它、量出与解析成功时
    /// 相同的宽度 ⇒ 按宽度断言必然假绿。真机上正是这条掩盖了 Toli 方案的家族名错误——
    /// 装了 Menk 字体的机器画面「看着是好的」。
    ///
    /// # ⚠️ CI 跑不到它
    ///
    /// 它住在 `#[cfg(windows)] mod imp` 里，而 CI 的 test job 跑 Linux、clippy job 只做
    /// 交叉编译不运行用例 ⇒ **本用例只在本地 Windows `cargo test` 时执行**。改这块前
    /// 请在 Windows 上跑一遍，别指望 CI 拦下回归。
    #[cfg(test)]
    mod family_resolve_tests {
        use super::*;

        /// 从单个字体文件建一个只含它的自定义字体集（与 `set_chaizi_font` 同一套调用）。
        fn collection_of(path: &str) -> Option<IDWriteFontCollection1> {
            unsafe {
                let factory: IDWriteFactory =
                    DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
                let f3: IDWriteFactory3 = factory.cast().ok()?;
                let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
                let file = f3.CreateFontFileReference(PCWSTR(w.as_ptr()), None).ok()?;
                let builder: IDWriteFontSetBuilder1 =
                    f3.CreateFontSetBuilder().ok()?.cast().ok()?;
                builder.AddFontFile(&file).ok()?;
                let set = builder.CreateFontSet().ok()?;
                f3.CreateFontCollectionFromFontSet(&set).ok()
            }
        }

        #[test]
        fn wrong_declared_family_resolves_to_what_the_font_reports() {
            const PATH: &str = r"C:\Windows\Fonts\consola.ttf";
            if !std::path::Path::new(PATH).exists() {
                return; // 精简版 Windows 可能没有这个文件；缺文件不该判失败
            }
            let Some(col) = collection_of(PATH) else {
                return;
            };
            assert_eq!(
                TextRenderer::resolve_family_in(&col, "这个家族名并不存在"),
                "Consolas",
                "写错的家族名必须解析成字体自报的家族名，否则 SetFontFamilyName 会静默不生效"
            );
            assert_eq!(
                TextRenderer::resolve_family_in(&col, "Consolas"),
                "Consolas",
                "写对的家族名应原样返回"
            );
        }
    }

    /// wine 与真 Windows 的 DirectWrite 都得把 drawing effect 原样传进 `DrawGlyphRun`，
    /// 且按 effect 边界切开字形段——[`TextRenderer::draw_runs`] 整个建立在这一点上。
    ///
    /// 断言落在**指针身份**上：反查表按 `as_raw()` 比对，若运行时把 effect 包了一层或
    /// QueryInterface 出另一个指针，颜色会整段退回基色而不报错。本仓 2026-09-27 在
    /// wine 9.0 上实测通过（effect 原样传入，两段各 2 个字形）。
    #[cfg(test)]
    mod effect_passthrough_tests {
        use super::*;

        /// 只记 `(字形数, effect 指针)` 的渲染器。
        #[implement(IDWriteTextRenderer)]
        #[derive(Default)]
        struct EffectRecorder {
            seen: std::rc::Rc<RefCell<Vec<(u32, usize)>>>,
        }

        #[allow(non_snake_case)]
        impl IDWritePixelSnapping_Impl for EffectRecorder_Impl {
            fn IsPixelSnappingDisabled(&self, _ctx: *const c_void) -> windows::core::Result<BOOL> {
                Ok(FALSE)
            }
            fn GetCurrentTransform(
                &self,
                _ctx: *const c_void,
                transform: *mut DWRITE_MATRIX,
            ) -> windows::core::Result<()> {
                unsafe {
                    if !transform.is_null() {
                        *transform = DWRITE_MATRIX {
                            m11: 1.0,
                            m22: 1.0,
                            ..Default::default()
                        };
                    }
                }
                Ok(())
            }
            fn GetPixelsPerDip(&self, _ctx: *const c_void) -> windows::core::Result<f32> {
                Ok(1.0)
            }
        }

        #[allow(non_snake_case)]
        impl IDWriteTextRenderer_Impl for EffectRecorder_Impl {
            fn DrawGlyphRun(
                &self,
                _ctx: *const c_void,
                _x: f32,
                _y: f32,
                _mode: DWRITE_MEASURING_MODE,
                glyph_run: *const DWRITE_GLYPH_RUN,
                _desc: *const DWRITE_GLYPH_RUN_DESCRIPTION,
                effect: Option<&windows::core::IUnknown>,
            ) -> windows::core::Result<()> {
                let n = unsafe { glyph_run.as_ref() }.map_or(0, |r| r.glyphCount);
                let p = effect.map_or(0, |e| e.as_raw() as usize);
                self.seen.borrow_mut().push((n, p));
                Ok(())
            }
            fn DrawUnderline(
                &self,
                _ctx: *const c_void,
                _x: f32,
                _y: f32,
                _u: *const DWRITE_UNDERLINE,
                _effect: Option<&windows::core::IUnknown>,
            ) -> windows::core::Result<()> {
                Ok(())
            }
            fn DrawStrikethrough(
                &self,
                _ctx: *const c_void,
                _x: f32,
                _y: f32,
                _s: *const DWRITE_STRIKETHROUGH,
                _effect: Option<&windows::core::IUnknown>,
            ) -> windows::core::Result<()> {
                Ok(())
            }
            fn DrawInlineObject(
                &self,
                _ctx: *const c_void,
                _x: f32,
                _y: f32,
                _obj: Option<&IDWriteInlineObject>,
                _sideways: BOOL,
                _rtl: BOOL,
                _effect: Option<&windows::core::IUnknown>,
            ) -> windows::core::Result<()> {
                Ok(())
            }
        }

        #[test]
        fn effect_reaches_draw_glyph_run_by_identity() {
            let r = TextRenderer::new("Microsoft YaHei UI", 16.0).expect("建 TextRenderer");
            let layout = r
                .create_layout("abcd", &TextStyle::new(16.0), 500.0, 100.0)
                .expect("create_layout");
            let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
            let (p1, p2) = unsafe {
                let mk = || -> windows::core::IUnknown {
                    r.factory.CreateTypography().unwrap().cast().unwrap()
                };
                let (e1, e2) = (mk(), mk());
                let range = |s: u32| DWRITE_TEXT_RANGE {
                    startPosition: s,
                    length: 2,
                };
                layout.SetDrawingEffect(&e1, range(0)).unwrap();
                layout.SetDrawingEffect(&e2, range(2)).unwrap();
                let rec: IDWriteTextRenderer = EffectRecorder { seen: seen.clone() }.into();
                layout.Draw(None, &rec, 0.0, 0.0).unwrap();
                (e1.as_raw() as usize, e2.as_raw() as usize)
            };
            assert_eq!(
                *seen.borrow(),
                vec![(2, p1), (2, p2)],
                "effect 应原样（同一指针）传进 DrawGlyphRun，且在 effect 边界切开字形段"
            );
        }
    }
} // mod imp (windows)

// macOS：真字形渲染走 CoreText（text/coretext.rs），re-export 为本模块的 TextRenderer。
#[cfg(target_os = "macos")]
pub use crate::text::coretext::TextRenderer;

// Linux 外部宿主形态（linux-host）：真字形渲染走 text/linux/。
#[cfg(all(target_os = "linux", feature = "linux-host"))]
pub use crate::text::linux::TextRenderer;

// 其余平台（含不开 linux-host 的 Linux）：保留 mock 桩（仅供编译/测试，无真实字形）。
// `mock_text` 由 build.rs 给出，恰为上面三条 cfg 的补集。
#[cfg(mock_text)]
pub use imp::TextRenderer;

/// mock 文本后端：测量用等宽近似（字符数 × 字号 × 0.6），绘制为空操作。
/// 让候选窗/工具栏/菜单等布局逻辑能在 Linux 上编译与跑测试。
#[cfg(mock_text)]
mod imp {
    use super::{ColorRun, TextMetrics, TextStyle};
    use crate::text::script::FontPlan;

    pub struct TextRenderer {
        font_size: f32,
        /// 只为让 `font_plan()` 有东西可还——mock 的等宽近似不看字体。
        plan: FontPlan,
        /// 被问过存在性的字族名，按提问顺序。见 [`Self::asked_families`]。
        asked_families: std::cell::RefCell<Vec<String>>,
        /// 测试注入的「系统字体集」：`Some` 时 [`Self::family_exists`] 按它作答，
        /// 让字体名解析的接线能在 Linux 上测到。见 [`Self::set_mock_families`]。
        mock_families: Option<Vec<String>>,
        /// 最近一次 [`Self::set_font_family`] 设进来的字族（接线测试读回）。
        family: String,
        /// 最近一次 [`Self::set_default_weight`] 设进来的字重（接线测试读回）。
        default_weight: i32,
        /// 绘制调用记录（每次 draw 一行），供渲染 golden 对拍。见 [`Self::take_draw_log`]。
        ///
        /// mock 不出像素，「画了什么、画在哪、用什么色」就只剩这份记录能对拍；只在测试构建
        /// 存在，生产路径（Linux 本就没有真实渲染）不付任何代价。
        #[cfg(test)]
        draw_log: std::cell::RefCell<Vec<String>>,
    }

    impl TextRenderer {
        pub fn new(_font_family: &str, font_size: f32) -> Result<Self, String> {
            Ok(Self {
                font_size,
                plan: FontPlan::default(),
                asked_families: std::cell::RefCell::new(Vec::new()),
                mock_families: None,
                family: String::new(),
                default_weight: 0,
                #[cfg(test)]
                draw_log: std::cell::RefCell::new(Vec::new()),
            })
        }

        /// 取走至今的绘制调用记录（取后清空）。
        #[cfg(test)]
        pub fn take_draw_log(&self) -> Vec<String> {
            std::mem::take(&mut *self.draw_log.borrow_mut())
        }

        /// 测试用：声明 mock 的「系统字体集」。之后 `family_exists` 按它回答 `Some(..)`。
        pub fn set_mock_families(&mut self, families: &[&str]) {
            self.mock_families = Some(families.iter().map(|s| s.to_string()).collect());
        }

        /// 最近一次设进来的全局字族。
        pub fn font_family(&self) -> &str {
            &self.family
        }

        /// 最近一次设进来的全局默认字重。
        pub fn default_weight(&self) -> i32 {
            self.default_weight
        }

        pub fn set_default_weight(&mut self, weight: i32) {
            self.default_weight = weight;
        }

        /// mock：没有 face 表可查。
        pub fn find_face(&self, _name: &str) -> Option<(String, i32)> {
            None
        }

        pub fn base_size(&self) -> f32 {
            self.font_size
        }

        pub fn set_base_size(&mut self, size: f32) {
            self.font_size = size;
        }

        pub fn set_font_family(&mut self, font_family: &str) {
            self.family = font_family.to_string();
        }

        /// mock：等宽近似不看字体，只存下来供 `font_plan()` 读回（接线类测试要断言它）。
        pub fn set_font_plan(&mut self, plan: FontPlan) {
            self.plan = plan;
        }

        pub fn font_plan(&self) -> &FontPlan {
            &self.plan
        }

        /// mock：没有系统字体集可问，一律「不知道」——调用方据此不发 warn。
        ///
        /// 但**问过什么要记下来**：mock 答不了「系统里有没有」，而「有没有真的去问」正是
        /// 接线测试唯一能钉住的那一半。字体缺失告警此前就是因为无人验证，在 CoreText 上
        /// 空转了很久没被发现（那边同样恒返回 `None`）。见 [`Self::asked_families`]。
        pub fn family_exists(&self, family: &str) -> Option<bool> {
            self.asked_families.borrow_mut().push(family.to_string());
            self.mock_families.as_ref().map(|fs| {
                fs.iter()
                    .any(|f| wind_config::font_name::font_name_eq(f, family))
            })
        }

        /// 至今被问过存在性的字族名，按提问顺序。仅 mock 后端有，供接线测试断言
        /// 「设字体 / 换主题时确实逐个查了」。
        pub fn asked_families(&self) -> Vec<String> {
            self.asked_families.borrow().clone()
        }

        pub fn set_chaizi_font(&mut self, _path: &str, _family: &str) -> Result<(), String> {
            Ok(())
        }

        pub fn measure_text(&self, text: &str) -> TextMetrics {
            self.measure_text_sized(text, self.font_size)
        }

        pub fn measure_text_sized(&self, text: &str, size: f32) -> TextMetrics {
            if text.is_empty() {
                return TextMetrics {
                    width: 0.0,
                    height: size * 1.2,
                };
            }
            TextMetrics {
                width: text.chars().count() as f32 * size * 0.6,
                height: size * 1.2,
            }
        }

        /// mock：字重/字体族不影响等宽近似测量，只取字号。
        pub fn measure(&self, text: &str, ts: &TextStyle) -> TextMetrics {
            self.measure_text_sized(text, ts.size)
        }

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
            self.draw(
                buf,
                buf_width,
                buf_height,
                x,
                y,
                text,
                &TextStyle::new(size),
                color,
            )
        }

        /// mock：不出像素（样式忽略）；测试构建下记一行绘制调用。
        #[allow(clippy::too_many_arguments)]
        #[cfg_attr(not(test), allow(unused_variables))]
        pub fn draw(
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
            #[cfg(test)]
            self.draw_log.borrow_mut().push(format!(
                "draw text={text:?} at=({x:?},{y:?}) size={:?} weight={} family={:?} \
                 color={color:?}",
                ts.size, ts.weight, ts.family,
            ));
            Ok(())
        }

        /// mock：同 [`Self::draw`] 不出像素。与真实后端同一口径先过 [`super::utf16_runs`]：
        /// 没有合法区间时就是 `draw`（记录也与之逐字相同），否则记一行 `draw_runs`——golden
        /// 据此断言出厂路径「没有任何调用走 runs」。
        #[allow(clippy::too_many_arguments)]
        #[cfg_attr(not(test), allow(unused_variables))]
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
            if super::utf16_runs(text, runs).is_empty() {
                return self.draw(buf, buf_width, buf_height, x, y, text, ts, color);
            }
            #[cfg(test)]
            self.draw_log.borrow_mut().push(format!(
                "draw_runs text={text:?} at=({x:?},{y:?}) size={:?} weight={} family={:?} \
                 color={color:?} runs={runs:?}",
                ts.size, ts.weight, ts.family,
            ));
            Ok(())
        }
    }
}

// 换行语义：关自动换行、留硬换行。两条缺一不可——只验前者会让多行候选静默退化成单行，
// 只验后者则放任溢出继续。需要真实 DirectWrite，gate 到 Windows。
#[cfg(all(test, windows))]
mod chaizi_font_clear_tests {
    use super::TextRenderer;

    const FONT: &str = r"C:\Windows\Fonts\consola.ttf";

    fn tr() -> TextRenderer {
        TextRenderer::new("Microsoft YaHei UI", 16.0).expect("建 TextRenderer")
    }

    /// 空路径 = 撤掉。撤不掉的话，五笔的黑体字根会接管蒙古文方案的全部私用区文字。
    #[test]
    fn empty_path_clears_the_chaizi_font() {
        if !std::path::Path::new(FONT).exists() {
            return; // 精简版 Windows 可能没有这个文件；缺文件不该判失败
        }
        let mut r = tr();
        r.set_chaizi_font(FONT, "").expect("加载系统字体");
        assert!(r.has_chaizi_font());
        r.set_chaizi_font("", "").expect("空路径不是错误");
        assert!(!r.has_chaizi_font());
        assert_eq!(r.measure_cache_len(), 0);
    }

    /// 新字体加载失败时不许沿用旧的：协调器不会重发，旧字体会一直留在新方案上。
    #[test]
    fn a_failed_load_does_not_keep_the_previous_font() {
        if !std::path::Path::new(FONT).exists() {
            return;
        }
        let mut r = tr();
        r.set_chaizi_font(FONT, "").expect("加载系统字体");
        assert!(r.set_chaizi_font(r"C:\不存在\no.ttf", "").is_err());
        assert!(!r.has_chaizi_font());
    }
}

#[cfg(all(test, windows))]
mod wrapping_tests {
    use super::{TextRenderer, TextStyle};

    fn tr() -> TextRenderer {
        TextRenderer::new("Microsoft YaHei UI", 16.0).expect("建 TextRenderer")
    }

    /// 自动换行必须关闭：宽过缓冲的文本只画一行（超出部分裁掉），不得折行。
    ///
    /// 折行的后果不是「看不全」而是「画到别处」——多出来的行落在节点框外，盖住相邻候选。
    /// 竖排 `vertical_max_width`（用户/主题显式配置正值时）或渲染层恒生效的屏幕安全钳制
    /// 都会钳窗口宽度，触发这条路径（出厂默认值为 0=不限，此处直接构造窄缓冲区复现）。
    #[test]
    fn long_text_clips_instead_of_wrapping() {
        let r = tr();
        let ts = TextStyle::new(16.0);
        let line_h = r.measure("中", &ts).height;

        const BW: u32 = 60; // 远窄于文本宽度
        const BH: u32 = 120;
        let mut buf = vec![255u8; (BW * BH * 4) as usize];
        r.draw(
            &mut buf,
            BW,
            BH,
            0.0,
            0.0,
            "这是一个很长的候选词条",
            &ts,
            [0, 0, 0, 255],
        )
        .expect("draw");

        let bottom = (0..BH as i32)
            .rfind(|&y| (0..BW as i32).any(|x| buf[((y * BW as i32 + x) * 4 + 2) as usize] < 128))
            .unwrap_or(-1);
        assert!(
            bottom >= 0,
            "字形应当被画出来，否则本用例测的是「什么都没画」"
        );
        assert!(
            (bottom as f32) <= line_h,
            "宽过缓冲的文本应裁切在单行内（底边 {bottom} ≤ 行高 {line_h:.0}），\
             实测溢出说明自动换行又被打开了"
        );
    }

    /// `\n` 硬换行必须保留——candidate_window 依赖它做多行候选。
    /// 这是上一条修复的边界：NO_WRAP 只该关自动换行，一起关掉硬换行就是过度修复。
    #[test]
    fn hard_newline_still_breaks_lines() {
        let r = tr();
        let ts = TextStyle::new(16.0);
        let one = r.measure("中文", &ts);
        let two = r.measure("中文\n第二行", &ts);
        assert!(
            two.height > one.height * 1.5,
            "含 \\n 的文本应约为两倍行高（实得 {:.1} vs 单行 {:.1}）",
            two.height,
            one.height
        );
    }
}

// 文字色 alpha 的**像素级**验证：混合公式改的是逐像素算术，只靠逻辑推导不算验过。
// 需要真实 DirectWrite 出字形，故 gate 到 Windows。
#[cfg(all(test, windows))]
mod alpha_text_tests {
    use super::{TextRenderer, TextStyle};

    const W: u32 = 48;
    const H: u32 = 48;

    /// 不透明白底的 BGRA 预乘缓冲（A=255 时预乘即直通）。
    fn white_buf() -> Vec<u8> {
        vec![255u8; (W * H * 4) as usize]
    }

    /// 缓冲中最暗的 R 通道值。取最暗而非固定坐标——字形的确切落点随字体/hinting 变，
    /// 但"块体最暗处"这个判据与位置无关。
    fn darkest_r(buf: &[u8]) -> u8 {
        buf.as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[2])
            .min()
            .unwrap_or(255)
    }

    /// 在白底画一个全块字符（█ U+2588，覆盖率≈1），返回缓冲。
    fn draw_block(alpha: u8) -> Vec<u8> {
        let r = TextRenderer::new("微软雅黑", 32.0).expect("建 TextRenderer");
        let mut buf = white_buf();
        r.draw(
            &mut buf,
            W,
            H,
            0.0,
            0.0,
            "\u{2588}",
            &TextStyle::new(32.0),
            [0, 0, 0, alpha],
        )
        .expect("draw");
        buf
    }

    /// alpha=255：全块应压到近黑——同时确认字形真的画出来了（否则下一条测了个寂寞）。
    #[test]
    fn opaque_text_is_near_black() {
        let d = darkest_r(&draw_block(255));
        assert!(d < 64, "不透明黑块中心应近黑，实得 {d}");
    }

    /// alpha=128：同一个全块应落在中灰，而非近黑。
    ///
    /// 这是修复前后的分水岭——旧实现丢弃 `color[3]`，此处会与上一条同样得到近黑。
    /// 区间放宽到 96..=176 以容纳字体覆盖率与 ClearType 的差异；判据是"明显不是黑"。
    #[test]
    fn half_alpha_text_blends_to_midtone() {
        let d = darkest_r(&draw_block(128));
        assert!(
            (96..=176).contains(&d),
            "50% alpha 黑块应混成中灰（96..=176），实得 {d}——落在近黑说明 alpha 又被丢了"
        );
    }

    /// 单调性：alpha 越低，字越淡。比固定区间更稳，不受字体覆盖率影响。
    #[test]
    fn lower_alpha_yields_lighter_text() {
        let opaque = darkest_r(&draw_block(255));
        let half = darkest_r(&draw_block(128));
        let faint = darkest_r(&draw_block(48));
        assert!(
            opaque < half && half < faint,
            "alpha 越低字应越淡，实得 255→{opaque} 128→{half} 48→{faint}"
        );
    }
}

// 分段着色的**像素级**验证（设计 text-span-colors.md §13.2 P1）：需要真实 DirectWrite 出字形，
// gate 到 Windows。wine 9.0 的 DirectWrite 会把 drawing effect 传进回调（见
// `imp::effect_passthrough_tests`），故本组在 Linux 上可经 `cargo xwin test` + wine 跑
// （wine 下须 `--test-threads=1`：多线程并发建 COM 工厂会让 wine 进程异常退出）。
#[cfg(all(test, windows))]
mod span_color_tests {
    use super::{ColorRun, TextRenderer, TextStyle};

    const W: u32 = 200;
    const H: u32 = 60;
    const SIZE: f32 = 32.0;
    const RED: [u8; 4] = [220, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 220, 255];

    fn tr() -> TextRenderer {
        TextRenderer::new("Microsoft YaHei UI", SIZE).expect("建 TextRenderer")
    }

    fn ts() -> TextStyle<'static> {
        TextStyle::new(SIZE)
    }

    /// 整块填同一个 BGRA 预乘像素。
    fn canvas(px: [u8; 4]) -> Vec<u8> {
        px.repeat((W * H) as usize)
    }

    fn run(start: u32, end: u32, rgba: [u8; 4]) -> ColorRun {
        ColorRun { start, end, rgba }
    }

    fn draw_runs(r: &TextRenderer, buf: &mut [u8], x: f32, text: &str, runs: &[ColorRun]) {
        r.draw_runs(buf, W, H, x, 4.0, text, &ts(), [0, 0, 0, 255], runs)
            .expect("draw_runs");
    }

    /// 两块缓冲里不同的像素下标。
    fn diff_px(a: &[u8], b: &[u8]) -> Vec<usize> {
        (0..(W * H) as usize)
            .filter(|&i| a[i * 4..i * 4 + 4] != b[i * 4..i * 4 + 4])
            .collect()
    }

    /// ① 非空：画了东西。后面几条「相等」断言在什么都没画时会平凡成立，先把这条钉住。
    #[test]
    fn draw_runs_changes_pixels() {
        let r = tr();
        let bg = canvas([255, 255, 255, 255]);
        let mut buf = bg.clone();
        draw_runs(
            &r,
            &mut buf,
            4.0,
            "abcd",
            &[run(0, 2, RED), run(2, 4, BLUE)],
        );
        assert!(!diff_px(&bg, &buf).is_empty(), "draw_runs 应改动像素");
    }

    /// ② 多个 effect 全取基色 ⇒ 与不带 effect 的 `draw` 逐字节相同，不透明底与半透明底都比。
    ///
    /// 区间边界落在空格上：effect 会把一个字形段切成几段分别光栅化，相邻两字的抗锯齿边若
    /// 落进同一像素，「一段一次混合」与「两段先后混合」在那一个像素上可以差 1——那是
    /// DirectWrite 自身的分段行为，不是本实现要守的东西。空格隔开后字形墨迹互不相邻。
    #[test]
    fn base_color_effects_equal_plain_draw() {
        let r = tr();
        let text = "ab cd ef";
        let black = [0, 0, 0, 255];
        for bg_px in [[255, 255, 255, 255], [90, 90, 90, 200]] {
            let mut plain = canvas(bg_px);
            r.draw(&mut plain, W, H, 4.0, 4.0, text, &ts(), black)
                .expect("draw");
            let mut runs = canvas(bg_px);
            draw_runs(
                &r,
                &mut runs,
                4.0,
                text,
                &[run(0, 2, black), run(3, 5, black), run(6, 8, black)],
            );
            assert!(!diff_px(&canvas(bg_px), &plain).is_empty(), "应画出字形");
            assert_eq!(
                diff_px(&plain, &runs),
                Vec::<usize>::new(),
                "底色 {bg_px:?}：全取基色的 effect 必须与无 effect 逐字节相同"
            );
        }
    }

    /// ③ 色相：红 `ab` + 蓝 `cd`，各自字形的 x 区间内，改动像素以本段颜色为主。
    ///
    /// 这条挡的是「effect 设上了但颜色反查错位 / 全退回基色」——那种情况下两半同色，
    /// ①②都照样绿。
    #[test]
    fn each_run_is_drawn_in_its_own_hue() {
        let r = tr();
        let x0 = 4.0;
        let split = x0 + r.measure("ab", &ts()).width;
        let bg = canvas([255, 255, 255, 255]);
        let mut buf = bg.clone();
        draw_runs(&r, &mut buf, x0, "abcd", &[run(0, 2, RED), run(2, 4, BLUE)]);
        // (偏红, 偏蓝) 像素数，按左右半分开。BGRA：[0]=B、[2]=R。
        let (mut left, mut right) = ((0, 0), (0, 0));
        for i in diff_px(&bg, &buf) {
            let (b, red) = (buf[i * 4] as i32, buf[i * 4 + 2] as i32);
            let side = if ((i as u32 % W) as f32) < split {
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
            "左半（红段）应以红为主：红 {} 蓝 {}",
            left.0,
            left.1
        );
        assert!(
            right.1 > 20 && right.1 > right.0 * 4,
            "右半（蓝段）应以蓝为主：红 {} 蓝 {}",
            right.0,
            right.1
        );
    }

    /// ④ 半透明底 + 两种 alpha：多遍绘制 ≡ 「逐段单独画在不透明底上、合成后再统一预乘一次」。
    ///
    /// 守的是 `draw_runs` 文档里的二次预乘：每遍都「拷底 → 预乘回写」的实现，下一遍会把
    /// 已预乘的字当直通值再混一次。参照全程在不透明底（预乘即直通）上合成，最后一次性
    /// 按窗口 alpha 预乘，不经过这个坑。
    ///
    /// 底取「半透明黑」：预乘值恒为 0，参照里「被改动过」与「终值 ≠ 底」两种判据重合，
    /// 比对可以逐字节。
    ///
    /// 二次预乘只在「后一遍碰到前一遍的像素」处显形，没有这种像素本用例就测不出东西，
    /// 故先断言它存在，再断言错误做法确实会被抓到。文本取 `_j`：`j` 的下伸部回勾到
    /// 下划线底下，墨迹必然相交。别换成「两个全块字紧挨着、从半像素起画」——wine 的
    /// DirectWrite 不做抗锯齿，字形按整像素落位，那样一个共同像素都没有（实测）。
    #[test]
    fn multi_pass_equals_per_run_composite() {
        let r = tr();
        let text = "_j";
        let x0 = 4.0;
        let red_half = [220, 0, 0, 128];
        let hide = |c: [u8; 4]| [c[0], c[1], c[2], 0];
        let only_a = [run(0, 1, red_half), run(1, 2, hide(BLUE))];
        let only_c = [run(0, 1, hide(red_half)), run(1, 2, BLUE)];
        let both = [run(0, 1, red_half), run(1, 2, BLUE)];

        // 参照：不透明黑底上先画 A、再在其上画 C，然后统一按 alpha 128 预乘。
        let opaque = canvas([0, 0, 0, 255]);
        let mut comp = opaque.clone();
        draw_runs(&r, &mut comp, x0, text, &only_a);
        draw_runs(&r, &mut comp, x0, text, &only_c);
        let translucent = canvas([0, 0, 0, 128]);
        let mut expected = translucent.clone();
        for i in diff_px(&opaque, &comp) {
            for c in 0..3 {
                expected[i * 4 + c] = (comp[i * 4 + c] as u32 * 128 / 255) as u8;
            }
        }

        // 前提：两段确有共同覆盖的像素。
        let mut a_alone = opaque.clone();
        draw_runs(&r, &mut a_alone, x0, text, &only_a);
        let mut c_alone = opaque.clone();
        draw_runs(&r, &mut c_alone, x0, text, &only_c);
        let a_px = diff_px(&opaque, &a_alone);
        let shared = diff_px(&opaque, &c_alone)
            .into_iter()
            .filter(|i| a_px.contains(i))
            .count();
        assert!(shared > 0, "两段应有共同覆盖的像素，否则测不出二次预乘");

        let mut got = translucent.clone();
        draw_runs(&r, &mut got, x0, text, &both);
        assert_eq!(
            diff_px(&expected, &got),
            Vec::<usize>::new(),
            "多遍绘制应等于逐段合成后统一预乘一次"
        );

        // 错误做法（每遍各自预乘回写）确实会被本用例抓到。
        let mut naive = translucent.clone();
        draw_runs(&r, &mut naive, x0, text, &only_a);
        draw_runs(&r, &mut naive, x0, text, &only_c);
        assert!(
            !diff_px(&expected, &naive).is_empty(),
            "逐遍预乘回写的错误做法应与参照不同，否则本用例没有鉴别力"
        );
    }

    /// effect 反查全部失败（运行时换了指针）且区间全覆盖时，字仍按基色画出来。
    ///
    /// 全覆盖时基色那组不存在，只靠「反查不到按基色」兜底；这条兜底若只挂在基色组上，
    /// 整段文字会一个像素都不画——宁可颜色不对也不丢字。
    #[test]
    fn failed_effect_lookup_still_draws_in_base_color() {
        let r = tr();
        let text = "ab cd";
        let black = [0, 0, 0, 255];
        let bg = canvas([255, 255, 255, 255]);
        let mut plain = bg.clone();
        r.draw(&mut plain, W, H, 4.0, 4.0, text, &ts(), black)
            .expect("draw");
        r.break_effect_lookup.set(true);
        let mut buf = bg.clone();
        // 区间边界落在空格上，理由同 `base_color_effects_equal_plain_draw`。
        draw_runs(&r, &mut buf, 4.0, text, &[run(0, 2, RED), run(2, 5, BLUE)]);
        assert!(!diff_px(&bg, &buf).is_empty(), "反查失败时字不能丢");
        // 判「按基色画」直接对拍整段基色的 draw：按色相判会被 ClearType 的彩边误伤
        // （真 Windows 上黑字边缘本就偏红偏蓝）。
        assert_eq!(
            diff_px(&plain, &buf),
            Vec::<usize>::new(),
            "反查失败应按基色画，与整段基色的 draw 逐字节相同"
        );
    }

    /// ⑤ 颜色只进绘制：`draw_runs` 不改测量结果，也不往测量缓存里塞条目。
    #[test]
    fn draw_runs_leaves_measure_untouched() {
        let r = tr();
        let text = "abcd你好";
        let before = r.measure(text, &ts());
        let entries = r.measure_cache_len();
        let mut buf = canvas([255, 255, 255, 255]);
        draw_runs(&r, &mut buf, 4.0, text, &[run(0, 2, RED), run(4, 7, BLUE)]);
        assert_eq!(r.measure_cache_len(), entries, "draw_runs 不应写测量缓存");
        let after = r.measure(text, &ts());
        assert_eq!(
            (before.width, before.height),
            (after.width, after.height),
            "draw_runs 前后测量应相同"
        );
    }
}

// 测量缓存的**接线**测试：键函数再正确，没接进 `TextRenderer::measure` 也是白搭，
// 而 `measure_key_tests` 直接调键函数，接线断了它照样全绿。这里从公开的测量入口进，
// 用缓存条目数确认它真的被查过、被写过。
//
// 需要真实 DirectWrite（`TextRenderer::new` 建 COM 工厂），故 gate 到 Windows；
// 键本身的正确性由跨平台的 `measure_key_tests` 在 Linux CI 上守。
#[cfg(all(test, windows))]
mod measure_cache_tests {
    use super::{TextRenderer, TextStyle};

    fn tr() -> TextRenderer {
        TextRenderer::new("微软雅黑", 14.0).expect("建 DirectWrite TextRenderer")
    }

    /// 默认样式 + 指定字号。
    fn ts(size: f32) -> TextStyle<'static> {
        TextStyle::new(size)
    }

    /// 测量结果入缓存，重复测量命中而不新增条目。
    #[test]
    fn repeated_measure_hits_cache() {
        let r = tr();
        assert_eq!(r.measure_cache_len(), 0, "起手应为空");
        let a = r.measure("你好", &ts(14.0));
        assert_eq!(r.measure_cache_len(), 1, "首次测量应入缓存");
        let b = r.measure("你好", &ts(14.0));
        assert_eq!(r.measure_cache_len(), 1, "重复测量应命中，不得新增");
        assert_eq!(a.width, b.width, "命中值须与首次一致");
        assert_eq!(a.height, b.height);
    }

    /// 空串走的是提前返回，不该占用缓存条目。
    #[test]
    fn empty_text_does_not_populate_cache() {
        let r = tr();
        let _ = r.measure("", &ts(14.0));
        assert_eq!(r.measure_cache_len(), 0);
    }

    /// 换字体族清空缓存——键里 `None` 表示"用全局 family"，全局一换这些条目就失效了。
    #[test]
    fn set_font_family_clears_cache() {
        let mut r = tr();
        let _ = r.measure("你好", &ts(14.0));
        assert_eq!(r.measure_cache_len(), 1);
        r.set_font_family("宋体");
        assert_eq!(r.measure_cache_len(), 0, "换字体族须清空测量缓存");
    }

    /// 不同字号各占一条（键含字号），且两者宽度确有差异——顺带证明缓存没把它们混为一谈。
    #[test]
    fn distinct_sizes_are_cached_separately() {
        let r = tr();
        let small = r.measure("你好", &ts(12.0));
        let large = r.measure("你好", &ts(24.0));
        assert_eq!(r.measure_cache_len(), 2, "两种字号应各占一条");
        assert!(
            large.width > small.width,
            "24px 应宽于 12px（得 {} vs {}）",
            large.width,
            small.width
        );
    }
}

// 测量缓存键的跨平台测试（`measure_key` 不依赖 DirectWrite，与 `pua_runs` 同样不限平台）。
//
// 这里测的是**键的区分度**而非缓存命中：键漏掉任何一项影响测量的输入，后果都是某段文本
// 静默套用另一段的宽度——布局错位，且因为是缓存命中路径，重现条件依赖于测量顺序，极难定位。
#[cfg(test)]
mod measure_key_tests {
    use super::{TextStyle, measure_key};

    /// 字号 14、字重 400、指定字体族的基准样式。
    fn base_style() -> TextStyle<'static> {
        TextStyle::new(14.0)
            .with_weight(400)
            .with_family(Some("微软雅黑"))
    }

    /// 同一组输入恒得同一个键（缓存能命中的前提）。
    #[test]
    fn same_inputs_yield_same_key() {
        assert_eq!(
            measure_key("你好", &base_style()),
            measure_key("你好", &base_style())
        );
    }

    /// 四项输入各自独立参与键——逐项只改一个，键都必须变。
    #[test]
    fn each_input_affects_key() {
        let s = base_style();
        let base = measure_key("你好", &s);
        assert_ne!(base, measure_key("你好啊", &s), "文本");
        assert_ne!(
            base,
            measure_key("你好", &TextStyle { size: 16.0, ..s }),
            "字号"
        );
        assert_ne!(base, measure_key("你好", &s.with_weight(700)), "字重");
        assert_ne!(
            base,
            measure_key("你好", &s.with_family(Some("宋体"))),
            "字体族"
        );
    }

    /// 字号必须按 `to_bits()` 精确入键，不能取整。
    ///
    /// 字号是 DPI 缩放后的浮点：125% 下 12px→15.0、13px→16.25，150% 下 14px→21.0。
    /// 若按 `as u32`/`round()` 入键，16.25 与 16.8 会撞进同一条缓存——注释与正文只差
    /// 一两个像素时恰好落进这个区间，表现为某一档 DPI 下注释宽度突然用了正文的值。
    #[test]
    fn fractional_sizes_do_not_collide() {
        assert_ne!(
            measure_key("你好", &TextStyle::new(16.25)),
            measure_key("你好", &TextStyle::new(16.8)),
            "同一整数区间内的两个字号不得共用缓存键"
        );
    }

    /// `None`（用全局字体族）与显式指定不是一回事：`set_font_family` 只会让前者失效。
    /// 两者若共用键，换字体后显式指定的条目会被连带清掉（性能损失，无正确性问题），
    /// 更糟的是反过来——全局族的条目被显式族的值命中，直接就是错误宽度。
    #[test]
    fn none_family_differs_from_explicit() {
        let s = TextStyle::new(14.0);
        assert_ne!(
            measure_key("你好", &s),
            measure_key("你好", &s.with_family(Some("微软雅黑"))),
        );
    }

    /// 空串字体族经 `with_family` 归一成 `None`——统一在构造处收口，免得各调用点
    /// 各自过滤，漏一处就多出一条与 `None` 等价却不同键的缓存。
    #[test]
    fn empty_family_normalizes_to_none() {
        let s = TextStyle::new(14.0);
        assert_eq!(
            measure_key("你好", &s),
            measure_key("你好", &s.with_family(Some(""))),
            "空串字体族应归一为 None"
        );
        assert_eq!(s.with_family(Some("  ")).family, None, "纯空白也应归一");
    }
}

// 分段颜色区间的纯逻辑（不依赖任何后端），同 `pua_runs` 不限平台。
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

// 私用区分段的跨平台测试（`pua_runs` 不依赖 DirectWrite，故不限平台，Windows 本机
// `cargo test` 也覆盖）。真实数据取自两份拆字库的首行，避免自造码位掩盖区间边界错误。
#[cfg(test)]
mod pua_runs_tests {
    use super::pua_runs;

    fn runs(s: &str) -> Vec<(usize, usize)> {
        pua_runs(&s.encode_utf16().collect::<Vec<u16>>())
    }

    /// 内置 wubi86 拆字库："的" → U+E0E1 U+E124 U+E147 U+E13D（BMP 私用区，单码元）。
    #[test]
    fn bmp_pua_run_is_detected() {
        assert_eq!(runs("\u{E0E1}\u{E124}\u{E147}\u{E13D}"), vec![(0, 4)]);
    }

    /// 986 拆字库："的" → U+F00FD U+F00F7 U+F013C（补充私用区 A，各占 2 码元）。
    /// 修复前这一段完全不命中，字根落回主字体渲染成方框。
    #[test]
    fn spua_a_run_is_detected() {
        assert_eq!(runs("\u{F00FD}\u{F00F7}\u{F013C}"), vec![(0, 6)]);
    }

    /// 补充私用区 B（第 16 平面）同样纳入。
    #[test]
    fn spua_b_run_is_detected() {
        assert_eq!(runs("\u{100000}\u{10FFFD}"), vec![(0, 4)]);
    }

    /// 非私用区的**代理对不得命中**——CJK 扩展 B（U+20000）等生僻字若被误切到字根
    /// 字体集，反而会变成方框。这是判据不能只看"是不是代理对"的原因。
    #[test]
    fn non_pua_supplementary_chars_are_excluded() {
        assert!(runs("\u{20000}\u{2A6DF}\u{1F600}").is_empty());
    }

    /// 混排：汉字 + 字根 + 编码，段起止按 UTF-16 码元定位（非字符数）。
    /// "的" 1 码元 + "：" 1 码元 → 字根段从下标 2 起、占 6 码元。
    #[test]
    fn mixed_text_run_offsets_are_utf16_units() {
        assert_eq!(runs("的：\u{F00FD}\u{F00F7}\u{F013C} rqy"), vec![(2, 6)]);
    }

    /// 多段：被普通字符隔开的字根分别成段。
    #[test]
    fn separate_runs_are_not_merged_across_plain_text() {
        assert_eq!(runs("\u{E0E1}中\u{F00FD}"), vec![(0, 1), (2, 2)]);
    }

    /// BMP 与补充私用区相邻时合并为一段（目标字体族相同，合并不改变渲染）。
    #[test]
    fn adjacent_bmp_and_supplementary_pua_merge() {
        assert_eq!(runs("\u{E0E1}\u{F00FD}"), vec![(0, 3)]);
    }

    /// 孤立高位代理（非法 UTF-16，可能来自截断的外部数据）不得命中、不得越界 panic。
    #[test]
    fn lone_lead_surrogate_is_ignored() {
        assert!(pua_runs(&[0xDB80]).is_empty());
        assert!(pua_runs(&[0xDB80, 0x4E2D]).is_empty());
    }

    #[test]
    fn empty_and_plain_text_yield_no_runs() {
        assert!(runs("").is_empty());
        assert!(runs("中文 abc 123").is_empty());
    }
}

// 字体方案（脚本指派 + 回退链）的接线测试。**必须用真实 DirectWrite**：切段与回退是否
// 真的作用到排版上，只有度量能回答——`font_runs` 本身的正确性另有 `text::script` 的
// 平台无关单测覆盖，两层缺一不可（同 `pua_runs` 的分层）。
//
// ⚠️ 依赖本机装有 Consolas / 宋体 / Microsoft YaHei UI——中文 Windows 上三者恒在，
// 与既有测试用 "Microsoft YaHei UI" 的假设同级。
#[cfg(all(test, windows))]
mod font_plan_tests {
    use super::super::script::{FontPlan, ScriptClass};
    use super::{TextRenderer, TextStyle};

    fn tr() -> TextRenderer {
        TextRenderer::new("Microsoft YaHei UI", 16.0).expect("建 DirectWrite TextRenderer")
    }

    fn chain(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// ★ 行高不随 emoji 起伏：含 emoji 的候选行与纯汉字行等高（UNIFORM 行距，
    /// 见 `line_height_for`）。不钉行距时雅黑下高 5%、宋体下高 17%，候选窗随之抖动。
    #[test]
    fn emoji_row_is_as_tall_as_cjk_row() {
        let r = tr();
        let ts = TextStyle::new(20.0);
        let cjk = r.measure("开心", &ts).height;
        let with_emoji = r.measure("开心\u{1F604}", &ts).height;
        let emoji_only = r.measure("\u{1F604}", &ts).height;
        assert_eq!(cjk, with_emoji, "含 emoji 的行变高了");
        assert_eq!(cjk, emoji_only, "纯 emoji 行与汉字行不等高");
        // 硬换行仍按行数翻倍——candidate_window 的多行候选依赖这一点。
        let two = r.measure("a\nb", &ts).height;
        assert!((two - cjk * 2.0).abs() < 0.01, "两行={two}，单行={cjk}");
        // 宋体基准下同样成立，且行高是宋体自己的（比雅黑矮），不是被 emoji 抬到雅黑那档。
        let mut s = tr();
        s.set_font_plan(FontPlan::new(chain(&["SimSun"]), vec![]));
        let s_cjk = s.measure("开心", &ts).height;
        assert_eq!(s_cjk, s.measure("开心\u{1F604}", &ts).height);
        assert!(s_cjk < cjk, "宋体行高 {s_cjk} 应低于雅黑 {cjk}");
        // 粗体走自己的探针：钉出来的行高等于粗体字面的自然行高，而不是常规字面的。
        let bold = TextStyle {
            weight: 700,
            ..TextStyle::new(20.0)
        };
        let r = tr();
        assert_eq!(
            r.measure("开心", &bold).height,
            r.measure_natural("开心", &bold).height,
            "粗体行高应与其自然行高一致"
        );
        assert_eq!(
            r.measure("开心", &bold).height,
            r.measure("开心\u{1F604}", &bold).height
        );
    }

    /// ★ 键帽序列在 emoji 指派下连成一个字形：宽度是 emoji 的整宽，而非「一个 1 + 零宽框」。
    ///
    /// 这是 `font_runs` 回溯改写的真排版验证——单元测试只能证明切段对，连字有没有真的
    /// 形成得问 DirectWrite。未指派 emoji 时基准字体自带「1」，回退不触发，宽度等于单个「1」。
    #[test]
    fn keycap_ligates_under_emoji_assignment() {
        let ts = TextStyle::new(20.0);
        let keycap = "1\u{FE0F}\u{20E3}";
        let plain = tr();
        let w_digit = plain.measure("1", &ts).width;
        let w_plain = plain.measure(keycap, &ts).width;
        let mut r = tr();
        r.set_font_plan(FontPlan::new(
            chain(&["Microsoft YaHei UI"]),
            vec![(ScriptClass::Emoji, chain(&["Segoe UI Emoji"]))],
        ));
        let w_emoji = r.measure(keycap, &ts).width;
        assert!(
            w_emoji > w_digit * 1.8,
            "键帽没连成 emoji：指派后宽 {w_emoji}，单个「1」宽 {w_digit}"
        );
        assert!(
            w_emoji > w_plain,
            "指派后应比未指派更宽（{w_emoji} vs {w_plain}）"
        );
    }

    /// 平凡方案（只有一个默认字体）必须与「不设方案」逐位等价——这条守的是零回归：
    /// 绝大多数用户不会配脚本指派，他们的排版结果一个像素都不能变。
    #[test]
    fn trivial_plan_does_not_change_layout() {
        let base = tr().measure("abc中文123", &TextStyle::new(16.0));
        let mut r = tr();
        r.set_font_plan(FontPlan::new(chain(&["Microsoft YaHei UI"]), vec![]));
        let with = r.measure("abc中文123", &TextStyle::new(16.0));
        assert_eq!(base.width, with.width);
        assert_eq!(base.height, with.height);
    }

    /// ★ 脚本指派真的作用到排版上：把拉丁指派给等宽的 Consolas 后，纯拉丁串的宽度必须变。
    ///
    /// 反向对照同样重要：**汉字串的宽度不得变**——只断言「变了」的话，一个「把整串都换成
    /// Consolas」的错误实现照样通过，而那正是切段没生效的表现。
    #[test]
    fn latin_assignment_applies_to_latin_only() {
        let text_latin = "illillill";
        let text_cjk = "中文候选";
        let ts = TextStyle::new(16.0);

        let plain = tr();
        let (w_latin0, w_cjk0) = (
            plain.measure(text_latin, &ts).width,
            plain.measure(text_cjk, &ts).width,
        );

        let mut r = tr();
        r.set_font_plan(FontPlan::new(
            chain(&["Microsoft YaHei UI"]),
            vec![(ScriptClass::Latin, chain(&["Consolas"]))],
        ));
        let (w_latin1, w_cjk1) = (
            r.measure(text_latin, &ts).width,
            r.measure(text_cjk, &ts).width,
        );

        assert_ne!(
            w_latin0, w_latin1,
            "拉丁指派没生效：宽度与未指派时相同（{w_latin0} vs {w_latin1}）"
        );
        assert_eq!(
            w_cjk0, w_cjk1,
            "汉字宽度不该受拉丁指派影响——切段没生效，整串都被换了字体"
        );

        // ★ 上面两条都是**纯单脚本**串，一个「只在整串同脚本时才应用指派」的实现能通过。
        // 混合串才真正走切段：拉丁那半的宽度变化量必须与纯拉丁串完全一致，
        // 汉字那半不贡献任何变化。
        let mixed = "illillill中文候选";
        let delta_mixed = r.measure(mixed, &ts).width - plain.measure(mixed, &ts).width;
        let delta_latin = w_latin1 - w_latin0;
        assert!(
            (delta_mixed - delta_latin).abs() < 0.5,
            "混合串里的拉丁段没被单独切出来：混合串变化 {delta_mixed}，纯拉丁串变化 {delta_latin}"
        );
    }

    /// ★★ 单渲染器、双链、链尾不同——直接验 `baseFamilyName` **筛选**是否生效。
    ///
    /// 这是多链设计的载重假设：靠 base family 区分「这一段该走哪条链」。
    /// 「中」是 Cjk 类、被声明并指派了 Consolas（无汉字字形）⇒ 由 cjk 链的链尾决定字形。
    /// 若 DWrite 不按 `baseFamilyName` 区分，第一条映射（默认链）会吃掉全部匹配，
    /// 两次都用默认链的链尾「宋体」⇒ 高度相等 ⇒ 红。
    ///
    /// ⚠️ 与 `fallback_chain_is_applied` 缺一不可：那条只证明「自定义回退挂上了」，
    /// 它的两个渲染器各只有一条映射，筛不筛选都不改变结果。
    #[test]
    fn fallback_chain_picks_the_chain_of_its_own_base() {
        let ts = TextStyle::new(16.0);
        let mk = |cjk_tail: &str| {
            let mut r = tr();
            r.set_font_plan(FontPlan::new(
                chain(&["Segoe UI", "宋体"]),
                vec![(ScriptClass::Cjk, chain(&["Consolas", cjk_tail]))],
            ));
            // 自然行高才分辨得出字体；生产路径的行距已钉死（见 `line_height_for`）。
            r.measure_natural("中", &ts).height
        };
        let with_yahei = mk("Microsoft YaHei UI");
        let with_songti = mk("宋体");
        assert_ne!(
            with_yahei, with_songti,
            "baseFamilyName 筛选没生效：cjk 链被默认链吃掉了（{with_yahei} vs {with_songti}）"
        );
    }

    /// ★★ 主题节点配了 `font_family` 时，用户配的默认回退链**仍须生效**。
    ///
    /// 叶子级字族让该段的 base 与任何一条链的链首都不匹配，若没有那条
    /// `baseFamilyName = None` 的兜底映射，`ui.font.fallback` 会整条静默失效——
    /// 而 macOS 侧（cascade list 是字体级属性）同样情形下链照常生效，
    /// 同一份配置两平台结果相反且都不吭声。
    #[test]
    fn default_chain_still_applies_under_a_leaf_family_override() {
        // 叶子级把 base 换成 Consolas（无汉字字形），默认链的链尾决定「中」的字形。
        let ts = TextStyle::new(16.0).with_family(Some("Consolas"));
        let mk = |tail: &str| {
            let mut r = tr();
            r.set_font_plan(FontPlan::new(chain(&["Segoe UI", tail]), vec![]));
            r.measure_natural("中", &ts).height
        };
        assert_ne!(
            mk("Microsoft YaHei UI"),
            mk("宋体"),
            "叶子级字族一非空，默认回退链就整条失效了"
        );
    }

    /// ★★ 回退链真的生效：Consolas 没有汉字字形，链尾决定「中」用什么字体渲染。
    /// 两条链的链尾字体行高不同（宋体 ≈1.16 em、雅黑 ≈1.33 em），故高度必须不同。
    ///
    /// ⚠️ 它**只**证明「自定义回退对象被挂上了」。筛选由
    /// `fallback_chain_picks_the_chain_of_its_own_base` 验证——这里每个渲染器只有一条映射。
    #[test]
    fn fallback_chain_is_applied() {
        let ts = TextStyle::new(16.0);
        let mut a = tr();
        a.set_font_plan(FontPlan::new(chain(&["Consolas", "宋体"]), vec![]));
        let mut b = tr();
        b.set_font_plan(FontPlan::new(
            chain(&["Consolas", "Microsoft YaHei UI"]),
            vec![],
        ));
        let ha = a.measure_natural("中", &ts).height;
        let hb = b.measure_natural("中", &ts).height;
        assert_ne!(
            ha, hb,
            "回退链未生效：两条链的链尾不同却量出同样的行高（{ha} vs {hb}）"
        );
    }

    /// ★★★ 判决性测量：默认链的兜底映射会不会把**脚本指派**整个吃掉。
    ///
    /// 配置形态取的就是 C 的样例：主字体有回退链、拉丁指派只有一项。
    /// 那条指派链因 `len < 2` 不建具名映射，于是拉丁段的 base 与任何具名 `baseFamilyName`
    /// 都不匹配——若它落到 `base = None` 的兜底映射上，就会被整段换成默认链的字体，
    /// **拉丁指派完全不生效**，而这正是本功能存在的理由。
    #[test]
    fn default_chain_fallback_does_not_swallow_a_script_assignment() {
        let ts = TextStyle::new(16.0);
        let mut mixed = tr();
        mixed.set_font_plan(FontPlan::new(
            chain(&["Microsoft YaHei UI", "宋体"]),
            vec![(ScriptClass::Latin, chain(&["Consolas"]))],
        ));
        let mut pure_consolas = tr();
        pure_consolas.set_font_plan(FontPlan::new(chain(&["Consolas"]), vec![]));
        let mut pure_yahei = tr();
        pure_yahei.set_font_plan(FontPlan::new(chain(&["Microsoft YaHei UI"]), vec![]));

        let w_mixed = mixed.measure("illillill", &ts).width;
        let w_consolas = pure_consolas.measure("illillill", &ts).width;
        let w_yahei = pure_yahei.measure("illillill", &ts).width;
        assert_ne!(
            w_consolas, w_yahei,
            "前置条件：Consolas 与雅黑量出同宽，本用例无从判别"
        );
        assert_eq!(
            w_mixed, w_consolas,
            "拉丁指派被默认链的兜底映射吃掉了（实测 {w_mixed}，Consolas {w_consolas}，雅黑 {w_yahei}）"
        );
    }

    /// ★★★ 同上，换成**叶子级字族**（方案级 `[candidate] font_family` 走的就是它）。
    ///
    /// 与 `default_chain_still_applies_under_a_leaf_family_override` 互补、缺一不可：
    /// 那条测的是「叶子字体**缺字**时链要接上」（量的是 Consolas 没有的汉字），
    /// 本条测的是「叶子字体**有这个字**时不能被链抢走」（量的是 Consolas 有的拉丁）。
    /// 只有前者的话，一个「兜底映射把每个字符都换成默认链」的实现照样通过——
    /// 而那会让方案级字体在配了 `ui.font.fallback` 时静默失效。
    #[test]
    fn leaf_family_survives_a_non_empty_default_chain() {
        let ts_leaf = TextStyle::new(16.0).with_family(Some("Consolas"));
        let ts = TextStyle::new(16.0);
        let mut with_chain = tr();
        with_chain.set_font_plan(FontPlan::new(
            chain(&["Microsoft YaHei UI", "宋体"]),
            vec![],
        ));
        let mut pure_consolas = tr();
        pure_consolas.set_font_plan(FontPlan::new(chain(&["Consolas"]), vec![]));
        assert_eq!(
            with_chain.measure("illillill", &ts_leaf).width,
            pure_consolas.measure("illillill", &ts).width,
            "叶子级字族被默认链的兜底映射换掉了 —— 方案级字体会在配了 fallback 时静默失效"
        );
    }

    /// 换方案必须清空测量缓存——方案不在 `measure_key` 里（它是渲染器级状态），
    /// 不清就会拿旧方案的宽度布局新方案的文字。
    ///
    /// ★ 断言落在**条目数**上：只断言「宽度变了」的话，缓存完全没接线（每次重算）
    /// 时也照样通过，而缓存正是这里唯一要验的东西。
    #[test]
    fn set_font_plan_clears_measure_cache() {
        let mut r = tr();
        let _ = r.measure("abc", &TextStyle::new(16.0));
        assert_eq!(r.measure_cache_len(), 1);
        r.set_font_plan(FontPlan::new(
            chain(&["Microsoft YaHei UI"]),
            vec![(ScriptClass::Latin, chain(&["Consolas"]))],
        ));
        assert_eq!(r.measure_cache_len(), 0, "换字体方案须清空测量缓存");
    }

    /// ★★ 换方案必须让**已构建的回退对象**跟着失效。
    ///
    /// 三条缓存测试断言的都是 `measure_cache_len()`，而回退对象是另一份派生状态：
    /// 删掉 `set_font_plan` 里的 `fallback.take()`，那三条照样绿。真实后果是用户在设置页
    /// 改一次 `ui.font.fallback`，新链要到**重启**才生效——正是最容易复发的那颗雷。
    /// 判据必须落在**同一个渲染器**上换第二次方案（`fallback_chain_is_applied` 用的是两个
    /// 各只 set 一次的渲染器，碰不到这条路径）。
    #[test]
    fn changing_the_plan_rebuilds_the_fallback_object() {
        let ts = TextStyle::new(16.0);
        let mut r = tr();
        r.set_font_plan(FontPlan::new(chain(&["Consolas", "宋体"]), vec![]));
        let h1 = r.measure_natural("中", &ts).height;
        r.set_font_plan(FontPlan::new(
            chain(&["Consolas", "Microsoft YaHei UI"]),
            vec![],
        ));
        let h2 = r.measure_natural("中", &ts).height;
        assert_ne!(h1, h2, "换方案后仍在用旧的回退对象，新链要重启才生效");
    }

    /// 设同一份方案是空操作，不该白清缓存（每帧重设方案是热重载路径的常态）。
    #[test]
    fn setting_the_same_plan_keeps_the_cache() {
        let mut r = tr();
        let p = || FontPlan::new(chain(&["Microsoft YaHei UI"]), vec![]);
        r.set_font_plan(p());
        let _ = r.measure("abc", &TextStyle::new(16.0));
        assert_eq!(r.measure_cache_len(), 1);
        r.set_font_plan(p());
        assert_eq!(r.measure_cache_len(), 1, "同一份方案不该触发清空");
    }

    /// 叶子级字族（主题节点的 `font_family`）仍然赢过方案默认链的链首，
    /// 但**赢不过脚本指派**——三层合成的次序，见 `create_layout` 的文档。
    #[test]
    fn leaf_family_beats_plan_default_but_not_script_assignment() {
        let mut r = tr();
        r.set_font_plan(FontPlan::new(
            chain(&["宋体"]),
            vec![(ScriptClass::Latin, chain(&["Consolas"]))],
        ));
        let ts_leaf = TextStyle::new(16.0).with_family(Some("Microsoft YaHei UI"));

        // 汉字走 base family：叶子级的雅黑应赢过方案默认链的宋体。
        let mut only_plan = tr();
        only_plan.set_font_plan(FontPlan::new(chain(&["宋体"]), vec![]));
        let mut leaf_only = tr();
        leaf_only.set_font_plan(FontPlan::new(chain(&["Microsoft YaHei UI"]), vec![]));
        let h_songti = only_plan.measure("中", &TextStyle::new(16.0)).height;
        let h_yahei = leaf_only.measure("中", &TextStyle::new(16.0)).height;
        // ★ 没有这条对照的话，一个**把 plan.base_family() 整个忽略**的实现照样通过：
        // `tr()` 本身就是用雅黑建的 TextFormat，右边的期望值与「什么都没做」不可区分。
        assert_ne!(
            h_songti, h_yahei,
            "宋体与雅黑量出同样的行高，本用例的对照失效、下面那条断言等于没测"
        );
        assert_eq!(
            r.measure("中", &ts_leaf).height,
            h_yahei,
            "叶子级字族没赢过方案默认链"
        );

        // 拉丁走脚本指派：叶子级字族不该把 Consolas 顶掉。
        let mut consolas = tr();
        consolas.set_font_plan(FontPlan::new(chain(&["Consolas"]), vec![]));
        assert_eq!(
            r.measure("illill", &ts_leaf).width,
            consolas.measure("illill", &TextStyle::new(16.0)).width,
            "脚本指派没赢过叶子级字族"
        );
    }
}

// 非 Windows mock 文本渲染器的冒烟测试：验证 mock 的等宽近似测量契约
// （字符数 × 字号 × 0.6）与 draw_text 空操作。
// 边界：真实字形宽度/渲染由 Windows + DirectWrite 决定，**不在此覆盖，须 Windows 实测**。
#[cfg(all(test, mock_text))]
mod tests {
    use super::TextRenderer;

    #[test]
    fn mock_measure_empty_is_zero_width() {
        let tr = TextRenderer::new("any", 20.0).unwrap();
        let m = tr.measure_text("");
        assert_eq!(m.width, 0.0);
        assert!(m.height > 0.0);
    }

    #[test]
    fn mock_measure_scales_with_char_count() {
        let tr = TextRenderer::new("any", 20.0).unwrap();
        let one = tr.measure_text("中").width;
        let three = tr.measure_text("中文字").width;
        assert!(three > one);
        assert!((three - one * 3.0).abs() < 1e-3);
    }

    #[test]
    fn mock_draw_text_is_ok() {
        let tr = TextRenderer::new("any", 16.0).unwrap();
        let mut buf = vec![0u8; 8 * 8 * 4];
        assert!(
            tr.draw_text(&mut buf, 8, 8, 0.0, 0.0, "x", [0, 0, 0, 255])
                .is_ok()
        );
    }

    /// mock 的 `draw_runs` 与真实后端同口径：区间全部非法时退回 `draw`，不记 `draw_runs` 行。
    #[test]
    fn mock_draw_runs_with_only_invalid_runs_is_plain_draw() {
        use super::{ColorRun, TextStyle};
        let tr = TextRenderer::new("any", 16.0).unwrap();
        let mut buf = vec![0u8; 8 * 8 * 4];
        let bad = [ColorRun {
            start: 1,
            end: 2, // 切在「你」中间
            rgba: [255, 0, 0, 255],
        }];
        let ts = TextStyle::new(16.0);
        tr.draw_runs(&mut buf, 8, 8, 0.0, 0.0, "你", &ts, [0; 4], &bad)
            .unwrap();
        tr.draw(&mut buf, 8, 8, 0.0, 0.0, "你", &ts, [0; 4])
            .unwrap();
        let log = tr.take_draw_log();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0], log[1], "全非法区间应与 draw 记录逐字相同");
    }
}

/// 方案自带字体（`[engine.chaizi]` 的 `font_path` / `font_family`）的家族名探针。
///
/// # 为什么需要它
///
/// 词库级字体走的是 [`TextRenderer::set_chaizi_font`] 那条**自定义字体集**通路：
/// PUA 段先 `SetFontCollection(自定义集)`，再 `SetFontFamilyName(方案里写的家族名)`。
/// 后者在集里找不到该家族名时**不报错**——`create_layout` 里连返回值都没接。方案作者把
/// `font_family` 写成了字体文件名、或写成自己起的别名（而不是 TTF `name` 表里的家族名）时，
/// 画面上只表现为「这一段字没画出来 / 尺寸不对」，日志里一片安静。
///
/// 本用例把「家族名写对」与「写错」两种情形的**测量宽度**和**实际着墨像素数**并排量出来，
/// 一次就能判断某个方案的字体到底有没有真的生效。
///
/// ⚠️ 需要真实字体文件，故 `#[ignore]`；用环境变量喂参数，不把任何用户路径写进仓库。
/// ```text
/// $env:WIND_TEST_FONT        = "…\toli\Menk Qagan StdEx Tig.ttf"
/// $env:WIND_TEST_FAMILY      = "Menk字体"                  # 方案里声明的
/// $env:WIND_TEST_FAMILY_REAL = "Menk Qagan StdEx Tig"      # 字体自报的
/// $env:WIND_TEST_TEXT        = "…"                          # 默认单个 U+E264
/// cargo test -p wind-ui --lib -j 2 dictionary_font_family -- --ignored --nocapture
/// ```
#[cfg(all(test, windows))]
mod dict_font_probe {
    use super::{TextRenderer, TextStyle};

    /// 缓冲区里「着了墨」的像素数（背景全白，取蓝通道判暗）。
    ///
    /// ★ 判据用**着墨像素**而不是测量宽度：字族名没匹配上时，测量仍可能回落到某个字体
    /// 而返回一个像模像样的宽度——只有真去画一遍，才分得开「量出来有」与「画出来有」。
    fn inked(buf: &[u8], w: u32, h: u32) -> usize {
        (0..(w * h) as usize)
            .filter(|i| buf[i * 4 + 2] < 200)
            .count()
    }

    /// 最右侧着墨列的 x（无墨返回 `None`）。
    ///
    /// ★★ 它是「测量说多宽」与「实际画到哪」的**对照量**：两者接近 = 画全了；
    /// 实际远小于测量 = 字形在中途丢了（缺字、字体没生效、或上游把串截短了）。
    /// 只看着墨总数分不开这两种——字形密的短串与字形疏的长串可以数出同一个总数。
    fn rightmost_ink(buf: &[u8], w: u32, h: u32) -> Option<u32> {
        (0..w)
            .rev()
            .find(|&x| (0..h).any(|y| buf[((y * w + x) * 4 + 2) as usize] < 200))
    }

    #[test]
    #[ignore = "需要真实词库字体文件；路径由 WIND_TEST_FONT 给出"]
    fn dictionary_font_family_name_mismatch_probe() {
        let Ok(path) = std::env::var("WIND_TEST_FONT") else {
            eprintln!("跳过：未设 WIND_TEST_FONT");
            return;
        };
        let declared = std::env::var("WIND_TEST_FAMILY").unwrap_or_default();
        let real = std::env::var("WIND_TEST_FAMILY_REAL").unwrap_or_default();
        let text = std::env::var("WIND_TEST_TEXT").unwrap_or_else(|_| "\u{E264}".to_string());
        eprintln!(
            "探针文本：{}",
            text.chars()
                .map(|c| format!("U+{:04X}", c as u32))
                .collect::<Vec<_>>()
                .join(" ")
        );

        // 「不设词库字体」是基线：PUA 段走主字体，本就画不出蒙文字形。
        // 没有它就分不清「家族名写对了」与「两种写法都没生效」。
        for family in ["<不设词库字体>", declared.as_str(), real.as_str()] {
            let mut r = TextRenderer::new("Microsoft YaHei UI", 18.0).expect("建 TextRenderer");
            if family != "<不设词库字体>" {
                if family.is_empty() {
                    continue;
                }
                if let Err(e) = r.set_chaizi_font(&path, family) {
                    eprintln!("family={family:?}: set_chaizi_font 失败：{e}");
                    continue;
                }
            }
            // ★ 字重是独立一维：词库自带字体通常**只含 Regular 一个字重**，而候选窗的选中态
            // 常被主题配成加粗（`[text.selected] font_weight`）。自定义字体集里找不到 Bold 时
            // 会发生什么，只有量一次才知道——「首候选空白、其余正常」正是这个形状。
            let weight: i32 = std::env::var("WIND_TEST_WEIGHT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let ts = TextStyle::new(18.0).with_weight(weight);
            let m = r.measure(&text, &ts);
            // 缓冲区按测量宽度开，比它宽出一截——太窄会让 `draw` 的裁剪矩形自己把右边裁掉，
            // 那正是本用例要测的那个量，不能由缓冲区尺寸制造出来。
            let bw = (m.width.ceil() as u32 + 60).max(240);
            let bh = (m.height.ceil() as u32 + 24).max(64);
            let mut buf = vec![255u8; (bw * bh * 4) as usize];
            r.draw(&mut buf, bw, bh, 2.0, 2.0, &text, &ts, [0, 0, 0, 255])
                .expect("draw");
            let right = rightmost_ink(&buf, bw, bh);
            eprintln!(
                "family={family:?} weight={weight}: measure={:.2}×{:.2}  着墨像素={}  最右着墨列={:?}（画满应≈{:.0}）",
                m.width,
                m.height,
                inked(&buf, bw, bh),
                right,
                m.width + 2.0
            );
            // 逐前缀宽度：`truncate_text_for_width` 的二分正是按这些量走的，
            // 非单调或跳变会让它裁在意料之外的位置。
            let chars: Vec<char> = text.chars().collect();
            let widths: Vec<String> = (1..=chars.len())
                .map(|n| {
                    let s: String = chars[..n].iter().collect();
                    format!("{:.0}", r.measure(&s, &ts).width)
                })
                .collect();
            eprintln!("  逐前缀宽度({} 字)：{}", chars.len(), widths.join(" "));
            // 存一张图：字形对不对、有没有躺着、每个字形多大，这些只有看画面才判得了，
            // 任何标量（宽度、着墨数）都答不上来。`WIND_TEST_OUT` 给目录，不设则不存。
            if let Ok(dir) = std::env::var("WIND_TEST_OUT") {
                let safe: String = family
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { '_' })
                    .collect();
                let path = format!("{dir}/probe_{safe}.png");
                // draw 出来的是 BGRA，image 要 RGBA——交换红蓝两通道。
                let rgba: Vec<u8> = buf
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[2], p[1], p[0], 255])
                    .collect();
                match image::save_buffer(&path, &rgba, bw, bh, image::ColorType::Rgba8) {
                    Ok(()) => eprintln!("  已存图：{path}"),
                    Err(e) => eprintln!("  存图失败：{e}"),
                }
            }
        }
    }
}
