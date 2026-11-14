//! 宿主端到端：本测试扮演核心，拉起真实的 `wind_input_ext`，扩展是 `examples/fake_extension`。
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::Value;
use wind_hook::frame::{Frame, read_frame, write_frame};
use wind_hook::msg::{CommitEvent, ExtensionGrant, ExtensionStatus, KeyEvent, ModeEvent, Msg};

fn fake_extension_exe() -> PathBuf {
    let host = Path::new(env!("CARGO_BIN_EXE_wind_input_ext"));
    host.parent().unwrap().join("examples/fake_extension")
}

struct Core {
    _tmp: tempfile::TempDir,
    extensions: PathBuf,
    child: Child,
    stdin: Option<ChildStdin>,
    status: Receiver<Vec<ExtensionStatus>>,
}

impl Core {
    fn start() -> Core {
        let tmp = tempfile::tempdir().unwrap();
        let extensions = tmp.path().join("extensions");
        std::fs::create_dir_all(&extensions).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_wind_input_ext"))
            .arg("--extensions-dir")
            .arg(&extensions)
            .arg("--log-dir")
            .arg(tmp.path().join("logs"))
            .arg("--core-version")
            .arg("test")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, status) = channel();
        std::thread::spawn(move || {
            while let Ok(Some(f)) = read_frame(&mut stdout) {
                if let Msg::Status { extensions } = f.msg {
                    let _ = tx.send(extensions);
                }
            }
        });
        Core {
            stdin: child.stdin.take(),
            _tmp: tmp,
            extensions,
            child,
            status,
        }
    }

    /// 装一个扩展：把假扩展拷进扩展目录，清单里写上参数。
    fn install(&self, id: &str, args: &[&str]) {
        self.install_with(id, args, "");
    }

    /// 同 [`Self::install`]，清单末尾追加 `extra`（如选项声明）。
    fn install_with(&self, id: &str, args: &[&str], extra: &str) {
        let dir = self.extensions.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fake_extension_exe(), dir.join("fake")).unwrap();
        let args: Vec<String> = args.iter().map(|a| format!("{a:?}")).collect();
        std::fs::write(
            dir.join("extension.toml"),
            format!("exe = \"fake\"\nargs = [{}]\n{extra}", args.join(", ")),
        )
        .unwrap();
    }

    fn send(&mut self, msg: Msg) {
        write_frame(self.stdin.as_mut().unwrap(), &Frame::new(msg)).unwrap();
    }

    fn config(&mut self, extensions: &[(&str, &[&str])]) {
        let extensions = extensions
            .iter()
            .map(|(id, g)| ExtensionGrant {
                id: id.to_string(),
                granted: g.iter().map(|s| s.to_string()).collect(),
                options: Default::default(),
            })
            .collect();
        self.send(Msg::Config { extensions });
    }

    /// 只配一个扩展，带上用户设的选项。
    fn config_options(&mut self, id: &str, granted: &[&str], options: Value) {
        self.send(Msg::Config {
            extensions: vec![ExtensionGrant {
                id: id.into(),
                granted: granted.iter().map(|s| s.to_string()).collect(),
                options: options.as_object().unwrap().clone(),
            }],
        });
    }

    /// 等到某个扩展的状态满足条件。
    fn wait_for(
        &self,
        id: &str,
        timeout: Duration,
        pred: impl Fn(&ExtensionStatus) -> bool,
    ) -> ExtensionStatus {
        let deadline = Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if let Ok(list) = self.status.recv_timeout(left)
                && let Some(s) = list.into_iter().find(|s| s.id == id)
                && pred(&s)
            {
                return s;
            }
        }
        panic!("等不到扩展 {id} 的目标状态");
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn key(name: &str) -> Msg {
    Msg::Key(KeyEvent {
        ts: 1,
        sid: 0,
        app: String::new(),
        key: Some(name.into()),
        mods: vec!["shift".into()],
        class: "letter".into(),
        handled: true,
        observed: false,
    })
}

/// 读假扩展写下的记录，直到满足条件或超时。
fn read_lines(path: &Path, timeout: Duration, done: impl Fn(&[String]) -> bool) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        // 只认以换行结尾的整行：假扩展边写边读，末尾可能是写了一半的行，解析 JSON 会偶发 panic。
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let complete = text.rfind('\n').map_or("", |i| &text[..i]);
        let lines: Vec<String> = complete.lines().map(String::from).collect();
        if done(&lines) || Instant::now() > deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn msgs(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["msg"].clone())
        .collect()
}

fn pid_of(lines: &[String]) -> u32 {
    lines
        .iter()
        .find_map(|l| l.strip_prefix("pid "))
        .unwrap()
        .parse()
        .unwrap()
}

/// 进程还在不在。`/proc` 只有 Linux 有，macOS 上永远判「不在」，故统一用 `kill -0`。
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn events_are_trimmed_to_granted_and_wanted_tiers() {
    let mut core = Core::start();
    let out = core._tmp.path().join("echo.log");
    let out_s = out.to_str().unwrap();
    // 扩展申请 key.detail + commit + mode；配置只授予 key.class + commit。
    core.install("echo", &["echo", out_s, "key.detail", "commit", "mode"]);
    core.config(&[("echo", &["key.class", "commit"])]);
    core.wait_for("echo", Duration::from_secs(5), |s| s.state == "running");
    // 等握手完成（Welcome 写进记录）再发事件：握手前的事件按设计丢弃。
    read_lines(&out, Duration::from_secs(5), |l| {
        msgs(l).iter().any(|m| m["t"] == "welcome")
    });

    core.send(key("a"));
    core.send(Msg::Mode(ModeEvent {
        ts: 2,
        chinese: true,
        full_width: false,
        schema: String::new(),
    }));
    core.send(Msg::Commit(CommitEvent {
        ts: 3,
        sid: 0,
        app: String::new(),
        text: "你好".into(),
        source: "candidate".into(),
        rect: None,
    }));

    let lines = read_lines(&out, Duration::from_secs(5), |l| {
        msgs(l).iter().any(|m| m["t"] == "commit")
    });
    let got = msgs(&lines);
    let kinds: Vec<&str> = got.iter().map(|m| m["t"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["welcome", "key", "commit"], "mode 未授权不应送达");
    assert_eq!(
        got[0]["granted"],
        serde_json::json!(["key.class", "commit"])
    );
    assert!(
        got[1].get("key").is_none() && got[1].get("mods").is_none(),
        "只有 key.class 时抹掉具体键"
    );
    assert_eq!(got[2]["text"], "你好");
}

#[test]
fn crashing_extension_is_restarted_then_tripped() {
    let mut core = Core::start();
    core.install("bad", &["crash"]);
    core.config(&[("bad", &["mode"])]);
    let s = core.wait_for("bad", Duration::from_secs(10), |s| s.state == "tripped");
    assert_eq!(s.restarts, 2, "退避重启两次后第三次熔断");
    assert!(s.error.is_some());
}

#[test]
fn missing_extension_reports_invalid() {
    let mut core = Core::start();
    core.config(&[("ghost", &["mode"])]);
    let s = core.wait_for("ghost", Duration::from_secs(5), |s| s.state == "invalid");
    assert!(s.error.unwrap().contains("extension.toml"));
}

#[test]
fn disabling_extension_stops_it() {
    let mut core = Core::start();
    let out = core._tmp.path().join("echo.log");
    core.install("echo", &["echo", out.to_str().unwrap(), "mode"]);
    core.config(&[("echo", &["mode"])]);
    core.wait_for("echo", Duration::from_secs(5), |s| s.state == "running");
    let pid = pid_of(&read_lines(&out, Duration::from_secs(5), |l| !l.is_empty()));
    assert!(alive(pid));
    core.config(&[]);
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(pid), "停用后扩展进程应退出");
}

#[test]
fn closing_core_pipe_takes_extensions_down() {
    let mut core = Core::start();
    let out = core._tmp.path().join("echo.log");
    core.install("echo", &["echo", out.to_str().unwrap(), "mode"]);
    core.config(&[("echo", &["mode"])]);
    core.wait_for("echo", Duration::from_secs(5), |s| s.state == "running");
    let pid = pid_of(&read_lines(&out, Duration::from_secs(5), |l| !l.is_empty()));

    drop(core.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        if matches!(core.child.try_wait(), Ok(Some(_))) {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(exited, "核心关管道后宿主应退出");
    assert!(!alive(pid), "宿主退出时应收走扩展");
}

#[test]
fn late_extension_receives_current_mode_after_handshake() {
    let mut core = Core::start();
    let out = core._tmp.path().join("echo.log");
    core.install("echo", &["echo", out.to_str().unwrap(), "mode"]);
    // 模式事件先于扩展启动到达：扩展握手后应补收这一份，而不是等下一次切换。
    core.config(&[("echo", &["mode"])]);
    core.send(Msg::Mode(ModeEvent {
        ts: 1,
        chinese: false,
        full_width: true,
        schema: "wubi86".into(),
    }));
    let lines = read_lines(&out, Duration::from_secs(5), |l| {
        msgs(l).iter().any(|m| m["t"] == "mode")
    });
    let got = msgs(&lines);
    let kinds: Vec<&str> = got.iter().map(|m| m["t"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["welcome", "mode"]);
    assert_eq!(got[1]["full_width"], true);
}

#[test]
fn invalid_extension_is_retried_on_next_config() {
    let mut core = Core::start();
    core.config(&[("late", &["mode"])]);
    core.wait_for("late", Duration::from_secs(5), |s| s.state == "invalid");
    // 用户随后装好了扩展、设置端再下发一次配置：应重新读清单并拉起。
    let out = core._tmp.path().join("late.log");
    core.install("late", &["echo", out.to_str().unwrap(), "mode"]);
    core.config(&[("late", &["mode"])]);
    core.wait_for("late", Duration::from_secs(5), |s| s.state == "running");
}

const VOLUME_OPTION: &str = r#"
[[option]]
key = "volume"
type = "number"
default = 0.6
min = 0.0
max = 1.0

[[option]]
key = "theme"
type = "choice"
default = "click"
choices = [{ value = "click" }, { value = "piano" }]
"#;

#[test]
fn options_are_validated_sent_after_handshake_and_updated_without_restart() {
    let mut core = Core::start();
    let out = core._tmp.path().join("echo.log");
    core.install_with(
        "echo",
        &["echo", out.to_str().unwrap(), "mode"],
        VOLUME_OPTION,
    );
    // 超范围的值夹住、不认识的键丢掉、没设的补默认值。
    core.config_options(
        "echo",
        &["mode"],
        serde_json::json!({"volume": 3.0, "bogus": 1}),
    );
    let lines = read_lines(&out, Duration::from_secs(5), |l| {
        msgs(l).iter().any(|m| m["t"] == "options")
    });
    let got = msgs(&lines);
    let kinds: Vec<&str> = got.iter().map(|m| m["t"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["welcome", "options"]);
    assert_eq!(
        got[1]["values"],
        serde_json::json!({"volume": 1.0, "theme": "click"})
    );
    let pid = pid_of(&lines);

    // 只改选项：扩展收到新值，进程不换。
    core.config_options("echo", &["mode"], serde_json::json!({"theme": "piano"}));
    let lines = read_lines(&out, Duration::from_secs(5), |l| {
        msgs(l).iter().filter(|m| m["t"] == "options").count() >= 2
    });
    let opts: Vec<Value> = msgs(&lines)
        .into_iter()
        .filter(|m| m["t"] == "options")
        .collect();
    assert_eq!(
        opts[1]["values"],
        serde_json::json!({"volume": 0.6, "theme": "piano"})
    );
    assert_eq!(pid_of(&lines), pid);
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("pid ")).count(),
        1,
        "没有重启"
    );

    // 值没变就不重发。
    core.config_options("echo", &["mode"], serde_json::json!({"theme": "piano"}));
    core.send(Msg::Mode(ModeEvent {
        ts: 9,
        chinese: true,
        full_width: false,
        schema: String::new(),
    }));
    let lines = read_lines(&out, Duration::from_secs(5), |l| {
        msgs(l).iter().any(|m| m["t"] == "mode")
    });
    assert_eq!(
        msgs(&lines).iter().filter(|m| m["t"] == "options").count(),
        2
    );
}

#[test]
fn extension_without_options_gets_no_options_message() {
    let mut core = Core::start();
    let out = core._tmp.path().join("echo.log");
    core.install("echo", &["echo", out.to_str().unwrap(), "mode"]);
    core.config_options("echo", &["mode"], serde_json::json!({"volume": 0.5}));
    core.send(Msg::Mode(ModeEvent {
        ts: 1,
        chinese: true,
        full_width: false,
        schema: String::new(),
    }));
    let lines = read_lines(&out, Duration::from_secs(5), |l| {
        msgs(l).iter().any(|m| m["t"] == "mode")
    });
    let kinds: Vec<String> = msgs(&lines)
        .iter()
        .map(|m| m["t"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["welcome", "mode"]);
}
