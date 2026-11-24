use super::{is_feature_target, set_comment_dict, state_unchanged_text};
use wind_config::CommentDictSpec;

fn manual(id: &str, enabled: bool) -> CommentDictSpec {
    CommentDictSpec {
        id: id.into(),
        path: format!("comments/{id}.txt"),
        enabled,
        ..Default::default()
    }
}

#[test]
fn state_unchanged_text_uses_menu_wording() {
    assert_eq!(state_unchanged_text("cn-en", true), "已是中文模式");
    assert_eq!(state_unchanged_text("cn-en", false), "已是英文模式");
    assert_eq!(state_unchanged_text("fullshape", false), "已是半角");
    assert_eq!(state_unchanged_text("s2t", true), "简入繁出已是开启状态");
    assert_eq!(state_unchanged_text("toolbar", false), "工具栏已是隐藏状态");
}

#[test]
fn feature_targets_are_recognized() {
    for t in ["emoji", "emoji:frost_greek", "emoji:猫猫", "dict:en"] {
        assert!(is_feature_target(t), "{t}");
    }
    for t in ["s2t", "cn-en", "emojis", "dictx"] {
        assert!(!is_feature_target(t), "{t}");
    }
}

/// 已在列表里的条目就地翻转，顺序与其它条目不动。
#[test]
fn flips_existing_entry_in_place() {
    let cfg = [manual("en", true), manual("chaifen", false)];
    let (list, on) = set_comment_dict(&cfg, &[], &[], "chaifen", None)
        .unwrap()
        .unwrap();
    assert!(on);
    assert_eq!(list.len(), 2);
    assert!(list[0].enabled && list[1].enabled);
    let (list, on) = set_comment_dict(&list, &[], &[], "en", None)
        .unwrap()
        .unwrap();
    assert!(!on && !list[0].enabled);
}

/// 指定状态：与当前一致时不动作（None）；不一致才改。
#[test]
fn set_is_noop_when_already_in_state() {
    let cfg = [manual("en", true)];
    assert!(
        set_comment_dict(&cfg, &[], &[], "en", Some(true))
            .unwrap()
            .is_none()
    );
    let (list, on) = set_comment_dict(&cfg, &[], &[], "en", Some(false))
        .unwrap()
        .unwrap();
    assert!(!on && !list[0].enabled);
}

/// 自动来源（未登记时是开的）不在列表里：要开不动作；要关 / 取反就追加一条关着的引用空壳。
#[test]
fn auto_source_gets_a_disabled_shell() {
    let cfg = [manual("en", true)];
    let auto = vec!["auto:pinyin/cn_dicts/corrections.dict.yaml".to_string()];
    assert!(
        set_comment_dict(&cfg, &auto, &[], &auto[0], Some(true))
            .unwrap()
            .is_none()
    );
    for want in [None, Some(false)] {
        let (list, on) = set_comment_dict(&cfg, &auto, &[], &auto[0], want)
            .unwrap()
            .unwrap();
        assert!(!on);
        let shell = &list[1];
        assert!(shell.auto && !shell.enabled && shell.path.is_empty());
    }
}

/// 内置库（未登记时是开的）：要开不动作；要关 / 取反就追加一条关着的 builtin 空壳。
#[test]
fn builtin_dict_gets_a_disabled_shell() {
    let cfg = [manual("en", true)];
    let builtin = vec!["zh_en".to_string()];
    assert!(
        set_comment_dict(&cfg, &[], &builtin, "zh_en", Some(true))
            .unwrap()
            .is_none()
    );
    for want in [None, Some(false)] {
        let (list, on) = set_comment_dict(&cfg, &[], &builtin, "zh_en", want)
            .unwrap()
            .unwrap();
        assert!(!on);
        let shell = &list[1];
        assert!(shell.id == "zh_en" && shell.builtin && !shell.enabled && !shell.auto);
    }
}

#[test]
fn unknown_id_is_an_error() {
    assert!(set_comment_dict(&[manual("en", true)], &[], &[], "nope", Some(true)).is_err());
}
