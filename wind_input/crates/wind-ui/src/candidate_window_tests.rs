use super::{CandidateScene, CandidateWindow, plan_present};

#[test]
fn present_full_when_not_on_screen() {
    assert_eq!(plan_present(None, (10, 20, 300, 100), true), (true, true));
}

#[test]
fn present_skips_identical_frame() {
    let g = (10, 20, 300, 100);
    assert_eq!(plan_present(Some(g), g, true), (false, false));
}

#[test]
fn present_moves_without_repaint_when_only_position_changed() {
    assert_eq!(
        plan_present(Some((10, 20, 300, 100)), (40, 20, 300, 100), true),
        (false, true)
    );
}

#[test]
fn present_repaints_in_place_when_pixels_changed() {
    let g = (10, 20, 300, 100);
    assert_eq!(plan_present(Some(g), g, false), (true, false));
}

/// 像素字节数相同但宽高互换（缓冲长度一样）也必须重推。
#[test]
fn present_repaints_when_size_changed_with_same_byte_count() {
    assert_eq!(
        plan_present(Some((10, 20, 300, 100)), (10, 20, 100, 300), true),
        (true, true)
    );
}

/// 主题位置偏移的方向语义：正值恒为「远离光标」——下方向下、上方向上。
///
/// 上方若照抄下方的加号，上翻时偏移会把窗口压向光标；只有上翻场景才露馅，故直接锁死。
#[test]
fn position_offset_always_pushes_away_from_caret() {
    // 下方：正值下移
    assert_eq!(CandidateWindow::apply_offset_y(300, false, 12), 312);
    // 上方：正值上移（同样是远离，不是压向光标）
    assert_eq!(CandidateWindow::apply_offset_y(300, true, 12), 288);
    // 零偏移两侧都是恒等
    assert_eq!(CandidateWindow::apply_offset_y(300, false, 0), 300);
    assert_eq!(CandidateWindow::apply_offset_y(300, true, 0), 300);
    // 负偏移语义对称（把窗口拉近光标）
    assert_eq!(CandidateWindow::apply_offset_y(300, false, -6), 294);
    assert_eq!(CandidateWindow::apply_offset_y(300, true, -6), 306);
}

/// 净锚点不含 Y 偏移——这是「偏移不左右上下翻转决策」的地基。
///
/// `below_ok`/`above_ok` 拿锚点跟工作区边界比：锚点含偏移时本该上翻的场景会被判成「上方也
/// 放不下」，落回下方再被钳到屏幕底、压住光标。
#[test]
fn caret_anchors_are_independent_of_y_offset() {
    let (caret_x, caret_y, caret_h, win_h) = (100, 200, 20, 80);
    let a = CandidateWindow::caret_anchors(caret_x, caret_y, caret_h, win_h, 0);
    // 净锚点即旧行为：下方=光标底端+gap，上方=底边贴光标顶端
    assert_eq!(a.1, caret_y + 2, "下方 = 光标底端 + gap");
    assert_eq!(a.2, caret_y - caret_h - win_h - 2, "上方 = 底边贴光标顶端");
    // x 偏移仍在锚点里（水平方向没有翻转决策，不受影响）
    let b = CandidateWindow::caret_anchors(caret_x, caret_y, caret_h, win_h, 5);
    assert_eq!(b.0 - a.0, 5, "x 偏移右移");
    assert_eq!((b.1, b.2), (a.1, a.2), "x 偏移不该动垂直锚点");
}

/// 「上方反转候选」只对竖排成立，横排必须原样保持 1..n。
///
/// 横排候选左右并列，"反转"只会把读序倒过来（1 跑到最右）。两个开关正交。
#[test]
fn flip_when_above_applies_to_vertical_only() {
    // (above, vertical, flip, swap) -> (flip_cands, swap_bands)
    let f = CandidateScene::above_layout;
    assert_eq!(
        f(true, true, true, false),
        (true, false),
        "竖排上方：反转生效"
    );
    assert_eq!(
        f(true, false, true, false),
        (false, false),
        "横排上方：反转不得生效"
    );
    assert_eq!(
        f(false, true, true, false),
        (false, false),
        "下方：一律不生效"
    );
    // swap 交换的是上下两条带，横竖排都成立
    assert_eq!(
        f(true, false, false, true),
        (false, true),
        "横排上方：交换仍生效"
    );
    assert_eq!(f(true, true, false, true), (false, true));
    // 叠加：竖排两个都开 → 两个都生效；横排只剩 swap
    assert_eq!(f(true, true, true, true), (true, true));
    assert_eq!(f(true, false, true, true), (false, true));
}

/// 插入符位置一律夹到合法字符边界——`split_at` 落在字符中间会 panic 掉 UI 线程。
#[test]
fn clamp_caret_never_splits_a_char() {
    assert_eq!(CandidateWindow::clamp_caret("nihao", 3), 3);
    assert_eq!(CandidateWindow::clamp_caret("nihao", 5), 5, "末尾合法");
    assert_eq!(CandidateWindow::clamp_caret("nihao", 99), 5, "越界截到末尾");
    assert_eq!(CandidateWindow::clamp_caret("", 4), 0, "空串恒 0");
    // 「你」占 3 字节：落在 1/2 时退回 0，落在 3 是边界
    assert_eq!(CandidateWindow::clamp_caret("你hao", 1), 0);
    assert_eq!(CandidateWindow::clamp_caret("你hao", 2), 0);
    assert_eq!(CandidateWindow::clamp_caret("你hao", 3), 3);
    assert_eq!(CandidateWindow::clamp_caret("你hao", 4), 4);
}

/// clamp 后切分恒安全（本测试的意义在于：若 clamp 失效，split_at 会 panic）。
#[test]
fn clamped_caret_is_safe_to_split() {
    for text in ["", "nihao", "你好hao", "·ni'hao"] {
        for caret in 0..=text.len() + 2 {
            let c = CandidateWindow::clamp_caret(text, caret);
            let (head, tail) = text.split_at(c);
            assert_eq!(format!("{head}{tail}"), text);
        }
    }
}

/// 固定位置的两个坐标系必须严格互逆。
///
/// 落盘的 custom_x/y 是内容左上，Win32 定位用窗口左上（含软阴影扩边）：固定定位走
/// content→window，拖动落定走 window→content。不互逆时每轮「拖动 → 落盘 → 重显」都多减
/// 一次阴影，候选窗往左上爬。
#[cfg(windows)]
#[test]
fn content_window_coord_roundtrip_is_lossless() {
    // 覆盖有/无阴影、负坐标（副屏在主屏左侧时屏幕坐标为负）
    for (content, ml, mt) in [
        ((100, 200), 12u32, 10u32),
        ((0, 0), 0, 0),
        ((-1920, 40), 8, 8),
        ((3, 7), 1, 0),
    ] {
        let w = CandidateWindow::content_to_window(content, ml, mt);
        let back = CandidateWindow::window_to_content(w, ml as i32, mt as i32);
        assert_eq!(back, content, "content={content:?} margin=({ml},{mt})");
    }
}

/// 阴影扩边确实被减掉了——若 content_to_window 退化成恒等函数，
/// 上面的 round-trip 仍会通过，但固定位置会整体偏移一个阴影的量。
#[cfg(windows)]
#[test]
fn content_to_window_subtracts_the_shadow_margin() {
    assert_eq!(
        CandidateWindow::content_to_window((100, 200), 12, 10),
        (88, 190)
    );
}
