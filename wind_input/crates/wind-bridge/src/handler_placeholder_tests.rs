use super::*;

/// 编码串类组合必须换占位空格：真实编码泄漏进宿主 composition 后，独立编码栏
/// （preedit_display = candidate_top）下会与候选窗 preedit 重复显示。
#[test]
fn placeholder_replaces_code_compositions() {
    let cases: Vec<(&str, KeyAction)> = vec![
        (
            "UpdateComposition",
            KeyAction::UpdateComposition {
                text: "skce".into(),
                caret_pos: 4,
            },
        ),
        (
            "InsertText",
            KeyAction::InsertText {
                text: "可能".into(),
                new_composition: Some("h".into()),
                mode_changed: false,
                chinese_mode: true,
                has_new_composition: true,
            },
        ),
        (
            // direct_commit 顶码：曾漏掉本变体，余码编码直落宿主（真机复现于
            // 「skce 顶码后快打 h」，h 被嵌进宿主而非占位空格）。
            "CommitThenDeferComposition",
            KeyAction::CommitThenDeferComposition {
                commit_text: "可能".into(),
                deferred_composition: "h".into(),
                timeout_ms: 150,
            },
        ),
    ];
    // 各档占位字符都要原样落到组合串上（GH#175：浏览器按 compat 规则用 ZWSP，钉钉表格类用 U+2800）。
    for placeholder in [COMPOSITION_PLACEHOLDER, "\u{200B}", "\u{2800}"] {
        for (name, action) in cases.clone() {
            let composition = match action.with_composition_placeholder(placeholder) {
                KeyAction::UpdateComposition { text, caret_pos } => {
                    assert_eq!(caret_pos, 0, "{name}: 占位后光标须置前");
                    text
                }
                KeyAction::InsertText {
                    new_composition, ..
                } => new_composition.expect("组合串不应消失"),
                KeyAction::CommitThenDeferComposition {
                    commit_text,
                    deferred_composition,
                    ..
                } => {
                    assert_eq!(commit_text, "可能", "{name}: 已承诺上屏的正文不得被改写");
                    deferred_composition
                }
                other => panic!("{name}: 变体不应改变，实际 {other:?}"),
            };
            assert_eq!(
                composition, placeholder,
                "{name}: 组合串应换成给定的占位字符"
            );
        }
    }
}

/// 正文类组合刻意不换：HoldComposition 系列的组合内容是中文符号本身，
/// 换成空格会把符号弄丢。守卫此边界，防止后来者"顺手补全所有变体"。
#[test]
fn placeholder_keeps_literal_symbol_compositions() {
    let held = KeyAction::HoldComposition {
        text: "。".into(),
        timeout_ms: 500,
    };
    match held.with_composition_placeholder(COMPOSITION_PLACEHOLDER) {
        KeyAction::HoldComposition { text, .. } => assert_eq!(text, "。"),
        other => panic!("HoldComposition 不应被改写，实际 {other:?}"),
    }

    let commit_hold = KeyAction::CommitAndHoldComposition {
        commit_text: "可能".into(),
        hold_text: "。".into(),
        timeout_ms: 500,
    };
    match commit_hold.with_composition_placeholder(COMPOSITION_PLACEHOLDER) {
        KeyAction::CommitAndHoldComposition {
            commit_text,
            hold_text,
            ..
        } => {
            assert_eq!(commit_text, "可能");
            assert_eq!(hold_text, "。");
        }
        other => panic!("CommitAndHoldComposition 不应被改写，实际 {other:?}"),
    }
}

/// 空组合串（无余码/无新组合）不得被塞进占位空格，否则宿主留下一个空组合。
#[test]
fn placeholder_skips_empty_compositions() {
    let empty_defer = KeyAction::CommitThenDeferComposition {
        commit_text: "可能".into(),
        deferred_composition: String::new(),
        timeout_ms: 150,
    };
    match empty_defer.with_composition_placeholder(COMPOSITION_PLACEHOLDER) {
        KeyAction::CommitThenDeferComposition {
            deferred_composition,
            ..
        } => assert!(deferred_composition.is_empty()),
        other => panic!("空组合串不应被改写，实际 {other:?}"),
    }
}
