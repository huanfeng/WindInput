use super::*;
use crate::palette::parse_hex;

fn fill(src: &str, dark: bool) -> HashMap<String, Rgba> {
    let v: Value = toml::from_str(src).unwrap();
    let mut p = resolve_palette(Some(&v), dark);
    fill_missing(Some(&v), &mut p, dark);
    p
}

fn hex(s: &str) -> Rgba {
    parse_hex(s).unwrap()
}

/// `_base` 本身的契约终值（亮 / 暗）——兜底的期望值就从它来，不另抄一份。
fn base(dark: bool) -> HashMap<String, Rgba> {
    resolve_palette(Some(&Value::Table(base_colors().clone())), dark)
}

/// 跟着气泡正文色走的三个名字（`tooltip_on_accent` / `tooltip_selection_text` 引用它）。
const TOOLTIP_TEXT_FAMILY: [&str; 3] = [
    "tooltip_text",
    "tooltip_on_accent",
    "tooltip_selection_text",
];

/// 写了气泡底色的空主题：契约名全数补齐，值等于 `_base` 的。
#[test]
fn theme_with_tooltip_bg_gets_every_contract_name_from_base() {
    for dark in [false, true] {
        let p = fill(r##"tooltip_bg = "#202020""##, dark);
        let b = base(dark);
        for n in all_names() {
            assert_eq!(p.get(&n), b.get(&n), "dark={dark}: {n}");
        }
        assert_eq!(p.len(), 23, "只补契约名，不带进 _base 的其它名字");
    }
}

/// 气泡底色写了但解析不出：渲染层照样用常量底色，气泡正文色也取渲染层原常量。
#[test]
fn unresolvable_tooltip_bg_counts_as_absent() {
    for bg in [r#""transparent""#, r#""${nowhere}""#] {
        let p = fill(&format!("tooltip_bg = {bg}\ntext = \"#333333\""), true);
        assert_eq!(p["tooltip_text"], TOOLTIP_TEXT, "tooltip_bg = {bg}");
    }
}

/// 连气泡底色也没写：气泡正文色取渲染层原常量（亮暗同值），外观与没有兜底时一致；
/// 其余契约名照旧取 `_base` 的。
#[test]
fn theme_without_tooltip_bg_keeps_render_layer_tooltip_text() {
    for dark in [false, true] {
        let p = fill(r##"text = "#333333""##, dark);
        let b = base(dark);
        for n in TOOLTIP_TEXT_FAMILY {
            assert_eq!(p[n], TOOLTIP_TEXT, "dark={dark}: {n}");
        }
        for n in all_names().filter(|n| !TOOLTIP_TEXT_FAMILY.contains(&n.as_str())) {
            if n != "text" && n != "selection_text" {
                assert_eq!(p.get(&n), b.get(&n), "dark={dark}: {n}");
            }
        }
        assert_eq!(p.len(), 22);
    }
}

/// 主题写了的名字一个不动；契约外的名字一个不加。
#[test]
fn own_values_are_never_overridden() {
    let src = r##"
error = "#010203"
tooltip_error = "#040506"
text = { light = "#111111", dark = "#EEEEEE" }
primary = "#ABCDEF"
"##;
    for dark in [false, true] {
        let p = fill(src, dark);
        assert_eq!(p["error"], hex("#010203"));
        assert_eq!(p["tooltip_error"], hex("#040506"));
        assert_eq!(p["text"], hex(if dark { "#EEEEEE" } else { "#111111" }));
        for extra in ["bg", "surface", "menu_bg", "tooltip_bg", "status_bg"] {
            assert!(!p.contains_key(extra), "不该补契约外的 {extra}");
        }
    }
}

/// 写了却解析不出的契约名不补，引用它的兜底也跟着解析不出。
#[test]
fn written_but_broken_names_stay_missing() {
    let p = fill(r#"text = "${nowhere}""#, false);
    assert!(!p.contains_key("text"));
    assert!(
        !p.contains_key("selection_text"),
        "selection_text = ${{text}}"
    );
    assert_eq!(p["error"], base(false)["error"]);
}

/// 引用型的兜底按本主题解析：accent_text 借本主题强调色，selection_text 借本主题 text，
/// tooltip_accent_text 借本主题 tooltip_accent。
#[test]
fn references_resolve_against_this_theme() {
    let src = r##"
accent = "#FF4554"
text = { light = "#111827", dark = "#F6FAFD" }
tooltip_accent = "#00C3E3"
tooltip_text = "#FAFAFA"
"##;
    for dark in [false, true] {
        let p = fill(src, dark);
        assert_eq!(p["accent_text"], hex("#FF4554"), "dark={dark}");
        let text = hex(if dark { "#F6FAFD" } else { "#111827" });
        assert_eq!(p["selection_text"], text);
        assert_eq!(p["tooltip_accent_text"], hex("#00C3E3"));
        assert_eq!(p["tooltip_on_accent"], hex("#FAFAFA"));
        assert_eq!(p["tooltip_selection_text"], hex("#FAFAFA"));
    }
    // 写了气泡底色、没写 tooltip_text：暗档 = ${text}，取本主题的 text。
    let p = fill("text = \"#123456\"\ntooltip_bg = \"#202020\"", true);
    assert_eq!(p["tooltip_text"], hex("#123456"));
}

/// 主题连 accent 也没有：accent 与 accent_text 取 `_base` 的；契约外的 primary 不借主题的。
#[test]
fn missing_accent_uses_base_not_theme_primary() {
    let p = fill(r##"primary = "#00FF00""##, false);
    assert_eq!(p["accent"], hex("#4285F4"));
    assert_eq!(p["accent_text"], hex("#4285F4"));
}
