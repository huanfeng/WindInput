use super::*;

/// 每个测试独立文件：redb 是单写者，共用文件会让并发测试互相阻塞。
fn store(tag: &str) -> Store {
    let p = std::env::temp_dir().join(format!(
        "wind_completion_test_{}_{}_{:?}.redb",
        std::process::id(),
        tag,
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&p);
    Store::open(&p).unwrap()
}

fn reopen(tag: &str, s: Store) -> Store {
    drop(s);
    let p = std::env::temp_dir().join(format!(
        "wind_completion_test_{}_{}_{:?}.redb",
        std::process::id(),
        tag,
        std::thread::current().id()
    ));
    Store::open(&p).unwrap()
}

fn has(s: &Store, kind: CompletionKind, text: &str) -> bool {
    s.get_completion(kind, text).unwrap().is_some()
}

/// dict.reload 指名补全文件：手改的内容生效，不被内存里未落盘的旧版本盖掉。
#[test]
fn reload_keeps_hand_edit_even_with_unflushed_changes() {
    let s = store("reload_edit");
    s.record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    s.flush_completions().unwrap();
    s.record_completion(CompletionKind::EmailSuffix, "163.com")
        .unwrap(); // 未落盘
    let p = s.completion_path();
    let edited = std::fs::read_to_string(&p)
        .unwrap()
        .replace("qq.com", "hand.example");
    std::fs::write(&p, &edited).unwrap();
    assert!(s.reload_completions());
    assert!(has(&s, CompletionKind::EmailSuffix, "hand.example"));
    assert!(!has(&s, CompletionKind::EmailSuffix, "qq.com"));
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        edited,
        "手改的文件没被盖掉"
    );
}

/// M-2 / §2.5 机写文件：坏了从 `.bak` 自愈，之后照常学习、照常落盘。
#[test]
fn corrupt_completion_self_heals_from_bak() {
    let s = store("heal");
    s.record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    s.flush_completions().unwrap();
    s.record_completion(CompletionKind::EmailSuffix, "163.com")
        .unwrap();
    s.flush_completions().unwrap(); // .bak = 只有 qq.com 的那一版
    let p = s.completion_path();
    let s = reopen("heal", s);
    std::fs::write(&p, b"\xff garbage").unwrap();
    let s = reopen("heal", s);
    assert!(has(&s, CompletionKind::EmailSuffix, "qq.com"));
    s.record_completion(CompletionKind::EmailSuffix, "gmail.com")
        .unwrap();
    s.flush_completions().unwrap();
    let txt = std::fs::read_to_string(&p).unwrap();
    assert!(txt.contains("gmail.com") && txt.contains("qq.com"), "{txt}");
}

#[test]
fn completion_without_good_bak_is_kept_aside_and_rebuilt() {
    let s = store("rebuild");
    let p = s.completion_path();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, b"\xff garbage").unwrap();
    s.record_completion(CompletionKind::UrlHistory, "example.com")
        .unwrap();
    s.flush_completions().unwrap();
    let kept = std::fs::read_dir(s.user_text_dir().join(".backup"))
        .unwrap()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("completion.table.yaml.corrupt-")
        });
    assert!(kept);
    assert!(std::fs::read_to_string(&p).unwrap().contains("example.com"));
}

#[test]
fn completion_file_is_table_text_in_data_dir() {
    let s = store("table");
    s.record_completion(CompletionKind::QuickHistory, "a\tb")
        .unwrap();
    s.flush_completions().unwrap();
    let p = s.completion_path();
    assert!(p.ends_with("completion.table.yaml"));
    assert!(p.starts_with(s.user_text_dir()));
    let txt = std::fs::read_to_string(&p).unwrap();
    assert!(txt.contains("kind: completion"), "{txt}");
    assert!(txt.contains("quick_history\ta\\tb\t1\t"), "{txt}");
    let s = reopen("table", s);
    assert!(has(&s, CompletionKind::QuickHistory, "a\tb"));
}

#[test]
fn completion_bad_rows_are_skipped_and_counted() {
    let s = store("badrow");
    let p = s.completion_path();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        "---\ncolumns: [kind, text, count, last_used]\nwind: {kind: completion, escape: true}\n...\n# no comment\nemail_suffix\tqq.com\t2\t1\nnope\tx\t1\t1\nemail_suffix\ty.com\tz\t1\n",
    )
    .unwrap();
    assert!(has(&s, CompletionKind::EmailSuffix, "qq.com"));
    assert_eq!(
        wind_userdata::issues::list_under(&p),
        vec![wind_userdata::issues::FileIssue {
            path: p.clone(),
            kind: wind_userdata::issues::IssueKind::BadLines(2)
        }]
    );
    wind_userdata::issues::clear(&p);
}

#[test]
fn record_accumulates_count() {
    let s = store("acc");
    assert_eq!(
        s.get_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap(),
        None
    );

    assert!(
        s.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap()
    );
    assert!(
        s.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap()
    );
    let rec = s
        .get_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap()
        .expect("记过两次应查得到");
    assert_eq!(rec.count, 2, "同一条重复记录应累加而非新增");
    assert!(rec.last_used > 0, "last_used 应被填上");
}

#[test]
fn kinds_do_not_leak_into_each_other() {
    let s = store("kinds");
    s.record_completion(CompletionKind::EmailSuffix, "same.text")
        .unwrap();
    s.record_completion(CompletionKind::UrlHistory, "same.text")
        .unwrap();

    // 同一条文本在两个 kind 下各有独立计数。
    s.record_completion(CompletionKind::UrlHistory, "same.text")
        .unwrap();
    assert_eq!(
        s.get_completion(CompletionKind::EmailSuffix, "same.text")
            .unwrap()
            .unwrap()
            .count,
        1
    );
    assert_eq!(
        s.get_completion(CompletionKind::UrlHistory, "same.text")
            .unwrap()
            .unwrap()
            .count,
        2
    );

    // 清一类不影响另一类——设置里「清空网址历史」不该抹掉邮箱学习。
    assert_eq!(s.clear_completions(CompletionKind::UrlHistory).unwrap(), 1);
    assert_eq!(
        s.list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap()
            .1,
        0
    );
    assert_eq!(
        s.list_completions(CompletionKind::EmailSuffix, "", 0, 0)
            .unwrap()
            .1,
        1
    );
}

#[test]
fn list_filters_by_prefix_and_sorts_by_count() {
    let s = store("list");
    for _ in 0..3 {
        s.record_completion(CompletionKind::UrlHistory, "www.b.com")
            .unwrap();
    }
    s.record_completion(CompletionKind::UrlHistory, "www.a.com")
        .unwrap();
    s.record_completion(CompletionKind::UrlHistory, "bbs.c.com")
        .unwrap();

    // 前缀过滤：bbs. 那条不在 www. 的结果里。
    let (rows, total) = s
        .list_completions(CompletionKind::UrlHistory, "www.", 0, 0)
        .unwrap();
    assert_eq!(total, 2, "www. 前缀应命中两条，实际 {:?}", rows);
    // 次数多的在前，与字典序相反 —— 证明排的是频次而不是 redb 的返回序。
    assert_eq!(rows[0].0, "www.b.com");
    assert_eq!(rows[1].0, "www.a.com");

    // 空前缀 = 全部。
    assert_eq!(
        s.list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap()
            .1,
        3
    );
}

#[test]
fn list_paginates() {
    let s = store("page");
    for t in ["a", "b", "c"] {
        s.record_completion(CompletionKind::UrlHistory, t).unwrap();
    }
    let (page, total) = s
        .list_completions(CompletionKind::UrlHistory, "", 1, 1)
        .unwrap();
    assert_eq!(total, 3, "total 是命中总数而非本页条数");
    assert_eq!(page.len(), 1);
}

#[test]
fn remove_one_leaves_the_rest() {
    let s = store("rm");
    s.record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    s.record_completion(CompletionKind::EmailSuffix, "163.com")
        .unwrap();

    assert!(
        s.remove_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap()
    );
    assert!(
        !s.remove_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap(),
        "删第二次应返回 false 而不是报错"
    );
    assert_eq!(
        s.list_completions(CompletionKind::EmailSuffix, "", 0, 0)
            .unwrap()
            .1,
        1
    );
}

#[test]
fn prune_keeps_the_top_entries_by_the_display_order() {
    let s = store("prune");
    // hot 记 3 次、mid 2 次、cold 1 次 —— 裁剪应按补全展示的同一排序取舍。
    for (text, n) in [("hot", 3), ("mid", 2), ("cold", 1)] {
        for _ in 0..n {
            s.record_completion(CompletionKind::UrlHistory, text)
                .unwrap();
        }
    }
    assert_eq!(
        s.prune_completions(CompletionKind::UrlHistory, 2, None)
            .unwrap(),
        1
    );
    let (rows, total) = s
        .list_completions(CompletionKind::UrlHistory, "", 0, 0)
        .unwrap();
    assert_eq!(total, 2);
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["hot", "mid"]
    );

    // 未超上限时不动任何东西；max=0 表示不限。
    assert_eq!(
        s.prune_completions(CompletionKind::UrlHistory, 5, None)
            .unwrap(),
        0
    );
    assert_eq!(
        s.prune_completions(CompletionKind::UrlHistory, 0, None)
            .unwrap(),
        0
    );
    assert_eq!(
        s.list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap()
            .1,
        2
    );
}

/// `keep` 保护刚写入的那条 —— 没有它，表一满就再也学不进新东西。
///
/// 新条目 `count` 恒为 1，在次数降序里排在所有老条目之后，于是调用方「先记再裁」
/// 的顺序会让它**当场被自己这次裁剪删掉**。这条钉的就是那个结构性缺陷。
#[test]
fn prune_keeps_the_just_written_entry() {
    let s = store("prunekeep");
    for (text, n) in [("old_hot", 5), ("old_mid", 3)] {
        for _ in 0..n {
            s.record_completion(CompletionKind::UrlHistory, text)
                .unwrap();
        }
    }
    // 模拟「上屏一条新网址」：count=1，排序上垫底。
    s.record_completion(CompletionKind::UrlHistory, "fresh")
        .unwrap();

    assert_eq!(
        s.prune_completions(CompletionKind::UrlHistory, 2, Some("fresh"))
            .unwrap(),
        1
    );
    let (rows, total) = s
        .list_completions(CompletionKind::UrlHistory, "", 0, 0)
        .unwrap();
    assert_eq!(total, 2, "保留集仍恰好是 max 条，实际 {rows:?}");
    let kept: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(
        kept.contains(&"fresh"),
        "刚写入的那条必须留下，实际 {kept:?}"
    );
    assert!(
        kept.contains(&"old_hot"),
        "最热的那条也该留下，实际 {kept:?}"
    );
    assert!(
        !kept.contains(&"old_mid"),
        "被挤掉的应是「除新条目外最冷的」"
    );

    // 不传 keep 时行为不变（批量维护等调用方仍走纯 LFU）。
    s.record_completion(CompletionKind::UrlHistory, "fresh2")
        .unwrap();
    s.prune_completions(CompletionKind::UrlHistory, 2, None)
        .unwrap();
    let (rows2, _) = s
        .list_completions(CompletionKind::UrlHistory, "", 0, 0)
        .unwrap();
    assert!(
        !rows2.iter().any(|r| r.0 == "fresh2"),
        "keep=None 时新条目照旧垫底被裁，实际 {rows2:?}"
    );
}

#[test]
fn oversized_and_empty_text_are_ignored() {
    let s = store("guard");
    assert!(!s.record_completion(CompletionKind::UrlHistory, "").unwrap());
    let long = "x".repeat(COMPLETION_TEXT_MAX_CHARS + 1);
    assert!(
        !s.record_completion(CompletionKind::UrlHistory, &long)
            .unwrap()
    );
    assert_eq!(
        s.list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap()
            .1,
        0
    );

    // 恰好等于上限的仍收。
    let ok = "x".repeat(COMPLETION_TEXT_MAX_CHARS);
    assert!(
        s.record_completion(CompletionKind::UrlHistory, &ok)
            .unwrap()
    );
}

#[test]
fn jsonl_roundtrip_carries_both_kinds() {
    let a = store("exp");
    a.record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    a.record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    a.record_completion(CompletionKind::UrlHistory, "www.x.com")
        .unwrap();
    let dump = a.export_completions_jsonl().unwrap();
    assert_eq!(dump.lines().count(), 2, "两个 kind 各一条:\n{dump}");

    let b = store("imp");
    let (imported, skipped) = b.import_completions_jsonl(&dump).unwrap();
    assert_eq!((imported, skipped), (2, 0));
    assert_eq!(
        b.get_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap()
            .unwrap()
            .count,
        2,
        "count 应随导出导入保住"
    );
    assert_eq!(
        b.list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap()
            .1,
        1
    );
}

/// 每个 kind 都要能经导出 / 导入往返——遍历 `ALL` 而不是手写，新增 kind 自动纳入。
/// 曾经导出循环是手写的两项列表，新加一类不改它，那一类就静默不进备份。
#[test]
fn jsonl_roundtrip_covers_every_kind() {
    let a = store("exp_all");
    for kind in CompletionKind::ALL {
        a.record_completion(kind, "Same_Text").unwrap();
    }
    let dump = a.export_completions_jsonl().unwrap();
    let b = store("imp_all");
    b.import_completions_jsonl(&dump).unwrap();
    for kind in CompletionKind::ALL {
        assert!(
            b.get_completion(kind, "Same_Text").unwrap().is_some(),
            "{kind:?} 没随导出导入保住:\n{dump}"
        );
        assert_eq!(CompletionKind::from_key(kind.as_str()), Some(kind));
    }
}

#[test]
fn import_merges_by_max_and_skips_junk() {
    let s = store("merge");
    s.record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap(); // count=1

    let jsonl = concat!(
        r#"{"kind":"email_suffix","text":"qq.com","count":9,"last_used":100}"#,
        "\n",
        r#"not json"#,
        "\n",
        r#"{"kind":"no_such_kind","text":"x","count":1}"#,
        "\n",
        r#"{"kind":"email_suffix","count":1}"#,
        "\n",
    );
    let (imported, skipped) = s.import_completions_jsonl(jsonl).unwrap();
    assert_eq!(imported, 1);
    assert_eq!(skipped, 3, "坏行/未知 kind/缺 text 各跳一条");
    assert_eq!(
        s.get_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap()
            .unwrap()
            .count,
        9,
        "已存在的条目取 max(count)"
    );
}

#[test]
fn kind_str_roundtrip_is_closed() {
    for k in [CompletionKind::EmailSuffix, CompletionKind::UrlHistory] {
        assert_eq!(CompletionKind::from_key(k.as_str()), Some(k));
    }
    assert_eq!(CompletionKind::from_key("emial_suffix"), None);
}

/// 按键路径读写都只碰内存（redb 暂停照样补全、记录、裁剪）。
#[test]
fn completion_hot_path_does_not_touch_redb() {
    let s = store("hot");
    s.record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    s.pause().unwrap();
    assert!(
        s.record_completion(CompletionKind::EmailSuffix, "163.com")
            .unwrap()
    );
    assert_eq!(
        s.list_completions(CompletionKind::EmailSuffix, "", 0, 0)
            .unwrap()
            .1,
        2
    );
    assert_eq!(
        s.prune_completions(CompletionKind::EmailSuffix, 1, None)
            .unwrap(),
        1
    );
    s.resume().unwrap();
}

/// 不起落盘线程，释放 Store 时同步落盘，重开后记录在。
#[test]
fn completion_survives_drop_without_flusher() {
    let p = std::env::temp_dir().join(format!("wind_cmp_drop_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    {
        let s = Store::open(&p).unwrap();
        s.record_completion(CompletionKind::UrlHistory, "example.com")
            .unwrap();
        s.record_completion(CompletionKind::UrlHistory, "example.com")
            .unwrap();
    }
    let s = Store::open(&p).unwrap();
    let rec = s
        .get_completion(CompletionKind::UrlHistory, "example.com")
        .unwrap()
        .unwrap();
    assert_eq!(rec.count, 2);
    assert!(s.completion_path().is_file());
}

/// 审查 I-2：落盘（含 fsync）期间不持数据锁——按键路径的 list / record 不被一次写盘卡住。
#[test]
fn flush_does_not_block_readers() {
    let s = std::sync::Arc::new(store("flush_nonblock"));
    s.record_completion(CompletionKind::EmailSuffix, "qq.com")
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = std::sync::Mutex::new(rx);
    let flusher = std::sync::Arc::new(std::sync::Mutex::new(None));
    let f2 = flusher.clone();
    let hook = wind_userdata::text::fold::add_before_rename_hook(Box::new(move || {
        if Some(std::thread::current().id()) == *f2.lock().unwrap() {
            // 模拟一次很慢的 fsync：等主线程读完再放行（最多 2 秒）
            let _ = rx
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(2));
        }
    }));
    let s2 = s.clone();
    let fl = flusher.clone();
    let h = std::thread::spawn(move || {
        *fl.lock().unwrap() = Some(std::thread::current().id());
        s2.flush_small_files().unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    let t = std::time::Instant::now();
    let n = s
        .list_completions(CompletionKind::EmailSuffix, "", 0, 0)
        .unwrap()
        .1;
    let waited = t.elapsed();
    let _ = tx.send(());
    h.join().unwrap();
    drop(hook);
    assert_eq!(n, 1);
    assert!(
        waited < std::time::Duration::from_millis(500),
        "读等了 {waited:?}"
    );
}
