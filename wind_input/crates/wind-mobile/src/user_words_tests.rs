use super::*;

/// 直接拼 [`MobileCore`]：`MobileCore::new` 要挂环境变量与出厂目录，测这几个口用不着。
/// ⚠️ 目录名带 pid：多会话并行跑测试时固定名会互删夹具。
fn core(tag: &str, meta: Option<&str>) -> (MobileCore, std::path::PathBuf) {
    let user = std::env::temp_dir().join(format!("wind_mobile_uw_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    if let Some(m) = meta {
        std::fs::write(user.join("meta.toml"), m).unwrap();
    }
    let (coord, _rx) =
        Coordinator::new_session(Config::default(), None, Some(&user), InputProfile::mobile());
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

#[test]
fn add_list_search_remove_clear() {
    let (m, dir) = core("crud", None);
    m.user_word_add("pinyin", "ni hao", "你好").unwrap();
    m.user_word_add("pinyin", "wo men", "我们").unwrap();

    let page = m.user_words("pinyin", "", 0, 50);
    assert_eq!(page.total, 2);
    assert_eq!(
        page.items[0],
        UserWord {
            code: "ni hao".into(),
            text: "你好".into(),
            weight: USER_WORD_ADD_WEIGHT,
        },
        "显示码带音节空格、权重取桌面加词默认值"
    );

    let hits = m.user_words("pinyin", "们", 0, 50);
    assert_eq!((hits.total, hits.items[0].text.as_str()), (1, "我们"));

    // 列表给的显示码原样回传即可删掉
    m.user_word_remove("pinyin", &page.items[0].code, "你好")
        .unwrap();
    assert_eq!(m.user_words("pinyin", "", 0, 50).total, 1);

    assert_eq!(m.user_words_clear("pinyin"), Ok(1));
    assert_eq!(m.user_words("pinyin", "", 0, 50), UserWordPage::default());
    let _ = std::fs::remove_dir_all(dir);
}

/// 预览走候选同一条求值：日期模板按此刻展开、组展开成各项、纯文本原样。
#[test]
fn phrase_preview_expands_like_a_candidate() {
    use chrono::TimeZone;
    let now = chrono::Local
        .with_ymd_and_hms(2026, 10, 21, 9, 5, 0)
        .unwrap();
    assert_eq!(
        phrase_preview("date", "$Y年$M月$D日", now),
        "2026年10月21日"
    );
    assert_eq!(phrase_preview("dz", "北京市朝阳区", now), "北京市朝阳区");
    assert_eq!(
        phrase_preview("zzhf", r#"$SS("常用回复", "好的", "收到")"#, now),
        "好的 · 收到",
        "组打全码时展开成各项"
    );
    assert_eq!(
        phrase_preview("x", r#"$SS("g", "1", "2", "3", "4", "5")"#, now),
        "1 · 2 · 3 · 4 · …",
        "多了只列前几个"
    );
}

/// 命令短语：加 / 列 / 开关 / 删用户短语；只读时拒写。系统短语的叠加与出厂开关语义在
/// wind-store 的测试里验，这里只验门面这一层。
#[test]
fn phrase_add_list_toggle_remove() {
    let (m, dir) = core("phrases", None);
    m.phrase_add(" dz ", " 北京市朝阳区 ").unwrap();
    let mine = |m: &MobileCore| {
        m.phrases()
            .into_iter()
            .filter(|p| p.code == "dz")
            .collect::<Vec<_>>()
    };
    assert_eq!(
        mine(&m),
        [Phrase {
            code: "dz".into(),
            text: "北京市朝阳区".into(),
            enabled: true,
            is_system: false,
            preview: "北京市朝阳区".into(),
        }],
        "编码与内容去掉首尾空白、默认启用、归属用户"
    );
    m.phrase_set_enabled("dz", "北京市朝阳区", false).unwrap();
    assert!(!mine(&m)[0].enabled);
    m.phrase_remove("dz", "北京市朝阳区").unwrap();
    assert!(mine(&m).is_empty());
    assert!(m.phrase_add("dz", "  ").is_err(), "空内容拒收");
    let _ = std::fs::remove_dir_all(dir);
}

/// 暂停学习只是转发：开关落到协调器上（学习路径的覆盖在 wind-coordinator 的
/// `tests/learning_suspended.rs` 验）。
#[test]
fn learning_suspended_forwards_to_coordinator() {
    let (m, dir) = core("suspend", None);
    assert!(!m.coord.learning_suspended(), "默认不暂停");
    m.set_learning_suspended(true);
    assert!(m.coord.learning_suspended());
    m.set_learning_suspended(false);
    assert!(!m.coord.learning_suspended());
    let _ = std::fs::remove_dir_all(dir);
}

/// 只读模式：写口带着原因拒绝；读口照约定返回空页（不 panic）。
#[test]
fn readonly_rejects_writes_with_reason() {
    let (m, dir) = core("ro", Some("format_version = 99\ndir_id = \"x\"\n"));
    let err = m.user_word_add("pinyin", "ni hao", "你好").unwrap_err();
    assert!(err.contains("99"), "要把只读原因交给用户: {err}");
    assert!(m.user_word_remove("pinyin", "ni hao", "你好").is_err());
    assert!(m.user_words_clear("pinyin").is_err());
    assert_eq!(m.user_words("pinyin", "", 0, 50), UserWordPage::default());
    assert!(m.phrase_add("dz", "地址").is_err(), "短语写入同样受只读闸");
    let _ = std::fs::remove_dir_all(dir);
}
