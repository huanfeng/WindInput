//! 英文词组分词索引（t42）的**失效通路**。
//!
//! 单独成文件而非并进 `engine_manager.rs`：这里测的是索引与词库启用状态的联动，
//! 与那边的「方案能不能加载、转换出不出候选」是两回事。
//!
//! ⚠️ 词库缺失时静默跳过，判据是耗时而非通过条数（同 `engine_manager.rs` 的教训）。

use std::path::PathBuf;
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_english(dir: &std::path::Path) -> bool {
    dir.join("schemas/english.schema.toml").exists() && dir.join("schemas/english").is_dir()
}

fn phrase_texts(mgr: &EngineManager, input: &str) -> Vec<String> {
    mgr.convert_with("english", input, 50)
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// ★ 关掉英文扩展词库后，分词不得再召回它里面的词组。
///
/// 关闭词库走的是**热摘**（`CodeTableEngine::set_dict_enabled` → `unregister_layer`，
/// 返回 true = 目标已达成 ⇒ 上层不重建引擎），而词组索引是在引擎构造时建的。没有失效
/// 通路的话它会继续召回 `en_ext` 里那 779/787 条词组，而同一串输入走原路径已经查不到
/// 它们了——典型的「关了没反应、顺手改别的设置又好了」。
///
/// 判据用两本库各一条：`iPhone 15 Pro Max` 在 `en_ext`、`Buenos Aires` 在主库。
/// 关掉 en_ext 后前者必须消失、后者必须还在——后半条证明作废的是索引，
/// 而不是把整个功能连坐关掉了。
#[test]
fn disabling_a_dictionary_invalidates_the_phrase_index() {
    let dir = data_dir();
    if !has_english(&dir) {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "english".into()];
    cfg.schema.active = "english".into();
    cfg.schema.english.phrase_seg = true;
    let mgr = EngineManager::new(&cfg, Some(&dir));
    assert!(mgr.ensure_schema("english"), "前提：英文方案应能加载");

    let before = phrase_texts(&mgr, "ip'max");
    assert!(
        before.iter().any(|t| t.starts_with("iPhone")),
        "前提：关库之前应能召回 en_ext 的 iPhone 词组，实际: {before:?}"
    );

    assert!(
        mgr.set_dict_enabled_live("english", "en_ext", false),
        "前提：en_ext 应能被关掉"
    );

    let after = phrase_texts(&mgr, "ip'max");
    assert!(
        !after.iter().any(|t| t.starts_with("iPhone")),
        "关掉 en_ext 后不该再召回它里面的 iPhone 词组，实际: {after:?}"
    );

    let main_dict = phrase_texts(&mgr, "bue'air");
    assert!(
        main_dict.iter().any(|t| t == "Buenos Aires"),
        "主库词组应仍在（作废的是索引，不是把功能连坐关掉），实际: {main_dict:?}"
    );
}
