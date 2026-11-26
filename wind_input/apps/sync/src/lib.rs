//! 数据同步进程 `wind_input_sync`（设计 `docs/design/data-sync.md` §3、§7）。
//!
//! 由 core 拉起，stdin / stdout 是继承的匿名管道，协议见 `wind-sync-ipc`；不开任何具名端点。
//! stdin EOF 或 `Shutdown` 即退出。日志写 stderr，由 core 轮转收集。

mod backends;
mod backup;
mod errors;
mod fsutil;
mod hostname;
pub mod learn_round;
mod link;
mod local;
mod log;
pub mod runner;
pub mod secrets;
mod statefile;
pub mod testing;
mod watch;

pub use backends::{BackendFactory, NetFactory};
pub use link::{CoreLink, PipeLink, REQUEST_TIMEOUT};
pub use local::FsLocal;
pub use runner::{Config, Input, Runner, Timing};

use std::io::{self, BufReader};
use std::path::PathBuf;
use std::sync::mpsc;

use wind_sync_ipc::{ToSync, read_frame};

/// 进程入口：读 Hello、起读线程、跑主循环。返回退出码。
pub fn run_process() -> i32 {
    // core 被 SIGKILL 时 stdin 也会断，这里只是让退出更及时。
    wind_supervise::die_with_parent();
    let mut stdin = BufReader::new(io::stdin());
    let hello = match read_frame::<_, ToSync>(&mut stdin) {
        Ok(Some(ToSync::Hello {
            userdata_dir,
            state_dir,
            settings,
            suffix,
            portable,
        })) => {
            log::log!("启动（变体后缀 {suffix:?}）");
            (userdata_dir, state_dir, settings, portable)
        }
        Ok(None) => return 0,
        Ok(Some(_)) | Err(_) => {
            log::log!("第一帧不是 Hello，退出");
            return 2;
        }
    };
    let (userdata_dir, state_dir, settings, portable) = hello;
    let state_dir = PathBuf::from(state_dir);
    let link = PipeLink::new(Box::new(io::stdout()), REQUEST_TIMEOUT);
    let (tx, rx) = mpsc::channel::<Input>();

    {
        let link = link.clone();
        let tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("sync-stdin".into())
            .spawn(move || {
                loop {
                    match read_frame::<_, ToSync>(&mut stdin) {
                        Ok(Some(ToSync::Reply { id, result, error })) => link.deliver(
                            id,
                            match error {
                                Some(e) => Err(e),
                                None => Ok(result.unwrap_or_default()),
                            },
                        ),
                        Ok(Some(m)) => {
                            if tx.send(Input::Core(m)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            log::log!("读取 core 的帧失败: {e}");
                            break;
                        }
                    }
                }
                link.fail_all("与核心的连接已断开");
                let _ = tx.send(Input::Eof);
            });
        if spawned.is_err() {
            return 1;
        }
    }

    let cfg = Config {
        userdata_dir: PathBuf::from(userdata_dir),
        secrets: secrets::default_store(&state_dir),
        state_dir,
        settings,
        link,
        factory: Box::new(NetFactory),
        timing: Timing::default(),
        watch: true,
        class: wind_sync::DeviceClass::Desktop,
        portable,
        on_demand: false,
    };
    Runner::new(cfg, tx).run(rx);
    0
}
