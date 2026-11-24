use super::*;
use crate::compat_overlay::parse_raw;
use serde_json::json;

const SYS: &str = "[[apps]]\nprocess = \"ahk.exe\"\nauto_pair = true\n\n[[apps]]\nprocess = \"ahk.exe\"\ntitle = \"Editor*\"\ninitial_mode = \"english\"\n";

fn layers(usr: &str) -> Layers {
    Layers {
        system: parse_raw(SYS).unwrap(),
        user: parse_raw(usr).unwrap(),
        warnings: Vec::new(),
    }
}

#[test]
fn process_apis_address_only_the_pure_process_rule() {
    let mut l =
        layers("[[apps]]\nprocess = \"AHK.exe\"\ntitle = \"editor*\"\ncaret_use_top = true\n");
    let views = l.view(Section::Apps);
    assert_eq!(
        views
            .iter()
            .map(|v| (v.process.as_str(), v.class.as_str(), v.title.as_str()))
            .collect::<Vec<_>>(),
        vec![("ahk.exe", "", ""), ("ahk.exe", "", "Editor*")],
        "窗口规则如实列出，条件沿用系统层写法"
    );
    assert_eq!(views[1].state, RuleState::Modified);
    let v = l.view_of(Section::Apps, "ahk.exe").unwrap();
    assert_eq!(v.state, RuleState::System, "纯进程那条没被用户动过");
    assert_eq!(v.effective["auto_pair"], json!(true));

    l.upsert(
        Section::Apps,
        "ahk.exe",
        &json!({"auto_pair": false}).as_object().unwrap().clone(),
    )
    .unwrap();
    assert_eq!(
        l.user.apps.len(),
        2,
        "新建了纯进程差异行: {:?}",
        l.user.apps
    );
    assert_eq!(
        l.user.apps[0]["caret_use_top"],
        json!(true),
        "窗口规则那行原样"
    );
    assert!(l.reset(Section::Apps, "ahk.exe"));
    assert_eq!(l.user.apps.len(), 1, "只删纯进程那条");
    assert_eq!(l.user.apps[0]["title"], json!("editor*"));
    assert!(!l.reset(Section::Apps, "ahk.exe"));
}

/// 只写窗口条件的规则（身份里 process = "*"）列成 `*` + 条件；按进程名（不带条件）的写入
/// 不会落到它头上。
#[test]
fn window_only_rules_are_listed_with_conditions_and_process_writes_never_touch_them() {
    let mut l = layers(
        "[[apps]]\nclass = \"Chrome_WidgetWin_*\"\ncomposition_placeholder = \"zwsp\"\n\n[[apps]]\nprocess = \"ahk.exe\"\nclass = \"X\"\ndisabled = true\n",
    );
    let listed: Vec<(String, String, String)> = l
        .view(Section::Apps)
        .into_iter()
        .map(|v| (v.process, v.class, v.title))
        .collect();
    let row = |p: &str, c: &str, t: &str| (p.to_string(), c.to_string(), t.to_string());
    assert_eq!(
        listed,
        vec![
            row("ahk.exe", "", ""),
            row("ahk.exe", "", "Editor*"),
            row("*", "Chrome_WidgetWin_*", ""),
            row("ahk.exe", "X", ""),
        ]
    );
    assert!(l.view_of(Section::Apps, "*").is_none());
    let before = l.user.apps.clone();

    l.set_disabled(Section::Apps, "ahk.exe", true).unwrap();
    l.set_disabled(Section::Apps, "ahk.exe", false).unwrap();
    assert!(!l.reset(Section::Apps, "*"), "没有纯 * 规则可还原");
    assert!(
        l.set_disabled(Section::Apps, "*", true).is_err(),
        "纯 * 规则不存在"
    );
    assert_eq!(l.user.apps, before, "按进程名的写入不碰窗口规则");
    l.upsert(
        Section::Apps,
        "ahk.exe",
        &json!({"caret_use_top": true}).as_object().unwrap().clone(),
    )
    .unwrap();
    assert_eq!(
        &l.user.apps[..2],
        &before[..],
        "窗口规则原样，纯进程差异另起一行"
    );
    assert!(l.user.apps[2].get("class").is_none());
    assert!(l.reset(Section::Apps, "ahk.exe"));
    assert_eq!(l.user.apps, before);
}

/// replace 导入会清掉用户层的窗口规则：预览必须把它列成 remove（列表里虽不显示）。
#[test]
fn replace_import_previews_removal_of_window_rules() {
    let l = layers("[[apps]]\nprocess = \"ahk.exe\"\ntitle = \"T\"\nauto_pair = true\n");
    let p = l.import_preview("", ImportMode::Replace).unwrap();
    assert_eq!(
        p.items
            .iter()
            .map(|i| (i.process.as_str(), i.action))
            .collect::<Vec<_>>(),
        vec![("ahk.exe", "remove")]
    );
}

#[test]
fn patching_class_or_title_is_refused_like_process() {
    let mut l = layers("");
    let err = l
        .upsert(
            Section::Apps,
            "ahk.exe",
            &json!({"class": "X"}).as_object().unwrap().clone(),
        )
        .unwrap_err();
    assert!(err.contains("不能通过补丁修改"), "{err}");
    assert!(l.reset_field(Section::Apps, "ahk.exe", "title").is_err());
}

/// 导入带窗口条件的规则：保留条件、按身份叠到同一条上，不能被压扁成整个进程的规则。
#[test]
fn import_keeps_window_conditions_in_the_identity() {
    let mut l = layers("");
    let p = l
        .import_apply(
            "[[apps]]\nprocess = \"AHK.EXE\"\ntitle = \" EDITOR* \"\ncaret_use_top = true\n\n[[apps]]\nclass = \"Chrome_WidgetWin_*\"\ncomposition_placeholder = \"zwsp\"\n\n[[apps]]\nprocess = \"x.exe\"\nclass = 1\nauto_pair = true\n",
            ImportMode::Merge,
        )
        .unwrap();
    assert_eq!(p.rejected.len(), 1, "{:?}", p.rejected);
    assert_eq!(l.user.apps.len(), 2, "{:?}", l.user.apps);
    let ahk = &l.user.apps[0];
    assert_eq!(ahk["title"], json!("Editor*"), "沿用系统层同身份那条的写法");
    assert_eq!(ahk["caret_use_top"], json!(true));
    assert!(ahk.get("initial_mode").is_none(), "只记差异");
    let chrome = &l.user.apps[1];
    assert!(chrome.get("process").is_none(), "没写 process 的保持不写");
    assert_eq!(chrome["class"], json!("Chrome_WidgetWin_*"));
    assert_eq!(
        p.items
            .iter()
            .map(|i| (i.process.as_str(), i.action))
            .collect::<Vec<_>>(),
        vec![("AHK.EXE", "override"), ("*", "add")]
    );
    assert!(
        l.view_of(Section::Apps, "ahk.exe").unwrap().user.is_none(),
        "纯进程那条没被碰"
    );
}

#[test]
fn side_sections_list_class_as_an_unknown_key() {
    let l = Layers {
        system: Raw::default(),
        user: parse_raw("[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"A\"]\nclass = \"X\"\n").unwrap(),
        warnings: Vec::new(),
    };
    let v = l
        .view_of(Section::InitialModeScope, "explorer.exe")
        .unwrap();
    assert_eq!(v.unknown_keys, vec!["class".to_string()]);
}
