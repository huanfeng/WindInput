//! `wind_input backup ...` 命令行：整机备份的创建/查看/还原。
//!
//! 经 RPC 打给运行中的 core（仅在线）。**文件读写在 core 进程内完成**
//! （`backup.create`/`backup.restore` 的 RPC 收路径参数），故 CLI 侧必须先把
//! 相对路径转为绝对路径——否则会按 core 进程的工作目录解析而非当前终端。
//! 路径经 [`resolve_path`] 解析，支持 `${APP_DIR}` / `${USER_DATA}` /
//! `${LOCAL_DATA}` 内部目录变量。

use serde_json::{Value, json};

use crate::cli_util::{resolve_path, rpc_online};

/// 还原可选的域名。backup.rs 的 `section_of` 只重映射 schema_file/theme_file/
/// stats_meta 三类，其余条目 type（config/state/dict/temp/freq/shadow/phrase/
/// common_chars/charset/completion/stats）原样即域名——白名单须覆盖备份包实际写入的
/// 全部 type，否则对应域无法单独还原（`--sections` 里写了也会被静默丢弃）。
///
/// ⚠️ 这张表与 `backup.rs::create_backup` 实际写入的 type 集合**没有编译期约束**，
/// 漏一个不报错、只是那个域从此单独还原不了。`common_chars` 与 `charset` 曾这样漏了
/// 一段时间，本次补齐并连同新增的 `completion` 一起登记。
const RESTORE_SECTIONS: &[&str] = &[
    "config",
    "dict",
    "temp",
    "freq",
    "shadow",
    "phrase",
    "common_chars",
    "charset",
    "completion",
    "schemas",
    "themes",
    "state",
    "stats",
];

/// 子命令入口。`args` 为 `backup` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        Some("create") => match args.get(1) {
            Some(file) => cmd_create(file, &args[2..]),
            None => return usage_err("create <文件.zip> [--stats] [--state]"),
        },
        Some("inspect") => match args.get(1) {
            Some(file) => cmd_inspect(file),
            None => return usage_err("inspect <文件.zip>"),
        },
        Some("restore") => match args.get(1) {
            Some(file) => cmd_restore(file, &args[2..]),
            None => return usage_err("restore <文件.zip> [--replace] [--sections a,b,...]"),
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
        "用法: wind_input backup <命令>   （需要输入法服务在线）\n\
         \n\
         命令:\n  \
         create <文件.zip> [--stats] [--state]      创建整机备份（可选包含统计/界面状态）\n  \
         inspect <文件.zip>                         查看备份包内容清单\n  \
         restore <文件.zip> [--replace] [--sections a,b]\n                                             \
         还原（缺省合并；--replace 覆盖）\n\
         \n\
         还原域: {}",
        RESTORE_SECTIONS.join(" / ")
    );
}

fn usage_err(form: &str) -> i32 {
    eprintln!("用法: wind_input backup {form}");
    2
}

fn cmd_create(file: &str, rest: &[String]) -> anyhow::Result<i32> {
    let mut include_stats = false;
    let mut include_state = false;
    for a in rest {
        match a.as_str() {
            "--stats" => include_stats = true,
            "--state" => include_state = true,
            other => anyhow::bail!("未知参数: {other}"),
        }
    }
    let path = resolve_path(file)?;
    let v = rpc_online(
        "backup.create",
        json!({ "path": path, "includeStats": include_stats, "includeState": include_state }),
    )?;
    let out = v.get("path").and_then(Value::as_str).unwrap_or(&path);
    let entries = manifest_entry_count(&v);
    match entries {
        Some(n) => println!("✓ 备份已创建: {out}（{n} 个条目）"),
        None => println!("✓ 备份已创建: {out}"),
    }
    Ok(0)
}

fn cmd_inspect(file: &str) -> anyhow::Result<i32> {
    let path = resolve_path(file)?;
    let v = rpc_online("backup.inspect", json!({ "path": path }))?;
    let Some(m) = v.get("manifest") else {
        anyhow::bail!("backup.inspect 返回了意外形状");
    };
    for (field, name) in [
        ("kind", "类型"),
        ("created_at", "创建时间"),
        ("platform", "平台"),
        ("app_version", "版本"),
    ] {
        if let Some(s) = m.get(field).and_then(Value::as_str)
            && !s.is_empty()
        {
            println!("{name}: {s}");
        }
    }
    if let Some(entries) = m.get("contents").and_then(Value::as_array) {
        println!("条目 {} 个:", entries.len());
        for e in entries {
            let path = e.get("path").and_then(Value::as_str).unwrap_or("?");
            let ty = e.get("type").and_then(Value::as_str).unwrap_or("");
            println!("  {path:<40} {ty}");
        }
    }
    Ok(0)
}

fn cmd_restore(file: &str, rest: &[String]) -> anyhow::Result<i32> {
    let mut replace = false;
    let mut sections: Option<Vec<String>> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--replace" => replace = true,
            "--sections" => {
                let raw = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--sections 缺少参数（逗号分隔的域名）"))?;
                let keys: Vec<String> = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
                for k in &keys {
                    if !RESTORE_SECTIONS.contains(&k.as_str()) {
                        anyhow::bail!("未知还原域: {k}（可选: {}）", RESTORE_SECTIONS.join(" / "));
                    }
                }
                if keys.is_empty() {
                    anyhow::bail!("--sections 参数为空");
                }
                sections = Some(keys);
            }
            other => anyhow::bail!("未知参数: {other}"),
        }
    }
    let path = resolve_path(file)?;
    let mut params = json!({ "path": path });
    if replace {
        params["strategy"] = json!("replace");
    }
    if let Some(s) = &sections {
        params["sections"] = json!(s);
    }
    let v = rpc_online("backup.restore", params)?;
    let restored = v
        .get("restored")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let conflicts = v
        .get("conflicts")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if conflicts > 0 {
        println!("✓ 已还原 {restored} 项（{conflicts} 项已存在跳过；用 --replace 覆盖）");
    } else {
        println!("✓ 已还原 {restored} 项");
    }
    println!("提示: 部分改动（如配置/方案）已即时刷新，若显示异常可重启输入法");
    Ok(0)
}

/// 从 create/inspect 响应中取 manifest 条目数。
fn manifest_entry_count(v: &Value) -> Option<usize> {
    v.get("manifest")?
        .get("contents")
        .and_then(Value::as_array)
        .map(Vec::len)
}
