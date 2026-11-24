//! `.wridx` 反查索引缓存的接线层验证：落盘 → 复用 → 失效。
//!
//! `reverseidx` 与 `cache_fp` 的单元测试分别证明了格式与指纹判据，但缓存路径怎么推、摘要从
//! 哪几个文件取、复用分支走没走到，只有让真的 `EngineManager` 跑一遍才能回答。
//!
//! 测试进程没有 `WIND_VARIANT`，`Config::cache_dir()` 落在 `%LOCALAPPDATA%\WindInput\cache`，
//! 与 Dev 变体（`WindInputDev\cache`）天然隔离，不会动到真实缓存。

use std::path::{Path, PathBuf};

use wind_config::Config;
use wind_engine::EngineManager;

const SCHEMA: &str = "wubi86";

/// 三个测试共用同一份磁盘缓存文件，必须串行，否则互相删改文件随机失败。
/// 中毒后取 `into_inner`：一个测试 panic 不该把其余两个变成看不懂的 poison 错误。
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

/// 在缓存根里找本方案的 `.wridx`。刻意不复制路径推导逻辑，照抄一份会悄悄过时。
fn find_wridx() -> Option<PathBuf> {
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
                .is_some_and(|n| n == format!("{SCHEMA}.wridx").as_str())
            {
                out.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(&Config::cache_dir()?, &mut found);
    found.pop()
}

fn fp_of(wridx: &Path) -> PathBuf {
    let mut s = wridx.as_os_str().to_os_string();
    s.push(".fp");
    PathBuf::from(s)
}

/// 一次构建的可观测快照：词数 + 一个真实词的编码 + 是否常驻。
fn snapshot(m: &EngineManager) -> (usize, Option<String>, bool, usize) {
    let idx = m
        .reverse_index_if_ready(SCHEMA)
        .expect("预热之后反查索引必须就绪");
    (
        idx.len(),
        idx.codes_of("中").map(|c| c.join("/")),
        idx.is_resident(),
        idx.data_bytes(),
    )
}

/// 落盘 → 复用：第二次构建不该重写文件，且结果必须完全一致。
///
/// 文件字节与 mtime 不变是复用分支唯一可靠的外部证据（重建分支恒会 rename 覆盖）；
/// 只断言结果一样对「复用没生效」不敏感，而那会让每次启动多一次全量构建。
#[test]
fn reverse_index_is_persisted_then_reused_without_rewriting() {
    let _guard = serialized();
    let Some(dir) = data_dir() else {
        eprintln!("!!! 跳过 reverse_index_cache：build_dev/data 不存在，本测试**没有真正运行**");
        return;
    };

    // ① 首次：可能复用上一轮跑测试留下的文件，故先强制一次真重建。
    let stale = find_wridx();
    if let Some(p) = &stale {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(fp_of(p));
        // 连同构建标记：别的测试进程若在构建中途被杀，残留标记会让本方案被退避跳过。
        let mut b = p.as_os_str().to_os_string();
        b.push(".building");
        let _ = std::fs::remove_file(b);
    }
    let m1 = mgr(&dir);
    // 返回值只说明本次预热做了事，区分不了重建与复用；判据是下面的 mtime 比对。
    assert!(m1.prewarm_reverse_index(SCHEMA), "新 manager 上预热应执行");
    let s1 = snapshot(&m1);
    assert!(s1.0 > 0, "真实词库上索引不该为空");
    drop(m1);

    let wridx = find_wridx().expect("构建后必须落盘出 .wridx");
    assert!(
        fp_of(&wridx).exists(),
        "必须同时写出指纹 sidecar，否则下次仍会重建"
    );
    let bytes1 = std::fs::read(&wridx).expect("读 .wridx");
    let mtime1 = std::fs::metadata(&wridx)
        .and_then(|m| m.modified())
        .expect("取 mtime");

    // ② 再来一个全新的 manager：必须走复用分支。
    let m2 = mgr(&dir);
    assert!(m2.prewarm_reverse_index(SCHEMA));
    let s2 = snapshot(&m2);
    assert_eq!(s1, s2, "复用得到的索引必须与首次构建完全一致");
    assert_eq!(
        std::fs::read(&wridx).unwrap(),
        bytes1,
        "复用路径不该重写文件"
    );
    assert_eq!(
        std::fs::metadata(&wridx).unwrap().modified().unwrap(),
        mtime1,
        "文件被重写过 ⇒ 走的是重建分支，复用没生效"
    );
}

/// 常驻与否必须只由体积决定，且两条路给出同样的查询结果。
/// 不写死「wubi86 应该常驻」，那会随词库更新失效；断言的是不变式本身。
#[test]
fn residency_follows_size_threshold_and_does_not_change_results() {
    let _guard = serialized();
    let Some(dir) = data_dir() else {
        eprintln!("!!! 跳过 residency：build_dev/data 不存在，本测试**没有真正运行**");
        return;
    };
    let m = mgr(&dir);
    m.prewarm_reverse_index(SCHEMA);
    let (_, codes, resident, bytes) = snapshot(&m);
    assert_eq!(
        resident,
        bytes <= wind_engine::manager::REVERSE_INDEX_RESIDENT_MAX,
        "{bytes} 字节的索引，常驻判定与阈值不一致"
    );

    // 同一份文件强制按另一种方式打开，查询结果必须逐字相同。
    let wridx = find_wridx().expect(".wridx 应已落盘");
    let other = if resident { 0 } else { usize::MAX };
    let alt = wind_dict::reverseidx::ReverseIndex::open(&wridx, other).expect("另一种方式应能打开");
    assert_ne!(alt.is_resident(), resident, "应确实换了一种打开方式");
    assert_eq!(
        alt.codes_of("中").map(|c| c.join("/")),
        codes,
        "常驻与 mmap 必须给出相同的反查结果"
    );
}

/// 启用的词库集合变了 → 缓存必须失效并重建；改回去 → 必须重新命中。
///
/// 靠 `schema_overrides` 打开一个出厂 `default_enabled = false` 的扩展库制造变化，
/// 即用户在设置页勾选扩展词库时的情形。
///
/// 不能用篡改 `.wdat.fp` 模拟：那份词库会先被判失效，`CachedDict::load_at_with` 从 yaml
/// 重建并把 `.fp` 写回正确值，`reverse_index_source_digests` 读到的摘要一字未变。
///
/// 反向那条同样要紧：只断言会失效，恒返回 `false` 的 `derived_cache_is_fresh` 也能过，
/// 而那意味着每次启动全量重建。
#[test]
fn changing_the_enabled_dict_set_invalidates_and_reverting_hits_again() {
    let _guard = serialized();
    let Some(dir) = data_dir() else {
        eprintln!("!!! 跳过 invalidation：build_dev/data 不存在，本测试**没有真正运行**");
        return;
    };
    let overrides = std::env::temp_dir().join(format!("wind_wridx_ovr_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&overrides);
    std::fs::create_dir_all(&overrides).unwrap();

    let build = |ovr: Option<&Path>| -> (String, usize) {
        let mut cfg = Config::default();
        cfg.schema.available = vec![SCHEMA.to_string()];
        cfg.schema.active = SCHEMA.to_string();
        let m = EngineManager::with_store_override(
            &cfg,
            Some(&dir),
            None,
            ovr.map(|p| p.to_path_buf()),
        );
        m.prewarm_reverse_index(SCHEMA);
        let n = m
            .reverse_index_if_ready(SCHEMA)
            .expect("预热后应就绪")
            .len();
        drop(m);
        let wridx = find_wridx().expect(".wridx 应已落盘");
        (std::fs::read_to_string(fp_of(&wridx)).expect("索引指纹"), n)
    };

    let (fp_base, n_base) = build(None);

    // 打开出厂默认关闭的「行政区域」扩展库
    std::fs::write(
        overrides.join(format!("{SCHEMA}.toml")),
        "[[dictionaries]]\nid = \"wubi86_xzqy\"\nenabled = true\n",
    )
    .unwrap();
    let (fp_more, n_more) = build(Some(&overrides));
    assert_ne!(
        fp_more, fp_base,
        "启用了新词库，索引缓存必须失效并重建（否则新词库对反查静默不生效）"
    );
    assert!(n_more > n_base, "多挂一个词库应多收词：{n_base} → {n_more}");

    // 关回去 → 指纹应回到原值，说明它确实是「当前启用的那组词库」的函数
    let (fp_back, n_back) = build(None);
    assert_eq!(fp_back, fp_base, "恢复原状后指纹应回到原值");
    assert_eq!(n_back, n_base);

    let _ = std::fs::remove_dir_all(&overrides);
}
