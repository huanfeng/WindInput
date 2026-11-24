//! 重启服务前的「有没有活跃组合」判据。只钉联想态这一格：四个原有析取项同时为空、
//! 而宿主里挂着 `assoc_composition` 占位的唯一形态。

use crate::Coordinator;
use wind_candidate::{Candidate, CandidateSource};
use wind_config::Config;

/// 摆一个联想态：文本已上屏 ⇒ 缓冲/前缀全空、`active` 为 `None`，只有联想候选在。
/// 宿主那边此刻挂着 `assoc_composition` 占位（本测试构造不出宿主，只摆核心侧）。
fn fill_assoc(c: &Coordinator) {
    let mut st = c.state.lock().unwrap();
    st.input_buffer.clear();
    st.input_buffer_cased.clear();
    st.committed_text.clear();
    st.preedit.clear(); // 嵌入模式（含压制态强制嵌入）下联想不给标识，这里就是空
    st.active = None;
    st.candidates = vec![Candidate {
        text: "输入法".into(),
        source: CandidateSource::Assoc,
        ..Default::default()
    }];
}

/// 联想态下重启服务必须认定「有组合」，否则宿主里的占位空格成孤儿。
///
/// 断言 `candidates` 被清空：判据为真才走 `reset_exclusive_modes`，清没清就是判据结论的
/// 可观测投影（headless 下截不到 `ClearComposition`）。`request_restart()` 在单测里是 no-op。
/// 变异检验：去掉判据里的 `!state.candidates.is_empty()` ⇒ 本条红。
#[test]
fn restarting_during_association_still_clears_the_host_composition() {
    let (c, _rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    fill_assoc(&c);
    // 前置：四个原有析取项确实全空，否则本条测的是别的东西。
    {
        let st = c.state.lock().unwrap();
        assert!(st.input_buffer.is_empty(), "前置：联想态缓冲为空");
        assert!(st.committed_text.is_empty(), "前置：联想态无已转换前缀");
        assert!(st.preedit.is_empty(), "前置：嵌入模式下联想不给标识");
        assert!(st.active.is_none(), "前置：联想不是 overlay 模式");
        assert!(st.assoc_active(), "前置：这确实是联想态");
    }

    c.restart_service();

    assert!(
        c.state.lock().unwrap().candidates.is_empty(),
        "联想态重启服务必须认定有组合并清理——否则宿主里那个占位空格没人收，\
             而服务一重启 assoc_placeholder_orphaned 也跟着没了，孤儿认领接不到"
    );
}
