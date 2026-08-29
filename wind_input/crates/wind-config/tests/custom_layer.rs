//! 定制版数据层（`data_custom`）的端到端接线：清单解析 → 层序 → 四层合并 →
//! 单文件解析 → `compat.toml` 三层。
//!
//! 为什么必须是集成测试（独立进程）：`Config::custom_manifest()` 与
//! `variant::custom_userdata_dir()` 都用 OnceLock 缓存，同一进程里只解析一次盘上状态。
//! 用 `WIND_INSTALL_ROOT` 把安装根重定向到临时目录（`data/` 与 `data_custom/` 同级），
//! 用 `WIND_DATADIR_CONF` 把用户目录重定向（与 `datadir_conf.rs` 同一杠杆）。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：多个测试在同一二进制里并行会争抢这两个环境变量与
//! OnceLock，先跑的那个会把层定死，后跑的静默测到错误的目标。
//! ⚠️ 两个环境变量都**必须**设：漏掉 `WIND_DATADIR_CONF` 时 `prune_user_config()`
//! 会真写用户的 `%APPDATA%\WindInput\config.toml`（本仓已有前科）。

use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_config::app_compat::{AppCompat, FirstShowMode};

/// 在 `dir/rel` 写一个带内容的文件（父目录按需创建）。
fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn custom_layer_sits_between_system_and_user() {
    let tmp = std::env::temp_dir().join("wind_custom_layer_e2e");
    let root = tmp.join("install");
    let data = root.join("data");
    let custom = root.join("data_custom");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 清单必须在**任何** OnceLock 初始化之前就位，否则本次进程里定制层恒为关闭。
    write_at(
        &custom,
        "custom.toml",
        r#"
[custom]
id = "huma-edition"
name = "虎码定制版"
version = "1.2"
base_version = "0.9.30"

[schemas]
hide = ["wubi86", "wubi86_pinyin"]

[themes]
hide = ["msime"]
"#,
    );

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会读写真实 %APPDATA%"
    );
    assert_eq!(
        Config::data_dir(),
        Some(data.clone()),
        "前置条件：data 层须跟随安装根注入点"
    );

    // ── 清单解析与层序 ────────────────────────────────────────────────────────

    let m = Config::custom_manifest().expect("清单可解析时必须判为定制版");
    assert_eq!(m.custom.id, "huma-edition");
    assert_eq!(m.custom.name, "虎码定制版");
    assert_eq!(m.custom.version, "1.2");
    // 本期只解析保存、不做强制版本检查（留 P3）。
    assert_eq!(m.custom.base_version, "0.9.30");
    assert_eq!(m.schemas.hide, ["wubi86", "wubi86_pinyin"]);
    assert_eq!(m.themes.hide, ["msime"]);
    assert_eq!(Config::custom_data_dir(), Some(custom.clone()));

    // 层序固定 user > custom > data；顺序错了等于把定制层压过用户的个人设置。
    assert_eq!(
        Config::resource_layers(),
        vec![user.clone(), custom.clone(), data.clone()],
        "层序必须是 [user, custom, data]"
    );

    // ── config.toml 四层合并（L1 → L2 → L2.5 → L3）────────────────────────────

    write_at(
        &data,
        "config.toml",
        "[schema]\nactive = \"wubi86\"\n\n[ui.candidate]\nper_page = 5\nper_page_extended = 5\n\n[debug]\nlog_max_files = 3\n",
    );
    write_at(
        &custom,
        "config.toml",
        "[schema]\nactive = \"tigercode\"\n\n[ui.candidate]\nper_page = 7\n",
    );
    write_at(&user, "config.toml", "[schema]\nactive = \"pinyin\"\n");

    let cfg = Config::load(Some(&data)).unwrap();
    assert!(
        !cfg.degradation.is_degraded(),
        "本用例的四层都是合法配置，不该触发段级降级：{:?}",
        cfg.degradation.sections
    );
    assert_eq!(cfg.schema.active, "pinyin", "用户层(L3)压过定制层(L2.5)");
    // ★ 本组断言里最关键的一条：用户层没写的键必须落到 L2.5，而不是穿透回 L2。
    assert_eq!(
        cfg.ui.candidate.per_page, 7,
        "用户层缺失的键须回落 L2.5（定制层），不是 L2"
    );
    assert_eq!(
        cfg.ui.candidate.per_page_extended, 5,
        "定制层没写的键须保留 L2 原值（深合并，不是整段替换）"
    );
    assert_eq!(cfg.debug.log_max_files, 3, "定制层未触及的段不受影响");

    // `system_preset_value` = L1⊕L2⊕L2.5，**不含**用户层。
    // 漏了 L2.5 就会把「用户点到定制默认位」判成「与默认不同」而永久钉死。
    let preset = Config::system_preset_value(Some(&data)).unwrap();
    assert_eq!(
        preset["schema"]["active"].as_str(),
        Some("tigercode"),
        "出厂默认须含定制层的值"
    );
    assert_eq!(preset["ui"]["candidate"]["per_page"].as_integer(), Some(7));
    assert_eq!(preset["debug"]["log_max_files"].as_integer(), Some(3));
    assert_ne!(
        preset["schema"]["active"].as_str(),
        Some("pinyin"),
        "出厂默认绝不能含用户层的值"
    );

    // ── 单文件解析的三层优先级（resolve_data_file / resolve_schema_resource）──

    write_at(&data, "all.txt", "data");
    write_at(&custom, "all.txt", "custom");
    let p_user = write_at(&user, "all.txt", "user");
    assert_eq!(
        Config::resolve_data_file(Some(&data), "all.txt"),
        Some(p_user),
        "三层齐备时用户层胜出"
    );

    write_at(&data, "cd.txt", "data");
    let p_custom = write_at(&custom, "cd.txt", "custom");
    assert_eq!(
        Config::resolve_data_file(Some(&data), "cd.txt"),
        Some(p_custom),
        "用户层缺失时定制层胜出（不得直接跌到 data）"
    );

    let p_data = write_at(&data, "only_data.txt", "data");
    assert_eq!(
        Config::resolve_data_file(Some(&data), "only_data.txt"),
        Some(p_data),
        "只有 data 层时回落 data"
    );

    let p_custom_only = write_at(&custom, "only_custom.txt", "custom");
    assert_eq!(
        Config::resolve_data_file(Some(&data), "only_custom.txt"),
        Some(p_custom_only),
        "定制层独有的文件必须能解析到（这是「加法不需要声明」的兑现）"
    );

    assert_eq!(
        Config::resolve_data_file(Some(&data), "nowhere.txt"),
        None,
        "三层均无须返回 None"
    );
    assert_eq!(
        Config::resolve_data_file(Some(&data), ""),
        None,
        "空 rel 不得退化成「返回目录本身」"
    );

    // schemas/ 子目录同规则；两套根不得串味。
    write_at(&data, "schemas/common_chars.txt", "data");
    let sc_custom = write_at(&custom, "schemas/common_chars.txt", "custom");
    assert_eq!(
        Config::resolve_schema_resource(Some(&data), "common_chars.txt"),
        Some(sc_custom),
        "定制层的 schemas/ 资源须压过 data"
    );
    assert_eq!(
        Config::resolve_data_file(Some(&data), "common_chars.txt"),
        None,
        "数据根解析不得穿透到 schemas/"
    );

    // ── compat.toml 三层，且 [[apps]] 与 [[initial_mode_scope]] 两段各自合并 ──

    write_at(
        &data,
        "compat.toml",
        r#"
[[apps]]
process = "a.exe"
caret_use_top = true

[[apps]]
process = "b.exe"
host_render = true

[[initial_mode_scope]]
process = "a.exe"
classes = ["DataCls"]
"#,
    );
    write_at(
        &custom,
        "compat.toml",
        r#"
[[apps]]
process = "b.exe"
first_show_mode = "instant"

[[initial_mode_scope]]
process = "c.exe"
classes = ["CustomCls"]
"#,
    );
    write_at(
        &user,
        "compat.toml",
        r#"
[[apps]]
process = "c.exe"
caret_use_top = true
"#,
    );

    // 走 `load`（而非 `load_layered`）：要测的正是「定制层由 custom_data_dir() 自动接上」
    // 这条接线，显式传参会把它绕过去。
    let compat = AppCompat::load(Some(&data), Some(&user));
    assert!(
        compat.get_rule("a.exe").unwrap().caret_use_top,
        "data 层里没被任何上层覆盖的规则须原样保留"
    );
    let b = compat.get_rule("b.exe").expect("b.exe 规则应在");
    assert_eq!(
        b.first_show_mode,
        FirstShowMode::Instant,
        "定制层须覆盖 data 层的同名进程"
    );
    assert!(
        !b.host_render,
        "同名进程是**整条覆盖**：定制层没写 host_render 就该是 false，不做字段级合并"
    );
    assert!(
        compat.get_rule("c.exe").unwrap().caret_use_top,
        "用户层规则须生效"
    );
    // ★ 两段各自合并：用户层为 c.exe 写了 [[apps]]，不得连带丢掉定制层给 c.exe 配的
    // [[initial_mode_scope]]。把两段并成一段就会在这里翻车。
    assert!(
        compat.initial_mode_applies_to_window("c.exe", "CustomCls"),
        "定制层的 initial_mode_scope 须生效"
    );
    assert!(
        !compat.initial_mode_applies_to_window("c.exe", "OtherCls"),
        "该进程确有作用域条目（否则任何窗口类都会返回 true，上一条断言即为假绿）"
    );
    assert!(
        compat.initial_mode_applies_to_window("a.exe", "DataCls"),
        "data 层的 initial_mode_scope 未被上层触及时须保留"
    );

    // ── 不变量 1：prune 前后 load() 逐键完全相同 ──────────────────────────────

    // 用户层里放两个键：一个等于「定制默认」（应被清掉，从此跟随定制层），
    // 一个是用户真实设置（必须留下）。
    write_at(
        &user,
        "config.toml",
        "[schema]\nactive = \"pinyin\"\n\n[ui.candidate]\nper_page = 7\n",
    );
    let before = toml::Value::try_from(Config::load(Some(&data)).unwrap()).unwrap();
    let removed = Config::prune_user_config().unwrap();
    assert!(
        removed >= 1,
        "等于定制默认(L2.5)的键必须被判为冗余并清掉，否则用户会被永久钉死在该值上"
    );
    let after_text = std::fs::read_to_string(user.join("config.toml")).unwrap();
    assert!(
        !after_text.contains("per_page"),
        "等于 L1⊕L2⊕L2.5 的键应被删除，实际:\n{after_text}"
    );
    assert!(
        after_text.contains("pinyin"),
        "与默认不同的用户设置绝不能被删，实际:\n{after_text}"
    );
    let after = toml::Value::try_from(Config::load(Some(&data)).unwrap()).unwrap();
    assert_eq!(
        before, after,
        "不变量：清理前后 load() 结果须逐键完全相同（删掉的键都会回落到同一个值）"
    );
    // 幂等：再跑一次删 0 个。
    assert_eq!(Config::prune_user_config().unwrap(), 0, "prune 必须幂等");

    let _ = std::fs::remove_dir_all(&tmp);
}
