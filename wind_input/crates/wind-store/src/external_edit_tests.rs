use crate::store::Store;

fn open(name: &str) -> Store {
    let p = std::env::temp_dir().join(format!("wind_xedit_{name}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    Store::open(&p).unwrap()
}

fn words(s: &Store) -> Vec<(String, String, i32)> {
    let mut v: Vec<_> = s
        .search_user_words_prefix("pinyin", "", 0)
        .unwrap()
        .into_iter()
        .map(|r| (r.text, r.code, r.weight))
        .collect();
    v.sort();
    v
}

/// 会话的意义：编辑期间新造的词不丢，外部删掉的行不复活，外部改的权重保留。
#[test]
fn words_session_keeps_external_edits_and_new_words() {
    let s = open("words");
    s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
        .unwrap();
    s.add_user_word("pinyin", "shijie", "世界", 5, 0b1001)
        .unwrap();
    let sess = s.begin_edit("pinyin/user.dict.yaml").unwrap();
    assert_eq!(sess.kind.as_str(), "words");
    assert_eq!(sess.file, "pinyin/user.dict.yaml");
    let text = std::fs::read_to_string(&sess.path).unwrap();
    assert!(text.contains("ni hao\t你好\t9"), "begin 先折回：{text}");
    assert!(text.contains("shi jie\t世界\t5"), "{text}");
    // 外部编辑：删一行、改一行权重
    let edited = text
        .lines()
        .filter(|l| !l.starts_with("shi jie\t世界\t"))
        .map(|l| {
            if l.starts_with("ni hao\t你好\t") {
                "ni hao\t你好\t20".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&sess.path, &edited).unwrap();
    // 期间照常造词；空闲折回与压实都不写文件
    s.add_user_word("pinyin", "ceshi", "测试", 3, 0b101)
        .unwrap();
    let ud = s.runtime().data();
    ud.maintain_text_idle(std::time::Duration::ZERO).unwrap();
    ud.compact().unwrap();
    assert_eq!(std::fs::read_to_string(&sess.path).unwrap(), edited);
    assert!(
        s.held_edit_files()
            .contains(&"pinyin/user.dict.yaml".to_string())
    );
    let r = s.end_edit(&sess.path.to_string_lossy()).unwrap();
    assert!(r.changed);
    assert!(r.issues.is_empty());
    let after = std::fs::read_to_string(&sess.path).unwrap();
    assert!(after.contains("ni hao\t你好\t20"), "{after}");
    assert!(after.contains("ce shi\t测试\t3"), "{after}");
    assert!(!after.contains("世界"), "删掉的行不复活：{after}");
    assert_eq!(
        words(&s),
        vec![
            ("你好".into(), "nihao".into(), 20),
            ("测试".into(), "ceshi".into(), 3)
        ]
    );
    assert!(s.held_edit_files().is_empty());
    assert!(s.edit_sessions().is_empty());
}

/// 编辑期间改权重直接拒绝（否则用户随后保存文件、mtime 变新，结束时这条改权重按时间规则被丢掉）；
/// 增词、删词不受 mtime 规则影响，照常在结束时落进文件。
#[test]
fn weight_change_rejected_while_editing_but_add_delete_survive_save() {
    let s = open("words_weight");
    s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
        .unwrap();
    s.add_user_word("pinyin", "shijie", "世界", 5, 0b1001)
        .unwrap();
    let sess = s.begin_edit("pinyin/user.dict.yaml").unwrap();
    let e = s
        .update_user_word_weight("pinyin", "nihao", "你好", 50)
        .unwrap_err()
        .to_string();
    assert!(e.contains("正在外部编辑 pinyin/user.dict.yaml"), "{e}");
    assert!(
        s.update_user_word_weight("wubi86", "a", "工", 5).is_ok(),
        "别的方案不受影响"
    );
    s.add_user_word("pinyin", "ceshi", "测试", 3, 0b101)
        .unwrap();
    s.remove_user_word("pinyin", "shijie", "世界").unwrap();
    // 用户随后保存文件（mtime 晚于上面的增删）
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let text = std::fs::read_to_string(&sess.path).unwrap();
    std::fs::write(
        &sess.path,
        text.replace("ni hao\t你好\t9", "ni hao\t你好\t20"),
    )
    .unwrap();
    s.end_edit("pinyin/user.dict.yaml").unwrap();
    assert_eq!(
        words(&s),
        vec![
            ("你好".into(), "nihao".into(), 20),
            ("测试".into(), "ceshi".into(), 3)
        ]
    );
    s.update_user_word_weight("pinyin", "nihao", "你好", 50)
        .unwrap();
}

/// 编辑期间整写用户词文件（清空 / 替换式导入 / 还原）拒绝：编辑器还开着，整写会被它的保存覆盖。
#[test]
fn rewrite_rejected_while_words_held() {
    let s = open("words_rewrite");
    s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
        .unwrap();
    let sess = s.begin_edit("pinyin/user.dict.yaml").unwrap();
    let before = std::fs::read(&sess.path).unwrap();
    let e = s.clear_user_words("pinyin").unwrap_err().to_string();
    assert!(e.contains("正在外部编辑"), "{e}");
    assert_eq!(std::fs::read(&sess.path).unwrap(), before);
    s.end_edit("pinyin/user.dict.yaml").unwrap();
    assert_eq!(s.clear_user_words("pinyin").unwrap(), 1);
}

/// 合并导入（导入后立即写回）在编辑期间也不写文件，结束时落到文件里。
#[test]
fn import_during_words_session_waits_for_end() {
    let s = open("words_import");
    let sess = s.begin_edit("pinyin/user.dict.yaml").unwrap();
    let before = std::fs::read_to_string(&sess.path).unwrap();
    let row = crate::wdict::WordIo {
        code: "ni hao".into(),
        text: "你好".into(),
        weight: 9,
        count: 0,
        boundary: None,
    };
    s.import_user_words("pinyin", &[row]).unwrap();
    assert_eq!(std::fs::read_to_string(&sess.path).unwrap(), before);
    s.end_edit("pinyin/user.dict.yaml").unwrap();
    let after = std::fs::read_to_string(&sess.path).unwrap();
    assert!(after.contains("ni hao\t你好\t9"), "{after}");
}

/// 临时词 / 词频也能编辑：开始前折回、编辑期间暂停写回；界面的删除 / 清空 / 转正 / 导入挡住，
/// 学习照常、结束后合并进用户改过的文件；外部删掉的行不复活。
#[test]
fn temp_and_freq_sessions_hold_writes_and_merge_on_end() {
    let s = open("learned");
    s.learn_temp_word("pinyin", "nihao", "你好", 10, 0b101)
        .unwrap();
    s.learn_temp_word("pinyin", "shijie", "世界", 10, 0b1001)
        .unwrap();
    let sess = s.begin_edit("pinyin/temp.dict.yaml").unwrap();
    assert_eq!(sess.kind.as_str(), "temp");
    let text = std::fs::read_to_string(&sess.path).unwrap();
    assert!(
        text.contains("你好") && text.contains("世界"),
        "begin 先折回：{text}"
    );
    let edited: String = text
        .lines()
        .filter(|l| !l.contains("\t世界\t"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&sess.path, &edited).unwrap();
    s.learn_temp_word("pinyin", "ceshi", "测试", 1, 0b101)
        .unwrap();
    let ud = s.runtime().data();
    ud.maintain_text_idle(std::time::Duration::ZERO).unwrap();
    ud.compact().unwrap();
    assert_eq!(std::fs::read_to_string(&sess.path).unwrap(), edited);
    assert!(
        s.held_edit_files()
            .contains(&"pinyin/temp.dict.yaml".to_string())
    );
    for e in [
        s.clear_temp_words("pinyin").map(|_| ()),
        s.remove_temp_word("pinyin", "nihao", "你好"),
        s.promote_temp_word("pinyin", "nihao", "你好").map(|_| ()),
        s.import_temp_word_rows("pinyin", &[]).map(|_| ()),
        s.import_temp_words_wdict("pinyin", "").map(|_| ()),
    ] {
        let e = e.unwrap_err().to_string();
        assert!(e.contains("正在外部编辑 pinyin/temp.dict.yaml"), "{e}");
    }
    assert!(
        s.remove_temp_word("wubi86", "a", "工").is_ok(),
        "别的方案不受影响"
    );
    let r = s.end_edit("pinyin/temp.dict.yaml").unwrap();
    assert!(r.changed);
    let after = std::fs::read_to_string(&sess.path).unwrap();
    assert!(after.contains("你好") && after.contains("测试"), "{after}");
    assert!(!after.contains("世界"), "删掉的行不复活：{after}");
    assert!(s.held_edit_files().is_empty());
    s.remove_temp_word("pinyin", "nihao", "你好").unwrap();

    s.record_freq("pinyin", "nihao", "你好").unwrap();
    let sess = s.begin_edit("pinyin/freq.table.yaml").unwrap();
    assert_eq!(sess.kind.as_str(), "freq");
    assert!(
        std::fs::read_to_string(&sess.path)
            .unwrap()
            .contains("你好")
    );
    s.record_freq("pinyin", "ceshi", "测试").unwrap();
    for e in [
        s.delete_freq("pinyin", "nihao", "你好"),
        s.clear_freq("pinyin").map(|_| ()),
        s.import_freq_rows("pinyin", &[]).map(|_| ()),
        s.import_freq_jsonl("pinyin", "").map(|_| ()),
    ] {
        assert!(e.unwrap_err().to_string().contains("正在外部编辑"));
    }
    s.end_edit("pinyin/freq.table.yaml").unwrap();
    assert!(
        std::fs::read_to_string(&sess.path)
            .unwrap()
            .contains("测试")
    );
    s.delete_freq("pinyin", "nihao", "你好").unwrap();
}

/// 临时词 / 词频改坏了（整份解析不了）：结束时把问题带回，写回不往坏文件里写。
#[test]
fn learned_end_reports_broken_file() {
    let s = open("learned_broken");
    s.learn_temp_word("pinyin", "nihao", "你好", 10, 0b101)
        .unwrap();
    s.record_freq("pinyin", "nihao", "你好").unwrap();
    for (f, bad) in [
        ("pinyin/temp.dict.yaml", &b"\xff\xfe"[..]),
        ("pinyin/freq.table.yaml", b"---\nbad: [\n...\n"),
        ("pinyin/temp.dict.yaml", b"---\ncolumns: [a]\n...\nx\n"),
    ] {
        let sess = s.begin_edit(f).unwrap();
        std::fs::write(&sess.path, bad).unwrap();
        let r = s.end_edit(f).unwrap();
        assert!(!r.issues.is_empty(), "{f}");
        assert_eq!(std::fs::read(&sess.path).unwrap(), bad);
    }
    // 读不了（这里让它变成目录）：同样要报
    s.record_freq("wubi86", "a", "工").unwrap();
    let sess = s.begin_edit("wubi86/freq.table.yaml").unwrap();
    std::fs::remove_file(&sess.path).unwrap();
    std::fs::create_dir(&sess.path).unwrap();
    let r = s.end_edit("wubi86/freq.table.yaml").unwrap();
    assert!(!r.changed);
    assert!(!r.issues.is_empty(), "{:?}", r.issues);
    std::fs::remove_dir(&sess.path).unwrap();
    let r = s.end_edit("wubi86/freq.table.yaml").unwrap();
    assert!(r.issues.is_empty(), "又读得了就清掉：{:?}", r.issues);
}

/// 没用过的方案：临时词 / 词频 begin 按空表新建（带说明行）。
#[test]
fn learned_begin_creates_missing_file() {
    let s = open("learned_new");
    for (f, b) in [
        ("wubi86/temp.dict.yaml", wind_userdata::text::banner::TEMP),
        ("wubi86/freq.table.yaml", wind_userdata::text::banner::FREQ),
    ] {
        let sess = s.begin_edit(f).unwrap();
        let text = std::fs::read_to_string(&sess.path).unwrap();
        assert!(text.starts_with(b), "{text}");
        s.end_edit(f).unwrap();
    }
}

/// 没有文件的方案：begin 按空表新建（带说明行）。
#[test]
fn words_begin_creates_missing_file() {
    let s = open("words_new");
    let sess = s.begin_edit("wubi86/user.dict.yaml").unwrap();
    let text = std::fs::read_to_string(&sess.path).unwrap();
    assert!(text.starts_with('#'), "{text}");
    s.end_edit("wubi86/user.dict.yaml").unwrap();
}

/// 草稿、联想历史、补全、统计由程序维护，不提供编辑；越界路径拒绝。
#[test]
fn machine_files_and_outside_paths_are_rejected() {
    let s = open("reject");
    for p in [
        "pinyin/draft.dict.yaml",
        "pinyin/assoc.table.yaml",
        "completion.table.yaml",
        "stats/2026-10.toml",
    ] {
        let e = s.begin_edit(p).unwrap_err().to_string();
        assert!(e.contains("自动维护"), "{p}: {e}");
    }
    assert!(s.begin_edit("/etc/passwd").is_err());
    assert!(s.begin_edit("../x/user.dict.yaml").is_err());
    assert!(s.edit_sessions().is_empty());
}

/// 小表编辑期间界面修改被挡住，结束后照常；手改内容在 end 后生效。
#[test]
fn small_tables_reject_writes_while_editing() {
    let s = open("small");
    let sess = s.begin_edit(&s.phrases_path().to_string_lossy()).unwrap();
    assert_eq!(sess.kind.as_str(), "phrases");
    assert!(sess.path.is_file(), "不存在按空表新建");
    let e = s.add_phrase("dz", "地址", 0, 1).unwrap_err().to_string();
    assert!(e.contains("正在外部编辑"), "{e}");
    std::fs::write(
        &sess.path,
        std::fs::read_to_string(&sess.path).unwrap() + "yx\tme@x.com\t1\n",
    )
    .unwrap();
    let r = s.end_edit("phrases.dict.yaml").unwrap();
    assert!(r.changed);
    assert!(r.reloaded.contains(&"phrases".to_string()));
    s.add_phrase("dz", "地址", 0, 1).unwrap();
    let texts: Vec<String> = s
        .list_phrases()
        .unwrap()
        .into_iter()
        .map(|p| p.text)
        .collect();
    assert!(texts.contains(&"me@x.com".to_string()) && texts.contains(&"地址".to_string()));

    s.begin_edit("pinyin/shadow.dict.yaml").unwrap();
    assert!(s.pin_shadow("pinyin", "a", "啊", None, 0).is_err());
    assert!(s.clear_shadow("pinyin").is_err());
    assert!(
        s.pin_shadow("wubi86", "a", "工", None, 0).is_ok(),
        "别的方案不受影响"
    );
    s.end_edit("pinyin/shadow.dict.yaml").unwrap();
    s.pin_shadow("pinyin", "a", "啊", None, 0).unwrap();

    s.begin_edit("quick_format.toml").unwrap();
    assert!(s.add_quick_format("date", "u1", "{y}").is_err());
    let r = s.end_edit("quick_format.toml").unwrap();
    assert!(!r.changed, "没改过");
    s.add_quick_format("date", "u1", "{y}").unwrap();

    s.begin_edit("common_chars.yaml").unwrap();
    assert!(s.set_common_char_override("槮", true).is_err());
    assert!(s.clear_common_char_overrides().is_err());
    s.end_edit("common_chars.yaml").unwrap();
    s.set_common_char_override("槮", true).unwrap();

    assert!(
        s.begin_edit("charsets/rare.yaml").is_err(),
        "没有个人调整的类不新建"
    );
    s.set_charset_doc("rare", "name: x\n").unwrap();
    s.begin_edit("charsets/rare.yaml").unwrap();
    assert!(s.set_charset_doc("rare", "name: y\n").is_err());
    assert!(s.clear_charset_docs().is_err());
    assert!(s.set_charset_doc("other", "name: z\n").is_ok());
    s.end_edit("charsets/rare.yaml").unwrap();
    s.remove_charset_doc("rare").unwrap();
}

/// 同一文件重复 begin 是同一会话（刷新时刻，不再折回）；超过租约自动按 end 处理。
#[test]
fn repeated_begin_and_lease_expiry() {
    let s = open("lease");
    s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
        .unwrap();
    let a = s.begin_edit("pinyin/user.dict.yaml").unwrap();
    let b = s.begin_edit(&a.path.to_string_lossy()).unwrap();
    assert_eq!(a.path, b.path);
    assert_eq!(s.edit_sessions().len(), 1);
    s.begin_edit("phrases.dict.yaml").unwrap();
    let fired = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let f2 = fired.clone();
    s.set_edit_expired_hook(std::sync::Arc::new(move |c: &[String]| {
        f2.lock().unwrap().extend(c.iter().cloned())
    }));
    std::fs::write(
        s.phrases_path(),
        std::fs::read_to_string(s.phrases_path()).unwrap() + "yx\tme@x.com\t1\n",
    )
    .unwrap();
    assert_eq!(
        s.expire_edit_sessions(std::time::Duration::from_secs(3600)),
        0
    );
    assert_eq!(s.expire_edit_sessions(std::time::Duration::ZERO), 2);
    assert!(s.edit_sessions().is_empty());
    assert!(s.held_edit_files().is_empty());
    assert!(fired.lock().unwrap().contains(&"phrases".to_string()));
    s.add_phrase("dz", "地址", 0, 1).unwrap();
}

/// 租约到期时结束失败（这里让会话的路径解析不了来模拟）也要删会话、恢复写回：否则暂停永远
/// 不放，delta 越积越多、压实被挡住。文件之后照常走隔离 / 退避。
#[test]
fn lease_expiry_releases_hold_even_if_end_fails() {
    let s = open("lease_fail");
    s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
        .unwrap();
    let sess = s.begin_edit("pinyin/user.dict.yaml").unwrap();
    {
        let mut m = s.edit_sessions.lock().unwrap();
        let e = m.remove(&sess.path).unwrap();
        m.insert(s.user_text_dir().join("../outside.yaml"), e);
    }
    assert!(s.user_words_held("pinyin"));
    assert_eq!(s.expire_edit_sessions(std::time::Duration::ZERO), 1);
    assert!(s.edit_sessions().is_empty());
    assert!(!s.user_words_held("pinyin"));
    assert!(s.held_edit_files().is_empty());
}

/// 租约从最后活动算：开始 / 重复 begin 的时刻与文件 mtime 取晚者。开始很久但刚保存过 ⇒ 不到期；
/// 两者都早于租约 ⇒ 到期。
#[test]
fn lease_counts_from_last_save() {
    let s = open("lease_mtime");
    let sess = s.begin_edit("phrases.dict.yaml").unwrap();
    let hours = |h: u64| std::time::Duration::from_secs(h * 3600);
    let backdate = |s: &Store| {
        for e in s.edit_sessions.lock().unwrap().values_mut() {
            e.active = std::time::SystemTime::now() - hours(3);
        }
    };
    backdate(&s);
    assert_eq!(s.expire_edit_sessions(hours(2)), 0, "刚保存过不到期");
    let f = std::fs::File::options()
        .write(true)
        .open(&sess.path)
        .unwrap();
    f.set_modified(std::time::SystemTime::now() - hours(3))
        .unwrap();
    drop(f);
    assert_eq!(s.expire_edit_sessions(hours(2)), 1);
    assert!(s.edit_sessions().is_empty());
}

/// end 同步期间有人重新 begin 同一文件：end 收尾不删新会话、不放掉它的暂停写回。
/// 按 end 的三段（取代次 → 同步 → 收尾）顺序调用，模拟交错。
#[test]
fn end_does_not_release_a_session_begun_meanwhile() {
    let s = open("end_race");
    let f = "pinyin/user.dict.yaml";
    s.begin_edit(f).unwrap();
    let (path, tp) = s.resolve_edit_path(f).unwrap();
    let held = |s: &Store| s.user_words_held("pinyin");

    // 重复 begin 插在同步与收尾之间
    let (g, _) = s.session_state(&path);
    s.sync_edited(&path, &tp, None).unwrap();
    s.begin_edit(f).unwrap();
    s.close_session(&path, &tp, g);
    assert!(held(&s) && s.edit_sessions().len() == 1, "新会话继续");

    // 没有会话的 end（core 重启过）同步期间有人新开会话
    s.end_edit(f).unwrap();
    assert!(!held(&s) && s.edit_sessions().is_empty());
    let (g, _) = s.session_state(&path);
    s.sync_edited(&path, &tp, None).unwrap();
    s.begin_edit(f).unwrap();
    s.close_session(&path, &tp, g);
    assert!(held(&s) && s.edit_sessions().len() == 1, "新会话继续");

    s.end_edit(f).unwrap();
    assert!(!held(&s) && s.edit_sessions().is_empty());
}

/// 分库 / 开关文件结束编辑：重新解析并报问题；按指纹判断变没变（没改就不让调用方重建方案）。
#[test]
fn user_dict_end_reparses_and_detects_change() {
    let s = open("udl_end");
    let lib = s.create_user_dict("wubi86", "我的").unwrap();
    let f = "wubi86/dicts/我的.dict.yaml";
    s.begin_edit(f).unwrap();
    let r = s.end_edit(f).unwrap();
    assert!(!r.changed && r.reloaded.is_empty(), "{:?}", r.reloaded);

    s.begin_edit(f).unwrap();
    let text = std::fs::read_to_string(&lib).unwrap();
    std::fs::write(&lib, text + "戈\ta\t100\n坏行\n").unwrap();
    let r = s.end_edit(f).unwrap();
    assert!(r.changed);
    assert_eq!(r.reloaded, ["dicts:wubi86"]);
    assert_eq!(r.issues.len(), 1, "坏行要报");

    let m = "wubi86/dicts.toml";
    let sess = s.begin_edit(m).unwrap();
    let r = s.end_edit(m).unwrap();
    assert!(!r.changed && r.issues.is_empty());
    s.begin_edit(m).unwrap();
    std::fs::write(&sess.path, "not = [valid").unwrap();
    let r = s.end_edit(m).unwrap();
    assert!(r.changed);
    assert_eq!(r.issues.len(), 1, "格式坏要报");
}

/// 定位数据目录：根、方案目录、数据文件可以，越界不行。
#[test]
fn locate_accepts_root_schema_dir_and_files() {
    let s = open("locate");
    let root = s.user_text_dir().to_path_buf();
    assert_eq!(s.locate_data_path(None).unwrap(), root);
    assert_eq!(s.locate_data_path(Some("")).unwrap(), root);
    assert_eq!(
        s.locate_data_path(Some("pinyin")).unwrap(),
        root.join("pinyin")
    );
    assert_eq!(
        s.locate_data_path(Some("pinyin/temp.dict.yaml")).unwrap(),
        root.join("pinyin").join("temp.dict.yaml")
    );
    assert!(s.locate_data_path(Some("..")).is_err());
    assert!(s.locate_data_path(Some("/etc")).is_err());
    assert!(s.locate_data_path(Some("pinyin/x.txt")).is_err());
}

/// 设置端「误读表 → 打开文件」：用户误读表在数据目录根下，与引擎读它的位置是同一个。
#[test]
fn locate_accepts_user_misread_table() {
    let s = open("locate_misread");
    let root = s.user_text_dir().to_path_buf();
    assert_eq!(
        s.locate_data_path(Some(crate::MISREAD_FILE)).unwrap(),
        root.join("pinyin_misread.yaml")
    );
    assert!(
        s.locate_data_path(Some("pinyin/pinyin_misread.yaml"))
            .is_err()
    );
    assert!(s.begin_edit(crate::MISREAD_FILE).is_err(), "不走编辑会话");
    assert_eq!(
        s.reload_user_text_paths(&[root
            .join(crate::MISREAD_FILE)
            .to_string_lossy()
            .into_owned()])
            .unwrap(),
        Vec::<String>::new(),
        "误读表随配置重载生效，store 无事可做"
    );
}

/// 用户数据锁被别的进程占着（引擎只读）⇒ begin 拒绝。
#[test]
fn readonly_engine_rejects_begin() {
    let d = std::env::temp_dir().join(format!("wind_xedit_ro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let loc = crate::store::StateLoc {
        state_dir: d.join("state"),
        dir_id: "x".into(),
        text_dir: d.join("data"),
        legacy_done: true,
    };
    let a = Store::open_with_state(d.join("a.redb"), loc.clone()).unwrap();
    let b = Store::open_with_state(d.join("b.redb"), loc).unwrap();
    assert!(b.runtime().data().readonly(), "前置：第二个实例只读");
    assert!(
        b.begin_edit("phrases.dict.yaml")
            .unwrap_err()
            .to_string()
            .contains("只读")
    );
    a.begin_edit("phrases.dict.yaml").unwrap();
    drop((a, b));
    let _ = std::fs::remove_dir_all(&d);
}

/// 用户分库与开关文件：可重载、可编辑；结束编辑改过才报「dicts:<数据方案>」（调用方据此失效方案）。
#[test]
fn user_dict_libs_reload_and_edit() {
    let s = open("udl");
    let root = s.user_text_dir().to_path_buf();
    let lib = s.create_user_dict("pinyin", "成语").unwrap();
    assert_eq!(
        s.reload_user_text_paths(&[lib.to_string_lossy().into_owned()])
            .unwrap(),
        ["dicts:pinyin"]
    );
    let meta = root.join("pinyin/dicts.toml");
    assert_eq!(
        s.reload_user_text_paths(&[meta.to_string_lossy().into_owned()])
            .unwrap(),
        ["dicts:pinyin"]
    );
    for bad in [
        "pinyin/dicts/sub/x.dict.yaml",
        "pinyin/dicts/.x.dict.yaml",
        "pinyin/dicts/x.txt",
    ] {
        assert!(
            s.reload_user_text_paths(&[root.join(bad).to_string_lossy().into_owned()])
                .is_err(),
            "{bad}"
        );
    }

    let sess = s.begin_edit("pinyin/dicts/成语.dict.yaml").unwrap();
    assert_eq!(sess.kind.as_str(), "user_dicts");
    assert!(
        s.remove_user_dict_entry("pinyin", "成语", "a", "b")
            .is_err(),
        "编辑期间界面改同一分库被挡"
    );
    let r = s.end_edit(&sess.path.to_string_lossy()).unwrap();
    assert!(r.reloaded.is_empty(), "没改不重建：{:?}", r.reloaded);
    assert!(
        s.begin_edit("pinyin/dicts/无.dict.yaml").is_err(),
        "分库不存在"
    );

    let sess = s.begin_edit("pinyin/dicts.toml").unwrap();
    let text = std::fs::read_to_string(&sess.path).unwrap();
    assert!(
        text.starts_with(wind_userdata::text::banner::USER_DICTS_META),
        "{text}"
    );
    assert!(
        s.set_user_dict_enabled("pinyin", "pinyin", "成语", false)
            .is_err(),
        "编辑期间开关被挡"
    );
    assert!(
        s.delete_user_dict("pinyin", "成语").is_err(),
        "编辑开关文件期间删分库被挡"
    );
    assert!(
        s.user_text_dir()
            .join("pinyin/dicts/成语.dict.yaml")
            .is_file(),
        "被挡时文件不能已经删掉"
    );
    s.end_edit("pinyin/dicts.toml").unwrap();
    s.set_user_dict_enabled("pinyin", "pinyin", "成语", false)
        .unwrap();
}
