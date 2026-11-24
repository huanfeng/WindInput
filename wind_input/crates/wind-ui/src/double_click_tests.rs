use super::*;

const WIN: Duration = Duration::from_millis(500);
const SLOP: (i32, i32) = (4, 4);

#[test]
fn two_quick_clicks_in_place_make_a_double_click() {
    let mut d = DoubleClick::new();
    let t0 = Instant::now();
    assert!(!d.on_click(t0, 10, 10, WIN, SLOP), "第一下不是双击");
    assert!(d.on_click(t0 + Duration::from_millis(200), 11, 9, WIN, SLOP));
}

#[test]
fn slow_second_click_is_a_new_first_click() {
    let mut d = DoubleClick::new();
    let t0 = Instant::now();
    d.on_click(t0, 10, 10, WIN, SLOP);
    assert!(!d.on_click(t0 + Duration::from_millis(600), 10, 10, WIN, SLOP));
    // 慢的那一下成为新的第一下，紧跟一下即构成双击。
    assert!(d.on_click(t0 + Duration::from_millis(700), 10, 10, WIN, SLOP));
}

#[test]
fn moved_second_click_is_not_a_double_click() {
    let mut d = DoubleClick::new();
    let t0 = Instant::now();
    d.on_click(t0, 10, 10, WIN, SLOP);
    assert!(!d.on_click(t0 + Duration::from_millis(100), 13, 10, WIN, SLOP));
}

#[test]
fn triple_click_fires_only_once() {
    let mut d = DoubleClick::new();
    let t0 = Instant::now();
    d.on_click(t0, 10, 10, WIN, SLOP);
    assert!(d.on_click(t0 + Duration::from_millis(100), 10, 10, WIN, SLOP));
    assert!(
        !d.on_click(t0 + Duration::from_millis(200), 10, 10, WIN, SLOP),
        "第三下不应再截一张"
    );
}

#[test]
fn reset_breaks_the_pair() {
    let mut d = DoubleClick::new();
    let t0 = Instant::now();
    d.on_click(t0, 10, 10, WIN, SLOP);
    d.reset();
    assert!(!d.on_click(t0 + Duration::from_millis(100), 10, 10, WIN, SLOP));
}
