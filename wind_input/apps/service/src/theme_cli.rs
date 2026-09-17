//! `wind_input theme ...` 命令行：列出主题、预览与导入主题包（`.wtheme`）。
//!
//! 与设置端的图形入口是同一条 RPC，只是把「看清楚再决定」这一步交给终端输出而不是
//! 确认框。存在的理由有两条：一是主题包的分发方（作者、市场）需要一个能脚本化的
//! 校验入口——打完包先在真机上过一遍导入，比发布后等用户报错便宜；二是靶机上的
//! 验证不该依赖 GUI。

use serde_json::{Value, json};

use crate::cli_util::rpc_online;

/// 子命令入口。`args` 为 `theme` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        Some("list") => cmd_list(),
        Some("preview") => match args.get(1) {
            Some(path) => cmd_preview(path),
            None => return usage_err("preview <包路径>"),
        },
        Some("import") => match args.get(1) {
            Some(path) => cmd_import(path, &args[2..]),
            None => return usage_err("import <包路径> [--force] [--id <主题id>]"),
        },
        Some("help") | Some("--help") | Some("-h") | None => {
            print_usage();
            return 0;
        }
        Some(other) => {
            eprintln!("未知子命令: {other}");
            print_usage();
            return 2;
        }
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn print_usage() {
    eprintln!(
        "用法: wind_input theme <命令>   （需要输入法服务在线）\n\
         \n\
         命令:\n  \
         list                              列出已安装主题（* = 当前生效）\n  \
         preview <包路径>                  只读查看一个 .wtheme 里装的是什么，不导入\n  \
         import <包路径> [选项]            导入主题包\n    \
         --force                         同 id 已存在时覆盖（默认拒绝并提示）\n    \
         --id <主题id>                   指定落到哪个目录 id（默认用包里的主题名）"
    );
}

fn usage_err(form: &str) -> i32 {
    eprintln!("用法: wind_input theme {form}");
    2
}

fn cmd_list() -> anyhow::Result<i32> {
    let items = rpc_online("theme.list", json!({}))?;
    let current = rpc_online("config.get", json!({ "key": "ui.theme.name" }))
        .ok()
        .and_then(|v| v.get("value").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let Some(rows) = items.as_array() else {
        anyhow::bail!("theme.list 返回的不是数组");
    };
    for row in rows {
        let id = row.get("name").and_then(Value::as_str).unwrap_or("");
        let display = row
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or(id);
        let builtin = row.get("builtin").and_then(Value::as_bool).unwrap_or(false);
        let mark = if id == current { "*" } else { " " };
        let kind = if builtin { "内置" } else { "用户" };
        println!("{mark} {id:<24} {kind}  {display}");
    }
    Ok(0)
}

fn cmd_preview(path: &str) -> anyhow::Result<i32> {
    let v = rpc_online("theme.previewPackage", json!({ "path": path }))?;
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or("（未填）")
            .to_string()
    };
    println!("主题名: {}", s("display_name"));
    println!("作者:   {}", s("author"));
    println!("版本:   {}", s("version"));
    println!(
        "资源:   {} 个文件{}",
        v.get("asset_count").and_then(Value::as_u64).unwrap_or(0),
        if v.get("has_preview").and_then(Value::as_bool) == Some(true) {
            "，含市场预览图"
        } else {
            ""
        }
    );
    Ok(0)
}

fn cmd_import(path: &str, rest: &[String]) -> anyhow::Result<i32> {
    let mut params = json!({ "path": path });
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--force" => params["force"] = json!(true),
            "--id" => match it.next() {
                Some(id) => params["slug"] = json!(id),
                None => {
                    eprintln!("--id 后面要跟主题 id");
                    return Ok(2);
                }
            },
            other => {
                eprintln!("未知选项: {other}");
                return Ok(2);
            }
        }
    }

    let v = rpc_online("theme.importPackage", params)?;
    // 同名已存在是**可预期的业务性失败**，core 走 result 字段回报（`conflict: true`）
    // 而非 error 通道。靠匹配报错文案来认冲突的话，core 改一次文案这里就静默失效。
    if v.get("conflict").and_then(Value::as_bool) == Some(true) {
        let msg = v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("该主题已存在");
        eprintln!("{msg}；要覆盖请加 --force");
        return Ok(1);
    }
    let id = v.get("slug").and_then(Value::as_str).unwrap_or("");
    let display = v.get("display_name").and_then(Value::as_str).unwrap_or(id);
    let files = v
        .get("files")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    println!("已导入「{display}」→ 主题 id {id}（{files} 个文件）");
    if v.get("reloaded").and_then(Value::as_bool) == Some(true) {
        println!("导入的正是当前生效主题，已即时重新加载。");
    } else {
        println!("在设置里切换到该主题即可生效。");
    }
    Ok(0)
}
