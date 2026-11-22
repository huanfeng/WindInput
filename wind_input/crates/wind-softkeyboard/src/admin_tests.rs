use super::*;

const LOWER: &str = r#"
[[pages]]
id = "punct"
name = "标点"
rows = ["· ！ ？", "“ ”"]
rows_shift = ["～ ¡"]

[[pages]]
id = "pc"
name = "PC键盘"
send_keys = true
rows = ["` 1 2"]
"#;

fn lower() -> SoftKeyboardTable {
    SoftKeyboardTable::parse(LOWER).unwrap()
}

fn set(page: &str, slot: &str, shift: bool, v: Option<&str>) -> Op {
    Op::SetKey {
        page: page.into(),
        slot: slot.into(),
        shift,
        value: v.map(str::to_string),
    }
}

/// 应用后按运行时同一条合并路径（`merge_user`）读回，取某键的输出。
fn runtime_output(user: &str, page: &str, slot: &str, shift: bool) -> Option<String> {
    let mut t = lower();
    t.merge_user(user).unwrap();
    t.page(page)?.output(slot, shift).map(str::to_string)
}

fn page<'a>(v: &'a [PageView], id: &str) -> &'a PageView {
    v.iter().find(|p| p.id == id).unwrap()
}

fn key<'a>(p: &'a PageView, slot: &str) -> &'a KeyView {
    p.keys.iter().find(|k| k.slot == slot).unwrap()
}

#[test]
fn view_lists_all_slots_with_defaults() {
    let v = view(&lower(), "").unwrap();
    assert_eq!(v.len(), 2);
    let p = page(&v, "punct");
    assert_eq!(p.keys.len(), crate::SLOT_COUNT);
    assert!(!p.user_created && !p.has_user_entry);
    let g = key(p, "grave");
    assert_eq!(g.base.as_deref(), Some("·"));
    assert_eq!(g.shift.as_deref(), Some("～"));
    assert_eq!(g.default_base, g.base);
    assert!(!g.base_patched);
    assert!(page(&v, "pc").send_keys);
}

#[test]
fn set_key_writes_a_sparse_patch_and_runtime_sees_it() {
    let out = apply(&lower(), "", &[set("punct", "q", false, Some("√"))]).unwrap();
    assert_eq!(
        runtime_output(&out, "punct", "q", false).as_deref(),
        Some("√")
    );
    // 只写这一键：没有画布、没有别的键。
    assert!(!out.contains("rows"), "{out}");
    assert!(out.contains("√"));
    let v = view(&lower(), &out).unwrap();
    let k = key(page(&v, "punct"), "q");
    assert!(k.base_patched && !k.shift_patched);
    assert_eq!(k.default_base.as_deref(), Some("“"));
}

#[test]
fn shift_layer_is_patched_separately() {
    let out = apply(&lower(), "", &[set("punct", "1", true, Some("¹"))]).unwrap();
    assert_eq!(
        runtime_output(&out, "punct", "1", true).as_deref(),
        Some("¹")
    );
    assert_eq!(
        runtime_output(&out, "punct", "1", false).as_deref(),
        Some("！")
    );
    assert!(out.contains("shift+1"), "{out}");
}

#[test]
fn setting_back_to_default_removes_the_patch_and_the_shell() {
    let once = apply(&lower(), "", &[set("punct", "q", false, Some("√"))]).unwrap();
    let back = apply(&lower(), &once, &[set("punct", "q", false, Some("“"))]).unwrap();
    assert!(
        !back.contains("[[pages]]"),
        "出厂面的空壳条目应删掉: {back}"
    );
    assert!(!view(&lower(), &back).unwrap()[0].has_user_entry);
}

#[test]
fn reset_key_restores_default() {
    let once = apply(&lower(), "", &[set("punct", "q", false, Some("√"))]).unwrap();
    let back = apply(
        &lower(),
        &once,
        &[Op::ResetKey {
            page: "punct".into(),
            slot: "q".into(),
            shift: false,
        }],
    )
    .unwrap();
    assert_eq!(
        runtime_output(&back, "punct", "q", false).as_deref(),
        Some("“")
    );
}

#[test]
fn clearing_a_key_writes_the_hole_placeholder() {
    let out = apply(&lower(), "", &[set("punct", "grave", false, None)]).unwrap();
    assert_eq!(runtime_output(&out, "punct", "grave", false), None);
    let out2 = apply(&lower(), "", &[set("punct", "grave", false, Some(""))]).unwrap();
    assert_eq!(out, out2, "空串与 None 同义");
}

#[test]
fn literal_dot_is_escaped() {
    let out = apply(&lower(), "", &[set("punct", "q", false, Some("."))]).unwrap();
    assert_eq!(
        runtime_output(&out, "punct", "q", false).as_deref(),
        Some(".")
    );
}

#[test]
fn alias_patches_are_replaced_not_shadowed() {
    // 用户手写了别名键 `[`；编辑器改 lbracket 时必须把它一并替换，不能并存。
    let user = "[[pages]]\nid = \"punct\"\nkeys = { \"[\" = \"A\" }\n";
    let out = apply(
        &lower(),
        user,
        &[set("punct", "lbracket", false, Some("B"))],
    )
    .unwrap();
    assert_eq!(
        runtime_output(&out, "punct", "[", false).as_deref(),
        Some("B")
    );
    assert!(!out.contains("\"[\""), "{out}");
}

#[test]
fn hand_written_comments_and_rows_survive() {
    let user = "# 我的注释\n[[pages]]\nid = \"punct\"\n# 画布注释\nrows = [\"a b c\"]\n";
    let out = apply(&lower(), user, &[set("punct", "1", false, Some("X"))]).unwrap();
    assert!(
        out.contains("# 我的注释") && out.contains("# 画布注释"),
        "{out}"
    );
    assert!(out.contains("rows = [\"a b c\"]"), "画布原样保留: {out}");
    assert_eq!(
        runtime_output(&out, "punct", "grave", false).as_deref(),
        Some("a")
    );
    assert_eq!(
        runtime_output(&out, "punct", "1", false).as_deref(),
        Some("X")
    );
    // 下层含用户手写画布：改回画布上的值，补丁消失但条目（有 rows）留下。
    let back = apply(&lower(), &out, &[set("punct", "1", false, Some("b"))]).unwrap();
    assert!(back.contains("rows") && !back.contains("X"), "{back}");
}

#[test]
fn create_blank_page_then_fill_it_in_one_batch() {
    let out = apply(
        &lower(),
        "",
        &[
            Op::CreatePage {
                id: "mine".into(),
                name: "我的".into(),
                copy_from: None,
            },
            set("mine", "a", false, Some("α")),
            set("mine", "a", true, Some("Α")),
        ],
    )
    .unwrap();
    assert!(out.starts_with("# 软键盘用户覆盖"), "新文件带说明头: {out}");
    assert_eq!(
        runtime_output(&out, "mine", "a", true).as_deref(),
        Some("Α")
    );
    let v = view(&lower(), &out).unwrap();
    let p = page(&v, "mine");
    assert!(p.user_created && p.has_user_entry);
    assert_eq!(p.name, "我的");
}

#[test]
fn blank_new_page_stays_visible_to_the_editor() {
    let out = apply(
        &lower(),
        "",
        &[Op::CreatePage {
            id: "mine".into(),
            name: "我的".into(),
            copy_from: None,
        }],
    )
    .unwrap();
    assert!(view(&lower(), &out).unwrap().iter().any(|p| p.id == "mine"));
}

#[test]
fn copy_page_copies_outputs_as_patches_and_yields_a_symbol_page() {
    let out = apply(
        &lower(),
        "",
        &[Op::CreatePage {
            id: "pc2".into(),
            name: "PC 副本".into(),
            copy_from: Some("pc".into()),
        }],
    )
    .unwrap();
    assert!(!out.contains("rows"), "复制也只写补丁: {out}");
    let v = view(&lower(), &out).unwrap();
    let p = page(&v, "pc2");
    assert!(!p.send_keys, "新建面恒是符号面，可编辑");
    assert_eq!(key(p, "1").base.as_deref(), Some("1"));
}

#[test]
fn create_rejects_bad_or_duplicate_ids() {
    let mk = |id: &str| Op::CreatePage {
        id: id.into(),
        name: "x".into(),
        copy_from: None,
    };
    assert!(apply(&lower(), "", &[mk("punct")]).is_err());
    assert!(apply(&lower(), "", &[mk("Bad Id")]).is_err());
    assert!(apply(&lower(), "", &[mk("")]).is_err());
    assert!(apply(&lower(), "", &[mk("ok"), mk("ok")]).is_err());
}

#[test]
fn rename_builtin_back_to_factory_name_leaves_no_trace() {
    let r = |n: &str| Op::RenamePage {
        page: "punct".into(),
        name: n.into(),
    };
    let out = apply(&lower(), "", &[r("中文标点")]).unwrap();
    assert_eq!(
        page(&view(&lower(), &out).unwrap(), "punct").name,
        "中文标点"
    );
    assert_eq!(
        page(&view(&lower(), &out).unwrap(), "punct").default_name,
        "标点"
    );
    let back = apply(&lower(), &out, &[r("标点")]).unwrap();
    assert!(
        back.trim().is_empty() || !back.contains("[[pages]]"),
        "{back}"
    );
    assert!(apply(&lower(), "", &[r("  ")]).is_err());
}

#[test]
fn delete_only_user_pages_reset_only_builtin_pages() {
    let created = apply(
        &lower(),
        "",
        &[
            Op::CreatePage {
                id: "mine".into(),
                name: "我的".into(),
                copy_from: None,
            },
            set("punct", "q", false, Some("√")),
        ],
    )
    .unwrap();
    assert!(
        apply(
            &lower(),
            &created,
            &[Op::DeletePage {
                page: "punct".into()
            }]
        )
        .is_err()
    );
    assert!(
        apply(
            &lower(),
            &created,
            &[Op::ResetPage {
                page: "mine".into()
            }]
        )
        .is_err()
    );

    let deleted = apply(
        &lower(),
        &created,
        &[Op::DeletePage {
            page: "mine".into(),
        }],
    )
    .unwrap();
    assert!(
        !view(&lower(), &deleted)
            .unwrap()
            .iter()
            .any(|p| p.id == "mine")
    );
    let reset = apply(
        &lower(),
        &deleted,
        &[Op::ResetPage {
            page: "punct".into(),
        }],
    )
    .unwrap();
    assert!(!reset.contains("[[pages]]"), "{reset}");
}

#[test]
fn failed_batch_produces_nothing() {
    let r = apply(
        &lower(),
        "",
        &[
            set("punct", "q", false, Some("√")),
            set("nope", "q", false, Some("x")),
        ],
    );
    assert!(r.is_err());
}

#[test]
fn rejects_unknown_slots_and_unparsable_files() {
    assert!(apply(&lower(), "", &[set("punct", "enter", false, Some("x"))]).is_err());
    assert!(apply(&lower(), "[[pages]\n", &[]).is_err());
    assert!(view(&lower(), "pages = 3").is_err());
}

#[test]
fn ops_deserialize_from_the_rpc_shape() {
    let v: Vec<Op> = toml::from_str::<toml::Value>(
        r#"
ops = [
  { op = "setKey", page = "punct", slot = "q", shift = true, value = "√" },
  { op = "createPage", id = "mine", name = "我的", copyFrom = "pc" },
  { op = "resetPage", page = "punct" },
]
"#,
    )
    .unwrap()
    .get("ops")
    .cloned()
    .unwrap()
    .try_into()
    .unwrap();
    assert_eq!(v[0], set("punct", "q", true, Some("√")));
    assert_eq!(
        v[1],
        Op::CreatePage {
            id: "mine".into(),
            name: "我的".into(),
            copy_from: Some("pc".into())
        }
    );
}
