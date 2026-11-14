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
//! 第二段 `first_show_gate_holds_until_caret`（基线 `desktop_ui_stream_gate.txt`）走桌面生产配置
//! （不设 caret_independent），覆盖首显闸门「先压后放」与 caret 坐标透传到 UpdateCandidates /
//! ShowStatusTip；事件驱动预热，兜底定时器依赖见该测试的文档注释。
//!
//! 归一化：`UiCommand` 的 Debug 输出里会带出厂数据目录的绝对路径（机器相关），
//! 一律替换成 `<DATA>`；除此之外不做任何改动。
//!
//! 流内容随平台有差异，故只在 linux 上跑。
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

/// 有出厂数据返回 true；缺数据时 CI 打印原因返回 false（调用方跳过），否则 panic。
fn has_data(dd: &std::path::Path) -> bool {
    if dd.join("schemas/wubi86.schema.toml").exists() {
        return true;
    }
    let msg = format!("缺少出厂数据 {}", dd.display());
    if std::env::var_os("CI").is_some() {
        eprintln!("跳过：{msg}（CI 环境）");
        return false;
    }
    panic!("{msg}，本地不允许跳过（设 CI 环境变量可跳过）");
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

/// 同 [`settle`] 但不写步骤标题（调用方已写）。
fn settle_no_header(rx: &Receiver<UiCommand>, out: &mut Vec<String>) {
    let raw = data_dir();
    let canon = raw.canonicalize().unwrap_or_else(|_| raw.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while let Ok(cmd) = rx.recv_timeout(Duration::from_millis(50)) {
        out.push(
            format!("{cmd:?}")
                .replace(&canon.display().to_string(), "<DATA>")
                .replace(&raw.display().to_string(), "<DATA>"),
        );
        assert!(
            std::time::Instant::now() < deadline,
            "一直有新消息，无法收敛"
        );
    }
}

/// 事件驱动预热：按 `w`，`recv` 到含「编码」段的候选帧（反查索引已建好、补刷帧已到）为止，
/// 再 Esc 并排空。60s 超时即失败。之后的帧都是稳态，不会再有索引补刷。
fn warm_until_code_segment(coord: &Coordinator, rx: &Receiver<UiCommand>) {
    letter(coord, 'w');
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let cmd = rx
            .recv_timeout(left)
            .expect("60s 内没等到含「编码」段的候选帧");
        if matches!(&cmd, UiCommand::UpdateCandidates { .. }) && format!("{cmd:?}").contains("编码")
        {
            break;
        }
    }
    coord.handle_key_event(&key(VK_ESCAPE));
    let _ = rx.try_iter().count();
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
    if !has_data(&dd) {
        return;
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
    // prepare 的假键帧（SetCandidateMinSize / HideCandidates）只属于预热，桌面生产路径不调用
    // prepare，故丢弃不录
    let mut discard = Vec::new();
    settle(&rx, "预热结束", &mut discard);
    // 再事件驱动地等索引补刷帧到齐（prepare 只保证假键走完，不保证后台建索引完成）
    warm_until_code_segment(&coord, &rx);

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

/// 首显闸门 + 坐标透传（桌面生产路径：不设 caret_independent）。
///
/// 只测「先压后放」的顺序：没有 caret 时新组合首帧被闸门压住（`notify_ui_update` 里
/// `arm_pending_first_show` 后 return），喂权威 caret 后出帧且帧里带着该坐标。
///
/// 实际依赖（没有一项靠 sleep 猜时间）：
/// - 冷启动：预热时按 `w` 并 `recv` 到含「编码」段的候选帧（即 `spawn_index_warm` 的补刷帧
///   已到），再 Esc 排空，之后的帧都是稳态。60s 超时即失败。
/// - 兜底定时器：非 fast 档为硬编码 150ms（first_show.rs `first_show_fallback_ms`），不可配。
///   录制步骤里按键后用 `try_iter` 立即确认无候选帧并马上喂 caret，间隔是微秒级；
///   极端调度延迟 >150ms 让定时器先触发时，帧序会变化、测试**明确失败**（不会静默通过）。
///   Esc 触发 `reset_first_show` 作废预热遗留的 pending 与 timer，不靠等待。
#[test]
fn first_show_gate_holds_until_caret() {
    let dd = data_dir();
    if !has_data(&dd) {
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;

    let (coord, rx) = Coordinator::new_headless_with_ui(cfg, Some(&dd));
    coord.prepare();
    let t0 = std::time::Instant::now();
    while coord.readiness() != wind_host::Readiness::Ready {
        assert!(t0.elapsed() < Duration::from_secs(60), "prepare 迟迟未就绪");
        std::thread::sleep(Duration::from_millis(10));
    }
    caret(&coord);
    warm_until_code_segment(&coord, &rx);

    let mut out: Vec<String> = Vec::new();
    // 按键后不 settle：立刻 try_iter 取走此刻已有的（同步产生的）命令，然后马上喂 caret
    out.push("## 无 caret 按 w".into());
    letter(&coord, 'w');
    let before: Vec<String> = rx.try_iter().map(|c| format!("{c:?}")).collect();
    assert!(
        !before.iter().any(|l| l.starts_with("UpdateCandidates")),
        "没有 caret 时首帧应被闸门压住，实际收到: {before:?}"
    );
    out.extend(before);
    out.push("## 喂 caret".into());
    caret(&coord);
    settle_no_header(&rx, &mut out);
    assert!(
        out.iter().any(|l| l.starts_with("UpdateCandidates")),
        "喂 caret 后应出候选帧，实际: {out:?}"
    );
    letter(&coord, 'q');
    settle(&rx, "再按 q", &mut out);
    letter(&coord, 'v');
    letter(&coord, 'b');
    settle(&rx, "再按 vb", &mut out);
    menu(&coord, MenuCmd::TogglePunct);
    settle(&rx, "切英文标点", &mut out);

    let mut text = out.join("\n");
    text.push('\n');
    assert!(text.contains("caret_x: 100"), "帧里应带 caret 坐标");
    assert!(text.contains("caret_valid: true"), "帧里应标 caret 有效");
    compare_golden("desktop_ui_stream_gate.txt", &text);
}
