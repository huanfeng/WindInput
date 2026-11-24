use super::*;

/// 按应用方案记忆表：旧 state.toml 缺字段 = 空表；写出再读回不丢；空表不写进文件。
#[test]
fn app_schemas_roundtrip_and_default_empty() {
    let old: RuntimeState = toml::from_str("last_chinese_mode = false\n").unwrap();
    assert!(old.app_schemas.is_empty());
    let empty = toml::to_string_pretty(&RuntimeState::default()).unwrap();
    assert!(!empty.contains("app_schemas"), "空表不应写进文件:\n{empty}");

    let mut rs = RuntimeState::default();
    rs.app_schemas.insert("weixin.exe".into(), "wubi86".into());
    rs.app_schemas
        .insert("com.tencent.xinwechat".into(), "english".into());
    let dir = std::env::temp_dir().join(format!("wind_state_app_schemas_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    rs.save(&dir).unwrap();
    let back = RuntimeState::load(&dir);
    assert_eq!(back.app_schemas, rs.app_schemas);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 旧 state.toml（无 last_* 三字段）反序列化应落到语义默认：中文/半角/中文标点。
#[test]
fn old_state_toml_defaults_to_chinese() {
    let rs: RuntimeState = toml::from_str("[toolbar_positions]\n").unwrap();
    assert!(rs.last_chinese_mode);
    assert!(!rs.last_full_width);
    assert!(rs.last_chinese_punct);
}

/// Default 与 serde 缺字段默认一致（load 失败回退 unwrap_or_default 的语义相同）。
#[test]
fn default_matches_serde_defaults() {
    let d = RuntimeState::default();
    assert!(d.last_chinese_mode);
    assert!(!d.last_full_width);
    assert!(d.last_chinese_punct);
}

/// 语言栏图标三项 roundtrip，且**未设置时不得出现在文件里**。
///
/// 后半条是要害：`None` 的语义是「用代码默认」，一旦被序列化成某个具体值写进
/// state.toml，这台机器就此被钉死在写入当天的默认上——之后改代码默认对它无效，
/// 表现为「新机器和老机器不一样」。toml 也确实不能表达 None，漏掉
/// skip_serializing_if 会直接让整个 save 失败（连工具栏位置一起丢）。
#[test]
fn langbar_icon_prefs_roundtrip_and_omit_when_unset() {
    let empty = toml::to_string_pretty(&RuntimeState::default()).unwrap();
    assert!(
        !empty.contains("langbar_icon"),
        "未设置的语言栏图标偏好被写进了文件:\n{empty}"
    );

    let rs = RuntimeState {
        langbar_icon_size_marks: Some(true),
        ..Default::default()
    };
    let s = toml::to_string_pretty(&rs).unwrap();
    let back: RuntimeState = toml::from_str(&s).unwrap();
    assert_eq!(back.langbar_icon_size_marks, Some(true));
}

/// 已移到 `[ui.langbar]` 的那几项**不能**再被 state.toml 读回。
///
/// 老机器的 state.toml 里还留着 `langbar_icon_shape` 等键。serde 默认忽略未知字段，
/// 所以它们只是被静默丢弃——这正是想要的（配置段才是真相源）。本条钉住这个行为：
/// 若哪天有人"顺手"把字段加回来，两个真相源就复活了，而症状（重启后形状变回旧值）
/// 要等到下次重启才看得见。
#[test]
fn migrated_keys_are_ignored_not_resurrected() {
    let legacy = r#"
last_chinese_mode = true
langbar_icon_shape = "outer_ring"
langbar_icon_colored = false
langbar_icon_size_marks = true
"#;
    let back: RuntimeState = toml::from_str(legacy).expect("旧文件必须仍能解析");
    // 未知键被忽略，且不影响其余字段的读回。
    assert_eq!(back.langbar_icon_size_marks, Some(true));
    assert!(back.last_chinese_mode);
    // 再写出去时不该把它们带回来。
    let out = toml::to_string_pretty(&back).unwrap();
    assert!(
        !out.contains("langbar_icon_shape") && !out.contains("langbar_icon_colored"),
        "已迁移的键又被写回 state.toml:\n{out}"
    );
}

/// 老的 `toolbar_positions`（**左上角**坐标）绝不能被当成新的右下角锚点读回。
///
/// 这是本次语义反转唯一的危险面：两个字段的类型完全相同
/// （`HashMap<String,(i32,i32)>`），把老键改名读进来编译得过、测试也不会红，
/// 但每一台老机器的工具栏都会在升级后跳到「右下角落在原左上角处」——横条约 132px
/// 的偏移，且用户无从得知为什么。故**必须靠改字段名让老数据失配**，
/// 落回默认位置（右下角）比错位可解释得多。
///
/// 同理老 key 不带 `@scalePct` 后缀，即便有人把字段名改回去也匹配不上——两道
/// 保险各自独立。若哪天有人为"平滑升级"补一段迁移，这条会红，那正是要挡的改动。
#[test]
fn legacy_toolbar_positions_must_not_be_read_as_anchors() {
    let legacy = r#"
last_chinese_mode = true

[toolbar_positions]
"1920,1040" = [1776, 998]
"#;
    let back: RuntimeState = toml::from_str(legacy).expect("旧文件必须仍能解析");
    assert!(
        back.toolbar_anchors.is_empty(),
        "老的左上角坐标被当成右下角锚点读回了: {:?}",
        back.toolbar_anchors
    );
    assert!(back.last_chinese_mode, "同文件其余字段仍须正常读回");
    // 再写出去时不得把老键带回来（否则两个真相源并存）。
    let out = toml::to_string_pretty(&back).unwrap();
    assert!(
        !out.contains("toolbar_positions"),
        "已改名的键又被写回 state.toml:\n{out}"
    );
}

/// 删掉字段后，**老 state.toml 里残留的那张表不得让整份文件读不出来**。
///
/// `candidate_pin_positions` 是随 `pin_candidate_position` 开关一起加的存储位，那个
/// 开关早已删除（见 `docs/redesign/config-schema.md`：「长期无消费点；该能力由
/// `ui.candidate.position_mode = "fixed"` 一套实现」），存储位被漏下，全仓零读写点。
///
/// ★★ 本条钉的不是"删对了"，是**删除之所以安全的那个前提**：`RuntimeState` 没有
/// `deny_unknown_fields`，未知键被静默忽略。哪天有人给它加上，老用户 state.toml 里
/// 的这张残表会让**整份文件解析失败** ⇒ 工具栏/软键盘位置、上次输入态、面板页码
/// 全部一次性丢光，而症状只是"设置怎么都不记了"，没人会联想到是加了个 serde 属性。
#[test]
fn removed_candidate_pin_positions_does_not_break_old_files() {
    let legacy = r#"
last_chinese_mode = false
last_softkeyboard_page = "symbols"

[toolbar_anchors]
"1920,1040@100" = [1908, 1028]

[candidate_pin_positions.notepad]
"1920,1040@100" = [800, 600]
"#;
    let back: RuntimeState = toml::from_str(legacy).expect("含已删字段的旧文件必须仍能解析");
    assert!(!back.last_chinese_mode, "残表不得影响同文件其余字段");
    assert_eq!(back.last_softkeyboard_page, "symbols");
    assert_eq!(
        back.toolbar_anchors.get("1920,1040@100").copied(),
        Some((1908, 1028)),
        "位置记忆必须照常读回"
    );
    // 写回时不得把残表带出来——否则删了字段，文件里那张表却永远留着。
    let out = toml::to_string_pretty(&back).unwrap();
    assert!(
        !out.contains("candidate_pin_positions"),
        "已删除的键又被写回 state.toml:\n{out}"
    );
}

/// 两族锚点 roundtrip，且 key 的 `@scalePct` 后缀能原样存活。
///
/// 后半条不是多余的：key 里带 `@` 与逗号，TOML 表名必须被正确引号包裹，
/// 写出来再读回去必须还是同一个 key——否则重启后一律失配，位置记忆整体失效，
/// 而症状（"还是记不住"）与压根没实现完全一样。
#[test]
fn anchors_roundtrip_with_scaled_monitor_key() {
    let key = "2560,1392@150".to_string();
    let mut rs = RuntimeState::default();
    rs.toolbar_anchors.insert(key.clone(), (2548, 1380));
    rs.softkeyboard_anchors.insert(key.clone(), (1800, 1380));

    let s = toml::to_string_pretty(&rs).unwrap();
    let back: RuntimeState = toml::from_str(&s).unwrap();
    assert_eq!(back.toolbar_anchors.get(&key), Some(&(2548, 1380)));
    assert_eq!(back.softkeyboard_anchors.get(&key), Some(&(1800, 1380)));
}

/// 钉住偏好 roundtrip；未钉住时不写这个键（老文件、新文件都不多一行噪声）。
#[test]
fn softkeyboard_pinned_roundtrip() {
    let rs = RuntimeState {
        softkeyboard_pinned: true,
        ..Default::default()
    };
    let s = toml::to_string_pretty(&rs).unwrap();
    assert!(
        toml::from_str::<RuntimeState>(&s)
            .unwrap()
            .softkeyboard_pinned
    );
    let plain = toml::to_string_pretty(&RuntimeState::default()).unwrap();
    assert!(!plain.contains("softkeyboard_pinned"), "{plain}");
}

/// 三字段 roundtrip。
#[test]
fn last_state_roundtrip() {
    let rs = RuntimeState {
        last_chinese_mode: false,
        last_full_width: true,
        last_chinese_punct: false,
        ..Default::default()
    };
    let s = toml::to_string_pretty(&rs).unwrap();
    let back: RuntimeState = toml::from_str(&s).unwrap();
    assert!(!back.last_chinese_mode);
    assert!(back.last_full_width);
    assert!(!back.last_chinese_punct);
}

/// 前置（设计 §2.1）：state.toml 从本机根挪进 state/，老位置的文件首次加载时搬过来。
#[test]
fn legacy_state_toml_is_moved_into_state_dir_on_load() {
    let root = std::env::temp_dir().join(format!("wind_state_move-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("state.toml"), "last_chinese_mode = false\n").unwrap();
    let state_dir = root.join("state");
    let st = RuntimeState::load(&state_dir);
    assert!(!st.last_chinese_mode, "应读到老位置的内容");
    assert!(state_dir.join("state.toml").is_file(), "应已搬进 state/");
    assert!(!root.join("state.toml").exists(), "老位置应已清空");
    // 再读一次走新位置
    assert!(!RuntimeState::load(&state_dir).last_chinese_mode);
    let _ = std::fs::remove_dir_all(&root);
}
