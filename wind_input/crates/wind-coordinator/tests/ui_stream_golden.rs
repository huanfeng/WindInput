//! 桌面 UiCommand 事件流基线（golden）。
//!
//! 固定脚本跑一遍 desktop Profile 的 headless 协调器，把收到的全部 `UiCommand`
//! 按 `{:?}` 逐行记下，与 `tests/golden/desktop_ui_stream.txt` 逐行比较。
//! 用途：之后改移动端输出通路时，证明桌面路径的输出分毫未变。
//!
//! 缺出厂数据：设了 `CI` 环境变量（主仓 CI 没有 build_dev/data）则打印原因并跳过；
//! 没设 `CI` 则失败，本地不允许悄悄跳过。
//! `UPDATE_GOLDEN` 只认精确的 `1`，且设了 `CI` 时拒绝写入。
//!
//! 重录：`UPDATE_GOLDEN=1 cargo test -p wind-coordinator --test ui_stream_golden`。
//! **基线已提交后不得重录**——重录就失去了比对的意义。
//!
//! 脚本前先 `set_caret_independent(true)`（首显闸门对新组合首帧 arm 兜底 timer 并丢弃该帧，
//! 出不出帧取决于定时器与按键谁先到）并等 `prepare` 就绪（冷启动惰性索引未建好时，
//! 首批候选 tooltip 缺「编码」段），这两项都会让基线随时序抖动，故在录制前去掉。
//!
//! 归一化：`UiCommand` 的 Debug 输出里会带出厂数据目录的绝对路径（机器相关），
//! 一律替换成 `<DATA>`；除此之外不做任何改动。
//!
//! 流内容随平台有差异，故只在 linux 上跑。
#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
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
    // 只认精确的 "1"；CI 环境一律不许写基线
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI 环境禁止 UPDATE_GOLDEN=1 重录基线"
        );
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到对照文件 {}: {e}", path.display()));
    if expected == actual {
        return;
    }
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    let ctx = |v: &[&str], first: usize| {
        let lo = first.saturating_sub(3);
        let hi = (first + 4).min(v.len());
        (lo..hi)
            .map(|i| format!("{:>5}| {}", i + 1, v[i]))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match (0..e.len().max(a.len())).find(|&i| e.get(i) != a.get(i)) {
        Some(first) => {
            // 差异行再做字节级定位：长行里肉眼找不到差在哪
            let (el, al) = (
                e.get(first).copied().unwrap_or(""),
                a.get(first).copied().unwrap_or(""),
            );
            let off = el
                .bytes()
                .zip(al.bytes())
                .position(|(x, y)| x != y)
                .unwrap_or(el.len().min(al.len()));
            let window = |s: &str| {
                let lo = (off.saturating_sub(80)..=off)
                    .rev()
                    .find(|&i| s.is_char_boundary(i))
                    .unwrap_or(0);
                let hi = (off + 80..=s.len().min(off + 83))
                    .find(|&i| s.is_char_boundary(i))
                    .unwrap_or(s.len());
                s[lo..hi.min(s.len())].to_string()
            };
            panic!(
                "与对照文件 {name} 不一致，首处差异在第 {} 行，第一个不同的字节偏移 {off}\n--- 期望 ---\n{}\n--- 实际 ---\n{}\n--- 差异处附近 ---\n期望: …{}…\n实际: …{}…",
                first + 1,
                ctx(&e, first),
                ctx(&a, first),
                window(el),
                window(al)
            );
        }
        // 逐行相同，只差末尾换行 / 行尾空白等
        None => panic!(
            "与对照文件 {name} 逐行相同但整体不等（只差末尾换行？）：期望 {} 字节，实际 {} 字节，期望以换行结尾={}，实际以换行结尾={}",
            expected.len(),
            actual.len(),
            expected.ends_with('\n'),
            actual.ends_with('\n')
        ),
    }
}

#[test]
fn desktop_ui_stream_matches_golden() {
    let dd = data_dir();
    if !dd.join("schemas/wubi86.schema.toml").exists() {
        let msg = format!("缺少出厂数据 {}", dd.display());
        if std::env::var_os("CI").is_some() {
            eprintln!("跳过：{msg}（CI 环境）");
            return;
        }
        panic!("{msg}，本地不允许跳过（设 CI 环境变量可跳过）");
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;

    let (coord, rx) = Coordinator::new_headless_with_ui(cfg, Some(&dd));
    let mut out: Vec<String> = Vec::new();
    settle(&rx, "构造", &mut out);
    // 关掉首显闸门：闸门对新组合首帧会 arm 兜底 timer 并丢弃该帧，是否出帧取决于定时器
    // 与按键谁先到，进基线会抖。其余输出路径不受影响。
    coord.set_caret_independent(true);
    // 冷启动预热：惰性索引未建好时首批候选的 tooltip 缺「编码」段，预热后才是稳态
    coord.prepare();
    let t0 = std::time::Instant::now();
    while coord.readiness() != wind_host::Readiness::Ready {
        assert!(t0.elapsed() < Duration::from_secs(60), "prepare 迟迟未就绪");
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(&rx, "预热结束", &mut out);

    for c in "wqvb".chars() {
        letter(&coord, c);
    }
    settle(&rx, "输入 wqvb", &mut out);
    for _ in 0..4 {
        coord.handle_key_event(&key(VK_BACK));
    }
    settle(&rx, "退格到空", &mut out);
    for c in "wqvb".chars() {
        letter(&coord, c);
    }
    settle(&rx, "再输入 wqvb", &mut out);
    coord.handle_key_event(&key(VK_SPACE));
    settle(&rx, "空格上屏", &mut out);
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
