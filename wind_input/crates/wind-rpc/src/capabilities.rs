//! Capability descriptor 生成：从 config_schema::REGISTRY + 系统预置配置
//! （Config::default ⊕ data/config.toml，不含用户层）动态生成 core 能力清单，
//! 经 system.capabilities 暴露给 setting。取代退役的 manifest.toml / manifest.rs。

use std::collections::BTreeMap;
use std::path::Path;

use wind_config::Config;
use wind_config::config_schema::{self, FieldType};

/// FieldType → capability type 字符串。
fn type_name(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Bool => "bool",
        FieldType::Int => "int",
        FieldType::Float => "float",
        FieldType::Str => "str",
        // ★ 可分档枚举对设置端**仍是 enum**：值域一样、单值写法一样，只是额外允许
        // 分档写法。报成一个新 type 会让设置端既有的 select 控件与清单校验全部落空，
        // 而它们本来就该继续工作 —— 分档能力另由 `byLayout` 标记位表达。
        FieldType::Enum(_) | FieldType::LayoutEnum(_) => "enum",
        FieldType::StrList => "strlist",
        FieldType::Map(_) => "map",
        FieldType::StructList => "structlist",
    }
}

/// 按点分键取 TOML 子值。
fn lookup_path<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.').try_fold(root, |v, seg| v.get(seg))
}

/// 生成 capability descriptor JSON。
///
/// `data_dir` 用于读取系统预置 `data/config.toml`（L2）。**定制层 L2.5
/// （`data_custom/config.toml`）由 `system_preset_value` 自行接上，不经本参数**——
/// 故 `data_dir = None` 时得到的是 L1⊕L2.5，在定制版上并非纯 L1 代码默认。
///
/// ⚠️ 由此，定制版设置页显示的 `default`（以及「恢复默认」的落点）是**定制默认值**。
/// 这是正确语义，不要当 bug 改——见 `Config::system_preset_value` 的文档。
pub fn generate(data_dir: Option<&Path>) -> anyhow::Result<serde_json::Value> {
    let preset = Config::system_preset_value(data_dir)?;
    let leaves: BTreeMap<String, toml::Value> =
        config_schema::leaf_entries(&preset).into_iter().collect();

    let mut config_keys = Vec::new();
    for f in config_schema::registry() {
        let mut entry = serde_json::Map::new();
        entry.insert("key".into(), serde_json::Value::String(f.key.to_string()));
        entry.insert(
            "type".into(),
            serde_json::Value::String(type_name(f.ty).to_string()),
        );
        // `values` = 这个键的**受限词表**，由 `type` 决定读法：
        // `enum` → 合法取值；`map` → 合法**键名**（值仍自由）。键名域为空的 map 不带此字段，
        // 设置端据此区分「自由命名的表」（自定义标点）与「类别固定的表」（字体脚本类）。
        let restricted: Option<&[&str]> = match f.ty {
            FieldType::Enum(allowed) | FieldType::LayoutEnum(allowed) => Some(allowed),
            FieldType::Map(keys) if !keys.is_empty() => Some(keys),
            _ => None,
        };
        // `byLayout` = 这个键的值可以按候选窗排布分档（`"h:hide v:always"`，见 core 的
        // `by_layout` 模块）。设置端据此决定给一个控件还是横竖两个。
        //
        // 与 `values` / `schemaOverride` 同风格：**不适用的键不带这个字段**（而不是给
        // false），设置端 `if let Some` 即可。
        if matches!(f.ty, FieldType::LayoutEnum(_)) {
            entry.insert("byLayout".into(), serde_json::Value::Bool(true));
        }
        if let Some(allowed) = restricted {
            entry.insert(
                "values".into(),
                serde_json::Value::Array(
                    allowed
                        .iter()
                        .map(|s| serde_json::Value::String(s.to_string()))
                        .collect(),
                ),
            );
        }
        // `map` 键整表就是一个值：出厂表非空时 `leaf_entries` 会下钻到 `<key>.<id>`，按键名
        // 查叶子就落空（`input.emoji.modules` 即此例），故直接按路径取子表。
        let value = match f.ty {
            FieldType::Map(_) => lookup_path(&preset, f.key),
            _ => leaves.get(f.key),
        };
        let default = match value {
            Some(v) => serde_json::to_value(v)?,
            None => serde_json::Value::Null,
        };
        entry.insert("default".into(), default);
        // `schemaOverride` = 这个全局项可被方案级配置覆盖，值是给用户看的一句话 + 方案侧段名。
        //
        // ★ 走快照而不是运行时查询：能不能被方案覆盖是**静态**的配置模型属性，与用户机器上
        // 有什么文件无关。设置页据此给那些行标一个提示图标，零额外 RPC、零读盘。
        // 不可覆盖的键**不带这个字段**（而不是给 null）：设置端 `if let Some` 即可，
        // 与同为可选的 `values` 一致。
        if let Some(o) = config_schema::schema_override_of(f.key) {
            entry.insert(
                "schemaOverride".into(),
                serde_json::json!({ "section": o.section, "note": o.note }),
            );
        }
        config_keys.push(serde_json::Value::Object(entry));
    }

    Ok(serde_json::json!({
        "appVersion": crate::APP_VERSION,
        "configKeys": config_keys,
    }))
}

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod tests;
