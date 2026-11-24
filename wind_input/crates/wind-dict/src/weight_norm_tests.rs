use super::*;

/// 虎码实测参数（median=397、max=10,359,470）——本设计的主要目标场景。
fn tiger() -> WeightNorm {
    WeightNorm::from_parts(397, 10_359_470, "log", DEFAULT_TARGET).unwrap()
}

/// 锚点必须精确落位：中位 → target，最大值 → 上界。
#[test]
fn anchors_land_exactly() {
    let n = tiger();
    assert_eq!(n.apply(397), DEFAULT_TARGET as i32, "中位须映到 target");
    assert_eq!(n.apply(10_359_470), WEIGHT_RANGE_MAX, "最大值须映到上界");
}

/// ★ **不归零**——这是选对数而非线性的全部理由。
///
/// 线性压缩虎码的因子约 1035，p50 的 397 会被整除成 0，与已拆除的
/// `PINYIN_TIER_SCALE`(÷100) 把拼音 p50=34 归零是同一个错误。
#[test]
fn never_collapses_the_low_end_to_zero() {
    let n = tiger();
    for w in [1, 2, 10, 100, 396] {
        assert!(n.apply(w) >= 1, "权重 {w} 不得被压成 0");
    }
    // 反向对照：线性模式下同样不归零（因为用的是浮点 + clamp，不是整数除法）。
    let lin = WeightNorm::from_parts(397, 10_359_470, "linear", DEFAULT_TARGET).unwrap();
    assert!(lin.apply(1) >= 1);
}

/// 保序：归一化不得改变库内任何两条的先后。
#[test]
fn preserves_order() {
    let n = tiger();
    let src = [1, 50, 397, 1_000, 10_000, 18_498, 1_000_000, 10_359_470];
    let out: Vec<i32> = src.iter().map(|&w| n.apply(w)).collect();
    for i in 1..out.len() {
        assert!(
            out[i] >= out[i - 1],
            "保序失败：{} → {} 但 {} → {}",
            src[i - 1],
            out[i - 1],
            src[i],
            out[i]
        );
    }
}

/// ★ **自洽性**：把守约词库（五笔 median=941、max=9999）代进去应当近似恒等。
///
/// 这条是公式没有偏心的证据——若它把五笔也改得面目全非，说明锚点选错了。
#[test]
fn well_behaved_dict_is_near_identity() {
    let n = WeightNorm::from_parts(941, 9_999, "log", DEFAULT_TARGET).unwrap();
    assert_eq!(n.apply(941), 1_000);
    assert_eq!(n.apply(9_999), WEIGHT_RANGE_MAX);
    // 中段偏差在可接受范围：对数映射会把低段略微拉高，但不改变量级。
    let mid = n.apply(3_544); // 五笔四码全码的 max
    assert!(
        (3_000..=8_000).contains(&mid),
        "五笔四码 max 归一化后应仍在中高段，实际 {mid}"
    );
}

/// 权重 0（无权重列）**原样返回**：它是「未定义」不是「最低」，
/// 归一化不该凭空造值——那是 `default_weight` 的职责。
#[test]
fn zero_weight_is_left_alone() {
    assert_eq!(tiger().apply(0), 0);
    assert_eq!(tiger().apply(-5), -5);
}

/// 参数不自洽一律返回 None，调用方据此告警——不可静默退化成恒等映射。
#[test]
fn rejects_incoherent_params() {
    assert!(
        WeightNorm::from_parts(0, 100, "log", 1000).is_none(),
        "median 须 > 0"
    );
    assert!(
        WeightNorm::from_parts(100, 100, "log", 1000).is_none(),
        "max 须 > median"
    );
    assert!(
        WeightNorm::from_parts(100, 50, "log", 1000).is_none(),
        "max 须 > median"
    );
    assert!(
        WeightNorm::from_parts(100, 1000, "log", WEIGHT_RANGE_MAX as i64).is_none(),
        "target 须 < 上界"
    );
    // target 省略（0）→ 取默认，仍然有效。
    assert!(WeightNorm::from_parts(397, 10_000, "log", 0).is_some());
}

/// ★★ **同一映射施加于多个词库时，库间相对关系不变**——这是「方案级而非按库」的全部理由。
///
/// 按库配过一版，实测反转：`aaah` 下主库「葡萄牙」(1485) 本压过扩展库「欧莱雅」(1170)，
/// 给扩展库单独配归一化后欧莱雅升到 3328、反超；而作者写的 `base_order = 1` 救不回来
/// （`better()` 是 `weight 降 → base_order 升`，weight 在前）。
/// 根因是**两个不同的映射函数之间没有保序保证**，不是参数没调好。
#[test]
fn one_map_across_dicts_preserves_cross_dict_order() {
    let n = WeightNorm::from_parts(397, 343_880, "log", DEFAULT_TARGET).unwrap();
    // (主库权重, 扩展库权重) —— 每一对里主库都更高
    let pairs = [(1485, 1170), (9999, 2125), (500, 499), (10_359_470, 18_526)];
    for (main, extra) in pairs {
        let (m, e) = (n.apply(main), n.apply(extra));
        assert!(
            m >= e,
            "库间序反转：主库 {main}→{m} 不应低于扩展库 {extra}→{e}"
        );
    }
}

/// 超出声明 max 的条目被 clamp 到上界，不会溢出成更大的数。
#[test]
fn beyond_declared_max_clamps() {
    let n = tiger();
    assert_eq!(n.apply(i32::MAX), WEIGHT_RANGE_MAX);
}
