//! 按一组键写的批量函数：结果与逐条调用相同（含同批重复键），且整批只提交一次。
//! 设置页「多选后批量操作」经这些函数一次写完（RPC 侧用例在 wind-webdata 的 `batch_tests`）。

use wind_store::Store;

fn store(tag: &str) -> Store {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    // 每个库独占一个目录：文本词库文件落在库文件旁，共用 temp 根会让并行用例互相读到对方的词
    let dir = std::env::temp_dir().join(format!(
        "wind_batch_{tag}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    Store::open(dir.join("user.redb")).expect("开库")
}

fn keys(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn words(s: &Store) -> Vec<(String, String, i32, u64)> {
    s.search_user_words_prefix("wb", "", 0)
        .unwrap()
        .into_iter()
        .map(|r| (r.code, r.text, r.weight, r.boundary))
        .collect()
}

/// 本线程在 `f` 里提交了几次文本文件。
fn commits_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let me = std::thread::current().id();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let _g = wind_userdata::text::fold::add_before_rename_hook(Box::new(move || {
        if std::thread::current().id() == me {
            n2.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let r = f();
    (r, n.load(Ordering::SeqCst))
}

/// 同批重复的键按逐条加词合并：权重取 max、边界取第一个非 0 的。
#[test]
fn add_user_words_matches_sequential_adds() {
    let rows: Vec<(String, String, i32, u64)> = [
        ("a", "工", 100, 0),
        ("b", "了", 5, 0),
        ("b", "了", 9, 0b1),
        ("b", "了", 3, 0b10),
        ("c", "以", 1, 0),
    ]
    .iter()
    .map(|(c, t, w, b)| (c.to_string(), t.to_string(), *w, *b))
    .collect();
    let seq = store("uw_seq");
    seq.add_user_word("wb", "a", "工", 200, 0).unwrap();
    for (c, t, w, b) in &rows {
        seq.add_user_word("wb", c, t, *w, *b).unwrap();
    }
    let bat = store("uw_bat");
    bat.add_user_word("wb", "a", "工", 200, 0).unwrap();
    let g0 = bat.words_generation_of("wb");
    bat.add_user_words("wb", &rows).unwrap();
    assert_eq!(words(&bat), words(&seq));
    assert_ne!(bat.words_generation_of("wb"), g0, "要推进结构代次");

    bat.remove_user_words("wb", &keys(&[("a", "工"), ("zz", "无"), ("b", "了")]))
        .unwrap();
    for (c, t) in [("a", "工"), ("zz", "无"), ("b", "了")] {
        seq.remove_user_word("wb", c, t).unwrap();
    }
    assert_eq!(words(&bat), words(&seq));
    assert_eq!(words(&bat).len(), 1);
}

/// 有一条过长：整批不写。
#[test]
fn add_user_words_rejects_whole_batch() {
    let s = store("uw_long");
    let long = "长".repeat(wind_userdata::runtime::USER_TEXT_MAX_CHARS + 1);
    let rows = vec![
        ("a".to_string(), "工".to_string(), 1, 0),
        ("b".to_string(), long, 1, 0),
    ];
    assert!(s.add_user_words("wb", &rows).is_err());
    assert!(words(&s).is_empty());
}

/// 转正：用户词与临时词在同一批里处理；不存在的跳过。
///
/// 同一个库先逐条、清空后再批量。样本不带边界：单音节码的边界文本文件表达不了，
/// 后台回读会把它冲成 0，带上它这条用例就是随机红。
#[test]
fn promote_temp_words_matches_sequential() {
    let s = store("pr");
    let learn = |s: &Store| {
        for (c, t) in [("gg", "王"), ("hh", "目"), ("jj", "日")] {
            s.learn_temp_word("wb", c, t, 100, 0).unwrap();
        }
    };
    let temps = |s: &Store| s.search_temp_words_prefix("wb", "", 0).unwrap().len();
    learn(&s);
    for (c, t) in [("gg", "王"), ("kk", "无"), ("jj", "日")] {
        s.promote_temp_word("wb", c, t).unwrap();
    }
    let (seq_words, seq_temps) = (words(&s), temps(&s));
    s.remove_user_words("wb", &keys(&[("gg", "王"), ("jj", "日")]))
        .unwrap();
    s.remove_temp_words("wb", &keys(&[("hh", "目")])).unwrap();
    learn(&s);
    let promoted = s
        .promote_temp_words(
            "wb",
            &keys(&[("gg", "王"), ("kk", "无"), ("jj", "日"), ("gg", "王")]),
        )
        .unwrap();
    assert_eq!(promoted, keys(&[("gg", "王"), ("jj", "日")]));
    assert_eq!(words(&s), seq_words);
    assert_eq!(temps(&s), 1);
    assert_eq!(temps(&s), seq_temps);
}

#[test]
fn remove_temp_words_and_delete_freqs() {
    let s = store("tf");
    for (c, t) in [("gg", "王"), ("hh", "目")] {
        s.learn_temp_word("wb", c, t, 100, 0).unwrap();
        s.record_freq("wb", c, t).unwrap();
    }
    s.remove_temp_words("wb", &keys(&[("gg", "王"), ("zz", "无")]))
        .unwrap();
    s.delete_freqs("wb", &keys(&[("hh", "目"), ("hh", "目")]))
        .unwrap();
    assert_eq!(s.search_temp_words_prefix("wb", "", 0).unwrap().len(), 1);
    assert!(s.get_freq("wb", "hh", "目").unwrap().is_none());
    assert!(s.get_freq("wb", "gg", "王").unwrap().is_some());
}

/// 短语：叠加视图算一次、文件提交一次；同批重复的键只写一行。
#[test]
fn phrase_batches_commit_once() {
    let s = store("ph");
    for (c, t) in [("aa", "甲"), ("bb", "乙"), ("cc", "丙")] {
        s.add_phrase(c, t, 0, 1).unwrap();
    }
    let (r, n) = commits_during(|| {
        s.set_phrases_enabled(
            &keys(&[("aa", "甲"), ("bb", "乙"), ("aa", "甲"), ("dd", "新")]),
            false,
        )
    });
    r.unwrap();
    assert_eq!(n, 1);
    let list = s.list_phrases().unwrap();
    assert_eq!(list.len(), 4, "{list:?}");
    assert_eq!(list.iter().filter(|p| !p.enabled).count(), 3, "{list:?}");

    let (r, n) = commits_during(|| s.remove_phrases(&keys(&[("aa", "甲"), ("dd", "新")])));
    r.unwrap();
    assert_eq!(n, 1);
    let left: Vec<_> = s
        .list_phrases()
        .unwrap()
        .into_iter()
        .map(|p| p.code)
        .collect();
    assert_eq!(left, ["bb", "cc"]);
}

/// Shadow：同编码多条规则合到一组改写，一次提交。
#[test]
fn shadow_rules_batch_groups_by_code() {
    let s = store("sh");
    s.pin_shadow("wb", "aa", "甲", None, 0).unwrap();
    s.pin_shadow("wb", "aa", "今日", Some("phrase:date"), 1)
        .unwrap();
    s.delete_shadow("wb", "aa", "乙").unwrap();
    s.pin_shadow("wb", "bb", "丙", None, 0).unwrap();
    let rules = vec![
        ("aa".to_string(), "甲".to_string(), None),
        ("bb".to_string(), "丙".to_string(), None),
        (
            "aa".to_string(),
            "别的文本".to_string(),
            Some("phrase:date".to_string()),
        ),
    ];
    let (r, n) = commits_during(|| s.remove_shadow_rules("wb", &rules));
    r.unwrap();
    assert_eq!(n, 1);
    let left = s.list_shadow_rules("wb").unwrap();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].0, "aa");
    assert!(left[0].1.pinned.is_empty());
    assert_eq!(left[0].1.deleted, ["乙"]);
}

/// 快捷格式：跨类别一次读改写；删除里有一条出厂条目 ⇒ 整批不写。
#[test]
fn quick_format_batches_are_all_or_nothing() {
    let s = store("qf");
    s.add_quick_format("date", "date.u1", "$Y").unwrap();
    s.add_quick_format("date", "date.u2", "$M").unwrap();
    let items = keys(&[("date", "date.basic"), ("number", "number.amount")]);
    let (r, n) = commits_during(|| {
        s.edit_quick_formats(&items, &mut |rec, id| {
            rec.set_enabled(id, false);
            Ok(())
        })
    });
    r.unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        s.get_quick_format("number").unwrap().unwrap().disabled,
        ["number.amount"]
    );

    let before = s.list_quick_format().unwrap();
    let r = s.delete_quick_formats(&keys(&[("date", "date.u1"), ("date", "date.basic")]));
    assert!(r.is_err());
    assert_eq!(
        format!("{:?}", s.list_quick_format().unwrap()),
        format!("{before:?}")
    );
    s.delete_quick_formats(&keys(&[("date", "date.u1"), ("date", "date.u2")]))
        .unwrap();
    assert!(
        s.get_quick_format("date")
            .unwrap()
            .unwrap()
            .added
            .is_empty()
    );
}

#[test]
fn remove_completions_counts_existing() {
    use wind_store::completion::CompletionKind;
    let s = store("cp");
    for t in ["a.com", "b.com"] {
        s.record_completion(CompletionKind::EmailSuffix, t).unwrap();
    }
    let n = s
        .remove_completions(
            CompletionKind::EmailSuffix,
            &["a.com".into(), "zz".into(), "a.com".into()],
        )
        .unwrap();
    assert_eq!(n, 1);
    let (left, _) = s
        .list_completions(CompletionKind::EmailSuffix, "", 0, 0)
        .unwrap();
    assert_eq!(left.len(), 1);
}
