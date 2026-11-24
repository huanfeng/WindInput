use super::*;

fn node(family: Option<&str>) -> RvNode {
    RvNode {
        font_family: family.map(str::to_string),
        ..Default::default()
    }
}

/// 收集要覆盖三件事：必选节点与可选节点都算、状态 patch 递归进去、空声明不算。
#[test]
fn declared_font_families_covers_states_and_optional_nodes() {
    let mut v = RvViews {
        text: node(Some("思源宋体")),
        index: node(Some("  ")), // 纯空白 = 没声明
        comment: node(None),
        ..Default::default()
    };
    v.item = node(Some("Consolas"));
    v.item.selected = Some(Box::new(node(Some("Consolas Bold"))));
    v.item.hover = Some(Box::new(node(None)));
    v.menu_item = Some(node(Some("Segoe UI")));
    v.toast = Some(node(None));

    let got = v.declared_font_families();
    assert_eq!(
        got,
        vec![
            ("views.item".to_string(), "Consolas".to_string()),
            (
                "views.item.selected".to_string(),
                "Consolas Bold".to_string()
            ),
            ("views.text".to_string(), "思源宋体".to_string()),
            ("views.menu.item".to_string(), "Segoe UI".to_string()),
        ],
        "顺序按节点表 + 状态 patch 紧跟其基态；空白与 None 不该出现"
    );
}

#[test]
fn clear_font_families_strips_every_declared_node_and_state() {
    let mut v = RvViews {
        text: node(Some("思源宋体")),
        footer_bar: node(Some("Consolas")),
        ..Default::default()
    };
    v.item.selected = Some(Box::new(node(Some("Consolas Bold"))));
    v.menu_item = Some(node(Some("Segoe UI")));
    assert!(!v.declared_font_families().is_empty());
    v.clear_font_families();
    assert!(
        v.declared_font_families().is_empty(),
        "清完仍有声明：节点表与告警表不同源"
    );
}

/// 旧 GDI face name 拆出的字重只在节点自己没写字重时采用；状态 patch 同样过一遍。
#[test]
fn resolve_font_families_rewrites_names_and_fills_only_unset_weights() {
    let mut v = RvViews {
        text: node(Some("思源宋体 SemiBold")),
        ..Default::default()
    };
    let mut sel = node(Some("思源宋体 SemiBold"));
    sel.font_weight = 800;
    v.item.selected = Some(Box::new(sel));
    v.resolve_font_families(|f| match f {
        "思源宋体 SemiBold" => ("思源宋体".to_string(), 600),
        other => (other.to_string(), 0),
    });
    assert_eq!(v.text.font_family.as_deref(), Some("思源宋体"));
    assert_eq!(v.text.font_weight, 600);
    let sel = v.item.selected.as_ref().unwrap();
    assert_eq!(sel.font_family.as_deref(), Some("思源宋体"));
    assert_eq!(sel.font_weight, 800, "节点显式写的字重不被名字里的字重覆盖");
    assert_eq!(v.comment.font_family, None, "没声明的节点不凭空长出字族");
}

/// 用户字重直接覆盖所有写了字重的节点，含状态 patch 与可选节点；未写的保持继承。
#[test]
fn apply_user_font_weight_overrides_every_written_weight() {
    let w = |fw: i32| RvNode {
        font_weight: fw,
        ..Default::default()
    };
    let mut v = RvViews {
        text: w(700),
        index: w(600),
        comment: w(300),
        ..Default::default()
    };
    v.text.selected = Some(Box::new(w(800)));
    v.text.hover = Some(Box::new(w(400)));
    v.item.selected = Some(Box::new(w(700)));
    v.status = Some(w(400));
    v.apply_user_font_weight(450);
    assert_eq!(v.text.font_weight, 450);
    assert_eq!(v.index.font_weight, 450);
    assert_eq!(v.comment.font_weight, 450);
    assert_eq!(v.text.selected.as_ref().unwrap().font_weight, 450);
    assert_eq!(v.text.hover.as_ref().unwrap().font_weight, 450);
    assert_eq!(v.item.selected.as_ref().unwrap().font_weight, 450);
    assert_eq!(v.status.as_ref().unwrap().font_weight, 450);
    assert_eq!(v.item.font_weight, 0, "未写的节点保持继承");
    v.apply_user_font_weight(0);
    assert_eq!(v.index.font_weight, 450, "0 = 不指定，原样不动");
}

/// 没配过字体的主题一条都不该报——否则每次换主题都白查一轮 COM。
#[test]
fn a_theme_without_font_family_declares_nothing() {
    assert!(RvViews::default().declared_font_families().is_empty());
}
