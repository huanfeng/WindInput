//! 词库文件（`.dict.yaml`）里的 `$SS` / `$AA` 组短语端到端（t172，看板 A2-24）。
//!
//! 报告：按文档把 `$SS(...)` 写进词库文件不生效，同样内容在设置页新增就可以。
//! 这里走真实的文件加载路径：临时方案目录 → 码表解析 → 候选构建 → `finalize_candidates`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn make_data_dir(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_ss_file_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("st")).unwrap();
    std::fs::write(
        schemas.join("st.schema.toml"),
        "[schema]\nid = \"st\"\nname = \"SS测试\"\n\
         [engine]\ntype = \"codetable\"\n\
         [engine.codetable]\nmax_code_length = 4\n\
         [[dictionaries]]\nid = \"main\"\npath = \"st/st.dict.yaml\"\ndefault = true\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("st/st.dict.yaml"),
        format!("---\nname: st\nversion: \"1\"\n...\n{body}"),
    )
    .unwrap();
    dir
}

fn coord_for(dir: &std::path::Path, ov_tag: &str) -> (std::sync::Arc<Coordinator>, PathBuf) {
    let mut c = Config::default();
    c.schema.available = vec!["st".into()];
    c.schema.active = "st".into();
    c.input.default.chinese_mode = true;
    let ov = std::env::temp_dir().join(format!("wind_ss_file_ov_{ov_tag}"));
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).unwrap();
    let coord = Coordinator::new_headless_with_override(c, Some(dir), Some(ov.clone()));
    (coord, ov)
}

fn type_keys(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&KeyEventData {
            key_code: ch.to_ascii_uppercase() as u32,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }
}

/// 文档写法（`文本\t编码`），精确码应炸开为各成员。
#[test]
fn ss_in_dict_file_expands_on_exact_code() {
    let dir = make_data_dir(
        "exact",
        "阿\ta\n$SS(\"括号\", \"【】\", \"（）\", \"〔〕\")\tuu\n",
    );
    let (coord, ov) = coord_for(&dir, "exact");
    type_keys(&coord, "uu");
    let got = coord.debug_all_candidate_texts();
    for want in ["【】", "（）", "〔〕"] {
        assert!(got.iter().any(|t| t == want), "缺 {want}，实际: {got:?}");
    }
    assert!(
        !got.iter().any(|t| t.contains("$SS")),
        "源码不应上候选: {got:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ov);
}

/// 前缀码应折叠为组名。
#[test]
fn ss_in_dict_file_folds_to_group_name_on_prefix() {
    let dir = make_data_dir("prefix", "阿\ta\n$SS(\"括号\", \"【】\", \"（）\")\tuuk\n");
    let (coord, ov) = coord_for(&dir, "prefix");
    type_keys(&coord, "uu");
    let got = coord.debug_all_candidate_texts();
    assert!(
        got.iter().any(|t| t == "括号"),
        "前缀应出组名，实际: {got:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ov);
}

/// 报告原样：`编码\t$SS(...)`（编码在前），混在「文本在前」的词库里。
#[test]
fn ss_in_dict_file_code_first_line_in_text_first_file() {
    let dir = make_data_dir(
        "codefirst",
        "阿\ta\n乙\tb\n丙\tc\nuu\t$SS(\"括号\", \"【】\", \"（）\", \"〔〕\")\n",
    );
    let (coord, ov) = coord_for(&dir, "codefirst");
    type_keys(&coord, "uu");
    let got = coord.debug_all_candidate_texts();
    for want in ["【】", "（）", "〔〕"] {
        assert!(got.iter().any(|t| t == want), "缺 {want}，实际: {got:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ov);
}
