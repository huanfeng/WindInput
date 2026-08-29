//! `wind_input config ...` 命令行：配置的查看/读写/导入导出。
//!
//! - list/describe/get/export 纯本地（registry + Config，无需运行中的 core）。
//! - set/import 优先经 RPC 发给运行中的 core（即时热重载），连不上则离线直写
//!   用户配置文件（下次启动生效）。
//! - 写入前一律按 config_schema 注册表校验（未知键/类型/枚举越界即拒绝）。

use serde_json::{Value, json};
use wind_config::Config;
use wind_config::config_schema::{
    FieldType, field, is_known_key, leaf_entries, parse_str_value, registry, validate,
};

// 变体后缀经 wind_config::variant::pipe_suffix() 运行时取得：CLI 与 core 同一 exe，自辨一致。

/// 子命令入口。`args` 为 `config` 之后的参数。返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => cmd_list(args.get(1).map(String::as_str)),
        Some("describe") | Some("desc") => match args.get(1) {
            Some(key) => cmd_describe(key),
            None => usage_err("describe <key>"),
        },
        Some("get") => match args.get(1) {
            Some(key) => cmd_get(key),
            None => usage_err("get <key>"),
        },
        Some("set") => match (args.get(1), args.get(2)) {
            (Some(key), Some(raw)) => cmd_set(key, raw),
            _ => usage_err("set <key> <value>"),
        },
        Some("export") => cmd_export(),
        Some("import") => match args.get(1) {
            Some(path) => cmd_import(path),
            None => usage_err("import <file.toml>"),
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
        "用法: wind_input config <命令>\n\
         \n\
         命令:\n  \
         list [前缀]          列出配置键与类型（可按键前缀过滤）\n  \
         describe <key>       显示某键的类型/可选值/当前值\n  \
         get <key>            读取某键当前值\n  \
         set <key> <value>    设置某键（优先热重载，core 未运行则离线写）\n  \
         export               导出当前完整配置（TOML）\n  \
         import <file.toml>   从 TOML 文件批量导入"
    );
}

fn usage_err(form: &str) -> i32 {
    eprintln!("用法: wind_input config {form}");
    2
}

fn cmd_list(prefix: Option<&str>) -> i32 {
    for fld in registry() {
        if let Some(p) = prefix
            && !fld.key.starts_with(p)
        {
            continue;
        }
        println!("{:<48} {}", fld.key, type_label(fld.ty));
    }
    0
}

fn cmd_describe(key: &str) -> i32 {
    let Some(fld) = field(key) else {
        eprintln!("未登记的配置键: {key}");
        return 1;
    };
    println!("键:     {key}");
    println!("类型:   {}", type_label(fld.ty));
    if let FieldType::Enum(vals) = fld.ty {
        println!("可选值: {}", vals.join(" | "));
    }
    match load_value(key) {
        Ok(v) => println!("当前值: {}", format_value(&v)),
        Err(e) => println!("当前值: <读取失败: {e}>"),
    }
    0
}

fn cmd_get(key: &str) -> i32 {
    if !is_known_key(key) {
        eprintln!("未登记的配置键: {key}");
        return 1;
    }
    match load_value(key) {
        Ok(v) => {
            println!("{}", format_value(&v));
            0
        }
        Err(e) => {
            eprintln!("读取失败: {e}");
            1
        }
    }
}

fn cmd_set(key: &str, raw: &str) -> i32 {
    let value = match parse_value(key, raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("无法设置 '{key}': {e}");
            return 1;
        }
    };
    if let Err(e) = validate(key, &value) {
        eprintln!("无法设置 '{key}': {e}");
        return 1;
    }
    apply_items(vec![(key.to_string(), value)])
}

fn cmd_export() -> i32 {
    match Config::load(Config::data_dir().as_deref()) {
        Ok(cfg) if cfg.degradation.is_degraded() => {
            // ⛔ 降级过就**拒绝导出**，宁可什么都不给。
            //
            // 导出产物的用途是备份和 `config export > config.toml` 回写，而降级后的配置里
            // 坏段已经被出厂值顶掉——导出去就是把这次数据丢失**固化**成用户的新配置，
            // 而且他从输出里完全看不出来。同 `preset_for_pruning` 取不到 preset 时退化为
            // 「不清理」：拿不到可信的全量就别动。
            eprintln!(
                "拒绝导出：本次加载有配置段解析失败并回落了出厂默认值，导出的内容不是你的真实配置。"
            );
            if cfg.degradation.total_fallback {
                eprintln!("  受影响：整份配置（无法定位到具体段）");
            } else {
                eprintln!("  受影响的段：{}", cfg.degradation.sections.join(", "));
            }
            eprintln!(
                "  请先修正配置文件里这些段的坏键（日志中有 WARN 记录了具体错误），再重新导出。"
            );
            // 用 1（操作失败）而非 2：本 CLI 里 2 是**用法错误**（未知子命令、参数缺失），
            // 而这是「用法没问题，但拒绝执行」。
            1
        }
        Ok(cfg) => match toml::to_string_pretty(&cfg) {
            Ok(s) => {
                print!("{s}");
                0
            }
            Err(e) => {
                eprintln!("序列化失败: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("加载配置失败: {e}");
            1
        }
    }
}

fn cmd_import(path: &str) -> i32 {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读取 {path} 失败: {e}");
            return 1;
        }
    };
    let root: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("解析 TOML 失败: {e}");
            return 1;
        }
    };
    let entries = leaf_entries(&root);
    if entries.is_empty() {
        eprintln!("文件无可导入的配置项");
        return 1;
    }
    // 全量校验，任一项不合法即整体中止（不部分写入）。
    let mut errors = Vec::new();
    for (k, v) in &entries {
        if let Err(e) = validate(k, v) {
            errors.push(format!("  {k}: {e}"));
        }
    }
    if !errors.is_empty() {
        eprintln!(
            "导入中止，{} 项不合法:\n{}",
            errors.len(),
            errors.join("\n")
        );
        return 1;
    }
    apply_items(entries)
}

/// 把若干 `(key, value)` 写入配置：优先 RPC 让运行中的 core 即时热重载；
/// 连不上则离线直写用户配置文件。
fn apply_items(items: Vec<(String, toml::Value)>) -> i32 {
    let json_items: Vec<Value> = items
        .iter()
        .filter_map(|(k, v)| {
            serde_json::to_value(v)
                .ok()
                .map(|jv| json!({ "key": k, "value": jv }))
        })
        .collect();

    match wind_rpc::client::call(
        wind_config::variant::pipe_suffix(),
        "config.setItems",
        json!({ "items": json_items }),
    ) {
        Ok(res) => {
            let restart = res
                .get("needsRestart")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let applied = res
                .get("applied")
                .and_then(Value::as_u64)
                .unwrap_or(items.len() as u64);
            // 正常情况 CLI 已预校验，skipped 应为空；防御性呈现 core 的跳过项。
            let skipped = res
                .get("skipped")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for s in &skipped {
                let k = s.get("key").and_then(Value::as_str).unwrap_or("?");
                let reason = s.get("reason").and_then(Value::as_str).unwrap_or("");
                eprintln!("⚠ 跳过 {k}: {reason}");
            }
            let note = if restart {
                "（需重启 core 完全生效）"
            } else {
                "（已热重载）"
            };
            println!("✓ 已应用 {applied} 项{note}");
            // 全部被跳过（一个都没应用）视为失败。
            if applied == 0 && !skipped.is_empty() {
                1
            } else {
                0
            }
        }
        Err(_) => {
            // core 未运行：离线直写，下次启动生效。
            for (k, v) in &items {
                let parts: Vec<&str> = k.split('.').collect();
                if let Err(e) = Config::set_user_value(&parts, v.clone()) {
                    eprintln!("写入 {k} 失败: {e}");
                    return 1;
                }
            }
            println!("✓ 已写入 {} 项（core 未运行，下次启动生效）", items.len());
            0
        }
    }
}

/// 读取某键的当前值（四层合并后），转为 JSON。
fn load_value(key: &str) -> anyhow::Result<Value> {
    let cfg = Config::load(Config::data_dir().as_deref())?;
    let full = serde_json::to_value(cfg)?;
    let mut cur = &full;
    for part in key.split('.') {
        cur = cur
            .get(part)
            .ok_or_else(|| anyhow::anyhow!("配置缺少键 {key}"))?;
    }
    Ok(cur.clone())
}

/// 按注册表类型把命令行原始字符串解析为 TOML 值（下沉共享实现，cmdbar 同用）。
fn parse_value(key: &str, raw: &str) -> Result<toml::Value, String> {
    parse_str_value(key, raw)
}

/// 类型的可读标签。
fn type_label(ty: FieldType) -> String {
    match ty {
        FieldType::Bool => "bool".into(),
        FieldType::Int => "int".into(),
        FieldType::Float => "float".into(),
        FieldType::Str => "string".into(),
        FieldType::Enum(vals) => format!("enum({})", vals.join("|")),
        FieldType::StrList => "string[]".into(),
        FieldType::Map(_) => "map".into(),
        FieldType::StructList => "array".into(),
    }
}

/// 显示一个 JSON 值：字符串去引号，其余按紧凑 JSON。
fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_value_typed_by_registry() {
        assert_eq!(
            parse_value("ui.candidate.per_page", "9").unwrap(),
            toml::Value::Integer(9)
        );
        assert_eq!(
            parse_value("ui.candidate.hide_window", "true").unwrap(),
            toml::Value::Boolean(true)
        );
        assert_eq!(
            parse_value("ui.candidate.hide_window", "off").unwrap(),
            toml::Value::Boolean(false)
        );
        assert_eq!(
            parse_value("ui.candidate.font_size", "18").unwrap(),
            toml::Value::Float(18.0)
        );
        assert_eq!(
            parse_value("ui.candidate.layout", "vertical").unwrap(),
            toml::Value::String("vertical".into())
        );
        // 字符串列表按逗号拆分
        let list = parse_value("schema.available", "wubi86, wubi86_pinyin").unwrap();
        assert_eq!(
            list,
            toml::Value::Array(vec![
                toml::Value::String("wubi86".into()),
                toml::Value::String("wubi86_pinyin".into()),
            ])
        );
    }

    #[test]
    fn parse_value_rejects_bad_input() {
        assert!(parse_value("ui.candidate.per_page", "seven").is_err());
        assert!(parse_value("ui.candidate.hide_window", "maybe").is_err());
        assert!(parse_value("no.such.key", "x").is_err());
    }

    #[test]
    fn parse_value_enum_passes_raw_then_validate_catches_range() {
        // parse 不校验枚举成员（交给 validate）；越界值先解析成字符串
        let v = parse_value("ui.candidate.layout", "diagonal").unwrap();
        assert_eq!(v, toml::Value::String("diagonal".into()));
        assert!(validate("ui.candidate.layout", &v).is_err());
    }

    #[test]
    fn format_value_unquotes_string() {
        assert_eq!(format_value(&json!("vertical")), "vertical");
        assert_eq!(format_value(&json!(7)), "7");
        assert_eq!(format_value(&json!(true)), "true");
    }
}
