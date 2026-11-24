use super::*;
use wind_config::Config;

#[test]
fn request_carries_sample_with_labels_and_per_page() {
    let mut cfg = Config::default();
    cfg.ui.candidate.per_page = 3;
    let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
    while rx.try_recv().is_ok() {}
    let mut theme = wind_theme::Resolved::default();
    theme.views.index_labels = vec!["a".into(), String::new()];
    let reply = c.request_candidate_preview(
        theme,
        true,
        1.5,
        PreviewKind::Candidate,
        None,
        None,
        None,
        Default::default(),
    );
    let req = loop {
        match rx.recv().unwrap() {
            UiCommand::RenderCandidatePreview(r) => break r,
            _ => continue,
        }
    };
    assert_eq!(req.preedit, PREVIEW_PREEDIT, "出厂分隔符就是 '");
    assert!(req.vertical);
    assert_eq!(req.scale, 1.5);
    assert_eq!(req.fit_w, None, "缺省不约束");
    let labels: Vec<_> = req.candidates.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["a", "2", "3"], "主题槽位优先、空槽回落数字");
    assert!(req.candidates.iter().any(|c| !c.comment.is_empty()));
    req.reply
        .try_send(Ok(CandidatePreviewImage {
            width: 1,
            height: 2,
            png: vec![],
        }))
        .unwrap();
    assert_eq!(reply.recv().unwrap().unwrap().height, 2);
}

#[test]
fn toolbar_request_fills_items_from_effective_config() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    while rx.try_recv().is_ok() {}
    let reply = c.request_candidate_preview(
        wind_theme::Resolved::default(),
        false,
        1.0,
        PreviewKind::Toolbar,
        None,
        None,
        None,
        Default::default(),
    );
    let req = loop {
        match rx.recv().unwrap() {
            UiCommand::RenderCandidatePreview(r) => break r,
            _ => continue,
        }
    };
    assert!(
        !req.toolbar_items.is_empty(),
        "不带草稿时格子取当前生效 ui.toolbar"
    );
    assert!(req.toolbar_state.is_none(), "状态走渲染端的固定样本");
    assert!(
        !req.candidates.is_empty(),
        "候选样本照填（toolbar 分支不消费）"
    );
    let _ = reply;
}

#[test]
fn draft_carries_toolbar_segment() {
    let c = Coordinator::new_headless(Config::default(), None);
    let ui: toml::Value =
        toml::from_str("[toolbar]\nvertical = true\nitems = [\"mode\", \"settings\"]").unwrap();
    let d = c.candidate_preview_draft(&ui).unwrap();
    assert!(d.toolbar_vertical, "草稿的 ui.toolbar.vertical");
    assert_eq!(
        d.toolbar_items,
        vec![ToolbarItem::Mode, ToolbarItem::Settings],
        "草稿的 ui.toolbar.items"
    );
}

#[test]
fn headless_without_ui_disconnects_immediately() {
    let c = Coordinator::new_headless(Config::default(), None);
    let reply = c.request_candidate_preview(
        wind_theme::Resolved::default(),
        false,
        1.0,
        PreviewKind::Candidate,
        None,
        None,
        None,
        Default::default(),
    );
    assert!(matches!(
        reply.recv_timeout(std::time::Duration::from_secs(5)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn fingerprint_follows_candidate_and_font_config() {
    let base = Coordinator::new_headless(Config::default(), None).candidate_preview_fingerprint();
    let mut cfg = Config::default();
    cfg.ui.candidate.font_size = 23.0;
    let sized = Coordinator::new_headless(cfg, None).candidate_preview_fingerprint();
    let mut cfg = Config::default();
    cfg.ui.font.family = "Noto Sans".into();
    let font = Coordinator::new_headless(cfg, None).candidate_preview_fingerprint();
    assert_ne!(base, sized);
    assert_ne!(base, font);
    assert_eq!(
        base,
        Coordinator::new_headless(Config::default(), None).candidate_preview_fingerprint()
    );
}

/// 草稿叠到当前配置：没给的键保留当前值，Map 整值收、未登记键丢、越界夹回。
#[test]
fn draft_overlays_current_config_and_sanitizes() {
    let mut cfg = Config::default();
    cfg.ui.font.family = "Live".into();
    cfg.ui.candidate.per_page = 7;
    let c = Coordinator::new_headless(cfg, None);
    let ui: toml::Value = toml::from_str(
        r#"
            [candidate]
            font_size = -3.0
            min_rows = 99
            per_page = 3
            shadow = "off"
            syllable_separator = " "
            bogus = 1
            [font]
            weight = 5000
            scripts = { latin = ["A", "B"] }
            [comment]
            enabled = false
            "#,
    )
    .unwrap();
    let d = c.candidate_preview_draft(&ui).unwrap();
    assert_eq!(d.appearance.font_family, "Live", "没给的键保留当前值");
    assert_eq!(d.candidate.per_page, 3);
    assert_eq!(d.appearance.font_size, 0.0);
    assert_eq!(d.appearance.font_weight, 950);
    assert_eq!(d.appearance.min_size[4], 3, "最小行数按每页数封顶");
    assert!(!d.appearance.shadow && !d.comments);
    assert_eq!(d.candidate.syllable_separator, " ");
    assert_eq!(
        d.appearance.font_scripts,
        vec![("latin".to_string(), vec!["A".to_string(), "B".to_string()])]
    );
    assert!(c.candidate_preview_draft(&toml::Value::Integer(1)).is_err());
}

/// 草稿里删掉一个子键（Map 整值），预览就不再用它：整值替换，不与当前配置深合并。
#[test]
fn draft_map_replaces_current_value_whole() {
    let mut cfg = Config::default();
    cfg.ui.font.scripts.insert("latin".into(), vec!["A".into()]);
    cfg.ui.font.scripts.insert("cjk".into(), vec!["B".into()]);
    let c = Coordinator::new_headless(cfg, None);
    let ui: toml::Value = toml::from_str("[font]\nscripts = { cjk = [\"B\"] }\n").unwrap();
    let d = c.candidate_preview_draft(&ui).unwrap();
    assert_eq!(
        d.appearance.font_scripts,
        vec![("cjk".to_string(), vec!["B".to_string()])]
    );
    // 动态注释寻址键不收（Config 里没有同名字段），也不让整份草稿失败。
    let ui: toml::Value = toml::from_str("[comment.template]\nmine = \"${code}\"\n").unwrap();
    assert!(c.candidate_preview_draft(&ui).is_ok());
}
