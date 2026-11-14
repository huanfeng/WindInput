//! 特殊模式的**标点顶屏**与**全码自动上屏**两个出口，按该模式选词出口（`commit_special_candidate`）
//! 同一口径记账：词频归特殊方案自身、进上屏历史、记输入统计。生僻字模式维持既有例外：
//! 不记词频、只记历史（1a71316e）。
//!
//! 现场：两个出口都只有 `record_commit`（统计），既不记词频也不进上屏历史——顶屏 / 自动上屏
//! 出去的字，`;` 重复上屏取不到，调频也永远不动。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;
use wind_store::stats::CommitSource;

const VK_BACKSLASH: u32 = 0xDC;
const VK_SEMICOLON: u32 = 0xBA;
const VK_COMMA: u32 = 0xBC;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 主方案拼音；`\` 进以 wubi86 为码表的特殊模式（经 override 层给它 `[overlay]` 段，
/// 并在方案名下开调频与满码唯一自动上屏——overlay 方案不继承全局码表配置）。
fn open(tag: &str) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    open_with(tag, |_| {})
}

fn open_with(tag: &str, edit: impl FnOnce(&mut Config)) -> (Arc<Coordinator>, Arc<Store>, PathBuf) {
    let base = std::env::temp_dir().join(format!("wind_special_acct_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ov = base.join("override");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("wubi86.toml"),
        "[overlay]\nkind = \"special\"\n\
         [engine.codetable]\nauto_commit_at_full = true\n\
         [engine.codetable.frequency]\nenabled = true\n",
    )
    .unwrap();
    let mut c = Config::default();
    c.schema.available = vec!["pinyin".into(), "wubi86".into(), "english".into()];
    c.schema.active = "pinyin".into();
    c.input.default.chinese_mode = true;
    c.keys
        .key_actions
        .insert("backslash".into(), "special:wubi86".into());
    edit(&mut c);
    let store = Arc::new(Store::open(base.join("user.redb")).unwrap());
    let coord = Coordinator::new_headless_with_store_override(
        c,
        Some(&data_dir()),
        Arc::clone(&store),
        Some(ov),
    );
    (coord, store, base)
}

fn enter_special(coord: &Coordinator) {
    coord.handle_key_event(&key(VK_BACKSLASH));
    assert_eq!(
        coord.debug_active_mode(),
        Some("special"),
        "前提：`\\` 进特殊模式"
    );
}

fn inserted(act: KeyAction) -> String {
    match act {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("前提：应上屏，实际: {other:?}"),
    }
}

/// 进快捷输入（空缓冲）断言重复上屏候选就是 `want`（即它进了上屏历史）。
fn assert_repeat(coord: &Coordinator, want: &str) {
    coord.handle_key_event(&key(VK_SEMICOLON));
    assert_eq!(
        coord.debug_page_texts().first().map(String::as_str),
        Some(want),
        "应进上屏历史（`;` 重复上屏取得到）"
    );
}

#[test]
fn punct_top_commit_records_freq_and_history() {
    skip_without_data!();
    let (coord, store, base) = open("punct");
    enter_special(&coord);
    type_str(&coord, "aa");
    let first = coord
        .debug_page_texts()
        .first()
        .cloned()
        .expect("前提：`aa` 有候选");
    coord.debug_capture_stat_events();
    let text = inserted(coord.handle_key_event(&key(VK_COMMA)));
    assert_eq!(text, format!("{first}，"), "前提：顶屏高亮 + 标点");
    assert!(
        store.get_freq("wubi86", "aa", &first).unwrap().is_some(),
        "标点顶屏应按特殊方案自身记词频（同选词出口）"
    );
    assert_eq!(
        coord.debug_take_stat_events(),
        vec![
            (CommitSource::SpecialMode, first.clone()),
            (CommitSource::Punctuation, "，".to_string()),
        ]
    );
    assert_repeat(&coord, &first);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn auto_commit_records_freq_and_history() {
    skip_without_data!();
    let (coord, store, base) = open("auto");
    enter_special(&coord);
    type_str(&coord, "aad");
    let act = coord.handle_key_event(&key(u32::from(b'G')));
    let text = inserted(act);
    assert_eq!(text, "工厂", "前提：`aadg` 满码唯一自动上屏");
    assert!(
        store.get_freq("wubi86", "aadg", "工厂").unwrap().is_some(),
        "自动上屏应按特殊方案自身记词频（同选词出口）"
    );
    assert_repeat(&coord, "工厂");
    let _ = std::fs::remove_dir_all(&base);
}

/// 生僻字模式的标点顶屏：维持「不记词频、只记历史」的既有例外。
#[test]
fn rare_char_punct_top_commit_records_history_only() {
    skip_without_data!();
    // 生僻字模式不经 overlay 方案：主方案五笔、z 直进（headless 短语层空，z 不是活码前缀）。
    let db = std::env::temp_dir().join(format!(
        "wind_special_acct_rare_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db);
    let store = Arc::new(Store::open(&db).unwrap());
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.schema.codetable.z_key_action = "rare_char".into();
    c.schema.codetable.frequency.enabled = true;
    let coord = Coordinator::new_headless_with_store(c, Some(&data_dir()), Arc::clone(&store));
    // 有 store 时系统短语层在位，z 是 `zz*` 的活码前缀、首键让位；第二键破前缀才夺取，
    // 残余码 `g` 带进生僻字模式。
    type_str(&coord, "zg");
    assert_eq!(
        coord.debug_active_mode(),
        Some("rare_char"),
        "前提：`zg` 破前缀夺取进生僻字模式"
    );
    type_str(&coord, "g");
    let first = coord
        .debug_page_texts()
        .first()
        .cloned()
        .expect("前提：生僻字模式下 `gg` 应有候选");
    let text = inserted(coord.handle_key_event(&key(VK_COMMA)));
    assert_eq!(text, format!("{first}，"));
    for bucket in ["wubi86", "pinyin"] {
        assert!(
            store.get_freq(bucket, "gg", &first).unwrap().is_none(),
            "生僻字模式不记词频（{bucket} 桶）"
        );
    }
    assert_repeat(&coord, &first);
    let _ = std::fs::remove_file(&db);
}

/// 特殊模式无候选按标点：读 `punct_on_empty_behavior`，与主路空码标点同一套语义。
#[test]
fn no_candidate_punct_follows_punct_on_empty_behavior() {
    skip_without_data!();
    for (policy, want) in [
        ("commit", Some("vvvx，")),
        ("clear", Some("，")),
        ("clear_no_input", None),
    ] {
        let (coord, _store, base) = open_with(&format!("empty_{policy}"), |c| {
            c.input.punct_on_empty_behavior = policy.into();
        });
        enter_special(&coord);
        type_str(&coord, "vvvx");
        assert!(
            coord.debug_page_texts().is_empty(),
            "前提：`vvvx` 无候选：{:?}",
            coord.debug_page_texts()
        );
        let got = match coord.handle_key_event(&key(VK_COMMA)) {
            KeyAction::InsertText { text, .. } => Some(text),
            KeyAction::ClearComposition => None,
            other => panic!("应上屏或清空，实际: {other:?}"),
        };
        assert_eq!(got.as_deref(), want, "{policy}");
        assert_eq!(coord.debug_active_mode(), None, "{policy}：应退出特殊模式");
        let _ = std::fs::remove_dir_all(&base);
    }
}
