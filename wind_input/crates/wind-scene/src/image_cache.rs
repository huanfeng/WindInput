//! 背景图解码与填充缓存（九宫格/拉伸/平铺/center）。
//!
//! 与 Go 版 `internal/ui/viewbox_image_resolver.go` 对齐（精简）。线程局部使用（UI 单线程）。
//! 源图解码后保留 unpremult RGBA 供采样；按 (源图, mode, slice, dest_w, dest_h) 缓存合成后的
//! 目标位图（tiny-skia Pixmap，**BGRA 序 + 预乘**，可直接作 Pattern 填到 BGRA 缓冲）。
//!
//! 图片源有两种形态：**文件路径**，以及主题里内嵌的 **`data:` URI**——主题编辑器上传的
//! 图片会被打包成后者随 theme.toml 一起发布。两种形态在 `wind_theme` 的求值层与
//! `theme_assets::asset_path` 里一路同等放行，最终都落到本模块解码。

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tiny_skia::{Pixmap, PremultipliedColorU8};

#[inline]
fn transparent() -> PremultipliedColorU8 {
    PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap()
}

/// 合成单像素为 BGRA 预乘（R/B 交换以适配 BGRA 缓冲）。
/// tint[3]>0：把图当 alpha mask、用 tint 色填充；否则按 premult 标志直通（svg 已预乘 / 位图未预乘）。
#[inline]
fn compose(r: u8, g: u8, b: u8, a: u8, tint: [u8; 4], premult: bool) -> PremultipliedColorU8 {
    if tint[3] > 0 {
        let ta = ((a as u16 * tint[3] as u16) / 255) as u8;
        let p = |c: u8| ((c as u16 * ta as u16) / 255) as u8;
        PremultipliedColorU8::from_rgba(p(tint[2]), p(tint[1]), p(tint[0]), ta)
            .unwrap_or_else(transparent)
    } else if premult {
        PremultipliedColorU8::from_rgba(b, g, r, a).unwrap_or_else(transparent)
    } else {
        let p = |c: u8| ((c as u16 * a as u16) / 255) as u8;
        PremultipliedColorU8::from_rgba(p(b), p(g), p(r), a).unwrap_or_else(transparent)
    }
}

/// 解析 SVG 源为 usvg 树。**外部引用一律不解析**。
///
/// usvg 默认的 href 解析器会把 `<image href="C:/...">` 当本地文件读进来渲染（其文档
/// 明写 "forbid access to local files (which is allowed by default)"）。主题可以来自
/// 市场，是不可信内容——在内嵌 SVG 放行之前它到不了这里（导入只收 TOML 文本，图片
/// 不落盘），放行之后这条路就通了，必须当场堵死。
fn svg_tree(src: &str) -> Option<resvg::usvg::Tree> {
    let data = read_source_bytes(src)?;
    let opts = resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_string: Box::new(|_, _| None),
            ..Default::default()
        },
        ..Default::default()
    };
    resvg::usvg::Tree::from_data(&data, &opts).ok()
}

/// 把 SVG 树按 `ts` 渲染进一张 w×h 画布（resvg 输出，预乘 RGBA）。
fn render_svg(
    tree: &resvg::usvg::Tree,
    w: u32,
    h: u32,
    ts: resvg::tiny_skia::Transform,
) -> Option<resvg::tiny_skia::Pixmap> {
    if !crate::skin::fits_image_limits(w, h) {
        return None;
    }
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(tree, ts, &mut pm.as_mut());
    Some(pm)
}

/// 栅格化 SVG 并拉伸到 w×h（两轴各自缩放，stretch / nine_slice 用）。
fn rasterize_svg(src: &str, w: u32, h: u32) -> Option<resvg::tiny_skia::Pixmap> {
    let tree = svg_tree(src)?;
    let size = tree.size();
    let sx = w as f32 / size.width().max(1.0);
    let sy = h as f32 / size.height().max(1.0);
    render_svg(&tree, w, h, resvg::tiny_skia::Transform::from_scale(sx, sy))
}

/// SVG center / tile：单元 = 文档尺寸 × k（统一比例，不为凑整而纵横各拉一份；单元按
/// ceil 取整，多出的边角透明）。**只栅格化目标盒里看得见的部分**——单元尺寸来自主题
/// （不可信），整单元栅格化可以大到分配不出来（4000² 文档 × 200% 就是 512 MB）：
/// - center：直接把居中后的单元渲进 w×h 画布，越界的部分 resvg 自己裁掉；
/// - tile：只渲 `min(单元, 盒)` 那么大一块，再按单元周期平铺——单元比盒大时，
///   盒内看到的恰是单元左上那一块，周期取渲出的尺寸结果相同。
///
/// 返回 `(画布, 采样用的源宽高)`：center 已是 w×h 成品，tile 交给 `sample_into` 平铺。
fn rasterize_svg_unit(
    src: &str,
    tile: bool,
    k: f32,
    w: u32,
    h: u32,
) -> Option<(resvg::tiny_skia::Pixmap, u32, u32)> {
    let tree = svg_tree(src)?;
    let size = tree.size();
    let unit = |d: f32| (d * k).ceil().clamp(1.0, u32::MAX as f32) as u32;
    let (uw, uh) = (unit(size.width()), unit(size.height()));
    let scale = resvg::tiny_skia::Transform::from_scale(k, k);
    if tile {
        let (rw, rh) = (uw.min(w), uh.min(h));
        return render_svg(&tree, rw, rh, scale).map(|pm| (pm, rw, rh));
    }
    // 居中偏移与位图 center（`map_src`）同一取整：整数像素对齐，不糊边。
    let ox = (w as i64 - uw as i64) / 2;
    let oy = (h as i64 - uh as i64) / 2;
    render_svg(&tree, w, h, scale.post_translate(ox as f32, oy as f32)).map(|pm| (pm, w, h))
}

/// 栅格化**内联 SVG 字符串** → tint 后的 BGRA 预乘 Pixmap（工具栏图标用，不走文件缓存）。
/// SVG 仅作 alpha 覆盖蒙版，用 `tint` 单色填充 → 图标颜色随主题。w/h 为目标像素（方形图标传等值）。
pub fn rasterize_svg_str_tinted(svg: &str, w: u32, h: u32, tint: [u8; 4]) -> Option<Pixmap> {
    if w == 0 || h == 0 {
        return None;
    }
    let tree =
        resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let mut rpm = resvg::tiny_skia::Pixmap::new(w, h)?;
    let sx = w as f32 / size.width().max(1.0);
    let sy = h as f32 / size.height().max(1.0);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(sx, sy),
        &mut rpm.as_mut(),
    );
    let rgba = rpm.data();
    let mut pm = Pixmap::new(w, h)?;
    for (i, p) in pm.pixels_mut().iter_mut().enumerate() {
        let b = i * 4;
        *p = compose(rgba[b], rgba[b + 1], rgba[b + 2], rgba[b + 3], tint, true);
    }
    Some(pm)
}

/// `data:` URI 的 MIME（不解码载荷）。非 data: 源返回 None。
fn data_uri_mime(src: &str) -> Option<&str> {
    // split 的迭代器至少产出一项，两处 next() 都不会是 None。
    let head = src
        .strip_prefix("data:")?
        .split(',')
        .next()
        .unwrap_or_default();
    Some(head.split(';').next().unwrap_or_default())
}

/// 解码 `data:<mime>[;...];base64,<载荷>` 的载荷字节。
///
/// 只认**标准字母表 + 带填充**的 base64：内嵌图片的唯一生产者是主题编辑器的
/// `FileReader.readAsDataURL`，它产出的一定是这种。百分号编码与 URL-safe 变体没有
/// 生产者，不实现——撞上时返回 None 走上层的「解码失败」告警，而不是静默画一块
/// 空白让人无从查起。
fn decode_data_uri(src: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let rest = src.strip_prefix("data:")?;
    let (head, payload) = rest.split_once(',')?;
    if !head.split(';').any(|p| p.eq_ignore_ascii_case("base64")) {
        return None;
    }
    // TOML 多行字符串里的 data: URI 可能带换行，base64 解码器不接受空白，先剔除。
    let cleaned: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .ok()
}

/// 读取图片源的原始字节：`data:` URI 就地解码，否则按文件路径读。
fn read_source_bytes(src: &str) -> Option<Vec<u8>> {
    if src.starts_with("data:") {
        return decode_data_uri(src);
    }
    std::fs::read(src).ok()
}

/// 动画装饰最多解这么多帧，其后的帧丢弃（不可信主题的解码耗时上限）。
pub const MAX_ANIM_FRAMES: usize = 300;

/// 解码限额：单边 [`crate::skin::MAX_IMAGE_SIDE`]，分配约 [`crate::skin::MAX_IMAGE_PIXELS`] 张 RGBA8。
fn decode_limits() -> image::Limits {
    let mut l = image::Limits::default();
    l.max_image_width = Some(crate::skin::MAX_IMAGE_SIDE);
    l.max_image_height = Some(crate::skin::MAX_IMAGE_SIDE);
    l.max_alloc = Some(crate::skin::MAX_IMAGE_PIXELS * 4 + 16 * 1024 * 1024);
    l
}

/// 解码位图源：`data:` URI 就地解码，否则读文件；按内容判格式，带解码限额，超限即失败。
fn decode_bitmap(src: &str) -> Option<image::DynamicImage> {
    let bytes = read_source_bytes(src)?;
    let reader = || {
        image::ImageReader::new(std::io::Cursor::new(&bytes[..]))
            .with_guessed_format()
            .ok()
    };
    let (w, h) = reader()?.into_dimensions().ok()?;
    if !crate::skin::fits_image_limits(w, h) {
        tracing::warn!("主题图片尺寸 {w}x{h} 超出上限，不解码: {}", brief(src));
        return None;
    }
    let mut r = reader()?;
    r.limits(decode_limits());
    r.decode().ok()
}

/// 是否 SVG：文件看扩展名，`data:` 看 MIME。
///
/// 只看扩展名会漏掉 `data:image/svg+xml;base64,...`（它不以 `.svg` 结尾），把矢量图
/// 丢进位图解码器 —— 那是必然失败且看不出原因的一条路。
/// 源是否 SVG（皮肤接入层据此回退：皮肤窗体只收位图）。
pub fn is_svg_source(src: &str) -> bool {
    is_svg(src)
}

fn is_svg(src: &str) -> bool {
    match data_uri_mime(src) {
        Some(mime) => mime.eq_ignore_ascii_case("image/svg+xml"),
        None => src.to_ascii_lowercase().ends_with(".svg"),
    }
}

/// 日志用的短标识：`data:` URI 动辄数百 KB，整条打进日志会把日志撑爆且毫无可读性。
fn brief(src: &str) -> Cow<'_, str> {
    if src.starts_with("data:") {
        let head: String = src.chars().take(48).collect();
        Cow::Owned(format!("{head}…（共 {} 字节）", src.len()))
    } else {
        Cow::Borrowed(src)
    }
}

/// 一次填充的「怎么铺」：模式 + 九宫切片 + 中段是否平铺。
///
/// 收成一个结构而不是三个平行参数，是为了让 `fill` 与缓存键都只多一处而不是三处——
/// 它们本来就是同进同出的一组。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FillSpec {
    /// 见 [`mode_code`]。
    pub mode: u8,
    /// 仅 nine_slice：源图四边切片像素 [上,右,下,左]。
    pub slice: [u32; 4],
    /// 仅 nine_slice：中段沿 [x, y] 轴平铺而非拉伸。
    pub repeat: [bool; 2],
    /// DPI 缩放 ×1000（整数才能进缓存键）。仅 SVG 的 center / tile 用：单元尺寸 =
    /// 文档尺寸 × scale，scale 不同单元就不同，必须分开缓存。位图按纹理像素，不看它。
    pub scale_milli: u32,
}

impl FillSpec {
    /// 整体拉伸、不切片——覆盖图（layer）与绝大多数调用的形态。
    pub const fn stretch() -> Self {
        Self {
            mode: MODE_STRETCH,
            slice: [0; 4],
            repeat: [false; 2],
            scale_milli: 1000,
        }
    }
}

/// stretch 的模式码。单独具名是为了让 [`FillSpec::stretch`] 不必把编码表抄第二份——
/// 抄了的话，改 [`mode_code`] 时编译器不会提醒那一处。
const MODE_STRETCH: u8 = 0;

/// SVG 是否按「文档尺寸 × DPI」单元铺（center / tile），即 `scale_milli` 是否有意义。
fn is_unit_mode(mode: u8) -> bool {
    mode == mode_code("center") || mode == mode_code("tile")
}

/// 填充模式码：0=stretch（默认）1=nine_slice 2=tile 3=center。
pub fn mode_code(mode: &str) -> u8 {
    match mode {
        "nine_slice" => 1,
        "tile" => 2,
        "center" => 3,
        _ => MODE_STRETCH,
    }
}

/// 填充位图的字节预算。
///
/// 候选窗的宽度随候选内容每次都在变，而填充缓存以**目标宽高**为键——宽度每变一个
/// 像素就是一条新条目。实测宽 200→800、高 40 这一段就是 45.9 MB，只涨不落。
/// 超预算即整体清空（同 `view.rs` 的 `SHADOW_CACHE`）：不做 LRU，因为淘汰谁都得重建，
/// 而重建一张候选窗尺寸的填充只要 0.3 ms，不值得为此维护一份访问序。
///
/// 取值两头受夹：
/// - **下界**：必须显著大于一帧的工作集，否则清空会在同一帧内反复发生，把「只费内存」
///   变成「每帧重建」。一帧的工作集不受本模块控制——layer 未写 size 时按原图尺寸填充
///   （`view.rs` 的 `paint_layer`），再乘 DPI scale，单张大图就能吃掉一大截。
/// - **上界**：定得太大，回收就白做了。实测（宽 200→800 扫一遍再回收，Linux glibc）：
///   8 MiB 峰值 15.4 → 回收后 6.1、16 MiB 峰值 23.6 → 回收后 8.0，都完整落回起点；
///   而 **32 MiB 峰值 39.4 → 回收后只到 36.5**——越过了 glibc 自动 trim 的门槛，内存
///   虽已释放却不再归还 OS（`malloc_trim(0)` 能把它压回 5.8 MiB，印证确实只是没归还）。
///
/// 16 MiB 落在两者之间：装得下「一张 4 MiB 的 layer + 一帧候选窗（约 600 KiB）」还有
/// 富余，又仍在会自动归还的那一侧。
const FILL_BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// 解码源图的字节预算。一张 985×255 的背景图解出来约 1 MB，够放几十张。
const SRC_BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// 闲置多久后整个丢掉。
///
/// 预算只挡住「涨到多高」，挡不住「不用了也不落」——不打字的时候候选窗背景图仍占着
/// 那几 MB。这条负责让它回落到零。30 秒：比一次连续输入的间隙长得多（重建代价才回来），
/// 又短到用户端起茶杯的工夫内存就还回去了。
pub const IDLE_EVICT_AFTER: Duration = Duration::from_secs(30);

/// 动画装饰的帧预算（全部动画合计）。单个动画按调用方给的份额隔帧丢弃，见 [`crate::skin::FrameBudget`]。
pub const ANIM_BUDGET_BYTES: usize = 24 * 1024 * 1024;

/// 动画装饰：逐帧按倍率缩好、合成到目标尺寸的位图（BGRA 序 + 预乘）与各帧时长（毫秒）。
struct Anim {
    frames: Vec<Pixmap>,
    durations: Vec<u32>,
    /// 降帧步长、原始帧数与映射是否可靠，见 [`AnimLayout`]。
    stride: usize,
    raw_frames: usize,
    aligned: bool,
}

/// 动画的时间轴：保留下来的各帧时长（毫秒），以及原始帧号到保留帧的映射
/// （原始第 `i` 帧 = 保留的第 `i / stride` 帧）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimLayout {
    pub durations: Vec<u32>,
    pub stride: usize,
    pub raw_frames: usize,
    /// false = 有帧解码 / 缩放失败、保留帧前移，原始帧号映射不可靠（帧区间片段按整段播）。
    pub aligned: bool,
}

/// 同一源下的动画键：倍率百万分值、目标宽高、fps 覆盖的位模式（0 = 不覆盖）、帧预算份额。
type AnimParams = (u32, u32, u32, u32, usize);

/// 动画帧的闲置回收：远长于 [`IDLE_EVICT_AFTER`]。建帧要整份解码加逐帧缩放，随 30 秒回收清掉的话
/// 每段打字的首键都要在 UI 线程上重建一遍。
pub const ANIM_IDLE_EVICT_AFTER: Duration = Duration::from_secs(10 * 60);

/// 判断动画格式只读源的前这么多字节。
const SNIFF_BYTES: usize = 64 * 1024;

/// 解码后的源图（unpremult RGBA8，row-major）。
struct Src {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// 图片源的内部句柄：把源字符串驻留成一个小整数，缓存键只带它。
///
/// 源字符串可能是一条数百 KB 的 `data:` URI，而 `fill` 是每次重绘都要查一次的路径：
/// 拿它直接作键，每次命中都要重新分配并拷贝整条 URI（旧实现的 `path.to_string()` 正是
/// 如此）。驻留后每次仍要对源串哈希一遍（省掉的是那次拷贝，不是数量级），彻底摘掉得
/// 在主题求值期就把句柄解析好塞进 `RvImage`，那是另一件事。
///
/// 用驻留句柄而非内容哈希：`ImgId` 是精确相等的，不存在两张图撞到同一张的可能。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct ImgId(u32);

type FillKey = (ImgId, FillSpec, u32, u32, [u8; 4]);

/// 皮肤位图键：源、倍率百万分值、目标宽高、两轴拉伸声明的摘要。
type SkinFillKey = (ImgId, u32, u32, u32, u64);

/// 按倍率缩好的源图（预乘 RGBA8）。
struct Atlas {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// 两轴拉伸声明的摘要（FNV-1a over 位模式），进缓存键用。
fn stretch_digest(x: &wind_theme::skin::Stretch, y: &wind_theme::skin::Stretch) -> u64 {
    use wind_theme::skin::{BandFill, Stretch};
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |v: u32| {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    let fill_code = |f: BandFill| match f {
        BandFill::Stretch => 1,
        BandFill::Tile => 2,
        BandFill::Round => 3,
    };
    for s in [x, y] {
        match s {
            Stretch::Whole(f) => {
                eat(10);
                eat(fill_code(*f));
            }
            Stretch::Fixed => eat(11),
            Stretch::Bands(b) => {
                eat(12);
                for band in b {
                    eat(band.from.to_bits());
                    eat(band.to.to_bits());
                    eat(fill_code(band.fill));
                }
            }
        }
        eat(0xFFFF);
    }
    h
}

#[derive(Default)]
pub struct ImageCache {
    ids: HashMap<String, ImgId>,
    /// 下一个待分配句柄。**必须与 `ids` 的表长解耦**：三张表现在都不回收，用表长当
    /// id 恰好也唯一，但一旦给 `ids` 加上回收，id 就会被复用，而 `src`/`fills` 里按旧
    /// id 索引的条目还在——背景图会串成另一张，且一声不响。
    next_id: u32,
    src: HashMap<ImgId, Option<Src>>,
    /// SVG 源的文档尺寸。SVG 没有位图源进不了 `src`，而 layer 未写 size 时每次重绘都
    /// 要问一遍原始尺寸，不缓存就是每次重新解析一遍 SVG。
    svg_sizes: HashMap<ImgId, Option<(u32, u32)>>,
    fills: HashMap<FillKey, Option<Pixmap>>,
    /// 皮肤图集：源图按倍率缩好的预乘 RGBA（键 = 源 + 倍率百万分值），计入 `src_bytes`。
    /// 百万分而非千分：装饰按目标宽高反推倍率，千分取整会让 3000px 的图缩到 1001 时图集成 1002，
    /// 再整轴拉伸一次就发虚。
    /// 图集建好即丢掉对应的源图——皮肤源图通常是几 MB 的大图，常驻的只该是目标尺寸那份。
    atlases: HashMap<(ImgId, u32), Option<Atlas>>,
    /// 源图尺寸。皮肤路径建完图集后丢源图，尺寸仍要能答。
    dims: HashMap<ImgId, (u32, u32)>,
    /// 皮肤窗体 / 装饰的目标位图，计入 `fill_bytes`。
    skin_fills: HashMap<SkinFillKey, Option<Pixmap>>,
    /// 以下动画三表按源串索引、不随 30 秒闲置回收（那会清掉 `ids`），只在换主题、预算溢出、
    /// 闲置 [`ANIM_IDLE_EVICT_AFTER`] 时清。
    /// 多帧动画源（APNG / GIF / 动态 WebP 且至少两帧）。
    animated: std::collections::HashSet<String>,
    /// 动画装饰的逐帧位图，计入 `anim_bytes`；None = 解出来不足两帧（按静态图画）或解码失败。
    anims: HashMap<String, HashMap<AnimParams, Option<Anim>>>,
    anim_bytes: usize,
    anim_last_use: Option<Instant>,
    /// `src` / `fills` 各自的驻留字节，对账两个预算。
    ///
    /// ⚠️ 两个计数**只在整表清空时归零**，精确性依赖「从不单独删某一条」。谁要加定向
    /// 淘汰，必须同时把该条的字节减回去，否则计数会静默失同步、预算随之失效。
    ///
    /// 两个预算都不含 `ids`（驻留源串，一条内嵌 `data:` URI 就是几百 KB）与 `svg_sizes`：
    /// 它们只在闲置回收时才缩。这是有意的——`ids` 的条数被「主题里有几张图」界住，
    /// 真正会随宽度爆炸的是 `fills`。
    src_bytes: usize,
    fill_bytes: usize,
    /// 已判定不是动画的源（随 `ids` 一起回收，不为静态图常驻源串）。
    not_animated: std::collections::HashSet<ImgId>,
    /// 最后一次被取用的时刻；None = 空缓存，没有什么可回收。
    last_use: Option<Instant>,
}

impl ImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一次取用，闲置回收的计时从这里重新起算。
    ///
    /// **命中也要刷新**，不只是未命中时：只在首次绘制打点的话，连续打字的会话会在开始
    /// 后 30 秒被整个回收一次，紧接着又全部重建——内存刚还回去就要回来，还白白吃一次
    /// 重建风暴。
    fn touch(&mut self) {
        self.touch_at(crate::clock::now());
    }

    /// 打点到指定时刻（测试用它陈述「最后一次取用是何时」）。
    fn touch_at(&mut self, now: Instant) {
        self.last_use = Some(now);
    }

    /// 仅测试可见：句柄表条目数（缓存里登记过多少个源）。
    #[cfg(any(test, feature = "test-util"))]
    pub fn ids_len_for_test(&self) -> usize {
        self.ids.len()
    }

    /// 仅测试可见：把最后取用时刻挪到过去，模拟闲置。
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_last_use_for_test(&mut self, now: Instant) {
        self.touch_at(now);
    }

    /// 闲置回收的到期时刻；None = 无需唤醒。
    ///
    /// ⚠ 调用方（UI 消息循环）必须把它登记进 `next_deadline`，否则线程睡下去就再也不会
    /// 来收这份内存——闲置回收只在有别的事把线程叫醒时才碰巧发生。
    pub fn next_deadline(&self) -> Option<Instant> {
        let main = self.last_use.map(|t| t + IDLE_EVICT_AFTER);
        let anim = self.anim_last_use.map(|t| t + ANIM_IDLE_EVICT_AFTER);
        main.into_iter().chain(anim).min()
    }

    /// 丢掉全部动画帧（换主题时）。
    pub fn clear_anims(&mut self) {
        self.animated.clear();
        self.not_animated.clear();
        self.anims.clear();
        self.anim_bytes = 0;
        self.anim_last_use = None;
    }

    fn touch_anim(&mut self) {
        self.anim_last_use = Some(crate::clock::now());
    }

    /// 仅测试可见：把动画表的最后取用时刻挪到过去。
    #[cfg(test)]
    fn set_anim_last_use_for_test(&mut self, now: Instant) {
        self.anim_last_use = Some(now);
    }

    /// 闲置够久就整个丢掉，返回是否真的清了。
    ///
    /// 连驻留表 `ids` 一起清：它存的是完整源串，一条内嵌 `data:` URI 就是几百 KB。
    /// `next_id` **不**跟着回退——句柄一旦复用，`src`/`fills` 里按旧 id 索引的残留就会
    /// 串成另一张图。这里全表一起清本不会留下残留，但那个不变量不该依赖「调用顺序恰好
    /// 安全」来维持。
    pub fn evict_if_idle(&mut self, now: Instant) -> bool {
        let anim = self
            .anim_last_use
            .is_some_and(|t| now.saturating_duration_since(t) >= ANIM_IDLE_EVICT_AFTER);
        if anim {
            self.clear_anims();
        }
        let Some(last) = self.last_use else {
            return anim;
        };
        if now.saturating_duration_since(last) < IDLE_EVICT_AFTER {
            return anim;
        }
        self.ids.clear();
        self.src.clear();
        self.svg_sizes.clear();
        self.fills.clear();
        self.atlases.clear();
        self.dims.clear();
        self.skin_fills.clear();
        self.not_animated.clear();
        self.src_bytes = 0;
        self.fill_bytes = 0;
        self.last_use = None;
        true
    }

    /// 仅测试可见：填充位图的条目数与驻留字节。
    #[cfg(test)]
    fn fill_stats(&self) -> (usize, usize) {
        (self.fills.len(), self.fill_bytes)
    }

    /// 仅测试可见：解码源图的条目数与驻留字节。
    #[cfg(test)]
    fn src_stats(&self) -> (usize, usize) {
        (self.src.len(), self.src_bytes)
    }

    /// 仅测试可见：**全部**驻留 —— 回收测试只盯 `fills` 的话，清 `ids`/`src`/`svg_sizes`
    /// 那几行删掉也照样绿，而它们才是内存回落的大头。
    #[cfg(test)]
    fn residency(&self) -> (usize, usize, usize, usize, usize, usize) {
        (
            self.ids.len(),
            self.src.len(),
            self.svg_sizes.len(),
            self.fills.len(),
            self.src_bytes,
            self.fill_bytes,
        )
    }

    /// 源字符串 → 驻留句柄（首见时登记）。
    fn id_of(&mut self, src: &str) -> ImgId {
        if let Some(id) = self.ids.get(src) {
            return *id;
        }
        let id = ImgId(self.next_id);
        // 回绕就是句柄复用，也就是上面那条不变量的反面。够不到，但它是承重的，配一行执行。
        self.next_id = self.next_id.checked_add(1).expect("ImgId 句柄用尽");
        self.ids.insert(src.to_string(), id);
        id
    }

    /// 解码源图（缓存；失败缓存 None 避免反复重试）。
    fn decode(&mut self, id: ImgId, src: &str) -> Option<&Src> {
        if !self.src.contains_key(&id) {
            let decoded = decode_bitmap(src).map(|img| {
                let rgba = img.to_rgba8();
                Src {
                    w: rgba.width(),
                    h: rgba.height(),
                    rgba: rgba.into_raw(),
                }
            });
            if decoded.is_none() {
                tracing::warn!("主题背景图解码失败: {}", brief(src));
            }
            let bytes = decoded.as_ref().map_or(0, |s| s.rgba.len());
            // 先腾地方再放：清空后仍超预算说明单张就超了，仍旧放进去——功能优先，
            // 下一张进来时再清一次。
            if self.src_bytes + bytes > SRC_BUDGET_BYTES {
                self.src.clear();
                self.atlases.clear();
                self.src_bytes = 0;
            }
            self.src_bytes += bytes;
            self.src.insert(id, decoded);
        }
        self.src.get(&id).and_then(|o| o.as_ref())
    }

    /// 源图原始尺寸（用于 layer size=0 时取原尺寸）。
    ///
    /// SVG 单独一条路：它进不了 `src`（没有位图可解码），而调用方拿不到尺寸就整层不
    /// 画——`.svg` 文件早先就是这样，内嵌 SVG 放行后这个洞会落到市场主题上。
    pub fn src_size(&mut self, path: &str) -> Option<(u32, u32)> {
        self.touch();
        let id = self.id_of(path);
        if is_svg(path) {
            return *self.svg_sizes.entry(id).or_insert_with(|| {
                let size = svg_tree(path).map(|t| {
                    let s = t.size();
                    (s.width().ceil() as u32, s.height().ceil() as u32)
                });
                if size.is_none() {
                    tracing::warn!("主题背景图（SVG）解析失败: {}", brief(path));
                }
                size
            });
        }
        if let Some(d) = self.dims.get(&id) {
            return Some(*d);
        }
        // 动画只读头取尺寸：逐帧解码另走 `build_anim`，原分辨率首帧不该常驻 `src`。
        let d = if self.is_animated(path) {
            header_dims(path)?
        } else {
            self.decode(id, path).map(|s| (s.w, s.h))?
        };
        self.dims.insert(id, d);
        Some(d)
    }

    /// 皮肤图集：源图按倍率 `k_micro / 1e6` 缩好的预乘 RGBA。建好后丢掉源图。
    /// SVG 不走这里（皮肤只收位图；SVG 窗体请用旧的 `nine_slice` 之外的整图拉伸）。
    fn atlas(&mut self, id: ImgId, path: &str, k_micro: u32) -> Option<&Atlas> {
        let key = (id, k_micro);
        if !self.atlases.contains_key(&key) {
            let k = k_micro as f32 / 1_000_000.0;
            let built = self.decode(id, path).and_then(|s| {
                let (w, h) = (
                    crate::skin::atlas_len(s.w, k),
                    crate::skin::atlas_len(s.h, k),
                );
                if !crate::skin::fits_image_limits(w, h) {
                    tracing::warn!("skin: 图集 {w}x{h} 超出上限，不建: {}", brief(path));
                    return None;
                }
                Some((
                    (s.w, s.h),
                    Atlas {
                        w,
                        h,
                        rgba: crate::skin::resample(&s.rgba, s.w, s.h, w, h),
                    },
                ))
            });
            let built = built.map(|(dims, atlas)| {
                self.dims.insert(id, dims);
                // 丢源图，字节同步减回（见 `src_bytes` 的不变量说明）。
                if let Some(Some(src)) = self.src.remove(&id) {
                    self.src_bytes = self.src_bytes.saturating_sub(src.rgba.len());
                }
                atlas
            });
            let bytes = built.as_ref().map_or(0, |a| a.rgba.len());
            if self.src_bytes + bytes > SRC_BUDGET_BYTES {
                self.src.clear();
                self.atlases.clear();
                self.src_bytes = 0;
            }
            self.src_bytes += bytes;
            self.atlases.insert(key, built);
        }
        self.atlases.get(&key).and_then(|o| o.as_ref())
    }

    /// 皮肤位图（BGRA 序 + 预乘）：源图按倍率 `k` 缩成图集，再按两轴拉伸声明（源像素）
    /// 合成 `w×h`。窗体与装饰共用（装饰传整轴拉伸）。
    pub fn skin_fill(
        &mut self,
        path: &str,
        k: f32,
        stretch_x: &wind_theme::skin::Stretch,
        stretch_y: &wind_theme::skin::Stretch,
        w: u32,
        h: u32,
    ) -> Option<&Pixmap> {
        self.touch();
        if w == 0 || h == 0 || !k.is_finite() || k <= 0.0 || is_svg(path) {
            return None;
        }
        let id = self.id_of(path);
        let k_micro = ((k * 1_000_000.0).round() as u32).max(1);
        let key = (id, k_micro, w, h, stretch_digest(stretch_x, stretch_y));
        if !self.skin_fills.contains_key(&key) {
            let (sw, sh) = self.src_size(path)?;
            let built = self
                .atlas(id, path, k_micro)
                .and_then(|a| atlas_pixmap(a, (sw, sh), stretch_x, stretch_y, w, h));
            let bytes = built.as_ref().map_or(0, |pm| pm.data().len());
            if self.fill_bytes + bytes > FILL_BUDGET_BYTES {
                self.fills.clear();
                self.skin_fills.clear();
                self.fill_bytes = 0;
            }
            self.fill_bytes += bytes;
            self.skin_fills.insert(key, built);
        }
        self.skin_fills.get(&key).and_then(|o| o.as_ref())
    }

    /// 源是否多帧动画（结果缓存）：先只读文件头判格式，是动画格式再试解前两帧，单帧 GIF 不算。
    pub fn is_animated(&mut self, path: &str) -> bool {
        if self.animated.contains(path) {
            return true;
        }
        let id = self.id_of(path);
        if self.not_animated.contains(&id) {
            return false;
        }
        let a = !is_svg(path)
            && read_source_prefix(path, SNIFF_BYTES).is_some_and(|b| sniff_animated(&b))
            && read_source_bytes(path)
                .and_then(|b| frames_of(&b).map(|f| f.take(2).filter(Result::is_ok).count()))
                .is_some_and(|n| n >= 2);
        if a {
            self.animated.insert(path.to_string());
        } else {
            self.not_animated.insert(id);
        }
        a
    }

    /// 动画装饰：按倍率 `k` 逐帧缩好、合成 `w×h`，帧数按 `budget` 字节隔帧丢弃。
    /// 不足两帧（含单帧 GIF）或解码失败返回 None，调用方按静态图画。
    fn anim(
        &mut self,
        path: &str,
        k: f32,
        w: u32,
        h: u32,
        fps: Option<f32>,
        budget: usize,
    ) -> Option<&Anim> {
        self.touch_anim();
        if w == 0 || h == 0 || !k.is_finite() || k <= 0.0 {
            return None;
        }
        let k_micro = ((k * 1_000_000.0).round() as u32).max(1);
        let key = (k_micro, w, h, fps.map_or(0, f32::to_bits), budget);
        if !self.anims.get(path).is_some_and(|m| m.contains_key(&key)) {
            let built = build_anim(path, k_micro as f32 / 1_000_000.0, w, h, fps, budget);
            let bytes = built
                .as_ref()
                .map_or(0, |a| a.frames.iter().map(|f| f.data().len()).sum());
            if self.anim_bytes + bytes > ANIM_BUDGET_BYTES {
                self.anims.clear();
                self.anim_bytes = 0;
            }
            self.anim_bytes += bytes;
            self.anims
                .entry(path.to_string())
                .or_default()
                .insert(key, built);
        }
        self.anims
            .get(path)
            .and_then(|m| m.get(&key))
            .and_then(|o| o.as_ref())
    }

    /// 动画装饰各帧时长（毫秒，已归一化）；不是动画（不足两帧）为 None。
    pub fn anim_durations(
        &mut self,
        path: &str,
        k: f32,
        w: u32,
        h: u32,
        fps: Option<f32>,
        budget: usize,
    ) -> Option<&[u32]> {
        self.anim(path, k, w, h, fps, budget)
            .map(|a| a.durations.as_slice())
    }

    /// 同 [`Self::anim_durations`]，另带降帧映射。
    pub fn anim_layout(
        &mut self,
        path: &str,
        k: f32,
        w: u32,
        h: u32,
        fps: Option<f32>,
        budget: usize,
    ) -> Option<AnimLayout> {
        self.anim(path, k, w, h, fps, budget).map(|a| AnimLayout {
            durations: a.durations.clone(),
            stride: a.stride,
            raw_frames: a.raw_frames,
            aligned: a.aligned,
        })
    }

    /// 动画装饰的第 `idx` 帧（对帧数取模）。
    #[allow(clippy::too_many_arguments)]
    pub fn anim_frame(
        &mut self,
        path: &str,
        k: f32,
        w: u32,
        h: u32,
        fps: Option<f32>,
        budget: usize,
        idx: usize,
    ) -> Option<&Pixmap> {
        let a = self.anim(path, k, w, h, fps, budget)?;
        a.frames.get(idx % a.frames.len())
    }

    /// 皮肤某轴的最小目标长度与「倍率下的自然长度」（设备像素）：`(min_w, min_h, nat_w, nat_h)`。
    /// 只要源图尺寸，不建图集。
    pub fn skin_metrics(
        &mut self,
        path: &str,
        k: f32,
        stretch_x: &wind_theme::skin::Stretch,
        stretch_y: &wind_theme::skin::Stretch,
    ) -> Option<(u32, u32, u32, u32)> {
        use crate::skin::{atlas_len, min_len, src_segments};
        self.touch();
        let (sw, sh) = self.src_size(path)?;
        let k_micro = ((k * 1_000_000.0).round() as u32).max(1);
        let k = k_micro as f32 / 1_000_000.0;
        let (aw, ah) = (atlas_len(sw, k), atlas_len(sh, k));
        Some((
            min_len(&src_segments(stretch_x, sw as f32), sw as f32, aw),
            min_len(&src_segments(stretch_y, sh as f32), sh as f32, ah),
            aw,
            ah,
        ))
    }

    /// 取（或构建）目标尺寸填充位图（BGRA 序 + 预乘）。
    /// tint=[0,0,0,0] 表示不染色；非零时把图当 alpha mask、用 tint 色填充（单色 SVG/图标随主题变色）。
    pub fn fill(
        &mut self,
        path: &str,
        spec: FillSpec,
        w: u32,
        h: u32,
        tint: [u8; 4],
    ) -> Option<&Pixmap> {
        self.touch();
        // scale 只决定 SVG center / tile 的单元尺寸；其余情形归一，免得每个 DPI 各驻留一份
        // 一模一样的填充。
        let spec = if is_svg(path) && is_unit_mode(spec.mode) {
            spec
        } else {
            FillSpec {
                scale_milli: 1000,
                ..spec
            }
        };
        let key = (self.id_of(path), spec, w, h, tint);
        if !self.fills.contains_key(&key) {
            let built = self.build_fill(key, path);
            let bytes = built.as_ref().map_or(0, |pm| pm.data().len());
            if self.fill_bytes + bytes > FILL_BUDGET_BYTES {
                self.fills.clear();
                self.skin_fills.clear();
                self.fill_bytes = 0;
            }
            self.fill_bytes += bytes;
            self.fills.insert(key, built);
        }
        self.fills.get(&key).and_then(|o| o.as_ref())
    }

    fn build_fill(&mut self, key: FillKey, path: &str) -> Option<Pixmap> {
        let (id, spec, w, h, tint) = key;
        if w == 0 || h == 0 || !crate::skin::fits_image_limits(w, h) {
            return None;
        }
        let mut pm = Pixmap::new(w, h)?;
        if is_svg(path) {
            // SVG：resvg 输出预乘 RGBA，逐像素 tint/直通 + R/B 交换。
            // 告警与位图侧对齐：静默画不出来正是这次要消灭的病症，别在这一侧留一份。
            let fail = || tracing::warn!("主题背景图（SVG）栅格化失败: {}", brief(path));
            if is_unit_mode(spec.mode) {
                // center / tile：以「文档尺寸 × DPI」为单元（见 rasterize_svg_unit），
                // 平铺沿用位图同一套采样。stretch / nine_slice 走下面的整盒拉伸（矢量切九宫意义不大）。
                let k = spec.scale_milli as f32 / 1000.0;
                let tile = spec.mode == mode_code("tile");
                let Some((unit, sw, sh)) = rasterize_svg_unit(path, tile, k, w, h) else {
                    fail();
                    return None;
                };
                let tile_spec = FillSpec {
                    mode: mode_code("tile"),
                    ..spec
                };
                // center 已渲成 w×h 成品：按 tile 采样即 1:1 直拷（sw=w、sh=h）。
                sample_into(
                    pm.pixels_mut(),
                    tile_spec,
                    (sw, sh, unit.data()),
                    w,
                    h,
                    tint,
                    true,
                );
                return Some(pm);
            }
            let Some(rendered) = rasterize_svg(path, w, h) else {
                fail();
                return None;
            };
            let rgba = rendered.data();
            let px = pm.pixels_mut();
            for (i, p) in px.iter_mut().enumerate() {
                let b = i * 4;
                *p = compose(rgba[b], rgba[b + 1], rgba[b + 2], rgba[b + 3], tint, true);
            }
            return Some(pm);
        }
        // 位图：image 解码（未预乘）→ 按模式采样 → tint/预乘 + R/B 交换。
        let src = self.decode(id, path)?;
        if src.w == 0 || src.h == 0 {
            return None;
        }
        sample_into(
            pm.pixels_mut(),
            spec,
            (src.w, src.h, &src.rgba),
            w,
            h,
            tint,
            false,
        );
        Some(pm)
    }
}

/// 图集（源图 `sw×sh` 按倍率缩好）按两轴拉伸声明合成 `w×h` 的 BGRA 预乘位图。
fn atlas_pixmap(
    a: &Atlas,
    (sw, sh): (u32, u32),
    stretch_x: &wind_theme::skin::Stretch,
    stretch_y: &wind_theme::skin::Stretch,
    w: u32,
    h: u32,
) -> Option<Pixmap> {
    use crate::skin::{compose, map_axis, src_segments, taps};
    let xs = taps(&map_axis(
        &src_segments(stretch_x, sw as f32),
        sw as f32,
        a.w,
        w,
    ));
    let ys = taps(&map_axis(
        &src_segments(stretch_y, sh as f32),
        sh as f32,
        a.h,
        h,
    ));
    if !crate::skin::fits_image_limits(w, h) {
        return None;
    }
    let rgba = compose(&a.rgba, a.w, &xs, &ys);
    let mut pm = Pixmap::new(w, h)?;
    for (p, c) in pm.pixels_mut().iter_mut().zip(rgba.as_chunks::<4>().0) {
        // 已预乘；R/B 交换以适配 BGRA 缓冲。
        *p = PremultipliedColorU8::from_rgba(c[2], c[1], c[0], c[3]).unwrap_or_else(transparent);
    }
    Some(pm)
}

/// 源的前 `n` 字节（`data:` URI 只解对应长度的 base64）。
fn read_source_prefix(src: &str, n: usize) -> Option<Vec<u8>> {
    use base64::Engine as _;
    use std::io::Read as _;
    if !src.starts_with("data:") {
        let mut out = Vec::new();
        std::fs::File::open(src)
            .ok()?
            .take(n as u64)
            .read_to_end(&mut out)
            .ok()?;
        return Some(out);
    }
    let (head, payload) = src.strip_prefix("data:")?.split_once(',')?;
    if !head.split(';').any(|p| p.eq_ignore_ascii_case("base64")) {
        return None;
    }
    let want = n.div_ceil(3) * 4;
    let mut cleaned: String = payload
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(want)
        .collect();
    // 截断处须落在 4 字符组边界上。
    if cleaned.len() == want {
        cleaned.truncate(want - want % 4);
    }
    base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .ok()
}

/// 文件头是否动画格式：GIF 一律算（帧数另判）；APNG 看 IDAT 之前有无 acTL；WebP 看 VP8X 的动画位。
fn sniff_animated(b: &[u8]) -> bool {
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

/// 只解析文件头取尺寸。
fn header_dims(src: &str) -> Option<(u32, u32)> {
    let bytes = read_source_bytes(src)?;
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// 动画格式（APNG / GIF / 动态 WebP）的逐帧解码器（帧已合成到整张画布）；静态图为 None。
fn frames_of(bytes: &[u8]) -> Option<image::Frames<'_>> {
    use image::codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder};
    use image::{AnimationDecoder, ImageDecoder};
    use std::io::Cursor;
    match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => {
            let d = PngDecoder::with_limits(Cursor::new(bytes), decode_limits()).ok()?;
            if !d.is_apng().ok()? {
                return None;
            }
            Some(d.apng().ok()?.into_frames())
        }
        image::ImageFormat::Gif => {
            let mut d = GifDecoder::new(Cursor::new(bytes)).ok()?;
            d.set_limits(decode_limits()).ok()?;
            Some(d.into_frames())
        }
        image::ImageFormat::WebP => {
            let mut d = WebPDecoder::new(Cursor::new(bytes)).ok()?;
            d.set_limits(decode_limits()).ok()?;
            d.has_animation().then(|| d.into_frames())
        }
        _ => None,
    }
}

/// 逐帧解码动画，每帧与静态装饰同一套缩放合成（面积平均缩到倍率 `k`，再整轴映射到 `w×h`）。
fn build_anim(path: &str, k: f32, w: u32, h: u32, fps: Option<f32>, budget: usize) -> Option<Anim> {
    use wind_theme::skin::{BandFill, Stretch};
    let t0 = crate::clock::now();
    let bytes = read_source_bytes(path)?;
    let frames = frames_of(&bytes)?;
    let whole = Stretch::Whole(BandFill::Stretch);
    let mut acc = crate::skin::FrameBudget::new(w as usize * h as usize * 4, budget);
    if !header_dims(path).is_some_and(|(sw, sh)| crate::skin::fits_image_limits(sw, sh))
        || !crate::skin::fits_image_limits(w, h)
    {
        tracing::warn!("skin: 动画装饰尺寸超出上限，不解帧: {}", brief(path));
        return None;
    }
    for frame in frames.take(MAX_ANIM_FRAMES) {
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "skin: 动画帧解码失败，只用已解出的帧: {} ({e})",
                    brief(path)
                );
                break;
            }
        };
        let (numer, denom) = frame.delay().numer_denom_ms();
        acc.push_with(crate::skin::frame_ms(numer, denom, fps), || {
            let buf = frame.buffer();
            let (sw, sh) = buf.dimensions();
            let (aw, ah) = (crate::skin::atlas_len(sw, k), crate::skin::atlas_len(sh, k));
            let atlas = Atlas {
                w: aw,
                h: ah,
                rgba: crate::skin::resample(buf.as_raw(), sw, sh, aw, ah),
            };
            atlas_pixmap(&atlas, (sw, sh), &whole, &whole, w, h)
        });
    }
    let dropped = acc.dropped();
    let (stride, raw_frames, aligned) = (acc.stride(), acc.seen(), acc.aligned());
    let (frames, durations): (Vec<Pixmap>, Vec<u32>) = acc.finish().into_iter().unzip();
    tracing::debug!(
        "skin: 动画建帧 {} 帧 {w}x{h}，耗时 {:?}",
        frames.len(),
        crate::clock::now().saturating_duration_since(t0)
    );
    if frames.len() < 2 {
        return None;
    }
    if dropped {
        tracing::warn!(
            "skin: 动画帧超出 {} KiB 预算，隔帧丢弃到 {} 帧: {}",
            budget / 1024,
            frames.len(),
            brief(path)
        );
    }
    Some(Anim {
        frames,
        durations,
        stride,
        raw_frames,
        aligned,
    })
}

/// 按 `spec` 把源图（`(sw, sh, RGBA8)`，`premult` 标明是否已预乘）采样进 `w×h` 的目标像素。
/// 位图各模式与 SVG 的 center / tile（单元已栅格化好）共用；None 处保持透明（Pixmap::new 已清零）。
fn sample_into(
    px: &mut [PremultipliedColorU8],
    spec: FillSpec,
    (sw, sh, data): (u32, u32, &[u8]),
    w: u32,
    h: u32,
    tint: [u8; 4],
    premult: bool,
) {
    for dy in 0..h {
        for dx in 0..w {
            let Some((sx, sy)) = map_src(spec, sw, sh, w, h, dx, dy) else {
                continue;
            };
            let si = ((sy * sw + sx) * 4) as usize;
            px[(dy * w + dx) as usize] = compose(
                data[si],
                data[si + 1],
                data[si + 2],
                data[si + 3],
                tint,
                premult,
            );
        }
    }
}

/// 目标像素 (dx,dy) → 源像素坐标；None=该处透明（仅 center 越界）。
#[allow(clippy::too_many_arguments)]
fn map_src(
    spec: FillSpec,
    sw: u32,
    sh: u32,
    w: u32,
    h: u32,
    dx: u32,
    dy: u32,
) -> Option<(u32, u32)> {
    let FillSpec {
        mode,
        slice,
        repeat,
        ..
    } = spec;
    match mode {
        1 => {
            // nine_slice：slice = [上,右,下,左]
            let sx = nine_axis(dx, w, sw, slice[3], slice[1], repeat[0])?;
            let sy = nine_axis(dy, h, sh, slice[0], slice[2], repeat[1])?;
            Some((sx, sy))
        }
        2 => Some((dx % sw, dy % sh)), // tile
        3 => {
            // center：源居中，越界透明
            let offx = (w as i64 - sw as i64) / 2;
            let offy = (h as i64 - sh as i64) / 2;
            let sx = dx as i64 - offx;
            let sy = dy as i64 - offy;
            if sx < 0 || sx >= sw as i64 || sy < 0 || sy >= sh as i64 {
                return None;
            }
            Some((sx as u32, sy as u32))
        }
        _ => {
            // stretch：等比映射到源
            let sx = (dx as u64 * sw as u64 / w as u64).min(sw as u64 - 1) as u32;
            let sy = (dy as u64 * sh as u64 / h as u64).min(sh as u64 - 1) as u32;
            Some((sx, sy))
        }
    }
}

/// 九宫格单轴映射：起/末 `s0`/`s1` 像素 1:1，中段按 `repeat` 平铺或拉伸。
fn nine_axis(d: u32, dlen: u32, slen: u32, s0: u32, s1: u32, repeat: bool) -> Option<u32> {
    // 切片过大时退化为整体拉伸，避免中段为负。
    if s0 + s1 >= slen || s0 + s1 >= dlen {
        return Some((d as u64 * slen as u64 / dlen as u64).min(slen as u64 - 1) as u32);
    }
    if d < s0 {
        Some(d) // 起始固定段
    } else if d >= dlen - s1 {
        Some(slen - (dlen - d)) // 末尾固定段（对齐到源末尾）
    } else {
        // 中段：dest [s0, dlen-s1) → src [s0, slen-s1)
        let dmid = d - s0;
        let dmid_len = dlen - s0 - s1;
        let smid_len = slen - s0 - s1;
        if repeat {
            // 平铺：目标尺寸变化只改变重复次数，纹理本身不缩放——候选窗宽度每变一下
            // 中段就被重新压缩一次（「呼吸」）的根源正是下面那条拉伸。
            Some(s0 + dmid % smid_len)
        } else {
            Some((s0 + (dmid as u64 * smid_len as u64 / dmid_len as u64) as u32).min(slen - 1))
        }
    }
}

/// 测试夹具：现生成的多帧动画（纯色帧，`(RGBA, 帧时长毫秒)`）。
#[cfg(any(test, feature = "test-util"))]
pub mod anim_fixtures {
    /// 多帧 GIF（循环播放）。GIF 帧时长以 10 ms 为单位。
    pub fn gif(w: u32, h: u32, frames: &[([u8; 4], u32)]) -> Vec<u8> {
        use image::codecs::gif::{GifEncoder, Repeat};
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut out);
            enc.set_repeat(Repeat::Infinite).expect("GIF 循环");
            enc.encode_frames(frames.iter().map(|&(c, ms)| {
                image::Frame::from_parts(
                    image::RgbaImage::from_pixel(w, h, image::Rgba(c)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(ms, 1),
                )
            }))
            .expect("GIF 编码");
        }
        out
    }

    /// 多帧 APNG，每帧给整张像素（RGBA8，行优先）。
    pub fn apng(w: u32, h: u32, frames: &[(Vec<u8>, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_animated(frames.len() as u32, 0).expect("APNG 帧数");
            let mut wr = enc.write_header().expect("APNG 头");
            for (px, ms) in frames {
                wr.set_frame_delay(*ms as u16, 1000).expect("APNG 帧时长");
                wr.write_image_data(px).expect("APNG 帧");
            }
            wr.finish().expect("APNG 结尾");
        }
        out
    }

    pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
        use base64::Engine as _;
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    use super::anim_fixtures;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    fn png_uri(w: u32, h: u32) -> String {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(RED));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        anim_fixtures::data_uri("image/png", &out)
    }

    #[test]
    fn decode_rejects_images_over_side_limit() {
        let side = crate::skin::MAX_IMAGE_SIDE;
        assert!(decode_bitmap(&png_uri(side, 1)).is_some(), "上限本身可解");
        assert!(
            decode_bitmap(&png_uri(side + 1, 1)).is_none(),
            "单边超限不解码"
        );
        let mut c = ImageCache::new();
        assert_eq!(c.src_size(&png_uri(side + 1, 1)), None);
    }

    #[test]
    fn animation_decodes_at_most_max_frames() {
        let frames: Vec<_> = (0..MAX_ANIM_FRAMES + 20)
            .map(|i| (vec![(i % 256) as u8, 0, 0, 255], 40))
            .collect();
        let uri = anim_fixtures::data_uri("image/png", &anim_fixtures::apng(1, 1, &frames));
        let mut c = ImageCache::new();
        let l = c
            .anim_layout(&uri, 1.0, 1, 1, None, ANIM_BUDGET_BYTES)
            .expect("动画");
        assert_eq!(l.raw_frames, MAX_ANIM_FRAMES, "超出的帧不解");
    }

    fn gif_uri(frames: &[([u8; 4], u32)]) -> String {
        anim_fixtures::data_uri("image/gif", &anim_fixtures::gif(4, 4, frames))
    }

    /// 第 `idx` 帧左上像素（BGRA 预乘）。
    fn frame_px(c: &mut ImageCache, uri: &str, budget: usize, idx: usize) -> [u8; 4] {
        let pm = c
            .anim_frame(uri, 1.0, 4, 4, None, budget, idx)
            .expect("动画帧");
        let p = pm.pixels()[0];
        [p.red(), p.green(), p.blue(), p.alpha()]
    }

    #[test]
    fn gif_frames_decode_with_normalized_durations() {
        let uri = gif_uri(&[(RED, 0), (GREEN, 50), (BLUE, 20)]);
        let mut c = ImageCache::new();
        assert!(c.is_animated(&uri));
        assert_eq!(
            c.anim_durations(&uri, 1.0, 4, 4, None, ANIM_BUDGET_BYTES),
            Some(&[100, 50, 33][..]),
            "0 ms 按 100 ms，不足 33 ms 钳到帧率上限"
        );
        // BGRA：红帧的蓝通道位是 0、红通道位在第三个。
        assert_eq!(
            frame_px(&mut c, &uri, ANIM_BUDGET_BYTES, 0),
            [0, 0, 255, 255]
        );
        assert_eq!(
            frame_px(&mut c, &uri, ANIM_BUDGET_BYTES, 1),
            [0, 255, 0, 255]
        );
        assert_eq!(
            frame_px(&mut c, &uri, ANIM_BUDGET_BYTES, 5),
            [255, 0, 0, 255],
            "帧号对帧数取模"
        );
    }

    #[test]
    fn fps_overrides_every_frame_duration() {
        let uri = gif_uri(&[(RED, 500), (GREEN, 50)]);
        let mut c = ImageCache::new();
        assert_eq!(
            c.anim_durations(&uri, 1.0, 4, 4, Some(8.0), ANIM_BUDGET_BYTES),
            Some(&[125, 125][..])
        );
    }

    #[test]
    fn over_budget_animation_drops_every_other_frame() {
        // 每帧 4×4×4 = 64 字节，预算 128：4 帧隔帧丢成 2 帧（第 0、2 帧），时长并入前一帧。
        let uri = gif_uri(&[(RED, 100), (GREEN, 200), (BLUE, 300), (WHITE, 400)]);
        let mut c = ImageCache::new();
        assert_eq!(
            c.anim_durations(&uri, 1.0, 4, 4, None, 128),
            Some(&[300, 700][..])
        );
        assert_eq!(
            frame_px(&mut c, &uri, 128, 1),
            [255, 0, 0, 255],
            "留下的是蓝帧"
        );
        assert_eq!(
            c.anim_layout(&uri, 1.0, 4, 4, None, 128),
            Some(AnimLayout {
                durations: vec![300, 700],
                stride: 2,
                raw_frames: 4,
                aligned: true
            }),
            "原始第 i 帧落在保留的第 i/2 帧"
        );
    }

    #[test]
    fn static_and_single_frame_images_are_not_animations() {
        let mut c = ImageCache::new();
        let png = png_data_uri(4, 4, RED);
        assert!(!c.is_animated(&png));
        let one = gif_uri(&[(RED, 100)]);
        assert_eq!(
            c.anim_durations(&one, 1.0, 4, 4, None, ANIM_BUDGET_BYTES),
            None,
            "单帧 GIF 按静态图画"
        );
    }

    #[test]
    fn apng_frames_scale_like_static_images() {
        // 8×2 竖条纹两帧，缩到 k = 0.5（4×1）：每帧与同像素静态 PNG 走静态路径的结果逐字节一致。
        let stripes = |a: [u8; 4], b: [u8; 4]| -> Vec<u8> {
            (0..16)
                .flat_map(|i| if i % 2 == 0 { a } else { b })
                .collect()
        };
        let f0 = stripes(RED, WHITE);
        let f1 = stripes(BLUE, GREEN);
        let uri = anim_fixtures::data_uri(
            "image/png",
            &anim_fixtures::apng(8, 2, &[(f0.clone(), 40), (f1.clone(), 120)]),
        );
        let mut c = ImageCache::new();
        assert!(c.is_animated(&uri));
        assert_eq!(
            c.anim_durations(&uri, 0.5, 4, 1, None, ANIM_BUDGET_BYTES),
            Some(&[40, 120][..])
        );
        let whole = wind_theme::skin::Stretch::Whole(wind_theme::skin::BandFill::Stretch);
        for (i, px) in [f0, f1].into_iter().enumerate() {
            let img = image::RgbaImage::from_raw(8, 2, px).unwrap();
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let still = anim_fixtures::data_uri("image/png", &png.into_inner());
            let want = c
                .skin_fill(&still, 0.5, &whole, &whole, 4, 1)
                .unwrap()
                .data()
                .to_vec();
            let got = c
                .anim_frame(&uri, 0.5, 4, 1, None, ANIM_BUDGET_BYTES, i)
                .unwrap()
                .data()
                .to_vec();
            assert_eq!(got, want, "第 {i} 帧");
        }
    }

    #[test]
    fn animation_frames_outlive_short_idle_eviction() {
        let uri = gif_uri(&[(RED, 100), (GREEN, 100)]);
        let mut c = ImageCache::new();
        c.anim_durations(&uri, 1.0, 4, 4, None, ANIM_BUDGET_BYTES);
        let bytes = c.anim_bytes;
        assert!(bytes > 0);
        let now = Instant::now();
        c.set_last_use_for_test(now - IDLE_EVICT_AFTER);
        assert!(c.evict_if_idle(now));
        assert_eq!(c.anim_bytes, bytes, "30 秒回收不清动画帧");
        assert!(
            c.next_deadline()
                .is_some_and(|t| t >= now + ANIM_IDLE_EVICT_AFTER - Duration::from_secs(1)),
            "仍登记动画帧的长闲置期限"
        );
        c.set_anim_last_use_for_test(now - ANIM_IDLE_EVICT_AFTER);
        assert!(c.evict_if_idle(now));
        assert_eq!((c.anims.len(), c.anim_bytes), (0, 0));
        assert_eq!(c.next_deadline(), None);
    }

    #[test]
    fn clear_anims_drops_frames_for_theme_switch() {
        let uri = gif_uri(&[(RED, 100), (GREEN, 100)]);
        let mut c = ImageCache::new();
        c.anim_durations(&uri, 1.0, 4, 4, None, ANIM_BUDGET_BYTES);
        c.clear_anims();
        assert_eq!((c.anims.len(), c.anim_bytes), (0, 0));
    }

    #[test]
    fn animated_source_is_not_kept_at_full_resolution() {
        let uri = gif_uri(&[(RED, 100), (GREEN, 100)]);
        let mut c = ImageCache::new();
        assert_eq!(c.src_size(&uri), Some((4, 4)), "只读头也拿得到尺寸");
        c.anim_durations(&uri, 1.0, 4, 4, None, ANIM_BUDGET_BYTES);
        assert_eq!(c.src_stats(), (0, 0), "原分辨率首帧不常驻");
    }

    #[test]
    fn sniff_reads_only_format_headers() {
        let gif = anim_fixtures::gif(4, 4, &[(RED, 100), (GREEN, 100)]);
        assert!(sniff_animated(&gif[..8]), "GIF 看签名");
        let still = image::RgbaImage::from_pixel(4, 4, image::Rgba(RED));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(still)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        assert!(!sniff_animated(&png.into_inner()));
        let apng = anim_fixtures::apng(1, 1, &[(RED.to_vec(), 40), (GREEN.to_vec(), 40)]);
        assert!(sniff_animated(&apng[..64]), "APNG 的 acTL 在文件头里");
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0".to_vec();
        webp.extend([0x02, 0, 0, 0]);
        assert!(sniff_animated(&webp));
        webp[20] = 0;
        assert!(!sniff_animated(&webp));
    }

    #[test]
    fn source_prefix_decodes_only_leading_bytes() {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let uri = anim_fixtures::data_uri("image/gif", &bytes);
        let head = read_source_prefix(&uri, 100).unwrap();
        assert!(head.len() >= 100 && head.len() < 110);
        assert_eq!(&head[..], &bytes[..head.len()]);
        let short = anim_fixtures::data_uri("image/gif", &bytes[..10]);
        assert_eq!(read_source_prefix(&short, 100).unwrap(), &bytes[..10]);
    }

    /// 把一张纯色图编码成 `data:image/png;base64,...`——主题编辑器发布图片的形态。
    fn png_data_uri(w: u32, h: u32, rgba: [u8; 4]) -> String {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("PNG 编码");
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
        )
    }

    /// 把一段纯色矩形 SVG 编码成 `data:image/svg+xml;base64,...`。
    fn svg_data_uri(w: u32, h: u32, fill: &str) -> String {
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}"><rect width="{w}" height="{h}" fill="{fill}"/></svg>"##
        );
        format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        )
    }

    /// 一行横向条纹图（每列一个颜色），用来看清像素落到了源图的哪一列。
    fn png_stripes_data_uri(reds: &[u8]) -> String {
        let mut img = image::RgbaImage::new(reds.len() as u32, 1);
        for (x, r) in reds.iter().enumerate() {
            img.put_pixel(x as u32, 0, image::Rgba([*r, 0, 0, 255]));
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("PNG 编码");
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
        )
    }

    /// 读一行填充结果里每个像素对应的源图红色分量。
    /// BGRA 序（见 compose）：源的 R 落在 blue 通道上。
    fn row_reds(pm: &Pixmap, w: u32) -> Vec<u8> {
        (0..w)
            .map(|x| pm.pixel(x, 0).expect("像素").blue())
            .collect()
    }

    /// 编辑器发布的主题把图片内嵌在 theme.toml 里，必须一路解码到像素。
    ///
    /// 这条是「市场主题导入后没有背景图」的回归护栏：曾经解码走的是
    /// `image::open(path)`，拿 data: URI 当文件名打开，必然失败且只留一行 warn。
    #[test]
    fn fill_decodes_inline_png_data_uri() {
        let uri = png_data_uri(2, 2, [255, 0, 0, 255]);
        let mut cache = ImageCache::new();

        assert_eq!(cache.src_size(&uri), Some((2, 2)));
        let pm = cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("内嵌 PNG 应当能填充");
        let px = pm.pixel(0, 0).expect("像素");
        // 输出是 BGRA 序（见 compose）：源的红落在 blue 通道上。
        assert_eq!(
            (px.blue(), px.green(), px.red(), px.alpha()),
            (255, 0, 0, 255)
        );
    }

    /// 内嵌 SVG 同样要认出来：它不以 `.svg` 结尾，只看扩展名会被丢进位图解码器。
    #[test]
    fn fill_decodes_inline_svg_data_uri() {
        let uri = svg_data_uri(4, 4, "#0000ff");
        let mut cache = ImageCache::new();

        let pm = cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("内嵌 SVG 应当能填充");
        let px = pm.pixel(0, 0).expect("像素");
        // BGRA 序：源的蓝落在 red 通道上。
        assert_eq!(
            (px.red(), px.green(), px.blue(), px.alpha()),
            (255, 0, 0, 255)
        );
    }

    /// 缓存键是驻留出来的 `ImgId`，两条不同的 data: URI 必须各自成键，不能互相顶替。
    #[test]
    fn distinct_data_uris_do_not_share_cache() {
        let red = png_data_uri(2, 2, [255, 0, 0, 255]);
        let green = png_data_uri(2, 2, [0, 255, 0, 255]);
        let mut cache = ImageCache::new();

        let r = cache
            .fill(&red, FillSpec::stretch(), 2, 2, [0, 0, 0, 0])
            .expect("红图")
            .pixel(0, 0)
            .expect("像素");
        let g = cache
            .fill(&green, FillSpec::stretch(), 2, 2, [0, 0, 0, 0])
            .expect("绿图")
            .pixel(0, 0)
            .expect("像素");

        assert_eq!((r.blue(), r.green()), (255, 0), "红图被别的图顶替了");
        assert_eq!((g.blue(), g.green()), (0, 255), "绿图被别的图顶替了");
    }

    /// 文件路径是内置主题资产的唯一形态，改造不能碰坏它。
    #[test]
    fn fill_still_reads_file_path_sources() {
        let dir = std::env::temp_dir().join(format!("wind-ui-imgcache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("solid.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .expect("写 PNG");
        let p = path.to_string_lossy().into_owned();

        let mut cache = ImageCache::new();
        assert_eq!(cache.src_size(&p), Some((2, 2)));
        let px = cache
            .fill(&p, FillSpec::stretch(), 2, 2, [0, 0, 0, 0])
            .expect("文件图应当能填充")
            .pixel(0, 0)
            .expect("像素");
        assert_eq!((px.blue(), px.green()), (255, 0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TOML 的多行字符串能让 data: URI 带上换行，base64 解码器不收空白——去空白那步
    /// 是承重的，这条正着测它（删掉那行 filter 就红）。
    #[test]
    fn data_uri_payload_tolerates_embedded_newlines() {
        let uri = png_data_uri(2, 2, [255, 0, 0, 255]);
        let (head, payload) = uri.split_once(",").expect("data: URI");
        let wrapped: String = payload
            .as_bytes()
            .chunks(64)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        let folded = format!("{head},{wrapped}");

        assert!(folded.contains('\n'), "构造的载荷必须真的折了行");
        assert_eq!(ImageCache::new().src_size(&folded), Some((2, 2)));
    }

    /// SVG 没有位图源，原始尺寸只能问文档自己。拿不到尺寸时 `view.rs` 的 layer
    /// 分支会整层不画——`.svg` 文件早先就是这样，内嵌 SVG 不能再走一遍。
    #[test]
    fn src_size_reads_svg_document_size() {
        let mut cache = ImageCache::new();
        assert_eq!(cache.src_size(&svg_data_uri(7, 3, "#0000ff")), Some((7, 3)));
        // 解析不了的 SVG 如实返回 None（并留下告警），不是 0×0。
        assert_eq!(cache.src_size("data:image/svg+xml;base64,####"), None);
    }

    /// 主题可以来自市场，是不可信内容：SVG 里的 `<image href="本地绝对路径">` 绝不能
    /// 被 usvg 读进来渲染（那是 usvg 的默认行为，本模块显式关掉了）。
    #[test]
    fn inline_svg_does_not_load_local_files() {
        let dir = std::env::temp_dir().join(format!("wind-ui-svghref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let secret = dir.join("secret.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]))
            .save(&secret)
            .expect("写 PNG");

        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4"><image xlink:href="{}" width="4" height="4"/></svg>"##,
            secret.to_string_lossy()
        );
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&svg)
        );

        let mut cache = ImageCache::new();
        let pm = cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("SVG 本身仍应栅格化成功（只是那张图不该被读进来）");
        let px = pm.pixel(0, 0).expect("像素");
        assert_eq!(
            px.alpha(),
            0,
            "本地文件被 usvg 读进来渲染了：{:?}",
            (px.red(), px.green(), px.blue(), px.alpha())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 上一条的正面：SVG 里内嵌成 data: 的位图要画出来（主题编辑器就引导作者这么做）。
    /// 它钉的是 resvg 的 `raster-images` feature——关掉后所有位图都静默不画，上一条照样绿。
    #[test]
    fn inline_svg_renders_embedded_data_uri_bitmap() {
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4"><image xlink:href="{}" width="4" height="4"/></svg>"##,
            png_data_uri(4, 4, [255, 0, 0, 255])
        );
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&svg)
        );

        let mut cache = ImageCache::new();
        let pm = cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("内嵌位图的 SVG 应当能填充");
        let px = pm.pixel(1, 1).expect("像素");
        // BGRA 序（见 compose）：源的红落在 blue 通道上。
        assert_eq!(
            (px.blue(), px.green(), px.red(), px.alpha()),
            (255, 0, 0, 255)
        );
    }

    /// 候选窗宽度每变一个像素就是一条新的填充缓存条目，没有上界就只涨不落
    /// （实测宽 200→800、高 40 这一段是 45.9 MB）。超预算必须整体让位。
    #[test]
    fn fills_stay_within_byte_budget() {
        let uri = png_data_uri(2, 2, [255, 0, 0, 255]);
        let mut cache = ImageCache::new();
        // 每张 2048×2048×4 = 16 MiB，预算 16 MiB ⇒ 从第二张起，每张进来前都要先清空。
        let edge = 2048;
        for i in 0..3u8 {
            cache
                .fill(&uri, FillSpec::stretch(), edge, edge, [0, 0, 0, i])
                .expect("填充");
        }
        let (count, bytes) = cache.fill_stats();
        assert_eq!(count, 1, "第三张进来时应已清空前两张，实际留下 {count} 条");
        assert!(
            bytes <= FILL_BUDGET_BYTES,
            "驻留字节 {bytes} 超出预算 {FILL_BUDGET_BYTES}"
        );

        // 命中路径不得再记一次账：记账一旦挪出 `if !contains_key`，每帧重绘都会给
        // fill_bytes 充气，几帧就清一次表——正是这次改动要消灭的那种抖动。
        //
        // 特意用**不会触发清空**的小尺寸：拿上面那种 16 MiB 的大图重复命中的话，
        // 重复记账会一次次撞上预算、清表、重建，十轮下来字节数可能恰好绕回原值，
        // 断言就废了（这条起初正是这么写的，变异测试才照出来）。
        let small = 64;
        cache
            .fill(&uri, FillSpec::stretch(), small, small, [0, 0, 0, 3])
            .expect("首次");
        let before = cache.fill_stats();
        for _ in 0..10 {
            cache
                .fill(&uri, FillSpec::stretch(), small, small, [0, 0, 0, 3])
                .expect("命中");
        }
        assert_eq!(cache.fill_stats(), before, "命中路径重复记账了");
    }

    /// 解码源图侧同样要有上界：一张 985×255 解出来就是 1 MB，主题图多了照样堆。
    #[test]
    fn decoded_sources_stay_within_byte_budget() {
        let mut cache = ImageCache::new();
        // 每张 2048×2048×4 = 16 MiB，预算 32 MiB ⇒ 第三张进来前必须先清空。
        for i in 0..3u8 {
            let uri = png_data_uri(2048, 2048, [i, 0, 0, 255]);
            assert_eq!(cache.src_size(&uri), Some((2048, 2048)));
        }
        let (count, bytes) = cache.src_stats();
        assert_eq!(count, 1, "第三张进来时应已清空前两张，实际留下 {count} 条");
        assert!(
            bytes <= SRC_BUDGET_BYTES,
            "驻留字节 {bytes} 超出预算 {SRC_BUDGET_BYTES}"
        );
    }

    /// 打点必须在**命中时**也刷新。只在未命中时打点的话，连续打字的会话会在开始
    /// 后 30 秒被整个回收，紧接着全部重建——内存刚还回去就要回来。
    #[test]
    fn touch_refreshes_on_cache_hit_too() {
        let uri = png_data_uri(2, 2, [255, 0, 0, 255]);
        let mut cache = ImageCache::new();
        cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("首次");

        let stale = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .expect("单调时钟回拨 10 秒");
        cache.set_last_use_for_test(stale);
        let before = cache.next_deadline().expect("有内容就该有到期时刻");

        cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("命中");

        let after = cache.next_deadline().expect("到期时刻");
        assert!(after > before, "命中没有刷新打点");
    }

    /// 回收边界：判据是「闲置时长 < 阈值才留」，恰好到点的那一刻就该收。
    #[test]
    fn idle_eviction_boundary_is_inclusive() {
        let uri = png_data_uri(2, 2, [255, 0, 0, 255]);
        let mut cache = ImageCache::new();
        cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("填充");

        let t = Instant::now();
        cache.set_last_use_for_test(t);
        assert!(
            !cache.evict_if_idle(t + IDLE_EVICT_AFTER - Duration::from_nanos(1)),
            "差一纳秒就不该收"
        );
        assert!(cache.evict_if_idle(t + IDLE_EVICT_AFTER), "到点就该收");
    }

    /// 预算只挡「涨到多高」，挡不住「不用了也不落」。闲置够久要整个还回去，
    /// 且还回去之后照常工作。
    #[test]
    fn idle_eviction_releases_everything_and_recovers() {
        let uri = png_data_uri(2, 2, [255, 0, 0, 255]);
        let mut cache = ImageCache::new();
        assert_eq!(cache.next_deadline(), None, "空缓存不该要求 UI 线程醒来");

        let t0 = Instant::now();
        // 四张表都得填上：只填 fills 的话，清 ids/src/svg_sizes 那几行删掉测试照样绿，
        // 而它们才是 RSS 回落的大头（ids 里一条内嵌 data: URI 就是几百 KB）。
        cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("填充");
        // src_size 也要打点：只取尺寸不填充的调用路径（layer 未写 size）同样该推迟回收。
        cache
            .src_size(&svg_data_uri(4, 4, "#0000ff"))
            .expect("SVG 尺寸");
        let res = cache.residency();
        assert!(
            res.0 >= 2 && res.1 >= 1 && res.2 >= 1 && res.3 >= 1 && res.4 > 0 && res.5 > 0,
            "四张表都该非空才谈得上验证回收，实际={res:?}"
        );

        assert!(cache.next_deadline().is_some(), "有内容就得登记回收时刻");
        assert!(!cache.evict_if_idle(t0), "刚取用过不该回收");

        let late = t0 + IDLE_EVICT_AFTER + Duration::from_secs(1);
        assert!(cache.evict_if_idle(late), "闲置超时该回收");
        assert!(!cache.evict_if_idle(late), "已经空了，不该重复报告回收");
        assert_eq!(
            cache.residency(),
            (0, 0, 0, 0, 0, 0),
            "回收必须让全部四张表归零，否则内存不会真的落回去"
        );
        assert_eq!(cache.next_deadline(), None);

        // 回收不是残废：同一张图再取用照样出正确像素。
        let px = cache
            .fill(&uri, FillSpec::stretch(), 4, 4, [0, 0, 0, 0])
            .expect("回收后应能重建")
            .pixel(0, 0)
            .expect("像素");
        assert_eq!((px.blue(), px.green(), px.alpha()), (255, 0, 255));
    }

    /// 九宫中段**平铺**：目标变宽只是多重复几次，纹理本身不缩放。
    ///
    /// 对照组是拉伸——候选窗宽度每变一下中段就被重新压缩一次（用户看到的「呼吸」），
    /// 根源就在那条按比例映射上。
    #[test]
    fn nine_slice_mid_repeats_instead_of_stretching() {
        let uri = png_stripes_data_uri(&[10, 20, 30, 40]);
        let mut cache = ImageCache::new();
        // slice 全 0 ⇒ 整轴都是中段，正好把中段的行为单独拎出来看。
        let repeat = FillSpec {
            mode: mode_code("nine_slice"),
            slice: [0; 4],
            repeat: [true, false],
            scale_milli: 1000,
        };
        let stretch = FillSpec {
            repeat: [false, false],
            ..repeat
        };

        let w8 = cache.fill(&uri, repeat, 8, 1, [0; 4]).expect("平铺 w=8");
        assert_eq!(row_reds(w8, 8), vec![10, 20, 30, 40, 10, 20, 30, 40]);
        let w6 = cache.fill(&uri, repeat, 6, 1, [0; 4]).expect("平铺 w=6");
        assert_eq!(
            row_reds(w6, 6),
            vec![10, 20, 30, 40, 10, 20],
            "平铺下，前四列必须与源图逐列一致，不随目标宽度变"
        );

        let s8 = cache.fill(&uri, stretch, 8, 1, [0; 4]).expect("拉伸 w=8");
        assert_eq!(
            row_reds(s8, 8),
            vec![10, 10, 20, 20, 30, 30, 40, 40],
            "拉伸下每列被摊成两格——这正是要能被 repeat 关掉的行为"
        );
    }

    /// 纵轴平铺要独立验一遍：横轴那条用的是 1px 高的源图，`dmid % 1` 恒 0，
    /// 就算 `repeat[1]` 根本没接线也照样绿。
    #[test]
    fn nine_slice_repeat_applies_to_vertical_axis_too() {
        // 1 宽 4 高的竖条纹，只让纵轴平铺。
        let mut img = image::RgbaImage::new(1, 4);
        for (y, r) in [10u8, 20, 30, 40].iter().enumerate() {
            img.put_pixel(0, y as u32, image::Rgba([*r, 0, 0, 255]));
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("PNG 编码");
        let uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
        );

        let mut cache = ImageCache::new();
        let spec = FillSpec {
            mode: mode_code("nine_slice"),
            slice: [0; 4],
            repeat: [false, true],
            scale_milli: 1000,
        };
        let pm = cache.fill(&uri, spec, 1, 6, [0; 4]).expect("纵向平铺");
        let col: Vec<u8> = (0..6)
            .map(|y| pm.pixel(0, y).expect("像素").blue())
            .collect();
        assert_eq!(col, vec![10, 20, 30, 40, 10, 20]);
    }

    /// 目标比切片还窄时同样退化为整体拉伸（`s0 + s1 >= dlen` 那条）。
    ///
    /// 这条在实机够得着：编码条窄于左右切片之和（本例主题是 59+9=68px）并不稀奇。
    #[test]
    fn nine_slice_falls_back_when_target_narrower_than_slices() {
        let uri = png_stripes_data_uri(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
        let mut cache = ImageCache::new();
        let spec = FillSpec {
            mode: mode_code("nine_slice"),
            slice: [0, 2, 0, 2],
            repeat: [true, false],
            scale_milli: 1000,
        };
        // 目标 3 < 左右切片和 4 ⇒ 整轴按比例拉伸：src = d*10/3 → 0, 3, 6
        let pm = cache.fill(&uri, spec, 3, 1, [0; 4]).expect("退化仍要出图");
        assert_eq!(row_reds(pm, 3), vec![10, 40, 70]);
    }

    /// 切片大到中段不剩时退化为整体拉伸，`repeat` 不该把这条路带歪（除零）。
    #[test]
    fn nine_slice_repeat_falls_back_when_no_middle_left() {
        let uri = png_stripes_data_uri(&[10, 20, 30, 40]);
        let mut cache = ImageCache::new();
        let spec = FillSpec {
            mode: mode_code("nine_slice"),
            // 左右切片加起来已经吃掉整张源图，中段宽度为 0。
            slice: [0, 2, 0, 2],
            repeat: [true, true],
            scale_milli: 1000,
        };
        let pm = cache.fill(&uri, spec, 8, 1, [0; 4]).expect("退化仍要出图");
        assert_eq!(row_reds(pm, 8), vec![10, 10, 20, 20, 30, 30, 40, 40]);
    }

    fn svg_spec(mode: &str, scale_milli: u32) -> FillSpec {
        FillSpec {
            mode: mode_code(mode),
            scale_milli,
            ..FillSpec::stretch()
        }
    }

    /// SVG `center`：单元 = 文档尺寸 × DPI，居中放进目标盒，四周透明（超出裁掉）。
    /// 此前 SVG 分支无视 mode 一律拉伸——整张盒被涂满。
    #[test]
    fn svg_center_places_document_sized_unit_in_the_middle() {
        let uri = svg_data_uri(4, 4, "#FF0000");
        let mut cache = ImageCache::new();
        let alpha_row = |pm: &Pixmap, y: u32| -> Vec<u8> {
            (0..16)
                .map(|x| pm.pixel(x, y).expect("像素").alpha())
                .collect()
        };

        // scale 2 → 单元 8×8，居中落在 [4,12)。
        let pm = cache
            .fill(&uri, svg_spec("center", 2000), 16, 16, [0; 4])
            .expect("SVG center 出图");
        let mut want = vec![0u8; 16];
        want[4..12].fill(255);
        assert_eq!(alpha_row(pm, 8), want, "单元没按文档×DPI 居中");
        assert_eq!(pm.pixel(8, 3).unwrap().alpha(), 0, "单元上方被涂");
        assert_eq!(pm.pixel(8, 12).unwrap().alpha(), 0, "单元下方被涂");

        // scale 不同单元不同：同图同盒、只差 scale，必须是另一份缓存。
        let pm1 = cache
            .fill(&uri, svg_spec("center", 1000), 16, 16, [0; 4])
            .expect("SVG center 出图");
        let mut want1 = vec![0u8; 16];
        want1[6..10].fill(255);
        assert_eq!(alpha_row(pm1, 8), want1, "scale 没进缓存键或没参与单元尺寸");

        // 单元比盒大：居中后四周裁掉，整盒涂满。
        let big = cache
            .fill(&uri, svg_spec("center", 2000), 4, 4, [0; 4])
            .expect("SVG center 出图");
        assert_eq!(big.pixel(0, 0).unwrap().alpha(), 255);
        assert_eq!(big.pixel(3, 3).unwrap().alpha(), 255);
    }

    /// SVG `tile`：以「文档尺寸 × DPI」为单元平铺，不随目标盒缩放。
    #[test]
    fn svg_tile_repeats_document_sized_unit() {
        // 2×1：左列红、右列蓝。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><rect width="1" height="1" fill="#FF0000"/><rect x="1" width="1" height="1" fill="#0000FF"/></svg>"##;
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        );
        let mut cache = ImageCache::new();
        // BGRA：源 R 落在 blue 通道（见 row_reds）；红 → 'r'，蓝 → 'b'。
        let row = |pm: &Pixmap, w: u32| -> String {
            (0..w)
                .map(|x| {
                    let p = pm.pixel(x, 0).expect("像素");
                    match (p.blue(), p.red()) {
                        (255, 0) => 'r',
                        (0, 255) => 'b',
                        _ => '?',
                    }
                })
                .collect()
        };
        let pm = cache
            .fill(&uri, svg_spec("tile", 2000), 12, 2, [0; 4])
            .expect("SVG tile 出图");
        assert_eq!(row(pm, 12), "rrbbrrbbrrbb", "没按 4×2 单元平铺");
        let pm1 = cache
            .fill(&uri, svg_spec("tile", 1000), 6, 1, [0; 4])
            .expect("SVG tile 出图");
        assert_eq!(row(pm1, 6), "rbrbrb");
    }

    /// 市场主题不可信：文档尺寸 × DPI 的单元可以大到分配不出来（这里 6 亿 px 宽）。
    /// center / tile 只该栅格化目标盒里看得见的那部分——整单元栅格化在这里直接出不了图。
    #[test]
    fn svg_center_and_tile_never_rasterize_beyond_the_box() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="300000000" height="1"><rect width="300000000" height="1" fill="#FF0000"/></svg>"##;
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        );
        let mut cache = ImageCache::new();
        // scale 2 → 单元 6e8×2：居中后盒内只剩中间两行。
        let c = cache
            .fill(&uri, svg_spec("center", 2000), 8, 8, [0; 4])
            .expect("超大单元的 center 也要出图");
        assert_eq!(c.pixel(0, 3).unwrap().alpha(), 255);
        assert_eq!(c.pixel(7, 4).unwrap().alpha(), 255);
        assert_eq!(c.pixel(4, 2).unwrap().alpha(), 0);
        assert_eq!(c.pixel(4, 5).unwrap().alpha(), 0);
        // tile：单元比盒宽，横向等于单元左段；纵向周期 2 行。
        let t = cache
            .fill(&uri, svg_spec("tile", 2000), 8, 4, [0; 4])
            .expect("超大单元的 tile 也要出图");
        for (x, y) in [(0, 0), (7, 1), (3, 2), (7, 3)] {
            assert_eq!(t.pixel(x, y).unwrap().alpha(), 255, "({x},{y})");
        }
    }

    /// 非整数文档尺寸：单元按统一比例 k 栅格化，不为凑满 ceil 后的格子而纵横各拉一份。
    /// 4×1.5 的满铺矩形在 scale 1 下占 1.5 行：第二行只有一半覆盖（半透明），不是被拉满。
    #[test]
    fn svg_unit_scales_uniformly_for_fractional_document() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="1.5"><rect width="4" height="1.5" fill="#FF0000"/></svg>"##;
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        );
        let mut cache = ImageCache::new();
        let pm = cache
            .fill(&uri, svg_spec("center", 1000), 4, 4, [0; 4])
            .expect("出图");
        let col: Vec<u8> = (0..4).map(|y| pm.pixel(1, y).unwrap().alpha()).collect();
        assert_eq!(col[0], 0);
        assert_eq!(col[1], 255);
        assert!(
            (100..=160).contains(&col[2]),
            "半行覆盖应是半透明，实得 {col:?}（255 = 被非等比拉满）"
        );
        assert_eq!(col[3], 0);
    }

    /// scale 只影响 SVG 的 center / tile：位图与 SVG stretch 在不同 DPI 下是同一份缓存，
    /// 否则每换一次 DPI 就多驻留一份一模一样的填充。
    #[test]
    fn scale_only_splits_cache_for_svg_center_and_tile() {
        let png = png_data_uri(4, 4, [255, 0, 0, 255]);
        let svg = svg_data_uri(4, 4, "#FF0000");
        let mut cache = ImageCache::new();
        for k in [1000, 2000] {
            cache
                .fill(&png, svg_spec("center", k), 8, 8, [0; 4])
                .unwrap();
            cache
                .fill(&svg, svg_spec("stretch", k), 8, 8, [0; 4])
                .unwrap();
        }
        assert_eq!(
            cache.fill_stats().0,
            2,
            "位图 / SVG stretch 按 DPI 分裂了缓存"
        );
        for k in [1000, 2000] {
            cache.fill(&svg, svg_spec("tile", k), 8, 8, [0; 4]).unwrap();
        }
        assert_eq!(
            cache.fill_stats().0,
            4,
            "SVG tile 不同 DPI 单元不同，必须分开缓存"
        );
    }

    /// stretch / nine_slice 维持原状：SVG 按目标尺寸整体拉伸，scale 不参与。
    #[test]
    fn svg_stretch_and_nine_slice_still_stretch() {
        let uri = svg_data_uri(4, 4, "#FF0000");
        let mut cache = ImageCache::new();
        for mode in ["stretch", "nine_slice"] {
            let pm = cache
                .fill(&uri, svg_spec(mode, 2000), 16, 16, [0; 4])
                .expect("SVG 出图");
            assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 255, "{mode}：没铺满");
            assert_eq!(pm.pixel(15, 15).unwrap().alpha(), 255, "{mode}：没铺满");
        }
    }

    /// tint 照常作用于 center：单元内换色，单元外仍透明。
    #[test]
    fn svg_center_honors_tint() {
        let uri = svg_data_uri(4, 4, "#FF0000");
        let mut cache = ImageCache::new();
        let pm = cache
            .fill(&uri, svg_spec("center", 1000), 8, 8, [0, 255, 0, 255])
            .expect("SVG center 出图");
        let inside = pm.pixel(4, 4).unwrap();
        assert_eq!((inside.green(), inside.alpha()), (255, 255), "tint 没生效");
        assert_eq!(inside.blue(), 0, "原色残留");
        assert_eq!(pm.pixel(1, 1).unwrap().alpha(), 0, "单元外被 tint 涂上");
    }

    #[test]
    fn svg_detection_covers_both_source_forms() {
        assert!(is_svg("chevron.svg"));
        assert!(is_svg("Chevron.SVG"));
        assert!(!is_svg("panel.png"));
        assert!(is_svg("data:image/svg+xml;base64,PHN2Zz48L3N2Zz4="));
        assert!(!is_svg("data:image/png;base64,iVBORw0KGgo="));
    }

    /// 无法解码的 data: 形态走「解码失败」这条既有路径（告警 + 缓存 None），不 panic。
    #[test]
    fn undecodable_data_uris_yield_none() {
        // 百分号编码形态：没有生产者，不实现。
        assert!(decode_data_uri("data:image/svg+xml,%3Csvg%3E").is_none());
        assert!(decode_data_uri("data:image/png;base64,!!!not-base64!!!").is_none());
        assert!(
            ImageCache::new()
                .src_size("data:image/png;base64,####")
                .is_none()
        );
    }

    /// data: URI 有数百 KB，日志里只能留一个短标识。
    #[test]
    fn brief_truncates_data_uri_for_logs() {
        let long = format!("data:image/png;base64,{}", "A".repeat(10_000));
        let s = brief(&long);
        assert!(s.len() < 200, "日志标识不该带上整条 data: URI：{s}");
        assert_eq!(brief("/themes/x/panel.png"), "/themes/x/panel.png");
    }
}
