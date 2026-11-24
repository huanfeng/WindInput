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
        .anim_layout(&uri, 1.0, 1, 1, None, ANIM_BUDGET_BYTES, Flip::NONE)
        .expect("动画");
    assert_eq!(l.raw_frames, MAX_ANIM_FRAMES, "超出的帧不解");
}

fn gif_uri(frames: &[([u8; 4], u32)]) -> String {
    anim_fixtures::data_uri("image/gif", &anim_fixtures::gif(4, 4, frames))
}

/// 第 `idx` 帧左上像素（BGRA 预乘）。
fn frame_px(c: &mut ImageCache, uri: &str, budget: usize, idx: usize) -> [u8; 4] {
    let pm = c
        .anim_frame(uri, 1.0, 4, 4, None, budget, Flip::NONE, idx)
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
        c.anim_durations(&uri, 1.0, 4, 4, None, ANIM_BUDGET_BYTES, Flip::NONE),
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
        c.anim_durations(&uri, 1.0, 4, 4, Some(8.0), ANIM_BUDGET_BYTES, Flip::NONE),
        Some(&[125, 125][..])
    );
}

#[test]
fn over_budget_animation_drops_every_other_frame() {
    // 每帧 4×4×4 = 64 字节，预算 128：4 帧隔帧丢成 2 帧（第 0、2 帧），时长并入前一帧。
    let uri = gif_uri(&[(RED, 100), (GREEN, 200), (BLUE, 300), (WHITE, 400)]);
    let mut c = ImageCache::new();
    assert_eq!(
        c.anim_durations(&uri, 1.0, 4, 4, None, 128, Flip::NONE),
        Some(&[300, 700][..])
    );
    assert_eq!(
        frame_px(&mut c, &uri, 128, 1),
        [255, 0, 0, 255],
        "留下的是蓝帧"
    );
    assert_eq!(
        c.anim_layout(&uri, 1.0, 4, 4, None, 128, Flip::NONE),
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
        c.anim_durations(&one, 1.0, 4, 4, None, ANIM_BUDGET_BYTES, Flip::NONE),
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
        c.anim_durations(&uri, 0.5, 4, 1, None, ANIM_BUDGET_BYTES, Flip::NONE),
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
            .skin_fill(&still, 0.5, &whole, &whole, 4, 1, Flip::NONE)
            .unwrap()
            .data()
            .to_vec();
        let got = c
            .anim_frame(&uri, 0.5, 4, 1, None, ANIM_BUDGET_BYTES, Flip::NONE, i)
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
    c.anim_durations(&uri, 1.0, 4, 4, None, ANIM_BUDGET_BYTES, Flip::NONE);
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
    c.anim_durations(&uri, 1.0, 4, 4, None, ANIM_BUDGET_BYTES, Flip::NONE);
    c.clear_anims();
    assert_eq!((c.anims.len(), c.anim_bytes), (0, 0));
}

#[test]
fn animated_source_is_not_kept_at_full_resolution() {
    let uri = gif_uri(&[(RED, 100), (GREEN, 100)]);
    let mut c = ImageCache::new();
    assert_eq!(c.src_size(&uri), Some((4, 4)), "只读头也拿得到尺寸");
    c.anim_durations(&uri, 1.0, 4, 4, None, ANIM_BUDGET_BYTES, Flip::NONE);
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

/// 编辑器发布的主题把图片以 data: URI 内嵌在 theme.toml 里，必须一路解码到像素，
/// 不能当文件名去 `image::open`。
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

/// SVG 没有位图源，原始尺寸只能问文档自己；拿不到尺寸时 `view.rs` 的 layer 分支会整层不画。
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

/// 候选窗宽度每变一个像素就是一条新的填充缓存条目，没有上界就只涨不落。超预算必须整体让位。
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
    // fill_bytes 充气，几帧就清一次表。
    // 特意用不会触发清空的小尺寸：大图重复记账会清表重建，字节数可能恰好绕回原值、断言失效。
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

/// 打点必须在命中时也刷新。只在未命中时打点的话，连续打字的会话会在开始
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

/// 读头取尺寸：不解码、源图不常驻；读不出答 None 且缓存；闲置回收一并清。
#[test]
fn header_size_reads_header_without_decoding() {
    let uri = png_data_uri(3, 2, [255, 0, 0, 255]);
    let mut cache = ImageCache::new();
    assert_eq!(cache.header_size(&uri), Some((3, 2)));
    assert_eq!(cache.src_stats(), (0, 0), "只读头，不解码");
    assert_eq!(cache.header_size("/nonexistent/x.png"), None);
    assert_eq!(cache.header_sizes.len(), 2, "读不出也缓存");
    assert_eq!(
        cache.header_size(&svg_data_uri(4, 5, "#0000ff")),
        Some((4, 5)),
        "SVG 取文档尺寸"
    );
    let late = crate::clock::now() + IDLE_EVICT_AFTER + Duration::from_secs(1);
    assert!(cache.evict_if_idle(late));
    assert!(cache.header_sizes.is_empty(), "闲置回收一并清");
}

/// 九宫中段平铺：目标变宽只是多重复几次，纹理本身不缩放（拉伸的话候选窗宽度每变一下
/// 中段就被重新压缩一次，看起来在「呼吸」）。
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
        flip: Flip::NONE,
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
        flip: Flip::NONE,
    };
    let pm = cache.fill(&uri, spec, 1, 6, [0; 4]).expect("纵向平铺");
    let col: Vec<u8> = (0..6)
        .map(|y| pm.pixel(0, y).expect("像素").blue())
        .collect();
    assert_eq!(col, vec![10, 20, 30, 40, 10, 20]);
}

/// 目标比切片还窄时同样退化为整体拉伸（`s0 + s1 >= dlen` 那条）；编码条窄于左右切片之和并不稀奇。
#[test]
fn nine_slice_falls_back_when_target_narrower_than_slices() {
    let uri = png_stripes_data_uri(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
    let mut cache = ImageCache::new();
    let spec = FillSpec {
        mode: mode_code("nine_slice"),
        slice: [0, 2, 0, 2],
        repeat: [true, false],
        scale_milli: 1000,
        flip: Flip::NONE,
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
        flip: Flip::NONE,
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

/// SVG `center`：单元 = 文档尺寸 × DPI，居中放进目标盒，四周透明（超出裁掉），不得无视 mode 拉伸。
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

// ── flip ──

const FLIPS: [Flip; 3] = [
    Flip { x: true, y: false },
    Flip { x: false, y: true },
    Flip { x: true, y: true },
];

/// 非对称夹具：`w×h`，每个像素的红色分量各不相同（行优先递增）。
fn png_grid_uri(w: u32, h: u32) -> String {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(20 + 30 * (y * w + x)) as u8, 0, 0, 255])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("PNG 编码");
    anim_fixtures::data_uri("image/png", &png.into_inner())
}

/// 由不翻的结果推出翻转后应有的像素（逐像素镜像，与实现无关的独立算法）。
fn mirrored(pm: &Pixmap, f: Flip) -> Vec<PremultipliedColorU8> {
    let (w, h) = (pm.width(), pm.height());
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let sx = if f.x { w - 1 - x } else { x };
            let sy = if f.y { h - 1 - y } else { y };
            out.push(pm.pixel(sx, sy).unwrap());
        }
    }
    out
}

fn assert_asymmetric(pm: &Pixmap) {
    let (w, h) = (pm.width(), pm.height());
    assert_ne!(pm.pixel(0, 0), pm.pixel(w - 1, 0), "夹具左右须不对称");
    assert_ne!(pm.pixel(0, 0), pm.pixel(0, h - 1), "夹具上下须不对称");
}

#[test]
fn fill_flips_after_composing_and_keys_apart() {
    let uri = png_grid_uri(3, 2);
    let mut c = ImageCache::new();
    let plain = c
        .fill(&uri, FillSpec::stretch(), 6, 4, [0; 4])
        .unwrap()
        .clone();
    assert_asymmetric(&plain);
    for f in FLIPS {
        let spec = FillSpec {
            flip: f,
            ..FillSpec::stretch()
        };
        let got = c.fill(&uri, spec, 6, 4, [0; 4]).unwrap();
        assert_eq!(got.pixels(), mirrored(&plain, f).as_slice(), "{f:?}");
    }
    // x：首末列互换。
    let x = FillSpec {
        flip: FLIPS[0],
        ..FillSpec::stretch()
    };
    let got = c.fill(&uri, x, 6, 4, [0; 4]).unwrap().clone();
    assert_eq!(got.pixel(0, 0), plain.pixel(5, 0));
    assert_eq!(got.pixel(5, 0), plain.pixel(0, 0));
    // 翻过之后再取不翻的，仍是原图：缓存键分得开、没被就地翻坏。
    let again = c.fill(&uri, FillSpec::stretch(), 6, 4, [0; 4]).unwrap();
    assert_eq!(again.data(), plain.data());
}

/// 九宫按源图切、合成后整体翻：右缘出现的是源图左切片。
#[test]
fn nine_slice_flip_mirrors_the_composed_result() {
    let uri = png_stripes_data_uri(&[10, 20, 30, 40]);
    let mut c = ImageCache::new();
    let spec = FillSpec {
        mode: mode_code("nine_slice"),
        slice: [0, 1, 0, 1],
        ..FillSpec::stretch()
    };
    let plain = row_reds(c.fill(&uri, spec, 8, 1, [0; 4]).unwrap(), 8);
    let flipped = FillSpec {
        flip: FLIPS[0],
        ..spec
    };
    let got = row_reds(c.fill(&uri, flipped, 8, 1, [0; 4]).unwrap(), 8);
    let mut want = plain.clone();
    want.reverse();
    assert_eq!(got, want);
    assert_eq!((got[0], got[7]), (40, 10));
}

/// tile 先按原相位铺满再整块翻：`x` 时从右缘起铺，末尾那截不完整的单元落到左侧。
#[test]
fn tile_flip_starts_from_right_edge() {
    let uri = png_stripes_data_uri(&[10, 20, 30, 40]);
    let mut c = ImageCache::new();
    let tile = FillSpec {
        mode: mode_code("tile"),
        ..FillSpec::stretch()
    };
    // 6 不是单元宽 4 的整数倍。
    let plain = row_reds(c.fill(&uri, tile, 6, 1, [0; 4]).unwrap(), 6);
    assert_eq!(
        plain,
        [10, 20, 30, 40, 10, 20],
        "不翻：从左缘起铺、右侧裁掉"
    );
    let flipped = FillSpec {
        flip: FLIPS[0],
        ..tile
    };
    let got = row_reds(c.fill(&uri, flipped, 6, 1, [0; 4]).unwrap(), 6);
    assert_eq!(got[5], 10, "末列 = 源图首列：从右缘起铺");
    assert_eq!(&got[2..], [40, 30, 20, 10], "右侧是一整个（镜像的）单元");
    assert_eq!(&got[..2], [20, 10], "不完整的那截落在左侧");
}

/// SVG（整盒栅格化与 center 单元两条路径）同样在合成后翻。
#[test]
fn svg_fill_flips_like_bitmaps() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="4"><rect width="8" height="4" fill="#0000ff"/><rect width="2" height="1" fill="#ff0000"/></svg>"##;
    let uri = format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    );
    let mut c = ImageCache::new();
    for mode in ["stretch", "center"] {
        let spec = FillSpec {
            mode: mode_code(mode),
            ..FillSpec::stretch()
        };
        let plain = c.fill(&uri, spec, 8, 4, [0; 4]).unwrap().clone();
        assert_asymmetric(&plain);
        for f in FLIPS {
            let got = c
                .fill(&uri, FillSpec { flip: f, ..spec }, 8, 4, [0; 4])
                .unwrap();
            assert_eq!(got.pixels(), mirrored(&plain, f).as_slice(), "{mode} {f:?}");
        }
    }
}

#[test]
fn skin_fill_flips_and_shares_the_atlas() {
    // 奇数宽高：中行 / 中列原地不动。
    use wind_theme::skin::{BandFill, Stretch};
    let whole = Stretch::Whole(BandFill::Stretch);
    let uri = png_grid_uri(3, 2);
    let mut c = ImageCache::new();
    let plain = c
        .skin_fill(&uri, 1.0, &whole, &whole, 5, 3, Flip::NONE)
        .unwrap()
        .clone();
    assert_asymmetric(&plain);
    let atlases = c.atlases.len();
    for f in FLIPS {
        let got = c.skin_fill(&uri, 1.0, &whole, &whole, 5, 3, f).unwrap();
        assert_eq!(got.pixels(), mirrored(&plain, f).as_slice(), "{f:?}");
    }
    assert_eq!(c.atlases.len(), atlases, "图集翻与不翻共用");
    assert_eq!(c.skin_fills.len(), 4, "合成位图按翻转位分开缓存");
    let again = c
        .skin_fill(&uri, 1.0, &whole, &whole, 5, 3, Flip::NONE)
        .unwrap();
    assert_eq!(again.data(), plain.data(), "不串图");
}

#[test]
fn anim_flips_every_frame() {
    // 2×1 两帧：左右颜色不同，且两帧互不相同。
    let frames = [([RED, BLUE].concat(), 100), ([GREEN, WHITE].concat(), 100)];
    let uri = anim_fixtures::data_uri("image/png", &anim_fixtures::apng(2, 1, &frames));
    let mut c = ImageCache::new();
    let px = |c: &mut ImageCache, f: Flip, i: usize| {
        let pm = c
            .anim_frame(&uri, 1.0, 2, 1, None, ANIM_BUDGET_BYTES, f, i)
            .expect("动画帧");
        (pm.pixel(0, 0).unwrap(), pm.pixel(1, 0).unwrap())
    };
    for i in 0..2 {
        let (l, r) = px(&mut c, Flip::NONE, i);
        assert_ne!(l, r, "第 {i} 帧夹具须不对称");
        assert_eq!(px(&mut c, FLIPS[0], i), (r, l), "第 {i} 帧要翻");
        assert_eq!(px(&mut c, FLIPS[1], i), (l, r), "1 像素高，上下翻不变");
        assert_eq!(px(&mut c, Flip::NONE, i), (l, r), "不串图");
    }
    assert_eq!(c.anims[&uri].len(), 3, "动画按翻转位分开缓存");
    assert_eq!(
        c.anim_layout(&uri, 1.0, 2, 1, None, ANIM_BUDGET_BYTES, FLIPS[0])
            .map(|l| l.durations),
        c.anim_layout(&uri, 1.0, 2, 1, None, ANIM_BUDGET_BYTES, Flip::NONE)
            .map(|l| l.durations),
        "翻转不改时间轴"
    );
}
