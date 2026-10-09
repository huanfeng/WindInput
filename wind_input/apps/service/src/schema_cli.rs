//! `wind_input schema ...` 命令行：方案配置与分类（扩展）词库开关的直通入口。
//!
//! 全部经 RPC 打给运行中的 core（**仅在线**，见 `cli_util` 模块文档）：
//! - `set`/`reset` 复用设置页同一条 override 稀疏 diff 落盘路径（getConfig →
//!   改单键 → saveConfig），方案文件本体永不改写；
//! - `dict enable/disable` 走 `schema.setDictEnabled`（override 只落 `{id, enabled}`，
//!   已加载引擎热插拔即时生效）；
//! - `set`/`reset` 成功后调 `schema.invalidate` 失效引擎缓存，下次使用按新配置重建。

use serde_json::{Value, json};

use crate::cli_util::{coerce_like, format_value, json_get_path, json_set_path, rpc_online};

/// 子命令入口。`args` 为 `schema` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        Some("list") => cmd_list(),
        Some("get") => match args.get(1) {
            Some(id) => cmd_get(id, args.get(2).map(String::as_str)),
            None => return usage_err("get <方案id> [键]"),
        },
        Some("set") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(id), Some(key), Some(raw)) => cmd_set(id, key, raw),
            _ => return usage_err("set <方案id> <键> <值>"),
        },
        Some("reset") => match args.get(1) {
            Some(id) if args.get(2).is_none() => cmd_reset(id),
            Some(_) => {
                eprintln!("暂不支持单键重置；reset 会清除该方案的全部定制（override）");
                return 2;
            }
            None => return usage_err("reset <方案id>"),
        },
        Some("rebuild") => match args.get(1) {
            None => cmd_rebuild(),
            Some(_) => {
                eprintln!("rebuild 为全量重建（不支持指定方案）：缓存指纹会在源变化时自动重建，");
                eprintln!("需要手动重建的场景（如升级后强制刷新）都应全量执行");
                return 2;
            }
        },
        Some("dict") => match args.get(1).map(String::as_str) {
            Some("list") => match args.get(2) {
                Some(id) => cmd_dict_list(id),
                None => return usage_err("dict list <方案id>"),
            },
            Some(op @ ("enable" | "disable")) => match args.get(2) {
                Some(id) if args.len() > 3 => cmd_dict_toggle(id, &args[3..], op == "enable"),
                _ => return usage_err("dict enable|disable <方案id> <词库id> [<词库id>...]"),
            },
            _ => return usage_err("dict list|enable|disable ..."),
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
        "用法: wind_input schema <命令>   （需要输入法服务在线）\n\
         \n\
         命令:\n  \
         list                              列出已安装方案（* = 当前激活）\n  \
         get <方案id> [键]                 查看方案配置（含定制合并；键为点路径如 engine.codetable.min_len）\n  \
         set <方案id> <键> <值>            修改方案配置（写入定制层，方案文件不动）\n  \
         reset <方案id>                    清除该方案的全部定制，恢复方案文件默认\n  \
         rebuild                           强制重建全部词库缓存（下次使用各方案时按源重新生成）\n  \
         dict list <方案id>                列出方案的词库及启用状态\n  \
         dict enable <方案id> <词库id>...  启用分类/扩展词库（可多个，即时生效）\n  \
         dict disable <方案id> <词库id>... 停用分类/扩展词库（可多个，即时生效）"
    );
}

fn usage_err(form: &str) -> i32 {
    eprintln!("用法: wind_input schema {form}");
    2
}

fn cmd_list() -> anyhow::Result<i32> {
    let items = rpc_online("schema.list", json!({}))?;
    let active = rpc_online("schema.active", json!({}))?
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(rows) = items.as_array() else {
        anyhow::bail!("schema.list 返回了意外形状");
    };
    for it in rows {
        let id = it.get("id").and_then(Value::as_str).unwrap_or("?");
        let name = it.get("name").and_then(Value::as_str).unwrap_or("");
        let etype = it.get("engineType").and_then(Value::as_str).unwrap_or("?");
        let builtin = it.get("builtin").and_then(Value::as_bool).unwrap_or(true);
        let mark = if id == active { "*" } else { " " };
        let origin = if builtin { "内置" } else { "用户" };
        println!("{mark} {id:<20} {name:<16} [{etype}] {origin}");
    }
    Ok(0)
}

/// 取合并后的方案配置；方案不存在（core 返回空对象）时报错。
fn fetch_config(id: &str) -> anyhow::Result<Value> {
    let v = rpc_online("schema.getConfig", json!({ "id": id }))?;
    if v.as_object().is_none_or(|o| o.is_empty()) {
        anyhow::bail!("方案不存在: {id}（用 `wind_input schema list` 查看可用方案）");
    }
    Ok(v)
}

fn cmd_get(id: &str, key: Option<&str>) -> anyhow::Result<i32> {
    let cfg = fetch_config(id)?;
    match key {
        None => println!("{}", serde_json::to_string_pretty(&cfg)?),
        Some(k) => match json_get_path(&cfg, k) {
            Some(v) => println!("{}", format_value(v)),
            None => anyhow::bail!("方案 {id} 配置无此键: {k}"),
        },
    }
    Ok(0)
}

fn cmd_set(id: &str, key: &str, raw: &str) -> anyhow::Result<i32> {
    // 词库开关走专门命令：dictionaries 是数组（点路径不达），且其 override
    // 必须保持 `{id, enabled}` 稀疏形态，不能经通用 set 路径触碰。
    if key == "dictionaries" || key.starts_with("dictionaries.") {
        anyhow::bail!("词库启停请用 `wind_input schema dict enable|disable {id} <词库id>`");
    }
    let mut cfg = fetch_config(id)?;
    let current = json_get_path(&cfg, key)
        .filter(|v| !v.is_null())
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!("方案 {id} 配置无此键: {key}（用 `schema get {id}` 查看全部键）")
        })?;
    let new = coerce_like(&current, raw).map_err(|e| anyhow::anyhow!("无法设置 '{key}': {e}"))?;
    if !json_set_path(&mut cfg, key, new) {
        anyhow::bail!("写入键 {key} 失败");
    }
    // 提交前剥掉 dictionaries：合并配置里它带着 merge_dict_overrides 注入的 enabled，
    // 与 base 不等时 json_diff 会把**整份结构数组**写进 override，冻结方案的词库定义
    // （path/顺序/新增库全部透不过来）。core 的 saveConfig 现已在入口无条件剥掉
    // dictionaries、无条件回填既有稀疏开关（论坛 t281），这里的剥除是冗余的双保险，
    // 保留它无害——也让 CLI 对较旧的 core 同样安全。
    if let Some(o) = cfg.as_object_mut() {
        o.remove("dictionaries");
    }
    rpc_online("schema.saveConfig", json!({ "id": id, "cfg": cfg }))?;
    // 失效引擎缓存让改动下次使用即生效（未加载方案下安全 no-op）。
    rpc_online("schema.invalidate", json!({ "id": id }))?;
    println!("✓ {id}: {key} = {raw}（已写入定制层并刷新引擎）");
    Ok(0)
}

fn cmd_reset(id: &str) -> anyhow::Result<i32> {
    // 先确认方案存在，避免对 typo 的 id 静默返回成功。
    fetch_config(id)?;
    rpc_online("schema.resetConfig", json!({ "id": id }))?;
    rpc_online("schema.invalidate", json!({ "id": id }))?;
    println!("✓ {id}: 已清除全部定制，恢复方案文件默认");
    Ok(0)
}

fn cmd_rebuild() -> anyhow::Result<i32> {
    let v = rpc_online("schema.rebuildCache", json!({}))?;
    let removed = v.get("removed").and_then(Value::as_u64).unwrap_or(0);
    let failed = v.get("failed").and_then(Value::as_u64).unwrap_or(0);
    if failed > 0 {
        println!(
            "✓ 已清除 {removed} 个缓存文件（{failed} 个仍被占用，可稍后再次执行 rebuild 清理）"
        );
    } else {
        println!("✓ 已清除 {removed} 个缓存文件");
    }
    println!("各方案将在下次使用时按源重新生成（首次可能稍慢）");
    Ok(0)
}

/// 词库行的启用状态：主库恒开；扩展库直接用引擎的判定 [`DictSpec::is_enabled`]。
///
/// 不在这里另写一份：此前手抄的版本把「enabled 与 default_enabled 都没写」当启用，引擎却当
/// 关闭——自动识别的扩展库正是这种形态，CLI 会把一张没开的库报成「启用」。
fn dict_status(d: &Value) -> &'static str {
    let spec: wind_config::schema::DictSpec = serde_json::from_value(d.clone()).unwrap_or_default();
    if spec.default {
        "主库"
    } else if spec.is_enabled() {
        "启用"
    } else {
        "停用"
    }
}

fn cmd_dict_list(id: &str) -> anyhow::Result<i32> {
    let cfg = fetch_config(id)?;
    let Some(dicts) = cfg.get("dictionaries").and_then(Value::as_array) else {
        println!("方案 {id} 未声明词库");
        return Ok(0);
    };
    for d in dicts {
        let did = d.get("id").and_then(Value::as_str).unwrap_or("?");
        let label = d.get("label").and_then(Value::as_str).unwrap_or("");
        println!("{:<4} {did:<16} {label}", dict_status(d));
    }
    Ok(0)
}

/// 批量启用/停用一个方案的多个词库。取一次配置整体校验（词库须存在、主库
/// 不可停用），任一非法即中止不做部分应用，再逐个走 `schema.setDictEnabled`。
fn cmd_dict_toggle(id: &str, dict_ids: &[String], enable: bool) -> anyhow::Result<i32> {
    let cfg = fetch_config(id)?;
    let dicts: Vec<&Value> = cfg
        .get("dictionaries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let find = |did: &str| {
        dicts
            .iter()
            .find(|d| d.get("id").and_then(Value::as_str) == Some(did))
            .copied()
    };
    // 先整体校验：拼错的词库 id 会静默写进 override，故须逐个确认存在；
    // 主库结构上是方案的编码来源，停用等于废掉方案，一律拒绝。
    for did in dict_ids {
        let Some(d) = find(did) else {
            anyhow::bail!(
                "方案 {id} 无此词库: {did}（用 `wind_input schema dict list {id}` 查看）"
            );
        };
        if !enable && d.get("default").and_then(Value::as_bool).unwrap_or(false) {
            anyhow::bail!("{did} 是方案 {id} 的主库，不可停用");
        }
    }
    let verb = if enable { "启用" } else { "停用" };
    for did in dict_ids {
        let r = rpc_online(
            "schema.setDictEnabled",
            json!({ "id": id, "dictId": did, "enabled": enable }),
        )?;
        let live = r.get("live").and_then(Value::as_bool).unwrap_or(false);
        let note = if live {
            "（已即时生效）"
        } else {
            "（方案未加载，下次使用时生效）"
        };
        println!("✓ {id}: 词库 {did} 已{verb}{note}");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 三态与引擎一致：两个开关都没写 ⇒ 停用（自动识别的扩展库正是这种形态）。
    #[test]
    fn dict_status_matches_engine_semantics() {
        assert_eq!(dict_status(&json!({ "id": "m", "default": true })), "主库");
        assert_eq!(dict_status(&json!({ "id": "dir:a" })), "停用");
        assert_eq!(
            dict_status(&json!({ "id": "x", "default_enabled": true })),
            "启用"
        );
        assert_eq!(
            dict_status(&json!({ "id": "x", "default_enabled": true, "enabled": false })),
            "停用"
        );
        assert_eq!(dict_status(&json!({ "id": "x", "enabled": true })), "启用");
    }
}
