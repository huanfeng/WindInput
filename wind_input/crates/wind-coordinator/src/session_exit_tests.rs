use super::{ActionOrigin, Coordinator};
use std::sync::atomic::Ordering;
use wind_bridge::handler::KeyAction;
use wind_config::Config;

fn finish(c: &Coordinator, origin: ActionOrigin, text: &str) -> KeyAction {
    let st = c.state.lock().unwrap();
    c.finish_session_action(st, origin, Coordinator::commit_action(text.into(), true))
}

/// 非按键来源上屏后：撤销计数是本次长度（随后的光标回声被认作自提交，不把它刷回 1）、
/// 打了自提交点、检索放宽随组合结束复原（不带进下一次组合）。
#[test]
fn non_key_origins_mark_self_commit_and_restore_scope() {
    use wind_bridge::handler::MessageHandler;
    for origin in [
        ActionOrigin::Pointer,
        ActionOrigin::MobilePick,
        ActionOrigin::CapsLockHook,
        ActionOrigin::Tooltip,
    ] {
        let c = Coordinator::new_headless(Config::default(), None);
        c.state.lock().unwrap().scope_relaxed = true;
        finish(&c, origin, "你好");
        assert!(
            c.last_self_commit.lock().unwrap().is_some(),
            "{origin:?}: 未打点"
        );
        c.handle_selection_changed(0);
        assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 2, "{origin:?}");
        assert!(
            !c.state.lock().unwrap().scope_relaxed,
            "{origin:?}: 放宽未复原"
        );
    }
}

/// 滑窗：候选上屏（鼠标 / 钩子）喂入；悬停提示上屏的不是输入，打断；移动端选词不碰
/// （移动端按键上屏不喂，只喂选词一侧会把不相邻的两次上屏拼成假词）。
#[test]
fn draft_window_per_origin() {
    let mut cfg = Config::default();
    cfg.schema.codetable.auto_phrase.enabled = true;
    let window_empty = |c: &Coordinator| c.draft_window.lock().unwrap().is_empty();
    for (origin, kept) in [
        (ActionOrigin::Pointer, true),
        (ActionOrigin::CapsLockHook, true),
        (ActionOrigin::Tooltip, false),
    ] {
        let c = Coordinator::new_headless(cfg.clone(), None);
        assert!(c.draft_enabled(), "前提：默认方案应启用码表造词");
        finish(&c, ActionOrigin::Key, "你");
        assert!(!window_empty(&c), "前提：按键上屏进滑窗");
        finish(&c, origin, "好");
        assert_eq!(!window_empty(&c), kept, "{origin:?}");
    }
    let c = Coordinator::new_headless(cfg, None);
    finish(&c, ActionOrigin::MobilePick, "好");
    assert!(window_empty(&c), "MobilePick 不应喂滑窗");
    finish(&c, ActionOrigin::Pointer, "好");
    assert!(!window_empty(&c), "对照：同一实例上 Pointer 会喂");
}

/// 撤销删的是光标前的字：配对插入「（|）」只计左段，多字符右段同理。
#[test]
fn cursor_insert_counts_only_text_before_the_cursor() {
    let c = Coordinator::new_headless(Config::default(), None);
    // 回退按「格」（字符）：右段含代理对时不能拿格数当 UTF-16 单元减。
    for (text, offset, want) in [("（）", 1, 1), ("<!---->", 3, 4), ("「😀」", 2, 1)] {
        c.note_self_commit(&KeyAction::InsertTextWithCursor {
            text: text.into(),
            cursor_offset: offset,
        });
        assert_eq!(c.last_commit_len.load(Ordering::Relaxed), want, "{text}");
    }
}

/// 命令输出：打点、打断滑窗、不记统计；软键盘点击与物理键对齐：喂滑窗、记统计。
#[test]
fn command_output_breaks_the_draft_while_softkeyboard_click_feeds_it() {
    let mut cfg = Config::default();
    cfg.schema.codetable.auto_phrase.enabled = true;
    for (origin, fed) in [
        (ActionOrigin::Command, false),
        (ActionOrigin::SoftKeyboardClick, true),
    ] {
        let c = Coordinator::new_headless(cfg.clone(), None);
        finish(&c, ActionOrigin::Key, "你");
        assert!(
            !c.draft_window.lock().unwrap().is_empty(),
            "前提：按键上屏进滑窗"
        );
        c.debug_capture_stat_events();
        c.push_commit_text_from(origin, "好");
        assert!(
            c.last_self_commit.lock().unwrap().is_some(),
            "{origin:?}: 未打点"
        );
        assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 1, "{origin:?}");
        assert_eq!(
            !c.draft_window.lock().unwrap().is_empty(),
            fed,
            "{origin:?}: 滑窗"
        );
        let recorded = c.debug_take_stat_events().iter().any(|(_, t)| t == "好");
        assert_eq!(recorded, fed, "{origin:?}: 统计");
    }
}

/// 命令栏连续多段 `type()` 累加成一笔撤销计数；中间执行过副作用动作则复位为 1。
#[test]
fn command_multi_segment_output_counts_as_one_undo() {
    let c = Coordinator::new_headless(Config::default(), None);
    c.debug_run_command(r#"$CC("t", type("ab"), type("cde"))"#);
    assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 5);
    c.debug_run_command(r#"$CC("t", type("ab"), key.tap("Left"))"#);
    assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 1);
}

/// 切换上屏（托盘 / 热键 / 按应用切方案）不经出口：组合清空时检索放宽要一并复原。
#[test]
fn mode_switch_take_restores_relaxed_scope() {
    let c = Coordinator::new_headless(Config::default(), None);
    let mut st = c.state.lock().unwrap();
    st.scope_relaxed = true;
    st.input_buffer = "ni".into();
    let _ = c.take_input_on_mode_switch(&mut st, false);
    assert!(!st.scope_relaxed);
}

/// 收尾各步只经 [`super::Coordinator::finish_session_action`] 调用：新上屏入口若绕过它直接调
/// 其中一步，就又回到「各入口凭记忆抄几步」的老路，§4.3 那张表也不再反映实情。
#[test]
fn exit_steps_are_only_called_from_finish_session_action() {
    const STEPS: &[&str] = &[
        "apply_auto_spacing(",
        "apply_commit_newline(",
        "record_input_stats(",
        "note_self_commit(",
        "note_draft_commit(",
        "expire_scope_override(",
        "touch_pair_state(",
    ];
    // (相对 src 的路径, 允许的步骤)：`debug_support.rs` 是给集成测试的转发口，只转发统计。
    const ALLOWED: &[(&str, &str)] = &[("debug_support.rs", "record_input_stats(")];
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    let mut bad = Vec::new();
    let mut in_finish = vec![0usize; STEPS.len()];
    for f in &files {
        let rel = f
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        // 整文件都是测试、`cfg(test)` 写在 lib.rs 的 mod 声明上的那类文件。
        if rel.ends_with("_tests.rs") {
            continue;
        }
        let prod = crate::source_scan::production_code(&std::fs::read_to_string(f).unwrap());
        let code: String = prod
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        for (k, step) in STEPS.iter().enumerate() {
            // 定义处 `fn step(` 不算调用；全路径 `Self::step(` 与方法调用 `.step(` 都算。
            let n = code.matches(step).count() - code.matches(&format!("fn {step}")).count();
            if rel == "session_exit.rs" {
                in_finish[k] = n;
            } else if n > 0 && !ALLOWED.contains(&(rel.as_str(), *step)) {
                bad.push(format!("{rel}: {step}…) ×{n}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "以下收尾步骤绕过了 finish_session_action：\n  {}\n\
             新上屏入口请调 finish_session_action 并在 exit_steps 里给来源填一行。",
        bad.join("\n  ")
    );
    for (step, n) in STEPS.iter().zip(&in_finish) {
        assert_eq!(
            *n, 1,
            "finish_session_action 应恰好调一次 {step}…)，实为 {n}"
        );
    }
}
