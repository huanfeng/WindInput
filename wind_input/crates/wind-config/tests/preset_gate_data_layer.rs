//! 不变量 2：`data/config.toml` 缺失时 `preset_for_pruning` 返回 `None`，**即使定制层在场**。
//!
//! 加了 `data_custom` 之后闸门语义没有放宽：L2 才是出厂基线的必要部分，定制层只写差异键。
//! 拿「L1⊕L2.5」这份残缺 preset 去逐键比对，会把用户显式设的值误判成默认而删掉，
//! `load()` 时再从 L2 回落成**另一个**值——用户的设置被静默改写，比不清理坏得多。
//!
//! `preset_for_pruning` 是私有的，故经 `prune_user_config()` 观测：闸门放行与否，
//! 差别就是「等于默认的冗余键有没有被删」。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：OnceLock 与两个环境变量在进程内只认一次。

use std::path::{Path, PathBuf};
use wind_config::Config;

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn missing_data_config_disables_pruning_even_with_custom_layer() {
    let tmp = std::env::temp_dir().join("wind_preset_gate_e2e");
    let root = tmp.join("install");
    let data = root.join("data");
    let custom = root.join("data_custom");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 定制层齐备，但 data/config.toml **刻意缺席**——这正是被测的形态。
    write_at(&custom, "custom.toml", "[custom]\nid = \"huma-edition\"\n");
    write_at(&custom, "config.toml", "[ui.candidate]\nper_page = 7\n");
    std::fs::create_dir_all(&data).unwrap();

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
    assert!(
        Config::custom_data_dir().is_some(),
        "前置条件：定制层须在场，否则测的是「两层都没有」这个无关形态"
    );
    assert!(
        !data.join("config.toml").is_file(),
        "前置条件：data/config.toml 必须缺席"
    );

    // 用户层放两个键：
    // - `ui.candidate.per_page = 7` 等于 L1⊕L2.5（若闸门被放宽就会被判冗余而删掉）；
    // - 一个退役键，用来证明 prune 确实跑到了（否则「没删 per_page」可能只是没执行）。
    let user_cfg = "[schema.quick_input]\nenable_english = true\n\n[ui.candidate]\nper_page = 7\n";
    write_at(&user, "config.toml", user_cfg);

    let removed = Config::prune_user_config().unwrap();
    let text = std::fs::read_to_string(user.join("config.toml")).unwrap();
    assert!(
        removed >= 1 && !text.contains("enable_english"),
        "反事实保险：退役键必须被清掉，证明 prune 真的执行过（removed={removed}）\n{text}"
    );
    assert!(
        text.contains("per_page"),
        "★ data/config.toml 缺席时不得用残缺 preset 删用户键，实际:\n{text}"
    );

    // 正向对照：补上 L2 之后，同一个键立刻被判为冗余。证明上面那条「没删」是闸门造成的，
    // 而不是「这个键本来就删不掉」。
    write_at(&data, "config.toml", "[schema]\nactive = \"wubi86\"\n");
    let removed2 = Config::prune_user_config().unwrap();
    let text2 = std::fs::read_to_string(user.join("config.toml")).unwrap();
    assert!(
        removed2 >= 1,
        "L2 到位后冗余键应被清理，removed={removed2}\n{text2}"
    );
    assert!(
        !text2.contains("per_page"),
        "L2 到位后等于 L1⊕L2⊕L2.5 的键须被删除，实际:\n{text2}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
