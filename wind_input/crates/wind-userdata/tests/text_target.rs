//! 文本目标（设计 §3.5 / §11 文本层）。
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wind_userdata::engine::{Dirs, TextBinding, UserData};
use wind_userdata::record::{OpKind, Payload, Target};

fn setup(name: &str) -> (PathBuf, Arc<UserData>, PathBuf) {
    let d = std::env::temp_dir().join(format!("wind_txt_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let ud = Arc::new(
        UserData::open(Dirs {
            state_dir: d.join("state"),
            dir_id: "dir".into(),
        })
        .unwrap(),
    );
    let file = d.join("user").join("py").join("user.dict.yaml");
    (d, ud, file)
}

fn bind(ud: &UserData, file: &Path) {
    ud.bind_text(
        Target::User,
        "py",
        TextBinding {
            path: file.to_path_buf(),
            name: "pinyin.user".into(),
            lowercase: false,
            syllable_codes: true,
            format: wind_userdata::engine::TextFormat::Words,
        },
    )
    .unwrap();
}

fn up(ud: &UserData, code: &str, text: &str, w: i32, b: u64) {
    ud.apply(
        Target::User,
        "py",
        OpKind::Upsert,
        code,
        text,
        Payload::Weight(w),
        b,
    )
    .unwrap();
}

fn texts(ud: &UserData, code: &str) -> Vec<(String, i32)> {
    ud.query_exact(Target::User, "py", code)
}

#[test]
fn fold_writes_text_and_reads_back_after_reopen() {
    let (d, ud, file) = setup("fold");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.fold(Target::User, "py").unwrap();
    let s = std::fs::read_to_string(&file).unwrap();
    assert!(s.contains("ni hao\t你好\t9"), "{s}");
    assert_eq!(ud.pending(Target::User, "py"), 0);
    drop(ud);
    let ud = UserData::open(Dirs {
        state_dir: d.join("state"),
        dir_id: "dir".into(),
    })
    .unwrap();
    bind(&ud, &file);
    assert_eq!(texts(&ud, "nihao"), vec![("你好".into(), 9)]);
}

/// 手改文件 ⇒ sync_text 由文本重建，已折回的记录不回放（删掉的行不复活）；未折回的操作仍叠加。
#[test]
fn hand_edit_wins_and_pending_ops_still_overlay() {
    let (_d, ud, file) = setup("handedit");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    up(&ud, "xian", "先", 1, 0b1);
    ud.fold(Target::User, "py").unwrap();
    let s = std::fs::read_to_string(&file)
        .unwrap()
        .replace("xian\t先\t1\n", "");
    std::fs::write(&file, s).unwrap();
    up(&ud, "hao", "好", 2, 0b1);
    assert!(ud.sync_text(Target::User, "py").unwrap(), "指纹变了要重建");
    assert!(texts(&ud, "xian").is_empty(), "手删的行不复活");
    assert_eq!(
        texts(&ud, "hao"),
        vec![("好".into(), 2)],
        "未折回的操作仍叠加"
    );
    assert!(!ud.sync_text(Target::User, "py").unwrap(), "没变就不重建");
}

/// Review Focus 3：折回期间文件被保存 ⇒ 本轮放弃，手改与运行时写入都不丢。
#[test]
fn editor_save_during_fold_discards_the_round() {
    let (_d, ud, file) = setup("race");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.fold(Target::User, "py").unwrap();
    up(&ud, "hao", "好", 2, 0b1);
    let me = std::thread::current().id();
    let hook = wind_userdata::text::fold::add_before_rename_hook(Box::new({
        let file = file.clone();
        move || {
            if std::thread::current().id() != me {
                return;
            }
            let s = std::fs::read_to_string(&file).unwrap();
            std::fs::write(&file, format!("{s}shou gai\t手改\t1\n")).unwrap();
        }
    }));
    assert!(ud.fold(Target::User, "py").is_err());
    drop(hook);
    assert_eq!(ud.pending(Target::User, "py"), 1, "delta 保留");
    assert!(
        std::fs::read_to_string(&file).unwrap().contains("手改"),
        "手改原样"
    );
    ud.sync_text(Target::User, "py").unwrap();
    ud.fold(Target::User, "py").unwrap();
    let s = std::fs::read_to_string(&file).unwrap();
    assert!(s.contains("手改") && s.contains("hao\t好\t2"), "{s}");
}

/// Review Focus 4：被删除 vs 读不了。
#[test]
fn deleted_file_vs_unreadable_file() {
    let (_d, ud, file) = setup("missing");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.fold(Target::User, "py").unwrap();
    std::fs::remove_file(&file).unwrap();
    assert!(ud.sync_text(Target::User, "py").unwrap());
    assert!(texts(&ud, "nihao").is_empty(), "文件被删 = 用户明确删除");
    up(&ud, "hao", "好", 2, 0b1);
    ud.fold(Target::User, "py").unwrap();
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("hao\t好\t2"),
        "之后的写入新建文件"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        up(&ud, "ni", "你", 1, 0b1);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = ud.fold(Target::User, "py");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        if r.is_err() {
            assert_eq!(
                ud.pending(Target::User, "py"),
                1,
                "读不了 ⇒ 本轮跳过、delta 保留"
            );
        } // root 下权限不生效时 fold 会成功，此分支不做断言
    }
}

#[test]
fn rewrite_replaces_whole_file_and_advances_watermark() {
    let (_d, ud, file) = setup("rewrite");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.rewrite_text(Target::User, "py", &[("hao".into(), "好".into(), 0b1, 3)])
        .unwrap();
    assert!(
        texts(&ud, "nihao").is_empty(),
        "rows 就是新全集：先折回待处理操作，再整文件替换"
    );
    assert_eq!(texts(&ud, "hao"), vec![("好".into(), 3)]);
    assert!(
        ud.fold_with(Target::User, "py", &mut |v| v).is_err(),
        "已绑定文本的目标不走 fold_with"
    );
}

#[test]
fn text_targets_fold_only_after_idle() {
    let (_d, ud, file) = setup("idle");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    assert_eq!(
        ud.maintain_text_idle(std::time::Duration::from_secs(3600))
            .unwrap(),
        0,
        "未空闲不折回"
    );
    assert_eq!(ud.maintain_text_idle(std::time::Duration::ZERO).unwrap(), 1);
    assert!(file.is_file());
}

fn wdats(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d.join("state/py"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".wdat"))
        .collect();
    v.sort();
    v
}

/// 文本以 `#` 开头的词：写回后不能被当成注释读丢。
#[test]
fn hash_leading_text_survives_fold() {
    let (_d, ud, file) = setup("hash");
    bind(&ud, &file);
    up(&ud, "tag", "#话题", 5, 0);
    ud.fold(Target::User, "py").unwrap();
    assert_eq!(texts(&ud, "tag"), vec![("#话题".into(), 5)]);
    // 没有 `# no comment` 的手写旧文件，追加时也要补上
    std::fs::write(
        &file,
        "---\nname: pinyin.user\ncolumns: [text, code, weight]\n...\n# 注释\n好\thao\t1\n",
    )
    .unwrap();
    ud.sync_text(Target::User, "py").unwrap();
    up(&ud, "include", "#include", 3, 0);
    ud.fold(Target::User, "py").unwrap();
    assert_eq!(texts(&ud, "include"), vec![("#include".into(), 3)]);
    assert_eq!(texts(&ud, "hao"), vec![("好".into(), 1)]);
    let s = std::fs::read_to_string(&file).unwrap();
    assert!(s.contains("...\n# 注释\n"), "原有注释仍是注释：{s}");
}

/// 重开后、绑定之前的折回 / 压实不能绕过文本把操作吃掉。
#[test]
fn fold_or_compact_before_bind_keeps_ops_for_text() {
    let (d, ud, file) = setup("prebind");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.fold(Target::User, "py").unwrap();
    drop(ud);
    let ud = Arc::new(
        UserData::open(Dirs {
            state_dir: d.join("state"),
            dir_id: "dir".into(),
        })
        .unwrap(),
    );
    up(&ud, "hao", "好", 2, 0b1);
    assert!(
        ud.fold(Target::User, "py").is_err(),
        "归文本管的目标未绑定时不折回"
    );
    ud.compact().unwrap();
    assert!(
        !ud.needs_maintain_with(1, u64::MAX),
        "未绑定的文本目标不催折回"
    );
    bind(&ud, &file);
    assert_eq!(texts(&ud, "hao"), vec![("好".into(), 2)]);
    ud.fold(Target::User, "py").unwrap();
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("hao\t好\t2")
    );
}

/// 只读实例绑定文本不写快照、不写 state。
#[test]
fn readonly_instance_binds_without_writing() {
    let (d, ud, file) = setup("ro");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.fold(Target::User, "py").unwrap();
    let mut s = std::fs::read_to_string(&file).unwrap();
    s.push_str("shou gai\t手改\t1\n");
    std::fs::write(&file, s).unwrap();
    let state_before = std::fs::read(d.join("state/journal/state")).unwrap();
    let w_before = wdats(&d);
    let ro = UserData::open(Dirs {
        state_dir: d.join("state"),
        dir_id: "dir".into(),
    })
    .unwrap();
    assert!(ro.readonly());
    bind(&ro, &file);
    assert!(ro.sync_text(Target::User, "py").is_err());
    assert_eq!(
        std::fs::read(d.join("state/journal/state")).unwrap(),
        state_before
    );
    assert_eq!(wdats(&d), w_before);
}

/// 一个方案的文本坏了不拖住别的方案；失败后退避，不每轮都催。
#[test]
fn isolated_text_does_not_block_other_targets() {
    let (d, ud, file) = setup("isolate");
    bind(&ud, &file);
    let file2 = d.join("user").join("wb").join("user.dict.yaml");
    ud.bind_text(
        Target::User,
        "wb",
        TextBinding {
            path: file2.clone(),
            name: "wb.user".into(),
            lowercase: false,
            syllable_codes: false,
            format: wind_userdata::engine::TextFormat::Words,
        },
    )
    .unwrap();
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.fold(Target::User, "py").unwrap();
    std::fs::write(&file, "---\nname: [\n...\n").unwrap();
    up(&ud, "hao", "好", 2, 0b1);
    ud.apply(
        Target::User,
        "wb",
        OpKind::Upsert,
        "a",
        "工",
        Payload::Weight(1),
        0,
    )
    .unwrap();
    assert_eq!(ud.maintain_text_idle(std::time::Duration::ZERO).unwrap(), 1);
    assert!(file2.is_file(), "wb 照常折回");
    assert_eq!(ud.pending(Target::User, "py"), 1, "py 的操作留在 delta");
    assert!(
        !ud.needs_maintain_with(usize::MAX, u64::MAX),
        "失败的目标退避期内不再催"
    );
}

/// 只有计数 / 时间的折回：不写文件，也不整段重建。
#[test]
fn incr_only_fold_does_not_rebuild_segment() {
    let (d, ud, file) = setup("incr");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.fold(Target::User, "py").unwrap();
    let g1 = wdats(&d);
    ud.apply(
        Target::User,
        "py",
        OpKind::Incr,
        "nihao",
        "你好",
        Payload::Count(1),
        0b101,
    )
    .unwrap();
    ud.fold(Target::User, "py").unwrap();
    assert_eq!(ud.pending(Target::User, "py"), 0);
    assert_eq!(wdats(&d), g1);
}

/// 后台节拍：没有新写入也会把空闲够久的文本目标折回；引擎释放后线程自行退出。
#[test]
fn ticker_folds_idle_text_without_further_writes() {
    let (_d, ud, file) = setup("ticker");
    bind(&ud, &file);
    up(&ud, "nihao", "你好", 9, 0b101);
    ud.set_text_idle_for_test(std::time::Duration::ZERO);
    let h = ud
        .spawn_ticker(std::time::Duration::from_millis(20))
        .unwrap();
    let t0 = std::time::Instant::now();
    while !file.is_file() && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(file.is_file(), "节拍应把空闲的文本目标折回");
    drop(ud);
    let t0 = std::time::Instant::now();
    while !h.is_finished() && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(h.is_finished(), "引擎释放后节拍线程退出");
}

/// 审查 M4：绑定一个从没用过的方案（文件不在、没有段）不建空快照目录。
#[test]
fn binding_an_unused_schema_creates_nothing() {
    let (d, ud, _file) = setup("unused");
    ud.bind_text(
        Target::User,
        "never",
        TextBinding {
            path: d.join("user").join("never").join("user.dict.yaml"),
            name: "never.user".into(),
            lowercase: false,
            syllable_codes: false,
            format: wind_userdata::engine::TextFormat::Words,
        },
    )
    .unwrap();
    assert!(!d.join("state").join("never").exists());
}

/// §2.5 行级：坏行跳过并登记到问题表；修好后清掉。
#[test]
fn text_bad_lines_are_reported_to_issues() {
    let (d, ud, file) = setup("issues");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "---\ncolumns: [text, code, weight]\n...\n好\thao\t1\n坏\n",
    )
    .unwrap();
    bind(&ud, &file);
    assert_eq!(
        wind_userdata::issues::list_under(&d),
        vec![wind_userdata::issues::FileIssue {
            path: file.clone(),
            kind: wind_userdata::issues::IssueKind::BadLines(1)
        }]
    );
    std::fs::write(
        &file,
        "---\ncolumns: [text, code, weight]\n...\n好\thao\t1\n",
    )
    .unwrap();
    ud.sync_text(Target::User, "py").unwrap();
    assert!(wind_userdata::issues::list_under(&d).is_empty(), "修好就清");
    std::fs::write(&file, b"\xff\xfe").unwrap();
    assert!(ud.sync_text(Target::User, "py").is_err());
    assert_eq!(
        wind_userdata::issues::list_under(&d)[0].kind,
        wind_userdata::issues::IssueKind::Isolated
    );
    std::fs::remove_file(&file).unwrap();
    ud.sync_text(Target::User, "py").unwrap();
    assert!(
        wind_userdata::issues::list_under(&d).is_empty(),
        "删掉文件也清"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// 外部编辑会话：暂停写回的目标不被空闲折回与压实写回；查询照常叠加 delta；释放即折回。
#[test]
fn held_text_target_is_not_folded_until_released() {
    let (_d, ud, file) = setup("held");
    bind(&ud, &file);
    ud.hold_text(Target::User, "py");
    assert!(ud.is_held(Target::User, "py"));
    up(&ud, "nihao", "你好", 9, 0b101);
    assert_eq!(ud.maintain_text_idle(std::time::Duration::ZERO).unwrap(), 0);
    assert!(!ud.needs_maintain(), "暂停中的目标不该触发维护");
    ud.compact().unwrap();
    assert!(!file.exists(), "暂停中不写文件");
    assert_eq!(ud.pending(Target::User, "py"), 1);
    assert_eq!(texts(&ud, "nihao"), vec![("你好".into(), 9)]);
    assert_eq!(ud.held_texts(), vec![(Target::User, "py".to_string())]);
    ud.release_text(Target::User, "py");
    assert!(!ud.is_held(Target::User, "py"));
    assert_eq!(ud.pending(Target::User, "py"), 0, "释放即折回");
    let s = std::fs::read_to_string(&file).unwrap();
    assert!(s.contains("ni hao\t你好\t9"), "{s}");
}
