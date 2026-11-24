//! 协调器按作用范围解析模板：出厂 `system.comments.toml` + 用户 `[ui.comment]`。
//! 要真实方案文件判方案类型，用 `build_dev/data`。端到端（到 UI）另见
//! `coordinator::mode_comment_e2e_tests`。
use super::*;
use crate::coordinator::Coordinator;
use std::path::PathBuf;
use wind_config::comment_lib::{CommentCells, CommentState, SystemComments};

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() && d.join("system.comments.toml").exists()
}

/// 出厂模板串，按 id 取（断言不抄模板原文）。
fn shipped(id: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/system.comments.toml");
    let sys = SystemComments::parse(&std::fs::read_to_string(p).unwrap()).unwrap();
    sys.templates
        .into_iter()
        .find(|t| t.id == id)
        .unwrap_or_else(|| panic!("出厂无模板 {id}"))
        .text
}

fn coord(active: &str, edit: impl FnOnce(&mut Config)) -> std::sync::Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.active = active.into();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into(), "shuangpin".into()];
    edit(&mut cfg);
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

/// 在 `active` 模式、`st` 态下的竖排模板。
fn tpl(c: &Coordinator, active: Option<ModeKind>, st: CommentState) -> String {
    c.refresh_config_in_memory(|cfg| cfg.ui.comment.state = st);
    let mut state = c.state.lock().unwrap();
    state.active = active;
    c.comment_template_for(&c.rt().config, &state, true)
}

#[test]
fn temp_english_uses_mode_cell() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 数据");
        return;
    }
    let c = coord("wubi86", |_| {});
    let mode = Some(ModeKind::TempEnglish);
    assert_eq!(
        tpl(&c, mode, CommentState::Default),
        "",
        "出厂临英默认态 none"
    );
    assert_eq!(tpl(&c, mode, CommentState::Detail), shipped("en_zh"));
}

#[test]
fn wubi_detail_falls_to_codetable_type() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 数据");
        return;
    }
    let c = coord("wubi86", |_| {});
    assert_eq!(
        tpl(&c, None, CommentState::Detail),
        shipped("chaizi_pinyin")
    );
    assert_eq!(
        tpl(&c, None, CommentState::Default),
        shipped("code"),
        "码表类型默认态跟随全局"
    );
}

#[test]
fn shuangpin_is_own_type() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 数据");
        return;
    }
    let only_shuangpin = |cfg: &mut Config| {
        cfg.ui.comment.assign.by_type.insert(
            "shuangpin".into(),
            CommentCells {
                detail_vertical: Some("chaizi".into()),
                ..Default::default()
            },
        );
    };
    let sp = coord("shuangpin", |_| {});
    assert_eq!(tpl(&sp, None, CommentState::Detail), shipped("pinyin_en"));
    let sp = coord("shuangpin", only_shuangpin);
    assert_eq!(tpl(&sp, None, CommentState::Detail), shipped("chaizi"));
    let py = coord("pinyin", only_shuangpin);
    assert_eq!(
        tpl(&py, None, CommentState::Detail),
        shipped("pinyin_en"),
        "全拼不受 type.shuangpin 影响"
    );
}

#[test]
fn mix_mode_cell_is_read_by_instance_id() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 数据");
        return;
    }
    let c = coord("wubi86", |cfg| {
        cfg.ui.comment.assign.mode.insert(
            "mix:quick_mix".into(),
            CommentCells {
                default_vertical: Some("pinyin".into()),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        tpl(&c, Some(ModeKind::Mix(0)), CommentState::Default),
        shipped("pinyin")
    );
    assert_eq!(
        tpl(&c, None, CommentState::Default),
        shipped("code"),
        "模式格只作用于本模式"
    );
}

fn cells(v: &str) -> CommentCells {
    CommentCells {
        default_vertical: Some(v.into()),
        detail_vertical: Some(v.into()),
        ..Default::default()
    }
}

/// ★ 模式无意见时落到方案层，而不是跳过它；进模式后模式层压过方案层。
#[test]
fn schema_layer_wins_when_mode_absent() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 数据");
        return;
    }
    let c = coord("wubi86", |cfg| {
        cfg.ui
            .comment
            .assign
            .schema
            .insert("wubi86".into(), cells("pinyin"));
        cfg.ui
            .comment
            .assign
            .mode
            .insert("url".into(), cells("chaizi"));
    });
    for st in [CommentState::Default, CommentState::Detail] {
        assert_eq!(tpl(&c, None, st), shipped("pinyin"), "{st:?}");
        assert_eq!(
            tpl(&c, Some(ModeKind::Url), st),
            shipped("chaizi"),
            "模式层排在方案层之上：{st:?}"
        );
    }
    let other = coord("pinyin", |cfg| {
        cfg.ui
            .comment
            .assign
            .schema
            .insert("wubi86".into(), cells("pinyin"));
    });
    assert_eq!(
        tpl(&other, None, CommentState::Default),
        "",
        "方案格只作用于本方案（拼音方案出厂默认态不显示）"
    );
}

/// ★ `none` 在方案层同样是有意见的值：压过下层，渲染成空。
#[test]
fn schema_none_hides_comment() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 数据");
        return;
    }
    let c = coord("wubi86", |cfg| {
        cfg.ui
            .comment
            .assign
            .schema
            .insert("wubi86".into(), cells("none"));
    });
    assert_eq!(tpl(&c, None, CommentState::Default), "");
    assert_eq!(tpl(&c, None, CommentState::Detail), "");
}

#[test]
fn disabled_master_switch_blanks_everything() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 数据");
        return;
    }
    let c = coord("wubi86", |cfg| cfg.ui.comment.enabled = false);
    for active in [
        None,
        Some(ModeKind::TempEnglish),
        Some(ModeKind::TempPinyin),
    ] {
        for st in [CommentState::Default, CommentState::Detail] {
            assert_eq!(tpl(&c, active, st), "", "{active:?} {st:?}");
        }
    }
}
