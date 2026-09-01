//! **闸四**的端到端验证：用户 `config.toml` 语法不合法时，谁都不许覆盖它。
//!
//! # 这条测试对应一次真机数据丢失
//!
//! 用户在 `config.toml` 里重复写了一个字段（TOML 里重复键是语法错误），重启后
//! 整份配置被替换成只含 `key_actions` 的空壳，全程无提示。链条是：
//!
//! 1. `read_toml_value` 的 `Err ⇒ None` 让用户层被**静默丢弃**，而 `degradation`
//!    全程干净——段级降级只看合并**之后**的 `try_into`，看不见合并**之前**的语法错误；
//! 2. 于是四道写盘闸（只问 `degradation`）全部放行；
//! 3. `materialize_key_actions` 里 `unwrap_or_else(空表)` 把空表当成种子整表写回。
//!
//! 放大器：`already_materialized(空表)` 恒为 false ⇒ 每次启动都重跑一遍。
//!
//! # 两条路径**刻意**不同处置，本文件把这个区别钉住
//!
//! | 路径 | 触发者 | 处置 |
//! |---|---|---|
//! | `materialize_key_actions` | 后台自动（用户没要求任何事） | 一个字节都不写 |
//! | `set_user_value` | 用户刚点了保存 | 先备份原件，再写救回的部分 |
//!
//! 后者不能也拒绝：那会变成「设置页点了没反应」，是本仓反复栽过的另一类坑。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：这些函数写用户层 config.toml，漏设
//! `WIND_DATADIR_CONF` 会真写 `%APPDATA%\WindInput\config.toml`（本仓已有前科）。

use std::path::{Path, PathBuf};
use wind_config::Config;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn unparsable_user_config_is_never_silently_overwritten() {
    let tmp = std::env::temp_dir().join("wind_unparsable_cfg_e2e");
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 闸二要求 L2 在场（否则测到的是闸二而不是闸四）。
    write_at(&data, "config.toml", "[schema]\nactive = \"wubi86\"\n");

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会写真实 %APPDATA%"
    );

    // 用户手写的配置：`per_page` 重复了一次（第 6 行），其余完全正常。
    // 这就是真机上那份配置的最小形态。
    let poisoned = "\
[schema]
active = \"pinyin\"

[ui.candidate]
per_page = 9
per_page = 5

[keys]
key_actions = { \"F2\" = \"toggle_schema\" }
";
    let file = write_at(&user, "config.toml", poisoned);

    // ── 一、load() 必须把语法故障如实报出来 ──────────────────────────────────

    let cfg = Config::load(Config::data_dir().as_deref()).expect("语法错不该让 load 整个失败");
    assert!(
        cfg.degradation.has_unparsable(),
        "语法故障必须进 degradation——它是四道写盘闸、RPC、设置页横幅共同的唯一出口"
    );
    assert!(cfg.degradation.is_degraded());
    let u = &cfg.degradation.unparsable[0];
    assert_eq!(u.layer, "user");
    assert_eq!(
        u.skipped_lines,
        vec![6],
        "报给用户的行号须是 1-based 的原始行号"
    );
    assert!(u.is_salvaged(), "其余三段应当救回来了");

    // 救回的部分确实生效了（容错不是摆设）。
    assert_eq!(cfg.schema.active, "pinyin", "无关段必须照常生效");

    // 判据传导：任何路径都不可信 ⇒ 写盘闸全部关上。
    assert!(cfg.degradation.taints("keys"));
    assert!(cfg.degradation.taints("ui.candidate"));

    // ── 二、materialize_key_actions：一个字节都不许改 ─────────────────────────

    let before = std::fs::read_to_string(&file).unwrap();
    let n = Config::materialize_key_actions().expect("闸拦下时应返回 Ok(0) 而不是 Err");
    assert_eq!(n, 0, "语法不合法时物化必须什么都不做");
    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        before, after,
        "闸四失效：用户配置被改写了。这正是真机上那次数据丢失的形态"
    );

    // 幂等标记也不许写——一旦置位，修好语法后也不会再自愈。
    assert!(
        !after.contains("key_actions_materialized"),
        "被拦下时不该留下版本标记"
    );

    // ── 三、set_user_value：允许写，但必须先备份 ──────────────────────────────

    // ⚠️ 值必须**不等于**出厂默认（L2 写的是 wubi86）：`set_user_value` 对等于默认的值
    // 走的是「删除该键」而不是「写入」，拿 wubi86 来测会得到一个没有 schema 段的文件，
    // 看起来像写丢了，实际是剪枝在正常工作。
    Config::set_user_value(&["schema", "active"], toml::Value::String("wubi98".into()))
        .expect("用户主动保存不该因为语法错而失败——那会变成「点了没反应」");

    let backups: Vec<PathBuf> = std::fs::read_dir(&user)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(".corrupt-") && n.ends_with(".bak"))
        })
        .collect();
    assert_eq!(backups.len(), 1, "语法不合法时写回必须留下且仅留下一份备份");
    assert_eq!(
        std::fs::read_to_string(&backups[0]).unwrap(),
        poisoned,
        "备份必须是**原件逐字节**，否则它救不回被跳过的那一行"
    );

    // 写回后的文件是「救回的部分 ⊕ 本次修改」，且已经是合法 TOML。
    let saved = std::fs::read_to_string(&file).unwrap();
    let v: toml::Value = toml::from_str(&saved).expect("写回的内容必须是合法 TOML");
    assert_eq!(
        v["schema"]["active"].as_str(),
        Some("wubi98"),
        "本次修改要落盘"
    );
    assert_eq!(
        v["ui"]["candidate"]["per_page"].as_integer(),
        Some(9),
        "救回的其余设置要一并保住——否则这条路径仍在丢用户数据"
    );

    // 第二次保存时文件已合法 ⇒ 不该再产生备份（否则每存一次攒一个文件）。
    Config::set_user_value(&["schema", "active"], toml::Value::String("pinyin".into())).unwrap();
    let n_backups = std::fs::read_dir(&user)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
        .count();
    assert_eq!(n_backups, 1, "文件已修好后不该继续攒备份");

    let _ = std::fs::remove_dir_all(&tmp);
}
