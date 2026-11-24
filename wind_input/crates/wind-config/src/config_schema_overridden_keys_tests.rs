use super::*;

fn schema_of(body: &str) -> crate::schema::Schema {
    let src = format!("[schema]\nid = \"t\"\n[engine]\ntype = \"codetable\"\n{body}");
    toml::from_str(&src).unwrap_or_else(|e| panic!("最小方案解析失败：{e}\n{src}"))
}

#[test]
fn overridden_keys_lists_codetable_option_fields_once() {
    let s = schema_of(
        "[engine.codetable]\ntop_code_commit = true\n[candidate]\nlayout = \"vertical\"\n",
    );
    let keys = schema_overridden_keys(&s);
    assert!(
        keys.contains(&"schema.codetable.top_code_commit"),
        "{keys:?}"
    );
    assert!(keys.contains(&"ui.candidate.layout"), "{keys:?}");
    assert_eq!(
        keys.iter()
            .filter(|k| **k == "schema.codetable.top_code_commit")
            .count(),
        1
    );
}

#[test]
fn overridden_keys_ignores_follow_and_none() {
    // 显式写 follow、写空的码元字符集、只写引擎固定参数：都不算覆盖全局。
    let s = schema_of(
        "[engine.codetable]\nmax_code_length = 4\ninput_chars = \"\"\n[candidate]\nlayout = \"follow\"\n[punct]\nmode = \"chinese\"\n",
    );
    assert!(
        schema_overridden_keys(&s).is_empty(),
        "{:?}",
        schema_overridden_keys(&s)
    );
}

/// 前缀型登记在方案文件里的落点段。与 `SchemaOverride::section` 刻意不共用：那是给用户看
/// 的提示文案（带方括号，如 `[engine.codetable]`），这里要的是点分路径。
fn schema_section_of_prefix(prefix: &str) -> &'static str {
    match prefix {
        "schema.codetable." => "engine.codetable",
        "schema.codetable.frequency." => "engine.codetable.frequency",
        "schema.codetable.english_merge." => "engine.codetable.english_merge",
        other => panic!(
            "新增了前缀登记 {other}：在此补它在方案文件里的段，并让 schema_overridden_keys 认它"
        ),
    }
}

/// 通用折叠登记的结构约束：必须是 `ui.candidate.<叶子>`、落点段是 `[candidate]`、
/// 注册表里有这个键，且叶子名不能与 `CandidateSpec` 的强类型字段撞车
/// （撞了 serde 会让强类型字段吃掉它，补丁永远收不到——登记成了摆设）。
#[test]
fn candidate_typed_keys_are_not_foldable() {
    for o in SCHEMA_OVERRIDES.iter().filter(|o| o.fold) {
        let leaf = o
            .key
            .strip_prefix("ui.candidate.")
            .unwrap_or_else(|| panic!("{} 不在 ui.candidate. 下，通用折叠器不支持", o.key));
        assert!(!leaf.contains('.'), "{} 只支持直接子键", o.key);
        assert_eq!(
            o.section, "[candidate]",
            "{} 的落点段必须是 [candidate]",
            o.key
        );
        assert!(is_known_key(o.key), "{} 未在注册表登记", o.key);
        assert!(
            !crate::schema::CANDIDATE_TYPED_KEYS.contains(&leaf),
            "{leaf} 是 CandidateSpec 的强类型字段，不能走通用折叠"
        );
    }
}

/// 精确登记项的最小方案片段——只设这一项。
fn minimal_setting_of_exact(key: &str) -> &'static str {
    match key {
        "schema.pinyin.aux_code.enabled" => "[engine.aux_code]\nenabled = true\n",
        "schema.pinyin.aux_code.max_phrase_len" => "[engine.aux_code]\nmax_phrase_len = 3\n",
        "schema.pinyin.aux_code.direct" => "[engine.aux_code]\ndirect = true\n",
        // 方案级没有 `custom_enabled`（见 `PunctSpec::custom_mappings` 的 ⛔）：声明了自己的
        // 表就整表替换、连全局开关一起换掉。于是两条登记由同一个字段判定，这是登记语义
        // 本身（见 SCHEMA_OVERRIDES 那条 note），不是豁免。
        "input.punct.custom_mappings" | "input.punct.custom_enabled" => {
            "[punct.custom_mappings]\n\",\" = [\"，\", \"，\", \"，\", \",\"]\n"
        }
        "ui.candidate.layout" => "[candidate]\nlayout = \"vertical\"\n",
        other => panic!(
            "新增了登记项 {other}：在此补一份只设该项的最小方案片段，并让 schema_overridden_keys 认它"
        ),
    }
}

fn sample_value(key: &str) -> String {
    match field(key)
        .unwrap_or_else(|| panic!("{key} 未在注册表登记"))
        .ty
    {
        FieldType::Bool => "true".into(),
        FieldType::Int => "1".into(),
        FieldType::Float => "1.5".into(),
        FieldType::Str => "\"x\"".into(),
        FieldType::Enum(vs) => format!("{:?}", vs[0]),
        other => panic!("{key} 的类型 {other:?} 在方案级没有标量形态，测试需要单独处理"),
    }
}

/// 每个被 SCHEMA_OVERRIDES 登记为「可被方案覆盖」的键，都必须能被本函数识别，
/// 否则全局页的「已被 N 个方案覆盖」会对它恒显示 0（R8.6），这是护栏。
///
/// 前缀型登记按注册表展开成它管辖的**全部直接子键**（与 `schema_override_of` 同一判据），
/// 每个键各造一份只设该键的方案。展开取自注册表而不是 `CodeTableSpec` 字段：全局页的行
/// 是按注册表画的，注册表里多出一个方案读不进去的键，这里就该红。
///
/// 豁免：无。每条登记都能由方案侧某个 Option / 非 Follow / 非空字段判定。
#[test]
fn overridden_keys_covers_every_schema_override_entry() {
    let mut checked = 0;
    for o in SCHEMA_OVERRIDES {
        let cases: Vec<(&str, String)> = if o.key.ends_with('.') {
            let section = schema_section_of_prefix(o.key);
            let keys: Vec<&str> = registry()
                .iter()
                .map(|f| f.key)
                .filter(|k| schema_override_of(k).is_some_and(|hit| hit.key == o.key))
                .collect();
            assert!(
                !keys.is_empty(),
                "前缀登记 {} 在注册表里一个子键都没有",
                o.key
            );
            keys.into_iter()
                .map(|k| {
                    let leaf = &k[o.key.len()..];
                    (k, format!("[{section}]\n{leaf} = {}\n", sample_value(k)))
                })
                .collect()
        } else if o.fold {
            // 通用折叠键：片段由注册表类型自动生成，新增一条登记不需要改测试。
            let leaf = o
                .key
                .strip_prefix("ui.candidate.")
                .expect("fold 键必须在 ui.candidate. 下");
            vec![(
                o.key,
                format!("[candidate]\n{leaf} = {}\n", sample_value(o.key)),
            )]
        } else {
            vec![(o.key, minimal_setting_of_exact(o.key).to_string())]
        };
        for (key, body) in cases {
            let got = schema_overridden_keys(&schema_of(&body));
            assert!(
                got.contains(&key),
                "只设了 {key} 的方案没被识别为覆盖它（片段：{body:?}，结果：{got:?}）"
            );
            checked += 1;
        }
    }
    // 防「循环体一次没进」的空过：登记表 + 前缀展开至少有这么多键。
    assert!(checked >= 30, "只验了 {checked} 个键，展开逻辑可能坏了");
}

#[test]
fn comment_dyn_keys_parse_by_shape() {
    use super::{CommentDynKey as K, comment_dyn_key};
    assert_eq!(
        comment_dyn_key("ui.comment.assign.global.detail_vertical"),
        Some(K::Cell {
            scope: "global",
            name: None,
            cell: "detail_vertical"
        })
    );
    assert_eq!(
        comment_dyn_key("ui.comment.assign.mode.mix:quick_mix.default_horizontal"),
        Some(K::Cell {
            scope: "mode",
            name: Some("mix:quick_mix"),
            cell: "default_horizontal"
        })
    );
    assert_eq!(
        comment_dyn_key("ui.comment.template.my_tpl"),
        Some(K::Template { id: "my_tpl" })
    );
    for bad in [
        "ui.comment.assign.global.nope",
        "ui.comment.assign.type.default_vertical",
        "ui.comment.assign.other.x.default_vertical",
        "ui.comment.template.",
        "ui.comment.template.my.tpl",
        "ui.comment.template.My",
        "ui.comment.template.none",
        "ui.comment.assign.mode.a.b.default_vertical",
    ] {
        assert_eq!(comment_dyn_key(bad), None, "{bad}");
    }
    assert!(is_addressable_key(
        "ui.comment.assign.type.pinyin.default_vertical"
    ));
    assert!(!is_known_key(
        "ui.comment.assign.type.pinyin.default_vertical"
    ));
    // 值形状：模板 id / none / inherit / 空串可，其余拒。
    let v = |x: &str| {
        validate(
            "ui.comment.assign.global.default_vertical",
            &toml::Value::from(x),
        )
    };
    assert!(v("").is_ok() && v("none").is_ok() && v("my_tpl").is_ok());
    assert!(v("My Tpl").is_err());
}

#[test]
fn lookup_json_reads_dynamic_keys_and_blank_when_unset() {
    let full = serde_json::json!({"ui":{"comment":{
        "templates":[{"id":"t1","name":"n","text":"${code}"}],
        "assign":{"global":{"default_vertical":"t1"},"type":{"pinyin":{"detail_vertical":"t1"}}}}}});
    let g = |k| lookup_json_value(&full, k).unwrap();
    assert_eq!(g("ui.comment.template.t1"), "${code}");
    assert_eq!(g("ui.comment.template.zz"), "");
    assert_eq!(g("ui.comment.assign.global.default_vertical"), "t1");
    assert_eq!(g("ui.comment.assign.global.detail_vertical"), "");
    assert_eq!(g("ui.comment.assign.type.pinyin.detail_vertical"), "t1");
    assert_eq!(g("ui.comment.assign.mode.url.default_vertical"), "");
}
