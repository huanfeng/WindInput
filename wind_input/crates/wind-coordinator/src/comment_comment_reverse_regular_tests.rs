//! 候选注释的编码反查只用常规索引（已启用词库）；「含未启用扩展词库」只给反查模式。
//! 自造夹具，不依赖 build_dev/data：主库 `a 工`、未启用扩展 `_xz` 里 `uuia 门头沟区`。
use crate::coordinator::Coordinator;
use std::path::PathBuf;
use wind_config::Config;

struct Cleanup {
    id: String,
    dir: PathBuf,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(cache) = Config::cache_dir() {
            let _ = std::fs::remove_dir_all(cache.join(&self.id));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn coord() -> (std::sync::Arc<Coordinator>, Cleanup) {
    // 同进程里多个用例并行，夹具目录 / 缓存目录必须各用各的，否则一个用例的 Drop 会删掉另一个的。
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let id = format!("zz_crs_on_{}_{n}", std::process::id());
    let dir = std::env::temp_dir().join(format!("wind_crs_{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    let s = dir.join("schemas");
    std::fs::create_dir_all(s.join(&id)).unwrap();
    std::fs::write(
        s.join(format!("{id}.schema.toml")),
        format!(
            "[schema]\nid = \"{id}\"\nname = \"注\"\n[engine]\ntype = \"codetable\"\n\
                 [engine.codetable]\nmax_code_length = 4\n\
                 [[dictionaries]]\nid = \"{id}_m\"\npath = \"{id}/m.dict.yaml\"\ntype = \"rime_codetable\"\ndefault = true\n\
                 [[dictionaries]]\nid = \"{id}_xz\"\npath = \"{id}/xz.dict.yaml\"\ntype = \"rime_codetable\"\ndefault_enabled = false\n"
        ),
    )
    .unwrap();
    let dict = |body: &str| {
        format!(
            "---\nname: d\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n{body}"
        )
    };
    std::fs::write(s.join(format!("{id}/m.dict.yaml")), dict("a\t工\t100\n")).unwrap();
    std::fs::write(
        s.join(format!("{id}/xz.dict.yaml")),
        dict("uuia\t门头沟区\t0\n"),
    )
    .unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec![id.clone()];
    cfg.schema.active = id.clone();
    cfg.input.reverse.lookup_disabled_dicts = true;
    (
        Coordinator::new_headless(cfg, Some(&dir)),
        Cleanup { id, dir },
    )
}

/// 开关开着，预热后注释仍查不到未启用库的码；启用库照常。
#[test]
fn prewarm_indexes_never_builds_comment_variant() {
    let (c, _g) = coord();
    c.prewarm_indexes();
    assert_eq!(
        c.engine_mgr.codetable_reverse_hint("工").as_deref(),
        Some("a")
    );
    assert_eq!(
        c.engine_mgr.codetable_reverse_hint("门头沟区").as_deref(),
        Some(""),
        "未启用库不进注释反查"
    );
}

/// 未启用库的 `*` 码只给剪贴板反查（`include_disabled = true`）；悬停提示等其余入口
/// 走 `eval_text_var`，不带。
#[test]
fn disabled_codes_only_reach_clipboard_reverse() {
    let (c, _g) = coord();
    c.prewarm_indexes();
    let rl = wind_reverse::ReverseLookup::default();
    let val = |name: &str, include: bool| {
        c.eval_text_var_ex(name, None, "门头沟区", &rl, include, 0)
            .map(|v| format!("{v:?}"))
            .unwrap_or_default()
    };
    for name in ["code_rev", "code_rev_all"] {
        assert!(
            val(name, true).contains("*uuia"),
            "{name} 剪贴板反查应带 * 码"
        );
        assert!(!val(name, false).contains("uuia"), "{name} 悬停等入口不带");
    }
    assert!(
        !format!(
            "{:?}",
            c.eval_text_var("code_rev_all", None, "门头沟区", &rl)
        )
        .contains("uuia")
    );
}
