use super::*;
use crate::text::dwrite::ColorRun;

fn window(upright: bool) -> CandidateWindow {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.set_orientation(false, upright, upright);
    w
}

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// 「a你b」：红 `a你`（0..4）、蓝 `b`（4..5）。
fn runs() -> Vec<ColorRun> {
    vec![
        ColorRun {
            start: 0,
            end: 4,
            rgba: RED,
        },
        ColorRun {
            start: 4,
            end: 5,
            rgba: BLUE,
        },
    ]
}

fn build(w: &CandidateWindow) -> View {
    let runs = runs();
    w.scene_input(false)
        .upright_text("a你b", None, |seg, _, off| {
            View::leaf(seg, [0, 0, 0, 255]).color_runs(ColorRun::slice(&runs, off, off + seg.len()))
        })
}

/// 不切格：偏移为 0，一个叶子拿全部区间，原样。
#[test]
fn unsplit_leaf_keeps_all_runs() {
    let v = build(&window(false));
    assert_eq!(v.text.as_deref(), Some("a你b"));
    assert_eq!(v.color_runs, runs());
}

/// 直立态逐格切：每格拿到自己范围内的那段、平移到格内偏移。
#[test]
fn upright_cells_get_their_own_slice() {
    let v = build(&window(true));
    // 每格是扶正包裹层（旋转节点）里的一个叶子。
    let cells: Vec<(&str, &[ColorRun])> = v
        .children
        .iter()
        .map(|c| {
            let leaf = &c.children[0];
            (leaf.text.as_deref().unwrap(), leaf.color_runs.as_slice())
        })
        .collect();
    let one = |end, rgba| {
        vec![ColorRun {
            start: 0,
            end,
            rgba,
        }]
    };
    assert_eq!(
        cells,
        vec![
            ("a", one(1, RED).as_slice()),
            ("你", one(3, RED).as_slice()),
            ("b", one(1, BLUE).as_slice()),
        ]
    );
}
