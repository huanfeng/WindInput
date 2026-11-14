//! 会话动作的统一收尾（落点指引 P6，见 `docs/architecture/feature-placement-guide.md` §4.3 / §5.2）。
//!
//! 上屏 / 组合动作的来源见 [`ActionOrigin`]；直接推文本的命令栏、软键盘点击、配对符号经
//! [`Coordinator::push_commit_text_from`] 进来。切方案时上屏与 DLL 排水不在表内，见落点指引 §4.3。
//! 各来源该接哪几步收尾写在 [`exit_steps`] 一张表里——新增来源时编译器会逼着在表里填一行，
//! 而不是在新入口处凭记忆抄几步。
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
    /// 命令输出：`$CC` 命令的 `type()`、工具栏按钮 / `command:` 绑定、mac / Linux 粘贴短语、
    /// `ime.pair` 配对符号。
    Command,
    /// 鼠标点软键盘键帽（物理键敲软键盘走 `Key`）。
    SoftKeyboardClick,
}

/// 草稿滑窗（码表自动造词）对一次上屏的处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftStep {
    /// 喂入：上屏文字是用户的输入，可与前后上屏连成词。
    Feed,
    /// 打断：上屏的不是输入（提示文本、命令输出），滑窗就此断开，同时标记词边界。
    Break,
    /// 不碰：该来源所在平台的按键上屏不经本出口、不喂滑窗，只在选词这一侧喂会把不相邻的
    /// 两次上屏拼成假词。
    Skip,
}

/// 某来源接哪几步收尾。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExitSteps {
    /// 中英交界补空格（`apply_auto_spacing`）。
    pub auto_spacing: bool,
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
    /// 收掉无占位联想：插进来的不是联想的下文。Windows 上此前靠光标回声被当成「用户挪光标」
    /// 顺带收掉，打了自提交点后回声被忽略，要在这里显式收。按键 / 选词来源由各自的分派处理联想。
    pub exit_bare_assoc: bool,
}

/// 各来源的收尾表（落点指引 §4.3）。
///
/// 非按键来源不接的两步：
/// - 换行改写：推送编码时改写（见 [`ExitSteps::commit_newline`]）。
/// - 输入统计兜底：候选类来源的上屏出口自己 `record_commit`，兜底只补漏记的 InsertText，收益小
///   （2026-11-13 定）。软键盘点击例外：它与物理键敲软键盘对齐，物理键靠兜底记账。
pub(crate) const fn exit_steps(origin: ActionOrigin) -> ExitSteps {
    match origin {
        ActionOrigin::Key => ExitSteps {
            auto_spacing: true,
            commit_newline: true,
            input_stats: true,
            self_commit: true,
            draft: DraftStep::Feed,
            expire_scope: true,
            touch_pair: true,
            exit_bare_assoc: false,
        },
        // 选的是候选，与键盘选词同为用户输入。
        ActionOrigin::Pointer | ActionOrigin::CapsLockHook => ExitSteps {
            auto_spacing: true,
            commit_newline: false,
            input_stats: false,
            self_commit: true,
            draft: DraftStep::Feed,
            expire_scope: true,
            touch_pair: true,
            exit_bare_assoc: false,
        },
        // 移动端按键直调内层 `handle_key_event`，按键上屏不经本出口、不喂滑窗；选词这一侧
        // 再喂，滑窗会把「点选的你 / 按键上屏的好 / 点选的吗」看成相邻的「你吗」。
        ActionOrigin::MobilePick => ExitSteps {
            auto_spacing: true,
            commit_newline: false,
            input_stats: false,
            self_commit: true,
            draft: DraftStep::Skip,
            expire_scope: true,
            touch_pair: true,
            exit_bare_assoc: false,
        },
        // 上屏的是提示里的一段文本，不是输入：打点（撤销删的是它、光标回声不误判），
        // 但不进造词滑窗。
        ActionOrigin::Tooltip => ExitSteps {
            auto_spacing: true,
            commit_newline: false,
            input_stats: false,
            self_commit: true,
            draft: DraftStep::Break,
            expire_scope: true,
            touch_pair: true,
            exit_bare_assoc: true,
        },
        // 命令输出不是输入：打点（撤销删的是它；mac / Linux 没有光标回声，不打点撤销计数就是上一笔的）、
        // 打断滑窗。不补空格：维持现状，命令输出的格式由命令自己定；`spacing_prev` 因此不推进到
        // 输出末字，由下一次空闲按键按宿主上报的光标前一字重新锁存。
        // 换行在出口改写（打点要按改写后的长度计；推送时再改写一次是幂等的）。
        ActionOrigin::Command => ExitSteps {
            auto_spacing: false,
            commit_newline: true,
            input_stats: false,
            self_commit: true,
            draft: DraftStep::Break,
            expire_scope: true,
            touch_pair: true,
            exit_bare_assoc: true,
        },
        // 与物理键敲软键盘（`Key` 来源）对齐：补空格、记统计、喂滑窗。
        ActionOrigin::SoftKeyboardClick => ExitSteps {
            auto_spacing: true,
            commit_newline: true,
            input_stats: true,
            self_commit: true,
            draft: DraftStep::Feed,
            expire_scope: true,
            touch_pair: true,
            exit_bare_assoc: true,
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
        let steps = exit_steps(origin);
        let action = if steps.auto_spacing {
            self.apply_auto_spacing(&mut st, action)
        } else {
            action
        };
        drop(st);
        // 换行改写在补空格之后：空格是上屏文本的前缀，与换行形式无关。
        let action = if steps.commit_newline {
            self.apply_commit_newline(action)
        } else {
            action
        };
        if steps.input_stats {
            // 去重标志由按键入口（内层 `handle_key_event`）复位；非按键来源要在这里复位，否则沿用
            // 上一次按键留下的值，兜底被误跳过。
            if origin != ActionOrigin::Key {
                self.stat_recorded
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }
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
        if steps.exit_bare_assoc && self.assoc_placeholder_is_none() {
            let exited = {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                self.exit_assoc(&mut st, crate::handle_assoc::AssocExit::CaretMoved)
            };
            if exited {
                self.push_hotkey_session_if_changed();
                self.notify_ui_hide();
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

/// 扩展事件 `commit.source` 的取值：软键盘点击单列，其余非按键来源归 `command`。
fn hook_source(origin: ActionOrigin) -> &'static str {
    match origin {
        ActionOrigin::SoftKeyboardClick => "softkeyboard",
        _ => "command",
    }
}

impl Coordinator {
    /// 直接推文本上屏（命令栏、软键盘点击、配对退化为纯上屏）：先按 `origin` 收尾，再经 push
    /// 通道投递。调用方不得持有状态锁。返回实际上屏的 UTF-16 长度（命令栏多段输出要累加撤销计数）。
    pub(crate) fn push_commit_text_from(&self, origin: ActionOrigin, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        let act = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let chinese_mode = st.chinese_mode;
            self.finish_session_action(
                st,
                origin,
                Coordinator::commit_action(text.to_string(), chinese_mode),
            )
        };
        match act {
            KeyAction::InsertText { text, .. } => {
                self.push_commit_text_as(&text, hook_source(origin));
                text.encode_utf16().count()
            }
            other => {
                debug_assert!(false, "收尾不应改变上屏动作的变体：{other:?}");
                tracing::warn!("push_commit_text_from: 收尾改变了动作变体，放弃上屏 {other:?}");
                0
            }
        }
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
}
