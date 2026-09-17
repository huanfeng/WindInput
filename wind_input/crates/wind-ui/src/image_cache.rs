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

/// 栅格化 SVG 到 w×h，返回预乘 RGBA 字节（resvg 输出）。
fn rasterize_svg(src: &str, w: u32, h: u32) -> Option<Vec<u8>> {
    let tree = svg_tree(src)?;
    let size = tree.size();
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h)?;
    let sx = w as f32 / size.width().max(1.0);
    let sy = h as f32 / size.height().max(1.0);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(sx, sy),
        &mut pm.as_mut(),
    );
    Some(pm.data().to_vec())
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

/// 解码位图源：`data:` URI 走内存解码，否则按文件路径打开。
fn decode_bitmap(src: &str) -> Option<image::DynamicImage> {
    if src.starts_with("data:") {
        return image::load_from_memory(&decode_data_uri(src)?).ok();
    }
    image::open(src).ok()
}

/// 是否 SVG：文件看扩展名，`data:` 看 MIME。
///
/// 只看扩展名会漏掉 `data:image/svg+xml;base64,...`（它不以 `.svg` 结尾），把矢量图
/// 丢进位图解码器 —— 那是必然失败且看不出原因的一条路。
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

/// 填充模式码：0=stretch（默认）1=nine_slice 2=tile 3=center。
pub fn mode_code(mode: &str) -> u8 {
    match mode {
        "nine_slice" => 1,
        "tile" => 2,
        "center" => 3,
        _ => 0, // stretch
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

type FillKey = (ImgId, u8, [u32; 4], u32, u32, [u8; 4]);

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
        self.touch_at(Instant::now());
    }

    /// 打点到指定时刻（测试用它陈述「最后一次取用是何时」）。
    fn touch_at(&mut self, now: Instant) {
        self.last_use = Some(now);
    }

    /// 仅测试可见：把最后取用时刻挪到过去，模拟闲置。
    #[cfg(test)]
    pub fn set_last_use_for_test(&mut self, now: Instant) {
        self.touch_at(now);
    }

    /// 闲置回收的到期时刻；None = 无需唤醒。
    ///
    /// ⚠ 调用方（UI 消息循环）必须把它登记进 `next_deadline`，否则线程睡下去就再也不会
    /// 来收这份内存——闲置回收只在有别的事把线程叫醒时才碰巧发生。
    pub fn next_deadline(&self) -> Option<Instant> {
        self.last_use.map(|t| t + IDLE_EVICT_AFTER)
    }

    /// 闲置够久就整个丢掉，返回是否真的清了。
    ///
    /// 连驻留表 `ids` 一起清：它存的是完整源串，一条内嵌 `data:` URI 就是几百 KB。
    /// `next_id` **不**跟着回退——句柄一旦复用，`src`/`fills` 里按旧 id 索引的残留就会
    /// 串成另一张图。这里全表一起清本不会留下残留，但那个不变量不该依赖「调用顺序恰好
    /// 安全」来维持。
    pub fn evict_if_idle(&mut self, now: Instant) -> bool {
        let Some(last) = self.last_use else {
            return false;
        };
        if now.saturating_duration_since(last) < IDLE_EVICT_AFTER {
            return false;
        }
        self.ids.clear();
        self.src.clear();
        self.svg_sizes.clear();
        self.fills.clear();
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
        self.decode(id, path).map(|s| (s.w, s.h))
    }

    /// 取（或构建）目标尺寸填充位图（BGRA 序 + 预乘）。
    /// tint=[0,0,0,0] 表示不染色；非零时把图当 alpha mask、用 tint 色填充（单色 SVG/图标随主题变色）。
    pub fn fill(
        &mut self,
        path: &str,
        mode: u8,
        slice: [u32; 4],
        w: u32,
        h: u32,
        tint: [u8; 4],
    ) -> Option<&Pixmap> {
        self.touch();
        let key = (self.id_of(path), mode, slice, w, h, tint);
        if !self.fills.contains_key(&key) {
            let built = self.build_fill(key, path);
            let bytes = built.as_ref().map_or(0, |pm| pm.data().len());
            if self.fill_bytes + bytes > FILL_BUDGET_BYTES {
                self.fills.clear();
                self.fill_bytes = 0;
            }
            self.fill_bytes += bytes;
            self.fills.insert(key, built);
        }
        self.fills.get(&key).and_then(|o| o.as_ref())
    }

    fn build_fill(&mut self, key: FillKey, path: &str) -> Option<Pixmap> {
        let (id, mode, slice, w, h, tint) = key;
        if w == 0 || h == 0 {
            return None;
        }
        let mut pm = Pixmap::new(w, h)?;
        if is_svg(path) {
            // SVG：按目标尺寸栅格化（resvg 输出预乘 RGBA），逐像素 tint/直通 + R/B 交换。
            // 告警与位图侧对齐：静默画不出来正是这次要消灭的病症，别在这一侧留一份。
            let Some(rgba) = rasterize_svg(path, w, h) else {
                tracing::warn!("主题背景图（SVG）栅格化失败: {}", brief(path));
                return None;
            };
            let px = pm.pixels_mut();
            for (i, p) in px.iter_mut().enumerate() {
                let b = i * 4;
                *p = compose(rgba[b], rgba[b + 1], rgba[b + 2], rgba[b + 3], tint, true);
            }
            return Some(pm);
        }
        // 位图：image 解码（未预乘）→ 按模式采样 → tint/预乘 + R/B 交换。
        let src = self.decode(id, path)?;
        let (sw, sh, data) = (src.w, src.h, &src.rgba);
        if sw == 0 || sh == 0 {
            return None;
        }
        let px = pm.pixels_mut();
        for dy in 0..h {
            for dx in 0..w {
                let Some((sx, sy)) = map_src(mode, slice, sw, sh, w, h, dx, dy) else {
                    continue; // 透明（Pixmap::new 已清零）
                };
                let si = ((sy * sw + sx) * 4) as usize;
                px[(dy * w + dx) as usize] = compose(
                    data[si],
                    data[si + 1],
                    data[si + 2],
                    data[si + 3],
                    tint,
                    false,
                );
            }
        }
        Some(pm)
    }
}

/// 目标像素 (dx,dy) → 源像素坐标；None=该处透明（仅 center 越界）。
#[allow(clippy::too_many_arguments)]
fn map_src(
    mode: u8,
    slice: [u32; 4],
    sw: u32,
    sh: u32,
    w: u32,
    h: u32,
    dx: u32,
    dy: u32,
) -> Option<(u32, u32)> {
    match mode {
        1 => {
            // nine_slice：slice = [上,右,下,左]
            let sx = nine_axis(dx, w, sw, slice[3], slice[1])?;
            let sy = nine_axis(dy, h, sh, slice[0], slice[2])?;
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

/// 九宫格单轴映射：起/末 `s0`/`s1` 像素 1:1，中段拉伸。
fn nine_axis(d: u32, dlen: u32, slen: u32, s0: u32, s1: u32) -> Option<u32> {
    // 切片过大时退化为整体拉伸，避免中段为负。
    if s0 + s1 >= slen || s0 + s1 >= dlen {
        return Some((d as u64 * slen as u64 / dlen as u64).min(slen as u64 - 1) as u32);
    }
    if d < s0 {
        Some(d) // 起始固定段
    } else if d >= dlen - s1 {
        Some(slen - (dlen - d)) // 末尾固定段（对齐到源末尾）
    } else {
        // 中段拉伸：dest [s0, dlen-s1) → src [s0, slen-s1)
        let dmid = d - s0;
        let dmid_len = dlen - s0 - s1;
        let smid_len = slen - s0 - s1;
        Some((s0 + (dmid as u64 * smid_len as u64 / dmid_len as u64) as u32).min(slen - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

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
            .fill(&uri, mode_code("stretch"), [0; 4], 4, 4, [0, 0, 0, 0])
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
            .fill(&uri, mode_code("stretch"), [0; 4], 4, 4, [0, 0, 0, 0])
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
            .fill(&red, 0, [0; 4], 2, 2, [0, 0, 0, 0])
            .expect("红图")
            .pixel(0, 0)
            .expect("像素");
        let g = cache
            .fill(&green, 0, [0; 4], 2, 2, [0, 0, 0, 0])
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
            .fill(&p, 0, [0; 4], 2, 2, [0, 0, 0, 0])
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
            .fill(&uri, 0, [0; 4], 4, 4, [0, 0, 0, 0])
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
                .fill(&uri, 0, [0; 4], edge, edge, [0, 0, 0, i])
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
            .fill(&uri, 0, [0; 4], small, small, [0, 0, 0, 3])
            .expect("首次");
        let before = cache.fill_stats();
        for _ in 0..10 {
            cache
                .fill(&uri, 0, [0; 4], small, small, [0, 0, 0, 3])
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
            .fill(&uri, 0, [0; 4], 4, 4, [0, 0, 0, 0])
            .expect("首次");

        let stale = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .expect("单调时钟回拨 10 秒");
        cache.set_last_use_for_test(stale);
        let before = cache.next_deadline().expect("有内容就该有到期时刻");

        cache
            .fill(&uri, 0, [0; 4], 4, 4, [0, 0, 0, 0])
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
            .fill(&uri, 0, [0; 4], 4, 4, [0, 0, 0, 0])
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
            .fill(&uri, 0, [0; 4], 4, 4, [0, 0, 0, 0])
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
            .fill(&uri, 0, [0; 4], 4, 4, [0, 0, 0, 0])
            .expect("回收后应能重建")
            .pixel(0, 0)
            .expect("像素");
        assert_eq!((px.blue(), px.green(), px.alpha()), (255, 0, 255));
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
