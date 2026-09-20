//! 便携版的**本机状态目录**与**用户数据目录**分家：`localdata/` vs `userdata/`（论坛 t120）。
//!
//! 两者曾经收敛到同一个 `userdata/`，于是词库缓存（上百 MB）和日志都长在用户数据里——
//! 楼主的原话是「这两个都是程序自动生成的，跟配置又无关」，备份 `userdata` 时全被拖走。
//!
//! 这条断言的是**分家本身**，而不是某个子目录的拼法：`cache_dir()` / `log_dir()` 都从
//! `local_dir()` 派生，真正会退化的是「便携下 local 又回到 userdata」这一步。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：`is_portable()` 是 OnceLock，一个进程只判一次
//! （同姊妹测试 `portable_no_seen_marker`）。

use wind_config::Config;

#[test]
fn portable_local_dir_is_localdata_not_userdata() {
    let tmp = std::env::temp_dir().join("wind_portable_localdata_split_e2e");
    let root = tmp.join("PortableApp");
    let user = root.join("userdata");
    let local = root.join("localdata");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&user).unwrap();
    // 便携标记必须在任何 OnceLock 初始化之前就位。
    std::fs::write(root.join("portable_mode"), "portable=1\n").unwrap();

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }

    // 前置：确实处于便携形态。少了这一条，下面几条可能只是因为根本没判成便携
    // ——那样测的是环境没搭起来，不是本次改动。
    assert_eq!(
        Config::user_config_dir().as_deref(),
        Some(user.as_path()),
        "前置：用户数据仍须落在便携 userdata 上（本次不动它）"
    );

    assert_eq!(
        Config::local_dir().as_deref(),
        Some(local.as_path()),
        "便携下本机状态目录须是 localdata"
    );
    assert_eq!(
        Config::cache_dir().as_deref(),
        Some(local.join("cache").as_path()),
        "缓存跟随 local_dir"
    );
    assert_eq!(
        Config::log_dir().as_deref(),
        Some(local.join("logs").as_path()),
        "日志跟随 local_dir（TSF DLL 那份在 C++ 里，靠两边注释互指）"
    );

    // 分家本身：这条才是楼主要的结果——备份 userdata 不会带走可重建产物。
    assert_ne!(
        Config::local_dir(),
        Config::user_config_dir(),
        "便携下两个目录不能再收敛到一起"
    );
    // 且 localdata 与 userdata **同级**，不是套在它下面（深度不变）。
    assert_eq!(
        Config::local_dir().as_deref().and_then(|d| d.parent()),
        Some(root.as_path()),
        "localdata 须与 userdata 同级"
    );

    // 收尾：固定名目录，不清的话下次跑会带着上次的残留。
    let _ = std::fs::remove_dir_all(&tmp);
}
