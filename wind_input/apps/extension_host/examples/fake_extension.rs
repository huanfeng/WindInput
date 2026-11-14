//! 集成测试用的假扩展。用法：
//!
//! - `fake_extension echo <输出文件> <档位>...`：握手申请这些档位，把收到的每条消息按行写进输出文件。
//!   启动时先写一行 `pid <进程号>`。stdin EOF 即退出。
//! - `fake_extension crash`：立即以退出码 3 退出。

use std::io::Write;

use wind_hook::frame::{Frame, read_frame, write_frame};
use wind_hook::msg::Msg;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("echo") => echo(&args[1], args[2..].to_vec()),
        _ => std::process::exit(3),
    }
}

fn echo(out: &str, want: Vec<String>) {
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)
        .unwrap();
    writeln!(out, "pid {}", std::process::id()).unwrap();
    let mut stdout = std::io::stdout();
    write_frame(
        &mut stdout,
        &Frame::new(Msg::Hello {
            protocol: vec![1],
            want,
        }),
    )
    .unwrap();
    let mut stdin = std::io::stdin().lock();
    while let Ok(Some(f)) = read_frame(&mut stdin) {
        let line = serde_json::json!({ "seq": f.seq, "msg": f.msg });
        writeln!(out, "{line}").unwrap();
    }
}
