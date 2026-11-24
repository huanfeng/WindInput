use super::*;

const WHITE: Rgba = [255, 255, 255, 255];
const BLACK: Rgba = [0, 0, 0, 255];

#[test]
fn contrast_ratio_extremes_and_symmetry() {
    assert_eq!(contrast_ratio(BLACK, WHITE), 21.0);
    assert_eq!(contrast_ratio(WHITE, BLACK), 21.0, "与参数顺序无关");
    assert_eq!(contrast_ratio([10, 20, 30, 255], [10, 20, 30, 255]), 1.0);
    assert_eq!(
        contrast_ratio([0, 0, 0, 0], WHITE),
        21.0,
        "只看 RGB、忽略 alpha"
    );
}

/// 3:1 两侧最近的灰：`#949494` 对白底刚过、`#959595` 刚不过（WCAG 公式的已知值）。
#[test]
fn contrast_ratio_grays_around_three() {
    let above = contrast_ratio([0x94, 0x94, 0x94, 255], WHITE);
    let below = contrast_ratio([0x95, 0x95, 0x95, 255], WHITE);
    assert!((above - 3.0335).abs() < 1e-4 && above > 3.0, "{above}");
    assert!((below - 2.9953).abs() < 1e-4 && below < 3.0, "{below}");
}

/// 恰为阈值算达标（`>=`），高出一点点的阈值不达标。实色对的对比度凑不出恰好 3.0，故用
/// 某对颜色自身的对比度当阈值。
#[test]
fn meets_contrast_is_inclusive_at_threshold() {
    let (a, b) = ([0x94, 0x94, 0x94, 255], WHITE);
    let exact = contrast_ratio(a, b);
    assert!(meets_contrast(a, b, exact));
    assert!(!meets_contrast(a, b, exact * (1.0 + f64::EPSILON)));
    assert!(meets_contrast(BLACK, WHITE, 21.0));
}

#[test]
fn composite_over_blends_by_top_alpha() {
    let red_half = [255, 0, 0, 128];
    assert_eq!(
        composite_over(red_half, [0, 0, 255, 255]),
        [128, 0, 127, 255]
    );
    assert_eq!(
        composite_over([1, 2, 3, 255], WHITE),
        [1, 2, 3, 255],
        "不透明顶层整体替换"
    );
    assert_eq!(
        composite_over([1, 2, 3, 0], [9, 8, 7, 255]),
        [9, 8, 7, 255],
        "全透明顶层不改底"
    );
    assert_eq!(
        composite_over([0, 0, 0, 51], [200, 100, 50, 128])[3],
        255,
        "结果恒不透明"
    );
}

#[test]
fn test_parse_hex() {
    assert_eq!(parse_hex("#FF8040"), Some([255, 128, 64, 255]));
    assert_eq!(parse_hex("#00000080"), Some([0, 0, 0, 128]));
    assert_eq!(parse_hex("nope"), None);
    // 非 ASCII：6 字节的「红红」长度凑得上 6 位分支，按字节切片会切在字中间而 panic。
    // 用户手改 config.toml 就能喂进来（角标色、语言栏主字色都走这里）。
    assert_eq!(parse_hex("红红"), None);
    assert_eq!(parse_hex("#红红ab"), None);
}

#[test]
fn short_hex_expands_each_digit() {
    assert_eq!(parse_hex("#F80"), Some([0xFF, 0x88, 0x00, 255]));
    assert_eq!(parse_hex(" #abc "), Some([0xAA, 0xBB, 0xCC, 255]));
    assert_eq!(parse_hex("#GG0"), None);
    // 「红」恰是 3 字节：不先挡非 ASCII 的话，按字节切片会切在字中间而 panic。
    assert_eq!(parse_hex("#红"), None);
}

/// `from_str_radix` 认前导 `+`：逐字符先验十六进制位，`#+F+F+F` 不是白色。
#[test]
fn plus_signs_are_not_hex_digits() {
    for bad in [
        "#+F+F+F",
        "+F+F+F",
        "+FFFFF",
        "#+FFFFF",
        "#+FF0000",
        "#FF+FFF80",
    ] {
        assert_eq!(parse_hex(bad), None, "{bad}");
    }
    assert_eq!(parse_hex("#ffFFff"), Some([255, 255, 255, 255]));
    // 既有宽容保留：多个前导 `#`、6 / 8 位裸写（用户主题里的老写法，收紧会静默回落默认色）。
    assert_eq!(parse_hex("##FFFFFF"), Some([255, 255, 255, 255]));
    assert_eq!(parse_hex("FFFFFF"), Some([255, 255, 255, 255]));
    assert_eq!(parse_hex("#FFF"), Some([255, 255, 255, 255]));
    assert_eq!(parse_hex("#FFFFFF80"), Some([255, 255, 255, 0x80]));
}

/// 3 位不带 `#` 一律不认：语言栏配置与内联色里，`bad` / `fed` / `ace` 是词不是颜色。
#[test]
fn short_hex_requires_hash() {
    for word in ["bad", "fed", "ace", "F80", "##F80"] {
        assert_eq!(parse_hex(word), None, "{word} 不该被当成颜色");
    }
    // 6 / 8 位的裸写是既有行为，保持。
    assert_eq!(parse_hex("FF8040"), Some([255, 128, 64, 255]));
}

#[test]
fn test_palette_var_and_lightdark() {
    let src = r##"
primary = "#4285F4"
accent = "${primary}"
bg = { light = "#FFFFFF", dark = "#2D2D2D" }
selection_text = "${text}"
text = { light = "#1E1E1E", dark = "#E0E0E0" }
"##;
    let v: Value = toml::from_str(src).unwrap();
    let light = resolve_palette(Some(&v), false);
    assert_eq!(light["primary"], [0x42, 0x85, 0xF4, 255]);
    assert_eq!(light["accent"], [0x42, 0x85, 0xF4, 255]); // ${primary}
    assert_eq!(light["bg"], [255, 255, 255, 255]);
    assert_eq!(light["selection_text"], [0x1E, 0x1E, 0x1E, 255]); // ${text}

    let dark = resolve_palette(Some(&v), true);
    assert_eq!(dark["bg"], [0x2D, 0x2D, 0x2D, 255]);
    assert_eq!(dark["text"], [0xE0, 0xE0, 0xE0, 255]);
}
