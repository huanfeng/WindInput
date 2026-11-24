use super::*;
use std::io::Write;

/// 造一个已含内容的日志文件，模拟「上一次运行留下的日志」。
fn seed(path: &Path, content: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

fn make_rotate(path: &Path, max_files: usize) -> FileRotate<AppendCountBeforeExt> {
    FileRotate::new(
        path,
        AppendCountBeforeExt::new(max_files),
        ContentLimit::Bytes(10 * 1024 * 1024),
        Compression::None,
        None,
    )
}

/// 对表间隔为零的自愈写端，测试里每次写入都会对一次表。
fn healing(path: &Path, max_files: usize, max_bytes: u64) -> SelfHealingLog {
    let mut log = SelfHealingLog::new(path, max_files, max_bytes);
    log.check_interval = Duration::ZERO;
    log
}

/// 清理删掉历史后照常轮转。裸 `FileRotate` 在这里会 `assert!(old_path.exists())`
/// 失败——core release 是 `panic = "abort"`，等于整个服务崩掉。
#[test]
fn rotation_survives_history_deleted_underneath() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    let mut log = healing(&path, 10, 100);
    for _ in 0..4 {
        log.write_all(&[b'a'; 60]).unwrap();
    }
    assert!(dir.path().join("wind_input.3.log").exists());

    for e in std::fs::read_dir(dir.path()).unwrap() {
        std::fs::remove_file(e.unwrap().path()).unwrap();
    }
    for _ in 0..30 {
        log.write_all(&[b'b'; 60]).unwrap();
    }
    assert!(path.exists());
    assert!(dir.path().join("wind_input.2.log").exists());
}

/// 当前那份被删：按原路径重开，之后的日志写进看得见的新文件，而不是幽灵 inode。
#[test]
fn deleted_current_file_is_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    let mut log = healing(&path, 10, 1024);
    log.write_all(b"before\n").unwrap();

    std::fs::remove_file(&path).unwrap();
    log.write_all(b"after\n").unwrap();
    log.flush().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
}

/// 整个日志目录被删也能接回来（目录一并建回）。
#[test]
fn deleted_log_dir_is_recreated() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("logs");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("wind_input.1.log");
    let mut log = healing(&path, 10, 1024);
    log.write_all(b"before\n").unwrap();

    std::fs::remove_dir_all(&dir).unwrap();
    log.write_all(b"after\n").unwrap();
    log.flush().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
}

/// 被清空后以真实大小重新计数：不会因为旧的累计值提前轮转出一个几乎空的历史份。
#[test]
fn truncation_resets_rotation_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    let mut log = healing(&path, 10, 100);
    log.write_all(&[b'a'; 90]).unwrap();

    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(0)
        .unwrap();
    log.write_all(&[b'b'; 50]).unwrap();
    assert!(!dir.path().join("wind_input.2.log").exists());
}

/// 一条日志不劈成两半：超限时整条进下一份。
#[test]
fn a_line_is_never_split_across_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    let mut log = healing(&path, 10, 100);
    log.write_all(&[b'a'; 80]).unwrap();
    log.write_all(&[b'b'; 40]).unwrap();
    log.flush().unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("wind_input.2.log")).unwrap(),
        vec![b'a'; 80]
    );
    assert_eq!(std::fs::read(&path).unwrap(), vec![b'b'; 40]);
}

/// 主文件路径 = `wind_input.1.log`。内部序号 n 落到磁盘 n+1，且末尾那个 `.1`
/// 必须被当成序号占位剥掉——否则会写出 `wind_input.1.4.log`。
#[test]
fn to_path_puts_index_before_extension() {
    let base = Path::new("/logs/wind_input.1.log");
    assert_eq!(
        LogIndex(3).to_path(base),
        PathBuf::from("/logs/wind_input.4.log")
    );
}

/// 当前日志的文件名写死：它出现在用户手里和文档里，改名必须是显式决定，
/// 不能是重构 [`CURRENT_INDEX`] 的副作用。
#[test]
fn current_log_file_name_is_index_one() {
    assert_eq!(
        format!("wind_input.{CURRENT_INDEX}.log"),
        "wind_input.1.log"
    );
}

/// 磁盘序号 ↔ 内部序号的换算只有这一处，两个方向都钉住。
/// `.1` 是当前文件而非历史，`from_disk` 必须拒绝它——认成历史的话，启动扫描会把
/// 正在写的这份也算进淘汰队列。
#[test]
fn disk_index_maps_to_internal_index() {
    assert_eq!(LogIndex(1).disk(), 2);
    assert_eq!(LogIndex(9).disk(), 10);
    assert_eq!(LogIndex::from_disk(2), Some(LogIndex(1)));
    assert_eq!(LogIndex::from_disk(10), Some(LogIndex(9)));
    assert_eq!(LogIndex::from_disk(CURRENT_INDEX), None, "当前文件不是历史");
    assert_eq!(LogIndex::from_disk(0), None);
}

/// 首次启动：没有旧日志，不应报错也不应凭空造出 `.1.log`。
/// （`rotate()` 内部是 `fs::rename`，对不存在的文件会 Err，故必须跳过。）
#[test]
fn first_start_does_not_rotate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");

    let mut r = make_rotate(&path, 10);
    rotate_on_startup(&mut r, &path);

    assert!(!dir.path().join("wind_input.2.log").exists());
}

/// 空日志文件不该白占一个序号，否则会把有用的历史更快挤出保留窗口。
#[test]
fn empty_log_does_not_rotate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    seed(&path, "");

    let mut r = make_rotate(&path, 10);
    rotate_on_startup(&mut r, &path);

    assert!(!dir.path().join("wind_input.2.log").exists());
}

/// 二次启动：上一次运行的内容整体搬到 `.2.log`，`.1.log` 让给本次运行。
#[test]
fn second_start_moves_previous_run_to_index_2() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    seed(&path, "run-1\n");

    let mut r = make_rotate(&path, 10);
    rotate_on_startup(&mut r, &path);

    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
        "run-1\n"
    );
    // `.1` 已重开且为空，本次运行从零写起
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    // 前两代命名一个都不该冒出来；`.1.2.log` 是「忘了剥主文件序号」的症状
    assert!(!dir.path().join("wind_input.log").exists());
    assert!(!dir.path().join("wind_input.log.1").exists());
    assert!(!dir.path().join("wind_input.1.2.log").exists());
}

/// 连续多次启动：序号依次后移，最老的一次被淘汰。
///
/// 同时钉住：级联重命名认得回上次写的文件（`scan_suffixes`），`max_files` 不含主文件。
#[test]
fn old_runs_are_evicted_beyond_max_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");

    for i in 1..=4 {
        seed(&path, &format!("run-{i}\n"));
        let mut r = make_rotate(&path, 2);
        rotate_on_startup(&mut r, &path);
    }

    // 最近两次运行（run-3 / run-4）保留，更早的被删
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
        "run-4\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.3.log")).unwrap(),
        "run-3\n"
    );
    assert!(!dir.path().join("wind_input.4.log").exists());
}

/// 二代布局（`wind_input.log` 是当前 + `.N.log` 是历史）整体后移一格，
/// 当前那份落到 `.1`。不搬的话无序号的最新日志会被扫描无视，`.1`（上一次运行）冒充最新。
#[test]
fn second_generation_layout_shifts_by_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    seed(&dir.path().join("wind_input.log"), "current\n");
    seed(&dir.path().join("wind_input.1.log"), "prev-1\n");
    seed(&dir.path().join("wind_input.2.log"), "prev-2\n");

    migrate_legacy_suffix(&path);

    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.1.log")).unwrap(),
        "current\n",
        "无序号的那份是最新的，必须落到 .1"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
        "prev-1\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.3.log")).unwrap(),
        "prev-2\n"
    );
    assert!(!dir.path().join("wind_input.log").exists());
}

/// 迁移必须幂等：已是新布局时一个文件都不能动。判据必须是「无序号的 wind_input.log
/// 在不在」；写成「有没有 .N.log」会每次启动把序号推一格，静默挤光历史。
#[test]
fn migration_is_idempotent_on_new_layout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    seed(&path, "current\n");
    seed(&dir.path().join("wind_input.2.log"), "prev\n");

    for _ in 0..3 {
        migrate_legacy_suffix(&path);
    }

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "current\n");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
        "prev\n"
    );
    assert!(
        !dir.path().join("wind_input.3.log").exists(),
        "序号被推移了"
    );
}

/// 一代命名（`wind_input.log.N`）迁到现在的 `.{N+1}.log`：N 是「第 N 老的历史」，
/// 而 `.1` 现在归当前那次运行，故整体让一格。
#[test]
fn first_generation_files_are_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    seed(&dir.path().join("wind_input.log.1"), "old-1\n");
    seed(&dir.path().join("wind_input.log.2"), "old-2\n");

    migrate_legacy_suffix(&path);

    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
        "old-1\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.3.log")).unwrap(),
        "old-2\n"
    );
    assert!(!dir.path().join("wind_input.log.1").exists());
}

/// 迁移不得误伤非序号后缀（`.log.bak` 之类），也不得覆盖已存在的目标。
#[test]
fn migration_skips_non_numeric_and_existing_targets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    seed(&dir.path().join("wind_input.log.bak"), "backup\n");
    seed(&dir.path().join("wind_input.log.1"), "legacy\n");
    seed(&dir.path().join("wind_input.2.log"), "already-new\n");

    migrate_legacy_suffix(&path);

    // 非数字后缀原样保留
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.log.bak")).unwrap(),
        "backup\n"
    );
    // 目标已存在 → 不覆盖，老文件留在原地
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
        "already-new\n"
    );
    assert!(dir.path().join("wind_input.log.1").exists());
}

/// 迁移后的文件必须能被 `scan_suffixes` 认回，否则会永不淘汰地堆积。
/// 这是「只改 to_path 不改 scan_suffixes」那个坑的回归测试。
#[test]
fn migrated_files_participate_in_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wind_input.1.log");
    seed(&dir.path().join("wind_input.log"), "current\n");
    seed(&dir.path().join("wind_input.log.1"), "old-1\n");

    migrate_legacy_suffix(&path);

    // 迁移后：.1 = current（二代那份最新）、.2 = old-1（一代那份历史）
    let mut r = make_rotate(&path, 10);
    rotate_on_startup(&mut r, &path);

    // 级联能推动 `.2` 说明 scan_suffixes 认得回迁移来的文件，否则会被原地覆盖。
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.2.log")).unwrap(),
        "current\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("wind_input.3.log")).unwrap(),
        "old-1\n"
    );
}
