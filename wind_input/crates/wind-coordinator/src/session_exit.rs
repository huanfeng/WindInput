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
    /// 鼠标点选候选（含移动端按下标选词）。
    Pointer,
    /// CapsLock 全局钩子（事件不经 TSF）。
    CapsLockHook,
    /// 悬停提示菜单的「上屏」。
    Tooltip,
}

/// 某来源接哪几步收尾。中英补空格各来源都接，不在表内。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExitSteps {
    /// 上屏换行改写。非按键来源经 push 通道投递，在编码时改写（`push_no_key_ctx_action`）。
    /// 这里若也做，鼠标点选返回给移动端与 `debug_mouse_select` 的动作会变（改写本身幂等）。
    pub commit_newline: bool,
    /// 顶层输入统计兜底（`record_input_stats`）。
    pub input_stats: bool,
    /// 自提交打点 + 码表造词投喂（`note_commit_action`）。
    pub note_commit: bool,
    /// 检索范围临时放宽在组合结束时复原（`expire_scope_override`）。
    pub expire_scope: bool,
    /// 配对状态保活（`touch_pair_state`）。
    pub touch_pair: bool,
}

/// 各来源的收尾表。**逐字复现现状**：非按键来源的 `false` 是否属漏接，见落点指引 §4.3 待定项，
/// 定案一项改一项，并登记候选行为变更清单。
pub(crate) const fn exit_steps(origin: ActionOrigin) -> ExitSteps {
    match origin {
        ActionOrigin::Key => ExitSteps {
            commit_newline: true,
            input_stats: true,
            note_commit: true,
            expire_scope: true,
            touch_pair: true,
        },
        ActionOrigin::Pointer | ActionOrigin::CapsLockHook | ActionOrigin::Tooltip => ExitSteps {
            commit_newline: false,
            input_stats: false,
            note_commit: false,
            expire_scope: false,
            touch_pair: false,
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
        if steps.note_commit {
            self.note_commit_action(&action);
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
    /// 收尾各步只经 [`super::Coordinator::finish_session_action`] 调用：新上屏入口若绕过它直接调
    /// 其中一步，就又回到「各入口凭记忆抄几步」的老路，§4.3 那张表也不再反映实情。
    #[test]
    fn exit_steps_are_only_called_from_finish_session_action() {
        const STEPS: &[&str] = &[
            "apply_auto_spacing(",
            "apply_commit_newline(",
            "record_input_stats(",
            "note_commit_action(",
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
