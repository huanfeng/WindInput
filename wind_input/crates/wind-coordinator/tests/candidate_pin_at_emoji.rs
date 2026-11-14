//! 开 emoji 扩展时「固定到第 N 位」整组灰、写端不写：emoji 挂在前 `max_hosts` 条之后（或
//! 常用候选之后的末尾档），词一挪 emoji 就改挂别处，界面名次没法稳定折算。置顶不受影响。
//!
//! emoji 表只从 `Config::data_dir()`（安装根）加载，故整棵目录树经便携标记重定向到临时目录
//! （同 `emoji_block_auto_commit.rs`）；一个进程只能重定向一次，故单开测试二进制。
//!
//! ⚠️ 依赖 `build_dev/data`；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_ui_types::{CandidateOp, MenuKind};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/pinyin.schema.toml").exists() && d.join("emoji/frost_emoji.txt").exists()
}

fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("wind_pin_at_emoji-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("userdata")).unwrap();
        let data = data_dir().canonicalize().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&data, root.join("data")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&data, root.join("data")).unwrap();
        std::fs::write(root.join(wind_config::variant::PORTABLE_MARKER_NAME), "").unwrap();
        // SAFETY: 在任何路径缓存初始化之前设置；本二进制只有这一个用例。
        unsafe {
            std::env::set_var("WIND_INSTALL_ROOT", &root);
        }
        assert_eq!(
            Config::user_config_dir(),
            Some(root.join("userdata")),
            "前置条件：用户目录须已重定向，否则本测试会读写真实用户配置"
        );
        root
    })
}

#[test]
fn pin_at_disabled_when_emoji_present() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 拼音方案 / emoji 数据");
        return;
    }
    let store_path = root().join("pin.redb");
    let _ = std::fs::remove_file(&store_path);
    let store = Arc::new(wind_store::Store::open(&store_path).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.emoji.enabled = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), Arc::clone(&store));
    coord.debug_sync_emoji_dict();
    for ch in "pingguo".chars() {
        coord.handle_key_event(&KeyEventData {
            key_code: (ch.to_ascii_uppercase() as u32) & 0xFF,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }
    let all = coord.debug_all_candidate_texts();
    assert_eq!(
        all.first().map(String::as_str),
        Some("苹果"),
        "前提：{all:?}"
    );
    assert!(
        all.get(1).is_some_and(|t| t.starts_with('🍎')),
        "前提：首选后挂 emoji：{all:?}"
    );
    let idx = all.len().min(4) - 1;
    let items = coord.candidate_menu_items(idx).unwrap();
    let pin = items
        .iter()
        .find(|m| m.label == "固定到第 N 位")
        .expect("拼音仍列出这一项");
    assert!(
        !pin.enabled && pin.children.iter().all(|c| !c.enabled),
        "整组灰：{:?}",
        pin.children
    );
    assert!(
        items
            .iter()
            .any(|m| m.kind == MenuKind::Op(CandidateOp::MoveTop) && m.enabled),
        "置顶不受影响"
    );
    coord.candidate_or_quick_format_op_at(CandidateOp::PinAt(1), idx);
    assert!(
        store
            .get_shadow_rules("pinyin", "pingguo")
            .unwrap()
            .is_none(),
        "写端同判据不写"
    );
}
