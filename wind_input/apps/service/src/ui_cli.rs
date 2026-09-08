//! `wind_input ui ...` 命令行：让运行中的输入法弹一条桌面提示。
//!
//! 受众是**没有控制台可看的调用方**：计划任务、快捷方式、批处理脚本，以及
//! `$CC` 词条里 `wind.cli("ui toast …")` 的写法。终端里手敲普通子命令不需要它——
//! 那些命令的结果本来就打在 stdout 上。
//!
//! 经 RPC 打给运行中的 core（仅在线）：toast 由 core 进程自己的 UI 线程渲染，
//! CLI 进程没有窗口也没有 `ui_tx`，除了转交没有第二条路。

use serde_json::json;

use crate::cli_util::rpc_online;

/// 子命令入口。`args` 为 `ui` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("toast") => match cmd_toast(&args[1..]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Some("help") | Some("--help") | Some("-h") | None => {
            print_usage();
            0
        }
        Some(other) => {
            eprintln!("未知子命令: {other}");
            print_usage();
            2
        }
    }
}

fn print_usage() {
    eprintln!(
        "用法: wind_input ui <命令>   （需要输入法服务在线）\n\
         \n\
         命令:\n  \
         toast <文案> [--kind info|success|error] [--color #RRGGBB] [--pos <位置>] [--ms <毫秒>]\n\
         \n\
         位置: bottom_center(默认) center top_center top_left top_right bottom_left bottom_right\n\
         --kind 与 --color 只能给一个（都是强调色的写法）"
    );
}

fn cmd_toast(rest: &[String]) -> anyhow::Result<i32> {
    let Some(text) = rest.first() else {
        anyhow::bail!("用法: wind_input ui toast <文案> [--kind …] [--color …] [--pos …] [--ms …]");
    };
    let mut params = json!({ "text": text });
    let mut it = rest[1..].iter();
    while let Some(a) = it.next() {
        // 旗标值缺失必须报错而不是当空串：`--kind` 后面漏了值时静默弹一条 info
        // toast，用户只会以为 --kind 没实现。
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{a} 缺少参数值"))
        };
        match a.as_str() {
            "--kind" => params["kind"] = json!(val()?),
            "--color" => params["color"] = json!(val()?),
            "--pos" => params["pos"] = json!(val()?),
            // 不让 ParseIntError 直接冒泡：那句 "invalid digit found in string" 既不说
            // 是哪个参数、也不是中文，与短语层 `ms="5秒"` 的报错口径对不上。
            "--ms" => {
                let v = val()?;
                params["ms"] = json!(
                    v.parse::<u64>()
                        .map_err(|_| anyhow::anyhow!("--ms 需要毫秒数, 收到 {v:?}"))?
                );
            }
            other => anyhow::bail!("未知参数: {other}"),
        }
    }
    rpc_online("ui.toast", params)?;
    // **刻意不打确认行**：本命令最主要的用法就是词条里的
    // `wind.cli("ui toast …")`，而 `wind.cli` 默认取 stdout 末行弹 toast——
    // 打一句「✓ 已发送提示」就会在几毫秒后把用户自己那条提示覆盖掉。
    // 提示已经弹在屏幕上了，成功与否看退出码即可，再打一行纯属冗余。
    Ok(0)
}
