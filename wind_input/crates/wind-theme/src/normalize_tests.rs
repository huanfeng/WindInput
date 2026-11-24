use super::*;
use crate::schema::{Dim, Theme};

/// 解析 flat TOML → normalize → typed Theme。
fn load(s: &str) -> Theme {
    let v: Value = toml::from_str(s).expect("parse toml");
    let n = normalize_theme(v);
    n.try_into().expect("type theme")
}

#[test]
fn flat_view_tables_move_under_views() {
    let t = load("[window]\npadding = 8\n");
    let views = t.views.expect("views");
    // padding 标量 8 → 四边 dp 8
    assert_eq!(views.window.padding.top, Some(Dim::Dp(8.0)));
    assert_eq!(views.window.padding.left, Some(Dim::Dp(8.0)));
}

#[test]
fn radius_and_shape_sink() {
    let t = load("[index]\nradius = 4\nshape = \"circle\"\nbackground = \"${accent}\"\n");
    let idx = &t.views.unwrap().index;
    assert_eq!(idx.border.radius, Some(Dim::Dp(4.0)));
    assert_eq!(idx.background.shape, "circle");
    // background 标量 → color
    assert!(idx.background.color.is_some());
}

#[test]
fn edges_array_two_and_four() {
    let t = load("[item]\npadding = [7, 10, 7, 8]\n[preedit_bar]\npadding = [3, 8]\n");
    let v = t.views.unwrap();
    assert_eq!(v.item.padding.top, Some(Dim::Dp(7.0)));
    assert_eq!(v.item.padding.right, Some(Dim::Dp(10.0)));
    assert_eq!(v.item.padding.left, Some(Dim::Dp(8.0)));
    assert_eq!(v.preedit_bar.padding.top, Some(Dim::Dp(3.0)));
    assert_eq!(v.preedit_bar.padding.right, Some(Dim::Dp(8.0)));
}

#[test]
fn shadow_offset_array_splits() {
    let t = load("[window]\nshadow = { offset = [2, 3], color = \"${shadow}\" }\n");
    let sh = t.views.unwrap().window.shadow.expect("shadow");
    assert_eq!(sh.offset_x, Some(Dim::Dp(2.0)));
    assert_eq!(sh.offset_y, Some(Dim::Dp(3.0)));
}

#[test]
fn border_with_style_and_px_width() {
    let t =
        load("[window]\nborder = { width = \"1px\", color = \"${border}\", style = \"dashed\" }\n");
    let b = &t.views.unwrap().window.border;
    assert_eq!(b.width, Some(Dim::Px(1.0)));
    assert_eq!(b.style.as_deref(), Some("dashed"));
}

/// `slice_repeat` 的两种人写简写都要能展开到 `{ x, y }`。
///
/// 分轴不是讲究：横轴随候选窗宽度变、纵轴一般固定，一个值管两轴会把
/// 「纵向压扁到条高」变成「只取源图顶部那几行」。
#[test]
fn slice_repeat_shorthand_expands_to_axes() {
    let scalar = load(
        "[window]\nbackground = { image = { ref = \"p.png\", mode = \"nine_slice\", slice_repeat = \"repeat\" } }\n",
    );
    let img = scalar
        .views
        .unwrap()
        .window
        .background
        .image
        .expect("image");
    assert_eq!(img.slice_repeat.x.as_deref(), Some("repeat"));
    assert_eq!(img.slice_repeat.y.as_deref(), Some("repeat"));

    let pair = load(
        "[window]\nbackground = { image = { ref = \"p.png\", mode = \"nine_slice\", slice_repeat = [\"repeat\", \"stretch\"] } }\n",
    );
    let img = pair.views.unwrap().window.background.image.expect("image");
    assert_eq!(img.slice_repeat.x.as_deref(), Some("repeat"));
    assert_eq!(img.slice_repeat.y.as_deref(), Some("stretch"), "两轴不该串");

    // 展开后的表形态（编辑器读别人主题时也认它），原样透传。
    let table = load(
        "[window]\nbackground = { image = { ref = \"p.png\", mode = \"nine_slice\", slice_repeat = { x = \"repeat\", y = \"stretch\" } } }\n",
    );
    let img = table.views.unwrap().window.background.image.expect("image");
    assert_eq!(img.slice_repeat.x.as_deref(), Some("repeat"));
    assert_eq!(img.slice_repeat.y.as_deref(), Some("stretch"));

    // 认不出的形态一律丢弃，而不是让整份主题加载失败（见 expand_axes）。
    // 判据是**效果**（不平铺 = 拉伸）而非「x/y 是 None」：丢弃如今产出的是显式的
    // "stretch"，求值层按 `== Some("repeat")` 坍缩，两者在渲染上等价 —— 断言若钉死
    // 实现形态，就会把「为了在 base 深合并里盖得住而改成显式值」这种正确改动判成红。
    for bad in ["42", "true", "[\"repeat\"]", "[\"a\", \"b\", \"c\"]"] {
        let t = load(&format!(
            "[window]\nbackground = {{ image = {{ ref = \"p.png\", mode = \"nine_slice\", slice_repeat = {bad} }} }}\n"
        ));
        let img = t.views.unwrap().window.background.image.expect("image");
        assert_ne!(
            img.slice_repeat.x.as_deref(),
            Some("repeat"),
            "非法值 {bad} 该被丢弃（效果 = 拉伸）"
        );
        assert_ne!(
            img.slice_repeat.y.as_deref(),
            Some("repeat"),
            "非法值 {bad} 该被丢弃（效果 = 拉伸）"
        );
    }

    // 不写就是两轴都拉伸（既有主题的行为不能被这个新字段改掉）。
    let none =
        load("[window]\nbackground = { image = { ref = \"p.png\", mode = \"nine_slice\" } }\n");
    let img = none.views.unwrap().window.background.image.expect("image");
    assert_eq!(img.slice_repeat.x, None);
    assert_eq!(img.slice_repeat.y, None);
}

#[test]
fn toolbar_button_mode_flatten() {
    let s = "[toolbar.button]\nbackground = \"${btn}\"\n[toolbar.button.chinese]\nbackground = \"${cn}\"\n[toolbar.button.english]\nbackground = \"${en}\"\n";
    let t = load(s);
    let tb = t.views.unwrap().toolbar.expect("toolbar");
    let mode = tb.button.mode.expect("mode");
    assert!(mode.chinese.background.color.is_some());
    assert!(mode.english.background.color.is_some());
}

#[test]
fn lightdark_background_wraps_as_color() {
    let t = load("[window]\nbackground = { light = \"#FFF\", dark = \"#000\" }\n");
    let c = t.views.unwrap().window.background.color.expect("color");
    assert_eq!(c.select(false), Some("#FFF"));
    assert_eq!(c.select(true), Some("#000"));
}

#[test]
fn nine_slice_image_slice_expands() {
    let s = "[window]\nbackground = { image = { ref = \"panel.png\", mode = \"nine_slice\", slice = 8 } }\n";
    let t = load(s);
    let img = t.views.unwrap().window.background.image.expect("image");
    assert_eq!(img.reference, "panel.png");
    assert_eq!(img.slice.top, Some(Dim::Dp(8.0)));
    assert_eq!(img.slice.left, Some(Dim::Dp(8.0)));
}

#[test]
fn item_states_recurse() {
    let s = "[item]\npadding = 4\n[item.selected]\nbackground = \"${sel}\"\ncolor = \"${sel_text}\"\n[item.hover]\nbackground = \"${hover}\"\n";
    let t = load(s);
    let item = &t.views.unwrap().item;
    let sel = item.selected.as_ref().expect("selected");
    assert!(sel.background.color.is_some());
    assert!(sel.color.is_some());
    assert!(item.hover.as_ref().unwrap().background.color.is_some());
}

/// 扁平写法 `[comment.selected.roles]` 落到 `views.comment.selected.roles`（§5.2：normalize 不用改，
/// `roles` 是普通子表、状态已递归——这条钉住它）。
#[test]
fn flat_state_roles_land_under_the_state() {
    let t = load(
        "[comment.roles]\npinyin = \"#008000\"\n[comment.selected.roles]\ncode_rev = \"${accent}\"\n",
    );
    let c = &t.views.unwrap().comment;
    assert!(c.roles.contains_key("pinyin"));
    assert!(c.selected.as_ref().unwrap().roles.contains_key("code_rev"));
}
