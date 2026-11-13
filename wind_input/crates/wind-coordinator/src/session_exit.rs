//! 会话动作的统一收尾（落点指引 P6，见 `docs/architecture/feature-placement-guide.md` §4.3 / §5.2）。
//!
//! 产出 `KeyAction` 的上屏 / 组合动作有四个来源：按键、鼠标点选、CapsLock 全局钩子、悬停提示菜单。
//! 不产出 `KeyAction`、直接推文本的通路（DLL 排水、切方案上屏、命令栏 / 软键盘、配对符号）不在表内，
//! 见落点指引 §4.3。各来源该接哪几步
//! 收尾写在 [`exit_steps`] 一张表里——新增来源时编译器会逼着在表里填一行，而不是在新入口处凭记忆
//! 抄几步。
//!
//! 只收**平台中立**的步骤。键位相关的（智能符号的同键判据、联想占位的透传改判、TSF 组合区占位）
//! 留在按键出口 `handle_key_event_policed`；移动端直调内层 `handle_key_event`，不经这里。

use std::sync::MutexGuard;

use wind_bridge::handler::KeyAction;

use crate::coordinator::{Coordinator, State};

/// 会话动作的来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionOrigin {
    /// 桌面按键（bridge 的按键出口）。
    Key,
    /// 桌面鼠标点选候选（含无障碍 UIElement 的「选高亮」）。
    Pointer,
    /// 移动端候选栏按下标选词（`candidate_pull::select_candidate`）。
    MobilePick,
    /// CapsLock 全局钩子（事件不经 TSF）。
    CapsLockHook,
    /// 悬停提示菜单的「上屏」。
    Tooltip,
}

/// 草稿滑窗（码表自动造词）对一次上屏的处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftStep {
    /// 喂入：上屏文字是用户的输入，可与前后上屏连成词。
    Feed,
    /// 打断：上屏的不是输入（提示文本），滑窗就此断开，同时标记词边界。
    Break,
    /// 不碰：该来源所在平台的按键上屏不经本出口、不喂滑窗，只在选词这一侧喂会把不相邻的
    /// 两次上屏拼成假词。
    Skip,
}

/// 某来源接哪几步收尾。中英补空格各来源都接，不在表内。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExitSteps {
    /// 上屏换行改写。非按键来源经 push 通道投递，在编码时改写（`push_no_key_ctx_action`）。
    /// 这里若也做，鼠标点选返回给移动端与 `debug_mouse_select` 的动作会变（改写本身幂等）。
    pub commit_newline: bool,
    /// 顶层输入统计兜底（`record_input_stats`）。
    pub input_stats: bool,
    /// 自提交打点（`note_self_commit`）：撤销上屏计数 + 光标回声判别。
    pub self_commit: bool,
    /// 草稿滑窗（`note_draft_commit` / `terminate_auto_phrase`）。
    pub draft: DraftStep,
    /// 检索范围临时放宽在组合结束时复原（`expire_scope_override`）。
    pub expire_scope: bool,
    /// 配对状态保活（`touch_pair_state`）。
    pub touch_pair: bool,
}

/// 各来源的收尾表（落点指引 §4.3）。
///
/// 非按键来源不接的两步：
/// - 换行改写：推送编码时改写（见 [`ExitSteps::commit_newline`]）。
/// - 输入统计兜底：这些来源的上屏出口自己 `record_commit`，兜底只补漏记的 InsertText；去重
///   标志 `stat_recorded` 由按键入口复位，接入还得在各入口复位它，收益小（2026-11-13 定）。
pub(crate) const fn exit_steps(origin: ActionOrigin) -> ExitSteps {
    match origin {
        ActionOrigin::Key => ExitSteps {
            commit_newline: true,
            input_stats: true,
            self_commit: true,
            draft: DraftStep::Feed,
            expire_scope: true,
            touch_pair: true,
        },
        // 选的是候选，与键盘选词同为用户输入。
        ActionOrigin::Pointer | ActionOrigin::CapsLockHook => ExitSteps {
            commit_newline: false,
            input_stats: false,
            self_commit: true,
            draft: DraftStep::Feed,
            expire_scope: true,
            touch_pair: true,
        },
        // 移动端按键直调内层 `handle_key_event`，按键上屏不经本出口、不喂滑窗；选词这一侧
        // 再喂，滑窗会把「点选的你 / 按键上屏的好 / 点选的吗」看成相邻的「你吗」。
        ActionOrigin::MobilePick => ExitSteps {
            commit_newline: false,
            input_stats: false,
            self_commit: true,
            draft: DraftStep::Skip,
            expire_scope: true,
            touch_pair: true,
        },
        // 上屏的是提示里的一段文本，不是输入：打点（撤销删的是它、光标回声不误判），
        // 但不进造词滑窗。
        ActionOrigin::Tooltip => ExitSteps {
            commit_newline: false,
            input_stats: false,
            self_commit: true,
            draft: DraftStep::Break,
            expire_scope: true,
            touch_pair: true,
        },
    }
}

impl Coordinator {
    /// 会话动作的平台中立收尾。
    ///
    /// 接过调用方持有的状态锁：中英补空格须与产出本动作的那段状态读写在**同一把锁**内（鼠标
    /// 点选是在选词的同一临界区里补的），补完即放锁——后面几步各自取锁。
    pub(crate) fn finish_session_action(
        &self,
        mut st: MutexGuard<'_, State>,
        origin: ActionOrigin,
        action: KeyAction,
    ) -> KeyAction {
        let action = self.apply_auto_spacing(&mut st, action);
        drop(st);
        let steps = exit_steps(origin);
        // 换行改写在补空格之后：空格是上屏文本的前缀，与换行形式无关。
        let action = if steps.commit_newline {
            self.apply_commit_newline(action)
        } else {
            action
        };
        if steps.input_stats {
            self.record_input_stats(&action);
        }
        if steps.self_commit {
            self.note_self_commit(&action);
        }
        // 滑窗吃一切真落屏的文字（与自提交打点同源的五种变体）。
        match steps.draft {
            DraftStep::Feed => self.note_draft_commit(&action),
            DraftStep::Break => {
                self.terminate_auto_phrase("非输入文本上屏");
                self.note_word_boundary();
                // 补空格置的「前缀是补的」标记原由 `note_draft_commit` 消费；这里不喂滑窗，
                // 不清的话会留给下一笔，把它开头真实的空格当成补的剥掉。
                self.auto_space_prefixed
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }
            DraftStep::Skip => {
                self.auto_space_prefixed
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                // 选词出口登记的上屏码同理作废，不留给以后的字。
                self.draft_window
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear_code_hint();
            }
        }
        if steps.expire_scope {
            self.expire_scope_override();
        }
        if steps.touch_pair {
            self.touch_pair_state();
        }
        action
    }
}

#[cfg(test)]
mod tests {
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
}
