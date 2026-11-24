use super::*;
use std::time::{Duration, SystemTime};

/// 只删过期的 TSF 日志，只认 `wind_tsf.` 前缀；`logs/tsf_log/` 与 `logs/` 两层都要扫。
#[test]
fn prunes_only_stale_tsf_logs() {
    let dir = std::env::temp_dir().join(format!("wind_prune_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sub = dir.join(TSF_LOG_SUBDIR);
    std::fs::create_dir_all(&sub).unwrap();

    let old_ts = SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 30);
    let mk = |base: &Path, name: &str, stale: bool| {
        let p = base.join(name);
        std::fs::write(&p, b"x").unwrap();
        if stale {
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(old_ts).unwrap();
        }
        p
    };

    // 当前落点：logs/tsf_log/
    let stale_tsf = mk(&sub, "wind_tsf.feishu.1234.log", true);
    let stale_old = mk(&sub, "wind_tsf.feishu.1234.old.log", true);
    let fresh_tsf = mk(&sub, "wind_tsf.notepad.5678.log", false);
    // 老版本平铺在 logs/ 的存量
    let legacy_shared = mk(&dir, "wind_tsf.log", true);
    let legacy_fresh = mk(&dir, "wind_tsf.wechat.99.log", false);
    let core_log = mk(&dir, "wind_input.log", true); // core 自己的，即使过期也不该碰
    let other = mk(&sub, "wind_tsf.feishu.1234.txt", true); // 非 .log

    let removed = prune_stale_tsf_logs(&dir, Duration::from_secs(60 * 60 * 24 * 7));

    assert_eq!(removed, 3, "两层加起来应只删三个过期的 TSF 日志");
    assert!(!stale_tsf.exists(), "过期 TSF 日志未删");
    assert!(!stale_old.exists(), "过期轮转产物未删");
    assert!(
        !legacy_shared.exists(),
        "老版本平铺在 logs/ 的存量没被回收——不扫这层就永远没人管它"
    );
    assert!(fresh_tsf.exists(), "活跃宿主的日志被误删——那正是排查现场");
    assert!(legacy_fresh.exists(), "未过期的存量同样不该删");
    assert!(core_log.exists(), "core 自己的日志不归本函数管");
    assert!(other.exists(), "非 .log 文件不该被碰");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 子目录名是 Rust 与 C++ 之间的跨语言契约，没有编译期约束，在这里扫源码钉死。
#[test]
fn subdir_name_matches_cpp_header() {
    const HEADER: &str = include_str!("../../../../wind_tsf/include/FileLogger.h");
    let expect = format!("#define WIND_LOG_SUBDIR_NAME    L\"{TSF_LOG_SUBDIR}\"");
    assert!(
        HEADER.contains(&expect),
        "FileLogger.h 里的 WIND_LOG_SUBDIR_NAME 与 TSF_LOG_SUBDIR 对不上，\
             应含：{expect}"
    );
}
