//! 方案常驻策略的引擎侧边界（`EngineManager::residency_protected` / `evict_idle` /
//! `refresh_schema_cache`），协调器侧的端到端在 wind-coordinator `tests/schema_residency.rs`。
//!
//! 缓存根经 `XDG_DATA_HOME` 重定向到临时目录（进程级 `OnceLock`，本二进制第一次初始化），
//! 方案全部自造：码表 ra / rb、混输 rm（主码表成员 rb），不依赖 build_dev。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use wind_config::Config;
use wind_engine::EngineManager;

static LOCK: Mutex<()> = Mutex::new(());

fn data_dir() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("wind_engine_residency-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // SAFETY: 首个用例进来时、任何 EngineManager 构造之前设置，之后只读。
        unsafe {
            std::env::set_var("XDG_DATA_HOME", root.join("xdg"));
        }
        let schemas = root.join("data/schemas");
        for (id, word) in [("ra", "甲"), ("rb", "乙")] {
            std::fs::create_dir_all(schemas.join(id)).unwrap();
            std::fs::write(
                schemas.join(format!("{id}.schema.toml")),
                format!(
                    "[schema]\nid = \"{id}\"\nname = \"{id}\"\n[engine]\ntype = \"codetable\"\n\
                     [engine.codetable]\nmax_code_length = 4\n\
                     [[dictionaries]]\nid = \"main\"\npath = \"{id}/{id}.dict.yaml\"\ndefault = true\n"
                ),
            )
            .unwrap();
            std::fs::write(
                schemas.join(format!("{id}/{id}.dict.yaml")),
                format!("---\nname: {id}\nversion: \"1\"\n...\n{word}\ta\n{word}{word}\taa\n"),
            )
            .unwrap();
        }
        std::fs::write(
            schemas.join("rm.schema.toml"),
            "[schema]\nid = \"rm\"\nname = \"rm\"\n[engine]\ntype = \"mixed\"\n\
             [engine.mixed]\nprimary_schema = \"rb\"\nsecondary_schema = \"\"\n",
        )
        .unwrap();
        root.join("data")
    })
}

fn manager(active: &str) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.active = active.into();
    cfg.schema.available = vec!["ra".into(), "rb".into(), "rm".into()];
    cfg.input.temp_pinyin.enabled = false;
    cfg.input.temp_english.show_candidates = false;
    let ov = data_dir().join("../overrides");
    EngineManager::with_store_override(&cfg, Some(data_dir()), None, Some(ov))
}

/// 混输自建子引擎、不用表里那份独立成员引擎：保护它只是多留一份没人用的副本。
#[test]
fn members_of_loaded_mixed_are_not_protected() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let em = manager("rm");
    assert!(em.prewarm_schema("rb"));
    assert!(
        !em.residency_protected().contains("rb"),
        "混输成员不保护：{:?}",
        em.residency_protected()
    );
    assert_eq!(em.evict_idle(Duration::ZERO), vec!["rb"]);
    assert_eq!(em.loaded_schemas(), vec!["rm"]);
    assert!(
        !em.convert_with("rm", "a", 5).candidates.is_empty(),
        "混输照常出字（它握着自己的成员子引擎）"
    );
}

/// 校验「建完即摘」不能摘掉这期间被人用过的方案（用户恰好切过去 / 在用）。
#[test]
fn refresh_keeps_schema_touched_between_build_and_unload() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let em = manager("ra");
    assert!(em.refresh_schema_cache_hooked("rb", || {
        assert!(!em.convert_with("rb", "a", 5).candidates.is_empty());
    }));
    assert_eq!(em.loaded_schemas(), vec!["ra", "rb"], "建完后被用过 ⇒ 不摘");
    // 对照：没人碰过就照常摘。
    let em = manager("ra");
    assert!(em.refresh_schema_cache("rb"));
    assert_eq!(em.loaded_schemas(), vec!["ra"]);
}

/// 摘掉编码来源方案（混输的主码表成员）的独立引擎时，它的单字全码表仍在用，不清。
#[test]
fn unloading_code_source_keeps_its_single_char_table() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let em = manager("rm");
    assert_eq!(em.code_source_schema(), "rb");
    assert!(em.prewarm_schema("rb"));
    assert!(em.prewarm_single_char_codes("rb"));
    assert!(em.evict_idle(Duration::ZERO).contains(&"rb".to_string()));
    assert!(
        em.single_char_codes_ready("rb"),
        "编码来源的单字全码表仍在用（自动造词取码），不随独立引擎清掉"
    );
}
