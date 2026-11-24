use super::*;

/// 每个用例独占一个目录（按用例名 + pid），保证并行跑互不干扰。
fn cache_in(case: &str) -> (TempDir, PathBuf) {
    let dir = std::env::temp_dir().join(format!("wind_build_guard_{case}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("a.wdat");
    (TempDir(dir), cache)
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn proceed(g: Gate) -> BuildAttempt {
    match g {
        Gate::Proceed(a) => a,
        Gate::Skip { reason } => panic!("不该跳过: {reason}"),
    }
}

/// 模拟一次「进程死在构建中」：登记后不 finish，再把持有者改成一个已死的进程。
fn die_in_build(cache: &Path, key: &str) {
    drop(proceed(begin(cache, key)));
    let marker = marker_path(cache);
    let m = read_marker(&marker).expect("begin 应已写入标记");
    std::fs::write(
        &marker,
        format!("{}\n{}\n{} 1\n", m.key, m.count, dead_pid()),
    )
    .unwrap();
}

/// 一个肯定已退出的进程的 pid：起一个立即结束的子进程并回收。
fn dead_pid() -> u32 {
    let mut c = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
        .args(if cfg!(windows) {
            &["/C", "exit"][..]
        } else {
            &[][..]
        })
        .spawn()
        .unwrap();
    let pid = c.id();
    c.wait().unwrap();
    pid
}

/// 连续死在构建里 MAX_ATTEMPTS 次后，同一份输入不再尝试。
#[test]
fn skips_after_repeated_deaths_with_same_input() {
    let (_d, cache) = cache_in("skips");
    for _ in 0..MAX_ATTEMPTS {
        die_in_build(&cache, "k1");
    }
    match begin(&cache, "k1") {
        Gate::Skip { reason } => {
            assert!(reason.contains("内存不足"), "原因要可读: {reason}");
            assert!(
                reason.contains(".building"),
                "要告诉用户怎么手动重试: {reason}"
            );
        }
        Gate::Proceed(_) => panic!("同一输入已连续失败，应跳过"),
    }
}

/// 源文件变了（输入键不同）就重新尝试。
#[test]
fn retries_when_input_changes() {
    let (_d, cache) = cache_in("input_changes");
    for _ in 0..MAX_ATTEMPTS {
        die_in_build(&cache, "k1");
    }
    proceed(begin(&cache, "k2")).finish();
}

/// 成功的构建不留记录，之后任意次都放行。
#[test]
fn finished_build_leaves_no_record() {
    let (_d, cache) = cache_in("finished");
    for _ in 0..MAX_ATTEMPTS + 2 {
        proceed(begin(&cache, "k1")).finish();
    }
    assert!(!marker_path(&cache).exists());
}

/// 删掉标记文件即手动重试。
#[test]
fn deleting_marker_forces_retry() {
    let (_d, cache) = cache_in("delete");
    for _ in 0..MAX_ATTEMPTS {
        die_in_build(&cache, "k1");
    }
    std::fs::remove_file(marker_path(&cache)).unwrap();
    proceed(begin(&cache, "k1")).finish();
}

/// ★ 回归（审查 H1）：持有者还活着的登记是「别人在建」，不是死亡。
/// 同 key 两个活着的 begin 都不 finish，第三次仍须放行。
#[test]
fn live_concurrent_builds_are_not_counted_as_deaths() {
    let (_d, cache) = cache_in("live");
    let a = proceed(begin(&cache, "k1"));
    let b = proceed(begin(&cache, "k1"));
    let c = proceed(begin(&cache, "k1"));
    drop((a, b));
    c.finish();
}

/// 持有者已死一次还不够，第二次死才跳过。
#[test]
fn skips_only_after_holder_died_max_times() {
    let (_d, cache) = cache_in("dead_twice");
    die_in_build(&cache, "k1");
    drop(proceed(begin(&cache, "k1"))); // 第一次死后仍放行
    let marker = marker_path(&cache);
    let m = read_marker(&marker).unwrap();
    std::fs::write(
        &marker,
        format!("{}\n{}\n{} 1\n", m.key, m.count, dead_pid()),
    )
    .unwrap();
    assert!(matches!(begin(&cache, "k1"), Gate::Skip { .. }));
}

/// 旧格式标记（无持有者行）按已死处理，与改动前语义一致。
#[test]
fn legacy_marker_without_holder_counts_as_dead() {
    let (_d, cache) = cache_in("legacy");
    std::fs::write(marker_path(&cache), format!("k1\n{MAX_ATTEMPTS}\n")).unwrap();
    assert!(matches!(begin(&cache, "k1"), Gate::Skip { .. }));
}

/// 写端判不了启动时间（未知平台）时记 `pid 0`：读端须当作活着、不计数，
/// 否则这些平台上并发的在建构建会被当成死亡（审查 H1 复现）。
#[test]
fn holder_with_unknown_start_counts_as_alive() {
    let (_d, cache) = cache_in("unknown_start");
    std::fs::write(
        marker_path(&cache),
        format!("k1\n{MAX_ATTEMPTS}\n{} 0\n", dead_pid()),
    )
    .unwrap();
    proceed(begin(&cache, "k1")).finish();
}

#[test]
fn current_process_is_alive_and_dead_child_is_not() {
    let me = Holder::current();
    assert_ne!(me.start, 0, "Linux / Windows / macOS 应能取到自身启动时间");
    assert!(me.is_alive());
    assert!(
        !Holder {
            pid: dead_pid(),
            start: 1
        }
        .is_alive()
    );
    assert!(
        !Holder {
            start: me.start ^ 1,
            ..me
        }
        .is_alive(),
        "pid 复用要识破"
    );
}
