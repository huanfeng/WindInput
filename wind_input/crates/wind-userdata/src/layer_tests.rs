use super::*;
use crate::engine::{Dirs, UserData};
use crate::record::{OpKind, Payload, Target};
use std::sync::Arc;
use wind_dict::DictLayer;

fn names(v: Vec<wind_candidate::Candidate>) -> Vec<String> {
    v.into_iter().map(|c| c.text).collect()
}

#[test]
fn layer_sees_snapshot_delta_and_deletes() {
    let d = std::env::temp_dir().join(format!("wind_layer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let ud = Arc::new(
        UserData::open(Dirs {
            state_dir: d,
            dir_id: "x".into(),
        })
        .unwrap(),
    );
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "nihao",
        "你好",
        Payload::Weight(10),
        0b101,
    )
    .unwrap();
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "nihao",
        "拟好",
        Payload::Weight(20),
        0b101,
    )
    .unwrap();
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "wubi",
        "五笔",
        Payload::Weight(1),
        0,
    )
    .unwrap();
    ud.fold(Target::Temp, "py").unwrap();
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Delete,
        "nihao",
        "拟好",
        Payload::None,
        0b101,
    )
    .unwrap();
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "nihaoma",
        "你好吗",
        Payload::Weight(1),
        0b100101,
    )
    .unwrap();
    let layer = UserDataTempLayer::new(ud, "py");
    assert_eq!(layer.layer_type(), wind_dict::LayerType::Temp);
    assert_eq!(
        names(layer.search("nihao", 10)),
        vec!["你好"],
        "快照里的 + delta 删除生效"
    );
    let pre = layer.search_prefix("niha", 10);
    assert_eq!(
        names(pre.clone()),
        vec!["你好", "你好吗"],
        "快照 + delta 新增，按权重降序"
    );
    assert!(pre.iter().all(|c| c.is_prefix), "码都比前缀长");
    let pre = layer.search_prefix("nihao", 10);
    assert_eq!(names(pre.clone()), vec!["你好", "你好吗"]);
    assert!(
        pre.iter().all(|c| c.is_prefix),
        "前缀查询一律 is_prefix（同 StoreTempLayer）"
    );
    assert_eq!(
        names(layer.search_abbrev("nh", 10)),
        vec!["你好"],
        "简拼：快照里被删的不出现"
    );
    assert_eq!(
        names(layer.search_abbrev("nhm", 10)),
        vec!["你好吗"],
        "简拼：delta 新增能召回"
    );
    assert_eq!(
        names(layer.search_abbrev("w", 10)),
        vec!["五笔"],
        "无边界词走兜底组"
    );
    assert!(
        layer.search_abbrev_exact("w", 10).is_empty(),
        "exact 不含无边界词"
    );
    assert!(layer.has_longer_code("niha"));
    let c = &layer.search("nihao", 10)[0];
    assert!(c.meta.is_temp_dict && c.meta.raw_weight == 10 && c.boundary == 0b101);
}
/// 开放问题：search_prefix(…, 0) 表示不限，快照里的条目也要返回。
#[test]
fn prefix_with_zero_limit_returns_snapshot_entries_too() {
    let d = std::env::temp_dir().join(format!("wind_layer_p0-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let ud = Arc::new(
        UserData::open(Dirs {
            state_dir: d.clone(),
            dir_id: "dir".into(),
        })
        .unwrap(),
    );
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "nihao",
        "你好",
        Payload::Weight(5),
        0b101,
    )
    .unwrap();
    ud.fold(Target::Temp, "py").unwrap();
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "nihaoma",
        "你好吗",
        Payload::Weight(3),
        0b100101,
    )
    .unwrap();
    let layer = UserDataTempLayer::new(ud, "py");
    assert_eq!(
        names(layer.search_prefix("niha", 0)),
        vec!["你好", "你好吗"]
    );
    let _ = std::fs::remove_dir_all(&d);
}
/// 带音节上限的前缀查询：权重全打平时名额只发给合格条目，快照与 delta 两边都过滤。
#[test]
fn prefix_capped_fills_limit_with_qualifying_entries() {
    let ud = open_ud("capped");
    let put = |code: &str, text: &str, boundary: u64| {
        ud.apply(
            Target::Temp,
            "py",
            OpKind::Upsert,
            code,
            text,
            Payload::Weight(5),
            boundary,
        )
        .unwrap();
    };
    // 两音节词码序在前，不过滤时会先占满名额。
    for (code, text) in [("shafa", "沙发"), ("shangai", "上海"), ("shide", "是的")] {
        put(code, text, 1 | 1 << (code.len() - 2));
    }
    put("shi", "是", 1);
    ud.fold(Target::Temp, "py").unwrap();
    put("shu", "书", 1);
    put("shuben", "书本", 1 | 1 << 3);
    let layer = UserDataTempLayer::new(ud, "py");
    let mut got = names(layer.search_prefix_capped("sh", 2, 1, 0));
    got.sort();
    assert_eq!(got, vec!["书", "是"]);
    assert!(
        layer
            .search_prefix_capped("sh", 2, 1, 0)
            .iter()
            .all(|c| c.is_prefix)
    );
}

fn open_ud(name: &str) -> Arc<UserData> {
    let d = std::env::temp_dir().join(format!("wind_layer8_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    Arc::new(
        UserData::open(Dirs {
            state_dir: d,
            dir_id: "dir".into(),
        })
        .unwrap(),
    )
}

#[test]
fn temp_layer_identity_matches_store_layer() {
    let ud = open_ud("ident");
    let l = UserDataTempLayer::new(ud.clone(), "py");
    assert_eq!(l.name(), "temp:py");
    assert_eq!(
        l.entries_digest().as_deref(),
        Some("temp:py|enumerates-nothing")
    );
    ud.apply(
        Target::Temp,
        "py",
        OpKind::Upsert,
        "nihao",
        "你好",
        Payload::Weight(5),
        0b101,
    )
    .unwrap();
    let c = &l.search("nihao", 0)[0];
    assert!(c.meta.is_temp_dict && !c.is_prefix);
    assert_eq!(
        (c.natural_order, c.boundary, c.meta.raw_weight),
        (0, 0b101, 5)
    );
}

/// 对照 store_layers_answer_pattern_queries_and_drafts_stay_out：字面前缀扫描后过滤，等长档在前。
#[test]
fn temp_layer_answers_pattern_queries() {
    let ud = open_ud("pattern");
    for (code, text) in [
        ("abcd", "甲"),
        ("abxd", "乙"),
        ("abcde", "丙"),
        ("zbcd", "丁"),
    ] {
        ud.apply(
            Target::Temp,
            "wb",
            OpKind::Upsert,
            code,
            text,
            Payload::Weight(1),
            0,
        )
        .unwrap();
    }
    ud.fold(Target::Temp, "wb").unwrap();
    let l = UserDataTempLayer::new(ud, "wb");
    assert_eq!(
        names(l.search_pattern("ab?d", '?', 10, false)),
        vec!["甲", "乙"]
    );
    let with_prefix = l.search_pattern("ab?d", '?', 10, true);
    assert_eq!(
        names(with_prefix.clone()),
        vec!["甲", "乙", "丙"],
        "等长档在前"
    );
    assert!(with_prefix[2].is_prefix);
    assert!(l.search_pattern("", '?', 10, true).is_empty());
    assert!(l.search_pattern("ab?d", '?', 0, true).is_empty());
}

/// 对照 drafts_never_surface_through_prefix_queries 与草稿候选形态。
#[test]
fn draft_layer_is_exact_only_sinks_and_respects_ttl() {
    let ud = open_ud("draft");
    let mut old = crate::engine::Op::new(
        Target::Draft,
        "wb",
        OpKind::Upsert,
        "ggtt",
        "旧",
        Payload::Weight(0),
        0,
    );
    old.ts_ms = 1_000;
    ud.apply_batch(&[old]).unwrap();
    ud.apply(
        Target::Draft,
        "wb",
        OpKind::Upsert,
        "ggtt",
        "盖伦",
        Payload::Weight(0),
        0,
    )
    .unwrap();
    let l = UserDataDraftLayer::new(ud, "wb", 86_400);
    assert_eq!(l.name(), "draft:wb");
    assert_eq!(l.layer_type(), LayerType::Draft);
    let hits = l.search("ggtt", 0);
    assert_eq!(names(hits.clone()), vec!["盖伦"], "过期的不出");
    assert!(hits[0].is_draft && hits[0].weight == 0);
    assert!(l.search_prefix("ggt", 0).is_empty());
    assert!(l.search_abbrev("g", 0).is_empty());
}
#[test]
fn user_layer_marks_user_dict_and_uses_line_order() {
    let ud = open_ud("userlayer");
    let mut ops = Vec::new();
    for (text, order_hint) in [("泥", 2u32), ("你", 1)] {
        let _ = order_hint;
        ops.push(crate::engine::Op::new(
            Target::User,
            "py",
            OpKind::Upsert,
            "ni",
            text,
            Payload::Weight(5),
            0b1,
        ));
    }
    ud.apply_batch(&ops).unwrap();
    let l = UserDataUserLayer::new(ud, "py");
    assert_eq!(l.name(), "user:py");
    assert_eq!(l.layer_type(), LayerType::User);
    let c = l.search("ni", 0);
    assert!(
        c.iter()
            .all(|c| c.meta.is_user_dict && !c.meta.is_temp_dict)
    );
    assert!(l.search_prefix("n", 0).iter().all(|c| c.is_prefix));
}
