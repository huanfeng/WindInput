use super::*;

fn lf() -> char {
    char::from_u32(10).unwrap()
}
fn cr() -> char {
    char::from_u32(13).unwrap()
}

fn insert(text: &str, composition: Option<&str>) -> KeyAction {
    KeyAction::InsertText {
        text: text.into(),
        new_composition: composition.map(|c| c.into()),
        mode_changed: false,
        chinese_mode: true,
        has_new_composition: composition.is_some(),
    }
}

fn text_of(a: &KeyAction) -> String {
    match a {
        KeyAction::InsertText { text, .. }
        | KeyAction::InsertTextWithCursor { text, .. }
        | KeyAction::ReplaceBackward { text, .. }
        | KeyAction::CommitReplacingHeld { text, .. }
        | KeyAction::HoldComposition { text, .. }
        | KeyAction::UpdateComposition { text, .. } => text.clone(),
        KeyAction::CommitAndHoldComposition { commit_text, .. }
        | KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text.clone(),
        other => panic!("没有上屏文本的 action: {other:?}"),
    }
}

/// `Keep` 必须是彻底的 no-op——这是出厂档，绝大多数用户走这条路。
#[test]
fn keep_is_a_noop() {
    let src = format!("一{}二", lf());
    let out = apply_newline_style(insert(&src, None), NewlineStyle::Keep);
    assert_eq!(text_of(&out), src);
}

/// 每一个「最终写入宿主文档」的字段都要被覆盖到。
///
/// 漏掉任何一个的症状都是「换行有时对有时不对」——取决于用户走的是哪条上屏路径，
/// 是本特性最难查的失败形态，故逐个变体钉住。
#[test]
fn every_commit_text_field_is_converted() {
    let src = format!("一{}二", lf());
    let want = format!("一{}二", cr());

    let cases: Vec<KeyAction> = vec![
        insert(&src, None),
        KeyAction::InsertTextWithCursor {
            text: src.clone(),
            cursor_offset: 1,
        },
        KeyAction::ReplaceBackward {
            count: 2,
            text: src.clone(),
        },
        KeyAction::CommitReplacingHeld {
            text: src.clone(),
            chinese_mode: true,
        },
        KeyAction::CommitAndHoldComposition {
            commit_text: src.clone(),
            hold_text: src.clone(),
            timeout_ms: 500,
        },
        KeyAction::CommitThenDeferComposition {
            commit_text: src.clone(),
            deferred_composition: src.clone(),
            timeout_ms: 500,
        },
    ];

    for case in cases {
        let label = format!("{case:?}");
        let out = apply_newline_style(case, NewlineStyle::Cr);
        assert_eq!(text_of(&out), want, "未转换上屏文本: {label}");
    }
}

/// ★★★ 组合区文本**一律不动**。
///
/// 它们是还在组合里的编码或预览，不是写进文档的正文。改了不会让上屏更对，只会让
/// 编码栏显示错乱——而且那种错乱只在带换行的词条上出现，极难复现。
#[test]
fn composition_text_is_never_touched() {
    let src = format!("一{}二", lf());

    // InsertText 的 new_composition
    let out = apply_newline_style(insert(&src, Some(&src)), NewlineStyle::Cr);
    match out {
        KeyAction::InsertText {
            new_composition, ..
        } => assert_eq!(new_composition.unwrap(), src, "new_composition 被改写了"),
        other => panic!("{other:?}"),
    }

    // CommitAndHoldComposition 的 hold_text
    let out = apply_newline_style(
        KeyAction::CommitAndHoldComposition {
            commit_text: src.clone(),
            hold_text: src.clone(),
            timeout_ms: 500,
        },
        NewlineStyle::Cr,
    );
    match out {
        KeyAction::CommitAndHoldComposition { hold_text, .. } => {
            assert_eq!(hold_text, src, "hold_text 被改写了")
        }
        other => panic!("{other:?}"),
    }

    // CommitThenDeferComposition 的 deferred_composition
    let out = apply_newline_style(
        KeyAction::CommitThenDeferComposition {
            commit_text: src.clone(),
            deferred_composition: src.clone(),
            timeout_ms: 500,
        },
        NewlineStyle::Cr,
    );
    match out {
        KeyAction::CommitThenDeferComposition {
            deferred_composition,
            ..
        } => assert_eq!(deferred_composition, src, "deferred_composition 被改写了"),
        other => panic!("{other:?}"),
    }

    // 纯组合区的两个变体整体不动
    for case in [
        KeyAction::UpdateComposition {
            text: src.clone(),
            caret_pos: 0,
        },
        KeyAction::HoldComposition {
            text: src.clone(),
            timeout_ms: 500,
        },
    ] {
        let out = apply_newline_style(case, NewlineStyle::Cr);
        assert_eq!(text_of(&out), src, "组合区 action 被改写了");
    }
}

/// 不带文本的 action 原样穿过，不因为加了这一层而改变。
#[test]
fn textless_actions_pass_through() {
    for case in [
        KeyAction::ClearComposition,
        KeyAction::PassThrough,
        KeyAction::Consumed,
        KeyAction::NotHandled,
        KeyAction::DeletePair,
        KeyAction::MoveCursorRight { count: 1 },
    ] {
        let label = format!("{case:?}");
        let out = apply_newline_style(case, NewlineStyle::Crlf);
        assert_eq!(format!("{out:?}"), label);
    }
}

/// ★★★ 短路判据的字段集必须与改写函数**完全一致**。
///
/// `apply_commit_newline` 先用 `commit_text_has_newline` 决定要不要查档位。判据里少看
/// 一个字段，那个字段就永远等不到改写——而且是静默的：改写函数本身仍然正确，测它也是
/// 绿的，只是那条路再也走不到。故两边逐个变体对齐着测。
#[test]
fn newline_detector_covers_exactly_the_converted_fields() {
    let src = format!("一{}二", lf());
    let plain = "一二";

    let with_newline: Vec<KeyAction> = vec![
        insert(&src, None),
        KeyAction::InsertTextWithCursor {
            text: src.clone(),
            cursor_offset: 1,
        },
        KeyAction::ReplaceBackward {
            count: 1,
            text: src.clone(),
        },
        KeyAction::CommitReplacingHeld {
            text: src.clone(),
            chinese_mode: true,
        },
        KeyAction::CommitAndHoldComposition {
            commit_text: src.clone(),
            hold_text: plain.into(),
            timeout_ms: 0,
        },
        KeyAction::CommitThenDeferComposition {
            commit_text: src.clone(),
            deferred_composition: plain.into(),
            timeout_ms: 0,
        },
    ];
    for case in with_newline {
        let label = format!("{case:?}");
        assert!(
            commit_text_has_newline(&case),
            "带换行正文却没被判据识别，这个字段将永远漏改: {label}"
        );
        // 判据说有，改写就必须真的改到。
        let out = apply_newline_style(case, NewlineStyle::Cr);
        assert!(text_of(&out).contains(cr()), "判据与改写不一致: {label}");
    }

    // 组合区带换行**不算**——它们不参与改写，判据也不该为它们唤起查表。
    for case in [
        KeyAction::UpdateComposition {
            text: src.clone(),
            caret_pos: 0,
        },
        KeyAction::HoldComposition {
            text: src.clone(),
            timeout_ms: 0,
        },
        KeyAction::CommitAndHoldComposition {
            commit_text: plain.into(),
            hold_text: src.clone(),
            timeout_ms: 0,
        },
        KeyAction::CommitThenDeferComposition {
            commit_text: plain.into(),
            deferred_composition: src.clone(),
            timeout_ms: 0,
        },
        KeyAction::PassThrough,
        KeyAction::Consumed,
    ] {
        let label = format!("{case:?}");
        assert!(!commit_text_has_newline(&case), "不该唤起查表: {label}");
    }
}

/// `cursor_offset` 是用户配的**格数**，不随文本长度变化——CRLF 让文本变长也不能动它。
#[test]
fn cursor_offset_survives_length_change() {
    let src = format!("（{}）", lf());
    let out = apply_newline_style(
        KeyAction::InsertTextWithCursor {
            text: src,
            cursor_offset: 1,
        },
        NewlineStyle::Crlf,
    );
    match out {
        KeyAction::InsertTextWithCursor { cursor_offset, .. } => {
            assert_eq!(cursor_offset, 1)
        }
        other => panic!("{other:?}"),
    }
}
