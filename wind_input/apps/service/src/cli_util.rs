//! CLI 子命令共享工具：RPC 调用（区分「core 未运行」与远端错误）、
//! JSON 点路径读写、按现值类型转换命令行字符串。
//!
//! `config` 之外的子命令（schema/dict/phrase/backup）统一**仅在线**：
//! 它们操作 redb 单写者库或 coordinator 内的 override 合并逻辑，离线直写
//! 会与运行中实例冲突，故连不上 core 一律报错退出（区别于 config 的离线降级）。

use serde_json::Value;

/// 展开路径里的 `${VAR}` 内部目录变量（变量集见 [`wind_config::dir_var`]）。`resolve`
/// 注入变量→目录字符串的映射，便于测试；未知变量或未闭合 `${` 一律报错，
/// 不静默留字面量——否则脚本会把文件写到诸如 `${USER_DATA}\x` 这种字面目录。
///
/// **与命令栏词法层的处置刻意不同**：那边未知 `${NAME}` 原样保留字面（短语文本里
/// `${YC}` 等旧模板变量合法存在，报错会整条丢候选）；这边是文件路径参数，留字面
/// 就是静默写坏位置，必须硬失败。两处策略勿"统一"。
fn expand_with(input: &str, resolve: impl Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            anyhow::bail!("路径变量未闭合（缺 `}}`）: {input}");
        };
        let name = &after[..end];
        let dir = resolve(name).ok_or_else(|| {
            anyhow::anyhow!(
                "未知路径变量 ${{{name}}}（支持: {}）",
                wind_config::dir_var_help()
            )
        })?;
        out.push_str(&dir);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 展开内部目录变量并转为绝对路径。
///
/// 备份/词库/短语等命令的文件参数统一经此解析：支持 `${APP_DIR}` /
/// `${USER_DATA}` / `${LOCAL_DATA}` 三个内部目录变量后再按当前终端工作目录
/// 绝对化。backup 的文件读写在 core 进程内完成，必须传绝对路径（否则按 core
/// 的工作目录解析）；dict/phrase 在 CLI 侧读写，绝对化亦无害且保持一致。
pub fn resolve_path(input: &str) -> anyhow::Result<String> {
    let expanded = expand_with(input, wind_config::dir_var_str)?;
    let abs =
        std::path::absolute(&expanded).map_err(|e| anyhow::anyhow!("路径 {expanded} 无效: {e}"))?;
    Ok(abs.to_string_lossy().into_owned())
}

/// RPC 调用失败的分类：连接失败（core 未运行）与远端错误须给用户不同提示。
pub enum RpcFailure {
    /// 控制通道不可达（core 未运行 / 管道打开或收发失败）。原始错误对用户
    /// 无行动价值（一律「请先启动输入法」），不携带。
    Offline,
    /// core 收到请求但返回错误文本。
    Remote(anyhow::Error),
}

/// 发一条控制通道 RPC。连接类失败（错误链含 `io::Error`）归为 [`RpcFailure::Offline`]。
pub fn rpc(method: &str, params: Value) -> Result<Value, RpcFailure> {
    match wind_rpc::client::call(wind_config::variant::pipe_suffix(), method, params) {
        Ok(v) => Ok(v),
        Err(e) => {
            if e.chain().any(|c| c.is::<std::io::Error>()) {
                Err(RpcFailure::Offline)
            } else {
                Err(RpcFailure::Remote(e))
            }
        }
    }
}

/// 仅在线调用：连不上 → 统一话术；远端错误原样透传。
pub fn rpc_online(method: &str, params: Value) -> anyhow::Result<Value> {
    match rpc(method, params) {
        Ok(v) => Ok(v),
        Err(RpcFailure::Offline) => {
            anyhow::bail!("core 未运行（此命令需要输入法服务在线，请先启动输入法）")
        }
        Err(RpcFailure::Remote(e)) => Err(e),
    }
}

/// 按 `a.b.c` 点路径下钻只读取值。不支持数组下标。
pub fn json_get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for part in path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

/// 按点路径写入值；沿途任一段缺失或终点父级不是对象则失败返回 false。
pub fn json_set_path(root: &mut Value, path: &str, new: Value) -> bool {
    let parts: Vec<&str> = path.split('.').collect();
    let Some((last, walk)) = parts.split_last() else {
        return false;
    };
    let mut cur = root;
    for part in walk {
        let Some(next) = cur.get_mut(*part) else {
            return false;
        };
        cur = next;
    }
    match cur {
        Value::Object(map) if map.contains_key(*last) => {
            map.insert((*last).to_string(), new);
            true
        }
        _ => false,
    }
}

/// 按「现有值的类型」把命令行原始字符串转换为 JSON 值。
///
/// 方案配置没有 config_schema 那样的注册表，类型信息只能取自合并后配置的现值；
/// 现值为 null 的键（未定义/Option 未设）由调用方先行拒绝。
pub fn coerce_like(current: &Value, raw: &str) -> Result<Value, String> {
    match current {
        Value::Bool(_) => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            _ => Err(format!("'{raw}' 不是布尔值（true/false）")),
        },
        Value::Number(n) if n.is_i64() || n.is_u64() => raw
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("'{raw}' 不是整数")),
        // 拒绝 nan/inf/溢出：`Value::from(非有限 f64)` 会静默变 null，落盘后键类型损坏。
        Value::Number(_) => match raw.trim().parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(Value::from(f)),
            _ => Err(format!("'{raw}' 不是有限数字")),
        },
        Value::String(_) => Ok(Value::String(raw.to_string())),
        Value::Array(_) => match serde_json::from_str::<Value>(raw) {
            Ok(v @ Value::Array(_)) => Ok(v),
            Ok(_) => Err(format!("'{raw}' 不是 JSON 数组")),
            Err(e) => Err(format!("数组值需为 JSON: {e}")),
        },
        Value::Object(_) => match serde_json::from_str::<Value>(raw) {
            Ok(v @ Value::Object(_)) => Ok(v),
            Ok(_) => Err(format!("'{raw}' 不是 JSON 对象")),
            Err(e) => Err(format!("对象值需为 JSON: {e}")),
        },
        Value::Null => Err("该键当前无值，无法推断类型".to_string()),
    }
}

/// 显示一个 JSON 值：字符串去引号，复合值 pretty，其余紧凑。
pub fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
#[path = "cli_util_tests.rs"]
mod tests;
