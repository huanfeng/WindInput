use super::*;

/// 移动端强制项在快照里可见：设置页、引擎读到的必须是同一个值。
#[test]
fn overrides_reach_snapshot_defaults() {
    let cfg = apply_overrides_to(Config::default());
    assert!(
        !cfg.mobile.association.space_commits,
        "联想态空格出厂不选联想（移动端段）"
    );
    assert_eq!(cfg.ui.candidate.preedit_display, "candidate_top");
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// 没选过 = 装了的全部；选过 = 交集 + 英文；一个中文都没留 = 全部
#[test]
fn enabled_schemas_rules() {
    let installed = ids(&["english", "pinyin", "shuangpin", "wubi86"]);
    assert_eq!(enabled_from(&installed, None), installed, "没选过：全部");
    assert_eq!(
        enabled_from(&installed, Some(&ids(&["wubi86", "pinyin", "gone"]))),
        ids(&["pinyin", "wubi86", "english"]),
        "交集（没装的丢掉）、英文补上"
    );
    assert_eq!(
        enabled_from(&installed, Some(&ids(&["english"]))),
        installed,
        "一个中文方案都没留：退回全部"
    );
    // 出厂列表 ["wubi86", "wubi86_pinyin"] 在移动端不该起作用——它不在用户层，
    // 这里只验纯函数：用户层若真写了它，也照交集办（不含英文时补上）
    let with_mix = ids(&["english", "wubi86", "wubi86_pinyin"]);
    assert!(
        enabled_from(&with_mix, Some(&ids(&["wubi86", "wubi86_pinyin"])))
            .contains(&"english".to_string())
    );
}

/// 段名模板的显示名：可选组整个去掉，字面文字留下
#[test]
fn tooltip_section_title_strips_optional_groups() {
    assert_eq!(section_title("编码{(${code_source})}"), "编码");
    assert_eq!(section_title("拆字"), "拆字");
    assert_eq!(section_title("{${x}}"), "");
}

/// 不存在的段名当场拒绝（不落盘）
#[test]
fn unknown_tooltip_section_is_rejected() {
    let dir = std::env::temp_dir();
    assert!(set_tooltip_section_enabled(&dir, "没有这一段", true).is_err());
}

/// 不存在的模块 id 当场拒绝（不落盘）
#[test]
fn unknown_emoji_module_is_rejected() {
    let dir = std::env::temp_dir();
    assert!(set_emoji_module(&dir, "no_such_module", true).is_err());
}

/// 内置模块都在列表里，没设过的取出厂开关
#[test]
fn builtin_emoji_modules_listed_with_factory_defaults() {
    let dir = std::env::temp_dir().join(format!("wind_mobile_emoji_{}", std::process::id()));
    let mods = emoji_modules(&dir);
    for m in wind_config::EMOJI_MODULES {
        let got = mods
            .iter()
            .find(|s| s.id == m.id)
            .expect("内置模块应在列表里");
        assert!(!got.user);
        assert_eq!(got.label, m.label);
    }
}

fn quick_mix_raw(extra: &str) -> Vec<toml::Value> {
    let text = format!(
        "[[schema.mix_modes]]\nid = \"quick_mix\"\ntrigger_keys = [\"semicolon\"]\n\
             members = [\"quick_input.calc\", \"english\"]\ncandidate_layout = \"vertical\"\n{extra}"
    );
    let v: toml::Value = toml::from_str(&text).unwrap();
    mix_modes_of(&v).unwrap()
}

/// 只换 quick_mix 的 members：触发键、布局等字段与别的实例原样保留
#[test]
fn patch_quick_mix_members_keeps_other_fields() {
    let base = quick_mix_raw("[[schema.mix_modes]]\nid = \"other\"\nmembers = [\"pinyin\"]\n");
    let out = patch_quick_mix_members(base, &ids(&["english", "$rare_char"])).unwrap();
    let q = out[0].as_table().unwrap();
    assert_eq!(
        q["members"].as_array().unwrap(),
        &vec![
            toml::Value::String("english".into()),
            toml::Value::String("$rare_char".into())
        ]
    );
    assert_eq!(
        q["trigger_keys"].as_array().unwrap(),
        &vec![toml::Value::String("semicolon".into())],
        "触发键不能被写成空（那会关掉快捷输入）"
    );
    assert_eq!(q["candidate_layout"].as_str(), Some("vertical"));
    assert_eq!(
        out[1].as_table().unwrap()["members"].as_array().unwrap(),
        &vec![toml::Value::String("pinyin".into())],
        "别的实例不动"
    );
}

/// 重复与空白成员丢掉；全关（空列表）照写
#[test]
fn patch_quick_mix_members_dedups_and_allows_empty() {
    let out =
        patch_quick_mix_members(quick_mix_raw(""), &ids(&["english", " ", "english"])).unwrap();
    assert_eq!(out[0]["members"].as_array().unwrap().len(), 1);
    let out = patch_quick_mix_members(quick_mix_raw(""), &[]).unwrap();
    assert!(out[0]["members"].as_array().unwrap().is_empty());
}

/// 没有 quick_mix 实例 → 拒绝，不新建
#[test]
fn patch_quick_mix_members_requires_instance() {
    let v: toml::Value = toml::from_str("[[schema.mix_modes]]\nid = \"other\"\n").unwrap();
    assert!(patch_quick_mix_members(mix_modes_of(&v).unwrap(), &ids(&["english"])).is_err());
    assert!(patch_quick_mix_members(Vec::new(), &ids(&["english"])).is_err());
}

/// 出厂原始值里 quick_mix 带着触发键：用户层没写过 mix_modes 时以它为底，写回后 `;` 仍能进
#[test]
fn preset_mix_modes_carry_trigger_keys() {
    let preset = toml::Value::try_from(Config::default()).unwrap();
    let modes = mix_modes_of(&preset).expect("出厂应有 mix_modes");
    let q = modes
        .iter()
        .find(|m| m.get("id").and_then(toml::Value::as_str) == Some(QUICK_MIX_ID))
        .expect("出厂应有 quick_mix");
    assert!(!q["trigger_keys"].as_array().unwrap().is_empty());
}

/// 注册表里报成 enum 的键都要带值域：设置页拿空选项画不出选择器（LayoutEnum 曾漏填）。
#[test]
fn every_enum_field_has_options() {
    let missing: Vec<_> = registry()
        .into_iter()
        .filter(|f| f.ty == "enum" && f.options.is_empty())
        .map(|f| f.key)
        .collect();
    assert!(missing.is_empty(), "以下枚举键没有选项：{missing:?}");
}
