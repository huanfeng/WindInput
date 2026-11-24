use super::*;

fn store() -> (Store, std::path::PathBuf) {
    // 每个测试独立文件：redb 是单写者，共用文件会让并发测试互相阻塞
    let p = std::env::temp_dir().join(format!(
        "wind_quick_format_test_{}_{:?}.redb",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&p);
    (Store::open(&p).unwrap(), p)
}

/// P5：用户层是 TOML（对齐出厂 system.quick.toml）；手改坏了按最后一次好数据服务、拒写、
/// 不重写文件（M-1）；修好后恢复。
#[test]
fn quick_format_is_toml_and_survives_broken_edit() {
    let (s, p) = store();
    s.set_quick_format_enabled("date", "date.lunar", false)
        .unwrap();
    let f = s.quick_format_path();
    assert!(f.ends_with("quick_format.toml"));
    let txt = std::fs::read_to_string(&f).unwrap();
    assert!(toml::from_str::<toml::Value>(&txt).is_ok(), "{txt}");
    std::fs::write(&f, "kinds = [[[").unwrap();
    s.quick_doc().sync();
    assert_eq!(
        s.get_quick_format("date").unwrap().unwrap().disabled,
        vec!["date.lunar".to_string()],
        "按最后一次好数据服务"
    );
    assert!(matches!(
        wind_userdata::issues::list_under(&f)[0].kind,
        wind_userdata::issues::IssueKind::Isolated
    ));
    assert!(s.set_quick_format_enabled("date", "x", false).is_err());
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "kinds = [[[");
    std::fs::write(&f, txt).unwrap();
    s.quick_doc().sync();
    s.set_quick_format_enabled("date", "x", false).unwrap();
    assert!(wind_userdata::issues::list_under(&f).is_empty(), "修好就清");
    let _ = std::fs::remove_file(&p);
}

/// 审查 M-2：整份还原遇到改坏的文件 ⇒ 坏文件另存留存，还原照常完成。
#[test]
fn replace_import_over_broken_file_keeps_it_aside() {
    let (s, p) = store();
    s.move_quick_format("date", "date.lunar", 0).unwrap();
    let y = s.export_quick_format_yaml().unwrap();
    let f = s.quick_format_path();
    std::fs::write(&f, "kinds = [[[").unwrap();
    s.quick_doc().sync();
    s.import_quick_format_yaml(&y, true).unwrap();
    assert_eq!(
        s.get_quick_format("date").unwrap().unwrap().moved[0].id,
        "date.lunar"
    );
    let kept = std::fs::read_dir(s.user_text_dir().join(".backup"))
        .unwrap()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("quick_format.toml.corrupt-")
        });
    assert!(kept);
    wind_userdata::issues::clear(&f);
    let _ = std::fs::remove_file(&p);
}

/// 备份包里的快捷格式仍是 YAML（老备份照样能还原，新备份老版本也能读）。
#[test]
fn backup_export_stays_yaml_and_round_trips() {
    let (s, p) = store();
    s.move_quick_format("date", "date.lunar", 0).unwrap();
    let y = s.export_quick_format_yaml().unwrap();
    assert!(serde_yaml::from_str::<serde_yaml::Value>(&y).is_ok(), "{y}");
    assert!(y.contains("date.lunar"));
    s.import_quick_format_yaml("", true).unwrap();
    assert!(s.get_quick_format("date").unwrap().is_none());
    s.import_quick_format_yaml(&y, true).unwrap();
    assert_eq!(
        s.get_quick_format("date").unwrap().unwrap().moved[0].id,
        "date.lunar"
    );
    let _ = std::fs::remove_file(&p);
}

#[test]
fn move_then_read_back() {
    let (s, p) = store();
    s.move_quick_format("date", "date.lunar", 0).unwrap();
    let rec = s.get_quick_format("date").unwrap().unwrap();
    assert_eq!(rec.moved.len(), 1);
    assert_eq!(rec.moved[0].id, "date.lunar");
    assert_eq!(rec.moved[0].position, 0);
    let _ = std::fs::remove_file(&p);
}

/// ★ LIFO：同一条格式再次移动时顶替旧规则，不是叠加两条。
#[test]
fn moving_same_entry_twice_replaces_not_appends() {
    let (s, p) = store();
    s.move_quick_format("date", "date.lunar", 3).unwrap();
    s.move_quick_format("date", "date.lunar", 0).unwrap();
    let rec = s.get_quick_format("date").unwrap().unwrap();
    assert_eq!(rec.moved.len(), 1, "同 id 只留最新一条");
    assert_eq!(rec.moved[0].position, 0);
    let _ = std::fs::remove_file(&p);
}

/// 最新的规则排在 index 0（应用时逆序遍历，故它最后生效、优先级最高）。
#[test]
fn newest_move_is_first() {
    let (s, p) = store();
    s.move_quick_format("date", "a", 1).unwrap();
    s.move_quick_format("date", "b", 2).unwrap();
    let rec = s.get_quick_format("date").unwrap().unwrap();
    assert_eq!(rec.moved[0].id, "b", "最新的在队首");
    assert_eq!(rec.moved[1].id, "a");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn disable_and_reenable() {
    let (s, p) = store();
    s.set_quick_format_enabled("date", "date.basic", false)
        .unwrap();
    assert_eq!(
        s.get_quick_format("date").unwrap().unwrap().disabled,
        vec!["date.basic".to_string()]
    );
    s.set_quick_format_enabled("date", "date.basic", true)
        .unwrap();
    // 记录变空 → 整条键被删除，而不是留一条空记录
    assert!(s.get_quick_format("date").unwrap().is_none());
    let _ = std::fs::remove_file(&p);
}

/// 重复停用同一条不产生重复项。
#[test]
fn disabling_twice_is_idempotent() {
    let (s, p) = store();
    s.set_quick_format_enabled("date", "x", false).unwrap();
    s.set_quick_format_enabled("date", "x", false).unwrap();
    assert_eq!(
        s.get_quick_format("date").unwrap().unwrap().disabled.len(),
        1
    );
    let _ = std::fs::remove_file(&p);
}

#[test]
fn reset_entry_clears_both_move_and_disable() {
    let (s, p) = store();
    s.move_quick_format("date", "x", 0).unwrap();
    s.set_quick_format_enabled("date", "x", false).unwrap();
    s.move_quick_format("date", "y", 1).unwrap();
    s.reset_quick_format_entry("date", "x").unwrap();
    let rec = s.get_quick_format("date").unwrap().unwrap();
    assert!(!rec.has_rule("x"), "x 的调整应清干净");
    assert!(rec.has_rule("y"), "不该连累其它条目");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn reset_kind_clears_everything() {
    let (s, p) = store();
    s.move_quick_format("date", "x", 0).unwrap();
    s.set_quick_format_enabled("date", "y", false).unwrap();
    s.move_quick_format("number", "z", 0).unwrap();
    s.reset_quick_format_kind("date").unwrap();
    assert!(s.get_quick_format("date").unwrap().is_none());
    assert!(
        s.get_quick_format("number").unwrap().is_some(),
        "只清本类，别的类不动"
    );
    let _ = std::fs::remove_file(&p);
}

/// ★ 键只到类别，不带方案/输入码——这正是本表与 shadow 的分界。
#[test]
fn kinds_are_independent_and_global() {
    let (s, p) = store();
    s.move_quick_format("date", "date.lunar", 0).unwrap();
    s.move_quick_format("number", "number.thousands", 0)
        .unwrap();
    let all = s.list_quick_format().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].0, "date", "按 kind 升序");
    assert_eq!(all[1].0, "number");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn missing_kind_reads_none() {
    let (s, p) = store();
    assert!(s.get_quick_format("date").unwrap().is_none());
    assert!(s.list_quick_format().unwrap().is_empty());
    let _ = std::fs::remove_file(&p);
}

#[test]
fn add_then_read_back() {
    let (s, p) = store();
    s.add_quick_format("date", "date.u1", "$Y/$M/$D").unwrap();
    let rec = s.get_quick_format("date").unwrap().unwrap();
    assert_eq!(rec.added.len(), 1);
    assert_eq!(rec.added[0].id, "date.u1");
    assert_eq!(rec.added[0].text, "$Y/$M/$D");
    assert!(rec.is_added("date.u1"));
    assert!(!rec.is_added("date.cn"), "出厂条目不属于 added");
    let _ = std::fs::remove_file(&p);
}

/// ★ 只有用户条目能改模板。出厂 id 走到这里必须返回 false——「不许编辑出厂模板」
/// 由数据结构兜住（它们不在 `added` 里），不靠调用方自觉。
#[test]
fn set_text_refuses_factory_entries() {
    let (s, p) = store();
    s.add_quick_format("date", "date.u1", "$Y").unwrap();
    assert!(s.set_quick_format_text("date", "date.u1", "$M").unwrap());
    assert!(
        !s.set_quick_format_text("date", "date.cn", "篡改").unwrap(),
        "出厂条目不可改写"
    );
    let rec = s.get_quick_format("date").unwrap().unwrap();
    assert_eq!(rec.added[0].text, "$M");
    assert_eq!(rec.added.len(), 1, "失败的改写不该添出一条来");
    let _ = std::fs::remove_file(&p);
}

/// ★ 删条目必须连带清掉它的调序规则。留着孤儿规则本身无害（应用时找不到目标即跳过），
/// 但用户重加一条同 id 的条目时，那条旧规则会突然复活、把它挪到意外的位置。
#[test]
fn delete_also_clears_its_rules() {
    let (s, p) = store();
    s.add_quick_format("date", "date.u1", "$Y").unwrap();
    s.move_quick_format("date", "date.u1", 0).unwrap();
    s.set_quick_format_enabled("date", "date.u1", false)
        .unwrap();
    s.move_quick_format("date", "date.cn", 2).unwrap();

    assert!(s.delete_quick_format("date", "date.u1").unwrap());
    let rec = s.get_quick_format("date").unwrap().unwrap();
    assert!(rec.added.is_empty());
    assert!(!rec.has_rule("date.u1"), "自己的规则要一起清掉");
    assert!(rec.has_rule("date.cn"), "别人的规则不受影响");

    assert!(
        !s.delete_quick_format("date", "date.u1").unwrap(),
        "删第二次报 false"
    );
    assert!(
        !s.delete_quick_format("date", "date.cn").unwrap(),
        "出厂条目删不掉"
    );
    let _ = std::fs::remove_file(&p);
}

/// ★★ 「恢复默认」清调整、**留用户条目**。
///
/// 反向对照：把实现换回 `t.remove(kind)` 这条就红——那正是它防的回归
/// （用户点一下右键的「恢复默认」就永久丢掉手写模板，不可逆且无预警）。
#[test]
fn reset_kind_keeps_added_entries() {
    let (s, p) = store();
    s.add_quick_format("date", "date.u1", "$Y").unwrap();
    s.move_quick_format("date", "date.lunar", 0).unwrap();
    s.set_quick_format_enabled("date", "date.cn", false)
        .unwrap();

    s.reset_quick_format_kind("date").unwrap();
    let rec = s.get_quick_format("date").unwrap().unwrap();
    assert!(rec.moved.is_empty(), "调序已清");
    assert!(rec.disabled.is_empty(), "停用已清");
    assert_eq!(rec.added.len(), 1, "★ 用户条目必须留下");
    assert_eq!(rec.added[0].text, "$Y");
    let _ = std::fs::remove_file(&p);
}

/// 没有用户条目时，「恢复默认」应把整条键删掉（不留空记录）——与改造前同行为。
#[test]
fn reset_kind_without_added_removes_the_key() {
    let (s, p) = store();
    s.move_quick_format("date", "date.lunar", 0).unwrap();
    s.reset_quick_format_kind("date").unwrap();
    assert!(s.get_quick_format("date").unwrap().is_none());
    let _ = std::fs::remove_file(&p);
}

/// 「导入并替换」用的清空**含**用户条目——replace 的语义是「用文件覆盖现状」，
/// 留着旧条目会得到「文件里的 + 我原有的」。与上一个测试刻意相反。
#[test]
fn clear_kind_removes_added_too() {
    let (s, p) = store();
    s.add_quick_format("date", "date.u1", "$Y").unwrap();
    s.add_quick_format("number", "number.u1", "$N").unwrap();
    s.clear_quick_format_kind("date").unwrap();
    assert!(s.get_quick_format("date").unwrap().is_none());
    assert!(s.get_quick_format("number").unwrap().is_some(), "只清本类");
    let _ = std::fs::remove_file(&p);
}

/// 只有用户条目、没有任何调整的记录**不是空的**，不能被 `modify` 的收尾逻辑删掉。
#[test]
fn record_with_only_added_is_not_empty() {
    let mut rec = QuickFormatRecord::default();
    assert!(rec.is_empty());
    rec.add_entry("date.u1", "$Y");
    assert!(!rec.is_empty(), "★ 否则加完一条就被当空记录删掉");
}

/// 快捷格式调整落用户目录文件；重开读回。
#[test]
fn quick_format_lives_in_user_dir() {
    let p = std::env::temp_dir().join(format!("wind_qf_file_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    {
        let s = Store::open(&p).unwrap();
        s.add_quick_format("date", "my", "yyyy").unwrap();
        s.set_quick_format_enabled("date", "iso", false).unwrap();
        let f = std::fs::read_to_string(s.quick_format_path()).unwrap();
        assert!(f.contains("my") && f.contains("iso"), "{f}");
    }
    let s = Store::open(&p).unwrap();
    let r = s.get_quick_format("date").unwrap().unwrap();
    assert_eq!(r.added[0].id, "my");
    assert_eq!(r.disabled, vec!["iso".to_string()]);
    s.clear_quick_format_kind("date").unwrap();
    assert!(s.get_quick_format("date").unwrap().is_none());
}
