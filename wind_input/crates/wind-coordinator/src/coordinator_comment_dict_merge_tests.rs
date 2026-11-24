//! 注释词库挂载表 × 自动派生项的合并（[`merge_comment_dicts`]）。

use super::*;
use wind_config::CommentDictSpec;

fn manual(id: &str, path: &str, enabled: bool) -> CommentDictSpec {
    CommentDictSpec {
        id: id.to_string(),
        path: path.to_string(),
        enabled,
        ..Default::default()
    }
}

fn shell(id: &str, enabled: bool) -> CommentDictSpec {
    CommentDictSpec {
        id: id.to_string(),
        auto: true,
        enabled,
        ..Default::default()
    }
}

fn auto(rel: &str, schemas: &[&str]) -> AutoCommentSource {
    AutoCommentSource {
        id: auto_comment_id(rel),
        rel: rel.to_string(),
        label: String::new(),
        path: std::path::PathBuf::from("/abs").join(rel),
        schemas: schemas.iter().map(|s| s.to_string()).collect(),
    }
}

fn ids(v: &[EffectiveCommentDict]) -> Vec<&str> {
    v.iter().map(|e| e.id.as_str()).collect()
}

fn builtin_dicts() -> Vec<wind_config::comment_lib::BuiltinCommentDict> {
    ["en_zh", "zh_en"]
        .iter()
        .map(|id| wind_config::comment_lib::BuiltinCommentDict {
            id: id.to_string(),
            label: id.to_string(),
            path: format!("comments/{id}.txt"),
        })
        .collect()
}

#[test]
fn builtin_dicts_enabled_by_default() {
    let out = merge_comment_dicts(&[], &builtin_dicts(), &[]);
    assert_eq!(ids(&out), ["en_zh", "zh_en"]);
    assert!(out.iter().all(|e| e.enabled));
}

#[test]
fn builtin_sits_between_manual_and_auto() {
    let cfg = [manual("mine", "comments/mine.txt", true)];
    let a = [auto("pinyin/frost.dict.yaml", &["pinyin"])];
    let out = merge_comment_dicts(&cfg, &builtin_dicts(), &a);
    assert_eq!(
        ids(&out),
        ["mine", "en_zh", "zh_en", "auto:pinyin/frost.dict.yaml"]
    );
}

#[test]
fn user_can_disable_builtin() {
    let cfg = [
        CommentDictSpec {
            id: "zh_en".into(),
            builtin: true,
            enabled: false,
            ..Default::default()
        },
        manual("mine", "comments/mine.txt", true),
    ];
    let out = merge_comment_dicts(&cfg, &builtin_dicts(), &[]);
    assert_eq!(ids(&out), ["zh_en", "mine", "en_zh"]);
    assert!(!out[0].enabled);
    assert!(out[2].enabled);
}

#[test]
fn manual_entry_with_builtin_id_wins() {
    let cfg = [manual("en_zh", "comments/mine.txt", true)];
    let out = merge_comment_dicts(&cfg, &builtin_dicts(), &[]);
    assert_eq!(ids(&out), ["en_zh", "zh_en"]);
    assert!(matches!(out[0].kind, EffectiveCommentKind::Manual { .. }));
}

#[test]
fn unknown_builtin_shell_is_dropped() {
    let cfg = [CommentDictSpec {
        id: "gone".into(),
        builtin: true,
        ..Default::default()
    }];
    assert!(merge_comment_dicts(&cfg, &[], &[]).is_empty());
}

#[test]
fn missing_builtin_dict_is_skipped() {
    //! 缺文件的内置库跳过，其余库照常挂载。
    let fixture =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/comment_data");
    let mut dicts = builtin_dicts();
    dicts.insert(
        0,
        wind_config::comment_lib::BuiltinCommentDict {
            id: "absent".into(),
            label: String::new(),
            path: "comments/absent.txt".into(),
        },
    );
    let specs = merge_comment_dicts(&[], &dicts, &[]);
    for _ in 0..2 {
        let (paths, ids) = resolve_comment_mounts(&specs, Some(&fixture));
        assert_eq!(paths.len(), 2);
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|(_, v)| v[0] != "absent"));
    }
}

#[test]
fn new_auto_sources_append_after_manual_entries() {
    //! 没被引用过的自动项排在**末尾** —— 装个新方案不该把用户排好的第一优先级挤下去。
    let cfg = [manual("en", "comments/en.dict.yaml", true)];
    let a = [auto("pinyin/frost.dict.yaml", &["pinyin"])];
    let out = merge_comment_dicts(&cfg, &[], &a);
    assert_eq!(ids(&out), ["en", "auto:pinyin/frost.dict.yaml"]);
    assert!(out[1].enabled, "新自动项默认启用");
    assert_eq!(out[1].schemas, ["pinyin"], "白名单取声明它的方案");
}

#[test]
fn shell_keeps_user_position_and_switch() {
    //! 引用空壳把自动项的**位置与开关**交回用户 —— 这正是它存在的理由：
    //! 顺序即优先级，而顺序只能存在这张表的数组位置里。
    let cfg = [
        shell("auto:pinyin/frost.dict.yaml", false),
        manual("en", "comments/en.dict.yaml", true),
    ];
    let a = [auto("pinyin/frost.dict.yaml", &["pinyin"])];
    let out = merge_comment_dicts(&cfg, &[], &a);
    assert_eq!(
        ids(&out),
        ["auto:pinyin/frost.dict.yaml", "en"],
        "空壳在前 ⇒ 自动项优先级高于手工项"
    );
    assert!(!out[0].enabled, "用户关掉的自动项不能被重新打开");
    assert_eq!(out.len(), 2, "已被空壳认领的自动项不得在末尾再出现一次");
}

#[test]
fn shell_path_and_schemas_come_from_detection_not_config() {
    //! 空壳里只有 id / enabled / 位置是用户的。词库换了层、方案改了声明，
    //! 用户不需要回来同步——所以这两项每次都从检测结果取。
    let cfg = [CommentDictSpec {
        id: auto_comment_id("pinyin/frost.dict.yaml"),
        auto: true,
        enabled: true,
        path: "早就过时的路径.dict.yaml".into(),
        schemas: vec!["wubi86".into()],
        ..Default::default()
    }];
    let a = [auto("pinyin/frost.dict.yaml", &["pinyin", "wubi_pinyin"])];
    let out = merge_comment_dicts(&cfg, &[], &a);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].kind,
        EffectiveCommentKind::Auto {
            path: std::path::PathBuf::from("/abs").join("pinyin/frost.dict.yaml")
        },
        "路径取检测结果，不取空壳里那个"
    );
    assert_eq!(
        out[0].schemas,
        ["pinyin", "wubi_pinyin"],
        "白名单同理，且是并集（同一词库被两个方案共用）"
    );
}

#[test]
fn unclaimed_shell_is_dropped_silently() {
    //! 认领不到的空壳（词库删了/停用了/总开关关了）**丢弃**而非退化成手工项 ——
    //! 它没有 path，留下来只会得到一条「注释词库不存在」的告警，而那不是错误。
    let cfg = [
        shell("auto:gone.dict.yaml", true),
        manual("en", "comments/en.dict.yaml", true),
    ];
    let out = merge_comment_dicts(&cfg, &[], &[]);
    assert_eq!(ids(&out), ["en"]);
}

#[test]
fn no_auto_sources_reproduces_plain_config_order() {
    //! 总开关关掉（`auto` 为空表）时，结果必须与本功能引入前逐字一致：
    //! 手工项按配置顺序、各自带自己的 path 与白名单。
    let cfg = [
        manual("a", "comments/a.dict.yaml", true),
        manual("b", "comments/b.dict.yaml", false),
    ];
    let out = merge_comment_dicts(&cfg, &[], &[]);
    assert_eq!(ids(&out), ["a", "b"]);
    assert!(out[0].enabled && !out[1].enabled);
    assert_eq!(
        out[1].kind,
        EffectiveCommentKind::Manual {
            path: "comments/b.dict.yaml".to_string()
        }
    );
}

#[test]
fn auto_id_normalizes_separator() {
    //! id 要跨平台稳定：方案文件里写 `\` 还是 `/`，认领结果必须一样，
    //! 否则同一个词库在两台机器上得到两个 id，用户排好的顺序换台机器就失效。
    assert_eq!(
        auto_comment_id("pinyin\\frost.dict.yaml"),
        auto_comment_id("pinyin/frost.dict.yaml")
    );
}
