use super::*;

/// 夹具用的「当前主程序版本」。与清单里的 `base_version` 主次相同即不告警。
const APP_VERSION: &str = "1.2.3";

/// 一份两层夹具：`root/data` 与 `root/data_custom`。
///
/// 全部自造、不依赖 `build_dev/data`：本命令的判据只有「注册表 + 盘上的文件名」，
/// 没有一条需要真实词库，自造夹具才能让每条用例只钉住它自己那一条判据。
struct Fixture {
    _dir: tempfile::TempDir,
    data: PathBuf,
    custom: PathBuf,
}

impl Fixture {
    /// 建一份干净的两层夹具：出厂 5 个方案 / 2 个主题 / 2 本 octrie，
    /// 定制层只有清单。各用例在此基础上按需写坏一处。
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("临时目录");
        let root = dir.path().to_path_buf();
        let data = root.join("data");
        let custom = root.join("data_custom");
        std::fs::create_dir_all(data.join("schemas")).unwrap();
        std::fs::create_dir_all(data.join("themes")).unwrap();
        std::fs::create_dir_all(data.join("opencc")).unwrap();
        std::fs::create_dir_all(&custom).unwrap();
        for id in ["wubi86", "pinyin", "english", "zzz_extra", "punct_mode"] {
            std::fs::write(data.join("schemas").join(format!("{id}.schema.toml")), "").unwrap();
        }
        for id in ["default", "msime"] {
            std::fs::create_dir_all(data.join("themes").join(id)).unwrap();
            std::fs::write(data.join("themes").join(id).join("theme.toml"), "").unwrap();
        }
        for name in ["STPhrases", "STCharacters"] {
            std::fs::write(data.join("opencc").join(format!("{name}.octrie")), "").unwrap();
        }
        std::fs::write(
            data.join("config.toml"),
            "[schema]\nactive = \"wubi86\"\navailable = [\"wubi86\", \"pinyin\"]\n\n[ui.candidate]\nper_page = 5\n",
        )
        .unwrap();
        let f = Fixture {
            _dir: dir,
            data,
            custom,
        };
        f.manifest(
            "[custom]\nid = \"demo-edition\"\nname = \"演示定制版\"\nversion = \"1.0\"\nbase_version = \"1.2.0\"\n",
        );
        f
    }

    fn manifest(&self, text: &str) -> &Self {
        std::fs::write(self.custom.join(CUSTOM_MANIFEST_NAME), text).unwrap();
        self
    }

    fn config(&self, text: &str) -> &Self {
        std::fs::write(self.custom.join(CONFIG_NAME), text).unwrap();
        self
    }

    fn run(&self) -> Report {
        check_layer(&self.custom, Some(&self.data), APP_VERSION)
    }
}

/// 报告里所有 `级别 文件 键 问题 改法` 拼成一坨，供「措辞点名到具体键」的断言用。
fn dump(rep: &Report) -> String {
    rep.findings
        .iter()
        .map(|f| {
            format!(
                "{} {} {} {} {}",
                f.level.label(),
                f.file,
                f.item.clone().unwrap_or_default(),
                f.problem,
                f.fix
            )
        })
        .collect::<Vec<_>>()
        .join("\n---\n")
}

/// 断言恰好有一条命中 `key` 的结论，且级别相符；返回那条的全文。
fn one(rep: &Report, level: Level, key: &str) -> String {
    let hits: Vec<&Finding> = rep
        .findings
        .iter()
        .filter(|f| f.item.as_deref().is_some_and(|i| i.contains(key)))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "应恰好有一条点名 `{key}` 的结论，实得 {}：\n{}",
        hits.len(),
        dump(rep)
    );
    assert_eq!(hits[0].level, level, "级别不符：\n{}", dump(rep));
    format!("{} {}", hits[0].problem, hits[0].fix)
}

// ---- 干净定制层：零报告 ----------------------------------------------

#[test]
fn clean_layer_reports_nothing() {
    let f = Fixture::new();
    f.config("[ui.candidate]\nper_page = 9\n");
    let rep = f.run();
    assert!(
        rep.findings.is_empty(),
        "干净的定制层不应有任何结论：\n{}",
        dump(&rep)
    );
    assert_eq!(rep.errors(), 0);
    assert_eq!(
        rep.identity.as_deref(),
        Some("演示定制版 demo-edition 1.0（基于 1.2.0）")
    );
}

/// 减法也算干净：hide 一个盘上真实存在、且没有任何配置引用的方案。
#[test]
fn clean_layer_with_hide_reports_nothing() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"demo-edition\"\nversion = \"1.0\"\nbase_version = \"1.2.0\"\n\n[schemas]\nhide = [\"zzz_extra\"]\n",
    );
    let rep = f.run();
    assert!(rep.findings.is_empty(), "不应有结论：\n{}", dump(&rep));
}

// ---- 配置键三类 ------------------------------------------------------

#[test]
fn type_mismatch_is_error_and_names_the_key() {
    let f = Fixture::new();
    f.config("[ui.candidate]\nper_page = \"九\"\n");
    let rep = f.run();
    let text = one(&rep, Level::Error, "ui.candidate.per_page");
    assert!(text.contains("int"), "应写出期望类型：{text}");
    assert!(
        text.contains("每个用户"),
        "应说清「定制版的每个用户都会踩到」：{text}"
    );
    assert_eq!(rep.errors(), 1);
}

#[test]
fn removed_key_is_warned_and_safe_to_delete() {
    let f = Fixture::new();
    f.config("[ui.candidate]\nno_such_legacy_key = 1\n");
    let rep = f.run();
    let text = one(&rep, Level::Warn, "ui.candidate.no_such_legacy_key");
    assert!(text.contains("删掉"), "应告诉他可以删：{text}");
    assert_eq!(rep.errors(), 0, "已移除的键不该拦住打包");
}

/// 整段都不认识时只报一次，而不是把段里每一行各报一遍。
#[test]
fn removed_section_is_reported_once() {
    let f = Fixture::new();
    f.config("[ui.gone_section]\na = 1\nb = 2\nc = 3\n");
    let rep = f.run();
    assert_eq!(rep.findings.len(), 1, "应只报一次：\n{}", dump(&rep));
    one(&rep, Level::Warn, "ui.gone_section");
}

#[test]
fn enum_out_of_range_is_error_with_allowed_values() {
    let f = Fixture::new();
    f.config("[ui.candidate]\nlayout = \"diagonal\"\n");
    let rep = f.run();
    let text = one(&rep, Level::Error, "ui.candidate.layout");
    assert!(text.contains("diagonal"), "应回显非法值：{text}");
    assert!(
        text.contains("vertical") && text.contains("horizontal"),
        "应列出合法值域：{text}"
    );
}

/// 配置段被写成标量：整段会回落出厂默认，是最重的一类。
#[test]
fn section_given_a_scalar_is_error() {
    let f = Fixture::new();
    f.config("[ui]\ncandidate = 3\n");
    let rep = f.run();
    let text = one(&rep, Level::Error, "ui.candidate");
    assert!(text.contains("整段"), "应说清整段回落：{text}");
}

// ---- Map 类型不下钻 ------------------------------------------------

/// 自定义标点映射的子路径是伪键，整张表才是配置项；下钻会把每条映射报成「未知键」。
/// 只含映射表的定制层应当一条结论都没有。
#[test]
fn map_typed_keys_are_not_walked_into() {
    let f = Fixture::new();
    f.config(
        "[input.punct]\ncustom_enabled = true\n\n[input.punct.custom_mappings]\n\",\" = [\"，\"]\n\".\" = [\"。\"]\n\"/\" = [\"、\", \"／\"]\n",
    );
    let rep = f.run();
    assert!(
        rep.findings.is_empty(),
        "映射表的条目被当成配置键报了出来：\n{}",
        dump(&rep)
    );
}

/// 判据①只认不透明叶子，不认「登记过」。行为上①被②覆盖，只能直接钉谓词。
/// 放宽回「登记过」的话，同时登记 `ui.font` 与 `ui.font.family` 时①会把整张表吞掉，
/// `family` 不再被校验。
#[test]
fn is_opaque_leaf_only_matches_opaque_types() {
    // 不透明叶子：整体是一个配置项。
    assert!(is_opaque_leaf("input.punct.custom_mappings")); // Map
    assert!(is_opaque_leaf("keys.key_actions")); // Map
    assert!(is_opaque_leaf("ui.font.scripts")); // Map（带键名值域）
    assert!(is_opaque_leaf("schema.mix_modes")); // StructList

    // 登记过、但不是不透明叶子的普通标量/数组键：①不得命中。
    for key in [
        "ui.candidate.per_page",
        "ui.candidate.layout",
        "schema.active",
        "schema.available",
        "ui.theme.name",
    ] {
        assert!(field(key).is_some(), "{key} 应当是已登记的键，用例前提变了");
        assert!(
            !is_opaque_leaf(key),
            "{key} 不是 Map/StructList，判据①不得命中它——命中就意味着①被放宽回了「登记过」，\n                 那在嵌套登记出现时会把子键整个吞掉"
        );
    }
    assert!(!is_opaque_leaf("no.such.key"));
}

/// 直接断言展开结果：Map 类型的键整张表是一个配置项。`field(prefix)` 命中与「前缀下
/// 无已登记键」两道判据在当前注册表里恒同真，本用例钉的是二者的合取结果。
#[test]
fn collect_leaves_stops_at_map_typed_keys() {
    let v: toml::Value = toml::from_str(
        "[input.punct.custom_mappings]\n\",\" = [\"，\"]\n\".\" = [\"。\"]\n\n[keys.key_actions]\nbacktick = \"temp_pinyin\"\n\n[ui.font.scripts]\nlatin = [\"Consolas\"]\n",
    )
    .unwrap();
    let mut rep = Report::default();
    let mut leaves = Vec::new();
    collect_leaves("", &v, &mut leaves, &mut rep);
    let mut keys: Vec<String> = leaves.into_iter().map(|l| l.key).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "input.punct.custom_mappings".to_string(),
            "keys.key_actions".to_string(),
            "ui.font.scripts".to_string(),
        ],
        "Map 类型的键应整表作一个配置项，不得下钻"
    );
}

/// Map 带键名值域时（`ui.font.scripts` 只认几个文字类别），越界的键名要报出来，
/// 但仍然只报那一个键名，不把整张表拆成配置项。
#[test]
fn map_with_key_domain_reports_out_of_domain_entry() {
    let f = Fixture::new();
    // 值必须写成数组（`BTreeMap<String, Vec<String>>`），否则 `ui.font` 整段降级，
    // 会同时命中 M1 那条。
    f.config("[ui.font.scripts]\nno_such_script = [\"Arial\"]\n");
    let rep = f.run();
    let text = one(&rep, Level::Warn, "ui.font.scripts.no_such_script");
    assert!(text.contains("静默丢弃"), "{text}");
}

// ---- 两个静默陷阱 ----------------------------------------------------

#[test]
fn key_actions_in_custom_layer_is_warned_with_the_reason() {
    let f = Fixture::new();
    f.config("[keys.key_actions]\nbacktick = \"temp_pinyin\"\n");
    let rep = f.run();
    let text = one(&rep, Level::Warn, "keys.key_actions");
    assert!(
        text.contains("已经装过旧版包的用户") && text.contains("永远不生效"),
        "必须点明「对存量用户永远不生效、且没有日志」：{text}"
    );
}

#[test]
fn trigger_keys_in_custom_layer_is_warned() {
    let f = Fixture::new();
    // 刻意用一个与出厂值不同的绑定：与出厂相同的话会同时触发「冗余键」那条，
    // 本用例就分不清自己钉住的是哪一条判据了。
    f.config("[input.temp_pinyin]\ntrigger_keys = [\"f9\", \"f10\"]\n");
    let rep = f.run();
    let text = one(&rep, Level::Warn, "input.temp_pinyin.trigger_keys");
    assert!(text.contains("永远不生效"), "{text}");
}

// ---- 只写差异键 ------------------------------------------------------

#[test]
fn key_equal_to_factory_value_is_warned_as_redundant() {
    let f = Fixture::new();
    // 出厂 data/config.toml 里 per_page 就是 5。
    f.config("[ui.candidate]\nper_page = 5\n");
    let rep = f.run();
    let text = one(&rep, Level::Warn, "ui.candidate.per_page");
    assert!(text.contains("出厂值"), "{text}");
}

/// 出厂 `data/config.toml` 拿不到时不做这项检查：只拿内置默认当对照，
/// 会把 data 层调过的键统统报成「与出厂不同」，那是纯噪音。
#[test]
fn redundancy_check_is_skipped_without_the_data_layer() {
    let f = Fixture::new();
    f.config("[ui.candidate]\nper_page = 5\n");
    let rep = check_layer(&f.custom, None, APP_VERSION);
    assert!(
        !dump(&rep).contains("与出厂值完全相同"),
        "不该在没有出厂对照时下这个结论：\n{}",
        dump(&rep)
    );
}

// ---- 清单 ------------------------------------------------------------

#[test]
fn missing_manifest_is_error_saying_whole_layer_is_ignored() {
    let f = Fixture::new();
    std::fs::remove_file(f.custom.join(CUSTOM_MANIFEST_NAME)).unwrap();
    let rep = f.run();
    assert_eq!(rep.errors(), 1, "{}", dump(&rep));
    let text = dump(&rep);
    assert!(text.contains("custom.toml"), "{text}");
    assert!(text.contains("完全忽略"), "要说清整层被忽略：{text}");
}

#[test]
fn manifest_syntax_error_is_error_saying_layer_falls_back() {
    let f = Fixture::new();
    f.manifest("[custom\nid = \"x\"\n");
    let rep = f.run();
    assert_eq!(rep.errors(), 1, "{}", dump(&rep));
    // 必须点到「语法错误」：「字段类型不对」那条出口的措辞里同样有「整个定制层不启用」。
    assert!(dump(&rep).contains("TOML 语法错误"), "{}", dump(&rep));
    assert!(dump(&rep).contains("整个定制层不启用"), "{}", dump(&rep));
}

/// `[schema] hide`（少一个 s）解析得过、一个字都不起作用——清单刻意没有
/// `deny_unknown_fields`，这类拼写错误只能由本命令报出来。
#[test]
fn manifest_unknown_section_is_warned() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"demo\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[schema]\nhide = [\"wubi86\"]\n",
    );
    let rep = f.run();
    let text = one(&rep, Level::Warn, "schema");
    assert!(text.contains("复数"), "应提示段名是复数：{text}");
}

#[test]
fn manifest_unknown_key_inside_known_section_is_warned() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"demo\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[schemas]\nhidden = [\"wubi86\"]\n",
    );
    let rep = f.run();
    one(&rep, Level::Warn, "schemas.hidden");
}

/// `[settings]` 是认识的段：写对了零报告（不能被当成未知段告警）。
#[test]
fn settings_hide_valid_selectors_report_nothing() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"demo\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[settings]\nhide = [\"key:input.temp_english.*\", \"feature:wizard\"]\n",
    );
    let rep = f.run();
    assert!(rep.findings.is_empty(), "{}", dump(&rep));
}

/// 坏选择器逐条告警，点名 `settings.hide`，并说清「隐藏 ≠ 关闭功能」。
#[test]
fn settings_hide_bad_selector_is_warned() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"demo\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[settings]\nhide = [\"input.temp_english.*\", \"key:ui.toolbar.*\"]\n",
    );
    let rep = f.run();
    let text = one(&rep, Level::Warn, "settings.hide");
    assert!(text.contains("类型前缀"), "{text}");
    assert!(text.contains("config.toml"), "应提示关功能要改配置：{text}");
}

#[test]
fn manifest_hide_as_string_is_error() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"demo\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[schemas]\nhide = \"wubi86\"\n",
    );
    let rep = f.run();
    assert!(rep.errors() >= 1, "{}", dump(&rep));
    assert!(dump(&rep).contains("数组"), "{}", dump(&rep));
}

#[test]
fn empty_identity_is_warned() {
    let f = Fixture::new();
    f.manifest("[custom]\nbase_version = \"1.2.0\"\n");
    let rep = f.run();
    one(&rep, Level::Warn, "custom.id");
    one(&rep, Level::Warn, "custom.version");
}

#[test]
fn base_version_minor_gap_is_warned_but_patch_gap_is_not() {
    let f = Fixture::new();
    // 1.2.0 vs 1.2.3：只差补丁号，不告警（否则每次小版本更新都刷一条）。
    assert!(
        !dump(&f.run()).contains("base_version"),
        "补丁号差异不该告警：\n{}",
        dump(&f.run())
    );
    f.manifest("[custom]\nid = \"d\"\nversion = \"1\"\nbase_version = \"0.9.30\"\n");
    let rep = f.run();
    let text = one(&rep, Level::Warn, "custom.base_version");
    assert!(
        text.contains("0.9.30") && text.contains(APP_VERSION),
        "{text}"
    );
}

// ---- hide 与「还在被引用」 -------------------------------------------

#[test]
fn hide_target_absent_from_disk_is_warned() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"d\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[schemas]\nhide = [\"wubi68\"]\n",
    );
    let rep = f.run();
    let text = one(&rep, Level::Warn, "wubi68");
    assert!(text.contains("拼错"), "{text}");
    assert!(text.contains("wubi86"), "应列出盘上真实存在的 id：{text}");
}

#[test]
fn hidden_schema_still_in_available_is_error_pointing_at_the_factory_file() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"d\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[schemas]\nhide = [\"pinyin\"]\n",
    );
    let rep = f.run();
    let text = one(&rep, Level::Error, "schema.available[1]");
    assert!(
        text.contains("data_custom/config.toml"),
        "引用来自出厂文件时，改法必须是「在定制层盖掉」而不是「去改 data/」：{text}"
    );
    assert!(
        rep.findings
            .iter()
            .any(|x| x.file.contains("data/config.toml")),
        "应指明引用出自出厂 config.toml：\n{}",
        dump(&rep)
    );
}

#[test]
fn hidden_schema_still_a_mix_member_is_error() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"d\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[schemas]\nhide = [\"english\"]\n",
    );
    f.config("[[schema.mix_modes]]\nid = \"quick_mix\"\nmembers = [\"english\", \"pinyin\"]\n");
    let rep = f.run();
    let text = one(
        &rep,
        Level::Error,
        "schema.mix_modes[0](quick_mix).members → english",
    );
    assert!(text.contains("静默跳过"), "{text}");
    assert!(
        text.contains("config.toml"),
        "引用来自定制层自己的 config.toml 时应这样说：{text}"
    );
}

/// mix 成员里的 `$primary_pinyin` 是占位符：真正被引用的是 `schema.primary_pinyin`
/// 指向的方案。不解析占位符就会漏掉这一整类。
#[test]
fn hidden_schema_reached_through_the_primary_pinyin_placeholder_is_error() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"d\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[schemas]\nhide = [\"pinyin\"]\n",
    );
    f.config(
        "[schema]\nactive = \"wubi86\"\navailable = [\"wubi86\"]\nprimary_pinyin = \"\"\n\n[[schema.mix_modes]]\nid = \"quick_mix\"\nmembers = [\"$primary_pinyin\"]\n",
    );
    let rep = f.run();
    one(
        &rep,
        Level::Error,
        "schema.mix_modes[0](quick_mix).members → $primary_pinyin",
    );
}

#[test]
fn hidden_schema_bound_to_a_special_mode_key_is_error() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"d\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[schemas]\nhide = [\"punct_mode\"]\n",
    );
    f.config("[keys.key_actions]\nslash = \"special:punct_mode\"\n");
    let rep = f.run();
    let text = one(&rep, Level::Error, "keys.key_actions.slash");
    assert!(
        text.contains("什么都不会发生"),
        "要写出用户看到的现象：{text}"
    );
}

#[test]
fn hidden_theme_still_selected_is_error() {
    let f = Fixture::new();
    f.manifest(
        "[custom]\nid = \"d\"\nversion = \"1\"\nbase_version = \"1.2.0\"\n\n[themes]\nhide = [\"msime\"]\n",
    );
    f.config("[ui.theme]\nname = \"msime\"\n");
    let rep = f.run();
    let text = one(&rep, Level::Error, "ui.theme.name");
    assert!(text.contains("兜底主题"), "{text}");
}

// ---- M1：注册表看不见的坏值（Map / StructList 的内部、越界的整数） ----

/// Map 内部的值注册表看不见：`"," = "，"`（值必须是数组）只过了一层形状判定，
/// 运行时反序列化失败会让 `input.punct` 整段回落出厂默认，体检必须报错。
#[test]
fn map_value_with_the_wrong_shape_is_error() {
    let f = Fixture::new();
    f.config("[input.punct.custom_mappings]\n\",\" = \"，\"\n");
    let rep = f.run();
    assert_eq!(rep.errors(), 1, "{}", dump(&rep));
    let text = dump(&rep);
    assert!(
        text.contains("input.punct.custom_mappings"),
        "要点名到具体位置：{text}"
    );
    assert!(
        text.contains("[\"，\"]"),
        "要给出正确写法（值是数组）：{text}"
    );
}

/// 同一个洞的另一面：`ui.font.scripts` 的值也是 `Vec<String>`。
#[test]
fn map_value_wrong_shape_in_font_scripts_is_error() {
    let f = Fixture::new();
    f.config("[ui.font.scripts]\nlatin = \"Consolas\"\n");
    let rep = f.run();
    assert_eq!(rep.errors(), 1, "{}", dump(&rep));
    assert!(
        dump(&rep).contains("ui.font.scripts.latin"),
        "{}",
        dump(&rep)
    );
}

/// 注册表表达不了的值域：`-1` 是合法整数，但字段是 `usize`。
#[test]
fn out_of_range_integer_is_error() {
    let f = Fixture::new();
    f.config("[ui.candidate]\nper_page = -1\n");
    let rep = f.run();
    assert_eq!(rep.errors(), 1, "{}", dump(&rep));
    assert!(
        dump(&rep).contains("per_page"),
        "要点名到具体键：{}",
        dump(&rep)
    );
}

/// 免责 2：注册表那条已经点过名的键，第二道判据不重复报——它的措辞更好
/// （写得出期望类型），两者互补而不是叠加。
#[test]
fn registry_named_key_is_not_reported_twice() {
    let f = Fixture::new();
    f.config("[ui.candidate]\nper_page = \"九\"\n");
    let rep = f.run();
    assert_eq!(rep.errors(), 1, "同一个键只该报一次：\n{}", dump(&rep));
    assert!(
        dump(&rep).contains("应为 int"),
        "留下的应是注册表那条（措辞更好）：{}",
        dump(&rep)
    );
}

/// 免责 1：出厂 `data/config.toml` 自己就反序列化不了时，把责任栽给定制层是最坏的
/// 一种误报。此时跳过本项并明说，其余检查照常。
#[test]
fn broken_factory_config_skips_the_probe_instead_of_blaming_the_custom_layer() {
    let f = Fixture::new();
    std::fs::write(
        f.data.join("config.toml"),
        "[ui.candidate]\nper_page = \"出厂就是坏的\"\n",
    )
    .unwrap();
    f.config("[input.punct.custom_mappings]\n\",\" = \"，\"\n");
    let rep = f.run();
    let text = dump(&rep);
    assert!(
        text.contains("出厂配置自身就反序列化不了"),
        "应声明本项已跳过：{text}"
    );
    assert!(
        !text.contains("input.punct.custom_mappings"),
        "不该把出厂层的毛病算到定制层头上：{text}"
    );
}

/// 没有出厂对照（`--data` 拿不到）时同样不做——理由与冗余键那条同构。
#[test]
fn probe_is_skipped_without_the_data_layer() {
    let f = Fixture::new();
    f.config("[input.punct.custom_mappings]\n\",\" = \"，\"\n");
    let rep = check_layer(&f.custom, None, APP_VERSION);
    assert_eq!(rep.errors(), 0, "{}", dump(&rep));
}

/// 错误串里的路径抠不出来时绝不猜：猜错的路径会把人带到无关的键上。
#[test]
fn error_path_extraction_never_guesses() {
    assert_eq!(
        extract_toml_error_path(
            "invalid type: string, expected a sequence in `ui.font.scripts.latin`"
        ),
        Some("ui.font.scripts.latin".to_string())
    );
    // 真实错误串里 `in` 常在换行之后；只认空格前缀会抽不出路径，去重失效、同一个键报两遍。
    assert_eq!(
        extract_toml_error_path(
            "invalid type: string \"九\", expected usize\nin `ui.candidate.per_page`\n"
        ),
        Some("ui.candidate.per_page".to_string())
    );
    assert_eq!(extract_toml_error_path("something went wrong"), None);
    assert_eq!(extract_toml_error_path("bad in ``"), None);
    // `in` 不是独立单词（`begin \``）不算命中。
    assert_eq!(extract_toml_error_path("begin `x`"), None);
}

// ---- M2：config.toml 存在但读不出来 --------------------------------

/// config.toml 存成 GBK 时定制层配置一条都不生效（运行时只有一行 INFO）；
/// 读失败不得当成「文件不存在」而报「没有发现问题」。
#[test]
fn config_that_cannot_be_read_is_error_not_silence() {
    let f = Fixture::new();
    // GBK 编码的「[ui]\n# 注释\n」——非法 UTF-8 字节序列。
    let gbk: Vec<u8> = vec![
        b'[', b'u', b'i', b']', b'\n', b'#', b' ', 0xD7, 0xA2, 0xCA, 0xCD, b'\n',
    ];
    std::fs::write(f.custom.join(CONFIG_NAME), gbk).unwrap();
    let rep = f.run();
    assert_eq!(rep.errors(), 1, "{}", dump(&rep));
    let text = dump(&rep);
    assert!(text.contains("读不出来"), "{text}");
    assert!(text.contains("UTF-8"), "要点出最常见的成因与改法：{text}");
}

/// 反向对照：没有 config.toml 是完全正常的（只做减法的包不必有它），不该报。
#[test]
fn absent_config_is_not_a_finding() {
    let f = Fixture::new();
    assert!(!f.custom.join(CONFIG_NAME).exists());
    let rep = f.run();
    assert!(rep.findings.is_empty(), "{}", dump(&rep));
}

// ---- 简繁数据 --------------------------------------------------------

/// 半套目录是正常用法（按名逐文件覆盖），不报任何问题。
#[test]
fn partial_opencc_set_is_fine() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.custom.join("opencc")).unwrap();
    std::fs::write(f.custom.join("opencc").join("STPhrases.octrie"), "").unwrap();
    let rep = f.run();
    assert!(rep.findings.is_empty(), "半套目录不该报：\n{}", dump(&rep));
}

#[test]
fn opencc_file_with_an_unknown_name_is_warned() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.custom.join("opencc")).unwrap();
    std::fs::write(f.custom.join("opencc").join("MyDict.octrie"), "").unwrap();
    let rep = f.run();
    assert_eq!(rep.findings.len(), 1, "{}", dump(&rep));
    let text = dump(&rep);
    assert!(text.contains("MyDict"), "{text}");
    assert!(
        text.contains("任何"),
        "完全对不上的名字：后果是任何平台都取不到，措辞要说死：{text}"
    );
    assert!(text.contains("STPhrases"), "应列出正确的名字：{text}");
}

/// 只差大小写不能与「完全对不上」共用措辞：Windows/macOS 文件系统默认大小写不敏感，
/// 加载侧 `p.is_file()` 会命中它，说「永远不会被加载」只在 Linux 成立。
#[test]
fn opencc_name_differing_only_in_case_says_it_currently_works() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.custom.join("opencc")).unwrap();
    std::fs::write(f.custom.join("opencc").join("stphrases.octrie"), "").unwrap();
    let rep = f.run();
    assert_eq!(rep.findings.len(), 1, "{}", dump(&rep));
    let text = dump(&rep);
    assert!(text.contains("STPhrases"), "应给出正确拼法：{text}");
    assert!(
        text.contains("现在能被取到"),
        "必须说清「当前能用」，不能沿用「永远不会被加载」：{text}"
    );
    assert!(
        !text.contains("永远不会被加载"),
        "这一种不成立，不许出现这句话：{text}"
    );
}

// ---- 目录本身 --------------------------------------------------------

#[test]
fn missing_directory_is_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let rep = check_layer(&dir.path().join("nope"), None, APP_VERSION);
    assert_eq!(rep.errors(), 1, "{}", dump(&rep));
    // 要点到「目录不存在」：摘掉这道判据后会落到「缺少 custom.toml」，条数不变。
    assert!(dump(&rep).contains("这个目录不存在"), "{}", dump(&rep));
}

// ---- 纯函数：不碰用户层 ----------------------------------------------

/// 体检的是「这个包发出去之后会怎样」，与定制者本机的个人设置无关。守住这一条，
/// 同一个包在任何机器上体检出的结论才一致；顺带保证测试永远不会读到真实 `%APPDATA%`。
#[test]
fn check_never_touches_the_user_layer() {
    // `file!()` 是相对工作区根的路径，而测试进程的工作目录是 crate 根，拼不出来。
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config_cli/custom_check.rs");
    let src = std::fs::read_to_string(&path).expect("读自身源码");
    let body = src.split("#[cfg(test)]").next().unwrap();
    // 只列 API 名。`custom_manifest`/`custom_data_dir` 读的是本机安装的定制层，
    // 不是 --custom 指定的那份。
    for forbidden in [
        "user_config_dir",
        "Config::load(",
        "custom_manifest",
        "custom_data_dir",
    ] {
        assert!(
            !body.contains(forbidden),
            "定制层体检不该出现 `{forbidden}`：它会把定制者本机的个人配置混进结论"
        );
    }
}
