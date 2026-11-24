//! 鼠标悬停目标（`Coordinator::hover_index`）的**清空覆盖面**。
//!
//! 本组测试锁的是一个曾经静默存在的缺陷：悬停目标此前是 `State` 的字段，清空只能由每个
//! 候选装填点手工执行。主路径 `update_candidates` 做了，overlay 各路径（特殊模式 / 临拼 /
//! 临英 / 混输·快捷输入 / 拼音组合复位）全部漏了——悬停高亮与 tooltip 于是跨按键、跨组合、
//! 跨模式存活，用户看到的是「候选窗再次弹出时，鼠标没动却已经有一项被高亮并弹出了 tooltip」。
//!
//! ★ 该缺陷在主路径上**物理不可观测**：普通输入每敲一键都重走 `update_candidates`，
//! 残留被持续覆盖掉。所以只测普通输入路径等于什么都没测——下面必须逐个 overlay 入口点名。
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

/// **反向对照**：悬停确实设得上。
///
/// 少了本条，下面所有「××之后归零」都可能因为悬停压根没设上而全部假绿——本仓
/// 「测了个恒为真的断言」已经栽过不止一次。
#[test]
fn mouse_hover_sets_target() {
    let c = coord();
    seed_candidates(&c, 5);
    c.mouse_hover(2);
    assert_eq!(c.hover_target(), 2, "有候选时悬停应设得上");
}

/// 候选窗隐藏 = 会话终结，悬停必须归零。
///
/// 这是根治点：`notify_ui_hide` 有 40+ 个调用点，把清空放在这里，任何一条隐藏通路都覆盖到。
/// （UI 侧 `CandidateMouse::reset_hover` 清的是防抖闸门，决定何时**发**事件；
/// 高亮与 tooltip 读的是本值，两者不是一回事。）
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
/// 逐个点名而不是抽样：它们是**平行的独立落点**，历史上正是「主路径做了、其余全漏」。
/// 新增候选来源时若忘了 `reset_candidate_view`，本测试不会自动覆盖到——但把入口逐个
/// 列在这里，至少让「又多了一个装填点」这件事在评审时看得见。
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

/// 「鼠标移出候选窗」这条 `Hover(-1)` 在候选恰好已清空时**不能被吞掉**。
///
/// 旧实现在 `mouse_hover` 开头对空候选直接 early-return，于是离开事件丢失、旧值残留。
/// 「候选没了」正是最该归零的时刻，拿它当早退条件恰好搞反了。
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
