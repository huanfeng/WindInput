//! 候选菜单的**绝对下标**入口（移动端候选长按菜单）：`candidate_menu_items` 构造、
//! `candidate_or_quick_format_op_at` 执行。
//!
//! 移动端候选栏可横滑、展开面板看全部，手里只有全量绝对下标；桌面右键是页内下标。
//! 这里守三件事：
//! - 第二页以后的候选按绝对下标取得到菜单，且与桌面翻到那页右键弹出的菜单**逐项一致**
//!   （两端共用一份构造，分叉的表现是「桌面能置顶、手机上是灰的」，不报错）；
//! - 执行不借道翻页、不受「只能操作当页」限制：第二页的候选能置顶、能隐藏；
//! - 暂停学习不拦这些显式操作。
//!
//! 夹具：wubi86，`aa` 有多页候选（首页之外的才测得出绝对下标）。
//! 词典缺失时自动跳过——⚠️ 跳过时计数照绿，确认真的跑了看 `--nocapture` 里有没有「跳过：」。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_keys::keymap::VK_NEXT;
use wind_ui_types::{CandidateOp, MenuItemSpec, MenuKind, UiCommand};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn data_ready() -> bool {
    let ok = data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists();
    if !ok {
        eprintln!("跳过：词库不存在");
    }
    ok
}

fn press_vk(c: &Coordinator, vk: u32) {
    c.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    });
}

fn type_code(c: &Coordinator, code: &str) {
    for ch in code.chars() {
        press_vk(c, (ch.to_ascii_uppercase() as u32) & 0xFF);
    }
}

/// 带 UI 通道与用户目录（shadow 规则要落 redb）。目录名带 pid：并行跑测试时固定名会互删夹具。
fn setup(tag: &str) -> (Arc<Coordinator>, Receiver<UiCommand>, PathBuf) {
    let user = std::env::temp_dir().join(format!("wind_cand_menu_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = false;
    let (c, rx) = Coordinator::new_headless_with_ui_at(cfg, Some(&data_dir()), Some(&user));
    // 照移动端（`MobileCore::new`）设：没有光标坐标可等，否则首帧延迟显示会把候选推送
    // 压住，「执行后推送了刷新」就断言不到。
    c.set_caret_independent(true);
    (c, rx, user)
}

/// 每页条数（`aa` 首页的长度）。前提：`aa` 至少两页，否则「第二页」无从谈起。
fn page_size_with_second_page(c: &Coordinator) -> usize {
    let pp = c.debug_page_texts().len();
    let total = c.debug_candidate_count();
    assert!(
        pp > 0 && total > pp + 1,
        "前提：aa 应有第二页（每页 {pp}，共 {total}）"
    );
    pp
}

fn desktop_menu(c: &Coordinator, rx: &Receiver<UiCommand>, page_local: usize) -> Vec<MenuItemSpec> {
    let _ = rx.try_iter().count();
    c.debug_show_candidate_menu(page_local);
    rx.try_iter()
        .find_map(|m| match m {
            UiCommand::ShowCandidateMenu { items, .. } => Some(items),
            _ => None,
        })
        .expect("桌面候选菜单没弹")
}

fn enabled_of(items: &[MenuItemSpec], op: CandidateOp) -> bool {
    items
        .iter()
        .find(|i| i.kind == MenuKind::Op(op))
        .unwrap_or_else(|| panic!("菜单缺 {op:?}: {items:?}"))
        .enabled
}

/// 第二页的候选：绝对下标取到的菜单 == 桌面翻到那页右键弹出的菜单（文案、启用态、顺序）。
///
/// 先按绝对下标把第二页首个后移一位，让它带上规则（「恢复默认」可用）——同页其它码表单字
/// 的菜单彼此一模一样，不做这一步的话，下标折算错了（取到别的候选）也比对得上。
#[test]
fn menu_by_absolute_index_matches_desktop_on_second_page() {
    if !data_ready() {
        return;
    }
    let (c, rx, user) = setup("match");
    type_code(&c, "aa");
    let pp = page_size_with_second_page(&c);
    let target = c.debug_all_candidate_texts()[pp].clone();
    c.candidate_or_quick_format_op_at(CandidateOp::MoveDown, pp);
    let n = pp + 1; // 第二页第 2 个
    assert_eq!(c.debug_all_candidate_texts()[n], target, "后移一位");

    // 首页时取：绝对下标与当前页无关。
    assert_eq!(c.debug_page_info().0, 0, "前提：仍在首页");
    let abs = c.candidate_menu_items(n).expect("第二页候选应有菜单");
    assert!(
        enabled_of(&abs, CandidateOp::MoveTop),
        "非首位应可置顶: {abs:?}"
    );
    assert!(
        enabled_of(&abs, CandidateOp::Reset),
        "有规则应可恢复: {abs:?}"
    );

    press_vk(&c, VK_NEXT);
    assert_eq!(c.debug_page_info().0, 1, "前提：已翻到第二页");
    assert_eq!(c.debug_page_texts()[1], target, "前提：第二页第 2 个就是它");
    let desk = desktop_menu(&c, &rx, 1);
    assert_eq!(abs, desk, "绝对下标菜单应与桌面右键同一候选的菜单逐项一致");
    // 翻页后再按绝对下标取，仍是同一份（不随当前页漂移）。
    assert_eq!(c.candidate_menu_items(n).as_ref(), Some(&desk));
    let _ = std::fs::remove_dir_all(&user);
}

/// 无候选 / 越界：没有菜单。
#[test]
fn out_of_range_index_has_no_menu() {
    if !data_ready() {
        return;
    }
    let (c, _rx, user) = setup("oob");
    assert!(c.candidate_menu_items(0).is_none(), "无候选时不应有菜单");
    type_code(&c, "aa");
    let total = c.debug_candidate_count();
    assert!(
        c.candidate_menu_items(total - 1).is_some(),
        "前提：末项有菜单"
    );
    assert!(
        c.candidate_menu_items(total).is_none(),
        "越界下标不应有菜单"
    );
    let _ = std::fs::remove_dir_all(&user);
}

/// 置顶第二页的候选 → 它成为首选，且刷新经 UI 通道推送（移动端靠这条拿新列表）；
/// 首位菜单的「恢复默认」随之可用，点了回到原位。暂停学习期间照做（显式操作不受暂停影响）。
#[test]
fn move_top_by_absolute_index_then_reset() {
    if !data_ready() {
        return;
    }
    let (c, rx, user) = setup("top");
    c.set_learning_suspended(true);
    type_code(&c, "aa");
    let pp = page_size_with_second_page(&c);
    let n = pp + 1;
    let before = c.debug_all_candidate_texts();
    let target = before[n].clone();

    let _ = rx.try_iter().count();
    c.candidate_or_quick_format_op_at(CandidateOp::MoveTop, n);
    let after = c.debug_all_candidate_texts();
    assert_eq!(after[0], target, "置顶后应成首选: {after:?}");
    assert!(
        rx.try_iter()
            .any(|m| matches!(m, UiCommand::UpdateCandidates { .. })),
        "执行后应推送候选刷新"
    );

    let top = c.candidate_menu_items(0).unwrap();
    assert!(
        enabled_of(&top, CandidateOp::Reset),
        "有规则后恢复默认应可用"
    );
    c.candidate_or_quick_format_op_at(CandidateOp::Reset, 0);
    assert_eq!(
        c.debug_all_candidate_texts(),
        before,
        "恢复默认后回到原顺序"
    );
    let _ = std::fs::remove_dir_all(&user);
}

/// 隐藏第二页的系统词 → 候选里不再有它；同一编码重打仍不出（规则已落库，不是一次性过滤）。
#[test]
fn delete_by_absolute_index_hides_it() {
    if !data_ready() {
        return;
    }
    let (c, _rx, user) = setup("del");
    type_code(&c, "aa");
    let pp = page_size_with_second_page(&c);
    let n = pp + 1;
    let target = c.debug_all_candidate_texts()[n].clone();
    let menu = c.candidate_menu_items(n).unwrap();
    assert!(
        enabled_of(&menu, CandidateOp::Delete),
        "删除应可用: {menu:?}"
    );

    c.candidate_or_quick_format_op_at(CandidateOp::Delete, n);
    assert!(
        !c.debug_all_candidate_texts().contains(&target),
        "删除后不应再出现 {target}"
    );
    press_vk(&c, 0x1B); // Esc 清空
    type_code(&c, "aa");
    assert!(
        !c.debug_all_candidate_texts().contains(&target),
        "重打后仍不应出现 {target}"
    );
    let _ = std::fs::remove_dir_all(&user);
}
