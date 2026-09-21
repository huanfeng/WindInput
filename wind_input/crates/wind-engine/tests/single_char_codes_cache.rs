//! `.wscc` 单字全码表缓存的**接线层**验证：落盘 → 复用。
//!
//! # 为什么单元测试不够
//!
//! `charcodes` 的单元测试证明了格式本身（往返、截断、异体版本），`manager` 的两条单测
//! 证明了路径形态与「cap 进指纹」这个判据。三者都对，接线仍可能是错的——而接线恰恰是
//! 最容易出问题的一段：复用分支到底走没走到，只有让**真的 `EngineManager` 跑一遍**才知道。
//! 本仓踩过的正是这一类：护栏全钉在纯函数上，绕开了消费点。
//!
//! 可观测量取 [`EngineManager::encode_word`] 而不是那张表本身：它是这张表**唯一的**生产
//! 消费点（自动造词按 `[[encoder.rules]]` 组码时逐字取全码），拿它当判据，
//! 测的就是用户实际会碰到的那条路。
//!
//! # 与 `reverse_index_cache.rs` 的关系
//!
//! 两者是同一个方案目录下的**两份不同产物**（`.wridx` / `.wscc`），谁也不重写谁的文件。
//! 「词库集合变了要失效」那条不在这里重复验证：两张表共用
//! `reverse_index_source_digests` 取摘要，那条已由那边的 invalidation 测试覆盖，
//! 这里只验本产物独有的接线。

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

/// 在缓存根里找本方案的 `.wscc`。刻意**不复制一遍路径推导逻辑**——照抄一份就等于
/// 「两处各写一份、其中一处悄悄过时」，那正是本测试要防的事。
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
/// 单字取码直接查这张表（`encode_word` 的 `codes.get(&c)` 分支），多字词则要先逐字取码
/// 再按 `[[encoder.rules]]` 组码——两条都带上，复用回来的表若少了条目，前者变 `Err`、
/// 后者也跟着整词失败。
fn snapshot(m: &EngineManager) -> Vec<Result<String, String>> {
    ["中", "工", "好", "你好", "中国人"]
        .iter()
        .map(|w| m.encode_word(SCHEMA, w).map_err(|e| e.to_string()))
        .collect()
}

/// ★ 落盘 → 复用：第二次构建**不该重写文件**，且取码结果必须完全一致。
///
/// 「文件字节与 mtime 都没变」是复用分支唯一可靠的外部证据——重建分支恒会 rename 覆盖。
/// 只断言「结果一样」是不够的：重建一遍结果当然也一样，那样测试对「复用根本没生效」
/// 这个真正的故障完全不敏感，而它的后果正是本次改动要消灭的东西——每次启动都要重新
/// 加载整组词库再全量扫一遍，自动造词的就绪闸因此在开机后头几秒一直关着。
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
    // 注意：返回值只说明「本次预热做了事」，**区分不了重建与复用**——
    // 真正的判据是下面的 mtime 比对，别把这条当成「确实重建了」的证据。
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

/// 重建 vs 复用的实测基准（`--ignored --nocapture` 手动跑，不进 CI）。
///
/// # 2026-09-21 wubi86 实测（Linux、文件缓存热、test profile）
///
/// `build=18.8ms  reuse=2.9ms  wscc=185 KB`
///
/// 拆开看：复用那 2.9 ms 里绝大部分是 `load_dicts_individually`——**命中缓存也照样要
/// 先加载整组词库**，因为缓存判据（各 `.wdat` 的摘要）本身就要从已加载的词库取。
/// 省掉的是那 ~16 ms 的全表 `for_each_entry` 扫描。
///
/// ⇒ **收益随词条数线性放大**：扫描是 O(词条数) 而加载不是。wubi86 这种十万词级的方案
/// 只省十几毫秒，feihuzj2 那种 251 万词的（反查索引 95.4 MB、构建秒级）才是这项改动
/// 真正的受益者。**那个量级本测试没有实测**，手上没有该词库。
///
/// ★ 顺带记一个本次未动的浪费：`.wridx` 与 `.wscc` 两条构建路径**各自**调一次
/// `load_dicts_individually`，即便双双命中缓存，同一组词库仍会被加载两遍。
/// 合并它要动反查索引那条路径，不在本次改动范围内。
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
