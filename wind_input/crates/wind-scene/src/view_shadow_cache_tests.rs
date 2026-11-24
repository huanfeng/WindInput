use super::*;

/// 在 64×64 透明缓冲上画一次阴影，返回缓冲。内容盒固定落在 (16,16)。
fn shadow_buf(bw: f32, bh: f32, radius: f32, blur: f32, color: [u8; 4]) -> Vec<u8> {
    const W: u32 = 64;
    const H: u32 = 64;
    let mut buf = vec![0u8; (W * H * 4) as usize];
    paint_blur_shadow(
        &mut buf, W, H, 16.0, 16.0, bw, bh, radius, blur, 0.0, 0.0, 0.0, color,
    );
    buf
}

/// 基准形状：够小以完整落在 64×64 内，blur 够大以真正触发模糊路径。
fn base(color: [u8; 4]) -> Vec<u8> {
    shadow_buf(20.0, 20.0, 4.0, 3.0, color)
}

const BLACK: [u8; 4] = [0, 0, 0, 200];

/// 缓存**确实在工作**：同几何重绘不新增条目，不同几何才新增。
///
/// 这条是本模块其余用例的地基。没有它，那些"结果一致 / 结果不同"的断言在缓存
/// 完全失效（每帧照旧重算模糊）时也会全绿——而消除"每帧重算"正是这次改动的
/// 全部目的，测不出来就等于没测。
///
/// 换色不新增条目这一条同时正面验证了键的设计：蒙版与颜色无关。
#[test]
fn cache_stores_and_reuses_masks() {
    shadow_cache_clear();
    assert_eq!(shadow_cache_len(), 0, "起手应为空");
    let _ = base(BLACK);
    assert_eq!(shadow_cache_len(), 1, "首次绘制应入缓存一条");
    let _ = base(BLACK);
    assert_eq!(shadow_cache_len(), 1, "同几何重绘应命中，不得新增");
    let _ = base([255, 0, 0, 200]);
    assert_eq!(shadow_cache_len(), 1, "仅换色应命中（蒙版与颜色无关）");
    let _ = shadow_buf(28.0, 20.0, 4.0, 3.0, BLACK);
    assert_eq!(shadow_cache_len(), 2, "不同几何应新增条目");
}

/// 同参数重复绘制结果逐字节一致——缓存命中路径与首次构建路径必须等价。
#[test]
fn cache_hit_reproduces_first_render() {
    let first = base(BLACK);
    let second = base(BLACK);
    assert_eq!(first, second, "缓存命中不得改变渲染结果");
}

/// 阴影颜色**不在缓存键里**（缓存的是 alpha 蒙版），输出却必须随颜色变。
///
/// 这是"缓存蒙版而非着色像素"这一设计的判据：一旦有人把着色也塞进缓存，
/// 换了颜色仍会取到上一色的像素——而因为几何没变，缓存必命中，错误 100% 复现。
#[test]
fn color_is_not_cached_though_absent_from_key() {
    let black = base(BLACK);
    let red = base([255, 0, 0, 200]);
    assert_ne!(black, red, "换色必须改变输出（着色不得被缓存）");
}

/// 主题明暗切换只改颜色、不改几何——此时蒙版应复用而输出仍正确。
/// 与上一用例同源，但特意走"先浅后深"的顺序，覆盖缓存已被填充后再换色的路径。
#[test]
fn alpha_only_change_still_affects_output() {
    let opaque = base([0, 0, 0, 255]);
    let faint = base([0, 0, 0, 64]);
    assert_ne!(opaque, faint, "仅改阴影透明度也必须改变输出");
}

/// 盒尺寸进键：只改宽度，输出必须变。
#[test]
fn box_size_changes_output() {
    let a = shadow_buf(20.0, 20.0, 4.0, 3.0, BLACK);
    let b = shadow_buf(28.0, 20.0, 4.0, 3.0, BLACK);
    assert_ne!(a, b, "盒宽变化必须改变输出");
}

/// 圆角进键：直角与圆角的蒙版不同。
#[test]
fn radius_changes_output() {
    let sharp = shadow_buf(20.0, 20.0, 0.0, 3.0, BLACK);
    let round = shadow_buf(20.0, 20.0, 9.0, 3.0, BLACK);
    assert_ne!(sharp, round, "圆角变化必须改变输出");
}

/// 模糊半径进键：它同时决定蒙版尺寸（pad）与衰减，漏掉它错得最明显。
#[test]
fn blur_changes_output() {
    let tight = shadow_buf(20.0, 20.0, 4.0, 1.0, BLACK);
    let wide = shadow_buf(20.0, 20.0, 4.0, 6.0, BLACK);
    assert_ne!(tight, wide, "模糊半径变化必须改变输出");
}

/// 缓存超限后整体清空，不得影响正确性——清空前后同参数结果须一致。
/// 用 33 组不同几何（> `SHADOW_CACHE_CAP`）挤掉基准条目，再重画基准比对。
#[test]
fn eviction_preserves_correctness() {
    let before = base(BLACK);
    for i in 0..=SHADOW_CACHE_CAP {
        let _ = shadow_buf(10.0 + i as f32, 12.0, 2.0, 2.0, BLACK);
    }
    let after = base(BLACK);
    assert_eq!(before, after, "缓存清空后重建的蒙版须与首次一致");
}

/// 把阴影缓存的最后取用时刻挪到 `ago` 之前（模拟闲置）。
fn age_shadow_cache(ago: std::time::Duration) {
    let stale = std::time::Instant::now()
        .checked_sub(ago)
        .expect("单调时钟回拨");
    SHADOW_CACHE.with(|c| c.borrow_mut().last_use = Some(stale));
}

/// 设计 §7：阴影蒙版并入图片缓存那套 30 秒闲置回收——闲置到期整个清掉、不再要求唤醒；
/// 清掉后重画与清之前逐字节一致（按需重建）。
#[test]
fn idle_shadow_cache_is_evicted_and_rebuilt() {
    shadow_cache_clear();
    let before = base(BLACK);
    assert_eq!(shadow_cache_len(), 1);
    assert!(paint_cache_next_deadline().is_some(), "有驻留就要求唤醒");
    age_shadow_cache(crate::image_cache::IDLE_EVICT_AFTER + std::time::Duration::from_secs(1));
    assert!(
        paint_cache_evict_if_idle(std::time::Instant::now()),
        "闲置到期应回收"
    );
    assert_eq!(shadow_cache_len(), 0);
    assert_eq!(paint_cache_next_deadline(), None, "清空后不再要求唤醒");
    assert_eq!(base(BLACK), before, "回收后按需重建，结果一致");
    assert_eq!(shadow_cache_len(), 1);
}

/// 期间用过（命中也算）就不回收：最后取用时刻随每次绘制刷新。
#[test]
fn used_shadow_cache_is_kept() {
    shadow_cache_clear();
    let _ = base(BLACK);
    age_shadow_cache(crate::image_cache::IDLE_EVICT_AFTER + std::time::Duration::from_secs(1));
    let _ = base(BLACK); // 命中
    assert!(
        !paint_cache_evict_if_idle(std::time::Instant::now()),
        "刚用过不回收"
    );
    assert_eq!(shadow_cache_len(), 1);
}
