//! `wind_input sync now|status` 命令行：触发一次立即同步 / 查看同步状态。
//!
//! 经 RPC 打给运行中的 core（仅在线）。格式化全部是「JSON → (文本, 退出码)」的纯函数，
//! 便于逐分支测试。

use serde_json::{Value, json};

use crate::cli_util::rpc_online;

/// 子命令入口。`args` 为 `sync` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("now") => call("sync.now", format_now),
        Some("status") => call("sync.status", |v| (format_status(v, local_time), 0)),
        _ => {
            print_usage();
            2
        }
    }
}

fn call(method: &str, fmt: impl Fn(&Value) -> (String, i32)) -> i32 {
    match rpc_online(method, json!({})) {
        Ok(v) => {
            let (text, code) = fmt(&v);
            if code == 0 {
                println!("{text}");
            } else {
                eprintln!("{text}");
            }
            code
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn print_usage() {
    eprintln!(
        "用法: wind_input sync <命令>   （需要输入法服务在线）\n\
         \n\
         命令:\n  \
         now       立即同步一次（结果以桌面提示给出）\n  \
         status    查看同步状态"
    );
}

/// `sync.now` 的结果 → (文本, 退出码)。
pub(crate) fn format_now(v: &Value) -> (String, i32) {
    match v.get("outcome").and_then(Value::as_str) {
        Some("requested") => ("已请求同步，完成后会提示".to_string(), 0),
        Some("disabled") => ("数据同步未启用".to_string(), 2),
        Some("failed") => {
            let e = v.get("error").and_then(Value::as_str).unwrap_or("未知错误");
            (format!("同步失败：{e}"), 1)
        }
        _ => (format!("无法识别的同步结果: {v}"), 1),
    }
}

fn phase_label(phase: &str) -> &str {
    match phase {
        "idle" => "空闲",
        "syncing" => "同步中",
        "error" => "出错",
        "disabled" => "未启用",
        "not_installed" => "未安装",
        "starting" => "启动中",
        other => other,
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// `sync.status` 的视图 → 多行文本。`fmt_time` 把 unix 秒转成本地时间串（测试可注入）。
pub(crate) fn format_status(v: &Value, fmt_time: impl Fn(i64) -> String) -> String {
    let phase = str_of(v, "phase").unwrap_or("");
    let device = str_of(v, "device").unwrap_or("—");
    let group = match (str_of(v, "group_name"), str_of(v, "group")) {
        (Some(name), Some(id)) => format!("{name} ({id})"),
        (None, Some(id)) => id.to_string(),
        (Some(name), None) => name.to_string(),
        (None, None) => "—".to_string(),
    };
    let last_ok = v
        .get("last_ok")
        .and_then(Value::as_i64)
        .map_or_else(|| "—".to_string(), fmt_time);
    let mut out = format!(
        "状态: {}\n设备: {device}\n组: {group}\n上次成功: {last_ok}",
        phase_label(phase)
    );
    if let Some(e) = str_of(v, "last_error") {
        out.push_str(&format!("\n上次错误: {e}"));
    }
    out
}

fn local_time(secs: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(secs, 0).single().map_or_else(
        || secs.to_string(),
        |t| t.format("%Y-%m-%d %H:%M:%S").to_string(),
    )
}

#[cfg(test)]
#[path = "sync_cli_tests.rs"]
mod tests;
