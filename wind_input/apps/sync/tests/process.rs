//! 进程级：真起 `wind_input_sync` 子进程，由测试扮演 core（匿名管道、帧协议、Hello、请求回执）。
//!
//! - `process_protocol_smoke`：不需要网络。
//! - `two_processes_sync_over_webdav`：真 WebDAV（dufs）+ 真 Argon2 参数 + 真文件监听，
//!   `#[ignore]`，由 `WD_URL` / `WD_USER` / `WD_PASS` 驱动：
//!   `docker run -d --rm -p 127.0.0.1:15001:5000 --tmpfs /data sigoden/dufs -A -a 'wind:wind-pass@/:rw' /data`
//!   `env -u ALL_PROXY -u HTTP_PROXY -u HTTPS_PROXY WD_URL=http://127.0.0.1:15001 WD_USER=wind WD_PASS=wind-pass cargo test -p wind_sync_host --test process -- --ignored`

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wind_sync_ipc::{Backend, FromSync, Op, Status, SyncSettings, ToSync, read_frame, write_frame};

const WAIT: Duration = Duration::from_secs(30);

struct Proc {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    rx: Receiver<FromSync>,
    requests: Arc<Mutex<Vec<String>>>,
    next_id: u64,
    user: PathBuf,
    _dir: tempfile::TempDir,
}

impl Proc {
    fn start(settings: SyncSettings) -> Proc {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        std::fs::create_dir_all(&user).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_wind_input_sync"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let (tx, rx) = mpsc::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        {
            let stdin = stdin.clone();
            let requests = requests.clone();
            std::thread::spawn(move || {
                while let Ok(Some(m)) = read_frame::<_, FromSync>(&mut out) {
                    // core 的角色：白名单请求一律成功
                    if let FromSync::Request { id, method, params } = &m {
                        let key = match params.get("path").and_then(Value::as_str) {
                            Some(p) => format!("{method} {p}"),
                            None => method.clone(),
                        };
                        requests.lock().unwrap().push(key);
                        let _ = write_frame(
                            &mut *stdin.lock().unwrap(),
                            &ToSync::reply(*id, Ok(json!({}))),
                        );
                        continue;
                    }
                    let _ = tx.send(m);
                }
            });
        }
        let p = Proc {
            child,
            stdin,
            rx,
            requests,
            next_id: 1,
            user,
            _dir: dir,
        };
        p.send(&ToSync::Hello {
            suffix: "_t".into(),
            userdata_dir: p.user.to_string_lossy().into_owned(),
            state_dir: p._dir.path().join("state").to_string_lossy().into_owned(),
            settings,
            portable: false,
        });
        p
    }

    fn send(&self, m: &ToSync) {
        write_frame(&mut *self.stdin.lock().unwrap(), m).unwrap();
    }

    fn recv_until<T>(&self, what: &str, mut f: impl FnMut(FromSync) -> Option<T>) -> T {
        let t = Instant::now();
        loop {
            let left = WAIT.saturating_sub(t.elapsed());
            let m = self
                .rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("等待「{what}」超时"));
            if let Some(v) = f(m) {
                return v;
            }
        }
    }

    fn status(&self, what: &str, f: impl Fn(&Status) -> bool) -> Status {
        self.recv_until(what, |m| match m {
            FromSync::Status(s) if f(&s) => Some(s),
            _ => None,
        })
    }

    fn cmd(&mut self, op: Op, args: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&ToSync::Command { id, op, args });
        self.recv_until("命令回执", |m| match m {
            FromSync::CommandReply {
                id: i,
                result,
                error,
            } if i == id => Some(match error {
                Some(e) => Err(e),
                None => Ok(result.unwrap_or_default()),
            }),
            _ => None,
        })
    }

    fn shutdown(mut self) -> std::process::ExitStatus {
        self.send(&ToSync::Shutdown);
        let t = Instant::now();
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s;
            }
            assert!(t.elapsed() < WAIT, "进程没有在 Shutdown 后退出");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn process_protocol_smoke() {
    let mut p = Proc::start(SyncSettings::default());
    p.status("未配置", |s| s.phase == "not_configured");
    assert!(p.cmd(Op::SyncNow, Value::Null).is_err());
    assert!(
        p.cmd(Op::ExportKey, Value::Null)
            .unwrap_err()
            .contains("没有可导出的密钥")
    );
    assert!(p.shutdown().success());
}

#[test]
fn first_frame_must_be_hello() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wind_input_sync"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    write_frame(&mut stdin, &ToSync::Shutdown).unwrap();
    let st = child.wait().unwrap();
    assert_eq!(st.code(), Some(2));
}

#[test]
fn stdin_eof_exits_cleanly() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wind_input_sync"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_frame(
        &mut stdin,
        &ToSync::Hello {
            suffix: String::new(),
            userdata_dir: dir.path().join("u").to_string_lossy().into_owned(),
            state_dir: dir.path().join("s").to_string_lossy().into_owned(),
            settings: SyncSettings::default(),
            portable: false,
        },
    )
    .unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
#[ignore = "需要 WD_URL（dufs）"]
fn two_processes_sync_over_webdav() {
    let url = std::env::var("WD_URL").expect("WD_URL");
    let user = std::env::var("WD_USER").expect("WD_USER");
    let pass = std::env::var("WD_PASS").expect("WD_PASS");
    // 每次用独立子目录，避免与旧数据互相影响
    let sub = format!("{url}/run-{}", std::process::id());
    let settings = SyncSettings {
        enabled: true,
        backend: Some(Backend::Webdav {
            url: sub.clone(),
            user,
        }),
        ..Default::default()
    };
    // dufs 需要目录先存在
    let st = Command::new("curl")
        .args([
            "-s",
            "-o",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "-X",
            "MKCOL",
            "-u",
        ])
        .arg(format!("{}:{pass}", std::env::var("WD_USER").unwrap()))
        .arg(&sub)
        .env("NO_PROXY", "*")
        .status()
        .unwrap();
    assert!(st.success());

    let mut a = Proc::start(settings.clone());
    let mut b = Proc::start(settings);
    for p in [&mut a, &mut b] {
        p.cmd(Op::SetSecret, json!({"password": pass})).unwrap();
        let t = p.cmd(Op::TestConnection, Value::Null).unwrap();
        assert_eq!(t["ok"], true);
    }
    assert_eq!(
        a.cmd(Op::SetPassphrase, json!({"passphrase": "pw1"}))
            .unwrap()["mode"],
        "init"
    );
    assert!(
        b.cmd(Op::SetPassphrase, json!({"passphrase": "bad"}))
            .unwrap_err()
            .contains("口令错误")
    );
    assert_eq!(
        b.cmd(Op::SetPassphrase, json!({"passphrase": "pw1"}))
            .unwrap()["mode"],
        "join"
    );
    // b 加入 a 的设备组（两进程同机，默认号相同，b 会自动改用带序号的号）
    let roster = a.cmd(Op::ListDevices, Value::Null).unwrap();
    let a_group = roster["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["is_self"] == true)
        .unwrap()["group"]
        .clone();
    b.cmd(Op::JoinGroup, json!({"group": a_group, "mode": "theirs"}))
        .unwrap();

    write(&a.user, "config.toml", "k = 1\n");
    write(
        &a.user,
        "data/pinyin/user.dict.yaml",
        "---\nname: user\ncolumns: [code, text, weight]\n...\nnihao\t你好\t1\n",
    );
    a.cmd(Op::SyncNow, Value::Null).unwrap();
    a.status("A 同步完成", |s| {
        s.phase == "idle" && s.last_ok.is_some()
    });
    b.cmd(Op::SyncNow, Value::Null).unwrap();
    b.status("B 同步完成", |s| {
        s.phase == "idle" && s.last_ok.is_some()
    });
    assert_eq!(
        std::fs::read_to_string(b.user.join("config.toml")).unwrap(),
        "k = 1\n"
    );
    assert!(b.user.join("data/pinyin/user.dict.yaml").is_file());
    let reqs = b.requests.lock().unwrap().clone();
    assert_eq!(
        reqs,
        // 本地原先没有的词库文件不开编辑会话，写入后靠 dict.reload 纳入
        vec!["config.reload", "dict.reload"]
    );
    assert!(a.shutdown().success());
    assert!(b.shutdown().success());
}

fn write(root: &Path, rel: &str, c: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}
