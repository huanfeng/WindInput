use crate::config::PinyinGlobalConfig;
use crate::schema::Schema;

fn pinyin_spec(toml_text: &str) -> crate::schema::PinyinSpec {
    let v: toml::Value = toml::from_str(toml_text).unwrap();
    Schema::from_toml_value(v).unwrap().engine.pinyin
}

#[test]
fn empty_scheme_section_keeps_baseline() {
    let base = PinyinGlobalConfig::default();
    let (got, problems) = base.for_scheme(&pinyin_spec("[engine.pinyin]\nscheme = \"full\"\n"));
    assert_eq!(
        toml::Value::try_from(&got).unwrap(),
        toml::Value::try_from(&base).unwrap()
    );
    assert!(problems.is_empty());
}

#[test]
fn scheme_overrides_scalar_and_nested_leaf_only() {
    let base = PinyinGlobalConfig::default();
    let spec = pinyin_spec(
        "[engine.pinyin]\nscheme = \"shuangpin\"\nsentence_count = 3\n\
         [engine.pinyin.shuangpin]\nlayout = \"xiaohe\"\nallow_full_pinyin = true\n\
         [engine.pinyin.english_merge]\nexact_position = 2\n",
    );
    let (got, problems) = base.for_scheme(&spec);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(got.sentence_count, 3);
    assert!(got.shuangpin.allow_full_pinyin);
    assert_eq!(got.english_merge.exact_position, 2);
    // 同一子段里没写的项保持基线。
    assert_eq!(got.english_merge.max_count, base.english_merge.max_count);
    assert_eq!(spec.shuangpin.layout, "xiaohe");
}

#[test]
fn separator_field_folds_in() {
    let base = PinyinGlobalConfig::default();
    let (got, _) = base.for_scheme(&pinyin_spec("[engine.pinyin]\nseparator = \"none\"\n"));
    assert_eq!(got.separator, "none");
}

#[test]
fn bad_leaf_is_dropped_and_reported_others_apply() {
    let base = PinyinGlobalConfig::default();
    let spec = pinyin_spec(
        "[engine.pinyin]\nsentence_count = \"many\"\nno_such_key = 1\nsingle_char = true\n\
         [engine.pinyin.fuzzy]\nenabled = true\n",
    );
    let (got, problems) = base.for_scheme(&spec);
    assert_eq!(got.sentence_count, base.sentence_count);
    assert!(got.single_char);
    assert_eq!(got.fuzzy.enabled, base.fuzzy.enabled);
    assert_eq!(problems.len(), 3, "{problems:?}");
}

#[test]
fn integer_written_for_float_field_is_accepted() {
    let base = PinyinGlobalConfig::default();
    let (got, problems) =
        base.for_scheme(&pinyin_spec("[engine.pinyin.frequency]\nhalf_life = 48\n"));
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(got.frequency.half_life, 48.0);
}
