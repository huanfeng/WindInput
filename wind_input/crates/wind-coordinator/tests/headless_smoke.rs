//! headless 形态冒烟：Android FFI 入口契约的最小闭环。
//!
//! 锁三件事：`new_headless_with_ui` 可用（验收标准 4）、喂键后 `Receiver<UiCommand>`
//! 收到**含候选**的 `UpdateCandidates`、清空后收到 `HideCandidates`。
//!
//! ⚠ 本文件是 tests/ 里唯一必须在 `--no-default-features` 下可编译的：类型一律走
//! `wind_ui_types::`（其余测试文件沿用 `wind_ui::` 再导出路径，默认 feature 才编译）。
//! headless 门（CI）单独跑它：`cargo test -p wind-coordinator --no-default-features
//! --test headless_smoke`。

use std::path::PathBuf;
use wind_bridge::handler::{CaretData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_ui_types::UiCommand;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    data_dir().join("schemas/wubi86.schema.toml").exists()
        || data_dir().join("schemas/wubi86.schema.yaml").exists()
}

fn key_event(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

/// 构造 → 喂一串键 → Receiver 收到含候选的 UiCommand → Esc 清空收到 Hide。
#[test]
fn feed_keys_yields_candidates_on_ui_receiver() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data 方案");
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;

    let (coord, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    let _ = rx.try_iter().count(); // 排空构造期下发（工具栏/主题等）

    // 五笔 "aa"（VK_A=0x41）：必有候选（艹/式 等）。
    coord.handle_key_event(&key_event(0x41));
    coord.handle_key_event(&key_event(0x41));

    // 释放候选窗首显闸门：新组合首帧要等**权威 caret**（TSF 域）才下发
    // UpdateCandidates（防 reflow 前陈旧坐标错位，见 coordinator 首显闸门注释）。
    // headless 下没有宿主上报，测试模拟一次权威坐标——Android FFI 同样要么喂
    // caret、要么把宿主 first_show_mode 设 instant，这正是本冒烟要锁的通道语义。
    coord.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });

    // 只锁「含候选」这一条：preedit 依 ui.candidate.preedit_display 配置可为空
    // （默认 in_app 由宿主画组合串），不属于本冒烟的通道契约。
    let mut got_candidates = false;
    for cmd in rx.try_iter() {
        if let UiCommand::UpdateCandidates { candidates, .. } = &cmd
            && !candidates.is_empty()
        {
            got_candidates = true;
        }
    }
    assert!(
        got_candidates,
        "喂键后 Receiver 必须收到含候选的 UpdateCandidates——这是 Android FFI 反向通道的最小契约"
    );

    // Esc 清空：composition 结束，UI 收到 HideCandidates。
    coord.handle_key_event(&key_event(wind_keys::keymap::VK_ESCAPE));
    let hidden = rx
        .try_iter()
        .any(|cmd| matches!(cmd, UiCommand::HideCandidates));
    assert!(hidden, "Esc 后必须下发 HideCandidates");
}
