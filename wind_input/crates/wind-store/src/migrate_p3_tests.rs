use crate::Store;
use crate::shadow::{ShadowPin, ShadowRecord};

fn db(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wind_mp3_{name}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(crate::store::derived_state_dir(&p));
    let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
    p
}

fn remove_marker(p: &std::path::Path, m: &str) {
    let _ = std::fs::remove_file(crate::store::derived_state_dir(p).join(m));
}

fn pin(w: &str, id: Option<&str>, pos: usize) -> ShadowPin {
    ShadowPin {
        word: w.into(),
        cand_id: id.map(String::from),
        position: pos,
    }
}

/// 旧 SHADOW 表迁成文本：每方案一文件、规则与顺序原样；完成写标记；重跑幂等。
#[test]
fn shadow_table_migrates_to_text() {
    let p = db("shadow");
    {
        let s = Store::open(&p).unwrap();
        s.legacy_put_shadow(
            "wb",
            "aa",
            &ShadowRecord {
                pinned: vec![pin("工", None, 1), pin("恭", None, 0)],
                deleted: vec!["式".into()],
            },
        )
        .unwrap();
        s.legacy_put_shadow(
            "pinyin",
            "",
            &ShadowRecord {
                pinned: vec![pin("日期", Some("phrase:rq:{date}"), 2)],
                deleted: vec![],
            },
        )
        .unwrap();
        // 造库时迁移已跑过（旧表当时为空）：删掉它写出的空文件与标记，模拟升级前
        let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
    }
    remove_marker(&p, super::MIGRATED_P3_SHADOW);
    let s = Store::open(&p).unwrap();
    assert!(s.state_dir().join(super::MIGRATED_P3_SHADOW).is_file());
    let r = s.get_shadow_rules("wb", "aa").unwrap().unwrap();
    assert_eq!(
        r.pinned.iter().map(|x| x.word.as_str()).collect::<Vec<_>>(),
        vec!["工", "恭"]
    );
    assert_eq!(r.deleted, vec!["式".to_string()]);
    assert_eq!(
        s.get_shadow_rules("pinyin", "").unwrap().unwrap().pinned[0].position,
        2
    );
    drop(s);
    remove_marker(&p, super::MIGRATED_P3_SHADOW);
    let s = Store::open(&p).unwrap();
    assert_eq!(
        s.get_shadow_rules("wb", "aa")
            .unwrap()
            .unwrap()
            .pinned
            .len(),
        2,
        "重跑不重复"
    );
}

/// 旧 PHRASES 表迁成文本：用户行原样；与 TOML 相同的系统行不写；被改过的系统行写成修改行。
/// 没有 TOML（`set_system_phrases` 之前）不迁。
#[test]
fn phrases_table_migrates_with_toml_knowledge() {
    let p = db("phrases");
    {
        let s = Store::open(&p).unwrap();
        s.legacy_put_phrase("yx", "me@x.com", 1800, 0, true, false, false, "")
            .unwrap();
        s.legacy_put_phrase("rq", "{date}", 100, 0, true, true, false, "")
            .unwrap();
        s.legacy_put_phrase("sj", "{time}", 100, 0, false, true, false, "")
            .unwrap();
    }
    remove_marker(&p, super::MIGRATED_P3_PHRASES);
    let s = Store::open(&p).unwrap();
    assert!(!s.state_dir().join(super::MIGRATED_P3_PHRASES).is_file());
    let sys = |c: &str, t: &str| crate::phrases::SystemPhrase {
        code: c.into(),
        text: t.into(),
        weight: 100,
        position: 0,
        category: String::new(),
        enabled: true,
    };
    s.set_system_phrases(&[sys("rq", "{date}"), sys("sj", "{time}")])
        .unwrap();
    assert!(s.state_dir().join(super::MIGRATED_P3_PHRASES).is_file());
    assert!(s.migrated_p3());
    let f = std::fs::read_to_string(s.phrases_path()).unwrap();
    assert!(f.contains("yx\tme@x.com"), "{f}");
    assert!(!f.contains("rq\t"), "{f}");
    assert!(f.contains("sj\t{time}\t100\t0\t0\t\t1"), "{f}");
}
