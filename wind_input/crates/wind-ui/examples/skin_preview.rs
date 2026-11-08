//! 皮肤预览：按主题渲染一帧候选窗并存成 PNG，不开窗口、不连服务。用于本机目视检查异形皮肤
//! （设计 `docs/design/theme-skin-animation.md`）。
//!
//! ```text
//! cargo run -p wind-ui --example skin_preview -- <主题目录> <主题名> <输出.png> [缩放] [vertical]
//! ```
//!
//! Linux 默认是 mock 文字后端（不出字形）；加 `--features linux-host` 用真实字形。

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    use std::path::PathBuf;
    use wind_ui::candidate_window::{CandidateWindow, CandidateWindowConfig};
    use wind_ui_types::CandidateItem;

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("用法: skin_preview <主题目录> <主题名> <输出.png> [缩放] [vertical]");
        std::process::exit(2);
    }
    let dirs = vec![
        PathBuf::from(&args[1]),
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data/themes")),
    ];
    let scale: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let vertical = args.get(5).is_some_and(|s| s == "vertical");
    wind_ui::dpi::set_host_scale(scale);
    let theme = wind_theme::load_resolved_dirs(&dirs, &args[2], false).expect("加载主题");
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).expect("候选窗");
    w.set_theme(theme);
    w.set_orientation(vertical, false, false);
    let cands = ["你好", "拟好", "尼好", "泥好", "倪好"]
        .iter()
        .map(|t| CandidateItem {
            text: t.to_string(),
            text_styled: Default::default(),
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: Default::default(),
            comment_above: Default::default(),
            no_index: false,
        })
        .collect();
    w.update("nihao", 5, "", cands, 0, -1, 1, 3);
    w.set_position(200, 200, 20, true);
    w.show();
    w.capture_to_file(std::path::Path::new(&args[3]))
        .expect("写 PNG");
}

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    eprintln!("skin_preview 只在 Linux 上用（Windows / macOS 请看实机）");
}
