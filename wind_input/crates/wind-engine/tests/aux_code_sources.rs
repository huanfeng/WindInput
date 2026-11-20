//! `[engine.aux_code].files` 的两种条目：文件路径与 `schema:<id>` 方案引用。

use std::path::PathBuf;
use wind_config::Config;
use wind_engine::{AuxSource, EngineManager};

fn setup(tag: &str, files: &str) -> (EngineManager, PathBuf) {
    let dir = std::env::temp_dir().join(format!("wind_aux_src_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
    std::fs::write(schemas.join("aux_code/t.txt"), "李=mz\n").unwrap();
    std::fs::write(
        schemas.join("pinyin.schema.toml"),
        format!(
            "[schema]\nid = \"pinyin\"\nname = \"拼\"\n[engine]\ntype = \"pinyin\"\n\
             [engine.aux_code]\nfiles = {files}\nenabled = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        schemas.join("wbx.schema.toml"),
        "[schema]\nid = \"wbx\"\nname = \"测五\"\n[engine]\ntype = \"codetable\"\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("en.schema.toml"),
        "[schema]\nid = \"en\"\nname = \"英\"\n[engine]\ntype = \"english\"\n",
    )
    .unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    (
        EngineManager::with_store_override(&cfg, Some(&dir), None, None::<PathBuf>),
        schemas,
    )
}

/// 顺序即优先级；坏条目（不存在 / 非码表 / 自己 / 空 id）逐条跳过，其余照常。
#[test]
fn schema_entries_are_resolved_in_order_and_bad_ones_skipped() {
    let (m, schemas) = setup(
        "mix",
        r#"["schema:wbx", "schema:nope", "schema:en", "schema:pinyin", "schema: ", "aux_code/t.txt"]"#,
    );
    let s = m.aux_code_settings();
    assert_eq!(
        s.sources,
        vec![
            AuxSource::Schema("wbx".into()),
            AuxSource::File(schemas.join("aux_code/t.txt")),
        ]
    );
    assert_eq!(s.schema_sources().collect::<Vec<_>>(), vec!["wbx"]);
}

#[test]
fn plain_file_entries_behave_as_before() {
    let (m, schemas) = setup("file", r#"["aux_code/t.txt", "aux_code/missing.txt"]"#);
    assert_eq!(
        m.aux_code_settings().sources,
        vec![AuxSource::File(schemas.join("aux_code/t.txt"))]
    );
}

/// `direct`（直接辅助码）的生效判据：`enabled` 是总闸、只认双拼、方案 tri-state 覆盖全局。
/// 见 `docs/design/aux-code-direct.md` §7。
#[test]
fn direct_requires_enabled_and_shuangpin() {
    let dir = std::env::temp_dir().join(format!("wind_aux_direct_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
    std::fs::write(schemas.join("aux_code/t.txt"), "李=mz\n").unwrap();
    let write = |id: &str, scheme: &str, aux: &str| {
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"{id}\"\n[engine]\ntype = \"pinyin\"\n\
                 [engine.pinyin]\nscheme = \"{scheme}\"\n\
                 [engine.aux_code]\nfiles = [\"aux_code/t.txt\"]\n{aux}"
            ),
        )
        .unwrap();
    };
    write("sp_on", "shuangpin", "enabled = true\ndirect = true\n");
    write(
        "sp_off_total",
        "shuangpin",
        "enabled = false\ndirect = true\n",
    );
    write("sp_follow", "shuangpin", "enabled = true\n");
    write("sp_veto", "shuangpin", "enabled = true\ndirect = false\n");
    write("qp_on", "", "enabled = true\ndirect = true\n");
    let manager = |global_direct: bool| {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["sp_on".into()];
        cfg.schema.active = "sp_on".into();
        cfg.schema.pinyin.aux_code.direct = global_direct;
        EngineManager::with_store_override(&cfg, Some(&dir), None, None::<PathBuf>)
    };
    let m = manager(false);
    assert!(
        m.aux_code_settings_of("sp_on").direct,
        "双拼 + enabled + direct"
    );
    assert!(
        !m.aux_code_settings_of("sp_off_total").direct,
        "enabled 是总闸：关了 direct 也不生效"
    );
    assert!(
        !m.aux_code_settings_of("sp_follow").direct,
        "方案不写 = 跟随全局（出厂关）"
    );
    assert!(
        !m.aux_code_settings_of("qp_on").direct,
        "全拼写了 direct 也不生效"
    );
    assert!(
        m.aux_code_settings_of("qp_on").enabled,
        "全拼的 direct 不生效不连累 enabled"
    );
    let m = manager(true);
    assert!(m.aux_code_settings_of("sp_follow").direct, "跟随全局开");
    assert!(
        !m.aux_code_settings_of("sp_veto").direct,
        "方案显式关压过全局开"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 逐键路径用的缓存版：设置页写方案覆盖层、重载全局配置之后立即看到新值（不能拿着旧值）。
#[test]
fn cached_settings_follow_override_writes_and_reload() {
    let dir = std::env::temp_dir().join(format!("wind_aux_cache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    let ov = dir.join("overrides");
    std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(schemas.join("aux_code/t.txt"), "李=mz\n").unwrap();
    std::fs::write(
        schemas.join("sp.schema.toml"),
        "[schema]\nid = \"sp\"\nname = \"sp\"\n[engine]\ntype = \"pinyin\"\n\
         [engine.pinyin]\nscheme = \"shuangpin\"\n\
         [engine.aux_code]\nfiles = [\"aux_code/t.txt\"]\nenabled = true\n",
    )
    .unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["sp".into()];
    cfg.schema.active = "sp".into();
    let m = EngineManager::with_store_override(&cfg, Some(&dir), None, Some(ov.clone()));
    assert!(!m.aux_code_settings_cached().direct, "出厂关");
    // 设置页写覆盖层（`write_schema_override` → `invalidate_schema`）。
    let v: toml::Value = toml::from_str("[engine.aux_code]\ndirect = true\n").unwrap();
    m.write_schema_override("sp", &v).unwrap();
    assert!(m.aux_code_settings_cached().direct, "写覆盖层后立即生效");
    m.delete_schema_override("sp").unwrap();
    assert!(!m.aux_code_settings_cached().direct, "删覆盖层后回落");
    // 全局配置重载。
    cfg.schema.pinyin.aux_code.direct = true;
    m.reload_from_config(&cfg);
    assert!(m.aux_code_settings_cached().direct, "重载全局后立即生效");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 连续辅助码的内部项（不进设置页）：`continuous` 默认随 `direct`、`continuous = false` 是逃生口、
/// `direct` 不生效时恒假；`preedit_separator` 合格值原样取、空串可用、不合格退回空格。
#[test]
fn continuous_and_preedit_separator_resolve() {
    let dir = std::env::temp_dir().join(format!("wind_aux_cont_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
    std::fs::write(schemas.join("aux_code/t.txt"), "李=mz\n").unwrap();
    let write = |id: &str, scheme: &str, aux: &str| {
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"{id}\"\n[engine]\ntype = \"pinyin\"\n\
                 [engine.pinyin]\nscheme = \"{scheme}\"\n\
                 [engine.aux_code]\nfiles = [\"aux_code/t.txt\"]\nenabled = true\n{aux}"
            ),
        )
        .unwrap();
    };
    write("sp_on", "shuangpin", "direct = true\n");
    write(
        "sp_hatch",
        "shuangpin",
        "direct = true\ncontinuous = false\n",
    );
    write("sp_cont_only", "shuangpin", "continuous = true\n");
    write("qp_on", "", "direct = true\n");
    write(
        "sp_empty",
        "shuangpin",
        "direct = true\npreedit_separator = \"\"\n",
    );
    write(
        "sp_dot",
        "shuangpin",
        "direct = true\npreedit_separator = \"·\"\n",
    );
    write(
        "sp_bad",
        "shuangpin",
        "direct = true\npreedit_separator = \"'\"\n",
    );
    let mut cfg = Config::default();
    cfg.schema.available = vec!["sp_on".into()];
    cfg.schema.active = "sp_on".into();
    let m = EngineManager::with_store_override(&cfg, Some(&dir), None, None::<PathBuf>);

    let s = m.aux_code_settings_of("sp_on");
    assert!(s.direct && s.continuous, "开 direct 的双拼默认走连续");
    assert_eq!(s.preedit_separator, None);
    assert_eq!(s.preedit_separator(), " ", "默认空格");
    let s = m.aux_code_settings_of("sp_hatch");
    assert!(s.direct && !s.continuous, "逃生口：仍是直接辅助，只做串尾");
    assert!(
        !m.aux_code_settings_of("sp_cont_only").continuous,
        "continuous 不是开关：direct 没开就不生效"
    );
    assert!(
        !m.aux_code_settings_of("qp_on").continuous,
        "全拼 direct 不生效，连续也不生效"
    );
    assert_eq!(m.aux_code_settings_of("sp_empty").preedit_separator(), "");
    assert_eq!(m.aux_code_settings_of("sp_dot").preedit_separator(), "·");
    let s = m.aux_code_settings_of("sp_bad");
    assert_eq!(s.preedit_separator, None, "不合格值不用");
    assert_eq!(s.preedit_separator(), " ");
    let _ = std::fs::remove_dir_all(&dir);
}
