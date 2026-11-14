//! 扩展宿主进程。由核心拉起，经 stdin/stdout 与核心通信；见 `lib.rs`。
// Windows 上不要控制台窗口：stdio 是核心建好的匿名管道，不依赖控制台。
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

fn usage() -> ! {
    eprintln!(
        "用法: wind_input_ext --extensions-dir <目录> [--log-dir <目录>] [--core-version <版本>]"
    );
    std::process::exit(2);
}

fn main() {
    let mut extensions_dir = None;
    let mut log_dir = None;
    let mut core_version = String::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.to_str() {
            Some("--extensions-dir") => extensions_dir = Some(PathBuf::from(val())),
            Some("--log-dir") => log_dir = Some(PathBuf::from(val())),
            Some("--core-version") => core_version = val().to_string_lossy().into_owned(),
            _ => usage(),
        }
    }
    let Some(extensions_dir) = extensions_dir else {
        usage()
    };
    std::process::exit(wind_extension_host::run(wind_extension_host::Opts {
        extensions_dir,
        log_dir,
        core_version,
    }));
}
