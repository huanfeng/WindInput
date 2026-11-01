//! `$SS` / `$AA` 组的展开态规则 + 嵌入 `$CC` 命令成员。
//!
//! 规则：前缀匹配恒折叠成组名；全码匹配**无重码**才自动展开，**有重码**不展开、折成组名候选，
//! 用户选中后才展开成员（且只列该组）。
//!
//! 自带临时码表方案，不依赖 `build_dev/data`。

use std::path::PathBuf;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

const VK_BACK: u32 = 0x08;

fn make_data_dir(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_grp_rule_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("st")).unwrap();
    std::fs::write(
        schemas.join("st.schema.toml"),
        "[schema]\nid = \"st\"\nname = \"组规则测试\"\n\
         [engine]\ntype = \"codetable\"\n\
         [engine.codetable]\nmax_code_length = 4\n\
         [[dictionaries]]\nid = \"main\"\npath = \"st/st.dict.yaml\"\ndefault = true\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("st/st.dict.yaml"),
        format!("---\nname: st\nversion: \"1\"\n...\n{body}"),
    )
    .unwrap();
    dir
}

struct Fixture {
    coord: std::sync::Arc<Coordinator>,
    dir: PathBuf,
    ov: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::remove_dir_all(&self.ov);
    }
}

fn fixture(tag: &str, body: &str) -> Fixture {
    let dir = make_data_dir(tag, body);
    let mut c = Config::default();
    c.schema.available = vec!["st".into()];
    c.schema.active = "st".into();
    c.input.default.chinese_mode = true;
    let ov = std::env::temp_dir().join(format!("wind_grp_rule_ov_{tag}"));
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).unwrap();
    let coord = Coordinator::new_headless_with_override(c, Some(&dir), Some(ov.clone()));
    Fixture { coord, dir, ov }
}

fn press(coord: &Coordinator, vk: u32) {
    coord.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    });
}

fn type_keys(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        press(coord, ch.to_ascii_uppercase() as u32);
    }
}

fn texts(f: &Fixture) -> Vec<String> {
    f.coord.debug_all_candidate_texts()
}

const SS: &str = r#"$SS("括号", "【】", "（）", "〔〕")"#;

/// 全码匹配、同码没有别的候选 → 自动展开。
#[test]
fn full_code_without_duplicates_expands() {
    let f = fixture("nodup", &format!("阿\ta\n{SS}\tuu\n"));
    type_keys(&f.coord, "uu");
    let got = texts(&f);
    for want in ["【】", "（）", "〔〕"] {
        assert!(got.iter().any(|t| t == want), "缺 {want}: {got:?}");
    }
    assert!(!got.iter().any(|t| t == "括号"), "无重码不该折叠: {got:?}");
}

/// 全码匹配但同码还有码表词 → 不展开，折成组名；码表词照常在。
#[test]
fn full_code_with_duplicate_folds_to_group_name() {
    let f = fixture("dup", &format!("阿\ta\n乙\tuu\n{SS}\tuu\n"));
    type_keys(&f.coord, "uu");
    let got = texts(&f);
    assert!(got.iter().any(|t| t == "乙"), "同码词必须还在: {got:?}");
    assert!(got.iter().any(|t| t == "括号"), "应折成组名: {got:?}");
    for m in ["【】", "（）", "〔〕"] {
        assert!(!got.iter().any(|t| t == m), "重码时不得展开 {m}: {got:?}");
    }
    let shapes = f.coord.debug_candidate_shapes();
    let g = shapes.iter().find(|s| s.0 == "括号").unwrap();
    assert!(g.2, "折叠后的组名候选必须是 is_group");
}

/// 选中折叠的组 → 展开且只列该组成员；退格再键入又回到折叠态（展开不粘连）。
#[test]
fn selecting_folded_group_expands_only_that_group() {
    let f = fixture("pick", &format!("阿\ta\n乙\tuu\n{SS}\tuu\n"));
    type_keys(&f.coord, "uu");
    let all = texts(&f);
    let idx = all.iter().position(|t| t == "括号").unwrap();
    press(&f.coord, 0x31 + idx as u32); // 数字键选第 idx+1 项
    let got = texts(&f);
    assert_eq!(got, ["【】", "（）", "〔〕"], "只列该组成员: {got:?}");
    assert_eq!(f.coord.debug_input_buffer(), "uu", "二级选择不改输入码");

    // Backspace 先退一级：回到折叠视图，输入码不变。
    press(&f.coord, VK_BACK);
    assert_eq!(f.coord.debug_input_buffer(), "uu", "退一级不删码");
    let back = texts(&f);
    assert!(
        back.iter().any(|t| t == "括号") && back.iter().any(|t| t == "乙"),
        "{back:?}"
    );
    assert!(!back.iter().any(|t| t == "【】"), "{back:?}");

    // 再按一次才真正删码。
    press(&f.coord, VK_BACK);
    assert_eq!(f.coord.debug_input_buffer(), "u");
    type_keys(&f.coord, "u");
    let again = texts(&f);
    assert!(
        again.iter().any(|t| t == "括号") && again.iter().any(|t| t == "乙"),
        "{again:?}"
    );
}

/// 开关关闭：展开态下 Backspace 直接删码，不先退一级。
#[test]
fn backspace_collapse_switch_off_deletes_char() {
    let dir = make_data_dir("swoff", &format!("阿\ta\n乙\tuu\n{SS}\tuu\n"));
    let mut c = Config::default();
    c.schema.available = vec!["st".into()];
    c.schema.active = "st".into();
    c.input.default.chinese_mode = true;
    c.input.cmdbar.backspace_collapse_group = false;
    let ov = std::env::temp_dir().join("wind_grp_rule_ov_swoff");
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).unwrap();
    let coord = Coordinator::new_headless_with_override(c, Some(&dir), Some(ov));
    type_keys(&coord, "uu");
    let idx = coord
        .debug_all_candidate_texts()
        .iter()
        .position(|t| t == "括号")
        .unwrap();
    press(&coord, 0x31 + idx as u32);
    press(&coord, VK_BACK);
    assert_eq!(coord.debug_input_buffer(), "u");
}

/// 无重码自动展开时 Backspace 不能成死键：照常删一个码。
#[test]
fn backspace_on_auto_expanded_group_deletes_char() {
    let f = fixture("nodead", &format!("阿\ta\n{SS}\tuu\n"));
    type_keys(&f.coord, "uu");
    assert!(texts(&f).iter().any(|t| t == "【】"));
    press(&f.coord, VK_BACK);
    assert_eq!(f.coord.debug_input_buffer(), "u");
}

/// 前缀导航选中后（无重码，展开）Backspace 同样不吞键。
#[test]
fn backspace_after_prefix_nav_select_deletes_char() {
    let f = fixture("navback", &format!("阿\ta\n{SS}\tuuk\n"));
    type_keys(&f.coord, "uu");
    let all = texts(&f);
    let idx = all.iter().position(|t| t == "括号").unwrap();
    press(&f.coord, 0x31 + idx as u32);
    assert_eq!(f.coord.debug_input_buffer(), "uuk");
    press(&f.coord, VK_BACK);
    assert_eq!(f.coord.debug_input_buffer(), "uu");
}

/// 前缀匹配恒折叠成组名（即使没有任何重码）。
#[test]
fn prefix_match_always_folds() {
    let f = fixture("prefix", &format!("阿\ta\n{SS}\tuuk\n"));
    type_keys(&f.coord, "uu");
    let got = texts(&f);
    assert!(got.iter().any(|t| t == "括号"), "{got:?}");
    assert!(!got.iter().any(|t| t == "【】"), "前缀不得展开: {got:?}");
}

/// 同码两个组本身就是重码 → 两个都折叠。
#[test]
fn two_groups_on_same_code_both_fold() {
    let other = r#"$AA("圆圈", "①②③")"#;
    let f = fixture("twogroups", &format!("阿\ta\n{SS}\tuu\n{other}\tuu\n"));
    type_keys(&f.coord, "uu");
    let got = texts(&f);
    assert!(
        got.iter().any(|t| t == "括号") && got.iter().any(|t| t == "圆圈"),
        "{got:?}"
    );
    assert!(!got.iter().any(|t| t == "①"), "{got:?}");
}

/// 短语库里的组：与同码码表词同样判重码（跨来源）。
#[test]
fn phrase_layer_group_counts_codetable_duplicates() {
    let seed = |text: &str| wind_phrase::PhraseSeed {
        code: "uu".into(),
        text: text.into(),
        weight: 2000,
        position: 0,
        is_system: false,
        category: String::new(),
    };
    // 无重码：短语组展开。
    let f = fixture("ph_nodup", "阿\ta\n");
    f.coord.debug_install_phrases(vec![seed(SS)]);
    type_keys(&f.coord, "uu");
    let got = texts(&f);
    assert!(got.iter().any(|t| t == "【】"), "{got:?}");

    // 有重码：同码码表词在 → 折叠。
    let f = fixture("ph_dup", "阿\ta\n乙\tuu\n");
    f.coord.debug_install_phrases(vec![seed(SS)]);
    type_keys(&f.coord, "uu");
    let got = texts(&f);
    assert!(
        got.iter().any(|t| t == "括号") && got.iter().any(|t| t == "乙"),
        "{got:?}"
    );
    assert!(!got.iter().any(|t| t == "【】"), "{got:?}");
}

/// `$SS` 内嵌 `$CC`：成员是独立的命令候选，命令源只含该成员，不含整组。
#[test]
fn embedded_command_members_are_independent_commands() {
    let src = r#"$SS("站点", "纯文本", $CC("百度", open("https://baidu.com")), $CC("GitHub", open("https://github.com")))"#;
    // 词库文件通路
    let f = fixture("cmd_dict", &format!("阿\ta\n{src}\tuu\n"));
    type_keys(&f.coord, "uu");
    check_command_members(&f);
    // 短语库通路
    let f = fixture("cmd_phrase", "阿\ta\n");
    f.coord.debug_install_phrases(vec![wind_phrase::PhraseSeed {
        code: "uu".into(),
        text: src.into(),
        weight: 2000,
        position: 0,
        is_system: false,
        category: String::new(),
    }]);
    type_keys(&f.coord, "uu");
    check_command_members(&f);
}

fn check_command_members(f: &Fixture) {
    let shapes = f.coord.debug_candidate_shapes();
    let find = |t: &str| {
        shapes
            .iter()
            .find(|s| s.0 == t)
            .unwrap_or_else(|| panic!("缺 {t}: {shapes:?}"))
    };
    let text = find("纯文本");
    assert!(!text.1, "字面成员不是命令");
    let baidu = find("百度");
    assert!(baidu.1, "命令成员必须是 is_command");
    assert!(
        baidu.3.starts_with("$CC(") && baidu.3.contains("baidu.com") && !baidu.3.contains("github"),
        "命令源只含该成员: {}",
        baidu.3
    );
    let gh = find("GitHub");
    assert!(
        gh.1 && gh.3.contains("github.com") && !gh.3.contains("baidu"),
        "{}",
        gh.3
    );
}
