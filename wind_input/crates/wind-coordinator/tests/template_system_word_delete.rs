//! 码表**系统词库**里的模板词条（`$Y年$M月$D日`）右键删除：规则记在模板源文本上（与词频同键，
//! 跨日稳定），读端认展开后的候选也认源文本，删了当场不再出。
//!
//! 夹具是临时数据目录里的一个最小码表方案，不依赖 `build_dev`。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;
use wind_ui_types::CandidateOp;

const DATE_TEMPLATE: &str = "$Y年$M月$D日";

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
        coord.handle_key_event_policed(&key((ch.to_ascii_uppercase() as u32) & 0xFF));
    }
}

/// 临时数据目录：`tmplct` 码表方案，`rq` 下一条模板词、一条普通词。
fn fixture(base: &Path) -> PathBuf {
    let data = base.join("data");
    let dir = data.join("schemas/tmplct");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        data.join("schemas/tmplct.schema.toml"),
        r#"[schema]
id = "tmplct"
name = "模板测试"

[engine]
type = "codetable"

[engine.codetable]
max_code_length = 4

[[dictionaries]]
id = "tmplct_main"
path = "tmplct/tmplct.dict.yaml"
type = "rime_codetable"
default = true
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("tmplct.dict.yaml"),
        format!(
            "---\nname: tmplct\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\nrq\t{DATE_TEMPLATE}\t900\nrq\t日期\t100\n"
        ),
    )
    .unwrap();
    data
}

fn is_date(t: &str) -> bool {
    t.contains('年') && t.contains('月') && t.contains('日') && !t.contains('$')
}

#[test]
fn template_system_word_delete_takes_effect() {
    let base = std::env::temp_dir().join(format!("wind_tmpl_sysdel_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data = fixture(&base);
    let store = Arc::new(Store::open(base.join("user.redb")).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec!["tmplct".into()];
    cfg.schema.active = "tmplct".into();
    cfg.input.default.chinese_mode = true;
    let open =
        || Coordinator::new_headless_with_store(cfg.clone(), Some(&data), Arc::clone(&store));

    let coord = open();
    type_str(&coord, "rq");
    let page = coord.debug_page_texts();
    let idx = page
        .iter()
        .position(|t| is_date(t))
        .unwrap_or_else(|| panic!("前提：`rq` 应有展开后的日期，实际 {page:?}"));
    assert!(page.iter().any(|t| t == "日期"), "前提：{page:?}");
    coord.debug_candidate_op(CandidateOp::Delete, idx);
    assert_eq!(
        store
            .get_shadow_rules("tmplct", "rq")
            .unwrap()
            .map(|r| r.deleted),
        Some(vec![DATE_TEMPLATE.to_string()]),
        "规则记模板源文本"
    );
    let now = coord.debug_page_texts();
    assert!(!now.iter().any(|t| is_date(t)), "当场不再出：{now:?}");

    let coord = open();
    type_str(&coord, "rq");
    let page = coord.debug_page_texts();
    assert!(!page.iter().any(|t| is_date(t)), "重开后也不出：{page:?}");
    assert!(page.iter().any(|t| t == "日期"), "同码别的词照出：{page:?}");
    let _ = std::fs::remove_dir_all(&base);
}
