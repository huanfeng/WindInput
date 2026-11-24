//! Linux 外部宿主形态（`linux-host`）下的系统能力：剪贴板、打开路径。
//!
//! 与 macOS 的 `pbcopy` / `open` 同一路数——服务进程不链接任何桌面库，全部经外部命令：
//! Wayland 会话用 `wl-clipboard`，X11 会话用 `xclip`（其次 `xsel`），打开走 `xdg-open`。
//! 工具缺失时**如实报错**，不返回假成功（同 `screenshot::copy_bgra_to_clipboard` 的既有约定）。
//!
//! ⚠ 服务进程由 systemd 用户单元拉起时可能没有 `WAYLAND_DISPLAY` / `DISPLAY`，此时选不到
//! 后端；安装单元须 `import-environment`，或由 addon 拉起服务以继承会话环境。
//!
//! 剪贴板命令一律限时（[`TIMEOUT`]）：X11 选区与 Wayland 剪贴板都要持有方应答，持有方挂住时
//! `xclip -o` / `wl-paste` 会一直等，调用线程跟着挂。超时即杀子进程并回收，按失败处理。

use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 剪贴板命令的最长等待。正常情况下几十毫秒内返回。
const TIMEOUT: Duration = Duration::from_secs(2);

/// 子进程退出后，等旁线程把管道读完 / 写完的宽限。子进程自己 fork 出去的后代还占着管道时
/// 读写线程会一直阻塞，到点就不等它了（线程随管道关闭自行结束）。
const PIPE_GRACE: Duration = Duration::from_millis(200);

/// 剪贴板后端。按会话类型择一：有 `WAYLAND_DISPLAY` 优先 wl-clipboard，否则 X11 工具。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    WlClipboard,
    Xclip,
    Xsel,
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn pick_backend() -> Option<Backend> {
    pick_backend_with(env_set, have)
}

/// 选后端的判据本体：环境变量与命令是否存在都由参数给出，测试不必改进程环境。
fn pick_backend_with(env: impl Fn(&str) -> bool, have: impl Fn(&str) -> bool) -> Option<Backend> {
    if env("WAYLAND_DISPLAY") && have("wl-copy") {
        return Some(Backend::WlClipboard);
    }
    if env("DISPLAY") {
        if have("xclip") {
            return Some(Backend::Xclip);
        }
        if have("xsel") {
            return Some(Backend::Xsel);
        }
    }
    None
}

fn no_backend() -> anyhow::Error {
    anyhow::anyhow!(
        "找不到可用的剪贴板工具（Wayland 需 wl-clipboard，X11 需 xclip 或 xsel），或会话环境变量缺失"
    )
}

/// 等子进程退出，最多 `timeout`；到点就杀掉并回收（不留僵尸），报错。
fn wait_timeout(child: &mut Child, name: &str, timeout: Duration) -> anyhow::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{name} 超过 {timeout:?} 未结束，已终止");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn program_name(cmd: &Command) -> String {
    format!("{:?}", cmd.get_program())
}

fn pipe_into(cmd: Command, data: &[u8]) -> anyhow::Result<()> {
    pipe_into_within(cmd, data, TIMEOUT)
}

fn pipe_into_within(mut cmd: Command, data: &[u8], timeout: Duration) -> anyhow::Result<()> {
    let name = program_name(&cmd);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("{name} 启动失败: {e}"))?;
    // 写在旁线程：对端不读时 write_all 会卡在写满的管道上，限时就无从谈起。写完 drop 掉
    // stdin 让对端读到 EOF；xclip / wl-copy 随后自行转入后台持有选区，wait 很快返回。
    let mut stdin = child.stdin.take();
    let data = data.to_vec();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = stdin.as_mut().map_or(Ok(()), |s| s.write_all(&data));
        drop(stdin);
        let _ = tx.send(r);
    });
    let status = wait_timeout(&mut child, &name, timeout)?;
    match rx.recv_timeout(PIPE_GRACE) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => anyhow::bail!("{name} 写入失败: {e}"),
        Err(_) => anyhow::bail!("{name} 已退出，但数据没写完"),
    }
    if !status.success() {
        anyhow::bail!("{name} 退出码非零: {status}");
    }
    Ok(())
}

/// 跑一条命令取 stdout，限时 `timeout`。
fn capture_within(mut cmd: Command, timeout: Duration) -> anyhow::Result<Vec<u8>> {
    let name = program_name(&cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("{name} 启动失败: {e}"))?;
    // 读在旁线程：输出大于管道容量时，不边读边等子进程就会互相卡住。
    let mut stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let r = stdout
            .as_mut()
            .map_or(Ok(0), |s| s.read_to_end(&mut buf))
            .map(|_| buf);
        let _ = tx.send(r);
    });
    let status = wait_timeout(&mut child, &name, timeout)?;
    let out = match rx.recv_timeout(PIPE_GRACE) {
        Ok(r) => r.map_err(|e| anyhow::anyhow!("{name} 读取失败: {e}"))?,
        Err(_) => anyhow::bail!("{name} 已退出，但输出管道仍被占着"),
    };
    if !status.success() {
        anyhow::bail!("{name} 退出码非零: {status}");
    }
    Ok(out)
}

/// 写文本剪贴板。
pub fn set_text(text: &str) -> anyhow::Result<()> {
    match pick_backend().ok_or_else(no_backend)? {
        Backend::WlClipboard => pipe_into(Command::new("wl-copy"), text.as_bytes()),
        Backend::Xclip => {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard"]);
            pipe_into(c, text.as_bytes())
        }
        Backend::Xsel => {
            let mut c = Command::new("xsel");
            c.args(["--clipboard", "--input"]);
            pipe_into(c, text.as_bytes())
        }
    }
}

/// 读文本剪贴板；任何失败（无工具、剪贴板为空、非文本）都按空串处理，与 Windows 侧一致。
pub fn get_text() -> String {
    let Some(backend) = pick_backend() else {
        return String::new();
    };
    let cmd = match backend {
        Backend::WlClipboard => {
            let mut c = Command::new("wl-paste");
            c.args(["--no-newline", "--type", "text"]);
            c
        }
        Backend::Xclip => {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard", "-o"]);
            c
        }
        Backend::Xsel => {
            let mut c = Command::new("xsel");
            c.args(["--clipboard", "--output"]);
            c
        }
    };
    match capture_within(cmd, TIMEOUT) {
        Ok(out) => String::from_utf8_lossy(&out).into_owned(),
        Err(e) => {
            tracing::debug!("读剪贴板失败: {e}");
            String::new()
        }
    }
}

/// 把 PNG 字节写入图片剪贴板。
pub fn set_png(png: &[u8]) -> anyhow::Result<()> {
    match pick_backend().ok_or_else(no_backend)? {
        Backend::WlClipboard => {
            let mut c = Command::new("wl-copy");
            c.args(["--type", "image/png"]);
            pipe_into(c, png)
        }
        Backend::Xclip => {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard", "-t", "image/png"]);
            pipe_into(c, png)
        }
        Backend::Xsel => anyhow::bail!("xsel 不支持图片剪贴板，请安装 xclip 或 wl-clipboard"),
    }
}

/// 按默认应用打开路径 / URL（不等待；后台回收，不留僵尸）。
///
/// 不限时：调用方本就不等它，而 xdg-open 在部分桌面上会前台跑到被打开的程序退出，
/// 到点杀它可能连带杀掉被打开的程序。
pub fn open(target: &str) -> std::io::Result<()> {
    crate::manager::spawn_reaped(
        Command::new("xdg-open")
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .map(|_| ())
}

#[cfg(test)]
#[path = "linux_host_tests.rs"]
mod tests;
