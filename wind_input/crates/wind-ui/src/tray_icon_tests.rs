use super::*;

#[test]
fn icon_name_is_ascii_and_matches_addon_samples() {
    // 与 wind_linux/tests/host_ui_test.cpp 的「运行时图标名」用例同一组样例（那边读本文件核对
    // 下面这些字面量）。正式版名字与改版前逐字相同：已装机用户磁盘上的运行时图标照认。
    assert_eq!(
        icon_name(false, Variant::Chinese, "拼"),
        "windinput-lbl-zh-e68bbc"
    );
    assert_eq!(
        icon_name(false, Variant::English, "英"),
        "windinput-lbl-en-e88bb1"
    );
    assert_eq!(
        icon_name(false, Variant::Caps, "A"),
        "windinput-lbl-caps-41"
    );
    assert_eq!(
        icon_name(false, Variant::English, "En"),
        "windinput-lbl-en-456e"
    );
    assert_eq!(
        icon_name(true, Variant::Chinese, "拼"),
        "windinput-lbl-dev-zh-e68bbc"
    );
    // 超长标签走散列：40 个「虎」（120 字节）。
    let long = "虎".repeat(40);
    assert_eq!(
        icon_name(false, Variant::Chinese, &long),
        "windinput-lbl-zh-h8b27ed0fc2ac336d"
    );
    // 32 字节恰好还是十六进制，33 字节起散列。
    assert_eq!(
        icon_name(false, Variant::Caps, &"a".repeat(32)),
        format!("windinput-lbl-caps-{}", "61".repeat(32))
    );
    assert!(icon_name(false, Variant::Caps, &"a".repeat(33)).starts_with("windinput-lbl-caps-h"));
}

/// 文件名不超过 255 字节，不论标签多长。
#[test]
fn icon_file_name_fits_name_max() {
    for n in [1, 10, 11, 32, 33, 100, 1000] {
        let name = icon_name(true, Variant::Caps, &"虎".repeat(n));
        assert!(name.len() + ".png".len() <= 255, "{n}: {}", name.len());
        assert!(name.is_ascii());
    }
}

/// 两个变体各认各的：正式版前缀是 dev 前缀的前缀，dev 的名字不能被正式版认领（反之亦然）；
/// 种子与别的应用的文件谁都不认。
#[test]
fn owns_distinguishes_variants() {
    let rel = icon_prefix(false);
    let dev = icon_prefix(true);
    assert!(owns(rel, "windinput-lbl-zh-e68bbc"));
    assert!(owns(rel, "windinput-lbl-caps-h8b27ed0fc2ac336d"));
    assert!(!owns(rel, "windinput-lbl-dev-zh-e68bbc"));
    assert!(owns(dev, "windinput-lbl-dev-zh-e68bbc"));
    assert!(!owns(dev, "windinput-lbl-zh-e68bbc"));
    for foreign in [
        "windinput-zh",
        "windinput",
        "windinput-lbl-xx-41",
        "windinput-lbl-zh-",
        "windinput-lbl-zh-4G",
        "windinput-lbl-zh-h123",
    ] {
        assert!(!owns(rel, foreign), "{foreign}");
        assert!(!owns(dev, foreign), "{foreign}");
    }
}

#[test]
fn pixel_art_rows_are_rectangular_and_fit() {
    for (ch, rows) in PIXEL_16 {
        let w = rows[0].chars().count();
        assert!(
            rows.iter().all(|r| r.chars().count() == w),
            "「{ch}」行宽不一致"
        );
        assert!(w <= 14 && rows.len() <= 14, "「{ch}」超出 16px 底板的内框");
    }
}

#[test]
fn pixel_art_only_for_single_known_char_at_16() {
    assert!(pixel_mask("中", 16).is_some());
    assert!(pixel_mask("中", 22).is_none());
    assert!(pixel_mask("虎", 16).is_none());
    assert!(pixel_mask("En", 16).is_none());
}

#[test]
fn user_dir_follows_xdg_data_home() {
    // 只读环境变量的纯函数难以隔离；这里只验结构：以 icons/hicolor 结尾。
    if let Some(d) = user_hicolor_dir() {
        assert!(d.ends_with("icons/hicolor"));
    }
}

/// 真渲染 + 落盘 + 清理（需要系统有 CJK 字体；本仓 Linux 形态的文字测试都有这个前提）。
#[test]
fn ensure_writes_every_size_once_and_prune_keeps_only_wanted() {
    let tmp = std::env::temp_dir().join(format!("wi-tray-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut t = TrayIcons::new(tmp.clone(), false);
    t.create_dirs().unwrap();
    assert!(tmp.join("48x48/apps").is_dir());
    let before = std::fs::metadata(&tmp).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let name = t.ensure(Variant::Chinese, "五").unwrap().unwrap();
    for &n in SIZES {
        let p = tmp.join(format!("{n}x{n}/apps/{name}.png"));
        let img = image::open(&p).expect("合法 PNG").to_rgba8();
        assert_eq!((img.width(), img.height()), (n as u32, n as u32));
    }
    // 写出新图标后主题根的 mtime 前进（GTK / GNOME Shell 据它重扫）。
    assert!(std::fs::metadata(&tmp).unwrap().modified().unwrap() > before);
    // 自定义标签（没有点阵）也画得出来，且不是空白：中心附近有白字像素。
    let custom = t.ensure(Variant::Chinese, "虎").unwrap().unwrap();
    let img = image::open(tmp.join(format!("24x24/apps/{custom}.png")))
        .unwrap()
        .to_rgba8();
    assert!(img.pixels().any(|p| p[0] > 200 && p[1] > 200 && p[2] > 200));
    // 空标签不画。
    assert_eq!(t.ensure(Variant::English, "").unwrap(), None);
    // 缓存命中但文件被别人删了：补查发现、重画（不会一直以为它在）。
    std::fs::remove_file(tmp.join(format!("64x64/apps/{name}.png"))).unwrap();
    assert_eq!(t.ensure(Variant::Chinese, "五").unwrap().unwrap(), name);
    assert!(tmp.join(format!("64x64/apps/{name}.png")).is_file());
    // 别人的文件不碰：种子、另一个变体（dev）的运行时图标。
    let foreign = tmp.join("16x16/apps/windinput-zh.png");
    std::fs::write(&foreign, b"x").unwrap();
    let other_variant = tmp.join("16x16/apps/windinput-lbl-dev-zh-e4ba94.png");
    std::fs::write(&other_variant, b"x").unwrap();
    // 本进程备好过的（ready）不在 keep 里也留着：启动清理在后台跑，期间前台可能刚写出新标签。
    let keep: HashSet<String> = [name.clone()].into_iter().collect();
    assert_eq!(t.prune(&keep), 0, "custom 是本进程备好的，不删");
    let mut fresh = TrayIcons::new(tmp.clone(), false);
    assert_eq!(fresh.prune(&keep), SIZES.len(), "新进程只保留 keep");
    assert!(tmp.join(format!("16x16/apps/{name}.png")).is_file());
    assert!(!tmp.join(format!("16x16/apps/{custom}.png")).exists());
    assert!(foreign.is_file());
    assert!(other_variant.is_file(), "正式版不删 dev 的图标");
    // dev 那边清理时同样不碰正式版的。
    let mut dev = TrayIcons::new(tmp.clone(), true);
    assert_eq!(dev.prune(&HashSet::new()), 1);
    assert!(!other_variant.exists());
    assert!(tmp.join(format!("16x16/apps/{name}.png")).is_file());
    let _ = std::fs::remove_dir_all(&tmp);
}
