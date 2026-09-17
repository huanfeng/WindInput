//! TOML「写入形态」→「内存形态」归一化。
//!
//! TOML 主题文件是**扁平 + 简写**的人写形态（见编辑器
//! `docs/superpowers/specs/2026-06-21-toml-theme-schema-design.md`）。本模块把解析得到的
//! 扁平 `toml::Value` 规整成与 typed `Theme`（嵌套）一致的规范形态，再交 serde `try_into`。
//! 这样渲染层（resolve/rvnode）与 schema 结构基本不动。
//!
//! 映射（flat file → canonical nested）：
//! - 顶层视图表（window/item/…/toolbar/menu）→ 收进 `views.*`
//! - 节点 `radius` → `border.radius`；`shape` → `background.shape`
//! - `background = "${bg}"`（标量）/`{light,dark}`（变体）→ `background = { color = … }`
//! - `margin`/`padding`/`slice` 标量/数组简写 → `{ top, right, bottom, left }`
//! - `shadow.offset = [x, y]` → `shadow.offset_x` / `offset_y`
//! - `toolbar.button.{chinese,english}` → `toolbar.button.mode.{…}`
//! - `toolbar.settings.{icon,hole}` 标量 → `{ color = … }`

use toml::Value;
use toml::value::Table;

/// 顶层视图节点白名单（单节点）。toolbar/menu 单独处理。
const VIEW_NODE_KEYS: &[&str] = &[
    "window",
    "candidate_list",
    "preedit_bar",
    "item",
    "index",
    "text",
    "comment",
    "accent_bar",
    "footer_bar",
    "mode_label",
    "status",
    "tooltip",
    "toast",
];

/// 顶层保留块（非视图节点）。
const RESERVED_TOP: &[&str] = &["meta", "colors", "behavior", "resources", "base", "views"];

/// 把扁平 TOML 根表归一化为规范嵌套形态。非 Table 顶层原样返回。
pub fn normalize_theme(root: Value) -> Value {
    let Value::Table(mut t) = root else {
        return root;
    };
    let mut views = Table::new();
    // 收集需迁入 views 的顶层键（避免借用冲突，先收集键名）。
    let move_keys: Vec<String> = t
        .keys()
        .filter(|k| !RESERVED_TOP.contains(&k.as_str()))
        .cloned()
        .collect();
    for k in move_keys {
        let Some(v) = t.remove(&k) else { continue };
        let nv = match k.as_str() {
            "toolbar" => normalize_toolbar(v),
            "menu" => normalize_menu(v),
            key if VIEW_NODE_KEYS.contains(&key) => normalize_node(v),
            // 未知顶层表：保留原样，交由 serde（未知字段忽略）。
            _ => v,
        };
        views.insert(k, nv);
    }
    if !views.is_empty() {
        t.insert("views".to_string(), Value::Table(views));
    }
    Value::Table(t)
}

/// 单个视图节点归一化（递归 selected/hover/disabled）。
fn normalize_node(v: Value) -> Value {
    let Value::Table(mut t) = v else { return v };

    // radius → border.radius
    if let Some(r) = t.remove("radius") {
        ensure_table(&mut t, "border").insert("radius".to_string(), r);
    }
    // background 标量/变体 → { color }，并展开内部 image.slice。
    let shape = t.remove("shape");
    if let Some(bg) = t.remove("background") {
        t.insert("background".to_string(), normalize_fill(bg));
    }
    // shape → background.shape（背景已规整为 Table）。
    if let Some(shape) = shape {
        ensure_table(&mut t, "background").insert("shape".to_string(), shape);
    }

    for k in ["margin", "padding"] {
        if let Some(e) = t.remove(k) {
            t.insert(k.to_string(), expand_edges(e));
        }
    }
    if let Some(sh) = t.remove("shadow") {
        t.insert("shadow".to_string(), normalize_shadow(sh));
    }
    // position_offset：`[x, y]` 简写 → `{ x, y }`（与 shadow.offset 的书写体验一致）。
    if let Some(po) = t.remove("position_offset") {
        t.insert("position_offset".to_string(), expand_point(po));
    }
    if let Some(Value::Array(arr)) = t.remove("layers") {
        let layers = arr.into_iter().map(normalize_image).collect();
        t.insert("layers".to_string(), Value::Array(layers));
    }
    for k in ["prev_image", "next_image"] {
        if let Some(im) = t.remove(k) {
            t.insert(k.to_string(), normalize_image(im));
        }
    }
    for k in ["selected", "hover", "disabled"] {
        if let Some(s) = t.remove(k) {
            t.insert(k.to_string(), normalize_node(s));
        }
    }
    Value::Table(t)
}

/// Fill 归一化：标量 → `{color}`；`{light,dark}` 变体 → `{color={light,dark}}`；
/// 复合表展开内部 `image.slice`。
fn normalize_fill(v: Value) -> Value {
    match v {
        Value::String(s) => {
            let mut t = Table::new();
            t.insert("color".to_string(), Value::String(s));
            Value::Table(t)
        }
        Value::Table(mut t) => {
            // 纯色亮暗变体背景：{light,dark} 无 color/image/gradient → 包成 color。
            if t.contains_key("light")
                && t.contains_key("dark")
                && !t.contains_key("color")
                && !t.contains_key("image")
                && !t.contains_key("gradient")
            {
                let mut wrap = Table::new();
                wrap.insert("color".to_string(), Value::Table(t));
                return Value::Table(wrap);
            }
            if let Some(img) = t.remove("image") {
                t.insert("image".to_string(), normalize_image(img));
            }
            Value::Table(t)
        }
        other => other,
    }
}

/// ImageFill 归一化：展开 `slice` 与 `slice_repeat` 简写。
fn normalize_image(v: Value) -> Value {
    let Value::Table(mut t) = v else { return v };
    if let Some(slice) = t.remove("slice") {
        t.insert("slice".to_string(), expand_edges(slice));
    }
    if let Some(r) = t.remove("slice_repeat") {
        t.insert("slice_repeat".to_string(), expand_axes(r));
    }
    Value::Table(t)
}

/// 双轴简写展开：标量 `"repeat"` → `{ x, y }` 同值；`[x, y]` 复用点展开；表原样。
///
/// **认不出的形态一律丢弃**（展开成空表 ⇒ 两轴都是 None ⇒ 拉伸），而不是原样留给 serde。
/// 留给 serde 的话，`slice_repeat = 42` 这种笔误会让**整份主题加载失败**（字段在但类型
/// 不对是硬错误，`#[serde(default)]` 只管字段缺失），一个枚举值写错就整套皮肤打不开。
/// 这个字段的取值是两个固定单词，写错的概率比尺寸类字段高得多，代价不该这么大。
/// 编辑器侧 `sliceRepeatF` 对同样的输入也是丢弃——两边对「作者写错了」的反应要一致。
fn expand_axes(v: Value) -> Value {
    match v {
        Value::String(_) => {
            let mut t = Table::new();
            t.insert("x".to_string(), v.clone());
            t.insert("y".to_string(), v);
            Value::Table(t)
        }
        Value::Array(_) => match expand_point(v) {
            // expand_point 对非法长度原样返回数组，那正是会炸 serde 的形态。
            Value::Table(t) => Value::Table(t),
            _ => Value::Table(Table::new()),
        },
        Value::Table(t) => Value::Table(t),
        _ => Value::Table(Table::new()),
    }
}

/// Shadow 归一化：`offset = [x, y]` → `offset_x` / `offset_y`。
fn normalize_shadow(v: Value) -> Value {
    let Value::Table(mut t) = v else { return v };
    if let Some(Value::Array(a)) = t.remove("offset") {
        let mut it = a.into_iter();
        if let Some(x) = it.next() {
            t.insert("offset_x".to_string(), x);
        }
        if let Some(y) = it.next() {
            t.insert("offset_y".to_string(), y);
        }
    }
    Value::Table(t)
}

/// Point 简写展开：`[x, y]` → `{ x, y }`；表/其它原样。
fn expand_point(v: Value) -> Value {
    let Value::Array(a) = v else { return v };
    if a.len() != 2 {
        // 非法长度：原样返回（serde 报错或忽略），与 expand_edges 同策。
        return Value::Array(a);
    }
    let mut t = Table::new();
    t.insert("x".to_string(), a[0].clone());
    t.insert("y".to_string(), a[1].clone());
    Value::Table(t)
}

/// Edges 简写展开：标量→四边相等；`[纵,横]`/`[上,右,下,左]`→分边；表→原样。
fn expand_edges(v: Value) -> Value {
    match v {
        Value::Integer(_) | Value::Float(_) | Value::String(_) => {
            let mut t = Table::new();
            for side in ["top", "right", "bottom", "left"] {
                t.insert(side.to_string(), v.clone());
            }
            Value::Table(t)
        }
        Value::Array(a) => {
            let mut t = Table::new();
            match a.len() {
                2 => {
                    t.insert("top".to_string(), a[0].clone());
                    t.insert("bottom".to_string(), a[0].clone());
                    t.insert("right".to_string(), a[1].clone());
                    t.insert("left".to_string(), a[1].clone());
                }
                4 => {
                    t.insert("top".to_string(), a[0].clone());
                    t.insert("right".to_string(), a[1].clone());
                    t.insert("bottom".to_string(), a[2].clone());
                    t.insert("left".to_string(), a[3].clone());
                }
                // 非法长度：原样返回（serde 报错或忽略）。
                _ => return Value::Array(a),
            }
            Value::Table(t)
        }
        // 已是 { top, … } 表 / 其它：原样。
        other => other,
    }
}

/// toolbar 归一化：背景 fill、grip/button/settings 子节点。
fn normalize_toolbar(v: Value) -> Value {
    let Value::Table(mut t) = v else { return v };
    if let Some(bg) = t.remove("background") {
        t.insert("background".to_string(), normalize_fill(bg));
    }
    if let Some(grip) = t.remove("grip") {
        t.insert("grip".to_string(), normalize_node(grip));
    }
    if let Some(btn) = t.remove("button") {
        t.insert("button".to_string(), normalize_toolbar_button(btn));
    }
    if let Some(set) = t.remove("settings") {
        t.insert("settings".to_string(), normalize_toolbar_settings(set));
    }
    Value::Table(t)
}

/// toolbar.button：`{chinese,english}` → `mode.{…}`，背景 fill。
fn normalize_toolbar_button(v: Value) -> Value {
    let Value::Table(mut t) = v else { return v };
    if let Some(bg) = t.remove("background") {
        t.insert("background".to_string(), normalize_fill(bg));
    }
    let cn = t.remove("chinese");
    let en = t.remove("english");
    if cn.is_some() || en.is_some() {
        let mut mode = Table::new();
        if let Some(c) = cn {
            mode.insert("chinese".to_string(), normalize_node(c));
        }
        if let Some(e) = en {
            mode.insert("english".to_string(), normalize_node(e));
        }
        t.insert("mode".to_string(), Value::Table(mode));
    }
    Value::Table(t)
}

/// toolbar.settings：background/icon/hole 均为 Fill。
fn normalize_toolbar_settings(v: Value) -> Value {
    let Value::Table(mut t) = v else { return v };
    for k in ["background", "icon", "hole"] {
        if let Some(f) = t.remove(k) {
            t.insert(k.to_string(), normalize_fill(f));
        }
    }
    Value::Table(t)
}

/// menu 归一化：root/item/separator 均为视图节点。
fn normalize_menu(v: Value) -> Value {
    let Value::Table(mut t) = v else { return v };
    for k in ["root", "item", "separator"] {
        if let Some(n) = t.remove(k) {
            t.insert(k.to_string(), normalize_node(n));
        }
    }
    Value::Table(t)
}

/// 取/建子表（用于 radius→border、shape→background 的下沉注入）。
fn ensure_table<'a>(t: &'a mut Table, key: &str) -> &'a mut Table {
    let entry = t
        .entry(key.to_string())
        .or_insert_with(|| Value::Table(Table::new()));
    if !entry.is_table() {
        *entry = Value::Table(Table::new());
    }
    entry.as_table_mut().expect("just ensured table")
}

#[cfg(test)]
mod tests {
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
        let t = load(
            "[window]\nborder = { width = \"1px\", color = \"${border}\", style = \"dashed\" }\n",
        );
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
        for bad in ["42", "true", "[\"repeat\"]", "[\"a\", \"b\", \"c\"]"] {
            let t = load(&format!(
                "[window]\nbackground = {{ image = {{ ref = \"p.png\", mode = \"nine_slice\", slice_repeat = {bad} }} }}\n"
            ));
            let img = t.views.unwrap().window.background.image.expect("image");
            assert_eq!(img.slice_repeat.x, None, "非法值 {bad} 该被丢弃");
            assert_eq!(img.slice_repeat.y, None, "非法值 {bad} 该被丢弃");
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
}
