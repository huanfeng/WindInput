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

// ---------- 闲置清扫（设计 §7）：单字全码表、按词查编码用户层 ----------

fn manager_with_store(
    active: &str,
    tag: &str,
) -> (EngineManager, std::sync::Arc<wind_store::Store>) {
    let p = std::env::temp_dir().join(format!(
        "wind_engine_residency_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    let store = std::sync::Arc::new(wind_store::Store::open(&p).unwrap());
    let mut cfg = Config::default();
    cfg.schema.active = active.into();
    cfg.schema.available = vec!["ra".into(), "rb".into(), "rm".into()];
    cfg.input.temp_pinyin.enabled = false;
    cfg.input.temp_english.show_candidates = false;
    let ov = data_dir().join("../overrides");
    let em =
        EngineManager::with_store_override(&cfg, Some(data_dir()), Some(store.clone()), Some(ov));
    (em, store)
}

const SHORT: Duration = Duration::from_millis(40);
/// 「刚用过不释放」那两条的阈值：秒级，慢机器上「用」与「清扫」之间多耽搁一会儿也不至于
/// 越过阈值而假红（释放方向的用例用 `SHORT` 无此顾虑）。
const KEEP: Duration = Duration::from_secs(2);
const LONG: Duration = Duration::from_secs(3600);

/// 等到 `cond` 成立（上限 5 秒）：按词查编码用户层的重建在后台线程上。
fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// 单字全码表：闲置到期被释放；释放后再用按需重建，取码结果与释放前一致。
#[test]
fn idle_single_char_table_is_released_and_rebuilt_on_demand() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let em = manager("ra");
    assert!(em.prewarm_single_char_codes("ra"));
    let before = em.encode_words("ra", &["甲"]);
    assert_eq!(before, vec!["a".to_string()], "前提：单字直取全码");
    std::thread::sleep(SHORT * 2);
    let released = em.evict_idle_caches(SHORT);
    assert!(
        released.iter().any(|r| r.contains("ra")),
        "闲置到期应释放单字全码表：{released:?}"
    );
    assert!(!em.single_char_codes_ready("ra"), "释放后不再就绪");
    assert_eq!(em.encode_words("ra", &["甲"]), before, "按需重建、结果一致");
    assert!(em.single_char_codes_ready("ra"));
}

/// 单字全码表：闲置期间用过就不释放（「最后使用」打在取表处）。
#[test]
fn used_single_char_table_is_kept() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let em = manager("ra");
    assert!(em.prewarm_single_char_codes("ra"));
    std::thread::sleep(KEEP + Duration::from_millis(300));
    // 只推进时钟，谁都不放；随后用一次——它的「最后使用」= 这一拍。
    assert!(em.evict_idle_caches(LONG).is_empty());
    let _ = em.encode_words("ra", &["甲"]);
    assert!(em.evict_idle_caches(KEEP).is_empty(), "刚用过不释放");
    assert!(em.single_char_codes_ready("ra"));
}

/// 按词查编码用户层：闲置到期的槽被释放；释放后再查起后台重建，结果与释放前一致。
#[test]
fn idle_user_text_slot_is_released_and_rebuilt_on_demand() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (em, store) = manager_with_store("ra", "ut_idle");
    store.add_user_word("ra", "zzzz", "甲", 0, 0).unwrap();
    assert!(em.prewarm_text_codes("ra"));
    assert_eq!(em.user_text_loaded(), 1);
    let before: Vec<String> = em
        .text_codes("ra")
        .codes_of("甲")
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(
        before.contains(&"zzzz".to_string()),
        "前提：用户码在：{before:?}"
    );
    std::thread::sleep(SHORT * 2);
    let released = em.evict_idle_caches(SHORT);
    assert!(
        released.iter().any(|r| r.contains("ra")),
        "闲置到期应释放用户层槽：{released:?}"
    );
    assert_eq!(em.user_text_loaded(), 0, "释放后不在内存");
    // 释放后第一次查：本次拿不到用户层（后台去建），下一次就有、且内容一致。
    let _ = em.text_codes("ra");
    assert!(wait_until(|| em.user_text_loaded() == 1), "按需重建");
    let after: Vec<String> = em
        .text_codes("ra")
        .codes_of("甲")
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(after, before);
}

/// 按词查编码用户层：闲置期间查过就不释放。
#[test]
fn used_user_text_slot_is_kept() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (em, store) = manager_with_store("ra", "ut_used");
    store.add_user_word("ra", "zzzz", "甲", 0, 0).unwrap();
    assert!(em.prewarm_text_codes("ra"));
    std::thread::sleep(KEEP + Duration::from_millis(300));
    assert!(em.evict_idle_caches(LONG).is_empty());
    let _ = em.text_codes("ra");
    assert!(em.evict_idle_caches(KEEP).is_empty(), "刚查过不释放");
    assert_eq!(em.user_text_loaded(), 1);
}

/// S4 审查 L7：方案引擎被闲置淘汰时，只属于它的按词查编码用户层槽随之释放；
/// 仍在用的方案（当前方案）的槽不动。
#[test]
fn unloading_schema_drops_its_user_text_slot() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (em, _store) = manager_with_store("ra", "ut_l7");
    assert!(em.prewarm_schema("rb"));
    em.prewarm_text_codes("ra");
    em.prewarm_text_codes("rb");
    assert_eq!(em.user_text_loaded(), 2, "前提：两槽都在");
    assert_eq!(em.evict_idle(Duration::ZERO), vec!["rb"]);
    assert_eq!(em.user_text_loaded(), 1, "被摘方案的槽随之释放");
    let r = em.memory_report();
    assert_eq!(
        r.user_text
            .iter()
            .map(|(k, _, _)| k.as_str())
            .collect::<Vec<_>>(),
        vec!["ra"],
        "当前方案的槽不动"
    );
}

/// 审查 LOW：用户层后台重建写回之后要通知上层重刷（否则释放后回来的第一键一直没有用户码，
/// 直到下一次按键）。
#[test]
fn background_user_text_rebuild_fires_built_hook() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (em, store) = manager_with_store("ra", "ut_hook");
    store.add_user_word("ra", "zzzz", "甲", 0, 0).unwrap();
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let fired = fired.clone();
        em.set_user_text_built_hook(std::sync::Arc::new(move || {
            fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
    }
    let _ = em.text_codes("ra"); // 冷启动：本次没有用户层，起后台重建
    assert!(
        wait_until(|| fired.load(std::sync::atomic::Ordering::SeqCst) >= 1),
        "后台重建写回后应调回调"
    );
    let codes: Vec<String> = em
        .text_codes("ra")
        .codes_of("甲")
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(
        codes.contains(&"zzzz".to_string()),
        "回调时新表已在槽里：{codes:?}"
    );
}
