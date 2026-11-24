//! 展开态的行为测试。合成候选的几组只走导航（不碰引擎），真实词库那组走完整输入链。
use super::*;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{KeyAction, MessageHandler};
use wind_ui_types::{CandidateGrid, UiCommand, UiEvent};

/// 出厂每页 7 个、可见 5 行。
const PP: usize = 7;
const ROWS: usize = 5;

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn coord(grid_on: bool) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let mut cfg = wind_config::Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = grid_on;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    c.debug_mark_coords_ready();
    (c, rx)
}

/// 造 `n` 条候选并挂上一段编码（有编码才有输入会话，Esc 等键才归输入法）。
fn seed(c: &Coordinator, n: usize) {
    let mut st = c.state.lock().unwrap();
    st.input_buffer = "yi".into();
    st.candidates = (0..n)
        .map(|i| wind_candidate::Candidate {
            text: format!("c{i}"),
            ..Default::default()
        })
        .collect();
    c.reset_candidate_view(&mut st);
}

/// (当前页, 页内高亮, 网格首行)。
fn view(c: &Coordinator) -> (usize, usize, Option<usize>) {
    let st = c.state.lock().unwrap();
    (st.current_page, st.selected_index, st.grid_top)
}

/// 最后一帧候选：(文字, 选中, 网格版式)。
fn last_frame(rx: &Receiver<UiCommand>) -> Option<(Vec<String>, usize, Option<CandidateGrid>)> {
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates {
                candidates,
                selected,
                grid,
                ..
            } => Some((
                candidates.into_iter().map(|i| i.text).collect(),
                selected,
                grid,
            )),
            _ => None,
        })
        .last()
}

/// 反向对照：开关关着时翻页键就是翻页，网格不出现。
#[test]
fn page_key_just_pages_when_grid_is_off() {
    let (c, _rx) = coord(false);
    seed(&c, PP * 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(view(&c), (1, 0, None));
}

/// 第一次按翻页键只展开、不翻页（D3）；下发的帧是可见区内逐行平铺的几页。
#[test]
fn first_page_key_expands_without_turning_the_page() {
    let (c, rx) = coord(true);
    seed(&c, PP * 8);
    let _ = rx.try_iter().count();
    assert!(matches!(
        c.handle_key_event(&key(keymap::VK_EQUAL)),
        KeyAction::Consumed
    ));
    assert_eq!(view(&c), (0, 0, Some(0)), "高亮不动，只是展开");
    let (texts, selected, grid) = last_frame(&rx).expect("展开要重绘");
    assert_eq!(texts.len(), PP * ROWS, "一屏 = 可见行数 × 每页数");
    assert_eq!(texts[PP], "c7", "第二行就是第二页");
    assert_eq!(selected, 0);
    assert_eq!(
        grid,
        Some(CandidateGrid {
            cols: PP as u16,
            rows: ROWS as u16,
            highlight_row: 0,
            index_start: 0,
            index_count: PP as u16,
            // 合成候选全是非汉字：单字 / 词组 / 生僻都没有内容 ⇒ 不显示标签栏。
            tabs: 0,
            active_tab: 0,
        }),
        "每页 7 个按整行补只编得下高亮行"
    );
}

/// 只有一页时没什么可展开：翻页键回到原有语义（翻不动）。
#[test]
fn single_page_does_not_expand() {
    let (c, _rx) = coord(true);
    seed(&c, PP);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(view(&c).2, None);
}

/// 「还能加载」但扩容后仍只有一页：不展开，也不记「本次展开过」。
#[test]
fn has_more_but_still_one_page_does_not_expand() {
    let (c, _rx) = coord(true);
    seed(&c, PP);
    c.state.lock().unwrap().has_more = true;
    c.handle_key_event(&key(keymap::VK_EQUAL));
    let st = c.state.lock().unwrap();
    assert_eq!(st.grid_top, None);
    assert!(!st.grid_keep);
}

/// 原地重建（置顶 / 调位 / 删除）后回到原来那一行、那一列，而不是跳回首行。
#[test]
fn in_place_rebuild_keeps_the_row_and_column() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    for _ in 0..3 {
        c.handle_key_event(&key(keymap::VK_DOWN));
    }
    c.handle_key_event(&key(keymap::VK_RIGHT));
    c.handle_key_event(&key(keymap::VK_RIGHT));
    let before = view(&c);
    let mut st = c.state.lock().unwrap();
    let snap = Coordinator::grid_snapshot(&st).expect("展开着");
    // 模拟 `update_candidates`：视图复位，再按「本次展开过」从首行展开回来。
    c.reset_candidate_view(&mut st);
    c.grid_resume(&mut st);
    assert_eq!(st.current_page, 0, "前置：重建会跳回首行");
    c.grid_restore(&mut st, snap);
    drop(st);
    assert_eq!(view(&c), before);
}

/// 只剩联想（候选非空、没有编码）时按键：上一次组合已结束，「本次展开过」作废。
#[test]
fn association_ends_the_kept_expansion() {
    let (c, _rx) = coord(true);
    {
        let mut st = c.state.lock().unwrap();
        st.candidates = vec![wind_candidate::Candidate {
            text: "们".into(),
            source: wind_candidate::CandidateSource::Assoc,
            ..Default::default()
        }];
        st.grid_keep = true;
    }
    c.handle_key_event(&key(u32::from(b'A')));
    assert!(!c.state.lock().unwrap().grid_keep);
}

/// 码表空码：候选为空但组合还在，「本次展开过」不能被当成组合结束清掉。
#[test]
fn empty_code_keeps_the_kept_expansion() {
    let (c, _rx) = coord(true);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "zzzz".into();
        st.grid_keep = true;
    }
    c.handle_key_event(&key(keymap::VK_LEFT));
    assert!(c.state.lock().unwrap().grid_keep);
}

/// 顶码：首选上屏、余码另起一次组合，不继承展开。
#[test]
fn top_code_commit_starts_collapsed() {
    let (c, _rx) = coord(true);
    let mut st = c.state.lock().unwrap();
    st.input_buffer = "aaaah".into();
    st.input_buffer_cased = "aaaah".into();
    st.grid_keep = true;
    c.commit_top_text(
        &mut st,
        "aaaa",
        "工".into(),
        None,
        None,
        "h",
        wind_candidate::CandidateSource::default(),
        None,
    );
    assert!(!st.grid_keep);
    assert_eq!(st.grid_top, None);
}

/// ↓ 换行并保持列号；← → 线性移动、越过行尾折到下一行。
#[test]
fn arrows_move_in_two_dimensions() {
    let (c, rx) = coord(true);
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_key_event(&key(keymap::VK_RIGHT));
    c.handle_key_event(&key(keymap::VK_RIGHT));
    assert_eq!(view(&c), (0, 2, Some(0)));
    c.handle_key_event(&key(keymap::VK_DOWN));
    assert_eq!(view(&c), (1, 2, Some(0)), "换行保持列号");
    let (_, selected, grid) = last_frame(&rx).unwrap();
    assert_eq!(selected, PP + 2, "选中项是平铺下标");
    assert_eq!(grid.unwrap().highlight_row, 1);
    for _ in 0..(PP - 2) {
        c.handle_key_event(&key(keymap::VK_RIGHT));
    }
    assert_eq!(view(&c), (2, 0, Some(0)), "越过行尾折到下一行行首");
    c.handle_key_event(&key(keymap::VK_LEFT));
    assert_eq!(view(&c), (1, PP - 1, Some(0)), "越过行首折回上一行行尾");
}

/// 展开态下 ←→ 归网格，不再移动编码区光标。
#[test]
fn arrows_do_not_move_the_preedit_caret_when_expanded() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 3);
    c.state.lock().unwrap().input_cursor_pos = 2;
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_key_event(&key(keymap::VK_LEFT));
    assert_eq!(c.state.lock().unwrap().input_cursor_pos, 2);
}

/// `-` `=` 逐行移动（同滚轮）：高亮到可见区末行再按，可见区滚一行；首行按 `-` 停住不收起。
#[test]
fn page_key_moves_a_row_when_expanded() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_key_event(&key(keymap::VK_RIGHT));
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(view(&c), (1, 1, Some(0)), "下移一行并保持列号");
    for _ in 0..3 {
        c.handle_key_event(&key(keymap::VK_EQUAL));
    }
    assert_eq!(view(&c), (4, 1, Some(0)), "到可见区末行");
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(view(&c), (5, 1, Some(1)), "末行再按，可见区滚一行");
    for _ in 0..5 {
        c.handle_key_event(&key(keymap::VK_MINUS));
    }
    assert_eq!(view(&c), (0, 1, Some(0)));
    assert!(matches!(
        c.handle_key_event(&key(keymap::VK_MINUS)),
        KeyAction::Consumed
    ));
    assert_eq!(view(&c), (0, 1, Some(0)), "首行按 - 停住，不收起");
}

/// PageUp/PageDown 一次滚一屏，高亮随之下移一屏并保持列号；末屏不留空行。
#[test]
fn page_up_down_scrolls_a_screen_when_expanded() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 8); // 8 页
    c.handle_key_event(&key(keymap::VK_NEXT));
    c.handle_key_event(&key(keymap::VK_RIGHT));
    c.handle_key_event(&key(keymap::VK_NEXT));
    // 末页是第 7 页：可见区最多滚到 3..=7，高亮从 0 下移 5 行到第 5 页。
    assert_eq!(view(&c), (5, 1, Some(3)));
    c.handle_key_event(&key(keymap::VK_NEXT));
    assert_eq!(view(&c), (7, 1, Some(3)), "到底后高亮落到末行");
    c.handle_key_event(&key(keymap::VK_PRIOR));
    assert_eq!(view(&c), (2, 1, Some(0)));
    c.handle_key_event(&key(keymap::VK_PRIOR));
    assert_eq!(view(&c), (0, 1, Some(0)), "首屏再往上翻回到首行，不收起");
    assert!(matches!(
        c.handle_key_event(&key(keymap::VK_PRIOR)),
        KeyAction::Consumed
    ));
    assert_eq!(view(&c).2, Some(0));
}

/// 关掉 `grid_pageupdown_screen` 后 PageUp/PageDown 也逐行。
#[test]
fn page_up_down_moves_a_row_when_screen_off() {
    let mut cfg = wind_config::Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = true;
    cfg.ui.candidate.grid_pageupdown_screen = false;
    let (c, _rx) = Coordinator::new_headless_with_ui(cfg, None);
    c.debug_mark_coords_ready();
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_NEXT));
    c.handle_key_event(&key(keymap::VK_NEXT));
    assert_eq!(view(&c), (1, 0, Some(0)));
    c.handle_key_event(&key(keymap::VK_PRIOR));
    c.handle_key_event(&key(keymap::VK_PRIOR));
    assert_eq!(view(&c), (0, 0, Some(0)), "首行停住，不收起");
}

/// 把 `.` / `,` 绑成「翻屏」动词；`grid_on` 控制是否开展开。
fn coord_screen_verbs(grid_on: bool) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let mut cfg = wind_config::Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = grid_on;
    cfg.keys
        .session_actions
        .insert("period".into(), "page_screen_next".into());
    cfg.keys
        .session_actions
        .insert("comma".into(), "page_screen_prev".into());
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    c.debug_mark_coords_ready();
    (c, rx)
}

/// 展开态：绑了翻屏动词的普通键与 PageUp/PageDown 一样翻整屏，而不是逐行。
#[test]
fn screen_verb_scrolls_a_screen_when_expanded() {
    let (c, _rx) = coord_screen_verbs(true);
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_PERIOD)); // 首按只展开
    assert_eq!(view(&c), (0, 0, Some(0)));
    c.handle_key_event(&key(keymap::VK_PERIOD));
    assert_eq!(view(&c), (ROWS, 0, Some(3)), "下移一整屏，可见区跟随");
    c.handle_key_event(&key(keymap::VK_COMMA));
    assert_eq!(view(&c).0, 0, "上翻一整屏");
}

/// 未展开（展开功能关着）：翻屏动词就是普通翻一页。
#[test]
fn screen_verb_pages_one_page_when_not_expanded() {
    let (c, _rx) = coord_screen_verbs(false);
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_PERIOD));
    assert_eq!(view(&c), (1, 0, None), "翻一页，不展开");
    c.handle_key_event(&key(keymap::VK_COMMA));
    assert_eq!(view(&c), (0, 0, None));
}

/// 对照：同一环境下 `=` 仍逐行（翻页动词不受翻屏动词影响）。
#[test]
fn page_verb_still_moves_a_row_next_to_screen_verb() {
    let (c, _rx) = coord_screen_verbs(true);
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(view(&c), (1, 0, Some(0)));
}

/// 高亮行滚出可见区时可见区跟随。
#[test]
fn visible_rows_follow_the_highlight() {
    let (c, rx) = coord(true);
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    for _ in 0..ROWS {
        c.handle_key_event(&key(keymap::VK_DOWN));
    }
    assert_eq!(view(&c), (5, 0, Some(1)));
    let (texts, selected, grid) = last_frame(&rx).unwrap();
    assert_eq!(texts[0], "c7", "可见区第一行是第 1 页");
    assert_eq!(grid.unwrap().highlight_row as usize, ROWS - 1);
    assert_eq!(selected, (ROWS - 1) * PP);
}

/// 末屏行数不足一屏：照常下发实有的那些，补行由渲染端负责。
#[test]
fn short_last_screen_sends_only_real_rows() {
    let (c, rx) = coord(true);
    seed(&c, PP * 2 + 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    let (texts, _, grid) = last_frame(&rx).unwrap();
    assert_eq!(texts.len(), PP * 2 + 3);
    assert_eq!(grid.unwrap().rows as usize, ROWS);
}

/// ↓ 到末行后停住；↑ 在第一行收起。
#[test]
fn up_on_first_row_collapses() {
    let (c, rx) = coord(true);
    seed(&c, PP * 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_key_event(&key(keymap::VK_DOWN));
    c.handle_key_event(&key(keymap::VK_DOWN));
    c.handle_key_event(&key(keymap::VK_DOWN));
    assert_eq!(view(&c).0, 2, "末行再按 ↓ 停住");
    c.handle_key_event(&key(keymap::VK_UP));
    c.handle_key_event(&key(keymap::VK_UP));
    assert_eq!(view(&c), (0, 0, Some(0)));
    c.handle_key_event(&key(keymap::VK_UP));
    assert_eq!(view(&c).2, None, "第一行按 ↑ 收起");
    let (texts, _, grid) = last_frame(&rx).unwrap();
    assert_eq!((texts.len(), grid), (PP, None), "收起后回到单行");
}

/// Esc 两段式（D4）：先收起、保留编码；再按才取消。
#[test]
fn escape_collapses_first_then_cancels() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_key_event(&key(keymap::VK_DOWN));
    assert!(matches!(
        c.handle_key_event(&key(keymap::VK_ESCAPE)),
        KeyAction::Consumed
    ));
    {
        let st = c.state.lock().unwrap();
        assert_eq!(st.grid_top, None);
        assert_eq!(st.input_buffer, "yi", "收起不动编码");
        assert_eq!(st.current_page, 1, "收起停在高亮所在的那一页");
    }
    assert!(matches!(
        c.handle_key_event(&key(keymap::VK_ESCAPE)),
        KeyAction::ClearComposition
    ));
}

/// 回车上屏这类出口只清候选、不经视图复位：组合结束后 ←→ / Esc 必须交还宿主，
/// 不能被残留的展开态吞掉（Linux / macOS 每个键都会进协调器）。
#[test]
fn keys_go_back_to_the_host_after_the_composition_ends() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert!(view(&c).2.is_some());
    c.handle_key_event(&key(keymap::VK_RETURN));
    assert!(
        c.state.lock().unwrap().candidates.is_empty(),
        "回车上屏了编码"
    );
    for vk in [keymap::VK_LEFT, keymap::VK_RIGHT, keymap::VK_ESCAPE] {
        assert!(
            matches!(c.handle_key_event(&key(vk)), KeyAction::PassThrough),
            "组合已结束，键 {vk:#x} 应交还宿主"
        );
    }
}

/// 在靠后的页上展开：可见区往前让、凑满一屏，而不是只露出一行真候选。
#[test]
fn expanding_near_the_end_fills_the_screen() {
    let (c, rx) = coord(true);
    seed(&c, PP * 6);
    for _ in 0..(PP * 4) {
        c.handle_key_event(&key(keymap::VK_DOWN)); // 收起态逐个移动，回卷到第 4 页
    }
    assert_eq!(view(&c), (4, 0, None));
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(view(&c), (4, 0, Some(1)), "6 页可见 5 行 ⇒ 首行是第 1 页");
    let (texts, selected, grid) = last_frame(&rx).unwrap();
    assert_eq!(texts.len(), PP * ROWS);
    assert_eq!(grid.unwrap().highlight_row, 3);
    assert_eq!(selected, 3 * PP);
}

/// UI 回传平铺下标：右键第 3 行第 2 格，高亮先切过去，菜单目标是那一格。
#[test]
fn flat_index_from_ui_lands_on_that_cell() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 8);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(c.resolve_event_index(2 * PP + 1, true), Some(1));
    assert_eq!(view(&c), (2, 1, Some(0)));
    assert_eq!(c.resolve_event_index(PP * ROWS, true), None, "可见区外");
    // 收起态原样返回（快捷键等调用方传的就是页内下标）。
    c.collapse_grid(&mut c.state.lock().unwrap());
    assert_eq!(c.resolve_event_index(3, true), Some(3));
}

/// 悬停校验按整个可见区，不按单页：第 2 行的格子也是合法悬停目标。
#[test]
fn hover_accepts_cells_beyond_the_first_row() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_ui_event(UiEvent::Hover((PP + 1) as i32));
    assert_eq!(c.hover_target(), (PP + 1) as i32);
}

/// 滚轮 / 翻页箭头在展开态滚一行；往上滚到顶不收起。收起态往下滚先展开。
#[test]
fn mouse_page_scrolls_rows() {
    let (c, _rx) = coord(true);
    seed(&c, PP * 3);
    c.handle_ui_event(UiEvent::Page(1));
    assert_eq!(view(&c), (0, 0, Some(0)), "收起态往下滚 = 展开");
    c.handle_ui_event(UiEvent::Page(1));
    assert_eq!(view(&c).0, 1);
    c.handle_ui_event(UiEvent::Page(-1));
    c.handle_ui_event(UiEvent::Page(-1));
    assert_eq!(view(&c), (0, 0, Some(0)), "滚到顶停住");
}

/// 只有横排展开：竖排下翻页键照旧翻页。
#[test]
fn vertical_layout_does_not_expand() {
    let mut cfg = wind_config::Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = true;
    cfg.ui.candidate.layout = "vertical".into();
    let (c, _rx) = Coordinator::new_headless_with_ui(cfg, None);
    seed(&c, PP * 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(view(&c), (1, 0, None));
}

/// 自定每页数与序号顺延档位的协调器。
fn coord_with(per_page: usize, fill: &str) -> (Arc<Coordinator>, Receiver<UiCommand>) {
    let mut cfg = wind_config::Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = true;
    cfg.ui.candidate.per_page = per_page;
    cfg.ui.candidate.grid_index_fill = fill.into();
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    c.debug_mark_coords_ready();
    (c, rx)
}

/// 最后一帧里带序号的格子：(平铺下标起点, 格数, 各格序号)。
fn index_frame(rx: &Receiver<UiCommand>) -> (u16, u16, Vec<String>) {
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates {
                candidates,
                grid: Some(g),
                ..
            } => Some((
                g.index_start,
                g.index_count,
                candidates.into_iter().map(|i| i.label).collect::<Vec<_>>(),
            )),
            _ => None,
        })
        .last()
        .expect("应下发展开态的帧")
}

/// 只补整行（出厂）：每页 5 个时高亮行 1–5、下一行 6–9、0（第 10 个由数字键 0 选，序号沿用
/// 普通候选窗的写法），数字键的选择范围跟着延到下一行。
#[test]
fn indices_continue_onto_whole_rows() {
    let (c, rx) = coord_with(5, "rows");
    seed(&c, 5 * 8);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_key_event(&key(keymap::VK_DOWN));
    let (from, count, labels) = index_frame(&rx);
    assert_eq!((from, count), (5, 10), "高亮在第 2 行：第 2、3 行带号");
    assert_eq!(
        &labels[5..15],
        &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"]
    );
    let st = c.state.lock().unwrap();
    assert_eq!(c.select_range(&st), (5, 15), "数字键的范围跟着序号走");
}

/// 高亮在可见区最后一行：下面没有可见行可补，只编高亮行（不给看不见的格子编号）。
#[test]
fn indices_stay_in_the_visible_rows() {
    let (c, rx) = coord_with(5, "rows");
    seed(&c, 5 * 8);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    for _ in 0..(ROWS - 1) {
        c.handle_key_event(&key(keymap::VK_DOWN));
    }
    let (from, count, _) = index_frame(&rx);
    assert_eq!((from, count), ((ROWS as u16 - 1) * 5, 5));
}

/// 逐格档：每页 7 个时编满 10 个，下一行前 3 格带 8、9、0；off 档只编高亮行。
#[test]
fn index_fill_cells_and_off() {
    let (c, rx) = coord_with(7, "cells");
    seed(&c, 7 * 4);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    let (from, count, labels) = index_frame(&rx);
    assert_eq!((from, count), (0, 10));
    assert_eq!(&labels[7..10], &["8", "9", "0"]);
    let (c, rx) = coord_with(5, "off");
    seed(&c, 5 * 4);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    assert_eq!(index_frame(&rx).1, 5);
}

/// 菜单开着时在另一格上右键：高亮与悬停都跟到新的这一格（悬停若停在旧格，旧格的悬停
/// 底色会留在原处，看上去像高亮没跟过来）。
#[test]
fn second_right_click_moves_highlight_and_hover() {
    let (c, rx) = coord(true);
    seed(&c, PP * 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    for idx in [PP + 2, 2 * PP + 4] {
        c.handle_ui_event(UiEvent::RequestCandidateMenu {
            page_local: idx,
            x: 0,
            y: 0,
        });
    }
    assert_eq!(c.hover_target(), (2 * PP + 4) as i32);
    let (_, selected, _) = last_frame(&rx).unwrap();
    assert_eq!(selected, 2 * PP + 4);
}

fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 真实词库：打 `yi`、展开、↓ 到第二行、按 2 ⇒ 上屏的是第 2 页第 2 个。
/// 数字键语义没动（作用于高亮行 = 当前页），这条把它钉在完整输入链上。
#[test]
fn number_key_selects_in_the_highlighted_row() {
    if !data_dir().join("schemas/pinyin.schema.toml").exists() {
        return;
    }
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = true;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    c.debug_mark_coords_ready();
    for ch in "YI".chars() {
        c.handle_key_event(&key(ch as u32));
    }
    c.handle_key_event(&key(keymap::VK_EQUAL));
    c.handle_key_event(&key(keymap::VK_DOWN));
    let (texts, selected, _) = last_frame(&rx).expect("展开态应下发候选");
    assert_eq!(selected, PP);
    let want = c.state.lock().unwrap().candidates[PP + 1].text.clone();
    assert_eq!(texts[PP + 1], want);
    match c.handle_key_event(&key(u32::from(b'2'))) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, want),
        other => panic!("按 2 应上屏第 2 页第 2 个，实得 {other:?}"),
    }
    assert_eq!(c.state.lock().unwrap().grid_top, None, "上屏后收起");
}

/// 打出 `code`（大写键码）并展开（真实词库）。词库缺失返回 `None`。
///
/// 分类用例用 `shijian`：它同时有单字与词组；`yi` 这类完整单音节一个
/// 词组都没有，测不到词组那一支。
fn typed_expanded(code: &str) -> Option<(Arc<Coordinator>, Receiver<UiCommand>)> {
    if !data_dir().join("schemas/pinyin.schema.toml").exists() {
        return None;
    }
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = true;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    c.debug_mark_coords_ready();
    for ch in code.chars() {
        c.handle_key_event(&key(ch as u32));
    }
    c.handle_key_event(&key(keymap::VK_EQUAL));
    Some((c, rx))
}

fn shift_tab() -> KeyEventData {
    KeyEventData {
        modifiers: MOD_SHIFT,
        ..key(keymap::VK_TAB)
    }
}

fn texts(c: &Coordinator) -> Vec<String> {
    c.state
        .lock()
        .unwrap()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect()
}

fn tab_of(c: &Coordinator) -> GridTab {
    c.state.lock().unwrap().grid_tab
}

/// 把高亮移到平铺下标 `idx` 上（展开态 ↓ 换行、→ 换列）。
fn highlight_flat(c: &Coordinator, idx: usize) {
    for _ in 0..idx / PP {
        c.handle_key_event(&key(keymap::VK_DOWN));
    }
    for _ in 0..idx % PP {
        c.handle_key_event(&key(keymap::VK_RIGHT));
    }
    let st = c.state.lock().unwrap();
    assert_eq!(st.current_page * PP + st.selected_index, idx);
}

/// 分步上屏：选一个只吃掉前一段的单字，余下那段的候选仍是展开的；继续打字也不收。
#[test]
fn step_commit_keeps_the_grid_expanded() {
    let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
        return;
    };
    let idx = texts(&c)
        .iter()
        .position(|t| t.chars().count() == 1)
        .expect("shijian 应有单字候选");
    highlight_flat(&c, idx);
    c.handle_key_event(&key(keymap::VK_SPACE));
    {
        let st = c.state.lock().unwrap();
        assert!(!st.committed_text.is_empty(), "单字应留在组合区");
        assert_eq!(st.input_buffer, "jian");
        assert_eq!(st.grid_top, Some(0), "余下那段从首行展开");
    }
    c.handle_key_event(&key(u32::from(b'G')));
    assert_eq!(view(&c).2, Some(0), "继续打字也保持展开");
}

/// Esc 收起之后，本次组合不再自动展开。
#[test]
fn escape_collapse_stops_re_expanding() {
    let Some((c, _rx)) = typed_expanded("SHI") else {
        return;
    };
    c.handle_key_event(&key(keymap::VK_ESCAPE));
    assert_eq!(view(&c).2, None);
    c.handle_key_event(&key(u32::from(b'J')));
    assert_eq!(view(&c).2, None, "收起后再打字不再展开");
}

/// 上屏结束组合：下一次组合从单行开始。
#[test]
fn next_composition_starts_collapsed() {
    let Some((c, _rx)) = typed_expanded("YI") else {
        return;
    };
    assert!(view(&c).2.is_some());
    c.handle_key_event(&key(keymap::VK_SPACE));
    assert!(c.state.lock().unwrap().input_buffer.is_empty(), "空格上屏");
    for ch in "YI".chars() {
        c.handle_key_event(&key(ch as u32));
    }
    assert_eq!(view(&c).2, None, "新组合不继承上一次的展开");
}

/// Tab 依次切到单字、词组：列表里只剩该类；切回全部，列表与切走之前逐条一致。
#[test]
fn tab_key_filters_by_category_and_back() {
    let Some((c, rx)) = typed_expanded("SHIJIAN") else {
        return;
    };
    let all = texts(&c);
    let units = |t: &str| wind_candidate::semantic_units(t);
    let han = |t: &str| t.chars().all(wind_candidate::is_han);
    c.handle_key_event(&key(keymap::VK_TAB));
    assert_eq!(tab_of(&c), GridTab::Single);
    let single = texts(&c);
    assert!(!single.is_empty());
    assert!(
        single.iter().all(|t| han(t) && units(t) == 1),
        "单字：{single:?}"
    );
    let (_, _, grid) = last_frame(&rx).unwrap();
    assert_eq!(grid.unwrap().active_tab, GridTab::Single as u8);
    c.handle_key_event(&key(keymap::VK_TAB));
    assert_eq!(tab_of(&c), GridTab::Phrase);
    assert!(texts(&c).iter().all(|t| han(t) && units(t) >= 2));
    // Shift+Tab 反向：词组 → 单字 → 全部。
    c.handle_key_event(&shift_tab());
    c.handle_key_event(&shift_tab());
    assert_eq!(tab_of(&c), GridTab::All);
    let back = texts(&c);
    assert_eq!(
        &back[..all.len().min(back.len())],
        &all[..all.len().min(back.len())]
    );
    assert!(back.len() >= all.len(), "补加载过只会更多，不会丢");
}

/// 切分类时高亮锚定在原来那个词上。
#[test]
fn switching_tab_keeps_the_highlighted_word() {
    let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
        return;
    };
    // 在「全部」里走到第一个单字上，记下它。
    let first_single = {
        let st = c.state.lock().unwrap();
        st.candidates
            .iter()
            .position(|x| wind_candidate::semantic_units(&x.text) == 1)
            .unwrap()
    };
    for _ in 0..first_single {
        c.handle_key_event(&key(keymap::VK_RIGHT));
    }
    let word = {
        let st = c.state.lock().unwrap();
        st.candidates[c.highlighted_global_index(&st)].text.clone()
    };
    c.handle_key_event(&key(keymap::VK_TAB));
    let st = c.state.lock().unwrap();
    assert_eq!(st.candidates[c.highlighted_global_index(&st)].text, word);
}

/// Tab 转一圈回到「全部」：高亮回到原来那个词、原来那一可见行。中途某个分类里没有它
/// （落到第一个）也不改锚点，只要切换之间没挪过高亮。
#[test]
fn tab_round_trip_returns_to_the_same_cell() {
    let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
        return;
    };
    // 第二页以后的第一个单字：它在「词组」里没有。
    let idx = {
        let st = c.state.lock().unwrap();
        (PP..st.candidates.len())
            .find(|&i| wind_candidate::semantic_units(&st.candidates[i].text) == 1)
            .expect("shijian 第二页以后应有单字")
    };
    highlight_flat(&c, idx);
    let before = {
        let st = c.state.lock().unwrap();
        (st.current_page, st.selected_index, st.grid_top)
    };
    assert!(before.2.unwrap() < before.0, "前提：高亮不在可见区首行");
    let n = c.grid_tabs(&c.state.lock().unwrap()).len();
    assert!(n >= 3, "前提：至少全部 / 单字 / 词组");
    for _ in 0..n {
        c.handle_key_event(&key(keymap::VK_TAB));
    }
    assert_eq!(tab_of(&c), GridTab::All);
    let st = c.state.lock().unwrap();
    assert_eq!((st.current_page, st.selected_index, st.grid_top), before);
}

/// 在「单字」里触发扩容：扩容重建的是完整列表（带词组），筛选要重新套上。
#[test]
fn expansion_keeps_the_filter() {
    let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
        return;
    };
    c.handle_key_event(&key(keymap::VK_TAB));
    let mut st = c.state.lock().unwrap();
    assert_eq!(st.grid_tab, GridTab::Single);
    // 白盒触发一次扩容：这个码首批就取完了，翻页触发不到。
    st.has_more = true;
    let limit = st.candidate_limit;
    c.expand_candidates(&mut st);
    assert!(st.candidate_limit > limit, "扩容确实重建过");
    assert_eq!(st.grid_tab, GridTab::Single);
    assert!(
        st.candidates
            .iter()
            .all(|x| wind_candidate::semantic_units(&x.text) == 1),
        "扩容后仍只有单字"
    );
    assert!(
        st.grid_full
            .iter()
            .any(|x| wind_candidate::semantic_units(&x.text) >= 2),
        "完整列表里有词组（重建出来的是完整列表）"
    );
}

/// 鼠标点标签（tag 段 `GRID_TAB_TAG_BASE`）切分类；数字键在筛过的列表里选词；
/// 上屏后再打字，分类随视图一起复位。
#[test]
fn tab_click_then_select_then_reset() {
    let Some((c, _rx)) = typed_expanded("SHIJIAN") else {
        return;
    };
    c.handle_ui_event(UiEvent::CandidateSelect(
        GRID_TAB_TAG_BASE as usize + GridTab::Phrase as usize,
    ));
    assert_eq!(tab_of(&c), GridTab::Phrase);
    let want = texts(&c)[1].clone();
    match c.handle_key_event(&key(u32::from(b'2'))) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, want),
        other => panic!("按 2 应上屏词组里的第 2 个，实得 {other:?}"),
    }
    for ch in "SHI".chars() {
        c.handle_key_event(&key(ch as u32));
    }
    let st = c.state.lock().unwrap();
    assert_eq!((st.grid_tab, st.grid_top), (GridTab::All, None));
    assert!(st.grid_full.is_empty());
}

/// 临英自己维护编码栏（前缀 + 临英缓冲）：点分类标签换了候选，编码栏不能被按普通模式重算成
/// `committed_text + input_buffer`——在 overlay 下那就是空串。临英是能展开网格的 overlay 里
/// 唯一不走高亮跟随的（生僻字 / 特殊模式不展开，见 `grid_eligible`）。
#[test]
fn tab_click_in_temp_english_keeps_the_preedit() {
    let (c, _rx) = coord(true);
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(crate::pipeline::ModeKind::TempEnglish);
        st.temp_english_buffer = "shi".into();
        st.temp_english_cursor = 3;
        st.preedit = "shi".into();
        st.candidates = (0..PP * 3)
            .map(|i| {
                let ch = char::from_u32(0x4e00 + i as u32).unwrap();
                cand(&if i % 2 == 0 {
                    ch.to_string()
                } else {
                    format!("{ch}{ch}")
                })
            })
            .collect();
        c.reset_candidate_view(&mut st);
        assert!(c.try_expand_grid(&mut st), "前提：展开");
    }
    c.grid_click_tab(GridTab::Phrase);
    let st = c.state.lock().unwrap();
    assert_eq!(st.grid_tab, GridTab::Phrase, "前提：点标签切到了词组");
    assert_eq!(st.preedit, "shi", "临英的编码栏不随点标签改写");
}

/// 当前候选里一条都没有的分类不给：合成候选全是非汉字，单字 / 词组都不出现，标签栏消失，
/// Tab 回到它原本的绑定（出厂高亮下移 ⇒ 展开态下换行），而不是被一路跳过的循环吞掉。
#[test]
fn empty_categories_are_not_offered() {
    let (c, rx) = coord(true);
    seed(&c, PP * 3);
    c.handle_key_event(&key(keymap::VK_EQUAL));
    let (_, _, grid) = last_frame(&rx).unwrap();
    assert_eq!(grid.unwrap().tabs, 0, "没有可用分类 ⇒ 不显示标签栏");
    c.handle_key_event(&key(keymap::VK_TAB));
    let st = c.state.lock().unwrap();
    assert_eq!(st.grid_tab, GridTab::All);
    assert_eq!(st.candidates.len(), PP * 3);
    assert_eq!(st.current_page, 1, "Tab 照旧是高亮下移（展开态即换行）");
}

fn cand(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        ..Default::default()
    }
}

/// 分类判据：词组不收整句，符号不收英文词。
#[test]
fn category_rules() {
    let admits = |t, c: &Candidate| Coordinator::grid_tab_admits(t, c, true);
    let mut sentence = cand("时间不早了");
    sentence.is_sentence = true;
    assert!(admits(GridTab::Phrase, &cand("时间")));
    assert!(!admits(GridTab::Phrase, &sentence), "整句不算词组");
    assert!(!admits(GridTab::Phrase, &cand("时")));
    assert!(admits(GridTab::Single, &cand("时")));
    assert!(admits(GridTab::Symbol, &cand("😀")));
    assert!(admits(GridTab::Symbol, &cand("※")));
    assert!(!admits(GridTab::Symbol, &cand("shijian")), "英文词不算符号");
    assert!(!admits(GridTab::Symbol, &cand("时")));
}

/// 调位位置按完整列表计：子集 [b, d] 对应完整列表 [A, b, C, d]。
#[test]
fn reorder_positions_map_back_to_the_full_list() {
    let (c, _rx) = coord(true);
    let mut st = c.state.lock().unwrap();
    st.candidates = vec![cand("b"), cand("d")];
    st.grid_full = vec![cand("A"), cand("b"), cand("C"), cand("d")];
    st.grid_tab = GridTab::Single;
    let p0 = c.reorder_positions(&st, 0);
    assert_eq!(
        (p0.here, p0.prev, p0.next, p0.last),
        (1, 0, 3, 3),
        "后移落到下一个可见词 d 的位置"
    );
    let p1 = c.reorder_positions(&st, 1);
    assert_eq!(
        (p1.here, p1.prev, p1.next),
        (3, 1, 3),
        "前移落到上一个可见词 b 的位置"
    );
    // 没在筛选：与 idx ± 1 逐字一致。
    st.grid_tab = GridTab::All;
    st.grid_full.clear();
    let p = c.reorder_positions(&st, 1);
    assert_eq!((p.here, p.prev, p.next, p.last), (1, 0, 1, 1));
}

/// shadow 之后插入的项（简繁变体、emoji、单字母重复项）不计位置：读端看到的是
/// [A, B, C]，界面是 [重复, A, A变体, B, 🙂, C]。
#[test]
fn reorder_positions_skip_items_inserted_after_shadow() {
    let (c, _rx) = coord(true);
    let mut st = c.state.lock().unwrap();
    let mut repeat = cand("上次");
    repeat.natural_order = crate::handle_candidate::LEADING_REPEAT_ORDER;
    let mut variant = cand("A");
    variant.s2t_override = Some("Ａ".into());
    let mut emoji = cand("🙂");
    emoji.is_emoji_suggestion = true;
    st.candidates = vec![repeat, cand("A"), variant, cand("B"), emoji, cand("C")];
    let p = |i| {
        let r = c.reorder_positions(&st, i);
        (r.here, r.prev, r.next, r.last)
    };
    assert_eq!(p(1), (0, 0, 1, 2), "A");
    assert_eq!(p(2), (0, 0, 1, 2), "A 的变体 = A");
    assert_eq!(p(3), (1, 0, 2, 2), "B：前移越过变体落到 A");
    assert_eq!(p(5), (2, 1, 2, 2), "C：前移越过 emoji 落到 B");
}

/// 分类筛选 + 简繁变体：完整列表 [出, 出(齣), 好, 和平]，「单字」子集 [出, 出(齣), 好]。
#[test]
fn reorder_and_pin_slots_under_filter_with_variants() {
    let (c, _rx) = coord(true);
    let mut st = c.state.lock().unwrap();
    let mut variant = cand("出");
    variant.s2t_override = Some("齣".into());
    st.grid_full = vec![cand("出"), variant.clone(), cand("好"), cand("和平")];
    st.candidates = vec![cand("出"), variant, cand("好")];
    st.grid_tab = GridTab::Single;
    let p = c.reorder_positions(&st, 2);
    assert_eq!((p.here, p.prev, p.next, p.last), (1, 0, 2, 2), "好");
    let s = c.pin_at_slots(&st, 2);
    assert_eq!(s.slots, [Some(0), None, None, Some(2)]);
    assert_eq!(s.ui_len, 4);
    // 点变体按原字算：原字连同变体一起挪。
    let s = c.pin_at_slots(&st, 1);
    assert_eq!(s.slots, [None, Some(1), Some(2)]);
}

/// 有 emoji 扩展（`[H, 😀, A, B]`）：整组落不到。
#[test]
fn pin_slots_all_none_with_emoji() {
    let (c, _rx) = coord(true);
    let mut st = c.state.lock().unwrap();
    let mut emoji = cand("😀");
    emoji.is_emoji_suggestion = true;
    st.candidates = vec![cand("H"), emoji, cand("A"), cand("B")];
    let s = c.pin_at_slots(&st, 3);
    assert!(s.has_emoji);
    assert!(s.slots.iter().all(Option::is_none), "{:?}", s.slots);
}

/// 单字母重复项占首位：「第 1 位」落不到；钉到第 2 位写读端 0。
#[test]
fn pin_slots_skip_leading_repeat() {
    let (c, _rx) = coord(true);
    let mut st = c.state.lock().unwrap();
    let mut repeat = cand("上次");
    repeat.natural_order = crate::handle_candidate::LEADING_REPEAT_ORDER;
    assert!(is_leading_repeat(&repeat));
    st.candidates = vec![repeat, cand("A"), cand("B")];
    let s = c.pin_at_slots(&st, 2);
    assert_eq!(s.slots, [None, Some(0), None]);
    assert_eq!(s.position_for(2), Some(0));
    assert_eq!(s.position_for(1), None);
    assert_eq!(s.position_for(0), None);
}

/// 退回完整列表（辅助码等要拿原始候选做快照的入口用）。
#[test]
fn restore_full_list() {
    let (c, _rx) = coord(true);
    let mut st = c.state.lock().unwrap();
    st.candidates = vec![cand("b")];
    st.grid_full = vec![cand("A"), cand("b")];
    st.grid_tab = GridTab::Single;
    c.grid_restore_full(&mut st);
    assert_eq!(st.grid_tab, GridTab::All);
    assert!(st.grid_full.is_empty());
    assert_eq!(st.candidates.len(), 2);
}

/// 用户把 Tab 另作他用（这里是整句切换键）时，展开态不截 Tab。
#[test]
fn tab_left_alone_when_bound_elsewhere() {
    let (c, _rx) = coord(true);
    assert!(
        c.tab_free_for_grid(false),
        "出厂 Tab = 高亮下移，可以让给分类"
    );
    let mut cfg = wind_config::Config::default();
    cfg.ui.candidate.grid_expand = true;
    cfg.schema.pinyin.sentence_cycle_key = "tab".into();
    let (c, _rx) = Coordinator::new_headless_with_ui(cfg, None);
    assert!(!c.tab_free_for_grid(false), "Tab 是整句切换键 ⇒ 不截");
}

/// 真实词库：常用字表在时给「生僻」，切过去只剩非常用（或手动标生僻）的汉字。
///
/// 无头构造不会从数据目录装常用字表，这里注入一张小表：名单里的算常用，其余汉字生僻。
/// 检索范围取「全部字符」：出厂智能档下同码有常用字就滤掉生僻字，完整列表里压根没有
/// 生僻字，标签也就（正确地）不给。
#[test]
fn rare_category_keeps_only_uncommon_han() {
    if !data_dir().join("schemas/pinyin.schema.toml").exists() {
        return;
    }
    let mut cfg = wind_config::Config::default();
    cfg.input.filter_mode = "gb18030".into();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = true;
    let (c, _rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    c.debug_mark_coords_ready();
    crate::charset_test_support::set_common_chars(&c, "以一亿已亦易矣乙译意依".chars());
    for ch in "YI".chars() {
        c.handle_key_event(&key(ch as u32));
    }
    c.handle_key_event(&key(keymap::VK_EQUAL));
    let mut st = c.state.lock().unwrap();
    assert!(
        c.grid_tabs(&st).contains(&GridTab::Rare),
        "常用字表在 ⇒ 给生僻"
    );
    assert!(c.grid_switch_tab(&mut st, GridTab::Rare));
    assert!(!st.candidates.is_empty());
    assert!(
        st.candidates
            .iter()
            .all(|x| !"以一亿已亦易矣乙译意依".contains(x.text.as_str())),
        "名单里的常用字不该出现在生僻里"
    );
}

/// 真实词库、每页 5 个：展开后按 6 上屏的是第二行行首（序号顺延到下一行，数字键跟着走）。
#[test]
fn number_key_follows_indices_onto_the_next_row() {
    if !data_dir().join("schemas/pinyin.schema.toml").exists() {
        return;
    }
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = true;
    cfg.ui.candidate.per_page = 5;
    let (c, _rx) = Coordinator::new_headless_with_ui(cfg, Some(&data_dir()));
    c.debug_mark_coords_ready();
    for ch in "YI".chars() {
        c.handle_key_event(&key(ch as u32));
    }
    c.handle_key_event(&key(keymap::VK_EQUAL));
    let want = c.state.lock().unwrap().candidates[5].text.clone();
    match c.handle_key_event(&key(u32::from(b'6'))) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, want),
        other => panic!("按 6 应上屏第二行行首，实得 {other:?}"),
    }
}

/// 五笔主方案下的临时拼音同样能展开（真实词库）：出厂引导键 `` ` ``，以及用户把 `;`
/// 改绑成临拼。出厂的 `;` 是快捷输入（mix），见下方 `mix_*` 那组。
#[test]
fn temp_pinyin_under_wubi_expands() {
    let d = data_dir();
    if !d.join("schemas/wubi86.schema.toml").exists()
        || !d.join("schemas/pinyin.schema.toml").exists()
    {
        return;
    }
    for (lead, rebind) in [(0xC0u32, false), (0xBA, true)] {
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        cfg.ui.candidate.grid_expand = true;
        if rebind {
            cfg.keys
                .key_actions
                .insert("semicolon".into(), "temp_pinyin".into());
        }
        let (c, _rx) = Coordinator::new_headless_with_ui(cfg, Some(&d));
        c.debug_mark_coords_ready();
        c.handle_key_event(&key(lead));
        for ch in "SHI".chars() {
            c.handle_key_event(&key(ch as u32));
        }
        assert_eq!(
            c.state.lock().unwrap().active,
            Some(crate::pipeline::ModeKind::TempPinyin),
            "前提：{lead:#x} 进临拼"
        );
        c.handle_key_event(&key(keymap::VK_NEXT));
        let st = c.state.lock().unwrap();
        assert_eq!(
            (st.grid_top, st.current_page, st.preedit.as_str()),
            (Some(0), 0, if rebind { ";shi" } else { "`shi" }),
            "{lead:#x}：PageDown 应展开且不翻页"
        );
    }
}

// ———————— 快捷输入（mix）：文本 / 词组透镜展开，数字 / 自由透镜与竖排不展开 ————————

/// 五笔主方案、`;` 进快捷输入，快捷输入布局为 `layout`。词库缺失返回 `None`。
fn mix_coord(
    layout: wind_config::LayoutIntent,
    global_vertical: bool,
) -> Option<(Arc<Coordinator>, Receiver<UiCommand>)> {
    mix_coord_with(layout, global_vertical, |_| {})
}

/// 同 [`mix_coord`]，构造前再改一笔配置。
fn mix_coord_with(
    layout: wind_config::LayoutIntent,
    global_vertical: bool,
    edit: impl FnOnce(&mut wind_config::Config),
) -> Option<(Arc<Coordinator>, Receiver<UiCommand>)> {
    let d = data_dir();
    if !["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
    {
        return None;
    }
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.ui.candidate.grid_expand = true;
    if global_vertical {
        cfg.ui.candidate.layout = "vertical".into();
    }
    cfg.schema.mix_modes[0].candidate_layout = layout;
    // 序号逐格顺延：每页 7 个时 8、9、0 落到第二行，测得到数字键跟着序号走。
    cfg.ui.candidate.grid_index_fill = "cells".into();
    edit(&mut cfg);
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, Some(&d));
    c.debug_mark_coords_ready();
    Some((c, rx))
}

/// `;` + 字母串（大写键码）。
fn mix_type(c: &Coordinator, code: &str) {
    c.handle_key_event(&key(0xBA));
    for ch in code.chars() {
        c.handle_key_event(&key(ch as u32));
    }
    assert_eq!(
        c.state.lock().unwrap().active,
        Some(ModeKind::Mix(0)),
        "前提：`;` 进快捷输入"
    );
}

fn preedit_of(c: &Coordinator) -> String {
    c.state.lock().unwrap().preedit.clone()
}

/// 横排（显式 horizontal，或 follow 且全局横排）：PageDown 只展开不翻页；
/// 竖排（出厂 vertical，或 follow 且全局竖排）照旧翻页、不展开。
#[test]
fn mix_expands_only_when_its_effective_layout_is_horizontal() {
    use wind_config::LayoutIntent as L;
    for (layout, global_vertical, expands) in [
        (L::Horizontal, false, true),
        (L::Follow, false, true),
        (L::Vertical, false, false),
        (L::Follow, true, false),
    ] {
        let Some((c, _rx)) = mix_coord(layout, global_vertical) else {
            return;
        };
        mix_type(&c, "SHI");
        assert!(
            c.total_pages(&c.state.lock().unwrap()) >= 2,
            "前提：多于一页"
        );
        c.handle_key_event(&key(keymap::VK_NEXT));
        let want = if expands {
            (0, 0, Some(0))
        } else {
            (1, 0, None)
        };
        assert_eq!(view(&c), want, "{layout:?} / 全局竖排={global_vertical}");
        assert_eq!(preedit_of(&c), ";shi");
    }
}

/// 展开态的移动与选词：↓ 换行保列、→ 线性、数字键选高亮行（序号顺延同临拼）、空格上屏高亮。
#[test]
fn mix_grid_navigation_and_selection() {
    let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
        return;
    };
    mix_type(&c, "SHI");
    c.handle_key_event(&key(keymap::VK_NEXT));
    let pp = c.per_page(Some(ModeKind::Mix(0)));
    c.handle_key_event(&key(keymap::VK_RIGHT));
    c.handle_key_event(&key(keymap::VK_DOWN));
    assert_eq!(view(&c), (1, 1, Some(0)), "↓ 换行保列、→ 行内移动");
    let all = texts(&c);
    match c.handle_key_event(&key(u32::from(b'2'))) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, all[pp + 1]),
        other => panic!("按 2 应上屏第 2 行第 2 个，实得 {other:?}"),
    }
    assert_eq!(
        c.state.lock().unwrap().active,
        None,
        "整段上屏后退出快捷输入"
    );

    mix_type(&c, "SHI");
    c.handle_key_event(&key(keymap::VK_NEXT));
    let all = texts(&c);
    match c.handle_key_event(&key(u32::from(b'9'))) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, all[8]),
        other => panic!("首行按 9 应上屏顺延到第二行的第 9 格，实得 {other:?}"),
    }

    mix_type(&c, "SHI");
    c.handle_key_event(&key(keymap::VK_NEXT));
    c.handle_key_event(&key(keymap::VK_DOWN));
    c.handle_key_event(&key(keymap::VK_DOWN));
    let want = texts(&c)[2 * pp].clone();
    match c.handle_key_event(&key(keymap::VK_SPACE)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, want),
        other => panic!("空格应上屏高亮项，实得 {other:?}"),
    }
}

/// 展开态下快捷输入选词键（出厂 Ctrl+数字）与数字键同一范围：Ctrl+9 选顺延到第二行的
/// 第 9 格，Ctrl+0 选第 10 格。
#[test]
fn mix_grid_quick_select_key_follows_index_span() {
    let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
        return;
    };
    for (d, idx) in [(b'9', 8usize), (b'0', 9)] {
        mix_type(&c, "SHI");
        c.handle_key_event(&key(keymap::VK_NEXT));
        assert!(
            Coordinator::grid_active(&c.state.lock().unwrap()),
            "前提：已展开"
        );
        let all = texts(&c);
        let mut ev = key(u32::from(d));
        ev.modifiers = wind_ipc::protocol::MOD_CTRL;
        match c.handle_key_event(&ev) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, all[idx]),
            other => panic!("Ctrl+{} 应上屏第 {} 格，实得 {other:?}", d as char, idx + 1),
        }
    }
}

/// 编码栏跟随高亮：展开态里 → 移到「西安」得 `;xi'an`，移回单字「先」得 `;xian`。
#[test]
fn mix_grid_preedit_follows_highlight() {
    let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
        return;
    };
    mix_type(&c, "XIAN");
    c.handle_key_event(&key(keymap::VK_NEXT));
    assert!(view(&c).2.is_some(), "前提：展开");
    let all = texts(&c);
    let xian = all
        .iter()
        .position(|t| t == "西安")
        .expect("xian 应有「西安」");
    let single = all.iter().position(|t| t == "先").expect("xian 应有「先」");
    let pp = c.per_page(Some(ModeKind::Mix(0)));
    assert!(xian < 5 * pp && single < 5 * pp, "前提：都在可见区内");
    let walk = |to: usize| {
        let (page, sel, _) = view(&c);
        let from = page * pp + sel;
        let k = if to > from {
            keymap::VK_RIGHT
        } else {
            keymap::VK_LEFT
        };
        for _ in 0..from.abs_diff(to) {
            c.handle_key_event(&key(k));
        }
    };
    walk(xian);
    assert_eq!(preedit_of(&c), ";xi'an");
    walk(single);
    assert_eq!(preedit_of(&c), ";xian");
}

/// 分类标签：mix 的拼音、英文成员混排，Tab 进「单字」只剩单个汉字；点标签切「词组」，
/// 编码栏仍是快捷输入的形态（前缀 + 按高亮切分），不被按主路重算成空串。
#[test]
fn mix_grid_tabs_filter_and_keep_the_preedit() {
    let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
        return;
    };
    mix_type(&c, "SHIJIAN");
    c.handle_key_event(&key(keymap::VK_NEXT));
    assert!(view(&c).2.is_some(), "前提：展开");
    c.handle_key_event(&key(keymap::VK_TAB));
    assert_eq!(tab_of(&c), GridTab::Single);
    assert!(
        texts(&c)
            .iter()
            .all(|t| t.chars().count() == 1 && t.chars().all(wind_candidate::is_han)),
        "单字分类只留单个汉字: {:?}",
        texts(&c)
    );
    c.grid_click_tab(GridTab::Phrase);
    assert_eq!(tab_of(&c), GridTab::Phrase);
    let first = texts(&c)[0].clone();
    assert!(first.chars().count() >= 2, "词组分类首项: {first}");
    let st = c.state.lock().unwrap();
    let (page, sel) = (st.current_page, st.selected_index);
    let hl = st.candidates[page * c.per_page(st.active) + sel]
        .text
        .clone();
    drop(st);
    assert_eq!(hl, first, "锚点不在词组里时落到第一个");
    assert_eq!(
        preedit_of(&c),
        ";shi'jian",
        "点标签后编码栏仍按快捷输入形态"
    );
}

/// Esc 两段式：先收起、保留组合；再按一次退出快捷输入。
#[test]
fn mix_grid_escape_collapses_then_cancels() {
    let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
        return;
    };
    mix_type(&c, "SHI");
    c.handle_key_event(&key(keymap::VK_NEXT));
    assert!(matches!(
        c.handle_key_event(&key(keymap::VK_ESCAPE)),
        KeyAction::Consumed
    ));
    {
        let st = c.state.lock().unwrap();
        assert_eq!((st.grid_top, st.active), (None, Some(ModeKind::Mix(0))));
        assert_eq!(st.preedit, ";shi");
    }
    c.handle_key_event(&key(keymap::VK_ESCAPE));
    assert_eq!(c.state.lock().unwrap().active, None, "第二次 Esc 退出");
}

/// 续打与分步上屏后保持展开（同临拼）；打进越界字符落自由透镜即收起，退格回文本透镜
/// 又展开回来——展开意图（`grid_keep`）属于这一次组合，不随透镜丢。
#[test]
fn mix_grid_resumes_like_temp_pinyin() {
    let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
        return;
    };
    mix_type(&c, "SHI");
    c.handle_key_event(&key(keymap::VK_NEXT));
    c.handle_key_event(&key(u32::from(b'J')));
    assert_eq!(view(&c).2, Some(0), "继续打字保持展开");

    let shift_minus = KeyEventData {
        modifiers: MOD_SHIFT,
        ..key(keymap::VK_MINUS)
    };
    c.handle_key_event(&shift_minus); // `_` 越界 → 自由透镜
    assert_eq!(c.state.lock().unwrap().mix_buffer, "shij_");
    assert_eq!(view(&c).2, None, "自由透镜不展开");
    c.handle_key_event(&key(keymap::VK_BACK));
    assert_eq!(view(&c).2, Some(0), "退回文本透镜又展开");

    let idx = texts(&c)
        .iter()
        .position(|t| t == "是")
        .expect("shij 应有「是」");
    let pp = c.per_page(Some(ModeKind::Mix(0)));
    assert!(idx < 5 * pp, "前提：「是」在可见区内");
    for _ in 0..idx / pp {
        c.handle_key_event(&key(keymap::VK_DOWN));
    }
    for _ in 0..idx % pp {
        c.handle_key_event(&key(keymap::VK_RIGHT));
    }
    c.handle_key_event(&key(keymap::VK_SPACE));
    {
        let st = c.state.lock().unwrap();
        assert_eq!(st.committed_text, "是", "分步上屏");
        assert_eq!(st.mix_buffer, "j");
    }
    assert_eq!(view(&c).2, Some(0), "余下那段从首行展开");
}

/// 数字透镜不展开：候选凑够多页也不行（它的格式表依赖单页）。mix 没有分批加载，
/// `has_more` 恒为假，展开态只铺已取到的候选。
#[test]
fn mix_numeric_lens_does_not_expand() {
    let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
        return;
    };
    c.handle_key_event(&key(0xBA));
    c.handle_key_event(&key(u32::from(b'1')));
    c.handle_key_event(&key(u32::from(b'2')));
    let mut st = c.state.lock().unwrap();
    assert_eq!(st.mix_buffer, "12", "前提：数字透镜");
    assert!(!st.has_more);
    st.candidates = (0..PP * 3).map(|i| cand(&format!("{i}"))).collect();
    c.reset_candidate_view(&mut st);
    assert!(!c.try_expand_grid(&mut st), "数字透镜不展开");
}

/// 主路残留的 `has_more = true` 不能带进临拼 / 快捷输入：它们不分批加载，`expand_candidates`
/// 只认主路缓冲。残留来自真实路径：五笔打 `q` 空格上屏后它仍为真。
#[test]
fn stale_has_more_does_not_leak_into_overlays() {
    for (lead, mode) in [(0xC0u32, ModeKind::TempPinyin), (0xBA, ModeKind::Mix(0))] {
        let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
            return;
        };
        c.handle_key_event(&key(u32::from(b'Q')));
        assert!(
            matches!(
                c.handle_key_event(&key(keymap::VK_SPACE)),
                KeyAction::InsertText { .. }
            ),
            "前提：q 空格上屏"
        );
        assert!(c.state.lock().unwrap().has_more, "前提：主路残留 has_more");
        c.handle_key_event(&key(lead));
        for ch in "SHI".chars() {
            c.handle_key_event(&key(ch as u32));
        }
        let before = texts(&c);
        c.handle_key_event(&key(keymap::VK_NEXT));
        assert_eq!(view(&c).2, Some(0), "{mode:?}：展开");
        // 一路换行到底：途经可见区要预取的位置（`grid_ensure_loaded`）。
        for _ in 0..16 {
            c.handle_key_event(&key(keymap::VK_DOWN));
        }
        assert_eq!(texts(&c), before, "{mode:?}：候选不被主路扩容改写");
        let st = c.state.lock().unwrap();
        assert_eq!(st.active, Some(mode));
        assert!(!st.has_more, "{mode:?}：不分批加载");
    }
}

/// 展开态下序号逐格顺延时第 10 格标「0」，按 0 要选得到它（快捷输入与临拼）；
/// 收起态的 0 维持原语义：快捷输入作字面（落自由透镜），临拼顶屏高亮候选再接 `0`。
#[test]
fn zero_key_selects_the_tenth_cell_when_expanded() {
    for (lead, collapsed) in [(0xBAu32, None), (0xC0, Some("是0"))] {
        let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
            return;
        };
        let enter = |c: &Coordinator| {
            c.handle_key_event(&key(lead));
            for ch in "SHI".chars() {
                c.handle_key_event(&key(ch as u32));
            }
        };
        enter(&c);
        c.handle_key_event(&key(keymap::VK_NEXT));
        assert!(view(&c).2.is_some(), "{lead:#x}：前提：展开");
        let all = texts(&c);
        match c.handle_key_event(&key(keymap::VK_0)) {
            KeyAction::InsertText { text, .. } => assert_eq!(text, all[9], "{lead:#x}"),
            other => panic!("{lead:#x}：展开态按 0 应上屏第 10 格，实得 {other:?}"),
        }

        enter(&c);
        let act = c.handle_key_event(&key(keymap::VK_0));
        match collapsed {
            None => assert_eq!(preedit_of(&c), ";shi0", "收起态的 0 照旧是字面"),
            Some(want) => match act {
                KeyAction::InsertText { text, .. } => assert_eq!(text, want),
                other => panic!("临拼收起态按 0 应顶屏，实得 {other:?}"),
            },
        }
    }
}

/// 把当前 mix 候选换成三页合成候选再试展开，返回 (透镜, 是否展开)。页数不再是变量，
/// 测到的就只剩透镜判据。
fn mix_lens_expands(c: &Coordinator) -> (crate::handle_mode::MixLens, bool) {
    let mut st = c.state.lock().unwrap();
    st.candidates = (0..PP * 3).map(|i| cand(&format!("c{i}"))).collect();
    c.reset_candidate_view(&mut st);
    (c.mix_lens(&st), c.try_expand_grid(&mut st))
}

fn shifted(k: u32) -> KeyEventData {
    KeyEventData {
        modifiers: MOD_SHIFT,
        ..key(k)
    }
}

/// 词组透镜（分词开、缓冲带分词符）多页时展开。
#[test]
fn mix_phrase_lens_expands() {
    use crate::handle_mode::MixLens;
    let Some((c, _rx)) = mix_coord_with(wind_config::LayoutIntent::Horizontal, false, |cfg| {
        cfg.input.temp_english.phrase_seg = true;
    }) else {
        return;
    };
    mix_type(&c, "NI");
    c.handle_key_event(&key(keymap::VK_QUOTE));
    for ch in "HAO".chars() {
        c.handle_key_event(&key(ch as u32));
    }
    assert_eq!(c.state.lock().unwrap().mix_buffer, "ni'hao");
    assert_eq!(mix_lens_expands(&c), (MixLens::Phrase, true));
}

/// 自由透镜与大写英文词透镜（`free_input = off` 下 Shift+字母）多页也不展开。
#[test]
fn mix_free_and_english_lenses_do_not_expand() {
    use crate::handle_mode::MixLens;
    let Some((c, _rx)) = mix_coord(wind_config::LayoutIntent::Horizontal, false) else {
        return;
    };
    mix_type(&c, "SHI");
    c.handle_key_event(&shifted(keymap::VK_MINUS));
    assert_eq!(c.state.lock().unwrap().mix_buffer, "shi_");
    assert_eq!(mix_lens_expands(&c), (MixLens::Free, false));

    let Some((c, _rx)) = mix_coord_with(wind_config::LayoutIntent::Horizontal, false, |cfg| {
        cfg.schema.mix_modes[0].free_input = wind_config::config::FreeInputMode::Off;
    }) else {
        return;
    };
    c.handle_key_event(&key(0xBA));
    c.handle_key_event(&shifted(u32::from(b'H')));
    c.handle_key_event(&key(u32::from(b'E')));
    assert_eq!(c.state.lock().unwrap().mix_buffer, "He");
    assert_eq!(mix_lens_expands(&c), (MixLens::English, false));
}
