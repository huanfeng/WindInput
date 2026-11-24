//! 数据同步进程。由 core 拉起，经 stdin/stdout 匿名管道通信；见 `lib.rs`。
// Windows 上不要控制台窗口：stdio 是核心建好的匿名管道，不依赖控制台。
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    std::process::exit(wind_sync_host::run_process());
}
