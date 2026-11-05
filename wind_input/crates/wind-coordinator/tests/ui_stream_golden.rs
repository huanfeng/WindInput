//! 桌面 UiCommand 事件流基线（golden）。
//!
//! 固定脚本跑一遍 desktop Profile 的 headless 协调器，把收到的全部 `UiCommand`
//! 按 `{:?}` 逐行记下，与 `tests/golden/desktop_ui_stream.txt` 逐行比较。
//! 用途：之后改移动端输出通路时，证明桌面路径的输出分毫未变。
//!
//! 重录：`UPDATE_GOLDEN=1 cargo test -p wind-coordinator --test ui_stream_golden`。
//! **基线已提交后不得重录**——重录就失去了比对的意义。
//!
//! 归一化：`UiCommand` 的 Debug 输出里会带出厂数据目录的绝对路径（机器相关），
//! 一律替换成 `<DATA>`；除此之外不做任何改动。
//!
//! 流内容随平台有差异，故只在 linux 上跑。缺出厂数据直接失败，不跳过。

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use wind_bridge::handler::{CaretData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, caret_source};
use wind_keys::keymap::{VK_BACK, VK_ESCAPE, VK_SPACE};
use wind_ui_types::{MenuCmd, MenuKind, UiCommand, UiEvent};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
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
    }
}

fn letter(coord: &Coordinator, c: char) {
    coord.handle_key_event(&key((c.to_ascii_uppercase() as u32) & 0xFF));
}

fn caret(coord: &Coordinator) {
    coord.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: 100,
        composition_start_y: 200,
        source: caret_source::TSF_SELECTION,
        composition_rect: None,
    });
}

fn menu(coord: &Coordinator, cmd: MenuCmd) {
    coord.inject_ui_event(UiEvent::MenuAction(MenuKind::Command(cmd)));
}

/// 收到静默（50ms 内无新消息）为止；总超时 10s，超时即失败。
fn settle(rx: &Receiver<UiCommand>, step: &str, out: &mut Vec<String>) {
    let raw = data_dir();
    let canon = raw.canonicalize().unwrap_or_else(|_| raw.clone());
    let norm = |s: String| {
        s.replace(&canon.display().to_string(), "<DATA>")
            .replace(&raw.display().to_string(), "<DATA>")
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    out.push(format!("## {step}"));
    while let Ok(cmd) = rx.recv_timeout(Duration::from_millis(50)) {
        out.push(norm(format!("{cmd:?}")));
        assert!(
            std::time::Instant::now() < deadline,
            "步骤「{step}」一直有新消息，无法收敛"
        );
    }
}

fn compare_golden(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到对照文件 {}: {e}", path.display()));
    if expected == actual {
        return;
    }
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    let first = (0..e.len().max(a.len()))
        .find(|&i| e.get(i) != a.get(i))
        .unwrap_or(0);
    let ctx = |v: &[&str]| {
        let lo = first.saturating_sub(3);
        let hi = (first + 4).min(v.len());
        (lo..hi)
            .map(|i| format!("{:>5}| {}", i + 1, v[i]))
            .collect::<Vec<_>>()
            .join("\n")
    };
    panic!(
        "与对照文件 {name} 不一致，首处差异在第 {} 行\n--- 期望 ---\n{}\n--- 实际 ---\n{}",
        first + 1,
        ctx(&e),
        ctx(&a)
    );
}

#[test]
fn desktop_ui_stream_matches_golden() {
    let dd = data_dir();
    assert!(
        dd.join("schemas/wubi86.schema.toml").exists(),
        "缺少出厂数据 {}，本录制不允许因缺数据而跳过",
        dd.display()
    );
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;

    let (coord, rx) = Coordinator::new_headless_with_ui(cfg, Some(&dd));
    let mut out: Vec<String> = Vec::new();
    settle(&rx, "构造", &mut out);
    // 桌面首显闸门要先有光标坐标，候选才会下发
    caret(&coord);
    settle(&rx, "光标就位", &mut out);

    for c in "wqvb".chars() {
        letter(&coord, c);
    }
    settle(&rx, "输入 wqvb", &mut out);
    for _ in 0..4 {
        coord.handle_key_event(&key(VK_BACK));
    }
    settle(&rx, "退格到空", &mut out);
    caret(&coord);
    for c in "wqvb".chars() {
        letter(&coord, c);
    }
    settle(&rx, "再输入 wqvb", &mut out);
    coord.handle_key_event(&key(VK_SPACE));
    settle(&rx, "空格上屏", &mut out);
    caret(&coord);
    for c in "wq".chars() {
        letter(&coord, c);
    }
    settle(&rx, "输入 wq", &mut out);
    coord.handle_key_event(&key(VK_ESCAPE));
    settle(&rx, "Esc 取消", &mut out);
    menu(&coord, MenuCmd::SchemaEnglish);
    settle(&rx, "菜单切英文", &mut out);
    menu(&coord, MenuCmd::SchemaSelect(0));
    settle(&rx, "菜单切回中文", &mut out);
    menu(&coord, MenuCmd::TogglePunct);
    settle(&rx, "切英文标点", &mut out);
    menu(&coord, MenuCmd::TogglePunct);
    settle(&rx, "切回中文标点", &mut out);

    let mut text = out.join("\n");
    text.push('\n');
    compare_golden("desktop_ui_stream.txt", &text);
}
