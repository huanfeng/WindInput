//! `KeyAction` → 宿主无关编辑指令流（[`wind_host::EditOp`]）。
//!
//! # 为什么要这层映射
//!
//! `KeyAction` 是**按 TSF 的编排方式**长出来的，一个枚举里混着三类东西：
//!
//! | 类别 | 例子 | 薄宿主能否直接用 |
//! |------|------|------------------|
//! | 编辑意图 | `InsertText` / `ReplaceBackward` / `MoveCursorRight` | 能 |
//! | TSF 时序编排 | `HoldComposition`（宿主起超时定时器）、`CommitThenDeferComposition`（等 keyup 才开新组合） | 不能，但可降级 |
//! | 状态通知 | `StatusUpdate` | 不是编辑 |
//!
//! Android 此前的做法是 `match` 剩下的一律压成「已消费、无输出」——于是**智能符号配对、
//! 回退替换、配对跳出在移动端静默失效**：不报错、不丢键，只是功能没有。
//! `InputConnection` 完全做得到这些，是类型没把语义带过来。
//!
//! # 关键约束：`match` 必须穷尽
//!
//! 本文件**不得出现 `_ =>` 兜底**。新增 `KeyAction` 变体时编译器会在这里报错，
//! 强制作者回答「这个动作在薄宿主上等价于什么」。此前 Android 侧那个 `_ =>`
//! 正是「新功能上了桌面、移动端悄无声息地没有」的制度性来源。

use wind_bridge::handler::KeyAction;
use wind_host::{EditOp, KeyOutcome, TimingHint};

/// 把主输入路的返回值翻译成宿主无关的编辑指令流。
///
/// TSF 编排降级为 [`KeyOutcome::hint`]，且 [`KeyOutcome::ops`] 里**已经给出忽略编排时的
/// 等价序列**——薄宿主直接执行 ops 即可得到正确（只是少了时序讲究）的行为。
pub fn to_outcome(action: KeyAction) -> KeyOutcome {
    match action {
        // ── 不消费 ──
        KeyAction::NotHandled | KeyAction::PassThrough => KeyOutcome::passthrough(),

        // ── 消费但无文本变更 ──
        KeyAction::Consumed => KeyOutcome::consumed_silently(),
        KeyAction::StatusUpdate(_) => KeyOutcome {
            consumed: true,
            mode_changed: true,
            ..KeyOutcome::default()
        },

        // ── 组合区 ──
        KeyAction::UpdateComposition { text, caret_pos } => {
            consumed(vec![EditOp::SetComposition {
                text,
                caret: caret_pos as usize,
            }])
        }
        KeyAction::ClearComposition => consumed(vec![EditOp::SetComposition {
            text: String::new(),
            caret: 0,
        }]),

        // 清组合 + 把键交还宿主：`consumed == false` 但 `ops` 非空（清掉联想的占位组合）。
        // 宿主按序执行 ops、再对这一键走系统默认行为（退格删字、回车换行/发送）。
        //
        // 此前这里降级成吃键，于是移动端的联想态退格、回车要按两下才生效，
        // `[mobile.association]` 里放行它们的开关形同虚设。Android 的 `RustSession.dispatch`
        // 不看 `consumed`、有 ops 就执行，故这一形状可表达。
        KeyAction::ClearCompositionThenPassThrough => KeyOutcome {
            consumed: false,
            ops: vec![EditOp::SetComposition {
                text: String::new(),
                caret: 0,
            }],
            hint: None,
            mode_changed: false,
        },

        // ── 上屏（可带新组合）──
        KeyAction::InsertText {
            text,
            new_composition,
            mode_changed,
            ..
        } => {
            let mut ops = Vec::new();
            if !text.is_empty() {
                ops.push(EditOp::Commit(text));
            }
            // `None` 与 `Some("")` 语义不同：前者「不动组合区」，后者「清空组合区」。
            // 合并这两者会让上屏后残留旧编码。
            if let Some(comp) = new_composition {
                ops.push(EditOp::SetComposition {
                    caret: comp.chars().count(),
                    text: comp,
                });
            }
            KeyOutcome {
                consumed: true,
                ops,
                hint: None,
                mode_changed,
            }
        }

        // 上屏后把光标往回挪（cursor_offset 是相对文本末尾的**回退**字符数）
        KeyAction::InsertTextWithCursor {
            text,
            cursor_offset,
        } => {
            let mut ops = vec![EditOp::Commit(text)];
            if cursor_offset > 0 {
                ops.push(EditOp::MoveCursor {
                    delta: -(cursor_offset as i32),
                });
            }
            consumed(ops)
        }

        // ── 配对/智能符号 ──
        KeyAction::MoveCursorRight { count } => consumed(vec![EditOp::MoveCursor {
            delta: count as i32,
        }]),

        // 智能删除配对：删掉光标两侧各一个字符。先右后左——先删左侧会让右侧的相对
        // 位置发生偏移，宿主再按原偏移删就删错了。
        KeyAction::DeletePair => consumed(vec![
            EditOp::MoveCursor { delta: 1 },
            EditOp::DeleteBackward { count: 1 },
            EditOp::DeleteBackward { count: 1 },
        ]),

        KeyAction::ReplaceBackward { count, text } => consumed(vec![EditOp::ReplaceBackward {
            count: count as usize,
            text,
        }]),

        // ── TSF 时序编排：给出等价降级序列 + hint ──
        //
        // hold：TSF 把 text 放进组合区，timeout 后自动提交；press2 到来则替换。
        // 薄宿主降级为「直接上屏」——少了那个「再按一下换成英文符号」的窗口，
        // 但文本结果一致，不会出现字符丢失或重复。
        KeyAction::HoldComposition { text, timeout_ms } => KeyOutcome {
            consumed: true,
            ops: vec![EditOp::Commit(text)],
            hint: Some(TimingHint::AutoCommitAfter { timeout_ms }),
            mode_changed: false,
        },

        // press2：把 hold 住的中文符号换成英文的。降级为「回退一个字符再插入」——
        // 这正是 hold 的语义在无 hold 能力宿主上的等价形式。
        KeyAction::CommitReplacingHeld { text, .. } => {
            consumed(vec![EditOp::ReplaceBackward { count: 1, text }])
        }

        KeyAction::CommitAndHoldComposition {
            commit_text,
            hold_text,
            timeout_ms,
        } => {
            let mut ops = Vec::new();
            if !commit_text.is_empty() {
                ops.push(EditOp::Commit(commit_text));
            }
            ops.push(EditOp::Commit(hold_text));
            KeyOutcome {
                consumed: true,
                ops,
                hint: Some(TimingHint::AutoCommitAfter { timeout_ms }),
                mode_changed: false,
            }
        }

        // 顶码 direct_commit：先提交顶出的文本，余码进新组合。TSF 要求把开新组合推迟到
        // keyup 以躲开 diff 式宿主的整锁合并；薄宿主直接建立即可（Android 的
        // InputConnection 没有那个合并问题）。
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            timeout_ms,
        } => {
            let mut ops = Vec::new();
            if !commit_text.is_empty() {
                ops.push(EditOp::Commit(commit_text));
            }
            ops.push(EditOp::SetComposition {
                caret: deferred_composition.chars().count(),
                text: deferred_composition,
            });
            KeyOutcome {
                consumed: true,
                ops,
                hint: Some(TimingHint::DeferCompositionUntilKeyUp { timeout_ms }),
                mode_changed: false,
            }
        }
    }
}

fn consumed(ops: Vec<EditOp>) -> KeyOutcome {
    KeyOutcome {
        consumed: true,
        ops,
        hint: None,
        mode_changed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_handled_is_passthrough() {
        let o = to_outcome(KeyAction::NotHandled);
        assert!(!o.consumed);
        assert!(o.ops.is_empty(), "不消费时不得带编辑指令");
    }

    #[test]
    fn insert_with_composition_emits_both() {
        let o = to_outcome(KeyAction::InsertText {
            text: "式".into(),
            new_composition: Some("aa".into()),
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: true,
        });
        assert_eq!(
            o.ops,
            vec![
                EditOp::Commit("式".into()),
                EditOp::SetComposition {
                    text: "aa".into(),
                    caret: 2
                },
            ],
        );
    }

    /// `None`（不动组合区）与 `Some("")`（清空组合区）不可合并
    #[test]
    fn none_composition_differs_from_empty() {
        let none = to_outcome(KeyAction::InsertText {
            text: "式".into(),
            new_composition: None,
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: false,
        });
        assert_eq!(none.ops, vec![EditOp::Commit("式".into())]);

        let empty = to_outcome(KeyAction::InsertText {
            text: "式".into(),
            new_composition: Some(String::new()),
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: true,
        });
        assert_eq!(
            empty.ops.last(),
            Some(&EditOp::SetComposition {
                text: String::new(),
                caret: 0
            }),
            "Some(\"\") 必须产生清空组合区的指令",
        );
    }

    /// 这些动作此前在 Android 上被 `_ =>` 吞掉，功能静默消失
    #[test]
    fn previously_dropped_actions_now_carry_semantics() {
        for (name, action) in [
            ("配对跳出", KeyAction::MoveCursorRight { count: 1 }),
            ("智能删除配对", KeyAction::DeletePair),
            (
                "智能符号替换",
                KeyAction::ReplaceBackward {
                    count: 1,
                    text: ".".into(),
                },
            ),
            (
                "hold 后 press2",
                KeyAction::CommitReplacingHeld {
                    text: ".".into(),
                    chinese_mode: false,
                },
            ),
        ] {
            let o = to_outcome(action);
            assert!(o.consumed, "{name} 应被消费");
            assert!(!o.ops.is_empty(), "{name} 必须产出编辑指令，不能被静默吞掉");
        }
    }

    /// hold 类降级后文本结果不能丢
    #[test]
    fn timing_hints_degrade_without_losing_text() {
        let o = to_outcome(KeyAction::HoldComposition {
            text: "。".into(),
            timeout_ms: 300,
        });
        assert_eq!(o.ops, vec![EditOp::Commit("。".into())]);
        assert_eq!(
            o.hint,
            Some(TimingHint::AutoCommitAfter { timeout_ms: 300 })
        );

        let o = to_outcome(KeyAction::CommitThenDeferComposition {
            commit_text: "式".into(),
            deferred_composition: "b".into(),
            timeout_ms: 50,
        });
        assert_eq!(
            o.ops,
            vec![
                EditOp::Commit("式".into()),
                EditOp::SetComposition {
                    text: "b".into(),
                    caret: 1
                },
            ],
        );
    }

    /// 清组合后把键交还宿主：不消费，但必须带清组合的指令，否则联想的占位组合会悬在宿主里
    #[test]
    fn clear_then_pass_through_clears_but_does_not_consume() {
        let o = to_outcome(KeyAction::ClearCompositionThenPassThrough);
        assert!(!o.consumed, "键要交还宿主");
        assert_eq!(
            o.ops,
            vec![EditOp::SetComposition {
                text: String::new(),
                caret: 0
            }]
        );
    }
}
