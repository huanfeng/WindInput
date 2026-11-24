use super::*;
use std::fs;

const THEME_TEXT: &str = "[meta]\nname = \"水墨\"\nauthor = \"某人\"\nversion = \"1.2\"\n\n[window]\nbackground = { image = { ref = \"assets/bg.png\" } }\n";

/// 手工写一个 zip（测守卫与识别用）。
fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in entries {
        // 图片条目走存储模式，大体积用例不必在 debug 下跑 deflate。
        w.start_file(*name, entry_options(name)).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap();
}

fn good_meta() -> String {
    format!(
        "[package]\nformat_version = {THEME_FORMAT_VERSION}\nkind = \"{THEME_KIND}\"\nname = \"水墨\"\n"
    )
}

/// 一个内容完整的包。
fn write_good(path: &Path) {
    write_zip(
        path,
        &[
            (THEME_META_NAME, good_meta().as_bytes()),
            (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
            (THEME_PREVIEW_NAME, b"fake-png"),
            ("assets/bg.png", b"bg-bytes"),
        ],
    );
}

#[test]
fn export_then_import_round_trips() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src-theme");
    fs::create_dir_all(src.join("assets")).unwrap();
    fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
    fs::write(src.join("assets/bg.png"), b"bg-bytes").unwrap();
    // 主题目录里的无关文件不该进包。
    fs::write(src.join("Thumbs.db"), b"junk").unwrap();

    let pkg = t.path().join("out.wtheme");
    export_package(&src, &pkg, Some(b"preview")).unwrap();

    let preview = preview_package(&pkg).unwrap();
    assert_eq!(preview.display_name, "水墨");
    assert_eq!(preview.author, "某人");
    assert_eq!(preview.version, "1.2");
    assert_eq!(preview.asset_count, 1);
    assert!(preview.has_preview);

    let dst = t.path().join("themes").join("shuimo");
    let out = import_package(&pkg, &dst, |_| Ok(())).unwrap();
    assert_eq!(out.display_name, "水墨");
    assert_eq!(
        fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
        THEME_TEXT
    );
    assert_eq!(fs::read(dst.join("assets/bg.png")).unwrap(), b"bg-bytes");
    // package.toml / preview.png 不是主题资源，落盘只会多两个死文件。
    assert!(!dst.join(THEME_META_NAME).exists());
    assert!(!dst.join(THEME_PREVIEW_NAME).exists());
    // 导出不收无关文件。
    assert!(!dst.join("Thumbs.db").exists());
}

/// 覆盖导入是**整目录替换**：上一版的资源不能留成垃圾。
#[test]
fn reimport_replaces_whole_directory() {
    let t = tempfile::tempdir().unwrap();
    let dst = t.path().join("themes").join("x");
    fs::create_dir_all(dst.join("assets")).unwrap();
    fs::write(dst.join(THEME_ENTRY_NAME), "[meta]\nname = \"旧\"\n").unwrap();
    fs::write(dst.join("assets/old.png"), b"stale").unwrap();

    let pkg = t.path().join("p.wtheme");
    write_good(&pkg);
    import_package(&pkg, &dst, |_| Ok(())).unwrap();

    assert!(dst.join("assets/bg.png").exists());
    assert!(
        !dst.join("assets/old.png").exists(),
        "上一版的资源该跟着走，否则主题目录越导越胀"
    );
}

/// 坏包不能把已装好的主题弄没。
#[test]
fn failed_import_leaves_target_untouched() {
    let t = tempfile::tempdir().unwrap();
    let dst = t.path().join("themes").join("x");
    fs::create_dir_all(&dst).unwrap();
    fs::write(dst.join(THEME_ENTRY_NAME), "[meta]\nname = \"原装\"\n").unwrap();

    let pkg = t.path().join("bad.wtheme");
    write_zip(&pkg, &[("assets/bg.png", b"only-assets")]); // 缺 theme.toml

    assert!(import_package(&pkg, &dst, |_| Ok(())).is_err());
    assert_eq!(
        fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
        "[meta]\nname = \"原装\"\n"
    );
}

/// 就位后的校验失败要整体回滚：装不上的新版不能把用户原来的主题顶掉。
///
/// 这一步只能在包已就位、旧目录还没删的窗口里做：调用方拿到失败时手上已没有旧内容。
#[test]
fn verify_failure_rolls_back_to_previous_theme() {
    let t = tempfile::tempdir().unwrap();
    let dst = t.path().join("themes").join("x");
    fs::create_dir_all(dst.join("assets")).unwrap();
    fs::write(dst.join(THEME_ENTRY_NAME), "[meta]\nname = \"原装\"\n").unwrap();
    fs::write(dst.join("assets/keep.png"), b"mine").unwrap();

    let pkg = t.path().join("p.wtheme");
    write_good(&pkg);
    let err = import_package(&pkg, &dst, |_| anyhow::bail!("base 引用的基础主题不存在"))
        .unwrap_err()
        .to_string();

    assert!(err.contains("base"), "校验失败的原因要透传：{err}");
    assert_eq!(
        fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
        "[meta]\nname = \"原装\"\n"
    );
    assert!(dst.join("assets/keep.png").exists(), "旧资源该跟着回来");
    assert!(!dst.join("assets/bg.png").exists(), "新版的资源不该残留");
}

/// 新装时校验失败：不留半个主题目录，否则主题列表里会多出一个打不开的条目。
#[test]
fn verify_failure_on_fresh_install_leaves_nothing() {
    let t = tempfile::tempdir().unwrap();
    let dst = t.path().join("themes").join("brand-new");
    let pkg = t.path().join("p.wtheme");
    write_good(&pkg);

    assert!(import_package(&pkg, &dst, |_| anyhow::bail!("装不上")).is_err());
    assert!(!dst.exists());
}

/// 校验看到的是**就位后的真实目录**——依赖链那类判断只有在这个位置才做得了。
#[test]
fn verify_sees_the_placed_directory() {
    let t = tempfile::tempdir().unwrap();
    let dst = t.path().join("themes").join("x");
    let pkg = t.path().join("p.wtheme");
    write_good(&pkg);

    let mut seen = None;
    import_package(&pkg, &dst, |dir| {
        seen = Some(dir.join(THEME_ENTRY_NAME).exists() && dir.join("assets/bg.png").exists());
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, Some(true), "校验时主题应当已经在目标位置了");
}

/// 路径穿越：`../` 与反斜杠形态都不能落到主题目录外面。
#[test]
fn rejects_path_traversal() {
    let t = tempfile::tempdir().unwrap();
    for evil in ["../evil.png", "assets/../../evil.png", "..\\evil.png"] {
        let pkg = t.path().join("e.wtheme");
        write_zip(
            &pkg,
            &[(THEME_ENTRY_NAME, THEME_TEXT.as_bytes()), (evil, b"pwned")],
        );
        assert!(preview_package(&pkg).is_err(), "穿越条目「{evil}」该被拒");
    }
}

/// 不在 assets/ 下的陌生条目宁可拒绝，也不要悄悄丢掉——悄悄丢的话，作者要等主题
/// 装进输入法、图不显示才发现自己把文件放错了位置。
#[test]
fn rejects_unknown_entries() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("u.wtheme");
    write_zip(
        &pkg,
        &[
            (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
            ("bg.png", b"misplaced"),
        ],
    );
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains("bg.png"), "报错要指出是哪个条目：{err}");
    assert!(err.contains(THEME_ASSETS_PREFIX), "还要说清该放哪儿：{err}");
}

#[test]
fn rejects_missing_theme_entry() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("n.wtheme");
    write_zip(&pkg, &[(THEME_META_NAME, good_meta().as_bytes())]);
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains(THEME_ENTRY_NAME), "{err}");
}

/// 扩展名只是路由提示，包内 kind 才是权威判据：方案包被改名成 .wtheme 时要说清。
#[test]
fn rejects_other_kinds_by_name() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("s.wtheme");
    write_zip(
        &pkg,
        &[
            (
                THEME_META_NAME,
                b"[package]\nformat_version = 1\nkind = \"scheme\"\n".as_slice(),
            ),
            (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
        ],
    );
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains("scheme"), "要说清它是什么：{err}");
}

/// 手工打的包可以没有 package.toml：此时按根条目推断，不该因此失败。
#[test]
fn accepts_hand_made_package_without_meta() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("h.wtheme");
    write_zip(
        &pkg,
        &[
            (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
            ("assets/bg.png", b"bg"),
        ],
    );
    let p = preview_package(&pkg).unwrap();
    // package.toml 缺席 ⇒ 展示信息回退到 theme.toml 自己的 meta。
    assert_eq!(p.display_name, "水墨");
    assert_eq!(p.author, "某人");
}

/// 未来版本的包不能按当前规则硬解——宁可让用户去升级。
#[test]
fn rejects_newer_format_version() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("v.wtheme");
    let meta = format!(
        "[package]\nformat_version = {}\nkind = \"{THEME_KIND}\"\n",
        THEME_FORMAT_VERSION + 1
    );
    write_zip(
        &pkg,
        &[
            (THEME_META_NAME, meta.as_bytes()),
            (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
        ],
    );
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains("升级"), "要告诉用户怎么办：{err}");
}

#[test]
fn rejects_too_many_entries() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("many.wtheme");
    let names: Vec<String> = (0..=MAX_THEME_ENTRIES)
        .map(|i| format!("assets/{i}.png"))
        .collect();
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .map(|n| (n.as_str(), b"x".as_slice()))
        .collect();
    write_zip(&pkg, &entries);
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains("条目过多"), "{err}");
}

/// 坏主题在**落盘前**就该被拦下，而不是写完再回滚。
#[test]
fn rejects_invalid_theme_text_before_writing() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("b.wtheme");
    write_zip(&pkg, &[(THEME_ENTRY_NAME, "这不是 TOML {{{".as_bytes())]);
    assert!(preview_package(&pkg).is_err());

    let dst = t.path().join("themes").join("x");
    assert!(import_package(&pkg, &dst, |_| Ok(())).is_err());
    assert!(!dst.exists(), "失败的导入不该留下半个主题目录");
}

/// 导出的条目顺序要稳定：同样的输入打出同样的包，否则每次导出都像「全变了」。
#[test]
fn export_entry_order_is_stable() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("s");
    fs::create_dir_all(src.join("assets/sub")).unwrap();
    fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
    for n in ["b.png", "a.png", "sub/c.png"] {
        fs::write(src.join("assets").join(n), b"x").unwrap();
    }
    let names = |p: &Path| -> Vec<String> {
        let f = fs::File::open(p).unwrap();
        zip::ZipArchive::new(f)
            .unwrap()
            .file_names()
            .map(String::from)
            .collect()
    };
    let p1 = t.path().join("1.wtheme");
    let p2 = t.path().join("2.wtheme");
    export_package(&src, &p1, None).unwrap();
    export_package(&src, &p2, None).unwrap();
    assert_eq!(names(&p1), names(&p2));
}

// ── 皮肤 ──

/// 程序生成的单帧 PNG（纯色 RGBA）。
fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    apng_bytes(w, h, &[rgba])
}

/// 程序生成的 APNG：每帧一种纯色；只有一帧时就是普通 PNG。
fn apng_bytes(w: u32, h: u32, frames: &[[u8; 4]]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        if frames.len() > 1 {
            enc.set_animated(frames.len() as u32, 0).unwrap();
        }
        let mut wr = enc.write_header().unwrap();
        for c in frames {
            let px: Vec<u8> = c.repeat((w * h) as usize);
            wr.write_image_data(&px).unwrap();
        }
        wr.finish().unwrap();
    }
    out
}

/// 帧数（APNG 的 acTL；普通 PNG 为 1）。
fn png_frame_count(bytes: &[u8]) -> u32 {
    let r = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    r.info().animation_control.map_or(1, |a| a.num_frames)
}

const SKIN_THEME: &str = r#"[meta]
name = "皮肤"

[resources]
frame_main = { light = "assets/frame.png", dark = "assets/frame_dark.png" }

[skin.horizontal.frame]
image = "frame_main"
srcset = [{ image = "assets/frame@2x.png", density = 2 }]

[[skin.horizontal.decor]]
image = "assets/pet.png"
anchor = "frame.top_right"

[skin.horizontal.decor.states]
idle = { frames = "0-1", loop = true }
alt = { image = "assets/alt.png" }

[skin.horizontal.decor.on]
commit = { play = { image = "assets/jump.png" }, then = "idle" }
"#;

/// 皮肤主题目录：窗体走 resources（含暗色变体）+ srcset，装饰是 APNG，
/// states / on 各引用另一张图。
fn write_skin_theme(dir: &Path) {
    fs::create_dir_all(dir.join("assets")).unwrap();
    fs::write(dir.join(THEME_ENTRY_NAME), SKIN_THEME).unwrap();
    for (n, c) in [
        ("frame.png", [255, 0, 0, 255]),
        ("frame_dark.png", [0, 0, 0, 255]),
        ("frame@2x.png", [255, 0, 0, 255]),
        ("alt.png", [0, 255, 0, 255]),
        ("jump.png", [0, 0, 255, 255]),
    ] {
        fs::write(dir.join("assets").join(n), png_bytes(4, 4, c)).unwrap();
    }
    let pet = apng_bytes(4, 4, &[[255, 255, 0, 255], [0, 255, 255, 255]]);
    fs::write(dir.join("assets/pet.png"), pet).unwrap();
}

/// 导出 → 导入 → 按实机的解析规则（resources 键优先，否则主题目录 join 引用）
/// 找到每一张皮肤图，且 APNG 帧数原样保留。
#[test]
fn skin_theme_round_trips_and_resolves() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src");
    write_skin_theme(&src);
    let pkg = t.path().join("skin.wtheme");
    export_package(&src, &pkg, None).unwrap();
    assert_eq!(preview_package(&pkg).unwrap().asset_count, 6);

    let themes = t.path().join("themes");
    import_package(&pkg, &themes.join("skin"), |_| Ok(())).unwrap();

    for dark in [false, true] {
        let r =
            wind_theme::resolve::load_resolved_dirs(std::slice::from_ref(&themes), "skin", dark)
                .unwrap();
        // 同 wind-scene `theme_assets::asset_path` 的查找顺序。
        let locate = |reference: &str| -> PathBuf {
            r.resources
                .get(reference)
                .map(PathBuf::from)
                .unwrap_or_else(|| r.asset_dirs[0].join(reference))
        };
        let h = r.skin.as_ref().unwrap().horizontal.as_ref().unwrap();
        let frame = locate(&h.frame.image);
        assert!(frame.ends_with(if dark { "frame_dark.png" } else { "frame.png" }));
        assert!(frame.is_file());
        assert!(locate(&h.frame.srcset[0].0).is_file());

        let decor = &h.decor[0];
        let pet = fs::read(locate(&decor.image)).unwrap();
        assert_eq!(png_frame_count(&pet), 2, "动画装饰要原样带过去");
        let alt = decor.anim.states.iter().find(|(n, _)| n == "alt").unwrap();
        let wind_theme::skin::DecorClip::Image(alt) = &alt.1 else {
            panic!("alt 应是另一张图");
        };
        assert!(locate(alt).is_file());
        let Some((_, wind_theme::skin::DecorAction::Play { clip, .. })) = decor.anim.on.first()
        else {
            panic!("commit 应是一次性片段");
        };
        let wind_theme::skin::DecorClip::Image(jump) = clip else {
            panic!("commit 片段应是另一张图");
        };
        assert!(locate(jump).is_file());
    }
}

/// 引用越出 assets/：导出报错并指出是哪个引用，不自动搬运。
#[test]
fn export_rejects_skin_ref_outside_assets() {
    let t = tempfile::tempdir().unwrap();
    for bad in ["frame.png", "../frame.png", "/abs/frame.png"] {
        let src = t.path().join("src");
        write_skin_theme(&src);
        fs::write(src.join("frame.png"), png_bytes(2, 2, [1, 2, 3, 255])).unwrap();
        let text = SKIN_THEME.replace("assets/frame@2x.png", bad);
        fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains(bad), "要指出是哪个引用：{err}");
        assert!(err.contains(THEME_ASSETS_PREFIX), "要说清该放哪儿：{err}");
    }
}

/// resources 键指向的路径同样要在 assets/ 下。
#[test]
fn export_rejects_resource_path_outside_assets() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src");
    write_skin_theme(&src);
    let text = SKIN_THEME.replace("\"assets/frame_dark.png\"", "\"frame_dark.png\"");
    fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
    let err = export_package(&src, &t.path().join("o.wtheme"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("frame_dark.png"), "{err}");
}

#[test]
fn export_rejects_missing_skin_asset() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src");
    write_skin_theme(&src);
    fs::remove_file(src.join("assets/jump.png")).unwrap();
    let err = export_package(&src, &t.path().join("o.wtheme"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("assets/jump.png"), "{err}");
}

/// 包里缺皮肤引用的资源：预览就拦下，导入也不动已装的主题。
#[test]
fn import_rejects_package_missing_skin_asset() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("m.wtheme");
    let text = "[meta]\nname = \"缺图\"\n[skin.horizontal.frame]\nimage = \"assets/frame.png\"\n[[skin.horizontal.decor]]\nimage = \"assets/pet.png\"\n";
    let frame = png_bytes(2, 2, [1, 2, 3, 255]);
    write_zip(
        &pkg,
        &[
            (THEME_ENTRY_NAME, text.as_bytes()),
            ("assets/frame.png", &frame),
        ],
    );
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains("assets/pet.png"), "{err}");

    let dst = t.path().join("themes").join("x");
    fs::create_dir_all(&dst).unwrap();
    fs::write(dst.join(THEME_ENTRY_NAME), "[meta]\nname = \"原装\"\n").unwrap();
    let mut verified = false;
    let err = import_package(&pkg, &dst, |_| {
        verified = true;
        Ok(())
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("assets/pet.png"), "{err}");
    assert!(!verified, "缺图在落盘前就该拦下");
    assert_eq!(
        fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
        "[meta]\nname = \"原装\"\n"
    );
}

/// data: 内联图不占包内文件，不算缺失。
#[test]
fn skin_data_uri_needs_no_asset() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("d.wtheme");
    let text = "[meta]\nname = \"内联\"\n[skin.horizontal.frame]\nimage = \"data:image/svg+xml;base64,PHN2Zy8+\"\n";
    write_zip(&pkg, &[(THEME_ENTRY_NAME, text.as_bytes())]);
    preview_package(&pkg).unwrap();
}

const MIB: usize = 1024 * 1024;

#[test]
fn rejects_image_over_single_entry_limit() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("big.wtheme");
    let big = vec![0u8; MAX_THEME_IMAGE_BYTES as usize + 1];
    write_zip(
        &pkg,
        &[
            (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
            ("assets/big.png", &big),
        ],
    );
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains("assets/big.png"), "要指出是哪张图：{err}");
}

/// 旧上限 32 MiB 装不下、新上限 64 MiB 内的皮肤包能导入。
#[test]
fn accepts_total_between_old_and_new_limit() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("ok.wtheme");
    let img = vec![0u8; 14 * MIB];
    write_zip(
        &pkg,
        &[
            (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
            ("assets/bg.png", &img),
            ("assets/b.png", &img),
            ("assets/c.png", &img),
        ],
    );
    let dst = t.path().join("themes").join("x");
    import_package(&pkg, &dst, |_| Ok(())).unwrap();
    assert_eq!(
        fs::metadata(dst.join("assets/c.png")).unwrap().len(),
        img.len() as u64
    );
}

#[test]
fn rejects_total_over_limit() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("huge.wtheme");
    let img = vec![0u8; 14 * MIB];
    let names: Vec<String> = (0..5).map(|i| format!("assets/{i}.png")).collect();
    let mut entries: Vec<(&str, &[u8])> = vec![(THEME_ENTRY_NAME, THEME_TEXT.as_bytes())];
    entries.extend(names.iter().map(|n| (n.as_str(), img.as_slice())));
    write_zip(&pkg, &entries);
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains("过大"), "{err}");
}

/// PNG / WebP / GIF 用存储模式；文本与其它资源照旧 deflate。
#[test]
fn export_stores_compressed_images() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("s");
    fs::create_dir_all(src.join("assets")).unwrap();
    fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
    for n in ["bg.png", "a.WEBP", "b.gif", "c.svg"] {
        fs::write(src.join("assets").join(n), vec![b'x'; 4096]).unwrap();
    }
    let pkg = t.path().join("o.wtheme");
    export_package(&src, &pkg, Some(&png_bytes(2, 2, [0, 0, 0, 255]))).unwrap();

    let mut a = zip::ZipArchive::new(fs::File::open(&pkg).unwrap()).unwrap();
    let method = |a: &mut zip::ZipArchive<fs::File>, n: &str| a.by_name(n).unwrap().compression();
    use zip::CompressionMethod::{Deflated, Stored};
    for n in [
        "assets/bg.png",
        "assets/a.WEBP",
        "assets/b.gif",
        THEME_PREVIEW_NAME,
    ] {
        assert_eq!(method(&mut a, n), Stored, "{n}");
    }
    for n in [THEME_ENTRY_NAME, THEME_META_NAME, "assets/c.svg"] {
        assert_eq!(method(&mut a, n), Deflated, "{n}");
    }
}

/// 导出时引用与收集到的条目名精确比对：大小写不敏感的文件系统上 `is_file()` 会放行
/// `assets/Frame.png` → `frame.png`，做出来的包自己却导不回来。
#[test]
fn export_rejects_skin_ref_case_mismatch() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src");
    write_skin_theme(&src);
    let text = SKIN_THEME.replace("assets/alt.png", "assets/Alt.png");
    fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
    let err = export_package(&src, &t.path().join("o.wtheme"), None)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("assets/Alt.png") && err.contains("大小写"),
        "{err}"
    );
}

/// `./` 段规范化后按同一个文件处理，导出、导入都认。
#[test]
fn skin_ref_with_dot_segment_round_trips() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src");
    write_skin_theme(&src);
    let text = SKIN_THEME.replace("\"assets/pet.png\"", "\"./assets/./pet.png\"");
    fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
    let pkg = t.path().join("o.wtheme");
    export_package(&src, &pkg, None).unwrap();
    import_package(&pkg, &t.path().join("themes/x"), |_| Ok(())).unwrap();
}

/// 尾随点 / 空格、保留名：Windows 会解析成另一个文件，直接拒。
#[test]
fn export_rejects_skin_ref_windows_would_rename() {
    let t = tempfile::tempdir().unwrap();
    for bad in ["assets/pet.png.", "assets/pet.png ", "assets/NUL.png"] {
        let src = t.path().join("src");
        write_skin_theme(&src);
        let text = SKIN_THEME.replace("assets/pet.png", bad);
        fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains(bad), "{bad:?}：{err}");
    }
}

/// 子主题引用父主题 `[resources]` 的键：说清是资源键没定义，不说「不在 assets/ 下」。
#[test]
fn export_reports_undefined_resource_key() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join(THEME_ENTRY_NAME),
        "base = \"parent\"\n[meta]\nname = \"子\"\n[skin.horizontal.frame]\nimage = \"frame_main\"\n",
    )
    .unwrap();
    let err = export_package(&src, &t.path().join("o.wtheme"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("资源键「frame_main」未在本主题定义"), "{err}");
}

#[test]
fn export_rejects_oversized_image_before_writing() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("s");
    fs::create_dir_all(src.join("assets")).unwrap();
    fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
    fs::write(
        src.join("assets/big.png"),
        vec![0u8; MAX_THEME_IMAGE_BYTES as usize + 1],
    )
    .unwrap();
    let out = t.path().join("o.wtheme");
    let err = export_package(&src, &out, None).unwrap_err().to_string();
    assert!(err.contains("big.png"), "要指出是哪个文件：{err}");
    assert!(!out.exists(), "不该留下导入时必然被拒的包");
}

#[test]
fn export_rejects_total_over_limit() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("s");
    fs::create_dir_all(src.join("assets")).unwrap();
    fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
    let img = vec![0u8; 14 * MIB];
    for i in 0..5 {
        fs::write(src.join(format!("assets/{i}.png")), &img).unwrap();
    }
    let err = export_package(&src, &t.path().join("o.wtheme"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("总量过大"), "{err}");
}

#[test]
fn export_rejects_too_many_entries() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("s");
    fs::create_dir_all(src.join("assets")).unwrap();
    fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
    // package.toml + theme.toml + 199 张 = 201。
    for i in 0..MAX_THEME_ENTRIES - 1 {
        fs::write(src.join(format!("assets/{i}.png")), b"x").unwrap();
    }
    let err = export_package(&src, &t.path().join("o.wtheme"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("过多"), "{err}");
}

/// 只差大小写的资源：大小写敏感的文件系统上导得出，装到 Windows / macOS 却互相覆盖。
#[test]
fn export_rejects_case_only_duplicate_assets() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("s");
    fs::create_dir_all(src.join("assets")).unwrap();
    fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
    fs::write(src.join("assets/Bg.png"), b"a").unwrap();
    if fs::write(src.join("assets/bg.png"), b"b").is_err()
        || fs::read_dir(src.join("assets")).unwrap().count() < 2
    {
        return; // 大小写不敏感的文件系统上造不出这种目录。
    }
    let err = export_package(&src, &t.path().join("o.wtheme"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("只差大小写"), "{err}");
}

#[test]
fn import_rejects_case_only_duplicate_entries() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("c.wtheme");
    write_zip(
        &pkg,
        &[
            (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
            ("assets/bg.png", b"a"),
            ("assets/BG.png", b"b"),
        ],
    );
    let err = preview_package(&pkg).unwrap_err().to_string();
    assert!(err.contains("只差大小写"), "{err}");
}

#[test]
fn import_rejects_windows_reserved_names() {
    let t = tempfile::tempdir().unwrap();
    for bad in ["assets/NUL.png", "assets/con", "assets/x.png."] {
        let pkg = t.path().join("r.wtheme");
        write_zip(
            &pkg,
            &[(THEME_ENTRY_NAME, THEME_TEXT.as_bytes()), (bad, b"x")],
        );
        assert!(preview_package(&pkg).is_err(), "{bad} 该被拒");
    }
}

/// 撒谎的条目：中央目录与本地头都声明 1 KB，实际解压 17 MiB。规模前筛骗得过，
/// 真正读取时的有界读取必须拦下。
#[test]
fn import_bounds_actual_size_not_declared_size() {
    let t = tempfile::tempdir().unwrap();
    let pkg = t.path().join("bomb.wtheme");
    let name = "assets/bomb.bin";
    {
        let mut w = zip::ZipWriter::new(fs::File::create(&pkg).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file(THEME_ENTRY_NAME, opts).unwrap();
        w.write_all(THEME_TEXT.as_bytes()).unwrap();
        w.start_file(name, opts).unwrap();
        w.write_all(&vec![0u8; 17 * MIB]).unwrap();
        w.finish().unwrap();
    }
    // 改写 uncompressed size：本地头 sig 0x04034b50 偏移 22、名字在 30；
    // 中央目录 sig 0x02014b50 偏移 24、名字在 46。
    let mut b = fs::read(&pkg).unwrap();
    let mut patched = 0;
    for i in 0..b.len().saturating_sub(46 + name.len()) {
        let (size_at, name_at) = match &b[i..i + 4] {
            [0x50, 0x4b, 0x03, 0x04] => (22, 30),
            [0x50, 0x4b, 0x01, 0x02] => (24, 46),
            _ => continue,
        };
        if &b[i + name_at..i + name_at + name.len()] == name.as_bytes() {
            b[i + size_at..i + size_at + 4].copy_from_slice(&1024u32.to_le_bytes());
            patched += 1;
        }
    }
    assert_eq!(patched, 2, "本地头与中央目录各改一处");
    fs::write(&pkg, &b).unwrap();
    let declared = zip::ZipArchive::new(fs::File::open(&pkg).unwrap())
        .unwrap()
        .by_name(name)
        .unwrap()
        .size();
    assert_eq!(declared, 1024, "前筛看到的是撒谎的大小");
    preview_package(&pkg).expect("前筛按声明大小放行");

    let dst = t.path().join("themes/x");
    let err = import_package(&pkg, &dst, |_| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(err.contains(name) && err.contains("超过上限"), "{err}");
    assert!(!dst.exists());
}

/// 符号链接不跟随：成环不死循环，指向主题目录外的文件不进包。
#[cfg(unix)]
#[test]
fn export_skips_symlinks() {
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("s");
    fs::create_dir_all(src.join("assets")).unwrap();
    fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
    fs::write(src.join("assets/bg.png"), b"bg").unwrap();
    let outside = t.path().join("secret.txt");
    fs::write(&outside, b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, src.join("assets/leak.txt")).unwrap();
    std::os::unix::fs::symlink(src.join("assets"), src.join("assets/loop")).unwrap();
    std::os::unix::fs::symlink(t.path(), src.join("assets/up")).unwrap();

    let pkg = t.path().join("o.wtheme");
    export_package(&src, &pkg, None).unwrap();
    let names: Vec<String> = zip::ZipArchive::new(fs::File::open(&pkg).unwrap())
        .unwrap()
        .file_names()
        .map(String::from)
        .collect();
    assert!(names.contains(&"assets/bg.png".to_string()));
    assert!(
        names
            .iter()
            .all(|n| !n.contains("leak") && !n.contains("loop") && !n.contains("up/")),
        "{names:?}"
    );
}
