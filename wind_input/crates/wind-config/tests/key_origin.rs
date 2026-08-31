//! `Config::key_origin` 的四层追溯：每层声明了什么、生效的是哪一层、降级标志。
//!
//! 为什么必须是集成测试（独立进程）：与 `custom_layer.rs` 同因——`custom_manifest()`
//! 与 `variant::custom_userdata_dir()` 都是 OnceLock，同一进程里只解析一次盘上状态。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：多个测试在同一二进制里并行会争抢环境变量与 OnceLock。
//! ⚠️ `WIND_DATADIR_CONF` 必须设，否则 `prune_user_config()` 会真写用户的
//! `%APPDATA%\WindInput\config.toml`（本仓已有前科）。

use std::path::{Path, PathBuf};
use wind_config::Config;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

/// 取某层的声明值（层名恒在，故直接 unwrap 定位失败即测试自身写错了）。
fn layer_value<'a>(o: &'a wind_config::KeyOrigin, name: &str) -> Option<&'a toml::Value> {
    o.layers
        .iter()
        .find(|l| l.layer == name)
        .unwrap_or_else(|| panic!("层 {name} 不在 layers 里"))
        .value
        .as_ref()
}

#[test]
fn key_origin_traces_all_four_layers() {
    let tmp = std::env::temp_dir().join("wind_key_origin_e2e");
    let root = tmp.join("install");
    let data = root.join("data");
    let custom = root.join("data_custom");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 清单必须在任何 OnceLock 初始化之前就位，否则本次进程里定制层恒为关闭。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\nid = \"huma\"\nname = \"虎码定制版\"\nversion = \"1.0\"\nbase_version = \"0.119.0\"\n",
    );

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }

    // L2：出厂层。`per_page` 三层都写，`flip_when_above` 只有这一层写。
    //
    // ⚠️ 这里的键名必须是**注册表里真实存在**的：写一个不存在的键（第一版写了
    // `horizontal`），serde 会静默丢弃它 ⇒ 生效值为 None ⇒ 归属报 None，
    // 看起来像本函数的 bug，实际是它判对了。
    write_at(
        &data,
        "config.toml",
        r#"
[ui.candidate]
per_page = 5
flip_when_above = true

[input.punct]
[input.punct.custom_mappings]
"," = ["，"]
"#,
    );
    // L2.5：定制层夹在中间。
    write_at(&custom, "config.toml", "[ui.candidate]\nper_page = 7\n");
    // L3：用户层。另写一个 custom_mappings 条目，用来验「表是跨层深合并的」；
    // 再把 keys.key_actions 写成标量制造段级降级。
    write_at(
        &user,
        "config.toml",
        r#"
[ui.candidate]
per_page = 9

[input.punct.custom_mappings]
"." = ["。"]

[keys]
key_actions = "这不是一张表"
"#,
    );

    // ── 三层都声明的标量：逐层可见，最高层生效 ──────────────────
    let o = Config::key_origin("ui.candidate.per_page", Some(&data)).unwrap();
    assert!(
        layer_value(&o, "default").is_some(),
        "代码默认层必须有这个键，否则注册表与结构体已脱节"
    );
    assert_eq!(layer_value(&o, "data"), Some(&toml::Value::Integer(5)));
    assert_eq!(layer_value(&o, "custom"), Some(&toml::Value::Integer(7)));
    assert_eq!(layer_value(&o, "user"), Some(&toml::Value::Integer(9)));
    assert_eq!(o.effective, Some(toml::Value::Integer(9)));
    assert_eq!(
        o.effective_layer,
        Some("user"),
        "标量由最高声明层整体覆盖，归属必须指得出来"
    );
    assert!(!o.degraded);

    // 层序是从低到高：呈现端直接按顺序渲染就是覆盖关系，不必自己排。
    let names: Vec<&str> = o.layers.iter().map(|l| l.layer).collect();
    assert_eq!(names, vec!["default", "data", "custom", "user"]);

    // ── 只有出厂层声明：归属指向 data，而不是「没人声明」──────────
    let o = Config::key_origin("ui.candidate.flip_when_above", Some(&data)).unwrap();
    assert_eq!(layer_value(&o, "custom"), None, "定制层没写这个键");
    assert_eq!(layer_value(&o, "user"), None, "用户层没写这个键");
    assert_eq!(o.effective_layer, Some("data"));

    // ── 表类型跨层深合并：归属必须是 None ──────────────────────
    //
    // ★ 这一条是本文件的核心。生效值是 data 的 `,` 与 user 的 `.` 的并集，
    // 报「user 生效」会把人带偏——他照着去改 user 层，改不动来自 data 的那一半。
    let o = Config::key_origin("input.punct.custom_mappings", Some(&data)).unwrap();
    let eff = o.effective.as_ref().unwrap().as_table().unwrap();
    assert!(eff.contains_key(","), "出厂层的条目应当还在");
    assert!(eff.contains_key("."), "用户层的条目应当合进来");
    assert_eq!(
        o.effective_layer, None,
        "多层并集指不到单独一层，必须报 None 而不是最高声明层"
    );

    // ── 段级降级：用户层写了值，但那一段整个回落了 ────────────────
    let o = Config::key_origin("keys.key_actions", Some(&data)).unwrap();
    assert!(
        layer_value(&o, "user").is_some(),
        "用户层确实写了这个键——正因如此，`degraded` 才是唯一能解释「为什么不生效」的信息"
    );
    assert!(
        o.degraded,
        "keys 段因标量而降级，该键必须报 degraded；否则用户只看到「配置文件里写着但不生效」"
    );

    // ── 不存在的键：四层全空，不 panic ─────────────────────────
    let o = Config::key_origin("ui.candidate.no_such_key", Some(&data)).unwrap();
    assert!(o.layers.iter().all(|l| l.value.is_none()));
    assert_eq!(o.effective, None);
    assert_eq!(o.effective_layer, None);

    let _ = std::fs::remove_dir_all(&tmp);
}
