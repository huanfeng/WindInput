//! UDS / SHM 端点路径（macOS 与 Linux 共用）
//!
//! 与 Go `internal/bridge/endpoint_darwin.go` 及 Swift `ProtocolTypes.swift`
//! 的 `BridgeEndpoints` 对齐。
use std::path::PathBuf;

/// 把**管道后缀**（`""` / `"_dev"`，见 `wind_config::variant::pipe_suffix`）映射成
/// **变体后缀**（`""` / `"Dev"`）。
///
/// 这两种后缀是两套命名风格，混用会让 dev 变体的两端各说各话：
/// 调用方一路传下来的是管道风格的 `_dev`（它给 `wind_input_ctrl_dev` 这类管道名用），
/// 但 Application Support 下的目录名与 SHM 名走的是变体风格 `Dev` ——
/// Swift `BridgeEndpoints.variantSuffix`、`wind_config::variant::app_dir_name()`
/// 和 `scripts/mac/dev.sh` 三处都是 `WindInputDev`。
///
/// 曾经此处直接把 `_dev` 拼进目录名，于是服务把 socket bind 到 `WindInput_dev/`，
/// 而 .app 去 `WindInputDev/` 连——dev 变体的 bridge/SHM 全程握不上手。
/// 改名须三处同步。
///
/// 未知后缀（测试/临时场景的 `_debug` 等）原样透传：它们不是正式变体，
/// 静默改名只会让排查现场更难看懂。
fn variant_suffix(pipe_suffix: &str) -> &str {
    match pipe_suffix {
        "_dev" => "Dev",
        other => other,
    }
}

/// runtime 目录：env 覆盖 → macOS: ~/Library/Application Support/WindInput{变体后缀} / Linux: $XDG_RUNTIME_DIR/WindInput{变体后缀} → /tmp 兜底（见 [`tmp_fallback_dir`]）
///
/// 两段刻意用不同风格的后缀：Application Support 段是**面向用户的应用目录名**
/// （与 .app、dev.sh、用户配置目录同名），/tmp 段是无 HOME 时的兜底，
/// 沿用管道风格的 snake 命名。
pub fn runtime_dir(suffix: &str) -> PathBuf {
    if let Ok(env) = std::env::var("WIND_INPUT_RUNTIME_DIR")
        && !env.is_empty()
    {
        return PathBuf::from(env);
    }
    // Linux（外部宿主形态）：socket 属运行时状态，放 `$XDG_RUNTIME_DIR`（tmpfs、仅本用户可访问、
    // 登出即清）；不放 HOME 下——那里没有 `Library/Application Support`，且 socket 不该落盘。
    // 目录名沿用变体风格（`WindInput` / `WindInputDev`），与 macOS 和 addon 侧一致。
    #[cfg(all(target_os = "linux", ext_presenter))]
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR")
        && !rt.is_empty()
    {
        return PathBuf::from(rt).join(format!("WindInput{}", variant_suffix(suffix)));
    }
    #[cfg(not(all(target_os = "linux", ext_presenter)))]
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        let dir = variant_suffix(suffix);
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join(format!("WindInput{dir}"));
    }
    tmp_fallback_dir(suffix)
}

/// 无会话目录时的 /tmp 兜底。macOS：`/tmp/wind_input{管道后缀}`（不变）。
///
/// Linux 外部宿主：`/tmp/wind_input{管道后缀}-<uid>`，并由 [`ensure_runtime_dir`] 建成 0700、
/// 校验私有。`/tmp` 人人可写：别的用户抢先建这个目录、在里面监听 socket，addon 就会把全部按键
/// （含密码框）发过去，对方再经推送通道注入文本 / 按键。addon 侧同一规则
/// （`Bridge.cpp::fallbackRuntimeDir` / `privateDirProblem`），两条通道另校验对端 uid。
pub fn tmp_fallback_dir(suffix: &str) -> PathBuf {
    #[cfg(all(target_os = "linux", ext_presenter))]
    return PathBuf::from(format!("/tmp/wind_input{suffix}-{}", current_uid()));
    #[cfg(not(all(target_os = "linux", ext_presenter)))]
    PathBuf::from(format!("/tmp/wind_input{suffix}"))
}

/// 建好运行时目录并返回其路径（Linux 外部宿主形态；服务启动、bind socket、建单例锁之前调）。
///
/// - 显式覆盖（`WIND_INPUT_RUNTIME_DIR`）与 `$XDG_RUNTIME_DIR` 下：缺就建（新建的各级 0700，
///   失败照旧不管），不校验——前者是用户自己指的，后者的父目录按 XDG 规范是本用户 0700。
/// - /tmp 兜底：建成 0700，再按 [`check_private_dir`] 校验（是目录、不是符号链接、属主是本用户、
///   组与其他人无权限），不合格返回错误，服务据此拒绝启动——绝不在别人的目录里 bind。
///
/// 其余形态（macOS、不开 linux-host 的 Linux）：同原先的 `create_dir_all`、忽略失败，行为不变。
/// 建不出来时照旧交给后面的 bind / 加锁去报错。
#[cfg(unix)]
pub fn ensure_runtime_dir(suffix: &str) -> std::io::Result<PathBuf> {
    let dir = runtime_dir(suffix);
    #[cfg(all(target_os = "linux", ext_presenter))]
    {
        use std::os::unix::fs::DirBuilderExt;
        if dir == tmp_fallback_dir(suffix) {
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            check_private_dir(&dir, current_uid())?;
            return Ok(dir);
        }
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir);
    }
    #[cfg(not(all(target_os = "linux", ext_presenter)))]
    let _ = std::fs::create_dir_all(&dir);
    Ok(dir)
}

/// 目录是否「本用户私有」：`lstat` 是目录（不跟符号链接）、属主 = `uid`、组与其他人无任何权限。
/// 与 addon `Bridge.cpp::privateDirProblem` 同一规则。
#[cfg(unix)]
pub fn check_private_dir(dir: &std::path::Path, uid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let bad = |why: String| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("运行时目录 {} 不可信：{why}", dir.display()),
        ))
    };
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return bad("是符号链接".into());
    }
    if !meta.is_dir() {
        return bad("不是目录".into());
    }
    if meta.uid() != uid {
        return bad(format!("属主是 uid {}", meta.uid()));
    }
    let mode = meta.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        return bad(format!("权限 {mode:04o}，组或其他人可访问"));
    }
    Ok(())
}

/// 连上来的对端是不是本用户的进程（Linux `SO_PEERCRED`）。取不到身份也算不是。
/// 服务端 accept 后据此拒绝别的用户连进来（按键注入 / 读状态）。
#[cfg(target_os = "linux")]
pub fn peer_is_current_user(stream: &std::os::unix::net::UnixStream) -> bool {
    peer_uid(stream) == Some(unsafe { libc::getuid() })
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: fd 在 stream 存活期内有效；cred / len 指向本栈上大小匹配的缓冲。
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0 && len as usize == std::mem::size_of::<libc::ucred>()).then_some(cred.uid)
}

pub fn request_socket_path(suffix: &str) -> PathBuf {
    runtime_dir(suffix).join("bridge.sock")
}

pub fn push_socket_path(suffix: &str) -> PathBuf {
    runtime_dir(suffix).join("bridge_push.sock")
}

/// POSIX shm 名（须以 '/' 开头，长度 <=31）。
///
/// 后缀与 socket 目录同源（`variant_suffix`），对齐 Swift
/// `CandidatePanelHost` 的 `"/WindInput_SHM\(BridgeEndpoints.variantSuffix)"`；
/// 不一致的话 dev 变体开出的是两段互不相干的共享内存，候选框永远拿不到帧。
///
/// Linux 外部宿主形态带 uid：`/WindInput{变体后缀}.<uid>`（见 [`shm_name_for_uid`]）。
/// macOS 不变（Swift 写死了名字，且 `/dev/shm` 式的全系统共用问题在那边由沙箱规避）。
pub fn shm_name(suffix: &str) -> String {
    #[cfg(all(target_os = "linux", ext_presenter))]
    return shm_name_for_uid(suffix, current_uid());
    #[cfg(not(all(target_os = "linux", ext_presenter)))]
    {
        let suffix = variant_suffix(suffix);
        let name = format!("/WindInput_SHM{suffix}");
        debug_assert!(name.len() <= 31, "shm name too long: {name}");
        name
    }
}

/// Linux 的 SHM 名：`/WindInput{变体后缀}.<uid>`，与 addon `Bridge.cpp::shmNameForUid` 同一规则
/// （两侧单测钉同一组样例）。
///
/// 为什么带 uid：POSIX shm 落在全系统共用的 `/dev/shm`。不带的话同机第二个用户的服务
/// 建不了段（上一个用户的同名段 `shm_unlink` 得 EPERM、`O_EXCL` 得 EEXIST），候选窗 / 气泡 /
/// 菜单全失效；别的用户还能抢先建个同名段喂假帧（addon 另按属主拒收）。
///
/// 长度：`/WindInput`(10) + `Dev`(3) + `.`(1) + uid（u32 至多 10 位）+ 层后缀（至多 `_MN5` 4）
/// = 28 ≤ 31。31 是 macOS 的 PSHMNAMLEN（Linux 实际上限是 NAME_MAX 255），两平台守同一个数。
/// 测试用的未知后缀原样透传（`_debug` 6 字节时最长 31，仍不越界）。
#[cfg(all(target_os = "linux", ext_presenter))]
pub fn shm_name_for_uid(suffix: &str, uid: u32) -> String {
    let name = format!("/WindInput{}.{uid}", variant_suffix(suffix));
    // 同 macOS 分支只断言基名：测试专用的长后缀（`_tm<pid>_<n>`）加层后缀会过 31，但 Linux 的真实
    // 上限是 255；正式变体连层后缀的上界由单测 `linux_shm_name_carries_uid` 钉在 31 以内。
    debug_assert!(name.len() <= 31, "shm name too long: {name}");
    name
}

#[cfg(all(target_os = "linux", ext_presenter))]
fn current_uid() -> u32 {
    // SAFETY: getuid 无参数、总是成功。
    unsafe { libc::getuid() }
}

/// Linux 光栅浮层（`CMD_OVERLAY_FRAME`）各层的 SHM 名：候选窗那段 + 层后缀。一层一段，
/// 候选窗与气泡同时在屏上时互不覆盖。后缀与 Windows host-render（`host_render_windows.rs`
/// 的 `KIND_SUFFIXES`）同形；Toast 那层 Windows 没有。未知层返回 `None`。
pub fn overlay_shm_name(suffix: &str, kind: u32) -> Option<String> {
    Some(format!("{}{}", shm_name(suffix), overlay_tail(kind)?))
}

/// 层后缀：`_TIP` / `_STS` / `_TST` / `_MN<k>` / `_TB` / `_SKB` / `_DEC`。
fn overlay_tail(kind: u32) -> Option<String> {
    use wind_ipc::protocol::overlay::*;
    let tail = match kind {
        OVERLAY_KIND_TOOLTIP => "_TIP".to_string(),
        OVERLAY_KIND_STATUS => "_STS".to_string(),
        OVERLAY_KIND_TOAST => "_TST".to_string(),
        OVERLAY_KIND_TOOLBAR => "_TB".to_string(),
        OVERLAY_KIND_SOFTKEYBOARD => "_SKB".to_string(),
        OVERLAY_KIND_DECOR => "_DEC".to_string(),
        // 菜单每级一段：`_MN0`、`_MN1`…
        k if (OVERLAY_KIND_MENU..OVERLAY_KIND_MENU + OVERLAY_MENU_LEVELS).contains(&k) => {
            format!("_MN{}", k - OVERLAY_KIND_MENU)
        }
        _ => return None,
    };
    Some(tail)
}

#[cfg(test)]
mod tests {
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
}
