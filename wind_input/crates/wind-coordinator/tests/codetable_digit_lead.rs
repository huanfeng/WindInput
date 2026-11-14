//! 码表方案把数字写进 `leading_chars` 时，核心侧数字能否起头（论坛 t322，三维天然码）。
//!
//! 只测**核心**这一层：键到了 core，`4suo` 能否组码出词。Windows 上空缓冲的半角数字键
//! 在 TSF 层就被放行给宿主（`wind_tsf/include/NumberKeyPolicy.h`），根本到不了这里——
//! 那是「数字作首码」未支持的真正卡点，见 docs/design/codetable-input-chars.md §7。
//!
//! 自带临时方案与词库，不依赖 `build_dev/data`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 造一个三维天然码式的临时方案：码元 `a-z1-9`，首码集由 `leading` 给出。
fn make_schema(tag: &str, leading: &str) -> (PathBuf, PathBuf, TempRoot) {
    let root = std::env::temp_dir().join(format!("wind_digit_lead_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let schemas = root.join("data/schemas");
    std::fs::create_dir_all(schemas.join("trm")).unwrap();
    std::fs::write(
        schemas.join("trm.schema.toml"),
        format!(
            "[schema]\nid = \"trm\"\nname = \"天然码测试\"\n\
             [engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 4\n\
             input_chars = \"a-z1-9\"\nleading_chars = \"{leading}\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"trm/trm.dict.yaml\"\ndefault = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        schemas.join("trm/trm.dict.yaml"),
        "---\nname: trm\nversion: \"1\"\n...\n炮台\t4suo\n蜀绣\t4swt\n四季豆\t4t1o\n阿\ta\n",
    )
    .unwrap();
    let ov = root.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    (root.join("data"), ov, TempRoot(root))
}

fn coord_for(tag: &str, leading: &str) -> (std::sync::Arc<Coordinator>, TempRoot) {
    let (data, ov, guard) = make_schema(tag, leading);
    let mut c = Config::default();
    c.schema.available = vec!["trm".into()];
    c.schema.active = "trm".into();
    c.input.default.chinese_mode = true;
    (
        Coordinator::new_headless_with_override(c, Some(&data), Some(ov)),
        guard,
    )
}

fn key(vk: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 字母与数字的 VK 恰为大写 ASCII。
fn press(coord: &Coordinator, code: &str) -> KeyAction {
    let mut last = KeyAction::PassThrough;
    for c in code.chars() {
        last = coord.handle_key_event(&key(u32::from(c.to_ascii_uppercase())));
    }
    last
}

#[test]
fn digit_leading_code_composes_in_core() {
    let (coord, _g) = coord_for("on", "a-z1-9");
    let first = press(&coord, "4");
    assert!(
        matches!(&first, KeyAction::UpdateComposition { text, .. } if text.contains('4')),
        "首码集含数字时，空缓冲的 4 应进缓冲，实际 {first:?}"
    );
    press(&coord, "suo");
    let texts = coord.debug_all_candidate_texts();
    assert!(
        texts.first().is_some_and(|t| t == "炮台"),
        "4suo 应出「炮台」，实际 {texts:?}"
    );
}

/// 码中带数字（`4t1o`）：组码中的 1 是码元而非选第 1 个候选。
#[test]
fn digit_inside_code_composes() {
    let (coord, _g) = coord_for("mid", "a-z1-9");
    press(&coord, "4t1o");
    let texts = coord.debug_all_candidate_texts();
    assert!(
        texts.first().is_some_and(|t| t == "四季豆"),
        "4t1o 应出「四季豆」，实际 {texts:?}"
    );
}

/// 反向对照：首码集不含数字时，空缓冲的 4 不进缓冲（证明上一条是 leading_chars 起的作用）。
#[test]
fn digit_does_not_lead_when_excluded() {
    let (coord, _g) = coord_for("off", "a-z");
    let act = press(&coord, "4");
    assert!(
        matches!(act, KeyAction::PassThrough),
        "首码集不含数字时 4 应透传，实际 {act:?}"
    );
}
