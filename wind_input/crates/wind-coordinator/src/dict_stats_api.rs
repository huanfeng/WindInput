//! 系统词库统计（`dict.systemStats`）：设置页「词库统计」对话框的数据源。
//!
//! 计算在 `EngineManager::dict_stats`（选库 / 缓存）与 `wind_dict::stats`（纯函数）；这里只
//! 补两件引擎够不着的事，并照 [`crate::schema_dicts_api`] 的做法放在协调器、webdata 只转发
//! ——移动端将来要开这个口，不必再抄一份：
//!
//! - **混输**没有自己的系统词库，按 `[engine.mixed]` 的主码表 / 拼音两个成员各统计一份，
//!   放进 `members` 并排给出；
//! - **用户词 / 临时词**是实时计数（与 `dict.stats` 同口径，走 `data_schema_id`），不进缓存。

use serde_json::{Value, json};

use crate::web_host::WebDataHost;

/// 回包里重码 Top 列表的上限（客户端要多少给多少，但不超过它）。
pub const TOP_N_MAX: usize = 200;

/// 统计 `schema_id` 的系统词库。`top_n` 超过 [`TOP_N_MAX`] 按上限算；`refresh` 绕过缓存。
///
/// 回包：码表 / 拼音 / 英文方案即 `SystemDictStats` 的 JSON 形态，另加 `cached` 与
/// `user {userWords, tempWords}`（无用户数据库时 `user` 为 null）；混输方案为
/// `{schemaId, schemaName, engineType: "mixed", members: [成员回包…]}`。
pub fn system_stats<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    top_n: usize,
    refresh: bool,
) -> anyhow::Result<Value> {
    let mgr = h.engine_mgr();
    let ty = mgr
        .schema_engine_type(schema_id)
        .ok_or_else(|| anyhow::anyhow!("方案不存在: {schema_id}"))?;
    let top_n = top_n.min(TOP_N_MAX);
    if ty != "mixed" {
        return one(h, schema_id, top_n, refresh);
    }
    let members: Vec<String> = [
        mgr.mixed_primary_schema(schema_id),
        mgr.mixed_secondary_schema(schema_id),
    ]
    .into_iter()
    .flatten()
    .collect();
    if members.is_empty() {
        anyhow::bail!("混输方案 {schema_id} 没有配置成员方案");
    }
    let members = members
        .iter()
        .map(|m| one(h, m, top_n, refresh))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(json!({
        "schemaId": schema_id,
        "schemaName": mgr.schema_name(schema_id),
        "engineType": "mixed",
        "members": members,
    }))
}

fn one<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    top_n: usize,
    refresh: bool,
) -> anyhow::Result<Value> {
    let (stats, cached) = h.engine_mgr().dict_stats(schema_id, top_n, refresh)?;
    let mut v = serde_json::to_value(&*stats)?;
    let user = h.user_store().map(|store| {
        // 与 `dict.stats` 同口径：拼音族折叠到同一个数据方案，只要条数就用 count_*。
        let ds = h.engine_mgr().data_schema_id(schema_id);
        json!({
            "userWords": store.count_user_words(&ds).unwrap_or(0),
            "tempWords": store.count_temp_words(&ds).unwrap_or(0),
        })
    });
    if let Value::Object(o) = &mut v {
        o.insert("cached".into(), json!(cached));
        o.insert("user".into(), user.unwrap_or(Value::Null));
    }
    Ok(v)
}
