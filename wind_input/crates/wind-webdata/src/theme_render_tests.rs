use super::*;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ui_types::{CandidatePreviewImage, CandidatePreviewRequest, UiCommand};

/// 在途名额是全进程的：动到它的用例串行跑，免得互相挤占出「忙」。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wind_theme_render_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prepared(dirs: Vec<PathBuf>, slug: &str, staged: bool) -> Prepared {
    Prepared {
        prefix: slug.into(),
        dirs,
        slug: slug.into(),
        staged,
        is_dark: false,
        kind: wind_ui_types::PreviewKind::Candidate,
        vertical: false,
        scale: 1.0,
        fit_w: None,
        fit_h: None,
        inline: false,
        staging: None,
        draft: None,
    }
}

/// 两层主题：`base_t`（带一张资源）← `child`。
fn two_themes(root: &Path) {
    let b = root.join("base_t");
    std::fs::create_dir_all(b.join("assets")).unwrap();
    std::fs::write(b.join("theme.toml"), "[meta]\nname = \"b\"\n").unwrap();
    std::fs::write(b.join("assets/bg.png"), b"one").unwrap();
    let c = root.join("child");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::write(
        c.join("theme.toml"),
        "base = \"base_t\"\n[meta]\nname = \"c\"\n",
    )
    .unwrap();
}

fn key(v: u32, stamp: &str, fp: &str, p: &Prepared) -> String {
    cache_key(v, stamp, fp, p).unwrap().unwrap()
}

#[test]
fn key_covers_text_assets_base_overrides_params_and_build() {
    for staged in [true, false] {
        let root = tmp(&format!("key{staged}"));
        two_themes(&root);
        let dirs = vec![root.clone()];
        let p = prepared(dirs.clone(), "child", staged);
        let k0 = key(1, "b", "fp", &p);
        assert_eq!(k0, key(1, "b", "fp", &p), "同输入同键");
        assert_eq!(k0.len(), 32);
        assert_ne!(k0, key(2, "b", "fp", &p), "渲染器版本");
        assert_ne!(k0, key(1, "b2", "fp", &p), "构建标识");
        assert_ne!(k0, key(1, "b", "fp2", &p), "外观覆盖");
        let mut q = prepared(dirs.clone(), "child", staged);
        q.is_dark = true;
        assert_ne!(k0, key(1, "b", "fp", &q), "明暗");
        let mut q = prepared(dirs.clone(), "child", staged);
        q.vertical = true;
        assert_ne!(k0, key(1, "b", "fp", &q), "横竖");
        let mut q = prepared(dirs.clone(), "child", staged);
        q.scale = 1.5;
        assert_ne!(k0, key(1, "b", "fp", &q), "缩放");
        let mut q = prepared(dirs.clone(), "child", staged);
        q.fit_w = Some(584);
        assert_ne!(k0, key(1, "b", "fp", &q), "fit_w 进键");
        let mut q = prepared(dirs.clone(), "child", staged);
        q.fit_h = Some(184);
        assert_ne!(k0, key(1, "b", "fp", &q), "fit_h 进键");
        // base 链上的资源变了（同长度内容 + 推后 mtime，元数据指纹也看得见）
        let bg = root.join("base_t/assets/bg.png");
        std::fs::write(&bg, b"two").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&bg).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        let k1 = key(1, "b", "fp", &p);
        assert_ne!(k0, k1, "base 资源（staged={staged}）");
        std::fs::write(
            root.join("child/theme.toml"),
            "base = \"base_t\"\n[meta]\nname = \"c2\"\n",
        )
        .unwrap();
        assert_ne!(k1, key(1, "b", "fp", &p), "主题文本（staged={staged}）");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn staged_key_is_location_independent_and_bounded() {
    let a = tmp("loc_a");
    let b = tmp("loc_b");
    two_themes(&a);
    two_themes(&b);
    assert_eq!(
        key(1, "", "fp", &prepared(vec![a.clone()], "child", true)),
        key(1, "", "fp", &prepared(vec![b.clone()], "child", true)),
        "内容指纹只看相对路径与字节"
    );
    // 目录符号链接被跳过，不当文件读、不报错。
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&a, a.join("child/loop")).unwrap();
        assert!(cache_key(1, "", "fp", &prepared(vec![a.clone()], "child", true)).is_ok());
    }
    // 文件数超预算：不缓存。
    let mut budget = HashBudget {
        files: 1,
        bytes: MAX_HASH_BYTES,
    };
    let mut h = siphasher::sip::SipHasher13::new();
    assert!(!hash_dir(&mut h, &a.join("base_t"), true, &mut budget).unwrap());
    let _ = std::fs::remove_dir_all(a);
    let _ = std::fs::remove_dir_all(b);
}

fn touch(path: &Path, bytes: usize, age: Duration, now: SystemTime) {
    std::fs::write(path, vec![0u8; bytes]).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(now - age).unwrap();
}

const K: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn prune_drops_idle_then_oldest_over_budget() {
    let dir = tmp("prune");
    let now = SystemTime::now();
    let day = Duration::from_secs(86400);
    let f = |n: &str| dir.join(format!("{n}.{K}.png"));
    touch(&f("idle"), 10, day * 40, now);
    touch(&f("old"), 60, day * 3, now);
    touch(&f("mid"), 60, day * 2, now);
    touch(&f("new"), 60, day, now);
    touch(&f("fresh"), 60, day * 5, now);
    touch(&dir.join(format!("x.{K}.tmp-1-0")), 5, day, now);
    touch(&dir.join("x.png.tmp-1-0"), 5, day, now);
    touch(&dir.join("keep.txt"), 500, day * 90, now);
    touch(&dir.join("other.png"), 500, day * 90, now);
    // fresh 是「本批刚写的」：哪怕 mtime 最旧也不删，但算进总量。
    let n = prune_cache(&dir, now, day * 30, 130, &[f("fresh")]).unwrap();
    assert_eq!(n, 4, "闲置 1 + 超量淘汰最旧 2 + 残留临时文件 1");
    assert!(!f("idle").exists() && !f("old").exists() && !f("mid").exists());
    assert!(f("new").exists() && f("fresh").exists());
    assert!(dir.join("x.png.tmp-1-0").exists(), "形态不对的临时文件不碰");
    assert!(dir.join("keep.txt").exists() && dir.join("other.png").exists());
    assert_eq!(
        prune_cache(&dir.join("absent"), now, day, 1, &[]).unwrap(),
        0
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn prune_removes_stale_staging_dirs() {
    let dir = tmp("prune_staging");
    let st = dir.join("staging/123-0-0/_render_preview");
    std::fs::create_dir_all(&st).unwrap();
    std::fs::write(st.join("theme.toml"), "x").unwrap();
    let now = SystemTime::now();
    assert_eq!(
        prune_cache(&dir, now, PREVIEW_MAX_IDLE, u64::MAX, &[]).unwrap(),
        0
    );
    assert!(st.exists(), "新鲜的解包目录可能正在用，不删");
    let later = now + STAGING_STALE + Duration::from_secs(60);
    assert_eq!(
        prune_cache(&dir, later, PREVIEW_MAX_IDLE, u64::MAX, &[]).unwrap(),
        1
    );
    assert!(!dir.join("staging/123-0-0").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn remove_theme_previews_matches_prefix_exactly() {
    let dir = tmp("forget");
    let zh = file_prefix("主题");
    for n in [
        format!("a.{K}.png"),
        format!("a.b.{K}.png"),
        format!("ab.{K}.png"),
        format!("{zh}.{K}.png"),
        "a.short.png".to_string(),
    ] {
        std::fs::write(dir.join(n), b"x").unwrap();
    }
    assert_eq!(remove_theme_previews(&dir, "a"), 1);
    assert!(dir.join(format!("a.b.{K}.png")).exists());
    assert!(dir.join(format!("ab.{K}.png")).exists());
    assert!(dir.join("a.short.png").exists());
    assert_eq!(remove_theme_previews(&dir, "主题"), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn file_prefix_is_filename_safe_and_distinct() {
    assert_eq!(file_prefix("my-theme.v2_x"), "my-theme.v2_x");
    let a = file_prefix("主题甲");
    let b = file_prefix("主题乙");
    assert_ne!(a, b);
    for p in [&a, &b] {
        assert!(
            p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        );
    }
}

#[test]
fn tmp_name_matching_is_exact() {
    assert!(is_preview_tmp(&format!("a.b.{K}.tmp-12-3")));
    assert!(!is_preview_tmp(&format!("a.{K}.tmp-12")));
    assert!(!is_preview_tmp("a.png.tmp-1-0"));
    assert!(!is_preview_tmp(&format!("a.{K}.tmp-x-1")));
}

#[test]
fn png_header_and_completeness() {
    let png = fake_png(300, 40);
    assert_eq!(png_size(&png), Some((300, 40)));
    assert!(png_complete(&png));
    assert!(!png_complete(&png[..png.len() - 1]));
    assert_eq!(png_size(b"not a png at all, no....."), None);
}

/// 结构完整的假 PNG（签名 + IHDR + IEND；只用到宽高与收尾校验）。
fn fake_png(w: u32, h: u32) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]);
    b
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data")
}

/// 真实主题目录 + 假 UI 线程：`reply` 为真时每个预览请求回一张假图（宽 = 序号），否则只收不回；
/// 收到的请求都留在 `held` 里（看取消标志用）。
type Held = Arc<std::sync::Mutex<Vec<Box<CandidatePreviewRequest>>>>;

fn coord_with_fake_ui(reply: bool) -> (Arc<Coordinator>, Arc<AtomicU64>, Held) {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), Some(&data_dir()));
    let seen = Arc::new(AtomicU64::new(0));
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (s, hd) = (Arc::clone(&seen), Arc::clone(&held));
    std::thread::spawn(move || {
        while let Ok(cmd) = rx.recv() {
            if let UiCommand::RenderCandidatePreview(req) = cmd {
                let n = s.fetch_add(1, Ordering::SeqCst) as u32 + 1;
                // 先存请求再回执：调用方一收到回执就会去 `held` 里取这条请求。
                let (tx, vertical) = (req.reply.clone(), req.vertical);
                hd.lock().unwrap().push(req);
                if reply {
                    let w = if vertical { 7 } else { 100 + n };
                    let _ = tx.try_send(Ok(CandidatePreviewImage {
                        width: w,
                        height: 30,
                        png: fake_png(w, 30),
                    }));
                }
            }
        }
    });
    (c, seen, held)
}

fn ok(v: &Value) -> bool {
    v["ok"].as_bool() == Some(true)
}

#[test]
fn batch_renders_misses_once_then_hits_cache() {
    let _g = serial();
    let (c, seen, _) = coord_with_fake_ui(true);
    let cache = tmp("batch");
    let mut items = vec![
        json!({ "name": "default", "style": "light" }),
        json!({ "name": "default", "style": "dark" }),
        json!({ "name": "default", "layout": "vertical", "inline": true }),
        json!({ "name": "no-such-theme" }),
        json!({ "name": "default", "scale": 9 }),
        json!({ "name": "default", "text": "x" }),
        json!({ "name": "../default" }),
        json!({ "text": "x".repeat(MAX_TEXT_BYTES + 1) }),
    ];
    // 超过在途上限的未命中：按波次投递，全都画出来，不回「忙」。
    for s in [0.5, 0.75, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 3.5, 4.0] {
        items.push(json!({ "name": "jade", "scale": s }));
    }
    let first = render_batch(c.as_ref(), &items, Some(&cache));
    assert_eq!(first.len(), items.len());
    assert!(ok(&first[0]) && ok(&first[1]) && ok(&first[2]), "{first:?}");
    for (i, v) in first.iter().enumerate().take(8).skip(3) {
        assert!(!ok(v), "第 {i} 条应失败：{v}");
    }
    assert!(first[8..].iter().all(ok), "{first:?}");
    assert_eq!(seen.load(Ordering::SeqCst), 13, "未命中各渲染一次");
    assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 0, "名额全部归还");
    assert_ne!(first[0]["hash"], first[1]["hash"]);
    assert_eq!(first[2]["width"], 7);
    assert!(first[2]["png"].is_string(), "inline 要带 base64");
    assert!(first[0].get("png").is_none());
    let path = PathBuf::from(first[0]["path"].as_str().unwrap());
    assert!(path.starts_with(&cache) && path.is_file());
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("default.")
    );
    assert_eq!(first[0]["cached"], false);

    let again = render_batch(c.as_ref(), &items[..3], Some(&cache));
    assert_eq!(seen.load(Ordering::SeqCst), 13, "全部命中，不再请求 UI");
    for (a, b) in first.iter().zip(&again) {
        assert_eq!(a["hash"], b["hash"]);
        assert_eq!(a["width"], b["width"], "宽高从 PNG 头读回");
        assert_eq!(b["cached"], true);
    }
    // 缓存文件坏了（截断）：删掉重画。
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 4]).unwrap();
    let fixed = render_batch(c.as_ref(), &items[..1], Some(&cache));
    assert_eq!(fixed[0]["cached"], false);
    assert_eq!(seen.load(Ordering::SeqCst), 14);
    assert!(png_complete(&std::fs::read(&path).unwrap()));
    assert_eq!(remove_theme_previews(&cache, "default"), 3);
    let _ = std::fs::remove_dir_all(cache);
}

/// `kind` 参数：缺省 = 候选窗（不带 kind 的旧请求行为不变）、非法值报错不出图、
/// `toolbar` 进请求结构；缺省朝向按形态取各自的当前配置（工具栏看 `ui.toolbar.vertical`），
/// `layout` 参数对两种形态都仍然生效。
#[test]
fn kind_param_defaults_validates_and_rides_request() {
    let _g = serial();
    let (c, _seen, held) = coord_with_fake_ui(true);
    let cache = tmp("kind");
    use wind_coordinator::web_host::WebDataHost;
    // inline=true：一律进 UI 线程，不落盘，防串台。
    let one = |item: Value| {
        let mut it = item;
        it["inline"] = json!(true);
        render_batch(c.as_ref(), &[it], Some(&cache)).pop().unwrap()
    };

    let bad = one(json!({ "name": "default", "kind": "bar" }));
    assert!(!ok(&bad), "{bad:?}");
    assert!(
        bad["error"].as_str().unwrap().starts_with("kind 只能是"),
        "{bad:?}"
    );
    assert!(
        held.lock().unwrap().is_empty(),
        "坏 kind 在 prepare 阶段就拒，不出图"
    );

    let out = one(json!({ "name": "default" }));
    assert!(ok(&out), "{out:?}");
    let req = held.lock().unwrap().pop().unwrap();
    assert_eq!(
        req.kind,
        wind_ui_types::PreviewKind::Candidate,
        "缺省 = 候选窗"
    );

    let out = one(json!({ "name": "default", "kind": "toolbar" }));
    assert!(ok(&out), "{out:?}");
    let req = held.lock().unwrap().pop().unwrap();
    assert_eq!(req.kind, wind_ui_types::PreviewKind::Toolbar);
    assert_eq!(
        req.vertical,
        c.toolbar_preview_vertical(),
        "缺省朝向取 ui.toolbar.vertical"
    );

    let out = one(json!({
        "name": "default",
        "kind": "toolbar",
        "layout": "vertical"
    }));
    assert!(ok(&out), "{out:?}");
    let req = held.lock().unwrap().pop().unwrap();
    assert!(req.vertical, "layout 参数仍然生效");
    let _ = std::fs::remove_dir_all(cache);
}

/// 缓存键含形态：同一主题同一参数下，候选窗与工具栏的键不同，缓存文件不互顶。
#[test]
fn cache_key_differs_between_kinds() {
    let root = tmp("kindkey");
    two_themes(&root);
    let mut p = prepared(vec![root], "child", false);
    let k0 = key(1, "b", "fp", &p);
    p.kind = wind_ui_types::PreviewKind::Toolbar;
    assert_ne!(k0, key(1, "b", "fp", &p), "形态要进缓存键");
}

/// `fit_w` / `fit_h` 参数：缺省不进请求（不约束，行为与旧请求一致）、可只给其一、
/// 非整数 / 0 / 负数 / 超出 u32 整条报错不出图（kind 同款：prepare 阶段就拒）。
#[test]
fn fit_param_defaults_validates_and_rides_request() {
    let _g = serial();
    let (c, _seen, held) = coord_with_fake_ui(true);
    let cache = tmp("fit");
    // inline=true：一律进 UI 线程，不落盘，防串台。
    let one = |item: Value| {
        let mut it = item;
        it["inline"] = json!(true);
        render_batch(c.as_ref(), &[it], Some(&cache)).pop().unwrap()
    };

    for (v, why) in [
        (json!(0), "0"),
        (json!(-3), "负数"),
        (json!(2.5), "非整数"),
        (json!("584"), "字符串"),
        (json!(u64::MAX), "超出 u32"),
    ] {
        let bad = one(json!({ "name": "default", "fit_w": v }));
        assert!(!ok(&bad), "{why} 应报错：{bad:?}");
        assert!(
            bad["error"]
                .as_str()
                .unwrap()
                .starts_with("fit_w 只能是正整数"),
            "{why} 文案：{bad:?}"
        );
    }
    assert!(
        held.lock().unwrap().is_empty(),
        "坏 fit 在 prepare 阶段就拒，不出图"
    );

    let out = one(json!({ "name": "default" }));
    assert!(ok(&out), "{out:?}");
    let req = held.lock().unwrap().pop().unwrap();
    assert_eq!(req.fit_w, None, "缺省不约束");
    assert_eq!(req.fit_h, None);

    let out = one(json!({ "name": "default", "fit_w": 584, "fit_h": 184 }));
    assert!(ok(&out), "{out:?}");
    let req = held.lock().unwrap().pop().unwrap();
    assert_eq!(req.fit_w, Some(584));
    assert_eq!(req.fit_h, Some(184));

    let out = one(json!({ "name": "default", "fit_h": 184 }));
    assert!(ok(&out), "{out:?}");
    let req = held.lock().unwrap().pop().unwrap();
    assert_eq!(req.fit_w, None, "只给一维");
    assert_eq!(req.fit_h, Some(184));
    let _ = std::fs::remove_dir_all(cache);
}

/// 带草稿 `ui` 的请求：草稿外观随请求到 UI 线程，结果内联、不落盘；坏参数报错。
#[test]
fn draft_ui_rides_request_and_skips_disk_cache() {
    let _g = serial();
    let (c, seen, held) = coord_with_fake_ui(true);
    let cache = tmp("draft");
    let ui = json!({
        "candidate": { "font_size": 999, "per_page": 3, "layout": "vertical", "syllable_separator": " " },
        "font": { "family": "Noto Sans", "weight": "粗" },
        "comment": { "enabled": false },
        "no_such": 1,
    });
    let out = render_batch(
        c.as_ref(),
        &[
            json!({ "name": "default", "ui": ui }),
            json!({ "name": "default", "ui": 3 }),
            json!({ "name": "default", "ui": { "font": { "family": "x".repeat(MAX_DRAFT_BYTES) } } }),
        ],
        Some(&cache),
    );
    assert!(ok(&out[0]), "{out:?}");
    assert!(out[0]["png"].is_string(), "草稿预览一律内联");
    assert!(out[0].get("path").is_none_or(Value::is_null), "{}", out[0]);
    assert!(!ok(&out[1]) && !ok(&out[2]), "{out:?}");
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    let leftovers = std::fs::read_dir(&cache)
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.path().is_file())
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "草稿预览不写缓存");

    let req = held.lock().unwrap().pop().unwrap();
    assert!(req.vertical, "layout 缺省取草稿的排列");
    assert_eq!(req.candidates.len(), 3);
    assert_eq!(req.preedit, "ni hao", "示例编码按草稿的音节分隔符显示");
    assert!(
        req.candidates.iter().all(|c| c.comment.is_empty()),
        "注释关"
    );
    let a = req.appearance.as_ref().expect("带草稿外观");
    assert_eq!(a.font_size, 72.0, "字号夹到设置页上限");
    assert_eq!(a.font_family, "Noto Sans");
    assert_eq!(a.font_weight, 0, "类型不符的叶子丢弃，回落当前值");
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn package_and_text_sources_render_and_clean_staging() {
    let _g = serial();
    let (c, seen, _) = coord_with_fake_ui(true);
    let cache = tmp("pkg");
    let themes = data_dir().join("themes");
    let pkg = cache.join("jade.wtheme");
    wind_transfer::theme::export_package(&themes.join("jade"), &pkg, None).unwrap();
    let text = std::fs::read_to_string(themes.join("jade/theme.toml")).unwrap();
    let items = [
        json!({ "package": pkg.to_string_lossy() }),
        json!({ "text": text }),
        json!({ "text": "this is = = not toml" }),
        json!({ "package": cache.join("missing.wtheme").to_string_lossy() }),
        json!({ "text": "base = \"../../etc\"\n" }),
    ];
    let out = render_batch(c.as_ref(), &items, Some(&cache));
    assert!(ok(&out[0]) && ok(&out[1]), "{out:?}");
    assert!(!ok(&out[2]) && !ok(&out[3]) && !ok(&out[4]), "{out:?}");
    let name = |v: &Value| {
        Path::new(v["path"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    assert!(name(&out[0]).starts_with("_pkg.") && name(&out[1]).starts_with("_text."));
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    // 同一个包再来一次：解到另一个临时目录，键相同，命中。
    let again = render_batch(c.as_ref(), &items[..1], Some(&cache));
    assert_eq!(again[0]["hash"], out[0]["hash"]);
    assert_eq!(again[0]["cached"], true);
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    let staging: Vec<_> = std::fs::read_dir(cache.join("staging"))
        .map(|r| r.flatten().collect())
        .unwrap_or_default();
    assert!(staging.is_empty(), "临时解包目录用完即删");
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn timeout_reports_error_cancels_request_and_cleans_staging() {
    let _g = serial();
    let (c, _, held) = coord_with_fake_ui(false);
    let cache = tmp("timeout");
    let text = std::fs::read_to_string(data_dir().join("themes/jade/theme.toml")).unwrap();
    let fast = Timeouts {
        base: Duration::from_millis(200),
        per_miss: Duration::ZERO,
        max: Duration::from_secs(1),
    };
    let t = Instant::now();
    let out = render_batch_with(c.as_ref(), &[json!({ "text": text })], Some(&cache), fast);
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(
        out[0]["error"].as_str().unwrap().contains("超时"),
        "{out:?}"
    );
    let held = held.lock().unwrap();
    assert_eq!(held.len(), 1);
    assert!(held[0].cancelled(), "超时后置位取消，UI 线程排到它就丢掉");
    assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 0);
    let staging: Vec<_> = std::fs::read_dir(cache.join("staging"))
        .map(|r| r.flatten().collect())
        .unwrap_or_default();
    assert!(staging.is_empty(), "超时也删临时目录");
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn busy_when_other_requests_hold_all_slots() {
    let _g = serial();
    let held: Vec<Slot> = (0..MAX_IN_FLIGHT)
        .map(|_| Slot::acquire().unwrap())
        .collect();
    let (c, seen, _) = coord_with_fake_ui(true);
    let cache = tmp("busy");
    let out = render_batch(c.as_ref(), &[json!({ "name": "default" })], Some(&cache));
    assert!(out[0]["error"].as_str().unwrap().contains("忙"), "{out:?}");
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    drop(held);
    assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 0);
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn dispatch_routes_and_validates_batch_shape() {
    use crate::WebDataRpc;
    // 只走参数校验失败的分支：成功路径会落到真实缓存目录，由 render_batch 的用例覆盖。
    let c = Coordinator::new_headless(Config::default(), None);
    let e = c
        .web_data_rpc("theme.renderPreviews", &json!({}))
        .unwrap_err();
    assert!(e.to_string().contains("items"), "{e}");
    let many: Vec<Value> = (0..=MAX_BATCH).map(|_| json!({})).collect();
    let e = c
        .web_data_rpc("theme.renderPreviews", &json!({ "items": many }))
        .unwrap_err();
    assert!(e.to_string().contains("最多"), "{e}");
    let e = c
        .web_data_rpc("theme.delete", &json!({ "name": "../../x" }))
        .unwrap_err();
    assert!(e.to_string().contains("slug"), "{e}");
}

#[test]
fn without_ui_thread_reports_error_instead_of_hanging() {
    let _g = serial();
    let c = Coordinator::new_headless(Config::default(), Some(&data_dir()));
    let cache = tmp("noui");
    let t = Instant::now();
    let out = render_batch(c.as_ref(), &[json!({ "name": "default" })], Some(&cache));
    assert!(!ok(&out[0]));
    assert!(t.elapsed() < Duration::from_secs(2), "通道断开应立即返回");
    let _ = std::fs::remove_dir_all(cache);
}
