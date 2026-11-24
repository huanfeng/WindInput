//! `.wscc` 单字全码表缓存的接线层验证：落盘 → 复用。
//!
//! `charcodes` 与 `manager` 的单元测试证明了格式与「cap 进指纹」判据，但复用分支走没走到，
//! 只有让真的 `EngineManager` 跑一遍才知道。可观测量取 [`EngineManager::encode_word`]：它是
//! 这张表唯一的生产消费点（自动造词按 `[[encoder.rules]]` 组码时逐字取全码）。
//!
//! 与 `reverse_index_cache.rs` 是同一方案目录下的两份产物（`.wridx` / `.wscc`），互不重写。
//! 「词库集合变了要失效」不在这里重复：两表共用 `reverse_index_source_digests`，由那边覆盖。

use std::path::{Path, PathBuf};

use wind_config::Config;
use wind_engine::EngineManager;

const SCHEMA: &str = "wubi86";

/// 本文件内的测试共用同一份磁盘缓存文件，必须串行（同 `reverse_index_cache.rs` 的理由）。
static CACHE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serialized() -> std::sync::MutexGuard<'static, ()> {
    CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    p.join("schemas")
        .join(format!("{SCHEMA}.schema.toml"))
        .exists()
        .then_some(p)
}

fn mgr(dir: &Path) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec![SCHEMA.to_string()];
    cfg.schema.active = SCHEMA.to_string();
    EngineManager::new(&cfg, Some(dir))
}

/// 在缓存根里找本方案的 `.wscc`。刻意不复制路径推导逻辑，照抄一份会悄悄过时。
fn find_wscc() -> Option<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p
                .file_name()
                .is_some_and(|n| n == format!("{SCHEMA}.wscc").as_str())
            {
                out.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(&Config::cache_dir()?, &mut found);
    found.pop()
}

fn fp_of(wscc: &Path) -> PathBuf {
    let mut s = wscc.as_os_str().to_os_string();
    s.push(".fp");
    PathBuf::from(s)
}

/// 一次构建的可观测快照：几个真实单字各自取到的全码。
///
/// 单字取码直接查这张表（`encode_word` 的 `codes.get(&c)` 分支），多字词先逐字取码再按
/// `[[encoder.rules]]` 组码；两条都带上，复用回来的表少了条目时前者变 `Err`、后者整词失败。
fn snapshot(m: &EngineManager) -> Vec<Result<String, String>> {
    ["中", "工", "好", "你好", "中国人"]
        .iter()
        .map(|w| m.encode_word(SCHEMA, w).map_err(|e| e.to_string()))
        .collect()
}

/// 落盘 → 复用：第二次构建不该重写文件，且取码结果必须完全一致。
///
/// 文件字节与 mtime 不变是复用分支唯一可靠的外部证据（重建分支恒会 rename 覆盖）；只断言
/// 结果一样对「复用没生效」不敏感，而那会让每次启动都全量扫词库，自动造词的就绪闸开机后
/// 头几秒一直关着。
#[test]
fn single_char_codes_are_persisted_then_reused_without_rewriting() {
    let _guard = serialized();
    let Some(dir) = data_dir() else {
        eprintln!(
            "!!! 跳过 single_char_codes_cache：build_dev/data 不存在，本测试**没有真正运行**"
        );
        return;
    };

    // ① 首次：可能复用上一轮跑测试留下的文件，故先强制一次真重建。
    if let Some(p) = find_wscc() {
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(fp_of(&p));
    }
    let m1 = mgr(&dir);
    // 返回值只说明本次预热做了事，区分不了重建与复用；判据是下面的 mtime 比对。
    assert!(
        m1.prewarm_single_char_codes(SCHEMA),
        "新 manager 上预热应执行"
    );
    assert!(m1.single_char_codes_ready(SCHEMA), "预热之后必须就绪");
    let s1 = snapshot(&m1);
    assert!(
        s1.iter().any(|r| r.is_ok()),
        "真实词库上不该一个词都取不到码：{s1:?}"
    );
    drop(m1);

    let wscc = find_wscc().expect("构建后必须落盘出 .wscc");
    assert!(
        fp_of(&wscc).exists(),
        "必须同时写出指纹 sidecar，否则下次仍会重建"
    );
    let bytes1 = std::fs::read(&wscc).expect("读 .wscc");
    let mtime1 = std::fs::metadata(&wscc)
        .and_then(|m| m.modified())
        .expect("取 mtime");

    // ② 再来一个全新的 manager：必须走复用分支。
    let m2 = mgr(&dir);
    assert!(m2.prewarm_single_char_codes(SCHEMA));
    assert_eq!(snapshot(&m2), s1, "复用得到的表必须与首次构建取码逐字一致");
    assert_eq!(
        std::fs::read(&wscc).unwrap(),
        bytes1,
        "复用路径不该重写文件"
    );
    assert_eq!(
        std::fs::metadata(&wscc).unwrap().modified().unwrap(),
        mtime1,
        "文件被重写过 ⇒ 走的是重建分支，复用没生效"
    );
}

/// 重建 vs 复用的基准（`--ignored --nocapture` 手动跑，不进 CI）。收益的构成见
/// docs/design/codetable-auto-phrase.md 单字全码表一节。
#[test]
#[ignore]
fn measure_build_vs_reuse() {
    let _guard = serialized();
    let dir = data_dir().expect("需要 build_dev/data");
    if let Some(p) = find_wscc() {
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(fp_of(&p));
    }
    let m1 = mgr(&dir);
    let t0 = std::time::Instant::now();
    m1.prewarm_single_char_codes(SCHEMA);
    let build = t0.elapsed();
    drop(m1);
    let sz = find_wscc()
        .map(|p| std::fs::metadata(&p).unwrap().len())
        .unwrap_or(0);
    let m2 = mgr(&dir);
    let t1 = std::time::Instant::now();
    m2.prewarm_single_char_codes(SCHEMA);
    let reuse = t1.elapsed();
    eprintln!("MEASURE {SCHEMA}: build={build:?} reuse={reuse:?} wscc={sz} B");
}
