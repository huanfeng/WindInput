//! 定制版减法（`[schemas] hide`）与 **mix 成员**的相互作用：被 hide 的成员静默跳过，
//! 其余成员照常。
//!
//! 这条为什么值一个独立文件：被跳过的成员**在候选面上不可观察**——它本来就不产候选，
//! 候选面少了几条与「那个方案没词」无从区分。一个把整个 mix 判空的实现同样会让
//! 「候选里没有英文词」这类断言全绿。故直接问 `Coordinator::mix_members`
//! （经 `debug_mix_members` 直通，不另算一遍）。
//!
//! 跳过机制本身**不是**在 mix 这一层新写的过滤：`mix_members` 的门卫一直是
//! `EngineManager::ensure_schema`，而定制版的减法拦在更下面的 `read_schema`
//! （引擎构建不出来 ⇒ 门卫自然放不进来）。本用例钉的是「这条链确实通着」。
//!
//! ⚠️ **依赖 `build_dev/data` 的真实词库**：正向对照要求没被 hide 的成员**真的**能构建
//! 出引擎，而空词库的方案 `ensure_schema` 同样是 false，那样两侧就分不开了。
//! 缺 `build_dev/data` 时本用例跳过、计数照绿。
//!
//! ⚠️ **这里的跳过判据不能用耗时**（本仓词库测试族的惯用判据在这条上不成立）：`.wdat`
//! 缓存热的时候整个用例连构造带断言只跑 0.0x 秒，与跳过分支的耗时无从区分。要确认它
//! 真在跑，直接看 `build_dev/data/schemas/wubi86.schema.toml` 在不在，或
//! `cargo test … -- --nocapture` 看有没有那行「跳过：」。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：`Config::custom_manifest()` 是 OnceLock，理由同
//! `wind-engine/tests/custom_layer_hide.rs`。

use std::path::{Path, PathBuf};
use wind_config::Config;
use wind_coordinator::Coordinator;

/// 真实数据层：仓库根的 `build_dev/data`（**只读**——它是主仓 junction 共享的产物目录，
/// 测试绝不写它。本用例的 custom / user 两层都在临时目录里）。
fn build_dev_data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn hidden_schema_is_skipped_as_mix_member_while_others_stay() {
    let data = build_dev_data();
    if !data.join("schemas/wubi86.schema.toml").exists()
        || !data.join("schemas/english.schema.toml").exists()
    {
        eprintln!("跳过：缺少 build_dev/data 方案与词库");
        return;
    }

    // ⚠️ 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
    let tmp = std::env::temp_dir().join(format!(
        "wind_coord_custom_hide_mix_e2e-{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let custom = root.join("data_custom");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();

    // 清单必须在**任何** OnceLock 初始化之前就位。
    // 定制者删掉英文方案（典型诉求：这个定制版不要英文候选那一路）。
    write_at(
        &custom,
        "custom.toml",
        "[custom]\nid = \"huma-edition\"\nversion = \"1.0\"\n\n\
         [schemas]\nhide = [\"english\"]\n",
    );

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    // data 层不经 install_root 而由构造参数给出（build_dev 只读），故这里只重定向
    // user 与 custom 两层。
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

    let mut cfg = Config::default();
    cfg.schema.active = "wubi86".to_string();
    cfg.schema.available = vec!["wubi86".to_string(), "pinyin".to_string()];
    // mix 实例 0 的成员改成三个**真方案**：其中 english 已被 hide。
    cfg.schema.mix_modes[0].members = vec![
        "wubi86".to_string(),
        "pinyin".to_string(),
        "english".to_string(),
    ];
    let coord = Coordinator::new_headless(cfg, Some(&data));

    let members = coord.debug_mix_members(0);
    assert!(
        !members.contains(&"english".to_string()),
        "被 hide 的方案不得作为 mix 成员生效（read_schema → None ⇒ ensure_schema 拦下），\
         实际={members:?}"
    );
    // 正向对照：其余成员照常。没有这条，一个「mix 成员恒空」的实现同样会让上一条通过。
    assert!(
        members.contains(&"wubi86".to_string()) && members.contains(&"pinyin".to_string()),
        "其余成员必须照常生效，实际={members:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
