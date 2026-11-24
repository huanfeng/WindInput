//! `userDicts.*`：用户分库（设计 `user-dict-libraries-and-external-edit.md` §3）的列表、条目、
//! 增删改名、导入与开关。文件操作在 `wind_store::user_dicts`，开关与 `schema.setDictEnabled`
//! 同一实现；改了文件或开关都失效用到该数据方案的方案（同 `dict.reload` 的 `dicts:<数据方案>`）。

use super::{
    WebDataRpc, batch_done, code_text_item, items_or_single, parse_sort, resolve_engine_type,
    store_text, str_param, ui_text,
};
use serde_json::{Value, json};
use wind_store::user_dicts::ID_PREFIX;
use wind_store::user_words::USER_DICTS_CHANGED;

/// 方案 id、它的数据方案（拼音族折叠到 `pinyin`）与 store。混输没有自己的分库（随成员方案走）。
fn target<'a, H: WebDataRpc + ?Sized>(
    h: &'a H,
    params: &'a Value,
) -> anyhow::Result<(&'a str, String, &'a wind_store::Store)> {
    let schema_id = str_param(params, "schemaId")?;
    let s = h
        .engine_mgr()
        .schema_merged(schema_id)
        .ok_or_else(|| anyhow::anyhow!("方案不存在: {schema_id}"))?;
    if resolve_engine_type(&s) == "mixed" {
        anyhow::bail!("混输方案没有自己的分库，请在它的码表 / 拼音方案下管理");
    }
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    Ok((schema_id, h.engine_mgr().data_schema_id(schema_id), store))
}

/// 写操作的前置：只读模式拒绝。
fn writable<'a, H: WebDataRpc + ?Sized>(
    h: &'a H,
    params: &'a Value,
) -> anyhow::Result<(&'a str, String, &'a wind_store::Store)> {
    if let Some(r) = h.storage_readonly_reason() {
        anyhow::bail!("{r}");
    }
    target(h, params)
}

fn changed<H: WebDataRpc + ?Sized>(h: &H, ds: &str) {
    h.apply_user_text_changes(&[format!("{USER_DICTS_CHANGED}{ds}")]);
}

/// `userDicts.list {schemaId}` → `[{name, label, enabled, entries, file, issues?}]`，按名排序。
pub(super) fn list<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (schema_id, ds, store) = target(h, params)?;
    let items: Vec<Value> = store
        .list_user_dicts(&ds, schema_id)?
        .into_iter()
        .map(|l| {
            let mut v = json!({
                "name": l.name,
                "label": l.label,
                "enabled": l.enabled,
                "entries": l.entries,
                "file": l.file,
            });
            if l.unreadable {
                v["issues"] = json!([{ "kind": "isolated" }]);
            } else if l.bad_lines > 0 {
                v["issues"] = json!([{ "kind": "bad_lines", "lines": l.bad_lines }]);
            }
            v
        })
        .collect();
    Ok(Value::Array(items))
}

/// `userDicts.entries {schemaId, name, offset, limit, query?, sortBy?, sortOrder?}` →
/// `{items: [{code, text, weight, enabled}], total}`（同 `dict.listPaged`）。
pub(super) fn entries<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (_, ds, store) = target(h, params)?;
    let name = str_param(params, "name")?;
    let all = store.user_dict_entries(&ds, name)?;
    let query = params
        .get("query")
        .or_else(|| params.get("prefix"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let limit = super::usize_param(params, "limit", 50);
    let offset = super::usize_param(params, "offset", 0);
    // 同 `dict.listPaged`：编码前缀命中在前，词条内容 / 编码中段包含在后
    let mut rows = if query.is_empty() {
        all
    } else {
        let (code_q, _) = wind_store::wdict::split_spaced_code(query);
        let code_q = code_q.to_lowercase();
        let q = query.to_lowercase();
        let (mut head, tail): (Vec<_>, Vec<_>) = all
            .into_iter()
            .filter(|e| {
                e.code.to_lowercase().contains(&code_q) || e.text.to_lowercase().contains(&q)
            })
            .partition(|e| e.code.to_lowercase().starts_with(&code_q));
        head.extend(tail);
        head
    };
    if let Some((by, desc)) = parse_sort(params, &["code", "text", "weight"]) {
        rows.sort_by(|a, b| {
            let o = match by {
                "weight" => a.weight.cmp(&b.weight),
                "text" => a.text.cmp(&b.text),
                _ => a.code.cmp(&b.code),
            };
            if desc { o.reverse() } else { o }
        });
    }
    let total = rows.len();
    let items: Vec<Value> = rows
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|e| {
            json!({
                "code": wind_store::wdict::join_code_by_boundary(&e.code, e.boundary),
                "text": ui_text(&e.text),
                "weight": e.weight,
                "enabled": true,
            })
        })
        .collect();
    Ok(json!({ "items": items, "total": total }))
}

/// `userDicts.removeEntry {schemaId, name, code, text}` → `{ok, removed}`。
/// 带 `items`（`[{code, text}]`）时一次补丁删一批 → `{ok, count}`。
pub(super) fn remove_entry<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (_, ds, store) = writable(h, params)?;
    let name = str_param(params, "name")?;
    let (keys, batch) = items_or_single(params, code_text_item)?;
    if keys.is_empty() {
        return Ok(batch_done(0));
    }
    let removed = store.remove_user_dict_entries(&ds, name, &keys)?;
    if removed {
        changed(h, &ds);
    }
    Ok(if batch {
        batch_done(keys.len())
    } else {
        json!({ "ok": true, "removed": removed })
    })
}

/// `userDicts.addEntry {schemaId, name, text, code, weight?}` → `{ok}`。按 (词, 码) 去重：已有的
/// 改成给的权重，没给权重就不动；新词没给权重时权重列留空（按方案默认）。
/// 带 `items`（`[{code, text, weight?}]`）时整批校验、一次补丁 → `{ok, count}`。
pub(super) fn add_entry<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (schema_id, ds, store) = writable(h, params)?;
    let name = str_param(params, "name")?;
    let (raw, batch) = items_or_single(params, |p| {
        let text = store_text(str_param(p, "text")?);
        let code = str_param(p, "code")?.to_string();
        let weight = match p.get("weight") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_i64()
                    .and_then(|w| i32::try_from(w).ok())
                    .ok_or_else(|| anyhow::anyhow!("权重须是整数：{v}"))?,
            ),
        };
        Ok((code, text, weight))
    })?;
    if raw.is_empty() {
        return Ok(batch_done(0));
    }
    // 拼音码同加词界面：ü 归一、补音节边界（码表原样）。带制表符 / 换行的码不规范化，留给
    // 存储层的校验报错。
    let plain: Vec<(&str, &str)> = raw
        .iter()
        .filter(|(c, _, _)| !c.contains(['\t', '\n', '\r']))
        .map(|(c, t, _)| (c.as_str(), t.as_str()))
        .collect();
    let mut normalized =
        wind_coordinator::user_words_api::normalize_add_codes(h, schema_id, &plain).into_iter();
    let rows: Vec<(String, String, u64, Option<i32>)> = raw
        .into_iter()
        .map(|(code, text, weight)| {
            let (code, boundary) = if code.contains(['\t', '\n', '\r']) {
                (code, 0)
            } else {
                normalized.next().expect("与 plain 同序等长")
            };
            (code, text, boundary, weight)
        })
        .collect();
    if store.add_user_dict_entries(&ds, name, &rows)? {
        changed(h, &ds);
    }
    Ok(if batch {
        batch_done(rows.len())
    } else {
        json!({ "ok": true })
    })
}

/// `userDicts.clear {schemaId, name}` → `{ok, removed}`：删光词条，保留文件头。
pub(super) fn clear<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (_, ds, store) = writable(h, params)?;
    let removed = store.clear_user_dict(&ds, str_param(params, "name")?)?;
    if removed > 0 {
        changed(h, &ds);
    }
    Ok(json!({ "ok": true, "removed": removed }))
}

/// `userDicts.create {schemaId, name}` → `{ok, file}`。
pub(super) fn create<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (_, ds, store) = writable(h, params)?;
    let name = str_param(params, "name")?;
    store.create_user_dict(&ds, name)?;
    changed(h, &ds);
    Ok(
        json!({ "ok": true, "file": format!("{ds}/{}/{name}.dict.yaml", wind_store::user_dicts::LIBS_DIR) }),
    )
}

/// `userDicts.rename {schemaId, name, newName}` → `{ok}`。
pub(super) fn rename<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (_, ds, store) = writable(h, params)?;
    let r = store.rename_user_dict(
        &ds,
        str_param(params, "name")?,
        str_param(params, "newName")?,
    );
    // 失败也失效：文件可能已改名、只是后面同步开关时出错，引擎不能留着旧路径
    changed(h, &ds);
    r?;
    Ok(json!({ "ok": true }))
}

/// `userDicts.delete {schemaId, name}` → `{ok}`。
pub(super) fn delete<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (_, ds, store) = writable(h, params)?;
    let r = store.delete_user_dict(&ds, str_param(params, "name")?);
    // 失败也失效：文件可能已删、只是后面改开关时出错，引擎不能拿着已删文件的旧数据
    changed(h, &ds);
    r?;
    Ok(json!({ "ok": true }))
}

/// `userDicts.importAs {schemaId, name, path | content}` → `{ok, imported, skipped, format}`。
/// 第三方 / 自有文件（Rime、TSV、WindDict 的 words 段）转成分库的 Rime 格式整写。纯词表
/// （无编码）先经 `dict.encodeWords` 出码、拼成 TSV 再以 `content` 传进来。
pub(super) fn import_as<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let (schema_id, ds, store) = writable(h, params)?;
    let name = str_param(params, "name")?;
    wind_store::user_dicts::validate_name(name)?;
    let content = super::dict_source_text(params)?;
    let content = content.as_ref();
    let (fmt, rows, skipped) = wind_store::import_formats::parse_words_auto(
        content,
        super::dict_transfer_api::code_policy_for(h, schema_id),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    if fmt == wind_store::import_formats::DictFormat::WindDict
        && let Some(src) = wind_store::wdict::read_header_field(content, "engine_type")
        && !src.is_empty()
        && Some(src.as_str())
            != h.engine_mgr()
                .schema_merged(schema_id)
                .as_ref()
                .map(resolve_engine_type)
    {
        anyhow::bail!("该文件的词库类型与当前方案不一致，导入会导致编码错乱，已阻止。");
    }
    let parsed = rows.len();
    let (rows, st) =
        super::dict_transfer_api::apply_pinyin_entry_contract(h, schema_id, rows, true);
    let imported = store.import_user_dict(&ds, name, &rows)?;
    changed(h, &ds);
    Ok(json!({
        "ok": true,
        "imported": imported,
        "skipped": skipped + (parsed - imported),
        "format": fmt.as_str(),
        "rejected": st.rejected,
    }))
}

/// `userDicts.setEnabled {schemaId, name, enabled}`：`schema.setDictEnabled` 的薄封装。
pub(super) fn set_enabled<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let p = json!({
        "id": str_param(params, "schemaId")?,
        "dictId": format!("{ID_PREFIX}{}", str_param(params, "name")?),
        "enabled": params.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
    });
    h.web_schema_set_dict_enabled(&p)
}

#[cfg(test)]
#[path = "user_dicts_tests.rs"]
mod tests;
