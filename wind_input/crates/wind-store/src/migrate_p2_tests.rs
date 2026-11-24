use crate::Store;

fn db(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wind_mp2_{name}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(crate::store::derived_state_dir(&p));
    let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
    p
}

/// 造一个「旧版」库：用户词只在 USER_WORDS 里、没有迁移标记。
fn legacy(name: &str, rows: &[(&str, &str, &str, i32, u64, u32)]) -> std::path::PathBuf {
    let p = db(name);
    {
        let s = Store::open(&p).unwrap();
        for (schema, code, text, w, b, o) in rows {
            s.legacy_put_user(schema, code, text, *w, *b, *o).unwrap();
        }
    }
    let _ = std::fs::remove_file(crate::store::derived_state_dir(&p).join(super::MIGRATED_P2));
    p
}

/// 词、权重、边界、同码先后、特殊文本都原样迁过去。
#[test]
fn migration_preserves_words_order_and_boundary() {
    let p = legacy(
        "basic",
        &[
            ("pinyin", "ni", "泥", 1, 0b1, 3),
            ("pinyin", "ni", "你", 1, 0b1, 2),
            ("pinyin", "nihao", "你好", 9, 0b101, 1),
            ("pinyin", "xianning", "西安宁", 5, 0, 0),
            ("pinyin", "tag", "#话题", 5, 0, 4),
            ("wb", "aaaa", "工\t具\n行", 7, 0, 1),
        ],
    );
    let s = Store::open(&p).unwrap();
    assert!(s.migrated_p2());
    let f = std::fs::read_to_string(s.user_text_path("pinyin")).unwrap();
    let body: Vec<&str> = f
        .lines()
        .skip_while(|l| *l != "# no comment")
        .skip(1)
        .collect();
    assert_eq!(
        body,
        vec![
            "xianning\t西安宁\t5",
            "ni hao\t你好\t9",
            "ni\t你\t1",
            "ni\t泥\t1",
            "tag\t#话题\t5",
        ],
        "order 0 的旧记录在前（同旧导出口径）"
    );
    assert_eq!(
        s.get_user_words("wb", "aaaa").unwrap()[0].text,
        "工\t具\n行"
    );
    let mut ni = s.get_user_words("pinyin", "ni").unwrap();
    ni.sort_by_key(|r| r.order);
    assert_eq!(
        ni.iter()
            .map(|r| (r.text.as_str(), r.boundary))
            .collect::<Vec<_>>(),
        vec![("你", 0b1), ("泥", 0b1)]
    );
    assert_eq!(s.get_user_words("pinyin", "tag").unwrap()[0].text, "#话题");
    assert_eq!(
        s.get_user_words("pinyin", "nihao").unwrap()[0].boundary,
        0b101
    );
}

/// 标记写入前崩溃，重跑合并：不重复、不丢、不覆盖两次启动之间新加 / 改过的词。
#[test]
fn rerun_after_crash_merges_without_loss() {
    let p = legacy(
        "rerun",
        &[("wb", "a", "工", 1, 0, 1), ("wb", "b", "了", 1, 0, 2)],
    );
    {
        let s = Store::open(&p).unwrap();
        s.add_user_word("wb", "c", "以", 3, 0).unwrap();
        s.update_user_word_weight("wb", "a", "工", 50).unwrap();
    }
    let _ = std::fs::remove_file(crate::store::derived_state_dir(&p).join(super::MIGRATED_P2));
    let s = Store::open(&p).unwrap();
    assert!(s.migrated_p2());
    let mut all: Vec<(String, i32)> = s
        .search_user_words_prefix("wb", "", 0)
        .unwrap()
        .into_iter()
        .map(|r| (r.text, r.weight))
        .collect();
    all.sort();
    assert_eq!(
        all,
        vec![("了".into(), 1), ("以".into(), 3), ("工".into(), 50)]
    );
    s.runtime()
        .data()
        .fold(wind_userdata::record::Target::User, "wb")
        .unwrap();
    let f = std::fs::read_to_string(s.user_text_path("wb")).unwrap();
    assert_eq!(f.matches("a\t工\t").count(), 1, "不重复成行");
}

/// 没有旧用户词：只写标记，不建空文件。
#[test]
fn empty_legacy_writes_marker_only() {
    let p = legacy("empty", &[]);
    let s = Store::open(&p).unwrap();
    assert!(s.migrated_p2());
    assert!(!s.user_text_path("pinyin").exists());
}
