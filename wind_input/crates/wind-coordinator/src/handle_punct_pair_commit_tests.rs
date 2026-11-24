use super::*;
use std::sync::Arc;
use wind_config::config::Config;

/// 中文模式 + 中文配对开。返回的协调器可直接调 `cmd_pair_commit`。
fn coord(chinese_pair: bool) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.auto_pair.chinese = chinese_pair;
    cfg.input.auto_pair.jump_out_keys = vec!["tab".into()];
    Coordinator::new_headless(cfg, None)
}

fn stack_top(c: &Coordinator) -> Option<(String, String, u32)> {
    let tr = c.pair_tracker.lock().unwrap_or_else(|e| e.into_inner());
    tr.peek()
        .map(|e| (e.left.clone(), e.right.clone(), e.jump_steps))
}

#[test]
fn punct_top_commit_pair_cursor_ignores_candidate_prefix() {
    for prefix in ["", "你", "你好", "𠮷你好"] {
        for (key_code, modifiers, pair) in [
            (keymap::VK_LBRACKET, 0, "【】"),
            (keymap::VK_9, MOD_SHIFT, "（）"),
        ] {
            let c = coord(true);
            if !prefix.is_empty() {
                c.state
                    .lock()
                    .unwrap()
                    .candidates
                    .push(wind_candidate::Candidate {
                        text: prefix.into(),
                        ..Default::default()
                    });
            }
            let act = c.handle_key_event(&KeyEventData {
                key_code,
                scan_code: 0,
                modifiers,
                event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
                toggles: 0,
                event_seq: 0,
                prev_char: 0,
                seq_char: 0,
            });
            match act {
                KeyAction::InsertTextWithCursor {
                    text,
                    cursor_offset,
                } => {
                    assert_eq!(text, format!("{prefix}{pair}"));
                    assert_eq!(cursor_offset, 1, "顶屏前缀不应影响配对回退格数");
                }
                other => panic!("应上屏候选及配对，实际: {other:?}"),
            }
        }
    }
}

/// 按一次 Tab（0x09），返回协调器裁决。
fn tab(c: &Coordinator) -> KeyAction {
    c.handle_key_event(&KeyEventData {
        key_code: 0x09,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    })
}

/// 多字符配对压栈后，跳出键应右移**声明的格数**而不是恒 1——
/// 恒 1 的话 `<!-- -->` 只跳出一格，光标停在 `--|>` 里面。
#[test]
fn multichar_pair_jumps_declared_steps() {
    let c = coord(true);
    c.cmd_pair_commit("<!--", "-->", 3);
    assert_eq!(
        stack_top(&c),
        Some(("<!--".into(), "-->".into(), 3)),
        "应压入多字符配对"
    );

    let act = tab(&c);
    assert!(
        matches!(act, KeyAction::MoveCursorRight { count: 3 }),
        "Tab 应按 jump_steps 右移 3 格，实际: {act:?}"
    );
    assert!(stack_top(&c).is_none(), "跳出后应弹栈");
}

/// 关掉 `auto_pair` 时退化为纯上屏：不压栈，Tab 不该被吞。
/// 这条同时锁住 C++ `_pairPendingDepth` 的同步前提——那侧只在带光标偏移的推送上记账。
#[test]
fn pair_disabled_degrades_to_plain_commit() {
    let c = coord(false);
    c.cmd_pair_commit("《", "》", 1);
    assert!(stack_top(&c).is_none(), "配对关闭时不该压栈");

    let act = tab(&c);
    assert!(
        !matches!(act, KeyAction::MoveCursorRight { .. }),
        "栈空时 Tab 应透传给宿主，实际: {act:?}"
    );
}

/// 右段为空 = 没有要越过的内容，压栈只会让下一次 Tab 空跑一格。
#[test]
fn empty_right_does_not_push() {
    let c = coord(true);
    c.cmd_pair_commit("前缀", "", 0);
    assert!(stack_top(&c).is_none(), "右段为空不该压栈");
}

/// 分级跳出：同一词条里写两条 `ime.pair`，内层先跳出、外层后跳出，
/// 且各自用自己的格数。栈退化成队列的话这条会反过来。
#[test]
fn nested_pairs_jump_inner_first() {
    let c = coord(true);
    c.cmd_pair_commit("（", "）", 1);
    c.cmd_pair_commit("【【", "】】", 2);

    assert!(
        matches!(tab(&c), KeyAction::MoveCursorRight { count: 2 }),
        "第一次 Tab 应跳出内层（2 格）"
    );
    assert!(
        matches!(tab(&c), KeyAction::MoveCursorRight { count: 1 }),
        "第二次 Tab 应跳出外层（1 格）"
    );
    assert!(stack_top(&c).is_none());
}
