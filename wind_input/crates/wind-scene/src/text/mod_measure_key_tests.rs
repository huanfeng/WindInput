use super::{TextStyle, measure_key};

/// 字号 14、字重 400、指定字体族的基准样式。
fn base_style() -> TextStyle<'static> {
    TextStyle::new(14.0)
        .with_weight(400)
        .with_family(Some("微软雅黑"))
}

/// 同一组输入恒得同一个键（缓存能命中的前提）。
#[test]
fn same_inputs_yield_same_key() {
    assert_eq!(
        measure_key("你好", &base_style()),
        measure_key("你好", &base_style())
    );
}

/// 四项输入各自独立参与键——逐项只改一个，键都必须变。
#[test]
fn each_input_affects_key() {
    let s = base_style();
    let base = measure_key("你好", &s);
    assert_ne!(base, measure_key("你好啊", &s), "文本");
    assert_ne!(
        base,
        measure_key("你好", &TextStyle { size: 16.0, ..s }),
        "字号"
    );
    assert_ne!(base, measure_key("你好", &s.with_weight(700)), "字重");
    assert_ne!(
        base,
        measure_key("你好", &s.with_family(Some("宋体"))),
        "字体族"
    );
}

/// 字号必须按 `to_bits()` 精确入键，不能取整。
///
/// 字号是 DPI 缩放后的浮点：125% 下 12px→15.0、13px→16.25，150% 下 14px→21.0。
/// 若按 `as u32`/`round()` 入键，16.25 与 16.8 会撞进同一条缓存——注释与正文只差
/// 一两个像素时恰好落进这个区间，表现为某一档 DPI 下注释宽度突然用了正文的值。
#[test]
fn fractional_sizes_do_not_collide() {
    assert_ne!(
        measure_key("你好", &TextStyle::new(16.25)),
        measure_key("你好", &TextStyle::new(16.8)),
        "同一整数区间内的两个字号不得共用缓存键"
    );
}

/// `None`（用全局字体族）与显式指定不是一回事：`set_font_family` 只会让前者失效。
/// 两者若共用键，换字体后显式指定的条目会被连带清掉（性能损失，无正确性问题），
/// 更糟的是反过来——全局族的条目被显式族的值命中，直接就是错误宽度。
#[test]
fn none_family_differs_from_explicit() {
    let s = TextStyle::new(14.0);
    assert_ne!(
        measure_key("你好", &s),
        measure_key("你好", &s.with_family(Some("微软雅黑"))),
    );
}

/// 空串字体族经 `with_family` 归一成 `None`——统一在构造处收口，免得各调用点
/// 各自过滤，漏一处就多出一条与 `None` 等价却不同键的缓存。
#[test]
fn empty_family_normalizes_to_none() {
    let s = TextStyle::new(14.0);
    assert_eq!(
        measure_key("你好", &s),
        measure_key("你好", &s.with_family(Some(""))),
        "空串字体族应归一为 None"
    );
    assert_eq!(s.with_family(Some("  ")).family, None, "纯空白也应归一");
}
