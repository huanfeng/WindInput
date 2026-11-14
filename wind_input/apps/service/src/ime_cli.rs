//! `wind_input ime ...` 命令行：读写运行中输入法的输入状态开关（中英、全半角、简繁、工具栏）。
//!
//! 受众是**脚本与外部工具**（如 wezterm 在特定场景下先切成英文）。词汇与命令栏的
//! `ime.get` / `ime.set` / `ime.toggle` 一致，两处文档、用法互相通用。
//!
//! 经 RPC 打给运行中的 core（仅在线）：状态在 core 进程里，切换要带状态气泡与工具栏刷新，
//! CLI 进程没有这些。切的是**全局**输入状态，不是某个窗口的。

use serde_json::{Value, json};

use crate::cli_util::rpc_online;

/// 目标白名单，与 core 侧 `ime_state_get` 一致。
const TARGETS: [&str; 5] = ["cn-en", "fullshape", "s2t", "t2s", "toolbar"];

/// 子命令入口。`args` 为 `ime` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        Some("get") => cmd_get(&args[1..]),
        Some("set") => cmd_set(&args[1..]),
        Some("toggle") => cmd_toggle(&args[1..]),
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
        "用法: wind_input ime <命令>   （需要输入法服务在线）\n\
         \n\
         命令:\n  \
         get [目标] [--json]        查看输入状态；不带目标 = 全部\n  \
         set <目标> <值> [--notify] 设为指定状态（幂等）；--notify = 已是该状态时也弹提示\n  \
         toggle <目标>              取反，并输出切换后的值\n\
         \n\
         目标: cn-en（中英）fullshape（全角）s2t（简入繁出）t2s（繁入简出）toolbar（工具栏）\n\
         值:   on / off（也认 true/false、1/0、开/关）；cn-en 另认 zh（中文，= on）/ en（英文，= off）\n\
         \n\
         示例:\n  \
         wind_input ime set cn-en en     # 切到英文\n  \
         wind_input ime get cn-en        # 输出 zh 或 en\n\
         \n\
         切换的是全局输入状态；cn-en 不含 CapsLock 的临时英文。"
    );
}

/// 校验目标名。认不出直接报错并列出可选项，不猜。
fn check_target(t: &str) -> anyhow::Result<&str> {
    if TARGETS.contains(&t) {
        Ok(t)
    } else {
        anyhow::bail!("不认识的目标 {t:?}，可选: {}", TARGETS.join(" / "))
    }
}

/// 解析状态值。`cn-en` 另认 zh/en；其余目标认 zh/en 没意义，按未知值报错。
fn parse_value(target: &str, raw: &str) -> anyhow::Result<bool> {
    let v = raw.trim().to_lowercase();
    match v.as_str() {
        "on" | "true" | "1" | "开" => Ok(true),
        "off" | "false" | "0" | "关" => Ok(false),
        "zh" | "中" | "中文" if target == "cn-en" => Ok(true),
        "en" | "英" | "英文" if target == "cn-en" => Ok(false),
        _ => anyhow::bail!("不认识的值 {raw:?}，可选: on / off{}", {
            if target == "cn-en" { " / zh / en" } else { "" }
        }),
    }
}

/// 把状态渲染成用户可读的词：`cn-en` 为 zh/en，其余为 on/off。
fn render(target: &str, on: bool) -> &'static str {
    match (target, on) {
        ("cn-en", true) => "zh",
        ("cn-en", false) => "en",
        (_, true) => "on",
        (_, false) => "off",
    }
}

fn usage_err(form: &str) -> i32 {
    eprintln!("用法: wind_input ime {form}");
    2
}

fn cmd_get(rest: &[String]) -> anyhow::Result<i32> {
    let mut json_out = false;
    let mut target: Option<&str> = None;
    for a in rest {
        match a.as_str() {
            "--json" => json_out = true,
            s if s.starts_with('-') => anyhow::bail!("未知参数: {s}"),
            s if target.is_none() => target = Some(check_target(s)?),
            _ => return Ok(usage_err("get [目标] [--json]")),
        }
    }
    let params = match target {
        Some(t) => json!({ "target": t }),
        None => json!({}),
    };
    let r = rpc_online("ime.get", params)?;
    if json_out {
        println!("{r}");
        return Ok(0);
    }
    match target {
        // 单个目标只输出一个词，脚本直接 `$(wind_input ime get cn-en)` 取用。
        Some(t) => println!("{}", render(t, bool_of(&r, "on")?)),
        None => {
            for t in TARGETS {
                println!("{t:<10} {}", render(t, bool_of(&r, t)?));
            }
        }
    }
    Ok(0)
}

fn cmd_set(rest: &[String]) -> anyhow::Result<i32> {
    let mut notify = false;
    let mut pos: Vec<&str> = Vec::new();
    for a in rest {
        match a.as_str() {
            "--notify" => notify = true,
            s if s.starts_with('-') => anyhow::bail!("未知参数: {s}"),
            s => pos.push(s),
        }
    }
    let [target, value] = pos.as_slice() else {
        return Ok(usage_err("set <目标> <值> [--notify]"));
    };
    let target = check_target(target)?;
    let on = parse_value(target, value)?;
    rpc_online(
        "ime.set",
        json!({ "target": target, "on": on, "notify": notify }),
    )?;
    // **刻意不打确认行**：本命令的主要用法是脚本 / `wind.cli("ime set …")`，
    // 后者默认取 stdout 末行弹 toast，确认行会让每次切换多弹一条（同 `ui toast`）。
    Ok(0)
}

fn cmd_toggle(rest: &[String]) -> anyhow::Result<i32> {
    let [target] = rest else {
        return Ok(usage_err("toggle <目标>"));
    };
    let target = check_target(target)?;
    let r = rpc_online("ime.toggle", json!({ "target": target }))?;
    println!("{}", render(target, bool_of(&r, "on")?));
    Ok(0)
}

fn bool_of(v: &Value, key: &str) -> anyhow::Result<bool> {
    v.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("服务端返回了意外形状（缺 {key}）"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cn_en_accepts_zh_en_aliases_but_other_targets_do_not() {
        assert!(parse_value("cn-en", "zh").unwrap());
        assert!(!parse_value("cn-en", "EN").unwrap());
        assert!(parse_value("cn-en", "on").unwrap());
        assert!(parse_value("fullshape", "zh").is_err());
        assert!(parse_value("s2t", "en").is_err());
    }

    #[test]
    fn generic_values_are_case_insensitive_and_unknown_rejected() {
        for v in ["on", "TRUE", "1", "开"] {
            assert!(parse_value("s2t", v).unwrap(), "{v}");
        }
        for v in ["off", "False", "0", "关"] {
            assert!(!parse_value("s2t", v).unwrap(), "{v}");
        }
        assert!(parse_value("s2t", "maybe").is_err());
    }

    #[test]
    fn unknown_target_lists_choices() {
        let e = check_target("layout").unwrap_err().to_string();
        assert!(e.contains("cn-en") && e.contains("toolbar"), "{e}");
        for t in TARGETS {
            assert_eq!(check_target(t).unwrap(), t);
        }
    }

    #[test]
    fn render_uses_zh_en_only_for_cn_en() {
        assert_eq!(render("cn-en", true), "zh");
        assert_eq!(render("cn-en", false), "en");
        assert_eq!(render("fullshape", true), "on");
        assert_eq!(render("toolbar", false), "off");
    }
}
