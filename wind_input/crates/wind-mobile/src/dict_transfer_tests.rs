use super::*;

fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
        && data_dir().join("schemas/pinyin.schema.toml").exists()
}

/// 带真词库的 [`MobileCore`]，当前方案五笔。⚠️ 目录名带 pid：多会话并行跑测试时固定名会互删夹具。
fn core(tag: &str) -> (MobileCore, std::path::PathBuf) {
    let user = std::env::temp_dir().join(format!("wind_mobile_dt_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = false;
    let (coord, _rx) =
        Coordinator::new_session(cfg, Some(&data_dir()), Some(&user), InputProfile::mobile());
    let core = MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: user.clone(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    (core, user)
}

/// 打一串码，返回全部候选文本，再 Esc 清掉组合。
fn type_code(m: &MobileCore, code: &str) -> Vec<String> {
    for ch in code.chars() {
        dispatch_key(&m.coord, ch.to_ascii_uppercase() as u32, 0);
    }
    let items = m.candidates(0, MAX_WINDOW).items;
    dispatch_key(&m.coord, 0x1B, 0);
    items
}

/// 加词 → 导出（头部带 engine_type、含该词）→ 清空 → 导入导出的文本 → 词回来且立即打得出。
#[test]
fn export_clear_import_round_trip_is_typable() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (m, dir) = core("rt");
    // 先打一遍把这个码的候选算进缓存——验的正是导入后缓存会不会挡住新词。
    assert!(
        !type_code(&m, "aaaa").iter().any(|t| t == "风起测词"),
        "前提：词库里没有"
    );
    m.user_word_add("wubi86", "aaaa", "风起测词").unwrap();

    let text = m.dict_export("wubi86").unwrap();
    assert!(
        text.contains("engine_type: codetable"),
        "头部要写引擎类型，导入端据此拒收跨类型文件:\n{text}"
    );
    assert!(text.contains("风起测词"), "导出要含刚加的词:\n{text}");

    assert_eq!(m.user_words_clear("wubi86"), Ok(1));
    assert!(!type_code(&m, "aaaa").iter().any(|t| t == "风起测词"));

    let r = m.dict_import("wubi86", &text, false).unwrap();
    let uw = r
        .sections
        .iter()
        .find(|s| s.key == "userWords")
        .expect("要有用户词段");
    assert_eq!((uw.added, uw.skipped, uw.rejected), (1, 0, 0), "{r:?}");
    assert_eq!(m.user_words("wubi86", "", 0, 50).total, 1);
    let after = type_code(&m, "aaaa");
    assert!(
        after.iter().any(|t| t == "风起测词"),
        "导完立即可打，实得 {after:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 五笔导出的文件导入拼音方案：按引擎类型拒收，且一条都不落库。
#[test]
fn codetable_file_rejected_by_pinyin_schema() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (m, dir) = core("cross");
    m.user_word_add("wubi86", "aaaa", "风起测词").unwrap();
    let text = m.dict_export("wubi86").unwrap();
    let err = m.dict_import("pinyin", &text, false).unwrap_err();
    assert!(err.contains("码表") && err.contains("拼音"), "{err}");
    assert_eq!(m.user_words("pinyin", "", 0, 50).total, 0, "拒收不落库");
    let _ = std::fs::remove_dir_all(dir);
}

/// 第三方纯文本：Rime（`---`/`...` 头，`词\t编码`）与 TSV（`编码\t词`）都只进用户词。
#[test]
fn rime_and_tsv_text_import_into_user_words() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let (m, dir) = core("rime");
    let rime = "---\nname: t\nversion: \"1\"\n...\n风起甲词\taaaa\t100\n风起乙词\taaab\n";
    let r = m.dict_import("wubi86", rime, false).unwrap();
    assert_eq!(r.sections.len(), 1, "{r:?}");
    assert_eq!(
        (r.sections[0].key.as_str(), r.sections[0].added),
        ("userWords", 2)
    );
    assert!(type_code(&m, "aaab").iter().any(|t| t == "风起乙词"));

    let r = m.dict_import("wubi86", "aaac\t风起丙词\n", false).unwrap();
    assert_eq!(r.sections[0].added, 1, "{r:?}");

    // replace：先清空再导入
    let r = m.dict_import("wubi86", "aaad\t风起丁词\n", true).unwrap();
    assert_eq!(r.sections[0].added, 1, "{r:?}");
    let page = m.user_words("wubi86", "", 0, 50);
    assert_eq!(
        page.items
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>(),
        ["风起丁词"],
        "replace 要先清空"
    );

    // 拼音方案：无空格的码过准入契约，由程序补上音节边界（显示码带空格即证）。
    let r = m
        .dict_import("pinyin", "fengqihaha\t风起哈哈\n", false)
        .unwrap();
    assert_eq!(
        (r.sections[0].added, r.sections[0].boundary_filled),
        (1, 1),
        "{r:?}"
    );
    let page = m.user_words("pinyin", "", 0, 50);
    assert_eq!(page.items[0].code, "feng qi ha ha");
    let _ = std::fs::remove_dir_all(dir);
}

/// 只读模式：导入带着原因拒绝（不碰数据）；导出读不到存储也报错而不是给空文件。
#[test]
fn readonly_rejects_import_with_reason() {
    let user = std::env::temp_dir().join(format!("wind_mobile_dt_ro_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.join("meta.toml"),
        "format_version = 99\ndir_id = \"x\"\n",
    )
    .unwrap();
    let (coord, _rx) =
        Coordinator::new_session(Config::default(), None, Some(&user), InputProfile::mobile());
    let m = MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: user.clone(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    let err = m.dict_import("pinyin", "aa\t工\n", false).unwrap_err();
    assert!(err.contains("99"), "要把只读原因交给用户: {err}");
    assert!(m.dict_export("pinyin").is_err());
    let _ = std::fs::remove_dir_all(&user);
}
