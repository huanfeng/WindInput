use super::*;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

/// 出厂关闭的系统短语：列表里呈现为关、不进输入期短语集；用户打开后生效；
/// 「恢复默认」回到出厂的关，而不是一律打开。
#[test]
fn system_phrase_default_off_survives_restore_defaults() {
    let path = tmp("wind_phrases_default_off.redb");
    let s = Store::open(&path).unwrap();
    let sys = vec![
        SystemPhrase {
            code: "tmts".into(),
            text: "$ts".into(),
            weight: 800,
            position: 0,
            category: String::new(),
            enabled: false,
        },
        SystemPhrase {
            code: "rq".into(),
            text: "$date".into(),
            weight: 1000,
            position: 0,
            category: String::new(),
            enabled: true,
        },
    ];
    s.set_system_phrases(&sys).unwrap();
    let enabled_of = |code: &str| {
        s.list_phrases()
            .unwrap()
            .into_iter()
            .find(|p| p.code == code)
            .map(|p| p.enabled)
    };
    assert_eq!(enabled_of("tmts"), Some(false), "出厂关闭照样列出、标为关");
    s.set_phrase_enabled("tmts", "$ts", true).unwrap();
    s.set_phrase_enabled("rq", "$date", false).unwrap();
    assert_eq!(enabled_of("tmts"), Some(true), "用户打开后生效");
    s.reset_system_enabled().unwrap();
    assert_eq!(enabled_of("tmts"), Some(false), "恢复默认回到出厂的关");
    assert_eq!(enabled_of("rq"), Some(true), "出厂开的恢复成开");
}

#[test]
fn sync_system_phrases_add_update_remove() {
    let path = tmp("wind_phrases_sync.redb");
    let s = Store::open(&path).unwrap();
    // 首轮：加两条系统短语
    let v1 = vec![
        SystemPhrase {
            code: "rq".into(),
            text: "$date".into(),
            weight: 1000,
            position: 0,
            category: String::new(),
            enabled: true,
        },
        SystemPhrase {
            code: "em".into(),
            text: "（＾＿＾）".into(),
            weight: 1000,
            position: 0,
            category: String::new(),
            enabled: true,
        },
    ];
    let st = s.sync_system_phrases(&v1).unwrap();
    assert_eq!((st.added, st.updated, st.removed), (2, 0, 0));
    // 用户关掉一条系统短语
    s.set_phrase_enabled("em", "（＾＿＾）", false).unwrap();
    // 次轮：em 改权重 + 删 rq + 加新 nn；em 的 enabled 应保留 false
    let v2 = vec![
        SystemPhrase {
            code: "em".into(),
            text: "（＾＿＾）".into(),
            weight: 500,
            position: 0,
            category: String::new(),
            enabled: true,
        },
        SystemPhrase {
            code: "nn".into(),
            text: "你好".into(),
            weight: 1000,
            position: 0,
            category: String::new(),
            enabled: true,
        },
    ];
    let st2 = s.sync_system_phrases(&v2).unwrap();
    assert_eq!((st2.added, st2.updated, st2.removed), (1, 1, 1));
    let list = s.list_phrases().unwrap();
    let em = list.iter().find(|p| p.code == "em").unwrap();
    assert_eq!(em.weight, 500, "内容更新");
    assert!(!em.enabled, "开关保留");
    assert!(em.is_system);
    assert!(!list.iter().any(|p| p.code == "rq"), "过时系统短语删除");
    assert!(list.iter().any(|p| p.code == "nn"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn sync_keeps_user_phrases() {
    let path = tmp("wind_phrases_sync_user.redb");
    let s = Store::open(&path).unwrap();
    s.add_phrase("me", "自定义", 0, 1).unwrap(); // 用户短语 is_system=false
    s.sync_system_phrases(&[SystemPhrase {
        code: "sys".into(),
        text: "系统".into(),
        weight: 1,
        position: 0,
        category: String::new(),
        enabled: true,
    }])
    .unwrap();
    // 再同步（sys 消失）应删 sys 但保留用户 me
    s.sync_system_phrases(&[]).unwrap();
    let list = s.list_phrases().unwrap();
    assert!(
        list.iter().any(|p| p.code == "me"),
        "用户短语不受系统同步影响"
    );
    assert!(!list.iter().any(|p| p.code == "sys"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn phrase_sys_hash_persist() {
    let path = tmp("wind_phrases_hash.redb");
    let s = Store::open(&path).unwrap();
    assert_eq!(s.phrase_sys_hash().unwrap(), None);
    s.set_phrase_sys_hash("abc123").unwrap();
    assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("abc123"));
    let _ = std::fs::remove_file(&path);
}

/// 没有说明行的旧标记文件照常读出。
#[test]
fn phrase_sys_hash_reads_file_without_banner() {
    let path = tmp("wind_phrases_hash_old.redb");
    let s = Store::open(&path).unwrap();
    std::fs::create_dir_all(s.user_text_dir()).unwrap();
    std::fs::write(s.phrase_sys_hash_path(), "abc123\n").unwrap();
    assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("abc123"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn list_system_and_user_split() {
    let path = tmp("wind_phrases_split.redb");
    let s = Store::open(&path).unwrap();
    s.sync_system_phrases(&[
        SystemPhrase {
            code: "a".into(),
            text: "甲".into(),
            weight: 1,
            position: 0,
            category: String::new(),
            enabled: true,
        },
        SystemPhrase {
            code: "b".into(),
            text: "乙".into(),
            weight: 1,
            position: 0,
            category: String::new(),
            enabled: true,
        },
    ])
    .unwrap();
    s.add_phrase("u1", "用户一", 0, 1).unwrap();
    s.add_phrase("u2", "用户二", 0, 1).unwrap();
    assert_eq!(s.list_system_phrases().unwrap().len(), 2);
    let (page, total) = s.list_user_phrases_paged(None, 0, 10).unwrap();
    assert_eq!(total, 2);
    assert_eq!(page.len(), 2);
    assert!(page.iter().all(|p| !p.is_system));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn user_paging_and_prefix() {
    let path = tmp("wind_phrases_page.redb");
    let s = Store::open(&path).unwrap();
    for i in 0..5 {
        s.add_phrase(&format!("c{i}"), &format!("词{i}"), 0, 1)
            .unwrap();
    }
    let (p0, total) = s.list_user_phrases_paged(None, 0, 2).unwrap();
    assert_eq!(total, 5);
    assert_eq!(p0.len(), 2);
    let (p2, _) = s.list_user_phrases_paged(None, 4, 2).unwrap();
    assert_eq!(p2.len(), 1, "末页不足一页");
    // prefix 过滤
    let (pf, tf) = s.list_user_phrases_paged(Some("c3"), 0, 10).unwrap();
    assert_eq!(tf, 1);
    assert_eq!(pf[0].code, "c3");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn enabled_for_input_and_resets() {
    let path = tmp("wind_phrases_enabled.redb");
    let s = Store::open(&path).unwrap();
    s.sync_system_phrases(&[SystemPhrase {
        code: "a".into(),
        text: "甲".into(),
        weight: 1,
        position: 0,
        category: String::new(),
        enabled: true,
    }])
    .unwrap();
    s.add_phrase("u", "用户", 0, 1).unwrap();
    s.set_phrase_enabled("a", "甲", false).unwrap(); // 禁用系统
    let inp = s.enabled_phrases_for_input().unwrap();
    assert!(inp.iter().all(|p| p.enabled));
    assert!(!inp.iter().any(|p| p.code == "a"), "禁用项不入输入集");
    assert!(inp.iter().any(|p| p.code == "u"));
    // 系统恢复默认：全部重新启用
    let n = s.reset_system_enabled().unwrap();
    assert_eq!(n, 1);
    assert!(
        s.enabled_phrases_for_input()
            .unwrap()
            .iter()
            .any(|p| p.code == "a")
    );
    // 用户清空
    assert_eq!(s.reset_user_phrases().unwrap(), 1);
    assert!(!s.list_phrases().unwrap().iter().any(|p| !p.is_system));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_phrase_crud() {
    let path = tmp("wind_phrases_crud.redb");
    let s = Store::open(&path).unwrap();
    s.add_phrase("rq", "2026-06-20", 0, 1).unwrap();
    s.add_phrase("yx", "user@example.com", 0, 1).unwrap();
    assert_eq!(s.list_phrases().unwrap().len(), 2);

    // 启停
    s.set_phrase_enabled("rq", "2026-06-20", false).unwrap();
    let rq = s
        .list_phrases()
        .unwrap()
        .into_iter()
        .find(|p| p.code == "rq")
        .unwrap();
    assert!(!rq.enabled);

    // 改 code（键迁移）
    s.update_phrase("yx", "user@example.com", Some("mail"), None, None, Some(5))
        .unwrap();
    let list = s.list_phrases().unwrap();
    assert!(list.iter().any(|p| p.code == "mail" && p.weight == 5));
    assert!(!list.iter().any(|p| p.code == "yx"));

    // 删除
    s.remove_phrase("rq", "2026-06-20").unwrap();
    assert_eq!(s.list_phrases().unwrap().len(), 1);

    // 重置清空
    assert_eq!(s.reset_phrases().unwrap(), 1);
    assert_eq!(s.list_phrases().unwrap().len(), 0);
    let _ = std::fs::remove_file(&path);
}

/// 导入撞键与 `add_phrase` 同口径：wdict 导入一条与系统短语同款的记录 → 遮蔽系统条目。
///
/// 导入是撞键的高发路径（用户常在导出文件里手工增删行），归属规则必须与手工新增一致，
/// 否则同一件事经两条入口会得到相反的归属。
#[test]
fn import_duplicate_shadows_system_entry() {
    let path = tmp("wind_phrases_keep_sys.redb");
    let s = Store::open(&path).unwrap();
    let sys = [SystemPhrase {
        code: "date".into(),
        text: "二〇二六年".into(),
        weight: 9,
        position: 5,
        category: String::new(),
        enabled: true,
    }];
    s.sync_system_phrases(&sys).unwrap();

    let wd = crate::wdict::export_phrases_wdict(
        &[crate::wdict::PhraseIo {
            code: "date".into(),
            text: "二〇二六年".into(),
            weight: 7,
            position: 3,
            enabled: true,
        }],
        "2026-07-21T00:00:00+08:00",
    );
    s.import_user_phrases_wdict(&wd).unwrap();

    let (rows, total) = s.list_user_phrases_paged(None, 0, 99).unwrap();
    assert_eq!(total, 1, "导入的条目归用户");
    assert!(rows[0].overrides_system, "记录遮蔽关系");
    assert_eq!((rows[0].weight, rows[0].position), (7, 3), "用导入的定义");
    assert!(s.list_system_phrases().unwrap().is_empty(), "系统条目隐去");

    // 同样可撤销：恢复默认认领回系统。
    assert_eq!(s.reclaim_system_phrases(&sys).unwrap(), 1);
    assert_eq!(s.list_system_phrases().unwrap().len(), 1);
    let _ = std::fs::remove_file(&path);
}

/// 撞键归属：用户建一条与系统短语完全同款的短语 → 用户那条生效，系统条目隐去。
/// 同时锁住撤销路径，否则系统条目一被遮蔽就再也回不来。
#[test]
fn user_phrase_shadows_system_entry() {
    let path = tmp("wind_phrases_shadow_sys.redb");
    let s = Store::open(&path).unwrap();
    let sys = [SystemPhrase {
        code: "date".into(),
        text: "$Y年$M月$D日".into(),
        weight: 1000,
        position: 1,
        category: String::new(),
        enabled: true,
    }];
    s.sync_system_phrases(&sys).unwrap();

    // 用户建了一条一模一样的（同 code 同 text），并给了自己的权重/位置。
    s.add_phrase("date", "$Y年$M月$D日", 9, 5000).unwrap();

    // ① 出现在用户短语列表，且用的是用户写的 weight/position。
    let (user_rows, total) = s.list_user_phrases_paged(None, 0, 99).unwrap();
    assert_eq!(total, 1, "用户建的短语必须在用户列表可见");
    assert_eq!((user_rows[0].weight, user_rows[0].position), (5000, 9));
    assert!(!user_rows[0].is_system, "归属转为用户");
    assert!(user_rows[0].overrides_system, "记录它遮蔽了系统条目");

    // ② 系统短语列表里隐去（只有一行，已归用户）。
    assert!(
        s.list_system_phrases().unwrap().is_empty(),
        "被遮蔽的系统条目从系统列表隐去"
    );

    // ③ 输入期生效的是用户那条。
    let inp = s.enabled_phrases_for_input().unwrap();
    assert_eq!(inp.len(), 1);
    assert_eq!(inp[0].weight, 5000, "输入期用用户定义");

    // ④ 导出（备份）含它，否则用户写的定义备份即丢。
    assert!(
        s.export_user_phrases_wdict("t")
            .unwrap()
            .contains("$Y年$M月$D日"),
        "遮蔽行须随用户短语导出"
    );

    // ⑤ 撤销路径：「系统恢复默认」→ reclaim 认领回系统 + sync 刷回 TOML 定义。
    assert_eq!(s.reclaim_system_phrases(&sys).unwrap(), 1);
    s.sync_system_phrases(&sys).unwrap();
    let after = s.list_system_phrases().unwrap();
    assert_eq!(after.len(), 1, "系统条目回来了");
    assert_eq!(
        (after[0].weight, after[0].position),
        (1000, 1),
        "还原成定义值"
    );
    assert!(!after[0].overrides_system);
    assert_eq!(s.list_user_phrases_paged(None, 0, 99).unwrap().1, 0);
    let _ = std::fs::remove_file(&path);
}

/// 撤销路径二：「清空用户短语」删掉遮蔽行后，系统条目立即重新呈现（系统条目来自 TOML、
/// 只在内存，从不随用户行一起消失）。
#[test]
fn clearing_user_phrases_restores_shadowed_system_entry() {
    let path = tmp("wind_phrases_clear_restore.redb");
    let s = Store::open(&path).unwrap();
    let sys = [SystemPhrase {
        code: "date".into(),
        text: "$Y年$M月$D日".into(),
        weight: 1000,
        position: 1,
        category: String::new(),
        enabled: true,
    }];
    s.sync_system_phrases(&sys).unwrap();
    s.add_phrase("date", "$Y年$M月$D日", 9, 5000).unwrap();

    // 清空用户短语：遮蔽行归属用户，会被删掉 → 系统条目以 TOML 定义值重新呈现。
    assert_eq!(s.reset_user_phrases().unwrap(), 1);
    let after = s.list_system_phrases().unwrap();
    assert_eq!(after.len(), 1, "系统条目应被补回");
    assert_eq!((after[0].weight, after[0].position), (1000, 1));
    let _ = std::fs::remove_file(&path);
}

/// `ensure_system_phrases` 绝不改动已存在的行（用 sync 代替它会把用户对系统短语改过的
/// 权重/次序重置回 TOML 默认值）。系统条目总在，删掉修改行即恢复默认，没有缺口可补，返回 0。
#[test]
fn ensure_system_phrases_only_fills_gaps() {
    let path = tmp("wind_phrases_ensure.redb");
    let s = Store::open(&path).unwrap();
    let sys = [
        SystemPhrase {
            code: "a".into(),
            text: "甲".into(),
            weight: 1000,
            position: 1,
            category: String::new(),
            enabled: true,
        },
        SystemPhrase {
            code: "b".into(),
            text: "乙".into(),
            weight: 1000,
            position: 2,
            category: String::new(),
            enabled: true,
        },
    ];
    s.sync_system_phrases(&sys).unwrap();

    // 用户在系统短语列表里改了「甲」的权重、并把「乙」停用。
    s.update_phrase("a", "甲", None, None, Some(9), Some(5555))
        .unwrap();
    s.set_phrase_enabled("b", "乙", false).unwrap();
    // 删掉「乙」的修改行 ⇒ 乙恢复默认（启用），不会出现缺口
    s.remove_phrase("b", "乙").unwrap();

    assert_eq!(s.ensure_system_phrases(&sys).unwrap(), 0, "没有缺口");

    let list = s.list_system_phrases().unwrap();
    let a = list.iter().find(|p| p.code == "a").unwrap();
    assert_eq!(
        (a.weight, a.position),
        (5555, 9),
        "已存在的行一字不改——这正是不能用 sync 的原因"
    );
    assert!(
        list.iter().any(|p| p.code == "b" && p.enabled),
        "删掉修改行 = 恢复默认"
    );

    // 幂等：再补一次不重复插入。
    assert_eq!(s.ensure_system_phrases(&sys).unwrap(), 0);
    let _ = std::fs::remove_file(&path);
}

/// 对照：同样的场景下 `sync_system_phrases` 会重置用户的编辑。
/// 锁住这个差异，避免日后有人「顺手统一成 sync」。
#[test]
fn sync_overwrites_user_edits_on_system_rows() {
    let path = tmp("wind_phrases_sync_overwrites.redb");
    let s = Store::open(&path).unwrap();
    let sys = [SystemPhrase {
        code: "a".into(),
        text: "甲".into(),
        weight: 1000,
        position: 1,
        category: String::new(),
        enabled: true,
    }];
    s.sync_system_phrases(&sys).unwrap();
    s.update_phrase("a", "甲", None, None, Some(9), Some(5555))
        .unwrap();

    s.sync_system_phrases(&sys).unwrap();
    let a = s.list_system_phrases().unwrap().pop().unwrap();
    assert_eq!(
        (a.weight, a.position),
        (1000, 1),
        "sync 以 TOML 为准（供恢复默认/文件变更用），故补齐场景不能用它"
    );
    let _ = std::fs::remove_file(&path);
}

/// 编辑既有系统短语（改权重）不改归属——那是在系统列表里正常调整，不产生遮蔽关系。
#[test]
fn editing_system_phrase_keeps_system_ownership() {
    let path = tmp("wind_phrases_edit_no_mark.redb");
    let s = Store::open(&path).unwrap();
    s.sync_system_phrases(&[SystemPhrase {
        code: "em".into(),
        text: "（＾＿＾）".into(),
        weight: 1000,
        position: 0,
        category: String::new(),
        enabled: true,
    }])
    .unwrap();
    s.update_phrase("em", "（＾＿＾）", None, None, None, Some(42))
        .unwrap();
    let row = s.list_system_phrases().unwrap().pop().unwrap();
    assert_eq!(row.weight, 42, "编辑生效");
    assert!(!row.overrides_system, "编辑不产生遮蔽关系");
    assert_eq!(s.list_user_phrases_paged(None, 0, 99).unwrap().1, 0);
    let _ = std::fs::remove_file(&path);
}

/// 存量修复：已被降级的行，「恢复默认」应认领回系统归属。
#[test]
fn reclaim_restores_downgraded_system_rows() {
    let path = tmp("wind_phrases_reclaim.redb");
    let s = Store::open(&path).unwrap();
    let sys = [SystemPhrase {
        code: "date".into(),
        text: "二〇二六年".into(),
        weight: 9,
        position: 5,
        category: String::new(),
        enabled: true,
    }];
    // 手工制造受损现场：用户行在先，且与系统条目同键
    s.add_phrase("date", "二〇二六年", 0, 1).unwrap();
    s.sync_system_phrases(&sys).unwrap();
    assert!(
        s.list_system_phrases().unwrap().is_empty(),
        "受损现场：系统列表应为空"
    );

    assert_eq!(s.reclaim_system_phrases(&sys).unwrap(), 1);
    assert_eq!(s.list_system_phrases().unwrap().len(), 1);
    assert!(s.list_user_phrases_paged(None, 0, 99).unwrap().0.is_empty());
    // 幂等：再认领一次不重复计数
    assert_eq!(s.reclaim_system_phrases(&sys).unwrap(), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn sync_does_not_overwrite_user_row() {
    let path = tmp("wind_phrases_no_overwrite_user.redb");
    let s = Store::open(&path).unwrap();
    // 先建用户行 (bj, 北京)，is_system=false
    s.add_phrase("bj", "北京", 0, 1).unwrap();
    // 同步含同键的系统条目
    s.sync_system_phrases(&[SystemPhrase {
        code: "bj".into(),
        text: "北京".into(),
        weight: 9,
        position: 0,
        category: String::new(),
        enabled: true,
    }])
    .unwrap();
    // 用户行应保持 is_system=false，不被系统化
    let row = s
        .list_phrases()
        .unwrap()
        .into_iter()
        .find(|p| p.code == "bj")
        .unwrap();
    assert!(!row.is_system, "用户行不应被系统短语覆写为 is_system=true");
    // 模拟系统条目移除（sync 空列表）：用户行不应被删
    s.sync_system_phrases(&[]).unwrap();
    let list = s.list_phrases().unwrap();
    assert!(
        list.iter().any(|p| p.code == "bj" && !p.is_system),
        "系统条目移除后用户行不应被静默删除"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn export_import_user_phrases_roundtrip() {
    let path = tmp("wind_phrases_io.redb");
    let s = Store::open(&path).unwrap();
    s.add_phrase("bj", "北京", 0, 1000).unwrap();
    s.add_phrase("ml", "多行\n内容", 2, 500).unwrap();
    let text = s
        .export_user_phrases_wdict("2026-07-02T00:00:00+08:00")
        .unwrap();
    // 清空后再导入
    s.reset_user_phrases().unwrap();
    assert_eq!(s.list_user_phrases_paged(None, 0, 99).unwrap().1, 0);
    let (imported, skipped) = s.import_user_phrases_wdict(&text).unwrap();
    assert_eq!((imported, skipped), (2, 0));
    let (rows, total) = s.list_user_phrases_paged(None, 0, 99).unwrap();
    assert_eq!(total, 2);
    assert!(
        rows.iter()
            .any(|p| p.code == "ml" && p.text == "多行\n内容"),
        "多行往返无损"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn import_upsert_merges() {
    let path = tmp("wind_phrases_import_merge.redb");
    let s = Store::open(&path).unwrap();
    s.add_phrase("bj", "北京", 0, 1).unwrap();
    // 导入含同键(权重不同)+新键
    let text = crate::wdict::export_phrases_wdict(
        &[
            crate::wdict::PhraseIo {
                code: "bj".into(),
                text: "北京".into(),
                weight: 9,
                position: 0,
                enabled: true,
            },
            crate::wdict::PhraseIo {
                code: "sh".into(),
                text: "上海".into(),
                weight: 1,
                position: 0,
                enabled: true,
            },
        ],
        "t",
    );
    let (imported, _) = s.import_user_phrases_wdict(&text).unwrap();
    assert_eq!(imported, 2);
    let (rows, total) = s.list_user_phrases_paged(None, 0, 99).unwrap();
    assert_eq!(total, 2, "同键合并不新增行");
    assert_eq!(
        rows.iter().find(|p| p.code == "bj").unwrap().weight,
        9,
        "同键更新权重"
    );
    let _ = std::fs::remove_file(&path);
}

fn sys(code: &str, text: &str, w: i32) -> SystemPhrase {
    SystemPhrase {
        code: code.into(),
        text: text.into(),
        weight: w,
        position: 0,
        category: String::new(),
        enabled: true,
    }
}

/// 短语落文本：用户行进 phrases.dict.yaml；系统条目不进文件；修改系统条目写一行 overrides_system=1。
#[test]
fn phrases_live_in_text_as_overlay() {
    let s = Store::open(tmp("wind_ph_text.redb")).unwrap();
    s.set_system_phrases(&[sys("rq", "{date}", 100), sys("sj", "{time}", 100)])
        .unwrap();
    s.add_phrase("yx", "me@x.com", 0, 1800).unwrap();
    s.set_phrase_enabled("sj", "{time}", false).unwrap();
    let f = std::fs::read_to_string(s.phrases_path()).unwrap();
    assert!(f.contains("yx\tme@x.com\t1800\t0\t1\t\t0"), "{f}");
    assert!(f.contains("sj\t{time}\t100\t0\t0\t\t1"), "{f}");
    assert!(!f.contains("rq\t"), "未改的系统条目不进文件");
    let input: Vec<String> = s
        .enabled_phrases_for_input()
        .unwrap()
        .into_iter()
        .map(|p| p.code)
        .collect();
    assert!(input.contains(&"rq".to_string()) && input.contains(&"yx".to_string()));
    assert!(!input.contains(&"sj".to_string()));
    let sj = s
        .list_system_phrases()
        .unwrap()
        .into_iter()
        .find(|p| p.code == "sj")
        .unwrap();
    assert!(sj.is_system && !sj.enabled);
}

/// TOML 升级 ⇒ 修改行回到新 TOML 的权重、保留禁用；TOML 删掉的条目连同修改行消失。
#[test]
fn toml_upgrade_resets_edits_but_keeps_disabled() {
    let s = Store::open(tmp("wind_ph_upgrade.redb")).unwrap();
    s.set_system_phrases(&[sys("a", "甲", 10), sys("b", "乙", 10), sys("c", "丙", 10)])
        .unwrap();
    s.set_phrase_enabled("a", "甲", false).unwrap();
    s.update_phrase("b", "乙", None, None, None, Some(99))
        .unwrap();
    s.set_phrase_enabled("c", "丙", false).unwrap();
    let new = [sys("a", "甲", 20), sys("b", "乙", 30)];
    s.set_system_phrases(&new).unwrap();
    s.sync_system_phrases(&new).unwrap();
    let all = s.list_phrases().unwrap();
    let get = |c: &str| all.iter().find(|p| p.code == c).cloned();
    assert_eq!(get("a").map(|p| (p.weight, p.enabled)), Some((20, false)));
    assert_eq!(get("b").map(|p| (p.weight, p.enabled)), Some((30, true)));
    assert!(get("c").is_none());
    assert!(
        !std::fs::read_to_string(s.phrases_path())
            .unwrap()
            .contains("丙")
    );
}

/// 用户行遮蔽系统条目；清空用户短语后系统条目恢复，对系统条目的修改保留。
#[test]
fn user_rows_shadow_and_reset_keeps_system_edits() {
    let s = Store::open(tmp("wind_ph_shadowing.redb")).unwrap();
    s.set_system_phrases(&[sys("a", "甲", 10), sys("b", "乙", 10)])
        .unwrap();
    s.add_phrase("a", "甲", 3, 500).unwrap();
    let a = s
        .list_phrases()
        .unwrap()
        .into_iter()
        .find(|p| p.code == "a")
        .unwrap();
    assert!(!a.is_system && a.overrides_system && a.weight == 500);
    s.set_phrase_enabled("b", "乙", false).unwrap();
    s.reset_user_phrases().unwrap();
    let all = s.list_phrases().unwrap();
    let a = all.iter().find(|p| p.code == "a").unwrap();
    assert!(a.is_system && a.weight == 10);
    assert!(
        !all.iter().find(|p| p.code == "b").unwrap().enabled,
        "对系统条目的修改保留"
    );
}

/// phrase_sys_hash 离开 redb：落用户目录（跟着短语文件走），重开读回。
#[test]
fn phrase_sys_hash_lives_beside_phrases_file() {
    let p = tmp("wind_ph_hash_state.redb");
    {
        let s = Store::open(&p).unwrap();
        s.set_phrase_sys_hash("h1").unwrap();
        assert!(s.phrase_sys_hash_path().is_file());
        assert_eq!(s.phrase_sys_hash_path().parent(), Some(s.user_text_dir()));
    }
    let s = Store::open(&p).unwrap();
    assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h1"));
}

/// 系统短语同步哈希随用户目录走——本机状态目录是新的（换机 / 清 state）时不得丢失，
/// 否则会触发一次同步、把用户对系统短语的修改重置回出厂。
#[test]
fn phrase_sys_hash_roams_with_user_dir() {
    let base = std::env::temp_dir().join(format!("wind_ph_hash_roam_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let loc = |st: &str| crate::store::StateLoc {
        state_dir: base.join(st),
        dir_id: "d".into(),
        text_dir: base.join(crate::DATA_DIR_NAME),
        legacy_done: false,
    };
    {
        let s = Store::open_with_state(base.join("a.redb"), loc("state1")).unwrap();
        s.set_phrase_sys_hash("h1").unwrap();
    }
    let s = Store::open_with_state(base.join("b.redb"), loc("state2")).unwrap();
    assert_eq!(s.phrase_sys_hash().unwrap().as_deref(), Some("h1"));
}
