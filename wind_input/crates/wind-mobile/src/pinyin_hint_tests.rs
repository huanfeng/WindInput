use super::*;
use wind_config::HintScope;

/// 自造夹具：`pinguo` 精确命中「拼过」，in/ing 模糊命中「苹果」「平过」。
/// ⚠️ 目录名带 pid：多会话并行跑测试时固定名会互删夹具。
fn core(tag: &str, hint: bool, scope: HintScope) -> (MobileCore, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("wind_mobile_fh_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("data/schemas");
    std::fs::create_dir_all(schemas.join("fh")).unwrap();
    std::fs::create_dir_all(dir.join("user")).unwrap();
    std::fs::write(
        schemas.join("fh/mini.dict.yaml"),
        "---\nname: mini\nversion: \"1\"\nsort: by_weight\n...\n\
             拼过\tpin guo\t5000\n苹果\tping guo\t3000\n平过\tping guo\t100\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("fh.schema.toml"),
        "[schema]\nid = \"fh\"\nname = \"fh\"\n[engine]\ntype = \"pinyin\"\n\
             [[dictionaries]]\nid = \"mini\"\npath = \"fh/mini.dict.yaml\"\n\
             type = \"rime_pinyin\"\ndefault = true\n",
    )
    .unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["fh".into()];
    cfg.schema.active = "fh".into();
    cfg.input.default.chinese_mode = true;
    let fz = &mut cfg.schema.pinyin.fuzzy;
    fz.enabled = true;
    fz.in_ing = true;
    let h = &mut cfg.input.correction.hint;
    h.enabled = hint;
    h.scope = scope;
    let (coord, _rx) = Coordinator::new_session(
        cfg,
        Some(&dir.join("data")),
        Some(&dir.join("user")),
        InputProfile::mobile(),
    );
    let core = MobileCore {
        coord,
        warming: Arc::new(AtomicBool::new(false)),
        data_dir: dir.join("user"),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    (core, dir)
}

fn pinguo(tag: &str, hint: bool, scope: HintScope) -> CandidateWindow {
    let (m, dir) = core(tag, hint, scope);
    for ch in "pinguo".chars() {
        dispatch_key(&m.coord, ch.to_ascii_uppercase() as u32, 0);
    }
    let w = m.candidates(0, MAX_WINDOW);
    let _ = std::fs::remove_dir_all(dir);
    w
}

fn joined(segs: &[TextSegment]) -> String {
    segs.iter().map(|s| s.text.as_str()).collect()
}

#[test]
fn candidates_carry_hint_segments() {
    let w = pinguo("on", true, HintScope::First);
    assert_eq!(w.text_segments.len(), w.items.len(), "与 items 一一对应");
    assert_eq!(w.items[0], "拼过", "前提：首选是精确命中：{:?}", w.items);
    assert!(w.text_segments[0].is_empty(), "精确命中不分段");
    let at = w
        .items
        .iter()
        .position(|t| t == "苹果")
        .expect("应有「苹果」");
    let segs = &w.text_segments[at];
    assert_eq!(joined(segs), "苹(ping)果");
    assert_eq!(
        segs.iter()
            .map(|s| (s.text.as_str(), s.role.as_deref()))
            .collect::<Vec<_>>(),
        vec![("苹", None), ("(ping)", Some("pinyin_hint")), ("果", None)]
    );
    let later = w
        .items
        .iter()
        .position(|t| t == "平过")
        .expect("应有「平过」");
    assert!(w.text_segments[later].is_empty(), "first：之后的不标");
}

#[test]
fn candidates_hint_scope_all_segments_every_fuzzy() {
    let w = pinguo("all", true, HintScope::All);
    let at = w
        .items
        .iter()
        .position(|t| t == "平过")
        .expect("应有「平过」");
    assert_eq!(joined(&w.text_segments[at]), "平(ping)过");
}

#[test]
fn candidates_hint_off_no_segments() {
    let w = pinguo("off", false, HintScope::All);
    assert!(w.items.iter().any(|t| t == "苹果"), "{:?}", w.items);
    assert!(w.text_segments.iter().all(Vec::is_empty));
}
