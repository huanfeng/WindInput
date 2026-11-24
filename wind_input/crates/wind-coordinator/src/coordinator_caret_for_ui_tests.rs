//! 「用于 UI 定位的光标坐标」闸门（[`Coordinator::resolve_caret_for_ui`]）。
//!
//! 候选窗与状态气泡共用本函数；测试同时钉住判据与回退（无效坐标不能让气泡飞到主屏左上角）。
use super::*;

fn coord() -> Arc<Coordinator> {
    Coordinator::new_headless(Config::default(), None)
}

/// 负坐标是合法的：主屏左上角才是虚拟桌面原点，左侧/上方的副屏整块为负。
#[test]
fn negative_coords_are_valid() {
    assert!(Coordinator::caret_is_valid(-1200, 500, 20), "左侧副屏");
    assert!(Coordinator::caret_is_valid(300, -600, 20), "上方副屏");
}

#[test]
fn sentinel_and_degenerate_inputs_are_invalid() {
    assert!(
        !Coordinator::caret_is_valid(0, 0, 20),
        "(0,0) 是宿主「没有坐标」的哨兵，不能当成主屏左上角来用"
    );
    assert!(
        !Coordinator::caret_is_valid(500, 500, 0),
        "height=0 = 宿主尚未 reflow，整组坐标不可信"
    );
    assert!(!Coordinator::caret_is_valid(40000, 500, 20), "越界脏数据");
    assert!(
        !Coordinator::caret_is_valid(i32::MIN, 500, 20),
        "未初始化极值必须安全判无效，不得在 abs() 溢出"
    );
}

/// 无效坐标必须回退到最近一次有效坐标，且该坐标可以在副屏（负值）。
#[test]
fn invalid_falls_back_to_last_valid_on_secondary_monitor() {
    let c = coord();
    assert_eq!(
        c.resolve_caret_for_ui(-1500, 400, 20),
        (-1500, 400, 20, true),
        "前置条件——副屏坐标应被认为有效并记为最近有效值"
    );
    assert_eq!(
        c.resolve_caret_for_ui(0, 0, 20),
        (-1500, 400, 20, true),
        "(0,0) 必须回退到副屏那条，而不是留在主屏原点"
    );
}

/// 尚无任何历史有效坐标时如实报 `valid=false`，由调用方决定临时显示 / 待重定位。
#[test]
fn no_history_reports_invalid() {
    let c = coord();
    assert_eq!(c.resolve_caret_for_ui(0, 0, 20), (0, 0, 20, false));
}
