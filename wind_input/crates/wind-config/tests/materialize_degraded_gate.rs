//! P0 连带闸门（闸三）的端到端验证：本次 `load()` 的 `keys` 段被降级时，
//! `materialize_key_actions()` 必须**什么都不做**。
//!
//! 为什么这条非测不可：段级降级把 `load()` 的 `Err` 变成了「成功但内容残缺」，
//! 而 `materialize_into` 是**无条件整表覆盖**并打一次性版本标记 ⇒ 闸门失效的后果是
//! 用户自定义按键绑定从磁盘**永久**消失、且再也不会重跑自愈；毒若恰在 `key_actions`
//! 里还会被自己覆盖掉，事后连现场都不剩。
//!
//! 此前这道闸只能靠代码审阅确认：`Config::data_dir()` 是 `current_exe()/data` 的硬编码，
//! 没有注入点，而闸二要求 `data/config.toml` 在场。`WIND_INSTALL_ROOT`（仿
//! `WIND_DATADIR_CONF`）补上了这个缺口，本文件是它的第一个消费者。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：`materialize_key_actions` 写用户层 config.toml，
//! 漏设 `WIND_DATADIR_CONF` 会真写 `%APPDATA%\WindInput\config.toml`。

use std::path::{Path, PathBuf};
use wind_config::Config;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn degraded_keys_section_blocks_materialization() {
    let tmp = std::env::temp_dir().join("wind_materialize_gate_e2e");
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 闸二要求 L2 在场（出厂绑定只在 L2 看得见）。
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
    assert!(
        data.join("config.toml").is_file(),
        "前置条件：闸二要求 data/config.toml 在场，否则测到的是闸二而非闸三"
    );

    // ── 有毒的 keys 段 ⇒ 一个字节都不许写 ────────────────────────────────────

    // `key_actions` 是 `BTreeMap<String, String>`，给它一个整数即类型不匹配。
    // 段级降级会把它定位到 `keys.key_actions`（顶层段再探一层子键），
    // 而闸三判的是 `affects("keys")`——前缀匹配，精确相等会漏判。
    let poisoned = "[keys]\nkey_actions = 42\n\n[schema]\nactive = \"pinyin\"\n";
    let file = write_at(&user, "config.toml", poisoned);

    // 前置确认：这份配置确实触发了 keys 段降级，否则下面的 Ok(0) 可能来自别的原因。
    let cfg = Config::load(Some(&data)).unwrap();
    assert!(
        cfg.degradation.affects("keys"),
        "前置条件：本用例须真的触发 keys 段降级，实际 sections={:?} total={}",
        cfg.degradation.sections,
        cfg.degradation.total_fallback
    );
    assert_eq!(
        cfg.schema.active, "pinyin",
        "降级只该丢有毒的那一段，其余段的用户值必须保留"
    );

    assert_eq!(
        Config::materialize_key_actions().unwrap(),
        0,
        "keys 段被降级时必须什么都不做"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        poisoned.as_bytes(),
        "★ 用户 config.toml 必须一个字节都没变（含未被写入的物化版本标记）"
    );

    // ── 正向对照：毒去掉之后照常物化 ────────────────────────────────────────
    //
    // 没有这一步，上面的 Ok(0) 与「这套环境下本来就无事可做」无法区分。

    let clean = "[keys.key_actions]\nbacktick = \"temp_english\"\n";
    std::fs::write(&file, clean).unwrap();
    let count = Config::materialize_key_actions().unwrap();
    assert!(count >= 1, "无降级时应正常物化，实得 {count}");
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(
        after.contains("key_actions_materialized"),
        "物化须落一次性版本标记，实际:\n{after}"
    );
    assert!(
        after.contains("backtick"),
        "用户已有的绑定须保留，实际:\n{after}"
    );

    // 幂等：版本标记已置位，再跑一次删 0 个。
    assert_eq!(
        Config::materialize_key_actions().unwrap(),
        0,
        "物化必须幂等"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
