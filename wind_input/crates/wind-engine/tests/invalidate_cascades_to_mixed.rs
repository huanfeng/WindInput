//! 失效一个方案，必须连带失效把它当成员的混输方案。
//!
//! 从 `engines` 表里摘掉一条，换不动已建好的引擎手里那个 `Arc`：混输在 `build_engine` 时就接走了
//! 成员子引擎。不扇出的后果有两重：
//!
//! - 功能：改了成员方案的配置，混输里仍是旧子引擎，表现为「关了没反应，顺手改别的设置又好了」。
//! - 内存：旧英文引擎被混输吊着，`shared_english_engine` 下次又建一份，共享英文引擎省下的原样吐回去。
//!
//! `write_schema_override` / `delete_schema_override` / `rebuild_all_caches` / `set_dict_enabled_live`
//! 都要扇出。判据落在 `is_loaded`（公共 API）而不是指针相等或 `Arc::strong_count`：要问的是
//! 「下次用混输时会不会重建」，`engines` 表里还在不在就是那个判据。
//!
//! 词库缺失时静默跳过（同 `english_engine_is_shared.rs` 的约定）。

use std::path::PathBuf;
use wind_config::Config;
use wind_engine::EngineManager;

const MIXED: &str = "wubi86_pinyin";
/// 混输的 primary（见 `schemas/wubi86_pinyin.schema.toml` 的 `[engine.mixed]`）。
const MEMBER: &str = "wubi86";

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn fixtures_present(dir: &std::path::Path) -> bool {
    // 英文词库目录也要在：缺了它 english 建不起来，下面「前提：混输借了共享英文引擎」
    // 会红，而不是按文件头说的静默跳过。
    dir.join("schemas/english.schema.toml").exists()
        && dir.join("schemas/english").is_dir()
        && dir.join(format!("schemas/{MIXED}.schema.toml")).exists()
}

/// 每个用例一个独立的 override 目录——写 override 是这些用例的动作本身，共用会串台。
/// 目录名带进程号：并发会话同时跑同一个用例时会互相 `remove_dir_all`。
fn manager(tag: &str) -> Option<(EngineManager, PathBuf)> {
    manager_with(tag, true)
}

fn manager_with(tag: &str, enable_english: bool) -> Option<(EngineManager, PathBuf)> {
    let dir = data_dir();
    if !fixtures_present(&dir) {
        eprintln!("跳过：缺少英文方案或混输方案");
        return None;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec![MIXED.into(), "english".into()];
    cfg.schema.active = MIXED.into();
    cfg.schema.mix.enable_english = enable_english;
    let ov = std::env::temp_dir().join(format!("wind_inv_cascade_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).ok()?;
    let mgr = EngineManager::with_store_override(&cfg, Some(&dir), None, Some(ov.clone()));
    Some((mgr, ov))
}

fn write_override(mgr: &EngineManager, id: &str) {
    let mut t = toml::value::Table::new();
    // 键无所谓，要的只是「override 变了」这个事件；用一个真实存在的布尔键，
    // 免得将来 override 解析加了严格校验时本用例莫名其妙地红。
    t.insert("commit_space".into(), toml::Value::Boolean(false));
    mgr.write_schema_override(id, &toml::Value::Table(t))
        .expect("写 override 应成功");
}

/// 改英文方案的配置 ⇒ 混输必须一起失效。
///
/// 变异验证：删掉 `invalidate_schema` 里的扇出循环即红（`english=false mixed=true`）。
#[test]
fn invalidating_english_also_invalidates_the_mixed_schema_that_borrows_it() {
    let Some((mgr, _ov)) = manager("english") else {
        return;
    };
    assert!(mgr.ensure_schema(MIXED), "前提：混输方案应能加载");
    assert!(mgr.is_loaded("english"), "前提：混输借了共享英文引擎");

    write_override(&mgr, "english");

    assert!(!mgr.is_loaded("english"), "英文方案自身当然要失效");
    assert!(
        !mgr.is_loaded(MIXED),
        "混输必须一起失效：它手里那个英文 Arc 是构造时接过来的，\
         `engines` 表换了登记也换不动它——不重建就是旧引擎继续服务，且旧的一直不释放"
    );
}

/// 改成员方案（混输的 primary 码表）的配置 ⇒ 混输同样必须失效。
///
/// 与上一条判据不同：english 走 `loaded_mixed_dependents` 里「一律返回」的快路，成员方案走读方案文件
/// 比对 `primary_schema` / `secondary_schema` 那条，删掉任一条都只会红一个用例。混输里那份码表是
/// 内联构造的副本、从不入 `engines` 表（`is_loaded(MEMBER)` 一直是 false），只能靠扇出换掉。
#[test]
fn invalidating_a_member_schema_also_invalidates_the_mixed_schema() {
    let Some((mgr, _ov)) = manager("member") else {
        return;
    };
    assert!(mgr.ensure_schema(MIXED), "前提：混输方案应能加载");

    write_override(&mgr, MEMBER);

    assert!(
        !mgr.is_loaded(MIXED),
        "改了 primary 码表的配置，混输必须重建；否则它内部那份副本永远是旧的"
    );
}

/// 锐利性对照：失效一个与混输无关的方案，混输不许被牵连。
///
/// 没有这一条，把扇出写成「一律失效所有已加载的混输」也能让上面两条全绿，那会让任何无关的配置
/// 改动都白白重建混输（秒级、用户可感知）。
#[test]
fn invalidating_an_unrelated_schema_leaves_the_mixed_schema_alone() {
    let Some((mgr, _ov)) = manager("unrelated") else {
        return;
    };
    assert!(mgr.ensure_schema(MIXED), "前提：混输方案应能加载");

    // 一个不是 MIXED 任何成员、也不是 english 的 id。
    mgr.invalidate_schema("zz_not_a_member");

    assert!(
        mgr.is_loaded(MIXED),
        "无关方案的失效不该波及混输——扇出的判据是成员关系，不是「是个混输就失效」"
    );
}

/// `enable_english` 关着时改英文方案的配置，混输不许被牵连。
///
/// `loaded_mixed_dependents` 对 english 一律返回全部混输；扇出若不看开关，混输手里根本没有英文
/// 子引擎时也会每保存一次英文 override 就重建所有混输。上一条用的是无关方案 id，守不住 english 这支。
/// 变异验证：删掉 `invalidate_schema` 里的 `english_unused_by_mixed` 判断即红。
#[test]
fn invalidating_english_leaves_mixed_alone_when_english_merge_is_off() {
    let Some((mgr, _ov)) = manager_with("english_off", false) else {
        return;
    };
    assert!(mgr.ensure_schema(MIXED), "前提：混输方案应能加载");
    assert!(
        !mgr.is_loaded("english"),
        "前提：开关关着时混输不该去建英文引擎"
    );

    write_override(&mgr, "english");

    assert!(
        mgr.is_loaded(MIXED),
        "混输没借英文子引擎，改英文配置不该让它重建"
    );
}
