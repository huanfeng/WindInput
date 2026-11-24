//! per-app 兼容规则：自动配对开关、智能符号方案、光标坐标校正。
use super::*;
use wind_config::config::SmartMethod;

fn coord_with(cfg: Config) -> Arc<Coordinator> {
    Coordinator::new_headless(cfg, None)
}

/// CaretData 无 `Default`，测试里显式构造（字段少，且显式写出更能看清哪些参与变换）。
fn caret(x: i32, y: i32, height: i32, cs_x: i32, cs_y: i32) -> CaretData {
    CaretData {
        x,
        y,
        height,
        composition_start_x: cs_x,
        composition_start_y: cs_y,
        source: 0,
        composition_rect: None,
    }
}

fn pair_cfg() -> Config {
    let mut cfg = Config::default();
    cfg.input.auto_pair.chinese = true;
    cfg.input.auto_pair.english = true;
    cfg.input.auto_pair.chinese_pairs = vec!["（）".to_string()];
    cfg.input.auto_pair.english_pairs = vec!["()".to_string()];
    cfg
}

/// per-app 关闭后，`active_pairs` 在**中英两种标点态**都必须返回 None。
///
/// 分别断言两种标点态而不是只测一种：全局开关本来就是 chinese / english 两个独立字段，
/// 只在其中一条上加闸门是本仓反复出现的「半截修复」形态。
#[test]
fn auto_pair_rule_off_kills_both_punct_modes() {
    let c = coord_with(pair_cfg());
    assert!(c.active_pairs(true).is_some(), "默认（未配规则）应跟随全局");
    assert!(c.active_pairs(false).is_some());

    c.active_compat.lock().unwrap().auto_pair = Some(false);
    assert!(c.active_pairs(true).is_none(), "中文标点态应被关掉");
    assert!(c.active_pairs(false).is_none(), "英文标点态应被关掉");

    // 显式启用 = 跟随全局的开关，不是无条件开。
    c.active_compat.lock().unwrap().auto_pair = Some(true);
    assert!(c.active_pairs(true).is_some());
}

/// `is_auto_pair_char` 建立在 `active_pairs` 之上，规则关闭后必须一并失效——
/// 它是「智能符号与自动配对互斥」的判据，若还认为字符参与配对，智能符号会被误让位。
#[test]
fn auto_pair_rule_off_releases_smart_symbol_interlock() {
    let c = coord_with(pair_cfg());
    c.state.lock().unwrap().chinese_punct = true;
    {
        let state = c.state.lock().unwrap();
        assert!(c.is_auto_pair_char(&state, '（'), "默认应认为参与配对");
    }

    c.active_compat.lock().unwrap().auto_pair = Some(false);
    {
        let state = c.state.lock().unwrap();
        assert!(!c.is_auto_pair_char(&state, '（'), "规则关闭后互锁应解除");
    }
}

/// 光标坐标校正：两个消费点（`apply_focus_caret` / `handle_caret_update`）共用
/// `apply_caret_compat`，此处直接锁住那个变换本身。`dpi_scale_for_point` 在
/// `cfg(test)` 下恒回退 1.0（见其文档），故这里的期望坐标等同于 dp 值本身；
/// 缩放本身的数学在 [`dp_offset_to_pixels_scales_with_dpi`] 单独覆盖。
#[test]
fn caret_offset_shifts_coordinates() {
    let c = coord_with(Config::default());
    {
        let mut ac = c.active_compat.lock().unwrap();
        ac.caret_offset_x = -3;
        ac.caret_offset_y = 7;
    }
    let mut data = caret(100, 200, 20, 0, 0);
    c.apply_caret_compat(&mut data);
    assert_eq!((data.x, data.y), (97, 207));
    assert_eq!(data.height, 20, "偏移不应改动行高");
    // compStart 为 0 表示"未提供"，不能被平移成一个假坐标。
    assert_eq!((data.composition_start_x, data.composition_start_y), (0, 0));

    // compStart 有真值时随之平移，保持与 caret 的锚点关系。
    let mut with_cs = caret(100, 200, 20, 50, 180);
    c.apply_caret_compat(&mut with_cs);
    assert_eq!(
        (with_cs.composition_start_x, with_cs.composition_start_y),
        (47, 187)
    );
}

/// dp→物理像素换算：同一份 dp 配置在不同缩放的显示器上，换算出的物理像素偏移应随
/// 缩放等比放大，这正是本功能要解决的「多屏不同缩放下无法完美兼容」的核心数学。
#[test]
fn dp_offset_to_pixels_scales_with_dpi() {
    assert_eq!(
        dp_offset_to_pixels(12, -2, 1.0),
        (12, -2),
        "100% 缩放下 dp==物理像素"
    );
    assert_eq!(
        dp_offset_to_pixels(12, -2, 1.5),
        (18, -3),
        "150% 缩放等比放大"
    );
    assert_eq!(dp_offset_to_pixels(12, -2, 2.0), (24, -4), "200% 缩放翻倍");
    assert_eq!(
        dp_offset_to_pixels(3, 0, 1.25),
        (4, 0),
        "四舍五入到最近物理像素"
    );
}

/// `caret_offset_shifts_coordinates` 只在 `cfg(test)` 恒 1.0 的 scale 下测过
/// `apply_caret_compat`，证明不了非 1.0 缩放真的接了进去（`dp_offset_to_pixels_scales_with_dpi`
/// 也只测纯数学，不碰 `apply_dp_offset` 这条落地路径）。此处直接调 `apply_dp_offset`
/// 本体、显式传 150% 缩放，钉住 caret 与 composition_start 两处都按 scale 换算
/// （2026-08-17 code review 指出的 test-wiring gap）。
#[test]
fn apply_dp_offset_wires_scale_into_full_transform() {
    let mut data = caret(100, 200, 20, 50, 180);
    apply_dp_offset(&mut data, -3, 7, 1.5);
    assert_eq!(
        (data.x, data.y),
        (100 - 5, 200 + 11),
        "150% 下 -3dp→-4.5→round(-5)px，7dp→10.5→round(11)px"
    );
    assert_eq!(
        (data.composition_start_x, data.composition_start_y),
        (50 - 5, 180 + 11),
        "组合起点须按同一 scale 同步平移，不能只动 caret"
    );
}

/// 零偏移必须是彻底的 no-op：未配规则的应用绝不能因为这条链路而坐标漂移。
#[test]
fn caret_offset_zero_is_noop() {
    let c = coord_with(Config::default());
    let orig = caret(100, 200, 20, 50, 180);
    let mut data = orig;
    c.apply_caret_compat(&mut data);
    assert_eq!((data.x, data.y), (orig.x, orig.y));
    assert_eq!(
        (data.composition_start_x, data.composition_start_y),
        (orig.composition_start_x, orig.composition_start_y)
    );
}

/// 智能符号方案：per-app 覆盖优先，未配则跟随全局。
#[test]
fn smart_method_per_app_overrides_global() {
    let mut cfg = Config::default();
    cfg.input.symbol.smart_method = SmartMethod::DeleteReplace;
    let c = coord_with(cfg);
    assert_eq!(c.effective_smart_method(), SmartMethod::DeleteReplace);

    c.active_compat.lock().unwrap().smart_method = Some(SmartMethod::HoldComposition);
    assert_eq!(c.effective_smart_method(), SmartMethod::HoldComposition);

    c.active_compat.lock().unwrap().smart_method = None;
    assert_eq!(
        c.effective_smart_method(),
        SmartMethod::DeleteReplace,
        "清除规则应回到全局值"
    );
}
