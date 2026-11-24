use super::*;

/// 无 `WAYLAND_DISPLAY` / `DISPLAY` 时工具再全也不选（避免在无头环境里误起进程）；
/// 有会话时按 wl-clipboard → xclip → xsel 的顺序取第一个装了的。
#[test]
fn backend_follows_session_env_then_tool_order() {
    let pick = |env: &[&str], tools: &[&str]| {
        pick_backend_with(|n| env.contains(&n), |c| tools.contains(&c))
    };
    let all = ["wl-copy", "xclip", "xsel"];
    assert_eq!(pick(&[], &all), None);
    assert_eq!(
        pick(&["WAYLAND_DISPLAY", "DISPLAY"], &all),
        Some(Backend::WlClipboard)
    );
    // Wayland 会话缺 wl-clipboard：经 XWayland 的 DISPLAY 退到 X11 工具。
    assert_eq!(
        pick(&["WAYLAND_DISPLAY", "DISPLAY"], &["xclip", "xsel"]),
        Some(Backend::Xclip)
    );
    assert_eq!(pick(&["WAYLAND_DISPLAY"], &["xclip", "xsel"]), None);
    assert_eq!(pick(&["DISPLAY"], &["xsel"]), Some(Backend::Xsel));
    assert_eq!(pick(&["DISPLAY"], &["wl-copy"]), None);
}

/// 本测试独占的临时目录（进程号 + 用例名，并发用例不撞）。
fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_lh_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 假剪贴板工具：不读 stdin、不写 stdout，起一个后代 `sleep` 一直占着继承来的管道——
/// 模拟持有方挂住时的 `xclip` / `wl-paste`。故意不 `exec`：杀掉的是 sh，后代还占着管道，
/// 正好覆盖「子进程死了、管道没关」那条路。
fn hanging_tool(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\nsleep 5\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// 本进程名下处于 Z（僵尸）态、进程名为 `comm` 的子进程数。
fn zombies_named(comm: &str) -> usize {
    let me = std::process::id().to_string();
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
        .filter(|stat| {
            // 「pid (comm) state ppid …」；comm 可含空格，按最后一个 ')' 切。
            let Some((head, tail)) = stat.rsplit_once(')') else {
                return false;
            };
            let name = head.split_once('(').map_or("", |(_, n)| n);
            let mut f = tail.split_whitespace();
            let (state, ppid) = (f.next(), f.next());
            name == comm && state == Some("Z") && ppid == Some(me.as_str())
        })
        .count()
}

#[test]
fn hanging_reader_times_out_and_is_reaped() {
    let dir = scratch("read");
    let tool = hanging_tool(&dir, "wifakepaste");
    let t = Instant::now();
    let r = capture_within(Command::new(&tool), Duration::from_millis(300));
    assert!(r.is_err(), "挂住的读取应当报错");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert_eq!(zombies_named("wifakepaste"), 0, "超时杀掉的子进程要回收");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 工具自己退出了，但留下的后代还占着 stdout（xclip / wl-copy 会 fork 到后台）：
/// 读线程等不到 EOF，过了宽限就放弃，不陪着它挂。
#[test]
fn descendant_holding_stdout_does_not_hang_reader() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("detach");
    let p = dir.join("wifakedetach");
    std::fs::write(&p, "#!/bin/sh\nsleep 5 &\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let t = Instant::now();
    assert!(capture_within(Command::new(&p), TIMEOUT).is_err());
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hanging_writer_times_out_and_is_reaped() {
    let dir = scratch("write");
    let tool = hanging_tool(&dir, "wifakecopy");
    // 远大于管道容量（64 KiB）：对端不读时 write_all 必然卡住，验证写不阻塞限时。
    let data = vec![b'x'; 1 << 20];
    let t = Instant::now();
    let r = pipe_into_within(Command::new(&tool), &data, Duration::from_millis(300));
    assert!(r.is_err(), "挂住的写入应当报错");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert_eq!(zombies_named("wifakecopy"), 0, "超时杀掉的子进程要回收");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 正常的工具照常工作：读到 stdout、写进去的字节对端读得到。
#[test]
fn well_behaved_tools_round_trip() {
    let dir = scratch("ok");
    let out = dir.join("got");
    let mut c = Command::new("sh");
    c.args(["-c", &format!("cat > '{}'", out.display())]);
    pipe_into_within(c, "清风".as_bytes(), TIMEOUT).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "清风");
    let mut c = Command::new("cat");
    c.arg(&out);
    assert_eq!(capture_within(c, TIMEOUT).unwrap(), "清风".as_bytes());
    let mut c = Command::new("sh");
    c.args(["-c", "exit 3"]);
    assert!(capture_within(c, TIMEOUT).is_err(), "退出码非零按失败");
    let _ = std::fs::remove_dir_all(&dir);
}
