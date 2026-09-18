//! 便携模式不再生成 `userdata/user_config.seen`（论坛 t150）。
//!
//! 这个标记是给**安装版**用的：非便携下用户配置落在漫游 `%APPDATA%`，开机早期漫游
//! profile 可能还没挂载完，`probe_user_config()` 看不到 `config.toml` 时靠它区分
//! 「新用户（永不等）」与「老用户但漫游迟到（要继续等）」——否则那一帧会按「没有用户配置」
//! 启动，用户看到的就是设置全丢（`wait_user_config_ready` 超时日志的原话是
//! "user settings will be ignored"）。
//!
//! 便携版完全不在这条链上：用户目录就在 exe 边上，`probe_user_config()` 第一个分支即
//! 返回 `Portable`，标记从来没人读。可 `local_dir()` 在便携下指向 `userdata/`，写端照写
//! 不误，于是用户的 `userdata\` 里凭空多出一个内容只有 `1`、删了也没影响的文件。
//!
//! ⚠️ 全文件仅此一个 `#[test]`：`is_portable()` 是 OnceLock，一个进程只判一次。

use std::path::Path;
use wind_config::Config;

#[test]
fn portable_does_not_write_user_config_seen() {
    let tmp = std::env::temp_dir().join("wind_portable_seen_marker_e2e");
    let root = tmp.join("PortableApp");
    let user = root.join("userdata");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&user).unwrap();
    // 便携标记必须在任何 OnceLock 初始化之前就位。
    std::fs::write(root.join("portable_mode"), "portable=1\n").unwrap();
    // 「这个用户定制过设置」——写端只有看到 config.toml 才会落标记，没有它测的就是另一条路。
    std::fs::write(user.join("config.toml"), "# user\n").unwrap();

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }

    // 前置：确实处于便携形态。少了这一条，下面的「文件不存在」可能只是因为
    // 用户目录压根没解析出来 —— 那样测的是环境没搭起来，不是本次改动。
    assert_eq!(
        Config::user_config_dir().as_deref(),
        Some(user.as_path()),
        "前置：用户目录须落在便携 userdata 上"
    );

    Config::mark_user_config_seen_if_present();

    let marker = user.join("user_config.seen");
    assert!(
        !Path::new(&marker).exists(),
        "便携模式不该生成这个标记（它在便携下永远没人读），实际生成在 {}",
        marker.display()
    );
    assert!(
        !Config::user_config_seen(),
        "读端在便携模式下也该恒为 false"
    );
    // 顺带钉住「没往别处写」：整棵便携树里不该出现同名文件。
    let stray: Vec<_> = walk(&root)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "user_config.seen"))
        .collect();
    assert!(
        stray.is_empty(),
        "便携树里不该有任何 user_config.seen：{stray:?}"
    );
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
