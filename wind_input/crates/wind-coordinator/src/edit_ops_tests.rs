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
