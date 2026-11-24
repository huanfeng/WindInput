//! 定制版减法（`custom.toml` 的 `[schemas] hide`）在 wind-engine 侧的端到端接线。
//!
//! 钉住六件事：
//!
//! 1. 主拦截点 `read_schema` 对被 hide 的方案返回 `None`（经 `ensure_schema` 观察——它是 mix 成员 /
//!    特殊模式激活前的门卫，返回 false 即「引擎构建不出来」）；
//! 2. 上层列表双保险：`installed_schemas` / `available_schemas` 都不含它；
//! 3. 被 hide 的 `[overlay]` 方案进不了特殊模式注册表（`special:<id>` 的分发源）；
//! 4. hide 是绝对的：用户在 `%APPDATA%\WindInput\schemas\` 放一份同名方案文件仍然不可见
//!    （见 `Config::custom_hides_schema` 的取舍说明）；
//! 5. `schema.active` 指向被 hide 的方案 ⇒ 降级到第一个可用方案；
//! 6. 降级不改写用户配置（真读盘比对 `config.toml` 的字节）：改写会让用户切回原版后丢掉自己的选择。
//!    防的是将来有人给降级分支加一句 `Config::set_user_string(&["schema","active"], …)`。
//!
//! 依赖 `build_dev/data` 的真实词库：第 1 条要求被 hide 的方案在没有 hide 时真能构建出引擎，
//! 自造的空词库方案 `ensure_schema` 恒 false，摘掉闸门照样绿。缺 `build_dev/data` 时跳过、计数照绿；
//! 跳过判据不能用耗时（缓存热时整个用例只跑 0.0x 秒），要确认它真在跑，看
//! `build_dev/data/schemas/wubi86.schema.toml` 在不在，或 `--nocapture` 看有没有「跳过：」。
//!
//! 必须是集成测试（独立进程）：`Config::custom_manifest()` 用 OnceLock 缓存。用 `WIND_INSTALL_ROOT`
//! 把安装根（= custom 层的根）、`WIND_DATADIR_CONF` 把用户目录重定向到临时目录；data 层由构造参数给出
//! （`build_dev/data` 只读，测试绝不写它）。全文件仅此一个 `#[test]`，否则并行会争抢环境变量与
//! OnceLock。两个环境变量都必须设：漏掉 `WIND_DATADIR_CONF` 时用户层会指向真实的 `%APPDATA%\WindInput`。

use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_engine::EngineManager;

/// 真实数据层：仓库根的 `build_dev/data`。
fn build_dev_data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

/// 最小可用的码表方案文件（`is_supported()` 为真即可进 `installed_schemas`；
/// 无词库 ⇒ 构建不出引擎，故只用于「列出来没有」这类断言）。
fn schema_toml(id: &str) -> String {
    format!("[schema]\nid = \"{id}\"\n\n[engine]\ntype = \"codetable\"\n")
}

/// 带 `[overlay]` 段的方案 = 一个「特殊模式」，进 `overlay_modes` 注册表。
fn overlay_schema_toml(id: &str) -> String {
    format!("{}\n[overlay]\n", schema_toml(id))
}

#[test]
fn hidden_schemas_are_absent_from_every_layer_and_active_falls_back() {
    let data = build_dev_data();
    if !data.join("schemas/wubi86.schema.toml").exists()
        || !data.join("schemas/pinyin.schema.toml").exists()
    {
        eprintln!("跳过：缺少 build_dev/data 方案与词库");
        return;
    }

    // 目录名带 pid：多 worktree / 多会话并行跑测试时，固定名 + 开头的 `remove_dir_all` 会互删夹具。
    let tmp = std::env::temp_dir().join(format!(
        "wind_engine_custom_hide_e2e-{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let custom = root.join("data_custom");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 清单必须在任何 OnceLock 初始化之前就位，否则本次进程里定制层恒为关闭。
    // 定制者的典型意图：删掉出厂五笔，换成自带的虎码。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\nid = \"huma-edition\"\nversion = \"1.0\"\n\n\
         [schemas]\nhide = [\"wubi86\", \"user_named\", \"aa_ov_hidden\"]\n",
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
        Config::custom_data_dir(),
        Some(custom.clone()),
        "前置条件：清单在场时 custom 层必须启用"
    );

    // custom 层：定制版自带的替代方案（加法不需要声明，直接放文件）。
    //
    // id 是刻意挑的，别改成 `huma`：降级落点的三条判据（层序 / overlay 排除 / english 排最后）
    // 互相兜底，夹具要让每一条都成为唯一屏障：
    // - `zz_huma` 排在所有 data 层 id 之后 ⇒ 只有「层序」能让它赢；
    // - `aa_ov_kept` 排在 custom 层最前 ⇒ 只有「overlay 排除」能挡住它；
    // - english 由下面 `cfg_en` 那个子场景负责（它在用户自己的 available 里排第一）。
    write_at(
        &custom,
        "schemas/zz_huma.schema.toml",
        &schema_toml("zz_huma"),
    );
    // custom 层：两个特殊模式（`[overlay]`），一个被 hide 一个不被 hide
    write_at(
        &custom,
        "schemas/aa_ov_hidden.schema.toml",
        &overlay_schema_toml("aa_ov_hidden"),
    );
    write_at(
        &custom,
        "schemas/aa_ov_kept.schema.toml",
        &overlay_schema_toml("aa_ov_kept"),
    );
    // 用户层：用户拿被 hide 的 id 命名了自己的方案——仍然不可见（hide 是绝对的）。
    write_at(
        &user,
        "schemas/user_named.schema.toml",
        &schema_toml("user_named"),
    );

    // 用户配置：active 还指着被删掉的 wubi86（原版用户升级到定制版的必然状态）。
    // 真写一份 config.toml，稍后按字节比对它有没有被程序改写。
    let user_config = user.join("config.toml");
    let user_config_text = "[schema]\nactive = \"wubi86\"\navailable = [\"wubi86\", \"pinyin\"]\n";
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(&user_config, user_config_text).unwrap();
    let before = std::fs::read(&user_config).unwrap();

    let mut cfg = Config::default();
    cfg.schema.active = "wubi86".to_string();
    cfg.schema.available = vec!["wubi86".to_string(), "pinyin".to_string()];
    let mgr = EngineManager::new(&cfg, Some(&data));

    // ── 1. 主拦截点：read_schema → None ⇒ build_engine 走不到它 ───────────────
    assert!(
        mgr.ensure_schema("pinyin"),
        "前置条件：没被 hide 的真方案必须**真的**能构建出引擎，\
         否则下一条断言两侧同因异果（空词库方案 ensure_schema 恒 false）"
    );
    assert!(
        !mgr.ensure_schema("wubi86"),
        "被 hide 的方案必须构建不出引擎（read_schema 返回 None），\
         否则 mix 成员 / 特殊模式的门卫会放它进来"
    );

    // ── 2. 上层列表双保险 ─────────────────────────────────────────────────────
    let installed = mgr.installed_schemas();
    assert!(
        !installed.contains(&"wubi86".to_string()),
        "installed_schemas 不得含被 hide 的方案，实际={installed:?}"
    );
    assert!(
        installed.contains(&"pinyin".to_string()) && installed.contains(&"zz_huma".to_string()),
        "其余各层的方案必须照常列出（data 的 pinyin、custom 的 zz_huma），实际={installed:?}"
    );
    let available = mgr.available_schemas();
    assert!(
        !available.contains(&"wubi86".to_string()),
        "available_schemas 不得含被 hide 的方案（构造期的 retain 会无条件保留活跃方案，\
         这道过滤就是为它准备的），实际={available:?}"
    );
    assert!(
        available.contains(&"pinyin".to_string()),
        "available 里其余方案照常，实际={available:?}"
    );

    // ── 2b. 特殊模式注册表（`special:<id>` 的分发源）─────────────────────────
    //
    // 注册表建自 `installed_schemas`，`read_schema` 与 `scan_layer_schema_ids` 两道都能拦住它，
    // 要同时摘掉两处本条才变红。
    assert!(
        mgr.overlay_index_of("aa_ov_kept").is_some(),
        "前置条件：没被 hide 的 overlay 方案必须在特殊模式注册表里，否则下一条恒真"
    );
    assert!(
        mgr.overlay_index_of("aa_ov_hidden").is_none(),
        "被 hide 的 overlay 方案不得进特殊模式注册表——否则 `special:<id>` 还能把它唤起来"
    );

    // ── 3. hide 是绝对的：用户层同名文件也不复活 ─────────────────────────────
    assert!(
        user.join("schemas/user_named.schema.toml").is_file(),
        "前置条件：用户层那份同名方案文件确实在盘上，否则下面这条断言恒真"
    );
    assert!(
        !installed.contains(&"user_named".to_string()),
        "被 hide 的 id 在**任何层**都不存在，用户层放一份同名文件也不例外，实际={installed:?}"
    );

    // ── 4. schema.active 指向被 hide 的方案 ⇒ 降级 ───────────────────────────
    assert_eq!(
        mgr.active_schema_id(),
        "pinyin",
        "active 被 hide 时应降级到 schema.available 里第一个可用方案"
    );

    // ── 5. 降级不改写用户配置（真读盘比对字节）─────────────────────────────
    let after = std::fs::read(&user_config).unwrap();
    assert_eq!(
        before, after,
        "降级只在内存里发生：用户 config.toml 必须逐字节未变。\
         改写它会让用户切回原版 / 卸掉定制包之后丢掉自己的 schema.active"
    );
    assert_eq!(
        std::fs::read_to_string(&user_config).unwrap(),
        user_config_text,
        "内容也要与写进去的原文一致（防止 before/after 同时被改成同一个错值）"
    );

    // ── 5b. 降级落点必须是定制版自带的替代品，不能是 english ────────────────
    //
    // 单方案用户（`schema.available` 只有五笔）装上虎码定制版：候选表若按字典序取第一个受支持的，
    // 落点是 `english`，首启一个汉字都打不出。`build_dev/data` 里没有方案标 `[schema] hidden`，
    // 加 hidden 过滤挡不住它——本用例钉的是排序 + 类型档位。
    assert!(
        installed.contains(&"english".to_string()),
        "前置条件：english 必须确实在候选范围内，否则「没落到 english」恒真，实际={installed:?}"
    );
    let mut cfg_solo = Config::default();
    cfg_solo.schema.active = "wubi86".to_string();
    cfg_solo.schema.available = vec!["wubi86".to_string()];
    let mgr_solo = EngineManager::new(&cfg_solo, Some(&data));
    assert_eq!(
        mgr_solo.active_schema_id(),
        "zz_huma",
        "单方案用户的降级落点必须是定制版自带的方案（custom 层优先于 data 层），\
         不是字典序或 data 层里碰巧排在前面的 english"
    );
    // available 不得变成空表：retain 的两条判据「等于活跃方案」（已降级成 zz_huma）与「受支持」
    // （wubi86 被 hide）在这个场景下双双落空，空表 = 方案菜单空白 + 循环切换键毫无反应。
    assert_eq!(
        mgr_solo.available_schemas(),
        vec!["zz_huma".to_string()],
        "降级目标必须进 available，否则方案菜单与循环切换都拿到空表"
    );
    // 这个场景里循环切换确实无处可去（用户只启用了一个方案）；「循环切换可用」由下面的双方案变体钉住。

    // 用户的 available 里就有 english 时，english 也不该赢——档位优先于列表顺序。
    // 同时这是 `ensure_fallback_listed` 生效后循环切换真的有得可去的场景。
    let mut cfg_en = Config::default();
    cfg_en.schema.active = "wubi86".to_string();
    cfg_en.schema.available = vec!["wubi86".to_string(), "english".to_string()];
    let mgr_en = EngineManager::new(&cfg_en, Some(&data));
    assert_eq!(
        mgr_en.active_schema_id(),
        "zz_huma",
        "english 在用户自己的 available 里也不该赢——它不出汉字，档位排最后"
    );
    let av_en = mgr_en.available_schemas();
    assert_eq!(
        av_en,
        vec!["zz_huma".to_string(), "english".to_string()],
        "降级目标插到表头，用户原有的 english 保留 ⇒ 循环切换有得可去，实际={av_en:?}"
    );

    // ── 6. available_schemas 那道过滤唯一独立生效的场合 ───────────────────
    //
    // 第 2 条里构造期的 `retain`（`schema_supported` → `read_schema` → None）已筛掉被 hide 的方案，
    // 那道过滤真正生效的是「活跃方案被无条件保留」（`sid == &active_id`）这条旁路，只在降级也找不到
    // 可用方案时才留下被 hide 的 active。这里造出那个角落：data 层是空目录 ⇒ 无处可降级。
    let empty_data = tmp.join("empty_data");
    std::fs::create_dir_all(&empty_data).unwrap();
    // 降级的兜底扫描看全部层（user / custom / data），故 custom 层那几个方案也得挪走，
    // 否则它们会成为可降级目标。
    for rel in [
        "schemas/zz_huma.schema.toml",
        "schemas/aa_ov_kept.schema.toml",
        "schemas/aa_ov_hidden.schema.toml",
    ] {
        std::fs::remove_file(custom.join(rel)).unwrap();
    }
    let mut cfg2 = Config::default();
    cfg2.schema.active = "wubi86".to_string();
    cfg2.schema.available = vec!["wubi86".to_string()];
    let mgr2 = EngineManager::new(&cfg2, Some(&empty_data));
    assert_eq!(
        mgr2.active_schema_id(),
        "wubi86",
        "前置条件：无处可降级时 active 原样保留（此时它就是被 hide 的那个）"
    );
    assert!(
        mgr2.available_schemas().is_empty(),
        "被 hide 的方案即便因「始终保留活跃方案」留在了内部列表里，\
         也不得从 available_schemas 出去，实际={:?}",
        mgr2.available_schemas()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
