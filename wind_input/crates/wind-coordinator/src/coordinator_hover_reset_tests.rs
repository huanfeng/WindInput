//! 鼠标悬停目标（`Coordinator::hover_index`）的清空覆盖面。
//!
//! 悬停残留会让候选窗再次弹出时鼠标没动却已有一项高亮并弹出 tooltip。
//!
//! 主路径上不可观测：普通输入每键都重走 `update_candidates`，残留被覆盖掉。所以只测
//! 普通输入等于没测，下面必须逐个 overlay 入口（特殊模式 / 临拼 / 临英 / 混输·快捷输入 /
//! 拼音组合复位）点名。
use super::*;

fn coord() -> Arc<Coordinator> {
    Coordinator::new_headless(Config::default(), None)
}

/// 造一页候选，好让 `mouse_hover` 有合法落点（它对空候选另有分支，见下面的专项测试）。
fn seed_candidates(c: &Coordinator, n: usize) {
    let mut st = c.state.lock().unwrap();
    st.candidates = (0..n)
        .map(|i| wind_candidate::Candidate {
            text: i.to_string(),
            ..Default::default()
        })
        .collect();
}

/// 反向对照：悬停确实设得上。
///
/// 少了本条，下面所有「××之后归零」都可能因悬停压根没设上而假绿。
#[test]
fn mouse_hover_sets_target() {
    let c = coord();
    seed_candidates(&c, 5);
    c.mouse_hover(2);
    assert_eq!(c.hover_target(), 2, "有候选时悬停应设得上");
}

/// 候选窗隐藏 = 会话终结，悬停必须归零。
///
/// 清空放在 `notify_ui_hide` 里，所有隐藏通路都覆盖到。UI 侧 `CandidateMouse::reset_hover`
/// 清的是决定何时发事件的防抖闸门，与本值不是一回事。
#[test]
fn notify_ui_hide_clears_hover() {
    let c = coord();
    seed_candidates(&c, 5);
    c.mouse_hover(2);
    c.notify_ui_hide();
    assert_eq!(c.hover_target(), -1, "候选窗隐藏后悬停必须归零");
}

/// 每一个 overlay 候选装填入口，装填后都必须已清除悬停。
///
/// 逐个点名而不是抽样：它们是平行的独立落点。新增候选来源忘了 `reset_candidate_view`
/// 本测试覆盖不到，需在此补一个入口。
#[test]
fn every_overlay_refill_clears_hover() {
    // (入口名, 调用) —— 名字进断言消息，失败时直接指出是哪条路径漏了。
    type RefillCase = (&'static str, fn(&Coordinator, &mut State));
    let cases: Vec<RefillCase> = vec![
        ("特殊模式 update_special_candidates", |c, st| {
            let _ = c.update_special_candidates(st);
        }),
        ("临时拼音 update_temp_pinyin_candidates", |c, st| {
            c.update_temp_pinyin_candidates(st)
        }),
        ("临时英文 update_temp_english_candidates", |c, st| {
            c.update_temp_english_candidates(st)
        }),
        ("混输·快捷输入 update_mix_candidates", |c, st| {
            c.update_mix_candidates(st)
        }),
        ("拼音组合复位 reset_pinyin_composition", |c, st| {
            c.reset_pinyin_composition(st)
        }),
    ];
    for (name, refill) in cases {
        let c = coord();
        seed_candidates(&c, 5);
        c.mouse_hover(2);
        assert_eq!(c.hover_target(), 2, "{name}：前置条件——悬停应已设上");

        let mut st = c.state.lock().unwrap();
        refill(&c, &mut st);
        assert_eq!(c.hover_target(), -1, "{name}：候选重新装填后悬停必须清除");
    }
}

/// 「鼠标移出候选窗」这条 `Hover(-1)` 在候选恰好已清空时不能被吞掉。
///
/// `mouse_hover` 不能对空候选早退：候选没了正是最该归零的时刻。
#[test]
fn leaving_clears_hover_even_when_candidates_already_empty() {
    let c = coord();
    seed_candidates(&c, 5);
    c.mouse_hover(2);
    c.state.lock().unwrap().candidates.clear();

    c.mouse_hover(-1);
    assert_eq!(c.hover_target(), -1, "候选已空时的离开事件不能被吞掉");
}

/// 键盘操作（移动高亮 / 翻页）同样取消悬停：两种高亮并存时视觉上会有两个「选中项」。
/// 此前这四处是仅有的清空点之一，改造成 `clear_hover` 后需确认语义没丢。
#[test]
fn keyboard_navigation_clears_hover() {
    let c = coord();
    seed_candidates(&c, 5);
    c.mouse_hover(2);
    let mut st = c.state.lock().unwrap();
    assert!(c.move_down(&mut st), "前置条件——应能下移");
    assert_eq!(c.hover_target(), -1, "键盘移动高亮后悬停应取消");
}
