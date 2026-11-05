//! 移动端事件流基线（golden）。
//!
//! 用真实的 `MobileCore::new` 配记录型 `MobileEventSink`，固定脚本跑一遍，把宿主收到的
//! 事件逐行写成「改动后仍存在的字段」，与 `tests/golden/mobile_events.txt` 逐行比较：
//!
//! - `C|preedit|caret|mode_label|has_candidates|selected|page|total_pages`
//! - `H`
//! - `S|chinese_mode|icon_label|full_width|chinese_punct|s2t_enabled|s2t_shown`
//! - `T|text`
//! - `## 步骤名`（脚本自己写的分隔行，不是事件）
//!
//! 候选文本本身不入基线（宿主按需经 `candidates` 拉取）。
//! 没有任何路径/时间类内容，无需归一化。
//!
//! 冷启动帧（预热 prepare 的帧、刚切到的拼音方案的后台索引补刷帧）在录制前预热掉，
//! 不进基线；已知的确定性重复事件（如菜单切英文连发两个 `H`）见报告。
//!
//! 缺出厂数据：设了 `CI` 环境变量（主仓 CI 没有 build_dev/data）则打印原因并跳过；
//! 没设 `CI` 则失败，本地不允许悄悄跳过。
//! `UPDATE_GOLDEN` 只认精确的 `1`，且设了 `CI` 时拒绝写入。
//!
//! 重录：`UPDATE_GOLDEN=1 cargo test -p wind-mobile --test event_stream_golden`。
//! **基线已提交后不得重录。**
//!
//! 只放这一个测试：`MobileCore::new` 会写进程环境变量，同进程并行跑多个会互相干扰。

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wind_keys::keymap::{VK_BACK, VK_ESCAPE, VK_SPACE};
use wind_mobile::{CandidateFrame, InputStatus, MobileCommand, MobileCore, MobileEventSink};

/// 测试结束（含 panic）时清理临时用户目录
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Recorder {
    lines: Mutex<Vec<String>>,
}

impl Recorder {
    fn push(&self, s: String) {
        self.lines.lock().unwrap().push(s);
    }
    fn truncate(&self, n: usize) {
        self.lines.lock().unwrap().truncate(n);
    }
    fn len(&self) -> usize {
        self.lines.lock().unwrap().len()
    }
}

impl MobileEventSink for Recorder {
    fn on_candidates(&self, f: CandidateFrame) {
        self.push(format!(
            "C|{}|{}|{}|{}|{}|{}|{}",
            f.preedit,
            f.preedit_caret,
            f.mode_label,
            f.has_candidates,
            f.selected,
            f.page,
            f.total_pages
        ));
    }
    fn on_hide_candidates(&self) {
        self.push("H".into());
    }
    fn on_status(&self, s: InputStatus) {
        self.push(format!(
            "S|{}|{}|{}|{}|{}|{}",
            s.chinese_mode, s.icon_label, s.full_width, s.chinese_punct, s.s2t_enabled, s.s2t_shown
        ));
    }
    fn on_toast(&self, text: String) {
        self.push(format!("T|{text}"));
    }
}

/// 步骤分隔行。必须在动作**之前**写入，动作引发的事件才落在它后面。
fn mark(rec: &Recorder, step: &str) {
    rec.push(format!("## {step}"));
}

/// 泵线程是异步的：轮询到连续 50ms 无新增事件为止；总超时 20s，超时即失败。
fn settle(rec: &Recorder, step: &str) {
    settle_quiet(rec, step, Duration::from_millis(50));
}

fn settle_quiet(rec: &Recorder, step: &str, quiet: Duration) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = rec.len();
    let mut quiet_since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(5));
        let n = rec.len();
        if n != last {
            last = n;
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= quiet {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "步骤「{step}」一直有新事件，无法收敛"
        );
    }
}

fn letters(core: &MobileCore, s: &str) {
    for c in s.chars() {
        core.key_down((c.to_ascii_uppercase() as u32) & 0xFF, 0);
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
fn mobile_event_stream_matches_golden() {
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !(data_dir.join("config.toml").exists()
        && data_dir.join("schemas/wubi86.schema.toml").exists())
    {
        let msg = format!("缺少出厂数据 {}", data_dir.display());
        if std::env::var_os("CI").is_some() {
            eprintln!("跳过：{msg}（CI 环境）");
            return;
        }
        panic!("{msg}，本地不允许跳过（设 CI 环境变量可跳过）");
    }
    let user_root = std::env::temp_dir().join(format!("wind-mobile-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user_root);
    std::fs::create_dir_all(&user_root).unwrap();
    let _cleanup = RemoveOnDrop(user_root.clone());

    let rec = Arc::new(Recorder::default());
    let core = MobileCore::new(&data_dir, &user_root, "wubi86", rec.clone());

    // 预热窗口：等 prepare 的后台线程完成，再等事件静默，之后才开始脚本
    let t0 = Instant::now();
    while core.debug_coordinator().readiness() != wind_host::Readiness::Ready {
        assert!(t0.elapsed() < Duration::from_secs(60), "prepare 迟迟未就绪");
        std::thread::sleep(Duration::from_millis(10));
    }
    // 反查索引阻塞建好（公开接口，幂等；若后台线程正在建则等它建完）。之后再打一次 w + Esc
    // 吸收可能的补刷帧（补刷只在编码非空时发），长静默收敛后截断，不进基线
    core.debug_coordinator().prewarm_indexes();
    let before_idx = rec.len();
    letters(&core, "w");
    settle_quiet(&rec, "索引预热", Duration::from_millis(300));
    core.key_down(VK_ESCAPE, 0);
    settle_quiet(&rec, "索引预热后取消", Duration::from_millis(300));
    rec.truncate(before_idx);
    settle(&rec, "预热结束");

    // 每步：先写分隔行、再动作、再等静默
    let step = |name: &str, act: &dyn Fn()| {
        mark(&rec, name);
        act();
        settle(&rec, name);
    };

    step("焦点获得", &|| core.focus_gained());
    step("输入 wqvb", &|| letters(&core, "wqvb"));
    step("退格到空", &|| {
        for _ in 0..4 {
            core.key_down(VK_BACK, 0);
        }
    });
    step("再输入 wqvb", &|| letters(&core, "wqvb"));
    step("空格上屏（进入联想）", &|| {
        core.key_down(VK_SPACE, 0);
    });
    step("退格离开联想", &|| {
        core.key_down(VK_BACK, 0);
    });
    step("输入 wq", &|| letters(&core, "wq"));
    step("Esc 取消", &|| {
        core.key_down(VK_ESCAPE, 0);
    });

    let schemas = core.schemas();
    let wubi = schemas
        .iter()
        .position(|s| s.id == "wubi86")
        .expect("wubi86");
    let pinyin = schemas
        .iter()
        .position(|s| s.id == "pinyin")
        .expect("pinyin");

    step("切英文", &|| core.run_command(MobileCommand::English));
    step("切回中文", &|| {
        core.run_command(MobileCommand::SelectSchema(wubi))
    });
    step("切英文标点", &|| {
        core.run_command(MobileCommand::TogglePunct)
    });
    step("切回中文标点", &|| {
        core.run_command(MobileCommand::TogglePunct)
    });
    step("切到拼音方案", &|| {
        core.run_command(MobileCommand::SelectSchema(pinyin))
    });
    // 冷启动预热（不进基线）：刚切到的拼音方案，首次出候选后会有后台建反查索引的补刷帧
    // （coordinator.rs `spawn_index_warm` 末尾的重渲染）和延时「正在建立词库索引…」提示，
    // 到达时刻取决于建索引耗时。先打一次 n 把它们吸收掉，用 500ms 长静默收敛，再截断记录。
    core.debug_coordinator().prewarm_indexes();
    let before_warm = rec.len();
    letters(&core, "n");
    settle_quiet(&rec, "拼音预热", Duration::from_millis(500));
    core.key_down(VK_ESCAPE, 0);
    settle_quiet(&rec, "拼音预热后取消", Duration::from_millis(200));
    rec.truncate(before_warm);

    step("拼音输入声母 n", &|| letters(&core, "n"));
    step("拼音 Esc 取消", &|| {
        core.key_down(VK_ESCAPE, 0);
    });
    step("ReloadConfig", &|| {
        core.run_command(MobileCommand::ReloadConfig)
    });

    let mut text = rec.lines.lock().unwrap().join("\n");
    text.push('\n');
    compare_golden("mobile_events.txt", &text);
}
