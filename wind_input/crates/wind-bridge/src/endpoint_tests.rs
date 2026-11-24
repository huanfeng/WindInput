use super::*;
use std::sync::Mutex;

// 串行化所有动 env 的测试（env 是进程全局，默认并行会互扰）
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 保存并在 Drop 时恢复指定 env 变量（panic-safe）
struct EnvRestore {
    keys: Vec<(&'static str, Option<std::ffi::OsString>)>,
}
impl EnvRestore {
    fn capture(keys: &[&'static str]) -> Self {
        Self {
            keys: keys.iter().map(|k| (*k, std::env::var_os(k))).collect(),
        }
    }
}
impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (k, v) in &self.keys {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

#[test]
fn runtime_dir_honors_env_override() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestore::capture(&["WIND_INPUT_RUNTIME_DIR", "HOME"]);
    unsafe { std::env::set_var("WIND_INPUT_RUNTIME_DIR", "/tmp/wind_test_rt") };
    assert_eq!(
        runtime_dir(""),
        std::path::PathBuf::from("/tmp/wind_test_rt")
    );
    assert_eq!(
        request_socket_path(""),
        std::path::PathBuf::from("/tmp/wind_test_rt/bridge.sock")
    );
    assert_eq!(
        push_socket_path(""),
        std::path::PathBuf::from("/tmp/wind_test_rt/bridge_push.sock")
    );
}

/// Linux 外部宿主形态：socket 在 `$XDG_RUNTIME_DIR/WindInput[Dev]`，且绝不落到 HOME 下。
#[cfg(all(target_os = "linux", ext_presenter))]
#[test]
fn linux_runtime_dir_uses_xdg_runtime_dir() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestore::capture(&["WIND_INPUT_RUNTIME_DIR", "XDG_RUNTIME_DIR", "HOME"]);
    unsafe { std::env::remove_var("WIND_INPUT_RUNTIME_DIR") };
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
    unsafe { std::env::set_var("HOME", "/home/tester") };
    assert_eq!(
        runtime_dir(""),
        std::path::PathBuf::from("/run/user/1000/WindInput")
    );
    assert_eq!(
        request_socket_path("_dev"),
        std::path::PathBuf::from("/run/user/1000/WindInputDev/bridge.sock")
    );
    // 没有 XDG_RUNTIME_DIR 时退到 /tmp（带 uid，各用户各一个），而不是 HOME 下的 macOS 式路径。
    unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    let uid = unsafe { libc::getuid() };
    assert_eq!(
        runtime_dir("_dev"),
        std::path::PathBuf::from(format!("/tmp/wind_input_dev-{uid}"))
    );
}

/// 私有目录校验：0700 的本用户目录通过；放开权限、符号链接、普通文件、别人的（换个期望 uid
/// 模拟）一律拒绝。
#[cfg(unix)]
#[test]
fn check_private_dir_rejects_unsafe_dirs() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let root = std::env::temp_dir().join(format!("wind_priv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let uid = unsafe { libc::getuid() };
    let ok = root.join("ok");
    std::fs::DirBuilder::new().mode(0o700).create(&ok).unwrap();
    assert!(check_private_dir(&ok, uid).is_ok());
    assert!(
        check_private_dir(&ok, uid + 1).is_err(),
        "属主不是期望的 uid"
    );
    let loose = root.join("loose");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&loose)
        .unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(check_private_dir(&loose, uid).is_err());
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o701)).unwrap();
    assert!(check_private_dir(&loose, uid).is_err());
    let link = root.join("link");
    std::os::unix::fs::symlink(&ok, &link).unwrap();
    assert!(check_private_dir(&link, uid).is_err(), "符号链接不跟");
    let file = root.join("file");
    std::fs::write(&file, b"x").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(check_private_dir(&file, uid).is_err());
    assert!(check_private_dir(&root.join("missing"), uid).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

/// 兜底目录：服务建成 0700；被放开权限后拒绝（不在别人可写的目录里 bind）。
#[cfg(all(target_os = "linux", ext_presenter))]
#[test]
fn ensure_runtime_dir_creates_private_tmp_fallback() {
    use std::os::unix::fs::PermissionsExt;
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestore::capture(&["WIND_INPUT_RUNTIME_DIR", "XDG_RUNTIME_DIR"]);
    unsafe { std::env::remove_var("WIND_INPUT_RUNTIME_DIR") };
    unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    // 测试专用后缀：不碰本机真服务可能在用的兜底目录。
    let suffix = format!("_t{}", std::process::id());
    let dir = tmp_fallback_dir(&suffix);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(ensure_runtime_dir(&suffix).unwrap(), dir);
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = ensure_runtime_dir(&suffix).unwrap_err();
    assert!(err.to_string().contains("不可信"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 对端 uid：socketpair 两端都是本进程。
#[cfg(target_os = "linux")]
#[test]
fn peer_uid_of_socketpair_is_us() {
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(peer_is_current_user(&a));
}

// 以下三条断言 macOS 式的 HOME 路径，Linux 外部宿主形态不适用。
// 这一条同时钉住 macOS 的 /tmp 兜底一字未变（不带 uid）。
#[cfg(not(all(target_os = "linux", ext_presenter)))]
#[test]
fn runtime_dir_tmp_fallback_with_suffix() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestore::capture(&["WIND_INPUT_RUNTIME_DIR", "HOME"]);
    unsafe { std::env::remove_var("WIND_INPUT_RUNTIME_DIR") };
    unsafe { std::env::remove_var("HOME") };
    assert_eq!(
        runtime_dir("_debug"),
        std::path::PathBuf::from("/tmp/wind_input_debug")
    );
    assert_eq!(
        request_socket_path("_debug"),
        std::path::PathBuf::from("/tmp/wind_input_debug/bridge.sock")
    );
    assert_eq!(
        push_socket_path("_debug"),
        std::path::PathBuf::from("/tmp/wind_input_debug/bridge_push.sock")
    );
}

/// macOS（及不开 linux-host 的 Linux）：名字一字不变，Swift 那边写死的是同一个串。
#[cfg(not(all(target_os = "linux", ext_presenter)))]
#[test]
fn shm_name_has_leading_slash_and_suffix() {
    assert_eq!(shm_name(""), "/WindInput_SHM");
    assert_eq!(shm_name("_debug"), "/WindInput_SHM_debug");
}

/// Linux 外部宿主：带 uid。样例与 addon `bridge_test`（`shmNameForUid`）逐字相同，
/// 那边读本文件核对这几个字面量——改规则两侧一起改。
#[cfg(all(target_os = "linux", ext_presenter))]
#[test]
fn linux_shm_name_carries_uid() {
    use wind_ipc::protocol::overlay::*;
    assert_eq!(shm_name_for_uid("", 1000), "/WindInput.1000");
    assert_eq!(
        format!(
            "{}{}",
            shm_name_for_uid("", 1000),
            overlay_tail(OVERLAY_KIND_STATUS).unwrap()
        ),
        "/WindInput.1000_STS"
    );
    let longest = format!(
        "{}{}",
        shm_name_for_uid("_dev", u32::MAX),
        overlay_tail(OVERLAY_KIND_MENU + OVERLAY_MENU_LEVELS - 1).unwrap()
    );
    assert_eq!(longest, "/WindInputDev.4294967295_MN5");
    assert!(longest.len() <= 31);
    assert_eq!(
        format!(
            "{}{}",
            shm_name_for_uid("_dev", u32::MAX),
            overlay_tail(OVERLAY_KIND_SOFTKEYBOARD).unwrap()
        ),
        "/WindInputDev.4294967295_SKB"
    );
    assert_eq!(
        format!(
            "{}{}",
            shm_name_for_uid("", 1000),
            overlay_tail(OVERLAY_KIND_TOOLBAR).unwrap()
        ),
        "/WindInput.1000_TB"
    );
    let uid = unsafe { libc::getuid() };
    assert_eq!(shm_name("_dev"), format!("/WindInputDev.{uid}"));
    assert_eq!(
        overlay_shm_name("", OVERLAY_KIND_TOAST).as_deref(),
        Some(format!("/WindInput.{uid}_TST").as_str())
    );
}

/// dev 变体的目录名必须是 `WindInputDev`（变体风格），不是把管道后缀 `_dev`
/// 直接拼上去的 `WindInput_dev`。三处对齐：Swift `BridgeEndpoints.runtimeDir`、
/// `wind_config::variant::app_dir_name()`、`scripts/mac/dev.sh` 的 `APP_SUPPORT`。
///
/// 这条曾经是真实故障：服务 bind 在 `WindInput_dev/bridge.sock`，
/// .app 连 `WindInputDev/bridge.sock`，dev 变体的 IPC 从来没通过。
#[cfg(not(all(target_os = "linux", ext_presenter)))]
#[test]
fn dev_runtime_dir_uses_app_dir_style_suffix() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestore::capture(&["WIND_INPUT_RUNTIME_DIR", "HOME"]);
    unsafe { std::env::remove_var("WIND_INPUT_RUNTIME_DIR") };
    unsafe { std::env::set_var("HOME", "/Users/tester") };

    assert_eq!(
        runtime_dir("_dev"),
        std::path::PathBuf::from("/Users/tester/Library/Application Support/WindInputDev")
    );
    assert_eq!(
        request_socket_path("_dev"),
        std::path::PathBuf::from(
            "/Users/tester/Library/Application Support/WindInputDev/bridge.sock"
        )
    );
    assert_eq!(
        push_socket_path("_dev"),
        std::path::PathBuf::from(
            "/Users/tester/Library/Application Support/WindInputDev/bridge_push.sock"
        )
    );
}

/// 正式变体（空后缀）不受映射影响 —— 已装机用户的 socket 路径不能因这次对齐而变。
#[cfg(not(all(target_os = "linux", ext_presenter)))]
#[test]
fn release_runtime_dir_unchanged() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestore::capture(&["WIND_INPUT_RUNTIME_DIR", "HOME"]);
    unsafe { std::env::remove_var("WIND_INPUT_RUNTIME_DIR") };
    unsafe { std::env::set_var("HOME", "/Users/tester") };

    assert_eq!(
        runtime_dir(""),
        std::path::PathBuf::from("/Users/tester/Library/Application Support/WindInput")
    );
}

/// SHM 名与 socket 目录同源：dev 走变体风格，未知后缀原样透传。
#[cfg(not(all(target_os = "linux", ext_presenter)))]
#[test]
fn overlay_shm_name_appends_kind_suffix() {
    use wind_ipc::protocol::overlay::*;
    assert_eq!(
        overlay_shm_name("_dev", OVERLAY_KIND_STATUS).as_deref(),
        Some("/WindInput_SHMDev_STS")
    );
    assert_eq!(
        overlay_shm_name("", OVERLAY_KIND_TOOLTIP).as_deref(),
        Some("/WindInput_SHM_TIP")
    );
    assert_eq!(
        overlay_shm_name("", OVERLAY_KIND_TOAST).as_deref(),
        Some("/WindInput_SHM_TST")
    );
    assert_eq!(overlay_shm_name("", 0), None, "候选窗那段不归本函数");
    assert_eq!(
        overlay_shm_name("_dev", OVERLAY_KIND_MENU + 2).as_deref(),
        Some("/WindInput_SHMDev_MN2")
    );
    assert_eq!(
        overlay_shm_name("", OVERLAY_KIND_MENU + OVERLAY_MENU_LEVELS).as_deref(),
        Some("/WindInput_SHM_TB"),
        "菜单级数上限之外紧接着是工具栏那层，不会出 _MN6"
    );
    assert_eq!(
        overlay_shm_name("_dev", OVERLAY_KIND_TOOLBAR).as_deref(),
        Some("/WindInput_SHMDev_TB")
    );
    assert_eq!(
        overlay_shm_name("_dev", OVERLAY_KIND_SOFTKEYBOARD).as_deref(),
        Some("/WindInput_SHMDev_SKB")
    );
    assert_eq!(
        overlay_shm_name("_dev", OVERLAY_KIND_DECOR).as_deref(),
        Some("/WindInput_SHMDev_DEC")
    );
}

#[cfg(not(all(target_os = "linux", ext_presenter)))]
#[test]
fn shm_name_maps_dev_suffix_like_socket_dir() {
    assert_eq!(shm_name("_dev"), "/WindInput_SHMDev");
    // POSIX shm 名上限 31 字节，映射后不得越界。
    assert!(shm_name("_dev").len() <= 31);
}
