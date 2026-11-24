//! UDS / SHM 端点路径（macOS 与 Linux 共用）
//!
//! 与 Go `internal/bridge/endpoint_darwin.go` 及 Swift `ProtocolTypes.swift`
//! 的 `BridgeEndpoints` 对齐。
use std::path::PathBuf;

/// 把**管道后缀**（`""` / `"_dev"`，见 `wind_config::variant::pipe_suffix`）映射成
/// **变体后缀**（`""` / `"Dev"`）。
///
/// 管道名用 `_dev`，而 Application Support 目录名与 SHM 名用 `Dev`；混用会让 dev 变体
/// 两端连不上。Swift `BridgeEndpoints.variantSuffix`、`wind_config::variant::app_dir_name()`
/// 和 `scripts/mac/dev.sh` 三处都是 `WindInputDev`，改名须同步。
///
/// 未知后缀（测试用的 `_debug` 等）原样透传，不静默改名。
fn variant_suffix(pipe_suffix: &str) -> &str {
    match pipe_suffix {
        "_dev" => "Dev",
        other => other,
    }
}

/// runtime 目录：env 覆盖 → macOS: ~/Library/Application Support/WindInput{变体后缀} / Linux: $XDG_RUNTIME_DIR/WindInput{变体后缀} → /tmp 兜底（见 [`tmp_fallback_dir`]）
///
/// 两段刻意用不同风格的后缀：Application Support 段是面向用户的应用目录名，
/// /tmp 兜底段沿用管道风格的 snake 命名。
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
/// 其余形态（macOS、不开 linux-host 的 Linux）：`create_dir_all` 并忽略失败，建不出来时
/// 交给后面的 bind / 加锁去报错。
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
/// 后缀与 socket 目录同源（`variant_suffix`），须与 Swift `CandidatePanelHost` 的
/// `"/WindInput_SHM\(BridgeEndpoints.variantSuffix)"` 一致，否则候选框拿不到帧。
///
/// Linux 外部宿主形态带 uid（见 [`shm_name_for_uid`]）；macOS 不带（Swift 写死了名字，
/// 全系统共用问题由沙箱规避）。
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
/// 带 uid：POSIX shm 落在全系统共用的 `/dev/shm`，不带的话同机第二个用户的服务建不了段，
/// 别的用户还能抢先建同名段喂假帧（addon 另按属主拒收）。
///
/// 长度上限守 31（macOS PSHMNAMLEN）：最长形态 `/WindInputDev.<10 位 uid>_MN5` 为 28，
/// 测试用的 `_debug` 后缀时也恰好 31。
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
#[path = "endpoint_tests.rs"]
mod tests;
