//! Web 设置数据 RPC：schema/dict/temp/freq/shadow/stats/theme/phrase/quick/commonChars 命名空间。
//!
//! 经 wind-rpc 的 `CoreRpc::data_rpc` 转发到此，方法名与前端 `contract.ts` 1:1 一致。
//! `schema.references` 暂返 `{}`；无 store/themes 时各方法返回合法空集（降级，不报错）。
//!
//! 只经 [`WebDataHost`] 窄面（wind-coordinator::web_host）触宿主，
//! 把 wind-transfer/fontdb 等重依赖挡在 Android 闭包之外。

mod theme_render;
pub use theme_render::set_preview_build_id;
mod user_dicts;

use serde_json::{Value, json};
use wind_coordinator::dict_transfer_api::{self, resolve_engine_type};
use wind_coordinator::external_files;
use wind_coordinator::handle_common_chars::CommonCharEdit;
use wind_coordinator::handle_quick_format::QuickFormatEdit;
use wind_coordinator::user_words_api;
use wind_coordinator::web_host::WebDataHost;
use wind_store::completion::CompletionKind;

fn str_param<'a>(p: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    p.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("缺少参数 {}", key))
}

/// 写类方法的批量形式：参数里带 `items`（数组）即走批量，缺省 / `null` 走原单条路径。
/// 批量时各条共享的参数（`schemaId`、`enabled` 等）仍在顶层，`items` 里只放每条不同的键；
/// 整批先全部解析完再一次写入，任一条不合法整批报错、什么都不改；成功回 [`batch_done`]。
fn batch_items(p: &Value) -> anyhow::Result<Option<&[Value]>> {
    match p.get("items") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => Ok(Some(a.as_slice())),
        Some(_) => anyhow::bail!("参数 items 须是数组"),
    }
}

/// 有 `items` 时逐条解析（错误带下标），否则把顶层参数当作唯一的一条。返回 `(各条, 是否批量)`。
fn items_or_single<T>(
    p: &Value,
    one: impl Fn(&Value) -> anyhow::Result<T>,
) -> anyhow::Result<(Vec<T>, bool)> {
    match batch_items(p)? {
        Some(items) => Ok((
            items
                .iter()
                .enumerate()
                .map(|(i, v)| one(v).map_err(|e| anyhow::anyhow!("items[{i}]：{e}")))
                .collect::<anyhow::Result<_>>()?,
            true,
        )),
        None => Ok((vec![one(p)?], false)),
    }
}

/// 批量形式的成功回包。
fn batch_done(count: usize) -> Value {
    json!({ "ok": true, "count": count })
}

/// 一条 `{code, text}`：text 从设置页的转义形态还原成存储域（见 [`store_text`]），code 原样。
fn code_text_item(p: &Value) -> anyhow::Result<(String, String)> {
    Ok((
        str_param(p, "code")?.to_string(),
        store_text(str_param(p, "text")?),
    ))
}

/// 一条 `{code, text}`，code 另拆掉音节空格（列表项的码带空格，词频 / 临时词的 key 是扁平的）。
fn flat_code_text_item(p: &Value) -> anyhow::Result<(String, String)> {
    let (code, text) = code_text_item(p)?;
    Ok((wind_store::wdict::split_spaced_code(&code).0, text))
}

/// 一条 `{kind, id}`（快捷输入格式）。
fn kind_id_item(p: &Value) -> anyhow::Result<(String, String)> {
    Ok((
        str_param(p, "kind")?.to_string(),
        str_param(p, "id")?.to_string(),
    ))
}

/// 导入类方法的文本来源：`path` 优先，`content` 回落。
///
/// 走 `content` 时整份词库塞进一帧 JSON-RPC，会撞设置端 16MB 读帧上限
/// （`wind-setting::rpc::MAX_FRAME`，约数十万条即触顶）；`path` 让 payload 与词库大小无关，
/// 与 `backup.*` 同一做法。`content` 留给没有文件可指的调用方：命令行管道、设置端
/// 「纯词列表」在内存里拼出的 TSV。路径不做额外校验，与 `backup.*` 一致（来自本机文件对话框，与 core 同权限）。
fn dict_source_text(params: &Value) -> anyhow::Result<std::borrow::Cow<'_, str>> {
    if let Some(path) = params.get("path").and_then(|v| v.as_str())
        && !path.is_empty()
    {
        let s = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取 {} 失败：{}", path, e))?;
        return Ok(std::borrow::Cow::Owned(s));
    }
    Ok(std::borrow::Cow::Borrowed(str_param(params, "content")?))
}

/// 按设置页点选的列给常用字列表排序。`sort_by` 为空 = 不动，保持字表原序。
///
/// `"text"` 与 `"char"` 都认：「字」列复用 `WordField::Text`，设置页发 `"text"`，而 json 字段名是
/// `"char"`；只认一个会让点列头静默无反应。降序用 `Ordering::reverse` 而非排完整体反转，
/// 保证同值组仍保持字表原序。
fn sort_common_char_rows(
    rows: &mut [wind_coordinator::handle_common_chars::CommonCharRow],
    sort_by: &str,
    desc: bool,
) {
    let key = |r: &wind_coordinator::handle_common_chars::CommonCharRow| match sort_by {
        // 多码位簇按首码位排——它没有单一码位，取首个是唯一稳定且直观的口径。
        "text" | "char" => (r.text.chars().next().map_or(0, |c| c as u32), false),
        // bool 升序 = false 在前 = 生僻在前。
        "baseCommon" => (0, r.base_common),
        "common" => (0, r.common),
        _ => (0, false),
    };
    if !matches!(sort_by, "text" | "char" | "baseCommon" | "common") {
        return;
    }
    rows.sort_by(|a, b| {
        let o = key(a).cmp(&key(b));
        if desc { o.reverse() } else { o }
    });
}

/// 参数里取一个字符，按字素簇校验（`⚽️`、`👨‍👩‍👧`、`🇨🇳` 各算一个）。
///
/// 多字符 / 空串一律拒绝而不是取首字符：「常用」是字级属性，悄悄截取会让用户误以为标了整个词。
fn char_param(p: &Value, key: &str) -> anyhow::Result<String> {
    let s = str_param(p, key)?;
    if s.is_empty() {
        anyhow::bail!("参数 {key} 不能为空");
    }
    wind_candidate::single_markable_char(s)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("参数 {key} 只能是一个字符（收到「{s}」）"))
}

/// 读取 `sections` 参数（字符串数组）→ 词库数据段；缺省返回 None（由调用方取引擎默认）。
fn dict_sections_param(p: &Value) -> Option<Vec<wind_store::dict_export::DictSection>> {
    let arr = p.get("sections")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str())
            .filter_map(wind_store::dict_export::DictSection::from_key)
            .collect(),
    )
}

/// 导入结果 → JSON（`{sections:[{key, added/updated/unchanged | imported, skipped, …}]}`）。
/// 过了准入契约的段（用户词 / 临时词）再并进分类统计，两种格式字段名一致，UI 共用一套渲染。
/// `rejectedSamples` 只有 WindDict 多段路径带，Rime/TSV 路径不回。
fn dict_report_json(out: &dict_transfer_api::DictImportOutcome) -> Value {
    let wind_dict = out.format == wind_store::import_formats::DictFormat::WindDict;
    let sections: Vec<Value> = out
        .sections
        .iter()
        .map(|s| {
            let mut o = serde_json::Map::new();
            o.insert("key".into(), json!(s.key));
            if let Some(w) = &s.words {
                o.insert("added".into(), json!(w.added));
                o.insert("updated".into(), json!(w.updated));
                o.insert("unchanged".into(), json!(w.unchanged));
                // 导入不撤删除规则：入了库但被候选调整隐藏、打不出来的条数。
                o.insert("hiddenByShadow".into(), json!(s.hidden_by_shadow));
            } else {
                o.insert("imported".into(), json!(s.imported));
            }
            // 闸口丢掉的行已并进 skipped，此处只补「为什么少」的分类。
            o.insert("skipped".into(), json!(s.skipped));
            if let Some(st) = &s.contract {
                o.insert("rejected".into(), json!(st.rejected));
                if wind_dict {
                    o.insert("rejectedSamples".into(), json!(st.samples));
                }
                o.insert("boundaryFilled".into(), json!(st.filled));
                o.insert("boundaryAmbiguous".into(), json!(st.ambiguous));
                o.insert("boundaryNoReading".into(), json!(st.no_reading));
                o.insert("noBoundary".into(), json!(st.no_boundary));
                // 用户选「不补充」时跳过的行数（与 rejected 分开：那是程序判非法，这是用户的选择）。
                o.insert("boundarySkipped".into(), json!(st.unfilled_skipped));
            }
            Value::Object(o)
        })
        .collect();
    json!({ "sections": sections })
}

fn i32_param(p: &Value, key: &str) -> i32 {
    p.get(key).and_then(|v| v.as_i64()).unwrap_or(0) as i32
}

/// 该数据 RPC 是否扫整张（某方案的）用户词 / 临时词 / 词频 / shadow 表，见 [`WebDataRpc::web_data_rpc`]。
///
/// 只收真正全表的：`dict.listPaged` 为给精确 `total` 三条分流都扫满；`dict.search` 是带
/// `limit` 的前缀查询、扫到够数即停，只有 `limit = 0`（不限）时才算。短语 / 补全 / 快捷输入 /
/// 常用字 / 字符类 / 统计这些全局小表不在内：条数有界，扫一遍填不起可观的缓存。
fn scans_whole_table(method: &str, params: &Value) -> bool {
    FULL_SCAN_METHODS.contains(&method)
        || (method == "dict.search" && usize_param(params, "limit", 50) == 0)
}

/// 无条件全表扫描的 RPC（`dict.search` 有条件，单独判，见 [`scans_whole_table`]）。
/// 方法名与分派表的对应由测试 `rpc_name_tables_match_the_dispatch_table` 锁住。
const FULL_SCAN_METHODS: &[&str] = &[
    "dict.listPaged",
    "dict.export",
    "dict.stats",
    "temp.list",
    "temp.listPaged",
    "temp.promoteAll",
    "temp.clear",
    "freq.listPaged",
    "shadow.list",
    "shadow.listPaged",
    "backup.create",
    // 预览要逐条比对现有词（新增 / 已存在 / 冲突），整表都读一遍。
    "dict.previewImport",
    // 清空是逐条删（整表都走一遍）；量通常不大，按扫描标记而不立即回收。
    "freq.clear",
    "shadow.clear",
];

/// 全表规模的写：成功之后立即回收页缓存（`drop_page_cache_after_bulk_write`）。
///
/// redb 刷盘会把脏页晋升进读缓存且长期不还，大词库导入后常驻内存明显上涨。只在成功时丢
/// （失败路径没落库，不必付关库重开的代价）；丢缓存失败只记日志，不把成功的写报成失败。
/// 放在分发出口而非各 handler 里，免得新增分支漏掉。`schema.delete` 级联清六张表，量级同 `dict.clear`。
const BULK_WRITE_METHODS: &[&str] = &[
    "backup.restore",
    "dict.clear",
    "dict.import",
    "schema.delete",
];

/// 可选的 `ids` 字符串数组：缺省 / `null` 为 `None`，类型不对报错。
fn optional_ids(p: &Value) -> anyhow::Result<Option<Vec<String>>> {
    match p.get("ids") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow::anyhow!("ids 须为字符串数组"))
            })
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Some),
        Some(_) => anyhow::bail!("ids 须为字符串数组"),
    }
}

fn usize_param(p: &Value, key: &str, default: usize) -> usize {
    p.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(default)
}

/// 解析排序参数。sortBy 在 valid_fields 中时返回 (字段名, is_desc)，否则返回 None（保持原顺序）。
fn parse_sort<'a>(params: &'a Value, valid_fields: &[&str]) -> Option<(&'a str, bool)> {
    let by = params.get("sortBy")?.as_str()?;
    if !valid_fields.contains(&by) {
        return None;
    }
    let desc = params.get("sortOrder").and_then(|v| v.as_str()) == Some("desc");
    Some((by, desc))
}

/// `system.memoryStats` 的回包：进程私有内存 + 已加载方案 + 各懒建结构的自报大小。
///
/// 数据源与 `Coordinator::debug_memory_report` 相同（[`wind_engine::EngineManager::memory_report`]
/// 与 `ReverseLookup::heap_bytes`），不另算一套。字段恒在：取不到的给 `null`、没有的给空数组。
///
/// - `process`：`{bytes, kind}`，口径见 `wind_coordinator::process_memory`（Windows Private、
///   Linux / Android RssAnon；macOS 两者皆 `null`）。
/// - `redb`：redb 不暴露页缓存的在用量，只给上限与「空闲回收」已发生的次数；库已暂停或无 store 时 `null`。
/// - 字节数都是各结构按容量自报、不含分配器开销，只用来回答「是哪一档涨了」。
fn memory_stats_json<H: WebDataHost + ?Sized>(h: &H) -> Value {
    let process = wind_coordinator::process_memory::private_memory();
    let r = h.engine_mgr().memory_report();
    let (chaizi, pinyin, comments) = h
        .reverse_lookup()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .heap_bytes();
    json!({
        "process": {
            "bytes": process.map(|p| p.bytes),
            "kind": process.map(|p| p.kind),
        },
        "schemas": r.engines.iter().map(|(id, ty, parts)| json!({
            "id": id,
            "type": ty,
            "parts": parts.iter().map(|p| json!({
                "name": p.name, "bytes": p.bytes, "allocs": p.allocs,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "reverseIndex": r.reverse_index.iter().map(|(id, heap, image)| json!({
            "schema": id, "heapBytes": heap, "imageBytes": image,
        })).collect::<Vec<_>>(),
        "userText": r.user_text.iter().map(|(id, bytes, building)| json!({
            "schema": id, "bytes": bytes, "building": building,
        })).collect::<Vec<_>>(),
        "userAssoc": r.user_assoc.as_ref().map(|(id, bytes)| json!({
            "schema": id, "bytes": bytes,
        })),
        "singleChar": r.single_char.as_ref().map(|(id, bytes, _)| json!({
            "schema": id, "bytes": bytes,
        })),
        "reverseLookup": {
            "chaiziBytes": chaizi, "pinyinBytes": pinyin, "commentBytes": comments,
        },
        "redb": h.user_store().filter(|s| !s.is_paused()).map(|s| json!({
            "cacheLimitBytes": wind_store::store::DEFAULT_CACHE_SIZE_BYTES,
            "pageCacheDrops": s.page_cache_drops(),
        })),
        // 词库缓存策略（`schema.dict_cache_policy`）的实际效果：生效的策略、预算，以及当前仍活着
        // 的映射里：锁定策略下已锁 / 锁页失败的字节数，预加载策略下已载入的字节数，超出预算
        // 而没处理的字节数；不适用的一项恒为 0。预算 u64::MAX（未设限）报 null。
        "dictCache": {
            "policy": wind_dict::mapping::policy().as_config(),
            "budgetBytes": Some(wind_dict::mapping::budget_bytes()).filter(|b| *b != u64::MAX),
            "lockedBytes": wind_dict::mapping::locked_bytes(),
            "lockFailedBytes": wind_dict::mapping::lock_failed_bytes(),
            "preloadedBytes": wind_dict::mapping::preloaded_bytes(),
            "overBudgetBytes": wind_dict::mapping::over_budget_bytes(),
        },
    })
}

/// 数据类 RPC 的分派体。模块私有：外部只能经 [`WebDataRpc::web_data_rpc`] 进来，那里统一
/// 做扫描标记与批量写之后的回收（trait 默认方法没有私有可见性，故不放进 trait）。
fn web_data_dispatch<H: WebDataRpc + ?Sized>(
    h: &H,
    method: &str,
    params: &Value,
) -> anyhow::Result<Value> {
    match method {
        // ── schema.* ─────────────────────────────────────────
        "schema.list" => h.web_schema_list(params),
        "schema.layouts" => h.web_schema_layouts(),
        "schema.active" => Ok(json!({ "id": h.engine_mgr().active_schema_id() })),
        "schema.setActive" => {
            let id = str_param(params, "id")?;
            let ok = h.engine_mgr().switch_schema(id);
            if ok {
                h.sync_chaizi_assets(); // 拆字库/字根字体随活跃方案切换
                h.note_global_schema_set(id); // 设置页是全局意图：按应用方案的全局方案跟随
                // 注释库不随方案变化，见 `Coordinator::sync_comment_dicts`。
            }
            Ok(json!({ "ok": ok }))
        }
        // ── 方案配置编辑（三层合并：默认 ← 方案文件 ← override 层）──
        "schema.getConfig" => h.web_schema_get_config(params),
        // 全局页「已被 N 个方案覆盖」的反查：全局键 → 覆盖它的方案 id 列表（R8.6）。
        "schema.overrideSummary" => h.web_schema_override_summary(),
        "schema.saveConfig" => h.web_schema_save_config(params),
        // 旧的类型级全局键（`schema.codetable.*` 等）的读写：转到它此刻所指的方案。
        // 调用方（`config.getItem` / `config.setItems`）已按注册表校验过键与值。
        "schema.aliasGet" => {
            let key = str_param(params, "key")?;
            let v = h
                .engine_mgr()
                .scheme_alias_get(key)
                .ok_or_else(|| anyhow::anyhow!("{key} 不是方案设置的别名"))??;
            Ok(json!({ "value": serde_json::to_value(v)? }))
        }
        // 某方案的全部设置项（键用旧全局键名）：生效值、方案默认值，以及方案文件改过的项
        // 原本的类型内置默认。设置页据此画控件、标出改过的项与方案预设。
        "schema.getSettings" => {
            let id = str_param(params, "id")?;
            let snap = h
                .engine_mgr()
                .scheme_settings_snapshot(id)
                .ok_or_else(|| anyhow::anyhow!("方案不存在或没有可设置的项: {id}"))??;
            Ok(json!({
                "id": id,
                "effective": serde_json::to_value(snap.effective)?,
                "default": serde_json::to_value(snap.default)?,
                // 方案文件改过的项在类型内置默认里的值：设置页据此列出「本方案预设」。
                "builtin": serde_json::to_value(snap.builtin)?,
            }))
        }
        // 改某方案的设置：`items` = [{key, value}] 写入，`reset` = [key] 恢复方案默认。
        // 值按注册表校验，有一项不合格整批不写。
        "schema.setSettings" => {
            let id = str_param(params, "id")?;
            let mut sets = Vec::new();
            for it in params
                .get("items")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let key = it
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("items 缺少 key"))?;
                let value = json_to_toml(it.get("value").unwrap_or(&Value::Null));
                wind_config::config_schema::validate(key, &value)
                    .map_err(|e| anyhow::anyhow!("{key}: {e}"))?;
                sets.push((key.to_string(), value));
            }
            let resets: Vec<String> = params
                .get("reset")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            h.engine_mgr().scheme_settings_apply(id, sets, &resets)?;
            h.refresh_schema_derived_config();
            Ok(json!({ "ok": true }))
        }
        "schema.aliasSet" => {
            let key = str_param(params, "key")?;
            let value = params
                .get("value")
                .ok_or_else(|| anyhow::anyhow!("缺少参数 value"))?;
            let id = h
                .engine_mgr()
                .scheme_alias_set(key, json_to_toml(value))
                .ok_or_else(|| anyhow::anyhow!("{key} 不是方案设置的别名"))??;
            h.refresh_schema_derived_config();
            Ok(json!({ "ok": true, "schema": id }))
        }
        "schema.auxCodeSources" => h.web_schema_aux_code_sources(params),
        "schema.resetConfig" => h.web_schema_reset_config(params),
        "schema.setDictEnabled" => h.web_schema_set_dict_enabled(params),
        // 失效方案的引擎缓存（未加载时安全 no-op）：CLI `schema set/reset` 后
        // 调用，让 override 改动在下次使用该方案时按新配置重建生效。
        "schema.invalidate" => {
            let id = str_param(params, "id")?;
            h.engine_mgr().invalidate_schema(id);
            // CLI 直接写 override 后走这里，与 saveConfig 同形：不重建吃键集等跨方案派生集合，
            // 方案自定义标点 / 引导键要等重启才生效。
            h.refresh_schema_derived_config();
            Ok(json!({ "ok": true }))
        }
        // 全量强制重建词库缓存（CLI `schema rebuild`）：失效全部引擎后删缓存产物。
        // 面向「指纹判新鲜但内容需重建」的场景（如解析器修复后存量缓存静默过期）。
        "schema.rebuildCache" => {
            let (removed, failed) = h.engine_mgr().rebuild_all_caches();
            Ok(json!({ "removed": removed, "failed": failed }))
        }
        // 重启服务（CLI `wind_input restart`），与托盘菜单同一流程。延迟发信号：main 收到即 exit，
        // 先让本条响应写回客户端。
        "system.restart" => {
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                wind_coordinator::request_restart();
            });
            Ok(json!({ "ok": true }))
        }
        // 内存诊断（只读，设置端「高级 → 性能 → 内存占用」与「复制诊断信息」）。
        "system.memoryStats" => Ok(memory_stats_json(h)),
        // 存储状态（设置页提示「只读模式」，用户数据设计 §7.1）。`issues` 路径相对数据目录；
        // `format_version` 是 meta.toml 实际值，`known_format_version` 是本版本认识的上限；
        // `write_error` 为最近一次写失败原因，成功写入后清空。用户数据锁不可用时同样报 readonly。
        "system.storageStatus" => {
            let store = h.user_store();
            let engine_readonly = store.is_some_and(|s| s.runtime().data().readonly());
            let reason = h.storage_readonly_reason().or_else(|| {
                engine_readonly.then(|| {
                    "用户数据被另一个进程占用，或文件锁不可用：本次运行不保存学习与修改。"
                        .to_string()
                })
            });
            let actual = store
                .and_then(|s| s.path().parent().map(wind_config::user_meta::read))
                .flatten()
                .map(|m| m.format_version);
            Ok(json!({
                "readonly": reason.is_some(),
                "reason": reason,
                "format_version": actual.unwrap_or(wind_config::user_meta::KNOWN_FORMAT_VERSION),
                "known_format_version": wind_config::user_meta::KNOWN_FORMAT_VERSION,
                "write_error": store.and_then(|s| s.runtime().data().write_error()),
                "issues": store.map(|s| storage_issues_json(s)).unwrap_or_default(),
            }))
        }
        // 桌面提示（CLI `wind_input ui toast`、外部脚本）：toast 只能由 core 的 UI 线程渲染。
        // 刻意不在 dict.import / config.set 等方法里自动弹：设置页已有自己的反馈，要提示的调用方显式调这条。
        "ui.toast" => {
            let text = str_param(params, "text")?;
            let s = |k: &str| {
                params
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let ms = params.get("ms").and_then(Value::as_u64).unwrap_or(0);
            h.ui_toast(text, &s("kind"), &s("color"), &s("pos"), ms)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(json!({ "ok": true }))
        }
        // 输入状态开关（CLI `wind_input ime`、外部脚本）。目标 / 值域校验在宿主侧，错误原样回传。
        // `get` 不带 target = 取全部。`set` 默认不弹「已是…」提示（脚本会反复调用），要提示传 notify。
        "ime.get" => {
            const ALL: [&str; 5] = ["cn-en", "fullshape", "s2t", "t2s", "toolbar"];
            let target = params.get("target").and_then(Value::as_str);
            match target {
                Some(t) => Ok(json!({ "target": t, "on": h.ime_state_get(t)? })),
                None => {
                    let mut m = serde_json::Map::new();
                    for t in ALL {
                        m.insert(t.to_string(), json!(h.ime_state_get(t)?));
                    }
                    Ok(Value::Object(m))
                }
            }
        }
        "ime.set" => {
            let target = str_param(params, "target")?;
            let on = params
                .get("on")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow::anyhow!("缺少布尔参数 on"))?;
            let notify = params
                .get("notify")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let changed = h.ime_state_set(target, on, notify)?;
            Ok(json!({ "target": target, "on": on, "changed": changed }))
        }
        "ime.toggle" => {
            let target = str_param(params, "target")?;
            let on = h.ime_state_toggle(target)?;
            Ok(json!({ "target": target, "on": on }))
        }
        "schema.delete" => h.web_schema_delete(params),
        "schema.references" => Ok(json!({})), // 引用关系（删除安全检查）：暂返空，前端宽松消费
        "scheme.exportPackage" => h.web_scheme_export_package(params),
        "scheme.importPackage" => h.web_scheme_import_package(params),
        "scheme.previewImport" => h.web_scheme_preview_import(params),
        // 文本信封（kind = "schema_text"）：形状与上面两个 path 版完全一致，
        // 设置端复用同一个确认对话框。
        "scheme.previewImportText" => h.web_scheme_preview_import_text(params),
        "scheme.importText" => h.web_scheme_import_text(params),

        // ── backup.*（整机备份，wind-transfer::backup）───────
        "backup.create" => h.web_backup_create(params),
        "backup.inspect" => h.web_backup_inspect(params),
        // 批量写：成功后立即回收页缓存，见 `BULK_WRITE_METHODS`。
        "backup.restore" => h.web_backup_restore(params),

        // ── dict.*（用户词库）────────────────────
        "comment.sources" => h.web_comment_sources(),
        // 设置端注释总表：模板表（带来源）、出厂分配、每格实际生效的模板与所在层。
        // 带 `comment`（未保存的 `[ui.comment]` 草稿）时按草稿算。
        "comment.library" => h.web_comment_library(params),
        "emoji.modules" => h.web_emoji_modules(),
        "dict.listPaged" => h.web_dict_list_paged(params),
        "dict.search" => h.web_dict_search(params),
        "dict.add" => h.web_dict_add(params),
        "dict.update" => h.web_dict_update(params),
        "dict.remove" => h.web_dict_remove(params),
        // 批量写（逐条 remove，动的页与导入同量级）：见 `BULK_WRITE_METHODS`。
        "dict.clear" => h.web_dict_clear(params),
        "dict.stats" => h.web_dict_stats(),
        // 系统词库统计（设置页「词库统计」）：本体在 `wind_coordinator::dict_stats_api`。
        // 扫的是 mmap 词库不是 redb，故不进 `FULL_SCAN_METHODS`（那张表管页缓存回收）。
        "dict.systemStats" => wind_coordinator::dict_stats_api::system_stats(
            h,
            str_param(params, "schemaId")?,
            usize_param(params, "topN", 50),
            params
                .get("refresh")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        // 词库窗「系统词库」：一级词库表（含主库、按生效先后）、二级头部卡片与只读词条分页。
        // 本体在 `wind_coordinator::schema_dicts_api`；开关仍走 `schema.setDictEnabled`。
        // 读的是词库文本不是 redb，不进 `FULL_SCAN_METHODS`。
        "sysDict.categories" => {
            wind_coordinator::schema_dicts_api::categories(h, str_param(params, "schema")?)
        }
        "sysDict.header" => wind_coordinator::schema_dicts_api::header(
            h,
            str_param(params, "schema")?,
            str_param(params, "dictId")?,
        ),
        "sysDict.entries" => wind_coordinator::schema_dicts_api::entries(
            h,
            str_param(params, "schema")?,
            str_param(params, "dictId")?,
            params.get("query").and_then(Value::as_str).unwrap_or(""),
            params.get("mode").and_then(Value::as_str),
            params.get("sort").and_then(Value::as_str),
            params.get("order").and_then(Value::as_str),
            usize_param(params, "offset", 0) as u64,
            usize_param(params, "limit", 100),
        ),
        // 加词界面的默认上下文：设置端 `--add-word` 裸启动（无 --schema / --text）时用它填窗口，
        // 也是窗内「最近输入」按钮的取值。深链带了参数就不调它。
        "dict.addWordContext" => {
            let ctx = h.add_word_context();
            Ok(json!({
                "schemaId": ctx.schema_id,
                "recentText": ctx.recent_text,
                "maxLen": ctx.max_len,
            }))
        }
        // 加词自动出码：按方案类型选拼音/五笔规则（reverse 反查表）。
        "dict.encode" => h.web_dict_encode(params),
        // 批量出码：纯词列表导入按批调用（设置端每批约 1000 词）。
        "dict.encodeWords" => h.web_dict_encode_words(params),
        "dict.genPinyin" => {
            // 取码要按真实文本算：转义形态里的 `\` `n` 会被当成两个待取码的字符。
            let text = str_param(params, "text")?;
            Ok(json!(h.gen_pinyin_word(&store_text(text))))
        }
        "dict.export" => h.web_dict_export(params),
        // 批量写：成功后立即回收页缓存，见 `BULK_WRITE_METHODS`。
        "dict.import" => h.web_dict_import(params),
        "dict.previewImport" => h.web_dict_preview_import(params),
        // 用户词文本手改后重新载入（设计 §5）；带 paths 只同步这几个文件，越界拒绝。
        "dict.reload" => h.web_dict_reload(params),
        // 显式折回全部待写回的数据（备份 / 导出 / 调试前）。
        "dict.flush" => h.web_dict_flush(),
        // 外部编辑会话（设计 user-dict-libraries-and-external-edit §4.2）：begin 暂停该文件的
        // 写回 / 挡住界面修改并给出绝对路径，end 按文件载入并恢复；超过租约 core 自动 end。
        "data.beginEdit" => h.web_data_begin_edit(params),
        "data.endEdit" => h.web_data_end_edit(params),
        "data.editSessions" => h.web_data_edit_sessions(),
        // 「打开数据目录」：数据目录或其下方案目录 / 数据文件的绝对路径。
        "data.locate" => h.web_data_locate(params),
        // 用户分库（设计 user-dict-libraries-and-external-edit §3）：`<数据方案>/dicts/*.dict.yaml`。
        "userDicts.list" => user_dicts::list(h, params),
        "userDicts.entries" => user_dicts::entries(h, params),
        "userDicts.removeEntry" => user_dicts::remove_entry(h, params),
        "userDicts.addEntry" => user_dicts::add_entry(h, params),
        "userDicts.clear" => user_dicts::clear(h, params),
        "userDicts.create" => user_dicts::create(h, params),
        "userDicts.rename" => user_dicts::rename(h, params),
        "userDicts.delete" => user_dicts::delete(h, params),
        "userDicts.importAs" => user_dicts::import_as(h, params),
        "userDicts.setEnabled" => user_dicts::set_enabled(h, params),
        // 删除一个方案的全部用户数据（数据方案目录、它的备份、方案覆盖）；共用数据的拒绝。
        "userdata.purgeSchema" => h.web_userdata_purge_schema(params),

        // ── temp.*（临时词，redb）─────────────────────────────
        "temp.list" => h.web_temp_list(params),
        // 分页版（设置页列表走这条；`temp.list` 保留给导出等全量取用方）。
        "temp.listPaged" => h.web_temp_list_paged(params),
        "temp.promote" => h.web_temp_promote(params),
        "temp.remove" => h.web_temp_remove(params),
        "temp.promoteAll" => h.web_temp_promote_all(params),
        "temp.clear" => h.web_temp_clear(params),

        // ── freq.*（用户词频）───────────────────
        "freq.listPaged" => h.web_freq_list_paged(params),
        "freq.delete" => h.web_freq_delete(params),
        "freq.clear" => h.web_freq_clear(params),

        // ── mailSuffix.* / urlHistory.*（补全学习数据）──
        // 两组共用一份实现，只差 `CompletionKind`（存储层是一张表两个分区）。
        // 命名空间分开是为了「清空网址历史」不顺带抹掉邮箱学习。
        "mailSuffix.listPaged" => h.web_completion_list_paged(CompletionKind::EmailSuffix, params),
        "mailSuffix.delete" => h.web_completion_delete(CompletionKind::EmailSuffix, params),
        "mailSuffix.clear" => h.web_completion_clear(CompletionKind::EmailSuffix),
        "urlHistory.listPaged" => h.web_completion_list_paged(CompletionKind::UrlHistory, params),
        "urlHistory.delete" => h.web_completion_delete(CompletionKind::UrlHistory, params),
        "urlHistory.clear" => h.web_completion_clear(CompletionKind::UrlHistory),
        // 快捷输入历史（成员 `quick_input.history`），同一张表的第三个分区。
        "quickHistory.listPaged" => {
            h.web_completion_list_paged(CompletionKind::QuickHistory, params)
        }
        "quickHistory.delete" => h.web_completion_delete(CompletionKind::QuickHistory, params),
        "quickHistory.clear" => h.web_completion_clear(CompletionKind::QuickHistory),

        // ── shadow.*（影子规则）─────────────────
        "shadow.list" => h.web_shadow_list(params),
        // 分页版（设置页列表走这条；`shadow.list` 保留给全量取用方）。
        "shadow.listPaged" => h.web_shadow_list_paged(params),
        // 整表撤销。设置端分页后手上只有当前页，不能靠逐条 remove_rule 凑出清空。
        "shadow.clear" => h.web_shadow_clear(params),
        "shadow.pin" => h.web_shadow_pin(params),
        "shadow.delete" => h.web_shadow_delete(params),
        "shadow.removeRule" => h.web_shadow_remove_rule(params),
        "shadow.addRule" => h.web_shadow_add_rule(params),

        // ── phrase.*（用户短语，全局）──────────
        "phrase.list" => h.web_phrase_list(),
        "phrase.add" => h.web_phrase_add(params),
        "phrase.update" => h.web_phrase_update(params),
        "phrase.remove" => h.web_phrase_remove(params),
        "phrase.setEnabled" => h.web_phrase_set_enabled(params),
        "phrase.resetDefault" => h.web_phrase_reset(),
        "phrase.listSystem" => h.web_phrase_list_system(),
        "phrase.listUser" => h.web_phrase_list_user(params),
        "phrase.export" => h.web_phrase_export(),
        "phrase.import" => h.web_phrase_import(params),
        "phrase.previewImportText" => h.web_phrase_preview_import_text(params),
        "phrase.importText" => h.web_phrase_import_text(params),
        "phrase.resetSystem" => h.web_phrase_reset_system(),

        // ── quick.*（快捷输入格式表的用户调整，全局）──
        // 基表（模板与出厂顺序）在 system.quick.toml，RPC 一律不写它（会抢走手写文件的所有权，
        // 见 handle_quick_format 模块文档）。
        "charset.list" => h.web_charset_list(),
        "charset.set" => h.web_charset_set(params),
        "charset.reset" => h.web_charset_reset(params),
        "charset.clearRedundant" => h.web_charset_clear_redundant(params),
        "charset.delete" => h.web_charset_delete(params),
        "charset.create" => h.web_charset_create(params),
        "commonChars.list" => h.web_common_chars_list(params),
        "commonChars.query" => h.web_common_chars_query(params),
        "commonChars.set" => h.web_common_chars_set(params),
        "commonChars.bulkByBlock" => h.web_common_chars_bulk(params),
        "commonChars.reset" => h.web_common_chars_reset(params),
        "commonChars.clear" => h.web_common_chars_clear(),
        "commonChars.export" => h.web_common_chars_export(),
        "commonChars.previewImport" => h.web_common_chars_preview_import(params),
        "commonChars.import" => h.web_common_chars_import(params),
        "quick.list" => h.web_quick_list(),
        "quick.move" => h.web_quick_move(params),
        "quick.setEnabled" => h.web_quick_set_enabled(params),
        "quick.resetEntry" => h.web_quick_reset_entry(params),
        "quick.resetKind" => h.web_quick_reset_kind(params),
        "quick.vars" => h.web_quick_vars(),
        "quick.add" => h.web_quick_add(params),
        "quick.setText" => h.web_quick_set_text(params),
        "quick.delete" => h.web_quick_delete(params),
        // ── 外部改了文件内容之后让 core 重读（数据同步、手改；core 不监听文件）──
        // compat.toml 重载规则表并刷新前台应用的缓存；成功广播由 dispatch 的 compat.* 统一发。
        "compat.reload" => {
            h.reload_compat();
            Ok(json!({ "ok": true }))
        }
        "schema.reloadFiles" => h.web_schema_reload_files(params),
        "theme.reloadFiles" => h.web_theme_reload_files(params),
        // ── compat.*：应用兼容规则的分层管理（逻辑全在 wind_config::compat_admin）──
        m if m.starts_with("compat.") => h.web_compat_rpc(m, params),
        // ── softkeyboard.*：软键盘编辑器（逻辑在 wind_softkeyboard::admin）──
        "softkeyboard.get" => h.softkeyboard_view(),
        "softkeyboard.apply" => h.web_softkeyboard_apply(params),
        // 外部改了 system.softkeyboard.toml 后让 core 重读（数据同步）
        "softkeyboard.reload" => {
            h.softkeyboard_reload();
            Ok(json!({ "ok": true }))
        }
        "quick.export" => h.web_quick_export(),
        "quick.import" => h.web_quick_import(params),
        "quick.previewImport" => h.web_quick_preview_import(params),

        // ── stats.*（输入统计，redb 每日聚合）────────────────
        "stats.summary" => h.web_stats_summary(params),
        "stats.daily" => h.web_stats_daily(params),
        "stats.devices" => h.web_stats_devices(),
        "stats.clear" => h.web_stats_clear(),
        "stats.pruneBefore" => h.web_stats_prune(params),
        // 同步认领另一台设备的身份时，本机统计换成那台设备的
        "stats.replaceMonths" => h.web_stats_replace_months(params),

        // ── appearance.*（设置页预览，text-span-colors.md §11）──
        "appearance.previewTemplate" => h.web_appearance_preview_template(params),

        // ── correction.*（拼写纠错，设置页自定义规则行的冲突示例）──
        "correction.conflictExample" => correction_conflict_example(params),

        // ── theme.* ──────────────────────────────────────────
        "theme.list" => h.web_theme_list(params),
        "theme.resolved" => Ok(serde_json::to_value(h.theme_follow_values())?),
        "theme.accent" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            Ok(serde_json::to_value(h.theme_accent(name))?)
        }
        "theme.preview" => h.web_theme_preview(params),
        "theme.renderPreview" => theme_render::render_one(h, params),
        "theme.renderPreviews" => theme_render::render_many(h, params),
        "theme.getText" => h.web_theme_get_text(params),
        "theme.delete" => h.web_theme_delete(params),
        "theme.importFromText" => h.web_theme_import_text(params),
        "theme.previewPackage" => h.web_theme_preview_package(params),
        "theme.importPackage" => h.web_theme_import_package(params),
        "theme.exportPackage" => h.web_theme_export_package(params),
        "theme.importFromUrl" => {
            anyhow::bail!("URL 导入未启用（features.theme.import_url=false）")
        }

        other => anyhow::bail!("unknown method: {}", other),
    }
}

/// `correction.conflictExample {rule}` → `{valid:true, example}`（无冲突时 example 为 null）
/// 或 `{valid:false, reason}`。判据与引擎同源（`wind_engine::pinyin::correction`）。
fn correction_conflict_example(params: &Value) -> anyhow::Result<Value> {
    use wind_engine::pinyin::correction::custom_rule_conflict;
    Ok(match custom_rule_conflict(str_param(params, "rule")?) {
        Ok(example) => json!({ "valid": true, "example": example }),
        Err(reason) => json!({ "valid": false, "reason": reason }),
    })
}

/// 数据文件问题 → 设置页用的 JSON（路径相对数据目录、`/` 分隔）。
fn storage_issues_json(store: &wind_store::Store) -> Vec<Value> {
    let root = store.user_text_dir();
    store
        .data_issues()
        .iter()
        .map(|i| issue_json(root, i))
        .collect()
}

/// 一条数据文件问题：路径相对数据目录（`storageStatus` 与 `data.endEdit` 共用）。
fn issue_json(root: &std::path::Path, i: &wind_store::issues::FileIssue) -> Value {
    use wind_store::issues::IssueKind;
    let rel = i.path.strip_prefix(root).unwrap_or(&i.path);
    let path: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut v = json!({ "path": path.join("/") });
    match &i.kind {
        IssueKind::BadLines(n) => {
            v["kind"] = json!("bad_lines");
            v["lines"] = json!(n);
        }
        IssueKind::Isolated => v["kind"] = json!("isolated"),
        IssueKind::Rebuilt { kept_as } => {
            v["kind"] = json!("rebuilt");
            v["kept_as"] = json!(kept_as);
        }
        IssueKind::Invalid { message } => {
            v["kind"] = json!("invalid");
            v["file"] = v["path"].clone();
            v["message"] = json!(message);
        }
    }
    v
}

/// 设置页数据 RPC 本体：全部方法为默认实现，只能经 [`WebDataHost`] 窄面触宿主（默认方法看不见
/// Coordinator 字段，窄面约束由编译期保证）。调用方 `use` 本 trait 后在 Coordinator 上调 `web_data_rpc`。
pub trait WebDataRpc: WebDataHost {
    /// 枚举本机字体：返回 (family, display_name)。family 为匹配/渲染用名(通常英文),
    /// display_name 优先取该字体含 CJK 字符的本地化名(如"微软雅黑"),否则同 family。
    /// 首次调用扫描系统字体目录（fontdb），开销可接受（设置页打开字体选择时一次）。
    fn list_font_families(&self) -> Vec<(String, String)> {
        fn has_cjk(s: &str) -> bool {
            s.chars().any(|c| {
                let u = c as u32;
                (0x4E00..=0x9FFF).contains(&u) // CJK 统一表意
                    || (0x3400..=0x4DBF).contains(&u) // 扩展 A
                    || (0xF900..=0xFAFF).contains(&u) // 兼容表意
            })
        }
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // family(英文) → 显示名;同 family 保留首个本地化名。BTreeMap 去重 + 按 family 升序。
        let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for face in db.faces() {
            let Some((family, _)) = face.families.first() else {
                continue;
            };
            if family.is_empty() {
                continue;
            }
            let display = face
                .families
                .iter()
                .map(|(n, _)| n)
                .find(|n| has_cjk(n))
                .cloned()
                .unwrap_or_else(|| family.clone());
            map.entry(family.clone()).or_insert(display);
        }
        map.into_iter().collect()
    }

    /// 数据类 RPC 总分派。方法名以 `<ns>.<method>` 形式分组；未知方法返回 Err。
    ///
    /// 全表扫描的方法做完后标记「有扫描待回收」，库空闲 3 秒后回收 redb 读缓存
    /// （`Store::mark_scan_pending`，`docs/design/memory-footprint.md` §6）；不立即回收，免得连续翻页
    /// 每页都冷启动。放在唯一出口免得分支漏掉；出错也标记（扫描可能已走了一半）。
    /// 批量写（[`BULK_WRITE_METHODS`]）成功后则立即回收。
    fn web_data_rpc(&self, method: &str, params: &Value) -> anyhow::Result<Value> {
        let r = web_data_dispatch(self, method, params);
        if r.is_ok() && BULK_WRITE_METHODS.contains(&method) {
            self.drop_page_cache_after_bulk_write(method);
        } else if scans_whole_table(method, params)
            && let Some(store) = self.user_store()
        {
            store.mark_scan_pending();
        }
        r
    }

    /// `schema.list` —— 方案全集（含元信息），供设置页方案管理与主方案下拉。
    ///
    /// `params.includeHidden = true` 时也列出隐藏方案（`[schema].hidden`，如英文、快符），默认不含。
    /// 每项带 `hidden` 字段，前端据此区分该行能配什么（隐藏方案配引导键，普通方案配直达热键）。
    fn web_schema_list(&self, params: &Value) -> anyhow::Result<Value> {
        use std::collections::HashMap;

        let include_hidden = params
            .get("includeHidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // 显示顺序三段：① 已启用的拼音方案；② 其余已启用方案，按 available 配置顺序；
        // ③ 未启用方案，按类型「拼音→码表→混输→英文」分组、组内 id 字典序。
        // installed_schemas() 仍返回 id 字典序全集，排序只在此展示层做。

        // 已启用方案 → 配置位置索引，供段①②保持配置顺序。
        let available = self.engine_mgr().available_schemas();
        let avail_pos: HashMap<&str, usize> = available
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();

        // 未启用段的类型分组顺序：拼音→码表→混输→英文（英文方案极少有人启用）。
        fn type_rank(t: &str) -> i64 {
            match t {
                "pinyin" => 0,
                "codetable" => 1,
                "mixed" => 2,
                "english" => 3,
                _ => 4,
            }
        }

        // 复合排序键 (段号, 段内主键, id)：段号先分档；段内主键在启用段是配置位置、
        // 在未启用段是类型档；id 仅在未启用段做字典序 tiebreak（启用段位置唯一，不参与）。
        let mut rows: Vec<((u8, i64, String), Value)> = self
            .engine_mgr()
            .installed_schemas()
            .iter()
            // 隐藏方案默认不列；已启用的例外，否则用户找不到怎么停用它。
            .filter(|id| {
                include_hidden
                    || avail_pos.contains_key(id.as_str())
                    || !self.engine_mgr().schema_is_hidden(id)
            })
            .map(|id| {
                // 取合并后 Schema 一次，带出方案元信息（备注/版本/图标/作者），供设置页方案列表与详情显示。
                let merged = self.engine_mgr().schema_merged(id);
                let engine_type = merged
                    .as_ref()
                    .map(resolve_engine_type)
                    .unwrap_or("codetable");
                let info = merged.as_ref().map(|s| &s.schema);
                let item = json!({
                    "id": id,
                    "name": self.engine_mgr().schema_name(id),
                    "engineType": merged.as_ref().map(resolve_engine_type),
                    "scheme": merged.as_ref().map(|s| {
                        if resolve_engine_type(s) == "pinyin" {
                            s.engine.pinyin.scheme.clone()
                        } else {
                            String::new()
                        }
                    }).unwrap_or_default(),
                    // 用户目录存在同名 schema.toml 即视为用户方案（可删除）；否则内置。
                    "builtin": !self.engine_mgr().is_user_schema(id),
                    // 隐藏方案（英文/快符）：设置页据此决定该行显示什么、能配什么。
                    "hidden": self.engine_mgr().schema_is_hidden(id),
                    // 方案角色（空 / "aux"）：aux = 辅助码专用码表方案，默认不进方案管理常规区。
                    "role": info.map(|i| i.role.clone()).unwrap_or_default(),
                    // 是否为 overlay 方案（带 `[overlay]` 段，可由引导键/直达热键临时叠加进入）：
                    // 设置页据此枚举 `special:<id>` 的可选项、决定是否显示 overlay 配置。
                    // 与 `hidden` 正交不可互推：隐藏的码表方案可能只是 mix 成员，overlay 方案也可不隐藏。
                    "overlay": merged.as_ref().is_some_and(|s| s.overlay.is_some()),
                    "description": info.map(|i| i.description.clone()).unwrap_or_default(),
                    "version": info.map(|i| i.version.clone()).unwrap_or_default(),
                    "icon_label": info.map(|i| i.icon_label.clone()).unwrap_or_default(),
                    "author": info.map(|i| i.author.clone()).unwrap_or_default(),
                });

                let key = match avail_pos.get(id.as_str()) {
                    // 已启用：拼音置顶(段0)，其余(段1)，段内按配置位置。
                    Some(&pos) => {
                        let seg = if engine_type == "pinyin" { 0 } else { 1 };
                        (seg, pos as i64, String::new())
                    }
                    // 未启用(段2)：按类型档 + id 字典序。
                    None => (2, type_rank(engine_type), id.clone()),
                };
                (key, item)
            })
            .collect();

        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let items: Vec<Value> = rows.into_iter().map(|(_, item)| item).collect();
        Ok(json!(items))
    }

    /// 双拼布局清单：合并扫描安装目录与用户目录的 `schemas/shuangpin/*.toml`，
    /// 返回 `[{id, name}]`，供设置页"双拼布局"下拉动态取值（取代前端硬编码）。
    fn web_schema_layouts(&self) -> anyhow::Result<Value> {
        let items: Vec<Value> = self
            .engine_mgr()
            .shuangpin_layouts()
            .into_iter()
            .map(|(id, name)| json!({ "id": id, "name": name }))
            .collect();
        Ok(json!(items))
    }

    /// 注释词库的可选来源：自动派生项 + `schemas/comments/` 下扫到的文件（`.dict.yaml` 与
    /// OpenCC 文本词典 `.txt`）。
    ///
    /// 设置页据此让用户挑选而不是手填路径（填错的表现是静默不生效）。
    /// 没有 comment 列的文件也列出、以 `hasComment` 标出，而不是滤掉让用户以为文件不见了。
    fn web_comment_sources(&self) -> anyhow::Result<Value> {
        let data_dir = wind_config::Config::data_dir();
        let auto: Vec<Value> = self
            .auto_comment_sources()
            .iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "label": a.label,
                    "rel": a.rel,
                    "schemas": a.schemas,
                })
            })
            .collect();
        let files: Vec<Value> = wind_config::Config::list_schema_resource_dir(
            data_dir.as_deref(),
            "comments",
            ".dict.yaml",
        )
        .into_iter()
        .map(|(rel, abs)| {
            json!({
                "path": rel,
                "label": dict_yaml_name(&abs),
                "hasComment": wind_reverse::declares_comment_column(&abs),
            })
        })
        .chain(
            // OpenCC 文本词典（`词<Tab>值…`）没有 YAML 头可判 columns，约定恒为「词 → 注释」，
            // 故 `hasComment` 直接给 true（解析见 `wind_reverse::parse_comment_dict` 的裸表分支）。
            wind_config::Config::list_schema_resource_dir(data_dir.as_deref(), "comments", ".txt")
                .into_iter()
                .map(|(rel, abs)| {
                    json!({
                        "path": rel,
                        "label": dict_yaml_name(&abs),
                        "hasComment": true,
                    })
                }),
        )
        .collect();
        let builtin: Vec<Value> = self
            .system_comment_dicts()
            .iter()
            .map(|b| json!({ "id": b.id, "label": b.label, "rel": b.path, "origin": "builtin" }))
            .collect();
        Ok(json!({ "auto": auto, "builtin": builtin, "files": files }))
    }

    fn web_comment_library(&self, params: &Value) -> anyhow::Result<Value> {
        use wind_config::comment_lib::{CommentLibrary, TemplateOrigin, UiCommentConfig};
        let system = self.system_comments();
        let lib = match params.get("comment").filter(|v| !v.is_null()) {
            Some(draft) => {
                let user: UiCommentConfig = serde_json::from_value(draft.clone())
                    .map_err(|e| anyhow::anyhow!("comment 草稿形态不对: {e}"))?;
                std::sync::Arc::new(CommentLibrary::build(system, &user).0)
            }
            None => self.comment_library(),
        };
        let templates: Vec<Value> = lib
            .templates()
            .map(|(t, o)| {
                let origin = match o {
                    TemplateOrigin::Builtin => "builtin",
                    TemplateOrigin::User => "user",
                    TemplateOrigin::UserOverridesBuiltin => "user_overrides_builtin",
                };
                let mut v = json!({
                    "id": t.id, "name": t.name, "text": t.text, "desc": t.desc, "origin": origin,
                });
                // 被用户覆盖的内置模板带上原版，供设置端「恢复内置」前对照。
                if o == TemplateOrigin::UserOverridesBuiltin
                    && let Some(b) = system.templates.iter().find(|b| b.id == t.id)
                {
                    v["builtin"] = json!({ "name": b.name, "text": b.text });
                }
                v
            })
            .collect();
        let active = self.engine_mgr().active_schema_id();
        let effective = lib.effective_table(&active, |s| self.comment_schema_type(s));
        Ok(json!({
            "templates": templates,
            "system_assign": system.assign,
            "effective": effective,
        }))
    }

    /// emoji 附加模块清单（内置 + 用户放进 `emoji/` 的 OpenCC 表），供设置页对话框列出。
    ///
    /// 只给清单与出厂值，不给开关状态：设置端按 `input.emoji.modules ?? defaultOn` 自己算，
    /// 否则未保存的勾选会被这里的旧值盖掉。
    fn web_emoji_modules(&self) -> anyhow::Result<Value> {
        let items: Vec<Value> =
            wind_config::Config::emoji_modules(wind_config::Config::data_dir().as_deref())
                .into_iter()
                .map(|m| {
                    json!({
                        "id": m.id,
                        "label": m.label,
                        "description": m.description,
                        "file": m.file,
                        "user": m.user,
                        "defaultOn": m.default_on,
                    })
                })
                .collect();
        Ok(json!({ "modules": items, "userDir": wind_config::EMOJI_USER_DIR }))
    }

    /// 三条分流（流式 / 排序 / 搜索两组）的本体在 [`user_words_api::list_paged`]（与移动端共用），
    /// 这里只做参数解析与显示域投影（[`word_item`]）。
    fn web_dict_list_paged(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let prefix = params
            .get("prefix")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("query").and_then(|v| v.as_str()))
            .unwrap_or("");
        let limit = usize_param(params, "limit", 50);
        let offset = usize_param(params, "offset", 0);
        let sort = parse_sort(params, &["code", "text", "weight"]).map(|(by, desc)| {
            let by = match by {
                "weight" => user_words_api::UserWordSort::Weight,
                "text" => user_words_api::UserWordSort::Text,
                _ => user_words_api::UserWordSort::Code,
            };
            (by, desc)
        });
        let page = user_words_api::list_paged(self, schema, prefix, sort, offset, limit)?;
        let items: Vec<Value> = page.items.into_iter().map(word_item).collect();
        Ok(json!({ "items": items, "total": page.total }))
    }

    fn web_dict_search(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let query = str_param(params, "query").unwrap_or("");
        let limit = usize_param(params, "limit", 50);
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // 列表显示的是带空格的音节码（见 word_item），用户很可能照着搜。key 是扁平的，
        // 不拆则 `ni ha` 一条也匹配不到。拆完仍是前缀语义（`ni ha` → `niha`）。
        // 末尾再过一道 ü 归一，理由见 `user_words_api::list_paged`。
        let (query, _) = wind_store::wdict::split_spaced_code(query);
        let query = user_words_api::normalize_pinyin_code(self, &schema, query);
        let items: Vec<Value> = store
            .search_user_words_prefix(&schema, &query, limit)?
            .into_iter()
            .map(word_item)
            .collect();
        Ok(json!(items))
    }

    /// 本体在 [`user_words_api::add`]（与移动端共用），这里只解参与还原转义。
    /// 带 `items`（`[{code, text, weight?}]`）走 [`user_words_api::add_batch`]。
    fn web_dict_add(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        // 设置页传的是转义形态，还原成存储域（真实文本）后再落库/比对。见 [`store_text`]。
        let (rows, batch) = items_or_single(params, |p| {
            let (code, text) = code_text_item(p)?;
            Ok((code, text, i32_param(p, "weight")))
        })?;
        if batch {
            user_words_api::add_batch(self, schema, &rows)?;
            return Ok(batch_done(rows.len()));
        }
        let (code, text, weight) = &rows[0];
        user_words_api::add(self, schema, code, text, *weight)?;
        Ok(json!({ "ok": true }))
    }

    fn web_dict_update(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let (code, text) = (str_param(params, "code")?, str_param(params, "text")?);
        // text 在这里是查找键（update_user_word_weight 按它匹配记录），须还原，见 [`store_text`]。
        let text = &store_text(text);
        let weight = i32_param(params, "weight");
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // 存在则改权重（boundary 沿用）；不存在则新增（upsert 语义）。
        // code 同样先规范化，否则带空格的码既查不到既有记录、又会新增出带空格的 key。
        let (code, boundary) = user_words_api::normalize_add_code(self, &schema, code, text);
        if !store.update_user_word_weight(&schema, &code, text, weight)? {
            store.add_user_word(&schema, &code, text, weight, boundary)?;
        }
        user_words_api::clear_deleted_for_added(self, &schema, &[(&code, text)]);
        Ok(json!({ "ok": true }))
    }

    fn web_dict_remove(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        // code 带音节空格由 [`user_words_api::remove`] 拆回扁平码；text 是列表给的转义形态、
        // key 是真实文本，不还原就删不掉。
        let (keys, batch) = items_or_single(params, code_text_item)?;
        if batch {
            user_words_api::remove_batch(self, schema, &keys)?;
            return Ok(batch_done(keys.len()));
        }
        let (code, text) = &keys[0];
        user_words_api::remove(self, schema, code, text)?;
        Ok(json!({ "ok": true }))
    }

    /// `dict.reload {paths?: [string]}` → `{reloaded: [schema]}`：不带 paths 全量扫描。
    fn web_dict_reload(&self, params: &Value) -> anyhow::Result<Value> {
        let paths: Option<Vec<String>> =
            params.get("paths").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });
        let reloaded = external_files::reload_dict_files(self, paths.as_deref())?;
        Ok(json!({ "reloaded": reloaded }))
    }

    /// `dict.flush` → `{ok: true, held: [file]}`；`held`：外部编辑中、这次没写回的用户词文件。
    fn web_dict_flush(&self) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        store.flush_user_data()?;
        Ok(json!({ "ok": true, "held": store.held_edit_files() }))
    }

    /// 外部编辑的目标文件：`{path}`，或 `{schemaId, kind: words|shadow|temp|freq|user_dicts}`、
    /// `{schemaId, kind: user_dict, name}`（按数据方案换算，拼音族折叠到 "pinyin"），或
    /// `{kind: phrases|quick_format|common_chars}`、`{kind: charsets, key}`。
    fn edit_target_param(
        &self,
        store: &wind_store::Store,
        params: &Value,
    ) -> anyhow::Result<String> {
        if let Some(p) = params.get("path").and_then(Value::as_str) {
            return Ok(p.to_string());
        }
        let schema = || -> anyhow::Result<String> {
            Ok(self
                .engine_mgr()
                .data_schema_id(str_param(params, "schemaId")?))
        };
        let path = match str_param(params, "kind")? {
            "words" => store.user_text_path(&schema()?),
            "shadow" => store.shadow_path(&schema()?),
            "phrases" => store.phrases_path(),
            "quick_format" => store.quick_format_path(),
            "common_chars" => store.common_chars_path(),
            "user_dict" => wind_store::user_dicts::lib_path(
                store.user_text_dir(),
                &schema()?,
                str_param(params, "name")?,
            ),
            "user_dicts" => wind_store::user_dicts::meta_path(store.user_text_dir(), &schema()?),
            "charsets" => store.charset_path(str_param(params, "key")?)?,
            "temp" => store.user_text_dir().join(schema()?).join("temp.dict.yaml"),
            "freq" => store
                .user_text_dir()
                .join(schema()?)
                .join("freq.table.yaml"),
            other => anyhow::bail!("不支持编辑的类别：{other}"),
        };
        Ok(path.to_string_lossy().into_owned())
    }

    /// `data.beginEdit {path}`（或类别参数，见 [`Self::edit_target_param`]）→
    /// `{path: 绝对路径, file: 相对数据目录, kind}`。只读模式拒绝。
    fn web_data_begin_edit(&self, params: &Value) -> anyhow::Result<Value> {
        if let Some(r) = self.storage_readonly_reason() {
            anyhow::bail!("{r}");
        }
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        if params.get("kind").and_then(Value::as_str) == Some("charsets") {
            self.prepare_charset_edit(store, str_param(params, "key")?)?;
        }
        let path = self.edit_target_param(store, params)?;
        let s = external_files::begin_edit(self, &path)?;
        Ok(json!({
            "path": s.path.to_string_lossy(),
            "file": s.file,
            "kind": s.kind.as_str(),
        }))
    }

    /// 按 key 编辑字符分类：出厂没有、用户层也没有的 key 拒绝；用户层还没有这个类的文件 ⇒ 先写一份
    /// 只带 key 的（说明行 + 头部），编辑器里打开就是可改的样子。
    fn prepare_charset_edit(&self, store: &wind_store::Store, key: &str) -> anyhow::Result<()> {
        if store.get_charset_doc(key)?.is_some() {
            return Ok(());
        }
        if !self.engine_mgr().charset_factory().contains_key(key) {
            anyhow::bail!("没有名为「{key}」的字符分类");
        }
        let doc = wind_config::charset_def::CharsetDoc {
            def: wind_config::charset_def::CharsetDef {
                key: key.to_string(),
                ..Default::default()
            },
            added: Vec::new(),
            removed: Vec::new(),
        };
        store.set_charset_doc(key, &wind_config::charset_def::render_doc(&doc)?)
    }

    /// `data.endEdit {path}`（或类别参数）→ `{changed, issues}`；变了的项重建内存镜像（同 `dict.reload`）。
    /// 问题在重建之后再取：字符分类的校验发生在重建字符分类时。
    fn web_data_end_edit(&self, params: &Value) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let r = external_files::end_edit(self, &self.edit_target_param(store, params)?)?;
        let root = store.user_text_dir();
        let issues: Vec<Value> = store
            .data_issues()
            .iter()
            .filter(|i| i.path == r.path)
            .map(|i| issue_json(root, i))
            .collect();
        Ok(json!({ "changed": r.changed, "issues": issues }))
    }

    /// `data.editSessions` → `[{file, since_secs}]`（设置端重开窗口时恢复编辑条）。
    fn web_data_edit_sessions(&self) -> anyhow::Result<Value> {
        Ok(Value::Array(
            external_files::edit_sessions(self)
                .into_iter()
                .map(|(file, d)| json!({ "file": file, "since_secs": d.as_secs() }))
                .collect(),
        ))
    }

    /// `data.locate {path?}` → `{path, exists}`（`exists`：该目录 / 文件此刻在不在）；`{schemaId}` 定位该方案的数据目录（拼音族折叠）。
    fn web_data_locate(&self, params: &Value) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let schema_dir = match (
            params.get("path"),
            params.get("schemaId").and_then(Value::as_str),
        ) {
            (None, Some(s)) => Some(self.engine_mgr().data_schema_id(s)),
            _ => None,
        };
        let p = store.locate_data_path(
            schema_dir
                .as_deref()
                .or_else(|| params.get("path").and_then(Value::as_str)),
        )?;
        let mut v = json!({ "path": p.to_string_lossy(), "exists": p.exists() });
        if schema_dir.is_some() {
            let id = params["schemaId"].as_str().unwrap_or_default();
            let reason = self.purge_block_reason(id);
            v["purgeable"] = json!(reason.is_none());
            if let Some(r) = reason {
                v["reason"] = json!(r);
            }
        }
        Ok(v)
    }

    fn web_dict_clear(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let n = user_words_api::clear(self, schema)?;
        Ok(json!(n))
    }

    /// 导出方案数据为单个多段 wdict 文件。`sections` 参数选类型；缺省按引擎类型取默认适用段。
    ///
    /// 带 `path` 时由 core 直接写盘、只回报字节数；缺省仍回 `{content}`。理由同
    /// [`dict_source_text`]（整库一帧 JSON 会撞设置端 16MB 读帧上限）。
    fn web_dict_export(&self, params: &Value) -> anyhow::Result<Value> {
        let schema_id = str_param(params, "schemaId")?;
        // 本体（数据方案折叠、engine_type 写头、缺省段按引擎类型）在
        // [`dict_transfer_api::export`]，与移动端共用。
        let content =
            dict_transfer_api::export(self, schema_id, dict_sections_param(params).as_deref())?;
        if let Some(path) = params.get("path").and_then(|v| v.as_str())
            && !path.is_empty()
        {
            std::fs::write(path, &content)
                .map_err(|e| anyhow::anyhow!("写入 {} 失败：{}", path, e))?;
            return Ok(json!({ "path": path, "bytes": content.len() }));
        }
        Ok(json!({ "content": content }))
    }

    /// 导入。WindDict 多段：`sections` 选要应用的类型（默认文件所含全部段）；
    /// Rime/TSV：仅用户词库。返回 `{sections:[...]}` 逐段结果。
    ///
    /// 本体（引擎类型校验、段交集、拼音准入契约）在 [`dict_transfer_api::import`]，与移动端
    /// 共用；这里只解参数（`path`/`content`、`strategy`、`fillBoundary`、`sections`）与拼 JSON。
    fn web_dict_import(&self, params: &Value) -> anyhow::Result<Value> {
        use wind_transfer::merge::Strategy;
        let schema_id = str_param(params, "schemaId")?;
        let content = dict_source_text(params)?;
        let replace = Strategy::from_param(
            params
                .get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        ) == Strategy::Replace;
        // 导入对话框的二选一（§5）。缺省 true：旧客户端与命令行不传这个参数。
        let fill_boundary = params
            .get("fillBoundary")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let out = dict_transfer_api::import(
            self,
            schema_id,
            content.as_ref(),
            replace,
            fill_boundary,
            dict_sections_param(params),
        )?;
        Ok(dict_report_json(&out))
    }

    /// 导入预览。回报文件含哪些段及各段计数（用户词库另带 willAdd/willUpdate/unchanged/samples）。
    fn web_dict_preview_import(&self, params: &Value) -> anyhow::Result<Value> {
        use wind_store::dict_export::DictSection;
        let schema_id = str_param(params, "schemaId")?;
        let data_schema = self.engine_mgr().data_schema_id(schema_id);
        let content = dict_source_text(params)?;
        let content = content.as_ref();
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let fmt = wind_store::import_formats::detect_dict_format(content);
        if fmt == wind_store::import_formats::DictFormat::WindDict {
            let present = wind_store::wdict::sections_present(content);
            let mut arr: Vec<Value> = Vec::new();
            for tag in &present {
                let Some(sec) = DictSection::from_key(tag) else {
                    continue;
                };
                match sec {
                    DictSection::UserWords => {
                        let (rows, sk) = wind_store::wdict::parse_words_wdict(content)
                            .map_err(|e| anyhow::anyhow!(e))?;
                        let total = rows.len();
                        // 预览必须跑与导入完全相同的准入判据，否则「预计入库 N 条」会骗人。
                        let (rows, ct) = dict_transfer_api::apply_pinyin_entry_contract(
                            self, schema_id, rows, true,
                        );
                        let (c, samples) = store.preview_import_user_words(&data_schema, &rows)?;
                        arr.push(json!({
                            "key": "userWords", "count": total,
                            "willAdd": c.added, "willUpdate": c.updated, "unchanged": c.unchanged,
                            "skipped": sk + ct.rejected, "samples": samples,
                            "rejected": ct.rejected,
                            "rejectedSamples": ct.samples,
                            "boundaryFilled": ct.filled,
                            "boundaryAmbiguous": ct.ambiguous,
                            "boundaryNoReading": ct.no_reading,
                            "noBoundary": ct.no_boundary,
                        }));
                    }
                    DictSection::TempWords => {
                        let (rows, sk) = wind_store::wdict::parse_temp_words_wdict(content)
                            .map_err(|e| anyhow::anyhow!(e))?;
                        let total = rows.len();
                        let (_rows, ct) = dict_transfer_api::apply_pinyin_entry_contract(
                            self, schema_id, rows, true,
                        );
                        arr.push(json!({
                            "key": "tempWords", "count": total,
                            "skipped": sk + ct.rejected,
                            "rejected": ct.rejected,
                            "rejectedSamples": ct.samples,
                            "boundaryFilled": ct.filled,
                            "boundaryAmbiguous": ct.ambiguous,
                            "boundaryNoReading": ct.no_reading,
                            "noBoundary": ct.no_boundary,
                        }));
                    }
                    DictSection::Freq => {
                        let (rows, sk) = wind_store::wdict::parse_freq_wdict(content)
                            .map_err(|e| anyhow::anyhow!(e))?;
                        arr.push(json!({ "key": "freq", "count": rows.len(), "skipped": sk }));
                    }
                    DictSection::Shadow => {
                        let (rows, sk) = wind_store::wdict::parse_shadow_wdict(content)
                            .map_err(|e| anyhow::anyhow!(e))?;
                        arr.push(json!({ "key": "shadow", "count": rows.len(), "skipped": sk }));
                    }
                }
            }
            // 来源方案/引擎（文件头部）+ 与目标方案的兼容性（引擎类型一致或来源未知）。
            let target = self
                .engine_mgr()
                .schema_merged(schema_id)
                .map(|s| resolve_engine_type(&s))
                .unwrap_or("codetable");
            let source_engine =
                wind_store::wdict::read_header_field(content, "engine_type").unwrap_or_default();
            let source_schema =
                wind_store::wdict::read_header_field(content, "schema_id").unwrap_or_default();
            let compatible = source_engine.is_empty() || source_engine == target;
            Ok(json!({
                "format": "winddict", "sections": arr,
                "sourceSchema": source_schema, "sourceEngine": source_engine,
                "targetEngine": target, "compatible": compatible,
            }))
        } else {
            let (fmt2, rows, skipped) = wind_store::import_formats::parse_words_auto(
                content,
                dict_transfer_api::code_policy_for(self, schema_id),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let total = rows.len();
            // 预览必须跑与导入完全相同的准入判据，否则「预计入库 N 条」会骗人。
            let (rows, contract) =
                dict_transfer_api::apply_pinyin_entry_contract(self, schema_id, rows, true);
            let (c, samples) = store.preview_import_user_words(&data_schema, &rows)?;
            // Rime/TSV 无来源引擎元信息，兼容性交由用户判断（不拦截）。
            Ok(json!({
                "format": fmt2.as_str(),
                "sections": [ {
                    "key": "userWords", "count": total,
                    "willAdd": c.added, "willUpdate": c.updated, "unchanged": c.unchanged,
                    "skipped": skipped + contract.rejected, "samples": samples,
                    // 三档处置的计数（见设计文档 §5）：rejected 一律不入库，
                    // noBoundary 是「入库了但仍缺边界」，ambiguous 是「多解已择一」。
                    "rejected": contract.rejected,
                    "rejectedSamples": contract.samples,
                    "boundaryFilled": contract.filled,
                    "boundaryAmbiguous": contract.ambiguous,
                    "boundaryNoReading": contract.no_reading,
                    "noBoundary": contract.no_boundary,
                    // 预览恒按「补充」求解，故没有 boundarySkipped：那个选择由用户在预览后做，
                    // 预览只报有多少条要做这个选择（= boundaryFilled）。
                } ],
                "compatible": true,
            }))
        }
    }

    /// 全表规模的写做完之后，把 redb 只涨不落的读缓存还回去（redb 刷盘时脏页晋升进堆上读缓存，
    /// 上界约 min(配额, 库文件大小)，见 wind-store 的 `tests/redb_cache_high_water.rs`）。
    ///
    /// 失败是真故障：`drop_page_cache` 重试后仍失败时 store 停在暂停态，此后读写全报
    /// `store is paused` 直到重启，故记 `error!`。仍不转成调用方的 `Err`：写已成功落库，
    /// 报失败只会让用户重做；日志是唯一诊断出口，文案须说清后果。
    fn drop_page_cache_after_bulk_write(&self, op: &str) {
        let Some(store) = self.user_store() else {
            return;
        };
        if let Err(e) = store.drop_page_cache() {
            tracing::error!(
                "{op} 之后丢弃页缓存失败：{e}。\
                 数据已写入，但存储层现处于暂停态，后续读写都会失败，请重启服务。"
            );
        }
    }

    fn web_dict_stats(&self) -> anyhow::Result<Value> {
        let store = match self.user_store() {
            Some(s) => s,
            None => return Ok(json!([])),
        };
        let mut out = Vec::new();
        for id in self.engine_mgr().available_schemas().iter() {
            // 只要条数就用 `count_*`，别 `list(..).len()`：那会为一个数字把整张表物化一遍，且每个方案各一遍。
            // 口径与列表一致，见 wind-store 的 `tests/counts_match_listings.rs`。
            // 三个计数同走 `data_schema_id`（与 `dict.listPaged` 等读路径同源），直传原始 id 会查空桶恒得 0。
            // 因此全拼与双拼显示同一个数（拼音族共享数据）；混输方案通常为 0（写端按来源落到目标方案，
            // 见 `write_data_schema_id`），这都是设计使然。
            let data_schema = self.engine_mgr().data_schema_id(id);
            let user_words = store.count_user_words(&data_schema).unwrap_or(0);
            let temp_words = store.count_temp_words(&data_schema).unwrap_or(0);
            let shadow_rules = store
                .list_shadow_rules(&data_schema)
                .map(|v| {
                    v.iter()
                        .map(|(_, r)| r.pinned.len() + r.deleted.len())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            out.push(json!({
                "schemaId": id,
                "name": self.engine_mgr().schema_name(id),
                "userWords": user_words,
                "tempWords": temp_words,
                "shadowRules": shadow_rules,
            }));
        }
        Ok(json!(out))
    }

    fn web_schema_get_config(&self, params: &Value) -> anyhow::Result<Value> {
        let id = str_param(params, "id")?;
        match self.engine_mgr().schema_merged(id) {
            Some(schema) => {
                let etype = resolve_engine_type(&schema);
                let mut v = serde_json::to_value(schema)?;
                // 确保 engine.type 为解析后的权威类型（schema.toml 可能未显式声明）
                if let Some(eng) = v.get_mut("engine").and_then(|e| e.as_object_mut()) {
                    eng.insert("type".to_string(), json!(etype));
                }
                // 码表方案附带「当前生效值」旁路字段：`engine.codetable` 的行为字段未设置时为 null，
                // 设置页需要知道实际按什么跑，而基线分普通/特殊两种（见 codetable_baseline），UI 算不出来。
                // 与 `engine.codetable`（显式写了什么，决定是否落盘）分开放，否则「跟随基线」与
                // 「显式等于基线值」无从区分。
                if etype == "codetable" {
                    let eff = self.engine_mgr().effective_codetable(id);
                    // 「取消覆盖后会变成什么」——设置页每一项的三态控件未勾选时显示它。
                    let followed = self.engine_mgr().followed_codetable(id);
                    // 「哪些项是用户自己改的」只能问 override 层：合并值分不清是方案作者写的还是用户改的。
                    let ov = self
                        .engine_mgr()
                        .get_schema_override(id)
                        .and_then(|t| {
                            t.get("engine")
                                .and_then(|e| e.get("codetable"))
                                .map(|c| serde_json::to_value(c.clone()))
                        })
                        .transpose()?
                        .unwrap_or_else(|| json!({}));
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("effectiveCodetable".to_string(), serde_json::to_value(eff)?);
                        obj.insert(
                            "followedCodetable".to_string(),
                            serde_json::to_value(followed)?,
                        );
                        obj.insert("codetableOverride".to_string(), ov);
                    }
                }
                // override 层的 `[punct]` 段，用来分辨自定义标点表的来源（合并值有、这里没有 ⇒ 方案作者写的）。
                // 设置页当前还没用上，见 `READONLY_SIDECAR_FIELDS`。不给「跟随全局时」的旁路字段：
                // 那就是全局 `input.punct`，设置页已有，再发一遍就是第二个真相源。
                let punct_ov = self
                    .engine_mgr()
                    .get_schema_override(id)
                    .and_then(|t| t.get("punct").map(|p| serde_json::to_value(p.clone())))
                    .transpose()?
                    .unwrap_or_else(|| json!({}));
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("punctOverride".to_string(), punct_ov);
                }
                // 首码集里的符号键（键名形式），给设置页判按键功能绑定是否与本方案码元首码冲突
                // （内核绑定优先，该符号起头的编码会静默失效，见 schema-key-actions.md §4.3）。
                // 给键名而不是 `input_chars` 原文，免得设置端再写一份区间语法解析器。组装放这里而不在引擎，
                // 因为引擎不该依赖 `wind-keys`。只报符号键：字母绑功能键另有 `bound_action_key_yields` 裁决。
                let leading_keys: Vec<&str> = match self.engine_mgr().schema_code_char_set(id) {
                    Some(set) => wind_keys::keymap::symbol_keys()
                        .filter(|(_, ch)| set.contains_leading(*ch))
                        .map(|(name, _)| name)
                        .collect(),
                    // 读不到方案就不提示——凭空报一个不存在的冲突比不报更糟。
                    None => Vec::new(),
                };
                // 按键总览（只读旁路）：本方案下每个绑过的键当前干什么、来自哪一层。
                // 组装在这里而不是设置页：全局那份要经 `effective_session_actions` 折算，
                // 展开规则只该有一处。见 [`keys_overview`]。
                let (overview, overview_degraded) = keys_overview(&v);
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("leadingCodeKeys".to_string(), json!(leading_keys));
                    obj.insert("keysOverview".to_string(), Value::Array(overview));
                    // 全局层不可信时的说明（见 [`keys_overview`]）。恒在，无降级时为 `null`。
                    obj.insert("keysOverviewDegraded".to_string(), overview_degraded);
                    // 方案对话框「跟随全局（值）」要显示的值（R8.2）：方案没覆盖时实际按什么跑。
                    // 不让设置页自己从全局配置里拼：字体那一档要回落到主题，而主题解析只在内核。
                    obj.insert("followedBehavior".to_string(), self.followed_behavior());
                    // 方案文件自己声明的辅助码来源（不含 override 层），设置页据此区分「跟随方案」与
                    // 「用户选的」（合并值 `engine.aux_code.files` 分不清）。
                    let base_files = self
                        .engine_mgr()
                        .schema_base(id)
                        .map(|s| s.engine.aux_code.files)
                        .unwrap_or_default();
                    obj.insert("auxCodeBaseFiles".to_string(), json!(base_files));
                }
                Ok(v)
            }
            None => Ok(json!({})),
        }
    }

    /// `schema.overrideSummary`：`{ "<全局键>": ["<方案 id>", ...] }`。
    ///
    /// 只数 `available_schemas()`（用户启用、未被定制版 hide 的方案）。按合并后的配置判
    /// （方案文件 ⊕ override），一个方案对一个键至多计一次；数组顺序即方案列表顺序。
    fn web_schema_override_summary(&self) -> anyhow::Result<Value> {
        let mgr = self.engine_mgr();
        let mut out = serde_json::Map::new();
        for id in mgr.available_schemas() {
            let Some(s) = mgr.schema_merged(&id) else {
                continue;
            };
            for key in wind_config::config_schema::schema_overridden_keys(&s) {
                if let Value::Array(ids) = out.entry(key).or_insert_with(|| json!([])) {
                    ids.push(json!(id));
                }
            }
        }
        Ok(Value::Object(out))
    }

    /// `getConfig.followedBehavior` 的组装：读盘上的全局配置（与 [`keys_overview`] 同一取法）
    /// + 当前主题的字体。
    fn followed_behavior(&self) -> Value {
        // 读不到全局配置时按出厂值给：这里只是「跟随时显示什么」的提示值，不参与写盘。
        let cfg = wind_config::Config::load(wind_config::Config::data_dir().as_deref())
            .unwrap_or_default();
        let theme_font = self.theme_follow_values().and_then(|t| t.font_family);
        followed_behavior_of(&cfg, theme_font)
    }

    fn web_schema_save_config(&self, params: &Value) -> anyhow::Result<Value> {
        let id = str_param(params, "id")?;
        let cfg = params
            .get("cfg")
            .ok_or_else(|| anyhow::anyhow!("缺少参数 cfg"))?;
        let cfg = &strip_readonly_fields(cfg);
        let base = self
            .engine_mgr()
            .schema_base(id)
            .ok_or_else(|| anyhow::anyhow!("方案不存在: {}", id))?;
        let base_json = serde_json::to_value(&base)?;
        // 稀疏 diff（仅变化项）写入 override 层，让方案文件后续更新仍能透传未改项。
        let mut diff = json_diff(&base_json, cfg).unwrap_or(json!({}));
        // `dictionaries` 不经本入口写（唯一写入口是 setDictEnabled），只把既有 override 里的那份原样带过去。
        // 传入的 cfg 是打开对话框时的快照，而设置端先 setDictEnabled 再 saveConfig；若照 diff 写，
        // 旧数组会冲掉刚落盘的开关并冻结整份词库定义（见 `schema_dicts_api` 的 `sparse_dict_overrides`）。
        // 在入口剥掉，不指望每个调用方（CLI、旧版设置端）先刷新快照。
        if let Some(o) = diff.as_object_mut() {
            o.remove("dictionaries");
        }
        let mut ov = json_to_toml(&diff);
        if let toml::Value::Table(t) = &mut ov
            && let Some(prev) = self.engine_mgr().get_schema_override(id)
            && let Some(d) = prev.get("dictionaries")
        {
            t.insert("dictionaries".to_string(), d.clone());
        }
        self.engine_mgr().write_schema_override(id, &ov)?;
        // 吃键集等跨方案派生集合只在 ConfigBundle 构建时算；方案 override 不在 config.toml
        // 里，不在此重建就要等重启 / 下次改全局设置才生效。
        self.refresh_schema_derived_config();
        Ok(json!({ "ok": true }))
    }

    /// 设置页「辅助码来源」下拉的两组可选项，见 `docs/design/aux-code-schema-source.md` §6。
    ///
    /// 码表名只读文件首行（`read_name`），缺头时退回文件名——列出来比滤掉好查。
    fn web_schema_aux_code_sources(&self, params: &Value) -> anyhow::Result<Value> {
        let exclude = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let o = self.engine_mgr().aux_code_source_options(exclude);
        let schemas: Vec<Value> = o
            .schemas
            .iter()
            .map(|(id, name)| {
                // role（空 / "aux"）：设置页据此把辅助码专用方案排到普通方案前面。
                let role = self
                    .engine_mgr()
                    .schema_merged(id)
                    .map(|s| s.schema.role.clone())
                    .unwrap_or_default();
                json!({ "id": id, "name": name, "role": role })
            })
            .collect();
        let files: Vec<Value> = o
            .files
            .iter()
            .map(|(rel, abs)| {
                let label = wind_aux_code::read_name(abs).unwrap_or_else(|| {
                    abs.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
                json!({ "path": rel, "label": label })
            })
            .collect();
        Ok(json!({ "schemas": schemas, "files": files }))
    }

    fn web_schema_reset_config(&self, params: &Value) -> anyhow::Result<Value> {
        let id = str_param(params, "id")?;
        self.engine_mgr().delete_schema_override(id)?;
        self.refresh_schema_derived_config(); // 同 saveConfig（GH#144）
        Ok(json!({ "ok": true }))
    }

    /// 本体在 [`wind_coordinator::schema_dicts_api::set_enabled`]（与移动端共用）：
    /// override 只落 `{id, enabled}` 稀疏项、先持久化再 live 翻（含混输扇出）、`user:` 分库
    /// 走 `dicts.toml`，都在那里。
    fn web_schema_set_dict_enabled(&self, params: &Value) -> anyhow::Result<Value> {
        let id = str_param(params, "id")?;
        let dict_id = str_param(params, "dictId")?;
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let live = wind_coordinator::schema_dicts_api::set_enabled(self, id, dict_id, enabled)?;
        Ok(json!({ "ok": true, "live": live }))
    }

    fn web_schema_delete(&self, params: &Value) -> anyhow::Result<Value> {
        let id = str_param(params, "id")?;
        if !self.engine_mgr().is_user_schema(id) {
            anyhow::bail!("内置方案不可删除: {id}");
        }
        let user = Self::user_schemas_dir()?;
        let system = Self::system_schemas_dirs();
        // 共享检查基准 = 其余已安装方案(含内置——混输可能引用用户资源)。
        let keep: Vec<String> = self
            .engine_mgr()
            .installed_schemas()
            .into_iter()
            .filter(|s| s != id)
            .collect();
        // 镜像导入的收集逻辑删文件:方案文件+引用资源+递归引用的用户方案,共享保留。
        let r = wind_transfer::scheme::delete_package(id, &user, &system, &keep)?;
        // 级联清词库数据:仅清数据域=方案自身的(拼音族数据在共享 pinyin 域,
        // data_schema_id≠自身时跳过;文件已删读不到类型时回落自身,清空域无害)。
        //
        // 清库出错不得跳过下方收尾（文件已删，不收尾则运行时状态到重启才与磁盘一致）：先记错误，收尾后再返回。
        let cleared: anyhow::Result<()> = (|| {
            if let Some(store) = self.user_store() {
                for sid in &r.schema_ids {
                    if self.engine_mgr().data_schema_id(sid) == *sid {
                        store.clear_user_words(sid)?;
                        store.clear_temp_words(sid)?;
                        // 草稿层同属该方案的数据域；不清的话 TTL 到期前仍会被召回并跃迁进已不存在的方案的临时词库。
                        store.clear_drafts(sid)?;
                        store.clear_freq(sid)?;
                        store.clear_assoc_history(sid)?;
                        store.clear_shadow(sid)?;
                    }
                }
            }
            Ok(())
        })();
        for sid in &r.schema_ids {
            self.engine_mgr().forget_deleted_schema(sid);
        }
        // 被删方案的自定义标点 / 码元首码 / 引导键仍在吃键集里：不重建的话，DLL 会继续吃这些键
        // 再原样吐回（非 TSF 宿主上即「吃了再吐」），直到重启。
        self.refresh_schema_derived_config();
        cleared?;
        Ok(json!({
            "ok": true,
            "deleted": r.deleted,
            "keptShared": r.kept_shared,
            "schemaIds": r.schema_ids,
        }))
    }

    /// 某方案写入的全部数据方案：非混输 = 自身的数据方案；混输 = 主方案的数据方案 + 有拼音子方案时的
    /// `pinyin`（与 `EngineManager::write_data_schema_id` 的分流一致）。
    fn written_data_schemes(&self, id: &str) -> Vec<String> {
        let em = self.engine_mgr();
        if em.schema_engine_type(id).as_deref() != Some("mixed") {
            return vec![em.data_schema_id(id)];
        }
        let mut v = Vec::new();
        if em.mixed_primary_schema(id).is_some() {
            v.extend(em.write_data_schema_id(id, wind_candidate::CandidateSource::CodeTable));
        }
        if em.mixed_secondary_schema(id).is_some() {
            v.extend(em.write_data_schema_id(id, wind_candidate::CandidateSource::Pinyin));
        }
        v
    }

    /// `userdata.purgeSchema` 拒绝的原因（能删 ⇒ None）；`data.locate` 的 `purgeable` / `reason`
    /// 用同一个判据。未安装的不删（防把 `stats` 之类的保留目录当方案）；混输的数据在主方案与拼音
    /// 名下，自身不删；数据方案被别的已装方案写入（拼音族、混输的主 / 拼音一侧）不单删。
    fn purge_block_reason(&self, id: &str) -> Option<String> {
        let em = self.engine_mgr();
        let installed = em.installed_schemas();
        if !installed.iter().any(|s| s == id) {
            return Some(format!("未安装方案 {id}"));
        }
        if em.schema_engine_type(id).as_deref() == Some("mixed") {
            let mut at: Vec<String> = Vec::new();
            if let Some(p) = em.mixed_primary_schema(id) {
                at.push(format!("「{}」", em.schema_name(&p)));
            }
            if em.mixed_secondary_schema(id).is_some() {
                at.push("拼音".into());
            }
            return Some(if at.is_empty() {
                "混输方案没有自己的数据".into()
            } else {
                format!("混输方案的数据在{}里，请在那里删除", at.join("和"))
            });
        }
        let ds = em.data_schema_id(id);
        installed
            .iter()
            .find(|s| *s != id && self.written_data_schemes(s).contains(&ds))
            .map(|other| format!("与「{}」共用数据，不能单独删除", em.schema_name(other)))
    }

    /// `userdata.purgeSchema {schemaId}` → `{ok, removed: [相对路径]}`：删掉该方案的全部用户数据
    /// ——数据方案目录（用户词 / Shadow / 学到的数据 / 分库与开关）、`.backup/` 里它的那份、
    /// `schema_overrides/<id>.toml`。数据方案与别的已装方案共用（拼音族）⇒ 拒绝；有文件正在外部
    /// 编辑 ⇒ 拒绝。统计不动。`removed` 里数据文件相对数据目录，方案覆盖写 `schema_overrides/<id>.toml`。
    fn web_userdata_purge_schema(&self, params: &Value) -> anyhow::Result<Value> {
        if let Some(r) = self.storage_readonly_reason() {
            anyhow::bail!("{r}");
        }
        let id = str_param(params, "schemaId")?;
        if let Some(r) = self.purge_block_reason(id) {
            anyhow::bail!("{r}");
        }
        let ds = self.engine_mgr().data_schema_id(id);
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let r = store.purge_data_schema(&ds);
        // 删到一半失败也失效：分库文件可能已删，引擎不能拿着旧数据
        self.apply_user_text_changes(&[
            ds.clone(),
            format!("{}{ds}", wind_store::user_words::USER_DICTS_CHANGED),
        ]);
        let mut removed = r?;
        if self.engine_mgr().delete_schema_override(id)? {
            removed.push(format!("schema_overrides/{id}.toml"));
        }
        self.refresh_schema_derived_config();
        Ok(json!({ "ok": true, "removed": removed }))
    }

    /// 用户 schemas 根目录(%APPDATA%/WindInput/schemas),不存在则创建。
    fn user_schemas_dir() -> anyhow::Result<std::path::PathBuf> {
        let dir = wind_config::Config::user_config_dir()
            .ok_or_else(|| anyhow::anyhow!("无用户配置目录"))?
            .join("schemas");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// 系统 schemas 根目录们：非用户层的每一层（`data_custom` 在前、`data` 在后）的
    /// `schemas/`，只保留真实存在的（测试环境可能一个都没有）。
    ///
    /// 必须含 `data_custom`：否则只存在于定制层的方案会被 `wind_transfer::scheme::locate` 判成
    /// `Missing`，导出空包、删除记不上 `system_refs`。定制层与出厂层同类：导出一并打包，删除永不触碰。
    fn system_schemas_dirs() -> Vec<std::path::PathBuf> {
        wind_config::Config::resource_layers_named()
            .into_iter()
            .filter(|l| !l.is_user())
            .map(|l| l.path.join("schemas"))
            .filter(|d| d.is_dir())
            .collect()
    }

    fn web_scheme_export_package(&self, params: &Value) -> anyhow::Result<Value> {
        let id = str_param(params, "id")?;
        let out = str_param(params, "path")?;
        let user = Self::user_schemas_dir()?;
        let system = Self::system_schemas_dirs();
        // 设置页方案定制层(schema_overrides/<id>.toml,见 write_schema_override):
        // 导出必须带上,否则定制过的方案(如换过双拼布局)导出的是未定制版本,
        // override 新指向的资源文件也会漏打包。
        let override_dir =
            wind_config::Config::user_config_dir().map(|d| d.join("schema_overrides"));
        let r = wind_transfer::scheme::export_package(
            id,
            &user,
            &system,
            override_dir.as_deref(),
            std::path::Path::new(out),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            &chrono::Local::now().to_rfc3339(),
        )?;
        Ok(json!({
            "path": r.path.to_string_lossy(),
            "packed": r.packed,
            "systemRefs": r.system_refs,
            "missing": r.missing,
        }))
    }

    fn web_scheme_import_package(&self, params: &Value) -> anyhow::Result<Value> {
        use wind_transfer::merge::Strategy;
        let path = str_param(params, "path")?;
        let strategy = Strategy::from_param(
            params
                .get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        let user = Self::user_schemas_dir()?;
        let r = wind_transfer::scheme::import_package(std::path::Path::new(path), &user, strategy)?;
        // 覆盖已加载方案时失效缓存(新方案为安全 no-op);列表可见性由 installed_schemas 实时扫盘天然生效。
        for id in &r.schema_ids {
            self.engine_mgr().invalidate_schema(id);
        }
        // 吃键集随之重建：进了 `available` 的方案稍后会经 config.setItems 自愈，但 overlay 方案
        // 不进 `available`，不在此重建的话它的自定义标点 / 符号码元 / 引导键会一直被透传到重启。
        if !r.schema_ids.is_empty() {
            self.refresh_schema_derived_config();
        }
        Ok(json!({
            "imported": r.imported,
            "conflicts": r.conflicts,
            "schemaIds": r.schema_ids,
        }))
    }

    fn web_scheme_preview_import(&self, params: &Value) -> anyhow::Result<Value> {
        let path = str_param(params, "path")?;
        let user = Self::user_schemas_dir()?;
        let p = wind_transfer::scheme::preview_import(std::path::Path::new(path), &user)?;
        Self::scheme_preview_json(&p)
    }

    /// `scheme.previewImportText { text }`：文本信封的只读预览。
    /// 文本不是信封时错误以 `not_schema_text:` 开头，设置端据此回落配置片段管线。
    fn web_scheme_preview_import_text(&self, params: &Value) -> anyhow::Result<Value> {
        let text = str_param(params, "text")?;
        let user = Self::user_schemas_dir()?;
        let p = wind_transfer::envelope::preview_import_text(text, &user)?;
        Self::scheme_preview_json(&p)
    }

    /// `scheme.importText { text, strategy? }`：文本信封落盘。响应形状与
    /// `scheme.importPackage` 一致；同样不应用包内配置片段（设置端两步编排）。
    fn web_scheme_import_text(&self, params: &Value) -> anyhow::Result<Value> {
        use wind_transfer::merge::Strategy;
        let text = str_param(params, "text")?;
        let strategy = Strategy::from_param(
            params
                .get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        let user = Self::user_schemas_dir()?;
        let r = wind_transfer::envelope::import_text(text, &user, strategy)?;
        // 覆盖已加载方案时失效缓存（新方案为安全 no-op），与 path 版同处置。
        for id in &r.schema_ids {
            self.engine_mgr().invalidate_schema(id);
        }
        if !r.schema_ids.is_empty() {
            self.refresh_schema_derived_config(); // 同 path 版（GH#144 同根）
        }
        Ok(json!({
            "imported": r.imported,
            "conflicts": r.conflicts,
            "schemaIds": r.schema_ids,
        }))
    }

    /// 预览响应的公共形状(zip 与文本信封共用,两条路复用同一个确认对话框)。
    ///
    /// 包级说明(`title` / `description`,来自 package.toml / 信封 `[package]`)提到顶层供对话框显示,
    /// 空则不输出。与 `configPatch.info`(片段自带说明)各显各的、不合并(§2.3)。
    fn scheme_preview_json(
        p: &wind_transfer::scheme::SchemeImportPreview,
    ) -> anyhow::Result<Value> {
        let mut out = json!({
            // v2:包元信息来自可选 package.toml(缺失时各字段为空串,前端显示"未知")。
            "package": serde_json::to_value(&p.meta)?,
            "willAdd": p.will_add,
            "conflicts": p.conflicts,
            "systemRefs": p.system_refs,
            "missing": p.missing,
        });
        if !p.meta.package.title.is_empty() {
            out["title"] = json!(p.meta.package.title);
        }
        if !p.meta.package.description.is_empty() {
            out["description"] = json!(p.meta.package.description);
        }
        if let Some(text) = &p.config_patch {
            out["configPatch"] = Self::config_patch_diff(text)?;
        }
        Ok(out)
    }

    /// 包内配置片段的逐键 diff(`{ text, ok, entries, info? }`),形状与 `config.previewPatch`
    /// 同源。`info` 是该片段自己的说明(config_patch.toml 的 `[package]` 段),与包级说明不合并。
    ///
    /// 只预览不应用:应用编排在设置端(导入方案文件 → `config.applyPatch`),那条路才有热重载与镜像回灌。
    fn config_patch_diff(text: &str) -> anyhow::Result<Value> {
        let fragment = wind_config::patch::parse_fragment(text)
            .map_err(|e| anyhow::anyhow!("包内配置片段无法解析: {e}"))?;
        // 说明非法即整体错误(与解析失败同级),判据与 config.previewPatch 完全一致。
        let info = wind_config::patch::extract_info(&fragment)
            .map_err(|e| anyhow::anyhow!("包内配置片段说明非法: {e}"))?;
        let cfg = wind_config::Config::load(wind_config::Config::data_dir().as_deref())?;
        // `degradation` 是 `#[serde(skip)]`，转成值树就没了，故先留一份。
        let degradation = cfg.degradation.clone();
        let current = cfg.to_toml_value()?;
        let mut entries = wind_config::patch::preview(&fragment, &current);
        // 与 `config.previewPatch` 同判据（落盘闸在 `config.applyPatch`，两处须一致）：降级时
        // Map 键的当前表是出厂值，不标出来用户看到的 diff 就是假的。
        wind_config::patch::mark_degraded_seeds(&mut entries, &degradation);
        let ok = entries.iter().all(|e| e.error.is_none());
        let mut out = json!({ "text": text, "ok": ok, "entries": entries });
        if let Some(info) = info {
            out["info"] = serde_json::to_value(info)?;
        }
        Ok(out)
    }

    fn web_backup_create(&self, params: &Value) -> anyhow::Result<Value> {
        use wind_transfer::backup::{BackupOptions, BackupSources, create_backup};
        let out = str_param(params, "path")?;
        let include_stats = params
            .get("includeStats")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_state = params
            .get("includeState")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let user_dir = wind_config::Config::user_config_dir();
        let cfg_file = user_dir.as_ref().map(|d| d.join("config.toml"));
        let compat_file = user_dir.as_ref().map(|d| d.join("compat.toml"));
        let schemas_dir = user_dir.as_ref().map(|d| d.join("schemas"));
        let schema_overrides_dir = user_dir.as_ref().map(|d| d.join("schema_overrides"));
        let themes_dir = user_dir.as_ref().map(|d| d.join("themes"));
        let state_file = wind_config::Config::state_dir().map(|d| d.join("state.toml"));
        let src = BackupSources {
            user_config_file: cfg_file.as_deref(),
            compat_file: compat_file.as_deref(),
            user_schemas_dir: schemas_dir.as_deref(),
            user_schema_overrides_dir: schema_overrides_dir.as_deref(),
            user_themes_dir: themes_dir.as_deref(),
            state_file: state_file.as_deref(),
        };
        let r = create_backup(
            store,
            &src,
            std::path::Path::new(out),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            &chrono::Local::now().to_rfc3339(),
            &BackupOptions {
                include_stats,
                include_state,
            },
        )?;
        let manifest = wind_transfer::bundle::read_manifest(&r.path)?;
        Ok(json!({
            "path": r.path.to_string_lossy(),
            "manifest": serde_json::to_value(&manifest)?,
        }))
    }

    fn web_backup_inspect(&self, params: &Value) -> anyhow::Result<Value> {
        let path = str_param(params, "path")?;
        let manifest = wind_transfer::bundle::read_manifest(std::path::Path::new(path))?;
        Ok(json!({ "manifest": serde_json::to_value(&manifest)? }))
    }

    fn web_backup_restore(&self, params: &Value) -> anyhow::Result<Value> {
        use wind_transfer::backup::{RestoreTargets, restore_backup};
        use wind_transfer::merge::Strategy;
        let path = str_param(params, "path")?;
        let strategy = Strategy::from_param(
            params
                .get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        let sections: Option<Vec<String>> = params.get("sections").and_then(|v| {
            v.as_array().map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        });
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let user_dir = wind_config::Config::user_config_dir();
        let cfg_file = user_dir.as_ref().map(|d| d.join("config.toml"));
        let compat_file = user_dir.as_ref().map(|d| d.join("compat.toml"));
        let schemas_dir = user_dir.as_ref().map(|d| d.join("schemas"));
        let schema_overrides_dir = user_dir.as_ref().map(|d| d.join("schema_overrides"));
        let themes_dir = user_dir.as_ref().map(|d| d.join("themes"));
        let state_file = wind_config::Config::state_dir().map(|d| d.join("state.toml"));
        let targets = RestoreTargets {
            user_config_file: cfg_file.as_deref(),
            compat_file: compat_file.as_deref(),
            user_schemas_dir: schemas_dir.as_deref(),
            user_schema_overrides_dir: schema_overrides_dir.as_deref(),
            user_themes_dir: themes_dir.as_deref(),
            state_file: state_file.as_deref(),
        };
        // 还原期间挂起统计收集器：后台 flush 若落在「还原写完、resume 之前」，会把还原前的
        // 今天与总计写回去。还原失败也要解除挂起。
        let collector = self.stat_collector();
        if let Some(c) = collector {
            c.suspend();
        }
        let r = match restore_backup(
            std::path::Path::new(path),
            store,
            &targets,
            strategy,
            sections.as_deref(),
        ) {
            Ok(r) => r,
            Err(e) => {
                if let Some(c) = collector {
                    c.unsuspend();
                }
                return Err(e);
            }
        };
        // 刷新:config 域生效、短语重建、涉及方案失效缓存(未加载时安全 no-op)。
        let touched_config = r.restored.iter().any(|p| p.starts_with("config/"));
        let touched_phrase = r.restored.iter().any(|p| p == "userdata/phrases.wdict");
        // 字符类用户层进了库，运行时 registry 还是旧的——不重装就是「还原成功、判定没变」。
        let touched_charsets = r.restored.iter().any(|p| p.starts_with("charsets/"));
        for id in &r.schemas_touched {
            self.engine_mgr().invalidate_schema(id);
        }
        for p in &r.restored {
            if let Some(rel) = p.strip_prefix("schemas/")
                && let Some(id) = rel.strip_suffix(".schema.toml")
                && !id.contains('/')
            {
                self.engine_mgr().invalidate_schema(id);
            }
            // schema_overrides/<id>.toml 也是方案配置合并层，改动后须失效该方案引擎缓存。
            if let Some(rel) = p.strip_prefix("schema_overrides/")
                && let Some(id) = rel.strip_suffix(".toml")
                && !id.contains('/')
            {
                self.engine_mgr().invalidate_schema(id);
            }
        }
        if touched_phrase {
            // Replace 还原会先 `reset_user_phrases`，遮蔽系统条目的用户行被删时那些系统短语也一并消失，
            // 须补回（与设置页「清空用户短语」同一约束）。
            self.restore_missing_system_phrases("备份还原");
            self.rebuild_phrases();
        }
        // 方案文件 / 方案 override 变了：吃键集的跨方案并集随之过期。只看这两个前缀
        // （`r.schemas_touched` 是数据域，不进并集）。`reload_user_config` 成功时自带同一套重建，
        // 故二者择一；它返回 `true` 表示加载失败（bundle 未重建），那时仍要单独补一次。
        let touched_schemas = r
            .restored
            .iter()
            .any(|p| p.starts_with("schemas/") || p.starts_with("schema_overrides/"));
        let reload_failed = touched_config && self.reload_user_config();
        if touched_schemas && (!touched_config || reload_failed) {
            self.refresh_schema_derived_config();
        }
        if touched_charsets {
            self.reload_charsets();
        }
        // 常用字 / 快捷格式的内存镜像、统计收集器都要跟着还原后的数据重载：收集器不 resume 的话，
        // 它下一次写盘会把还原前的「今天」和总计写回去。
        let touched = |p: &str| r.restored.iter().any(|x| x == p);
        let mut changed = Vec::new();
        if touched("userdata/common_chars.jsonl") {
            changed.push("common_chars".to_string());
        }
        if touched("userdata/quick_format.yaml") {
            changed.push("quick_format".to_string());
        }
        // 用户分库（`data/<数据方案>/…`）写回了：失效用到该数据方案的方案
        for p in &r.restored {
            if let Some((ds, _)) = p.strip_prefix("data/").and_then(|r| r.split_once('/')) {
                let k = format!("{}{ds}", wind_store::user_words::USER_DICTS_CHANGED);
                if !changed.contains(&k) {
                    changed.push(k);
                }
            }
        }
        self.apply_user_text_changes(&changed);
        if let Some(c) = collector {
            if touched("userdata/stats.jsonl") || touched("userdata/stats_meta.json") {
                c.resume();
            } else {
                c.unsuspend();
            }
        }
        Ok(json!({
            "restored": r.restored,
            "conflicts": r.conflicts,
            "schemasTouched": r.schemas_touched,
        }))
    }

    fn web_dict_encode(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        // 同 dict.genPinyin：取码基于真实文本，不能拿转义形态去逐字反查。
        let text = store_text(str_param(params, "text")?);
        let code = self
            .encode_texts(schema, std::slice::from_ref(&text))
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok(json!(code))
    }

    /// 批量出码（纯词列表导入）。规则与 `dict.encode` 完全一致——两者共用
    /// [`Self::encode_texts`]，避免两条出码口径各自漂移。
    ///
    /// 契约：`codes` 与入参 `texts` 同序等长，出不了码的位置为空串；调用方靠下标配回词，
    /// 故非字符串元素也要占位，不能 filter 掉。
    fn web_dict_encode_words(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let texts: Vec<String> = params
            .get("texts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|x| store_text(x.as_str().unwrap_or("")))
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({ "codes": self.encode_texts(schema, &texts) }))
    }

    /// 出码统一入口：拼音类方案出拼音码，其余（码表/混输）按方案 `[[encoder.rules]]` 出词组码，
    /// 单字直取其全码（词组公式不接受单字，见 `EngineManager::encode_words`）。
    /// 返回与 `texts` 同序等长，失败位置为空串。一次性准备在这一层之下完成，单元素切片无额外代价。
    fn encode_texts(&self, schema: &str, texts: &[String]) -> Vec<String> {
        if texts.is_empty() {
            return Vec::new();
        }
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let is_pinyin = self
            .engine_mgr()
            .schema_engine_type(schema)
            .map(|t| t == "pinyin")
            .unwrap_or(false);
        if !is_pinyin {
            // 与自动造词/快捷加词同一取码入口（码源=码表词库自身，规则=方案声明的公式），
            // 见 docs/design/codetable-auto-phrase.md §2「码源统一」。
            return self.engine_mgr().encode_words(schema, &refs);
        }
        // 优先词级消歧（多音字按词典权重），引擎无果时回退逐字反查表。
        // 两条路都回带空格的音节码（与 word_item 同形）；写入侧 normalize_add_code 会拆回扁平 key，
        // 并把空格当作显式切分采信。
        let generated = self.engine_mgr().generate_words_pinyin(schema, &refs);
        // 反查表的读锁在循环外取一次：逐词加锁在万级批量上纯属浪费。
        let reverse = self
            .reverse_lookup()
            .read()
            .unwrap_or_else(|e| e.into_inner());
        generated
            .into_iter()
            .zip(&refs)
            .map(|(code, text)| code.unwrap_or_else(|| reverse.gen_pinyin(text)))
            .collect()
    }

    /// 为词语生成拼音码：优先用拼音引擎词级消歧（活跃方案→"pinyin"方案），
    /// 都无果时回退逐字反查表（pinyin_map.txt）。用于 dict.genPinyin（无方案上下文）。
    ///
    /// 同 `dict.encode`：回带空格的音节码，写入侧负责拆回扁平 key。
    fn gen_pinyin_word(&self, text: &str) -> String {
        let active = self.engine_mgr().active_schema_id();
        self.engine_mgr()
            .generate_word_pinyin(&active, text)
            .or_else(|| self.engine_mgr().generate_word_pinyin("pinyin", text))
            .unwrap_or_else(|| {
                self.reverse_lookup()
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .gen_pinyin(text)
            })
    }

    fn web_freq_list_paged(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let prefix = params.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
        let offset = usize_param(params, "offset", 0);
        let limit = usize_param(params, "limit", 50);
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let sort = parse_sort(params, &["code", "text", "count", "lastUsed"]);
        // 无搜索且无排序：走 store 分页快路径；否则全量拉取
        //（编码前缀 ∪ 词条内容包含）→ 排序 → 内存切片。
        let (page, total) = if prefix.is_empty() && sort.is_none() {
            store.list_freq_paged(&schema, "", offset, limit)?
        } else {
            // 词频表的 key 是扁平码；用户可能从用户词库列表复制带空格的串来搜，先拆
            //（对无空格串恒等，故无副作用）。
            let (code_prefix, _) = wind_store::wdict::split_spaced_code(prefix);
            let mut all = if prefix.is_empty() {
                // 只排序不搜索：排序是跨页全局的，只能全量物化。
                store.list_freq_paged(&schema, &code_prefix, 0, 0)?.0
            } else {
                // 一遍扫描分两组，编码前缀命中在前、其余命中（词条内容包含、编码中段包含）在后。
                // 两组天然互斥，无需去重。
                let q = prefix.to_lowercase();
                let code_q = code_prefix.to_lowercase();
                let mut head = Vec::new();
                let mut tail = Vec::new();
                store.for_each_freq(&schema, "", &mut |c, t, rec| {
                    if c.starts_with(code_prefix.as_str()) {
                        head.push((c.to_string(), t.to_string(), rec));
                    } else if t.to_lowercase().contains(&q)
                        || (!code_q.is_empty() && c.to_lowercase().contains(&code_q))
                    {
                        tail.push((c.to_string(), t.to_string(), rec));
                    }
                    true
                })?;
                head.append(&mut tail);
                head
            };
            let total = all.len();
            if let Some((by, desc)) = sort {
                all.sort_by(|(ca, ta, ra), (cb, tb, rb)| {
                    let ord = match by {
                        "count" => ra.count.cmp(&rb.count),
                        "lastUsed" => ra.last_used.cmp(&rb.last_used),
                        "text" => ta.cmp(tb),
                        _ => ca.cmp(cb),
                    };
                    if desc { ord.reverse() } else { ord }
                });
            }
            let page = all.into_iter().skip(offset).take(limit).collect();
            (page, total)
        };
        let items: Vec<Value> = page
            .into_iter()
            .map(|(code, text, rec)| {
                let code = self.freq_display_code(&schema, &code, &text);
                json!({ "code": code, "text": ui_text(&text), "count": rec.count, "lastUsed": rec.last_used })
            })
            .collect();
        Ok(json!({ "items": items, "total": total }))
    }

    /// 词频列表的编码显示：反查音节边界后渲染成带空格的音节码，与用户词库/临时词库列表同形。
    ///
    /// 词频表是唯一不带 boundary 的持久层，边界只能反查：系统词典 → 用户词表 → 临时词表，
    /// 都查不到原样返回扁平码（简拼码、码表方案、已删词条的遗留记录，属正常降级）。
    /// 只对当前页反查，开销与总规模无关。不给词频表加 boundary 字段，是因为存量记录补不上，
    /// 列表会新旧混杂。
    fn freq_display_code(&self, schema: &str, code: &str, text: &str) -> String {
        let mut b = self.engine_mgr().syllable_boundary_of(schema, code, text);
        if b == 0
            && let Some(store) = self.user_store()
        {
            let from = |recs: Vec<wind_store::user_words::UserWordRecord>| {
                recs.into_iter()
                    .find(|w| w.text == text)
                    .map(|w| w.boundary)
                    .filter(|x| *x != 0)
            };
            b = store
                .get_user_words(schema, code)
                .ok()
                .and_then(from)
                .or_else(|| store.get_temp_words(schema, code).ok().and_then(from))
                .unwrap_or(0);
        }
        wind_store::wdict::join_code_by_boundary(code, b)
    }

    fn web_freq_delete(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        // 列表项的 code 带音节空格（见 freq_display_code），而词频表 key 是扁平的。
        let (keys, batch) = items_or_single(params, flat_code_text_item)?;
        if keys.is_empty() {
            return Ok(batch_done(0));
        }
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        if batch {
            store.delete_freqs(&schema, &keys)?;
            return Ok(batch_done(keys.len()));
        }
        let (code, text) = &keys[0];
        store.delete_freq(&schema, code, text)?;
        Ok(json!({ "ok": true }))
    }

    fn web_freq_clear(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // 联想历史同是「用出来的」使用数据，随「清空词频」一并清。
        store.clear_assoc_history(&schema)?;
        Ok(json!(store.clear_freq(&schema)?))
    }

    /// 补全学习数据的分页列表（邮箱后缀 / 网址历史共用）。
    ///
    /// 不分方案（全局属性，见 `wind_store::completion`），参数里没有 `schemaId`；
    /// 设置端对应 `CategorySpec::scoped_by_schema = false`。
    fn web_completion_list_paged(
        &self,
        kind: CompletionKind,
        params: &Value,
    ) -> anyhow::Result<Value> {
        // `prefix` 与 `query` 是设置端发的同值别名（`build_list_params`），取到哪个算哪个。
        let query = params
            .get("prefix")
            .or_else(|| params.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let offset = usize_param(params, "offset", 0);
        let limit = usize_param(params, "limit", 50);
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let sort = parse_sort(params, &["text", "count", "lastUsed"]);

        // 搜索是包含而非前缀（用户记得的多半是网址中段），store 只提供前缀扫描，故有搜索或排序时
        // 全量拉取后内存过滤/排序；这两类数据有 `history_max` 封顶，规模可控。
        let (page, total) = if query.is_empty() && sort.is_none() {
            store.list_completions(kind, "", offset, limit)?
        } else {
            let (all, _) = store.list_completions(kind, "", 0, 0)?;
            let q = query.to_lowercase();
            let mut all: Vec<_> = all
                .into_iter()
                .filter(|(text, _)| q.is_empty() || text.to_lowercase().contains(&q))
                .collect();
            if let Some((by, desc)) = sort {
                all.sort_by(|(ta, ra), (tb, rb)| {
                    let ord = match by {
                        "count" => ra.count.cmp(&rb.count),
                        "lastUsed" => ra.last_used.cmp(&rb.last_used),
                        _ => ta.cmp(tb),
                    };
                    if desc { ord.reverse() } else { ord }
                });
            }
            let total = all.len();
            let page = all.into_iter().skip(offset).take(limit).collect();
            (page, total)
        };
        let items: Vec<Value> = page
            .into_iter()
            .map(|(text, rec)| {
                json!({ "text": ui_text(&text), "count": rec.count, "lastUsed": rec.last_used })
            })
            .collect();
        Ok(json!({ "items": items, "total": total }))
    }

    /// 删一条补全学习数据。返回它本来在不在。带 `items`（`[{text}]`）时一次删一批。
    fn web_completion_delete(&self, kind: CompletionKind, params: &Value) -> anyhow::Result<Value> {
        let (texts, batch) = items_or_single(params, |p| Ok(store_text(str_param(p, "text")?)))?;
        if texts.is_empty() {
            return Ok(batch_done(0));
        }
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        if batch {
            store.remove_completions(kind, &texts)?;
            return Ok(batch_done(texts.len()));
        }
        Ok(json!(store.remove_completion(kind, &texts[0])?))
    }

    /// 清空某一类补全学习数据，返回删除条数。
    ///
    /// 无内存镜像要一并清：补全候选每次刷新都现查库。
    fn web_completion_clear(&self, kind: CompletionKind) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        Ok(json!(store.clear_completions(kind)?))
    }

    fn web_shadow_list(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        Ok(json!(self.shadow_rows(&schema)?))
    }

    /// 影子规则的展平行集：一条 `ShadowRecord` 里的每个 pinned / deleted 各占一行。
    ///
    /// [`Self::web_shadow_list`] 与 [`Self::web_shadow_list_paged`] 共用，加字段只改这一处。
    fn shadow_rows(&self, schema: &str) -> anyhow::Result<Vec<Value>> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let mut out = Vec::new();
        for (code, rec) in store.list_shadow_rules(schema)? {
            for p in rec.pinned {
                out.push(json!({
                    "code": code,
                    "word": p.word,
                    "candId": p.cand_id,
                    "type": "pin",
                    "position": p.position,
                }));
            }
            for d in rec.deleted {
                out.push(json!({
                    "code": code,
                    "word": d,
                    "candId": Value::Null,
                    "type": "delete",
                }));
            }
        }
        Ok(out)
    }

    /// 影子规则分页列表（`{items,total}`，与 dict/freq 同形）。
    ///
    /// 规则数没有上限，一次全取会让 IPC 与设置端表格（不做虚拟化）在上万条时卡住。
    /// 搜索/排序都在切片之前做，故跨页全局有效。
    fn web_shadow_list_paged(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let query = params
            .get("prefix")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("query").and_then(|v| v.as_str()))
            .unwrap_or("");
        let offset = usize_param(params, "offset", 0);
        let limit = usize_param(params, "limit", 50);
        let mut all = self.shadow_rows(&schema)?;
        // 「词」列显示的是 candId 里的模板原文而非 `word`（见 [`shadow_display_text`]），
        // 搜索与排序都按显示文本。
        if !query.is_empty() {
            let q = query.to_lowercase();
            all.retain(|r| {
                let f = |k: &str| {
                    r.get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase()
                };
                f("code").contains(&q)
                    || f("word").contains(&q)
                    || shadow_display_text(r).to_lowercase().contains(&q)
            });
        }
        let total = all.len();
        // `text` 是设置端列模型里「词」列的字段名（各类别统一叫 text），此处落到 word 上。
        if let Some((by, desc)) = parse_sort(params, &["code", "word", "text", "type", "position"])
        {
            all.sort_by(|a, b| {
                let ord = match by {
                    // delete 类规则不带 position，缺字段按 0 排（与设置端 from_json 的回落一致）。
                    "position" => {
                        let n = |v: &Value| v.get("position").and_then(|x| x.as_i64()).unwrap_or(0);
                        n(a).cmp(&n(b))
                    }
                    "word" | "text" => shadow_display_text(a).cmp(&shadow_display_text(b)),
                    k => {
                        let g = |v: &'_ Value| {
                            v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
                        };
                        g(a).cmp(&g(b))
                    }
                };
                if desc { ord.reverse() } else { ord }
            });
        }
        let items: Vec<Value> = all.into_iter().skip(offset).take(limit).collect();
        Ok(json!({ "items": items, "total": total }))
    }

    /// 撤销该方案下的全部影子规则，返回撤销条数。
    fn web_shadow_clear(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        Ok(json!(store.clear_shadow(&schema)?))
    }

    fn web_shadow_pin(&self, params: &Value) -> anyhow::Result<Value> {
        let (schema, code, word) = (
            str_param(params, "schemaId")?,
            str_param(params, "code")?,
            str_param(params, "word")?,
        );
        let cand_id = params.get("candId").and_then(|v| v.as_str());
        let position = usize_param(params, "position", 0);
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        store.pin_shadow(&schema, code, word, cand_id, position)?;
        Ok(json!({ "ok": true }))
    }

    fn web_shadow_delete(&self, params: &Value) -> anyhow::Result<Value> {
        let (schema, code, word) = (
            str_param(params, "schemaId")?,
            str_param(params, "code")?,
            str_param(params, "word")?,
        );
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        store.delete_shadow(&schema, code, word)?;
        Ok(json!({ "ok": true }))
    }

    fn web_shadow_remove_rule(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        // 设置页删规则时回传 `shadow.list` 给出的 candId：动态短语规则的 word 是写入当天的
        // 求值文本，只按 word 定位会删不掉（列表里看得见、点删除无效）。
        let (rules, batch) = items_or_single(params, |p| {
            Ok((
                str_param(p, "code")?.to_string(),
                str_param(p, "word")?.to_string(),
                p.get("candId").and_then(|v| v.as_str()).map(str::to_string),
            ))
        })?;
        if rules.is_empty() {
            return Ok(batch_done(0));
        }
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        store.remove_shadow_rules(&schema, &rules)?;
        Ok(if batch {
            batch_done(rules.len())
        } else {
            json!({ "ok": true })
        })
    }

    /// 候选调整手动添加/编辑：type="hide" 转屏蔽；否则（pin）按 position 置顶。
    /// 匹配设置端候选调整对话框契约。
    ///
    /// 编辑既有规则时设置端回传的 `candId` 必须透传：这条路先按 `same_target` 删旧规则再插入，
    /// 不带 id 就擦掉了短语规则跨日命中所需的稳定 id（次日失配）。
    fn web_shadow_add_rule(&self, params: &Value) -> anyhow::Result<Value> {
        let (schema, code, word) = (
            str_param(params, "schemaId")?,
            str_param(params, "code")?,
            str_param(params, "word")?,
        );
        let kind = params.get("type").and_then(|v| v.as_str()).unwrap_or("pin");
        let cand_id = params.get("candId").and_then(|v| v.as_str());
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        if kind == "hide" {
            store.delete_shadow(&schema, code, word)?;
        } else {
            let position = usize_param(params, "position", 0);
            store.pin_shadow(&schema, code, word, cand_id, position)?;
        }
        Ok(json!({ "ok": true }))
    }

    fn web_temp_list(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // code 带音节空格、text 投影成转义形态（与 word_item 同形）；remove/promote 两个入口各自
        // 拆回扁平码、用 `store_text` 还原。三处须同改，否则「显示得了、删不掉」。
        let items: Vec<Value> = store
            .search_temp_words_prefix(&schema, "", 0)?
            .into_iter()
            .map(|r| {
                let code = wind_store::wdict::join_code_by_boundary(&r.code, r.boundary);
                json!({ "code": code, "text": ui_text(&r.text), "count": r.count })
            })
            .collect();
        Ok(json!(items))
    }

    /// 临时词分页列表（`{items,total}`，与 dict/freq 同形）。
    ///
    /// 临时词量级与用户词库同级，不宜一次全取。
    /// 搜索语义与 [`Self::web_dict_list_paged`] 对齐（编码前缀 ∪ 编码中段 ∪ 词条内容），
    /// 排序同样在切片前做，故跨页全局有效。
    fn web_temp_list_paged(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let prefix = params
            .get("prefix")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("query").and_then(|v| v.as_str()))
            .unwrap_or("");
        let offset = usize_param(params, "offset", 0);
        let limit = usize_param(params, "limit", 50);
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // 列表显示的是带音节空格的码（见 web_temp_list），用户很可能照着搜；key 是扁平的。
        let (code_prefix, _) = wind_store::wdict::split_spaced_code(prefix);
        let mut all = store.search_temp_words_prefix(&schema, &code_prefix, 0)?;
        // 并入两类补充命中（词条内容包含、编码中段包含），与 web_dict_list_paged 同款；仅在有搜索词时全量扫描。
        if !prefix.is_empty() {
            let q = prefix.to_lowercase();
            let code_q = code_prefix.to_lowercase();
            let seen: std::collections::HashSet<(String, String)> = all
                .iter()
                .map(|w| (w.code.clone(), w.text.clone()))
                .collect();
            for w in store.search_temp_words_prefix(&schema, "", 0)? {
                let hit = w.text.to_lowercase().contains(&q)
                    || (!code_q.is_empty() && w.code.to_lowercase().contains(&code_q));
                if hit && !seen.contains(&(w.code.clone(), w.text.clone())) {
                    all.push(w);
                }
            }
        }
        // 最近使用：词频表的 last_used 与加入时间取大者。从没被选中过的临时词在词频表里没有记录，
        // 只看词频会全部并列成「从未使用」。
        let keys: Vec<(String, String, String)> = all
            .iter()
            .map(|w| (schema.to_string(), w.code.clone(), w.text.clone()))
            .collect();
        let freq = store.get_freq_batch(&keys)?;
        let mut all: Vec<(_, i64)> = all
            .into_iter()
            .zip(freq)
            .map(|(w, f)| {
                let used = f.map_or(0, |f| f.last_used).max(w.created_at);
                (w, used)
            })
            .collect();
        // 筛选在切片前做：`maxCount` 次数上限、`usedBefore` 最近使用早于该时刻（unix 秒）。
        let max_count = params.get("maxCount").and_then(|v| v.as_u64());
        let used_before = params.get("usedBefore").and_then(|v| v.as_i64());
        all.retain(|(w, used)| {
            max_count.is_none_or(|n| u64::from(w.count) <= n)
                && used_before.is_none_or(|t| *used < t)
        });
        let total = all.len();
        if let Some((by, desc)) = parse_sort(params, &["code", "text", "count", "lastUsed"]) {
            all.sort_by(|(a, au), (b, bu)| {
                let ord = match by {
                    "count" => a.count.cmp(&b.count),
                    "text" => a.text.cmp(&b.text),
                    "lastUsed" => au.cmp(bu),
                    _ => a.code.cmp(&b.code),
                };
                if desc { ord.reverse() } else { ord }
            });
        }
        let items: Vec<Value> = all
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(r, used)| {
                let code = wind_store::wdict::join_code_by_boundary(&r.code, r.boundary);
                json!({ "code": code, "text": ui_text(&r.text), "count": r.count, "lastUsed": used })
            })
            .collect();
        Ok(json!({ "items": items, "total": total }))
    }

    fn web_temp_promote(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        // 列表项的 code 带音节空格（见 web_temp_list），key 是扁平的。
        let (keys, batch) = items_or_single(params, flat_code_text_item)?;
        if keys.is_empty() {
            return Ok(batch_done(0));
        }
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // 手动转正（含批量选中）是显式动作，照单条撤规则；自动晋升 `maybe_promote_temp` 与「全部转正」不撤。
        let promoted = if batch {
            store.promote_temp_words(&schema, &keys)?
        } else {
            let (code, text) = &keys[0];
            if store.promote_temp_word(&schema, code, text)? {
                vec![(code.clone(), text.clone())]
            } else {
                Vec::new()
            }
        };
        let words: Vec<(&str, &str)> = promoted
            .iter()
            .map(|(c, t)| (c.as_str(), t.as_str()))
            .collect();
        user_words_api::clear_deleted_for_added(self, &schema, &words);
        Ok(if batch {
            batch_done(keys.len())
        } else {
            json!({ "ok": true })
        })
    }

    fn web_temp_remove(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        // 同 promote：列表项的 code 带音节空格，不拆则删不掉。
        let (keys, batch) = items_or_single(params, flat_code_text_item)?;
        if keys.is_empty() {
            return Ok(batch_done(0));
        }
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        if batch {
            store.remove_temp_words(&schema, &keys)?;
            return Ok(batch_done(keys.len()));
        }
        let (code, text) = &keys[0];
        store.remove_temp_word(&schema, code, text)?;
        Ok(json!({ "ok": true }))
    }

    fn web_temp_promote_all(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let mut promoted = Vec::new();
        for r in store.search_temp_words_prefix(&schema, "", 0)? {
            if store.promote_temp_word(&schema, &r.code, &r.text)? {
                promoted.push((r.code, r.text));
            }
        }
        // 批量转正不撤删除规则，只报其中被隐藏的条数。
        let words: Vec<(&str, &str)> = promoted
            .iter()
            .map(|(c, t)| (c.as_str(), t.as_str()))
            .collect();
        let hidden = user_words_api::count_hidden_by_shadow(self, &schema, &words);
        Ok(json!({ "promoted": promoted.len() as u64, "hiddenByShadow": hidden }))
    }

    fn web_temp_clear(&self, params: &Value) -> anyhow::Result<Value> {
        let schema = str_param(params, "schemaId")?;
        let schema = self.engine_mgr().data_schema_id(schema); // 拼音族折叠到 "pinyin"
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let all = store.search_temp_words_prefix(&schema, "", 0)?;
        let n = all.len();
        for r in all {
            store.remove_temp_word(&schema, &r.code, &r.text)?;
        }
        Ok(json!(n))
    }

    fn web_phrase_list(&self) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let items: Vec<Value> = store
            .list_phrases()?
            .into_iter()
            .map(|p| {
                json!({
                    "code": p.code,
                    "text": ui_text(&p.text),
                    "position": p.position,
                    "weight": p.weight,
                    "enabled": p.enabled,
                    "isSystem": p.is_system,
                })
            })
            .collect();
        Ok(json!(items))
    }

    fn web_phrase_add(&self, params: &Value) -> anyhow::Result<Value> {
        let (code, text) = (str_param(params, "code")?, str_param(params, "text")?);
        let position = i32_param(params, "position");
        // 缺省值与取值依据见 `wind_store::phrases::DEFAULT_USER_PHRASE_WEIGHT`
        // （分发导入走同一个常量）。
        let weight = params
            .get("weight")
            .and_then(|v| v.as_i64())
            .unwrap_or(wind_store::phrases::DEFAULT_USER_PHRASE_WEIGHT as i64)
            as i32;
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        store.add_phrase(code, &store_text(text), position, weight)?;
        self.rebuild_phrases();
        Ok(json!({ "ok": true }))
    }

    fn web_phrase_update(&self, params: &Value) -> anyhow::Result<Value> {
        let (code, text) = (str_param(params, "code")?, str_param(params, "text")?);
        let new_code = params.get("newCode").and_then(|v| v.as_str());
        let new_text = params.get("newText").and_then(|v| v.as_str());
        let position = params
            .get("position")
            .and_then(|v| v.as_i64())
            .map(|n| n as i32);
        let weight = params
            .get("weight")
            .and_then(|v| v.as_i64())
            .map(|n| n as i32);
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // text 是查找键、new_text 是新值，两者都来自设置页，都要还原成存储域。
        let text = store_text(text);
        let new_text = new_text.map(store_text);
        store.update_phrase(code, &text, new_code, new_text.as_deref(), position, weight)?;
        // 若同时携带 enabled，应用到新键。
        if let Some(en) = params.get("enabled").and_then(|v| v.as_bool()) {
            store.set_phrase_enabled(
                new_code.unwrap_or(code),
                new_text.as_deref().unwrap_or(&text),
                en,
            )?;
        }
        // 改 code/text 时 `update_phrase` 内部会 remove 旧键——若改的是一条遮蔽了系统条目的
        // 用户短语，旧键一删那条系统短语也没了。与 `web_phrase_remove` 同一条约束。
        self.restore_missing_system_phrases("编辑短语");
        self.rebuild_phrases();
        Ok(json!({ "ok": true }))
    }

    fn web_phrase_remove(&self, params: &Value) -> anyhow::Result<Value> {
        let (keys, batch) = items_or_single(params, code_text_item)?;
        if keys.is_empty() {
            return Ok(batch_done(0));
        }
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        store.remove_phrases(&keys)?;
        // 删掉的可能是遮蔽了系统条目的用户短语（`overrides_system`，同一主键），删后要补回系统条目，
        // 让系统默认那条重新露出来。
        self.restore_missing_system_phrases("删除短语");
        self.rebuild_phrases();
        Ok(if batch {
            batch_done(keys.len())
        } else {
            json!({ "ok": true })
        })
    }

    fn web_phrase_set_enabled(&self, params: &Value) -> anyhow::Result<Value> {
        // text 是查找键，须还原成存储域（见 `store_text`）——漏了就「开关点了没反应」。
        let (keys, batch) = items_or_single(params, code_text_item)?;
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if keys.is_empty() {
            return Ok(batch_done(0));
        }
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        store.set_phrases_enabled(&keys, enabled)?;
        self.rebuild_phrases();
        Ok(if batch {
            batch_done(keys.len())
        } else {
            json!({ "ok": true })
        })
    }

    fn web_phrase_reset(&self) -> anyhow::Result<Value> {
        if let Some(store) = self.user_store() {
            store.reset_user_phrases()?;
            // 用户行里可能有遮蔽了系统条目的（`overrides_system`），删掉后那些系统短语
            // 也一并没了 → 补回缺失的，否则要等到 TOML 哈希变动才恢复。
            self.restore_missing_system_phrases("清空用户短语");
        }
        self.rebuild_phrases();
        Ok(json!({ "ok": true }))
    }

    fn web_phrase_list_system(&self) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let items: Vec<Value> = store
            .list_system_phrases()?
            .into_iter()
            .map(|p| {
                json!({
                    "code": p.code, "text": ui_text(&p.text), "weight": p.weight,
                    "position": p.position, "enabled": p.enabled, "isSystem": true,
                })
            })
            .collect();
        Ok(json!(items))
    }

    fn web_phrase_list_user(&self, params: &Value) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let prefix = params.get("prefix").and_then(|v| v.as_str());
        let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
        // 带 sortBy 时全量拉取 → 排序 → 内存切片；否则走 store 分页路径
        let (rows, total) = if let Some((by, desc)) =
            parse_sort(params, &["code", "text", "weight", "position", "enabled"])
        {
            let (mut all, total) = store.list_user_phrases_paged(prefix, 0, usize::MAX)?;
            all.sort_by(|a, b| {
                let ord = match by {
                    "weight" => a.weight.cmp(&b.weight),
                    "position" => a.position.cmp(&b.position),
                    "enabled" => a.enabled.cmp(&b.enabled),
                    "text" => a.text.cmp(&b.text),
                    _ => a.code.cmp(&b.code),
                };
                if desc { ord.reverse() } else { ord }
            });
            let page = all.into_iter().skip(offset).take(limit).collect();
            (page, total)
        } else {
            store.list_user_phrases_paged(prefix, offset, limit)?
        };
        let items: Vec<Value> = rows
            .into_iter()
            .map(|p| {
                json!({
                    "code": p.code, "text": ui_text(&p.text), "weight": p.weight,
                    "position": p.position, "enabled": p.enabled, "isSystem": false,
                    // 这条用户短语遮蔽了同码同内容的系统条目（该系统条目已从系统列表隐去，
                    // 输入期生效的是这条）。供 UI 标注来源，并提示「删除本条即恢复系统默认」。
                    "overridesSystem": p.overrides_system,
                })
            })
            .collect();
        Ok(json!({ "items": items, "total": total }))
    }

    fn web_phrase_export(&self) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let content = store.export_user_phrases_wdict("")?;
        Ok(json!({ "content": content }))
    }

    fn web_phrase_import(&self, params: &Value) -> anyhow::Result<Value> {
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let content = str_param(params, "content")?;
        let (imported, skipped) = store.import_user_phrases_wdict(content)?;
        self.rebuild_phrases();
        Ok(json!({ "imported": imported, "skipped": skipped }))
    }

    /// 逐行短语文本（`wind:p1` 分发格式）的导入预览。
    ///
    /// 与 `phrase.import`（wdict，整表 upsert、原样写回 position）分开：本条只新增、位置本地重算，
    /// 预览要回答「会不会动到已有的短语」。
    fn web_phrase_preview_import_text(&self, params: &Value) -> anyhow::Result<Value> {
        let content = str_param(params, "content")?;
        let (doc, items, checks, plan) = self.phrase_text_plan(content)?;

        let mut counts = (0usize, 0usize, 0usize);
        let entries: Vec<Value> = doc
            .entries
            .iter()
            .zip(&items)
            .zip(&checks)
            .zip(&plan)
            .map(|(((e, (_, stored)), ck), st)| {
                use wind_store::phrases::PhraseImportStatus as S;
                match st {
                    S::New => counts.0 += 1,
                    S::ExistsUser => counts.1 += 1,
                    S::ShadowsSystem => counts.2 += 1,
                }
                json!({
                    "line": e.line,
                    "code": e.code,
                    // 回显分发原文：分发域与设置页显示域同形。
                    "text": e.text,
                    "status": match st {
                        S::New => "new",
                        S::ExistsUser => "existsUser",
                        S::ShadowsSystem => "shadowsSystem",
                    },
                    "hints": ck.hints.iter().map(hint_json).collect::<Vec<_>>(),
                    "error": ck.error,
                    // 存储域与分发域不同（普通文本的 `\\`）时给 UI 一个对照位；相同则省略。
                    "storedText": (stored != &e.text).then(|| stored.clone()),
                })
            })
            .collect();

        let problems: Vec<Value> = doc
            .problems
            .iter()
            .map(|p| json!({ "line": p.line, "raw": p.raw, "message": p.reason.message() }))
            .collect();

        Ok(json!({
            "title": doc.title,
            "entries": entries,
            "problems": problems,
            "counts": {
                "new": counts.0,
                "existsUser": counts.1,
                "shadowsSystem": counts.2,
                "skippedLines": doc.problems.len(),
            },
        }))
    }

    /// 应用逐行短语文本。
    ///
    /// `accept` = 要导入的源行号数组（来自预览的 `line`），缺省导入全部可导入条目。
    /// 语法错的条目无论是否列出都不装（装进去只会在触发时失败）。
    fn web_phrase_import_text(&self, params: &Value) -> anyhow::Result<Value> {
        let content = str_param(params, "content")?;
        let (doc, items, checks, _) = self.phrase_text_plan(content)?;
        let accept: Option<std::collections::HashSet<u64>> = params
            .get("accept")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(serde_json::Value::as_u64).collect());

        let mut selected: Vec<(String, String)> = Vec::new();
        let (mut skipped_invalid, mut skipped_unselected) = (0, 0);
        for ((e, item), ck) in doc.entries.iter().zip(&items).zip(&checks) {
            if !ck.is_importable() {
                skipped_invalid += 1;
                continue;
            }
            if accept
                .as_ref()
                .is_some_and(|set| !set.contains(&(e.line as u64)))
            {
                skipped_unselected += 1;
                continue;
            }
            selected.push(item.clone());
        }

        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        let rep = store
            .import_phrases_appending(&selected, wind_store::phrases::DEFAULT_USER_PHRASE_WEIGHT)?;
        self.rebuild_phrases();
        Ok(json!({
            "added": rep.added,
            "skippedExisting": rep.skipped_existing,
            "shadowedSystem": rep.shadowed_system,
            "skippedInvalid": skipped_invalid,
            "skippedUnselected": skipped_unselected,
        }))
    }

    /// 解析 + 存储域投影 + 静态检查 + 落点判定。预览与应用共用，保证两者判定一致。
    #[allow(clippy::type_complexity)]
    fn phrase_text_plan(
        &self,
        content: &str,
    ) -> anyhow::Result<(
        wind_store::phrase_text::PhraseTextDoc,
        Vec<(String, String)>,
        Vec<wind_store::phrase_text::EntryCheck>,
        Vec<wind_store::phrases::PhraseImportStatus>,
    )> {
        let doc =
            wind_store::phrase_text::parse_phrase_text(content).map_err(|e| anyhow::anyhow!(e))?;
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
        // 存储域投影与手动新增（`web_phrase_add`）走同一个 `store_text`，否则同一条短语经不同入口进库会不同。
        let items: Vec<(String, String)> = doc
            .entries
            .iter()
            .map(|e| (e.code.clone(), store_text(&e.text)))
            .collect();
        let plan = store.plan_phrase_import(&items)?;
        let texts: Vec<String> = items.iter().map(|(_, t)| t.clone()).collect();
        let checks = wind_store::phrase_text::check_entries(&texts);
        Ok((doc, items, checks, plan))
    }

    fn web_phrase_reset_system(&self) -> anyhow::Result<Value> {
        let n = self.restore_system_phrases();
        Ok(json!({ "ok": true, "changed": n }))
    }

    // ───────── quick.*（快捷输入格式表）─────────

    /// 全部格式条目，含被停用的。
    ///
    /// `displayPos` 是列表行号（停用项也占一行，只给人看）；`moveIndex` 是候选里的位置，
    /// 也是 `quick.move` 唯一认的口径，停用项为 null。别混用。
    fn web_quick_list(&self) -> anyhow::Result<Value> {
        let rows: Vec<Value> = self
            .quick_format_rows()
            .into_iter()
            .map(|r| {
                json!({
                    "kind": r.kind,
                    "id": r.id,
                    "text": r.text,
                    "displayPos": r.display_pos,
                    "moveIndex": r.move_index,
                    "enabled": r.enabled,
                    "adjusted": r.adjusted,
                    "user": r.user,
                    "sample": r.sample,
                })
            })
            .collect();
        Ok(json!(rows))
    }

    fn web_quick_move(&self, params: &Value) -> anyhow::Result<Value> {
        let (kind, id) = (str_param(params, "kind")?, str_param(params, "id")?);
        let index = usize_param(params, "index", 0);
        self.quick_format_edit(kind, id, QuickFormatEdit::MoveTo(index))?;
        Ok(json!({ "ok": true }))
    }

    // ── 常用字表（右键「设为生僻字/常用字」的设置页一侧）────────────────────
    //
    // 列表是全表（出厂字 + 用户加的），用来回答「这个字现在算不算常用」；改过的行带 `adjusted`，
    // 「恢复默认」据此只对它们放行。

    /// 全表 + 搜索 + 分页。
    ///
    /// 返回 `{items,total}`（全表约 8 千条，不宜每次刷新整份过 IPC），与非分页类别的裸数组
    /// 形状刻意不同，由设置端 spec 的 `paged: true` 决定解析分支。
    fn web_common_chars_list(&self, params: &Value) -> anyhow::Result<Value> {
        let query = params
            .get("prefix")
            .or_else(|| params.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // 「只看已修改」：全表 8104 条里自己动过的那几个，靠翻页是找不到的。
        let only_modified = params
            .get("onlyModified")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut rows = self.common_char_rows(query, only_modified);
        sort_common_char_rows(
            &mut rows,
            params.get("sortBy").and_then(|v| v.as_str()).unwrap_or(""),
            params.get("sortOrder").and_then(|v| v.as_str()) == Some("desc"),
        );
        let total = rows.len();
        let offset = usize_param(params, "offset", 0);
        // limit=0 视为不限（调用方不分页时直接全取）。
        let limit = match usize_param(params, "limit", 0) {
            0 => total,
            n => n,
        };
        let items: Vec<Value> = rows
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|r| {
                json!({
                    "char": r.text,
                    "common": r.common,
                    // 默认判定一起给：界面靠它显示「默认 → 现在」的对照。
                    "baseCommon": r.base_common,
                    // 这一行改过没有。设置页据此决定「恢复默认」灰不灰。
                    "adjusted": r.overridden,
                    // 类型：所属 Unicode 块（字形相近的 ⺡/氵、℃/㎡ 处置方式完全不同）。
                    "block": r.block,
                    "blockRange": wind_candidate::block_of_cluster(&r.text).range_text(),
                    // 整类批量能不能点。汉字块恒 false——见 `block_allows_bulk_edit`。
                    "blockBulkEditable": r.block_bulk_editable,
                    // 决定这个字常用性的字符类（null = 没有类命中它）。与 `block` 不可互替：
                    // `block` 决定「整类批量」扫哪个码位区间，`class` 决定「改类的 default」影响谁。
                    "class": r.class,
                })
            })
            .collect();
        Ok(json!({ "items": items, "total": total }))
    }

    fn web_common_chars_query(&self, params: &Value) -> anyhow::Result<Value> {
        let ch = char_param(params, "char")?;
        let st = self.common_char_state(&ch);
        Ok(json!({
            "char": ch.to_string(),
            // false ⇒ 界面应拒绝添加：读端根本不查这类字符，存了也永不生效。
            "governed": st.governed,
            "baseCommon": st.base_common,
            "override": st.over,
            // 当前生效判定，省得前端再算一遍（算错了就是两边显示不一致）。
            "effective": st.over.unwrap_or(st.base_common),
        }))
    }

    fn web_common_chars_set(&self, params: &Value) -> anyhow::Result<Value> {
        let (chars, batch) = items_or_single(params, |p| char_param(p, "char"))?;
        let common = params
            .get("common")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| anyhow::anyhow!("缺少参数 common"))?;
        if batch {
            if !chars.is_empty() {
                self.common_char_edit_batch(&chars, CommonCharEdit::Set(common))?;
            }
            return Ok(batch_done(chars.len()));
        }
        self.common_char_edit(&chars[0], CommonCharEdit::Set(common))?;
        Ok(json!({ "ok": true }))
    }

    /// `charset.list` —— 字符类全表。
    ///
    /// `overrideCount` 界面必须显示：类上压着大量逐条覆盖时，改 `default` 看不出任何效果（设计文档 §7.2）。
    fn web_charset_list(&self) -> anyhow::Result<Value> {
        let items: Vec<Value> = self
            .charset_rows()
            .into_iter()
            .map(|r| {
                json!({
                    "key": r.key,
                    "name": r.name,
                    "ranges": r.ranges,
                    "memberCount": r.member_count,
                    "order": r.order,
                    // 三态：null = 本类不表态（跟随下层/兜底判常用）。
                    "default": r.default_common,
                    "noFreq": r.no_freq,
                    "inRare": r.in_rare,
                    "enabled": r.enabled,
                    // 内置区块类的 ranges 只读——要改范围请新建自己的类（§4.3）。
                    "builtin": r.builtin,
                    "overridden": r.overridden,
                    "overrideCount": r.override_count,
                })
            })
            .collect();
        // 裸数组而非 `{items,total}`：设置端 spec 里 `paged: false`，包成对象会静默解析出 0 行。
        Ok(Value::Array(items))
    }

    /// `charset.set` —— 改一个类的属性。缺席的字段不动。
    fn web_charset_set(&self, params: &Value) -> anyhow::Result<Value> {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("缺少参数 key"))?;
        let edit = wind_coordinator::handle_charset::CharsetEdit {
            // "common" / "rare" / "inherit"；inherit 是本层不表态、跟随出厂更新，与显式常用不同。
            default_common: params
                .get("default")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            no_freq: params.get("noFreq").and_then(|v| v.as_bool()),
            in_rare: params.get("inRare").and_then(|v| v.as_bool()),
            enabled: params.get("enabled").and_then(|v| v.as_bool()),
            order: params
                .get("order")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
        };
        self.charset_edit(key, &edit)?;
        Ok(json!({ "ok": true }))
    }

    /// `charset.reset` —— 撤掉用户层对这个类的全部调整。
    fn web_charset_reset(&self, params: &Value) -> anyhow::Result<Value> {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("缺少参数 key"))?;
        self.charset_reset(key)?;
        Ok(json!({ "ok": true }))
    }

    /// `charset.clearRedundant` —— 清理压在这个类上、方向与默认相同的逐条覆盖。
    fn web_charset_clear_redundant(&self, params: &Value) -> anyhow::Result<Value> {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("缺少参数 key"))?;
        let o = self.charset_clear_redundant(key)?;
        Ok(json!({ "scanned": o.scanned, "removed": o.removed }))
    }

    /// `charset.create {key, name, default?, order?}` —— 新建一个空的自建分类。`default` 同
    /// `charset.set`（`common` / `rare` / `inherit`）；不给 `order` 排在出厂常用汉字表之前。
    fn web_charset_create(&self, params: &Value) -> anyhow::Result<Value> {
        let key = str_param(params, "key")?;
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let edit = wind_coordinator::handle_charset::CharsetEdit {
            default_common: params
                .get("default")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            order: params
                .get("order")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
            ..Default::default()
        };
        self.charset_create(key, name, &edit)?;
        Ok(json!({ "ok": true, "key": key }))
    }

    /// `charset.delete` —— 删一个自建类。出厂类会被拒绝。
    fn web_charset_delete(&self, params: &Value) -> anyhow::Result<Value> {
        let key = str_param(params, "key")?;
        self.charset_delete(key)?;
        Ok(json!({ "ok": true }))
    }

    /// 按当前行所属的 Unicode 块整类设常用/生僻。`apply` 缺省为 `false`（只预览）。
    ///
    /// 预览与执行走同一个方法、同一次扫描，只差 `apply` 开关，保证预览报的条数就是执行写的条数。
    fn web_common_chars_bulk(&self, params: &Value) -> anyhow::Result<Value> {
        let ch = char_param(params, "char")?;
        let common = params
            .get("common")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| anyhow::anyhow!("缺少参数 common"))?;
        let apply = params
            .get("apply")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let o = self.common_char_bulk_by_block(&ch, common, apply)?;
        Ok(json!({
            "block": o.block,
            "chars": o.chars,
            // 界面必须显示它：一个字符可能出现在数百条词条里，只报 chars 会低估影响面。
            "entries": o.entries,
            "written": o.written,
            "sample": o.sample,
        }))
    }

    fn web_common_chars_reset(&self, params: &Value) -> anyhow::Result<Value> {
        let (chars, batch) = items_or_single(params, |p| char_param(p, "char"))?;
        if batch {
            if !chars.is_empty() {
                self.common_char_edit_batch(&chars, CommonCharEdit::Reset)?;
            }
            return Ok(batch_done(chars.len()));
        }
        self.common_char_edit(&chars[0], CommonCharEdit::Reset)?;
        Ok(json!({ "ok": true }))
    }

    fn web_common_chars_clear(&self) -> anyhow::Result<Value> {
        // 整表操作没有单字归属，传一个占位字符（`ClearAll` 忽略它），
        // 与 `quick.resetKind` 传空 id 同一惯例。
        self.common_char_edit("", CommonCharEdit::ClearAll)?;
        Ok(json!({ "ok": true }))
    }

    /// 导出用户调整为 TOML 文本。与 `quick.export` 同形（`{content}`），设置页那一路
    /// 写文件的代码因此可以照抄。
    fn web_common_chars_export(&self) -> anyhow::Result<Value> {
        Ok(json!({ "content": self.common_chars_export()? }))
    }

    fn web_common_chars_preview_import(&self, params: &Value) -> anyhow::Result<Value> {
        let p = self.common_chars_preview_import(str_param(params, "content")?)?;
        Ok(json!({
            "common": p.common,
            "rare": p.rare,
            "skipped": p.skipped,
        }))
    }

    fn web_common_chars_import(&self, params: &Value) -> anyhow::Result<Value> {
        let content = str_param(params, "content")?;
        // 与词库/短语/快捷输入导入同一套参数名：`strategy = "replace"` 先清空，其余为合并。
        let replace = params
            .get("strategy")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("replace"));
        let o = self.common_chars_import(content, replace)?;
        Ok(json!({
            "imported": o.imported,
            // 与默认同向、无需落库的字。设置页须说明，否则写入条数少于导入条数像是丢了数据。
            "sameAsDefault": o.same_as_default,
            "skipped": o.skipped,
        }))
    }

    fn web_quick_set_enabled(&self, params: &Value) -> anyhow::Result<Value> {
        let (items, batch) = items_or_single(params, kind_id_item)?;
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| anyhow::anyhow!("缺少参数 enabled"))?;
        self.quick_edit_items(&items, batch, QuickFormatEdit::SetEnabled(enabled))
    }

    fn web_quick_reset_entry(&self, params: &Value) -> anyhow::Result<Value> {
        let (items, batch) = items_or_single(params, kind_id_item)?;
        self.quick_edit_items(&items, batch, QuickFormatEdit::ResetEntry)
    }

    /// `quick.setEnabled` / `quick.resetEntry` 的单条与批量分流（批量可跨类别）。
    fn quick_edit_items(
        &self,
        items: &[(String, String)],
        batch: bool,
        edit: QuickFormatEdit,
    ) -> anyhow::Result<Value> {
        if batch {
            if !items.is_empty() {
                self.quick_format_edit_batch(items, edit)?;
            }
            return Ok(batch_done(items.len()));
        }
        let (kind, id) = &items[0];
        self.quick_format_edit(kind, id, edit)?;
        Ok(json!({ "ok": true }))
    }

    fn web_quick_reset_kind(&self, params: &Value) -> anyhow::Result<Value> {
        let kind = str_param(params, "kind")?;
        // 整类操作没有单条归属，id 传空串（`QuickFormatEdit::ResetKind` 忽略它）。
        self.quick_format_edit(kind, "", QuickFormatEdit::ResetKind)?;
        Ok(json!({ "ok": true }))
    }

    /// 每类可用的模板变量清单，供设置页的模板输入框提示。
    ///
    /// 静态数据，一次取完即可缓存。真相源在 core 的 `FormatKind::var_hints`，设置端别自己硬编码。
    fn web_quick_vars(&self) -> anyhow::Result<Value> {
        let kinds: Vec<Value> = self
            .quick_format_var_hints()
            .into_iter()
            .map(|(kind, vars)| {
                json!({
                    "kind": kind,
                    "vars": vars
                        .into_iter()
                        .map(|(name, desc)| json!({ "name": name, "desc": desc }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!(kinds))
    }

    /// 新增用户自定义条目。返回分配到的 id，好让设置页立刻高亮/滚到那一行。
    ///
    /// 没有对应的「改出厂条目模板」RPC：那条路径被刻意否决，出厂条目只能停用与调序
    /// （见 `docs/design/quick-input-format-table.md` §11.1）。
    fn web_quick_add(&self, params: &Value) -> anyhow::Result<Value> {
        let (kind, text) = (str_param(params, "kind")?, str_param(params, "text")?);
        let id = self.quick_format_add(kind, text)?;
        Ok(json!({ "ok": true, "id": id }))
    }

    fn web_quick_set_text(&self, params: &Value) -> anyhow::Result<Value> {
        let (kind, id) = (str_param(params, "kind")?, str_param(params, "id")?);
        self.quick_format_set_text(kind, id, str_param(params, "text")?)?;
        Ok(json!({ "ok": true }))
    }

    fn web_quick_delete(&self, params: &Value) -> anyhow::Result<Value> {
        let (items, batch) = items_or_single(params, kind_id_item)?;
        if batch {
            if !items.is_empty() {
                self.quick_format_delete_batch(&items)?;
            }
            return Ok(batch_done(items.len()));
        }
        let (kind, id) = &items[0];
        self.quick_format_delete(kind, id)?;
        Ok(json!({ "ok": true }))
    }

    /// `softkeyboard.apply`：`{ ops: [{ op: "setKey", ... }, ...] }`，操作形态见
    /// `wind_softkeyboard::admin::Op`。
    fn web_softkeyboard_apply(&self, params: &Value) -> anyhow::Result<Value> {
        let ops: Vec<wind_softkeyboard::admin::Op> = serde_json::from_value(
            params
                .get("ops")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("缺少参数 ops"))?,
        )
        .map_err(|e| anyhow::anyhow!("ops 格式无效：{e}"))?;
        self.softkeyboard_apply(&ops)
    }

    /// `compat.*` 的薄分派：纯逻辑在 `compat_admin::rpc`，这里只负责取目录、
    /// 写成功后让宿主重载。
    fn web_compat_rpc(&self, method: &str, params: &Value) -> anyhow::Result<Value> {
        let data_dir = wind_config::Config::data_dir();
        let custom_dir = wind_config::Config::custom_data_dir();
        let user_dir = wind_config::Config::user_config_dir();
        let out = wind_config::compat_admin::rpc(
            method,
            params,
            data_dir.as_deref(),
            custom_dir.as_deref(),
            user_dir.as_deref(),
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        if out.wrote {
            self.reload_compat();
        }
        Ok(out.value)
    }

    fn web_quick_export(&self) -> anyhow::Result<Value> {
        Ok(json!({ "content": self.quick_format_export()? }))
    }

    fn web_quick_preview_import(&self, params: &Value) -> anyhow::Result<Value> {
        let p = self.quick_format_preview_import(str_param(params, "content")?)?;
        Ok(json!({
            "moved": p.moved,
            "disabled": p.disabled,
            "formats": p.formats,
            "skipped": p.skipped,
            "kinds": p.kinds,
        }))
    }

    fn web_quick_import(&self, params: &Value) -> anyhow::Result<Value> {
        let content = str_param(params, "content")?;
        // 与词库/短语导入同一套参数名：`strategy = "replace"` 先清空，其余为合并。
        let replace = params
            .get("strategy")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("replace"));
        let o = self.quick_format_import(content, replace)?;
        Ok(json!({
            "moved": o.moved,
            "disabled": o.disabled,
            "formats": o.formats,
            "skipped": o.skipped,
        }))
    }

    /// `device` 参数：缺省或 `all` = 全部设备，`self` = 本机，其余须是 `stats.devices` 里的设备号。
    fn stats_scope(&self, params: &Value) -> anyhow::Result<wind_store::stats_report::StatsScope> {
        let dev = match params.get("device") {
            None | Some(Value::Null) => "all",
            Some(v) => v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("device 须为字符串"))?,
        };
        wind_store::stats_report::StatsScope::parse(self.user_store().map(|s| &**s), dev)
            .map_err(anyhow::Error::msg)
    }

    fn web_stats_devices(&self) -> anyhow::Result<Value> {
        Ok(json!(
            self.user_store()
                .map(|s| s.list_device_stats())
                .unwrap_or_default()
        ))
    }

    fn web_stats_summary(&self, params: &Value) -> anyhow::Result<Value> {
        let scope = self.stats_scope(params)?;
        let (collector, store) = match (self.stat_collector(), self.user_store()) {
            (Some(c), Some(s)) => (c, s),
            _ => return Ok(Self::empty_stats_summary()),
        };
        // 算法在 wind-store（与移动端共用一份），这里只管序列化
        let s = wind_store::stats_report::summary_for(store, collector, &scope);
        Ok(json!({
            "today_chars": s.today_chars,
            "today_chinese": s.today_chinese,
            "today_english": s.today_english,
            "total_chars": s.total_chars,
            "active_days": s.active_days,
            "daily_avg": s.daily_avg,
            "streak_current": s.streak_current,
            "streak_max": s.streak_max,
            "week_chars": s.week_chars,
            "month_chars": s.month_chars,
            "max_day_chars": s.max_day_chars,
            "max_day_date": s.max_day_date,
            "avg_code_len": s.avg_code_len,
            "first_select_rate": s.first_select_rate,
            "today_speed": s.today_speed,
            "overall_speed": s.overall_speed,
            "max_speed": s.max_speed,
        }))
    }

    fn web_stats_daily(&self, params: &Value) -> anyhow::Result<Value> {
        let from = str_param(params, "from")?.to_string();
        let to = str_param(params, "to")?.to_string();
        let store = match self.user_store() {
            Some(s) => s,
            None => return Ok(json!([])),
        };
        // 区间内连续日期补零值，输出完整 DailyStatItem（便于前端绘图）。
        let scope = self.stats_scope(params)?;
        let days = wind_store::stats_report::daily_range_for(
            store,
            self.stat_collector(),
            &scope,
            &from,
            &to,
        )?;
        Ok(json!(
            days.iter()
                .map(|(d, r)| Self::daily_item_json(d, r))
                .collect::<Vec<_>>()
        ))
    }

    fn web_stats_clear(&self) -> anyhow::Result<Value> {
        wind_store::stats_report::clear(self.user_store().map(|s| &**s), self.stat_collector())?;
        Ok(json!({ "ok": true }))
    }

    /// 参数 `months`：`{"YYYY-MM": "月表原文", ...}`，整体替换本机每日统计（没给出的月份删除）。
    fn web_stats_replace_months(&self, params: &Value) -> anyhow::Result<Value> {
        let months: std::collections::BTreeMap<String, String> = serde_json::from_value(
            params
                .get("months")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("缺少参数 months"))?,
        )
        .map_err(|e| anyhow::anyhow!("months 须为「月份 → 月表原文」的对象：{e}"))?;
        let store = self
            .user_store()
            .ok_or_else(|| anyhow::anyhow!("用户数据未就绪"))?;
        let n = wind_store::stats_report::replace_months(store, self.stat_collector(), &months)?;
        Ok(json!({ "written": n }))
    }

    fn web_stats_prune(&self, params: &Value) -> anyhow::Result<Value> {
        // 参数 days：删除早于 (今天 - days) 的统计。
        let days = params
            .get("days")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let store = match self.user_store() {
            Some(s) => s,
            None => return Ok(json!({ "pruned": 0 })),
        };
        let before = (chrono::Local::now().date_naive() - chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string();
        let n = store.prune_stats_before(&before)?;
        // 重建元数据（剔除已删历史）并让采集器重载：先 flush 今日落库，recalc 后 resume。
        if let Some(c) = self.stat_collector() {
            c.flush();
            store.recalculate_stats_meta(c.speed_factor())?;
            c.resume();
        } else {
            // 无采集器时没有系数可读，按 1.0 重算：这条分支只在无 store 的测试壳里走得到。
            store.recalculate_stats_meta(1.0)?;
        }
        Ok(json!({ "pruned": n }))
    }

    /// 组装前端 DailyStatItem JSON（紧凑字段名，含按方案 bs / 按来源 src）。
    fn daily_item_json(date: &str, r: &wind_store::stats::DailyStats) -> Value {
        let (sp_chars, sp_millis) = r.speed_parts();
        let bs: serde_json::Map<String, Value> = r
            .by_schema
            .iter()
            .map(|(k, s)| {
                (
                    k.clone(),
                    json!({
                        "tc": s.total_chars,
                        "cn": s.commit_count,
                        "cls": s.code_len_sum,
                        "clc": s.code_len_count,
                        "cpd": s.cand_pos_dist,
                    }),
                )
            })
            .collect();
        json!({
            "d": date,
            "tc": r.total(),
            "cc": r.chinese,
            "ec": r.english,
            "pc": r.punct,
            "oc": r.other,
            "h": r.hours,
            "cn": r.commit_count,
            "cls": r.code_len_sum,
            "clc": r.code_len_count,
            "cld": r.code_len_dist,
            "cpd": r.cand_pos_dist,
            "as": r.active_seconds,
            // 速度专用分子/分母（v2 模型）：与 tc/as 分开，前端画速度曲线用这两个。
            "sc": sp_chars,
            "am": sp_millis,
            "bs": bs,
            "src": r.by_source,
        })
    }

    /// 无采集器/存储时的空摘要（17 字段全 0，对齐前端 StatsSummary 形状）。
    fn empty_stats_summary() -> Value {
        json!({
            "today_chars": 0, "today_chinese": 0, "today_english": 0,
            "total_chars": 0, "active_days": 0, "daily_avg": 0,
            "streak_current": 0, "streak_max": 0, "week_chars": 0, "month_chars": 0,
            "max_day_chars": 0, "max_day_date": "", "avg_code_len": 0.0,
            "first_select_rate": 0.0, "today_speed": 0, "overall_speed": 0, "max_speed": 0,
        })
    }

    /// 主题查找目录：各资源层的 `themes/`，按层序 `user > custom > data`
    /// （宿主的 `theme_search_dirs()` 是唯一真相源）。
    fn theme_dirs(&self) -> Vec<std::path::PathBuf> {
        // 直接复用宿主的搜索链，别自己拼一份：两份链不一致会出现「列表里有、选了却没变化」。
        self.theme_search_dirs()
    }

    /// 用户主题写入目录（导入/删除）。
    fn user_themes_dir(&self) -> Option<std::path::PathBuf> {
        wind_config::Config::user_config_dir().map(|u| u.join("themes"))
    }

    fn web_theme_preview(&self, params: &Value) -> anyhow::Result<Value> {
        let name = str_param(params, "name")?;
        let dirs = self.theme_dirs();
        // 合并 base 链 + 归一化（扁平人写形态 → 嵌套内存形态）后的主题配置（toml::Value → JSON），
        // 供前端预览渲染（保持历史 views.* 嵌套契约）。
        let merged = wind_theme::load_merged_dirs(&dirs, name, 0)?;
        let normalized = wind_theme::normalize::normalize_theme(merged);
        Ok(serde_json::to_value(&normalized)?)
    }

    /// 按 slug 在主题搜索链里定位主题目录，读取主题的入口（`getText`、导出）共用这一套判据。
    ///
    /// 与 [`Self::theme_target_dir`]（写入侧，只认用户目录）分开：读取要跨整条链。
    /// 定制版 `[themes] hide` 掉的 id 按不存在处理，与列表（`list_themes_full`）和导入
    /// （`theme_target_dir`）一致。
    fn theme_dir_of(&self, slug: &str) -> anyhow::Result<std::path::PathBuf> {
        if !wind_theme::is_plain_slug(slug) {
            anyhow::bail!("非法主题 slug");
        }
        if wind_config::Config::custom_hides_theme(slug) {
            anyhow::bail!("主题不存在");
        }
        self.theme_dirs()
            .into_iter()
            .map(|d| d.join(slug))
            .find(|d| d.join(wind_transfer::theme::THEME_ENTRY_NAME).is_file())
            .ok_or_else(|| anyhow::anyhow!("主题不存在"))
    }

    fn web_theme_get_text(&self, params: &Value) -> anyhow::Result<Value> {
        let slug = str_param(params, "slug")?;
        let path = self
            .theme_dir_of(slug)?
            .join(wind_transfer::theme::THEME_ENTRY_NAME);
        let toml =
            std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("读取主题失败：{e}"))?;
        Ok(json!({ "slug": slug, "toml": toml }))
    }

    /// `schema.reloadFiles {ids?}`：方案文件（`schemas/`、override、词库）被外部改了之后，
    /// 失效这些方案的引擎与解析缓存，下次使用按新内容重建；缺省 `ids` = 全部已安装方案。
    /// 与 `schema.invalidate` 同一条路（先失效、再重建跨方案派生集合）。
    fn web_schema_reload_files(&self, params: &Value) -> anyhow::Result<Value> {
        let ids = external_files::reload_schema_files(self, optional_ids(params)?);
        Ok(json!({ "ok": true, "reloaded": ids }))
    }

    /// `theme.reloadFiles {ids?}`：主题文件被外部改了之后，丢掉这些主题的预览图缓存；
    /// 当前生效主题在其中（或缺省 `ids`）时重读并下发。
    fn web_theme_reload_files(&self, params: &Value) -> anyhow::Result<Value> {
        let (ids, applied) = external_files::reload_theme_files(
            self,
            optional_ids(params)?,
            &theme_render::forget_theme,
        )?;
        Ok(json!({ "ok": true, "reloaded": ids, "applied": applied }))
    }

    fn web_theme_delete(&self, params: &Value) -> anyhow::Result<Value> {
        let name = str_param(params, "name")?;
        // 拼进用户主题目录再 remove_dir_all：`..` / 绝对路径能删到目录外去。
        if !wind_theme::is_plain_slug(name) {
            anyhow::bail!("非法主题 slug");
        }
        let user_dir = self
            .user_themes_dir()
            .ok_or_else(|| anyhow::anyhow!("无用户主题目录"))?;
        let target = user_dir.join(name);
        if !target.join("theme.toml").exists() {
            anyhow::bail!("内置主题不可删除或主题不存在: {}", name);
        }
        std::fs::remove_dir_all(&target)?;
        theme_render::forget_theme(name);
        Ok(json!({ "ok": true }))
    }

    /// 主题落到哪个目录 id，文本导入与包导入共用这一套判据。
    ///
    /// - 传了 `slug`（主题唯一 id）：目录已存在则就地写回（不新建），否则以 slug 建目录；
    ///   slug 不合法直接报错；
    /// - 未传（设置端的粘贴 / 文件 / 链接 / 包导入都不传）：从显示名（`meta.name` / 包名）
    ///   派生合法 id（[`wind_theme::slug_from_name`]），「Nord: Dark」「黑/白」也导得进来。
    ///
    /// 定制版 `[themes] hide` 掉的 id 当场拒掉：hide 是绝对的（见 `Config::custom_hides_theme`），
    /// 放行的话导入回执 ok，主题却永远不进列表。
    fn theme_target_dir(
        &self,
        params: &Value,
        fallback_name: &str,
    ) -> anyhow::Result<(String, std::path::PathBuf)> {
        let user_dir = self
            .user_themes_dir()
            .ok_or_else(|| anyhow::anyhow!("无用户主题目录"))?;
        // id 来自调用方或主题文本，都不可信，落盘前必须是普通名字：否则 `../../x`、绝对路径、`C:x`
        // 会跳出用户主题目录，而导入失败的回滚会对目标 remove_dir_all。不合法的 slug 直接报错。
        let theme_id = match params
            .get("slug")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(slug) => slug.to_string(),
            None => wind_theme::slug_from_name(fallback_name),
        };
        if !wind_theme::is_plain_slug(&theme_id) {
            anyhow::bail!(
                "主题 id「{theme_id}」不合法：不能含路径分隔符、盘符、「..」或 8.3 短名「~数字」，\
                 不能以点或空格结尾，也不能是 Windows 保留名（CON、AUX、NUL、COM1 等）"
            );
        }
        if wind_config::Config::custom_hides_theme(&theme_id) {
            anyhow::bail!("本定制版已移除主题 id「{theme_id}」，该 id 不可用；请换一个 id 再导入");
        }
        let target = user_dir.join(&theme_id);
        if target.parent() != Some(user_dir.as_path()) {
            anyhow::bail!("主题 id「{theme_id}」不合法");
        }
        Ok((theme_id, target))
    }

    /// 「同名已存在」的机器可读回执。
    ///
    /// 可预期的业务性失败：客户端据此弹「是否覆盖」再以 force=true 重推，故走 result 字段而非
    /// error 通道（那样只能靠匹配文案认冲突）。文案用已有主题的显示名：不同名字可能派生出
    /// 同一目录（`a/b` 与 `a-b`）。
    fn theme_conflict_json(theme_id: &str, display_name: &str, target: &std::path::Path) -> Value {
        let existing = std::fs::read_to_string(target.join("theme.toml"))
            .ok()
            .and_then(|t| wind_theme::meta_from_text(&t))
            .map(|m| m.name)
            .filter(|n| !n.trim().is_empty());
        let error = match &existing {
            Some(old) if old != display_name => {
                format!("已有主题「{old}」占用同一目录，导入「{display_name}」会覆盖它")
            }
            Some(old) => format!("主题「{old}」已存在"),
            None => format!("主题目录「{theme_id}」已存在"),
        };
        json!({
            "ok": false,
            "conflict": true,
            "slug": theme_id,
            "display_name": display_name,
            "existing_name": existing,
            "error": error,
        })
    }

    /// 推的正好是当前生效主题 → 立刻重解析下发，编辑器里改完即见效。返回是否重载了。
    ///
    /// 判据是目录 id 相同，不比对内容（base 继承链可能间接影响，重解析一次很便宜）。
    /// 用户目录优先于安装目录，故与内置主题同 id 的用户主题同样算。
    fn theme_reload_if_current(&self, theme_id: &str) -> bool {
        if self.current_theme_name() != theme_id {
            return false;
        }
        // 明暗沿用当前 style（system 时按系统实时判定），与 on_system_theme_changed 同一出口。
        let dark = self.current_theme_is_dark();
        tracing::info!("导入的是当前主题 {}，重新加载以即时生效", theme_id);
        self.push_theme(theme_id, dark);
        true
    }

    fn web_theme_import_text(&self, params: &Value) -> anyhow::Result<Value> {
        // 参数键沿用 "yaml"（前端契约未改），内容为 TOML 文本。
        let text = str_param(params, "yaml")?;
        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // 校验可解析为合法主题（仅自身，未校验 base 依赖链）。
        wind_theme::validate_text(text)?;
        let meta = wind_theme::meta_from_text(text)
            .ok_or_else(|| anyhow::anyhow!("主题缺少 meta.name"))?;
        if meta.name.trim().is_empty() {
            anyhow::bail!("主题 meta.name 为空");
        }
        let (theme_id, target) = self.theme_target_dir(params, &meta.name)?;
        let file = target.join("theme.toml");
        let existed_before = file.exists();
        if existed_before && !force {
            return Ok(Self::theme_conflict_json(&theme_id, &meta.name, &target));
        }
        // 覆盖已存在主题前备份原文本，供依赖链校验失败时回滚。
        let backup = if existed_before {
            std::fs::read(&file).ok()
        } else {
            None
        };
        // 回滚只删本次新建的目录：目录原本就在（哪怕里面没有 theme.toml）时只撤掉写入的文件。
        let created_dir = !target.exists();
        std::fs::create_dir_all(&target)?;
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &file)?;

        // 依赖链校验：写入后按真实主题目录做完整 base 链合并求值，
        // 捕获「base 引用的基础主题不存在」「继承成环」「合并后结构非法」等 validate_text 单文件校验
        // 无法发现的问题。校验失败则回滚（新写入的删除目录；覆盖的恢复原文本）。
        let dirs = self.theme_dirs();
        if let Err(e) = wind_theme::theme::load_typed_dirs(&dirs, &theme_id) {
            match backup {
                Some(bytes) => {
                    let _ = std::fs::write(&file, bytes);
                }
                None if created_dir => {
                    let _ = std::fs::remove_dir_all(&target);
                }
                None => {
                    let _ = std::fs::remove_file(&file);
                }
            }
            anyhow::bail!(
                "主题依赖校验失败：{}（请检查 base 引用的基础主题是否存在）",
                e
            );
        }

        let reloaded = self.theme_reload_if_current(&theme_id);
        Ok(json!({
            "ok": true,
            "slug": theme_id,
            "display_name": meta.name,
            // 供设置层如实回报给编辑器。
            "reloaded": reloaded,
        }))
    }

    /// `theme.previewPackage { path }`：只读预览一个 `.wtheme`，不落任何盘。
    ///
    /// 设置端据此弹「要导入这个主题吗」的确认。
    fn web_theme_preview_package(&self, params: &Value) -> anyhow::Result<Value> {
        let path = str_param(params, "path")?;
        let p = wind_transfer::theme::preview_package(std::path::Path::new(path))?;
        Ok(json!({
            "display_name": p.display_name,
            "author": p.author,
            "version": p.version,
            "asset_count": p.asset_count,
            "has_preview": p.has_preview,
        }))
    }

    /// `theme.importPackage { path, slug?, force? }`：主题包落盘。
    ///
    /// 回执形状与 `theme.importFromText` 一致（ok/slug/display_name/reloaded/conflict）。
    fn web_theme_import_package(&self, params: &Value) -> anyhow::Result<Value> {
        let path = str_param(params, "path")?;
        let package = std::path::Path::new(path);
        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // 先预览：既拿到展示名（未传 slug 时用它定目录 id），也把坏包挡在动目录之前。
        let preview = wind_transfer::theme::preview_package(package)?;
        let (theme_id, target) = self.theme_target_dir(params, &preview.display_name)?;
        if target.exists() && !force {
            return Ok(Self::theme_conflict_json(
                &theme_id,
                &preview.display_name,
                &target,
            ));
        }

        // 依赖链校验交给 transfer 的校验回调：要在新目录已就位、旧目录未删时跑，失败才回滚得掉。
        // 与单文件导入回滚方式不同，判据（能否按 base 链加载）相同。
        let dirs = self.theme_dirs();
        let id_for_verify = theme_id.clone();
        let out = wind_transfer::theme::import_package(package, &target, move |_| {
            wind_theme::theme::load_typed_dirs(&dirs, &id_for_verify)
                .map(|_| ())
                .map_err(|e| {
                    anyhow::anyhow!("主题依赖校验失败：{e}（请检查 base 引用的基础主题是否存在）")
                })
        })?;

        let reloaded = self.theme_reload_if_current(&theme_id);
        Ok(json!({
            "ok": true,
            "slug": theme_id,
            "display_name": out.display_name,
            "files": out.files,
            "reloaded": reloaded,
        }))
    }

    /// `theme.exportPackage { slug, path }`：把一个已装主题打成 `.wtheme`。
    ///
    /// 源目录走整条搜索链（内置主题也导得出）。落点由调用方给全路径（与 `scheme.exportPackage` 同形）。
    ///
    /// `path` 不做任何校验，前提是它永远不由网页或远端决定：本 RPC 只经本机 ctrl 通道调用，
    /// 路径来自原生保存框或 CLI 参数；主题编辑器连接若要出包，`path` 须是设置端在私有临时目录
    /// 以 O_EXCL 新建的文件。哪天让网页或远端影响 `path`，必须先加落点白名单（否则是任意写）。
    fn web_theme_export_package(&self, params: &Value) -> anyhow::Result<Value> {
        let slug = str_param(params, "slug")?;
        let out = str_param(params, "path")?;
        let dir = self.theme_dir_of(slug)?;
        let png = self.theme_preview_png(&dir);
        let out_path = std::path::Path::new(out);

        // 先写 `.tmp` 兄弟文件，自检通过才 rename 到位（与导入侧「中途失败目标原封不动」同一纪律）：
        // 直接截断写 `out_path` 的话，自检失败会毁掉该路径上原有的好包。
        let tmp = out_path.with_extension("wtheme.tmp");
        let done = (|| -> anyhow::Result<wind_transfer::theme::ThemePreview> {
            wind_transfer::theme::export_package(&dir, &tmp, png.as_deref())?;
            // 读回自检：导出与导入各有一套条目判据，两边走岔时在这里当场报出。
            wind_transfer::theme::preview_package(&tmp)
        })();
        let p = match done {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                // 补一句主语（哪个主题），底层原因由 RPC 出口按 `{:#}` 展开整条链发给客户端。
                return Err(e.context(format!("导出主题「{slug}」失败")));
            }
        };
        std::fs::rename(&tmp, out_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            anyhow::anyhow!("写入 {} 失败：{e}", out_path.display())
        })?;

        Ok(json!({
            "ok": true,
            "path": out_path.to_string_lossy(),
            "display_name": p.display_name,
            "asset_count": p.asset_count,
            "has_preview": p.has_preview,
        }))
    }

    /// 主题目录下的市场预览图，没有或读不动就当没有。
    ///
    /// 有界读取：常驻服务进程不能被一张误放的原图撑爆。超限与读失败都按「没有预览图」处理，
    /// 但要记日志，否则用户无从追查回执里为何少了预览图。
    fn theme_preview_png(&self, dir: &std::path::Path) -> Option<Vec<u8>> {
        /// 与 `wind-transfer` 给主题文本的限额同档（4 MiB）：预览图是分发用的缩略图，
        /// 越过这个量级的必然是误放的原图。
        const MAX_PREVIEW_BYTES: u64 = 4 * 1024 * 1024;

        let path = dir.join(wind_transfer::theme::THEME_PREVIEW_NAME);
        match std::fs::metadata(&path) {
            Err(_) => None, // 没有预览图是常态，不值得一条日志
            Ok(m) if m.len() > MAX_PREVIEW_BYTES => {
                tracing::warn!(
                    "主题预览图过大（{} 字节，上限 {MAX_PREVIEW_BYTES}），本次导出不含预览图",
                    m.len()
                );
                None
            }
            Ok(_) => match std::fs::read(&path) {
                Ok(b) => Some(b),
                Err(e) => {
                    tracing::warn!("主题预览图读取失败（{e}），本次导出不含预览图");
                    None
                }
            },
        }
    }

    /// `appearance.previewTemplate`：模板在当前主题下的渲染效果（设置页输入框下方的预览行）。
    ///
    /// 入参 `{ template, scene: "comment" | "label" | "content", each?, swatches?: [名字], sample? }`。
    /// `sample` 是样例 id（缺省 = 「你好」），未知 id 报错。回包：
    /// ```text
    /// { candidate, sample,                     // 本次用的样例词与它的 id
    ///   samples: [{id, text, lang: "zh"|"en"}], // 全部可用样例，设置端据此按语种取
    ///   text, fg, bg, runs: [{start, end, rgba}],
    ///   selected: { fg, bg, runs } | null,      // 仅 comment：高亮候选里的样子
    ///   problems: [{start, end, message}],
    ///   swatches: { 名字: rgba } }              // 入参 swatches 里每个名字写成 `$[名字]` 的实际颜色
    /// ```
    /// `swatches` 给设置页「插入颜色」的色块着色：求法与模板里的内联色完全相同（气泡场景先查
    /// `tooltip_<名>`），色块因此就是插进去以后的样子。
    /// 两类区间坐标系不同：`runs` 是输出偏移（`text` 里的 UTF-8 字节区间，给预览上色）；
    /// `problems` 是模板偏移（入参 `template` 里的 UTF-8 字节区间，给输入框标位置）。设置端
    /// 的输入框显示转义后的模板（`\t` 以两个字符显示），要把 `problems` 换算到显示文本上再标。
    /// 颜色一律 `[R, G, B, A]`。等于正文色的区间不下发（与渲染层同一口径）。
    fn web_appearance_preview_template(&self, params: &Value) -> anyhow::Result<Value> {
        use wind_coordinator::template_preview::TemplateScene;
        use wind_theme::fallback;
        use wind_theme::{Atom, Rgba, TextState};
        let template = str_param(params, "template")?;
        // 入参上限：设置页的模板输入框是单行，真实模板几十字节；上限只为挡住异常入参
        // （引擎本身已有嵌套上限与线性配对，这里再省掉无意义的大块求值）。
        anyhow::ensure!(
            template.len() <= PREVIEW_TEMPLATE_MAX,
            "模板过长（{} 字节，上限 {PREVIEW_TEMPLATE_MAX}）",
            template.len()
        );
        let swatch_names: Vec<&str> = params
            .get("swatches")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        anyhow::ensure!(
            swatch_names.len() <= PREVIEW_SWATCHES_MAX,
            "swatches 过多（{} 个，上限 {PREVIEW_SWATCHES_MAX}）",
            swatch_names.len()
        );
        let scene = match str_param(params, "scene")? {
            "comment" => TemplateScene::Comment,
            "label" => TemplateScene::TooltipLabel,
            "content" => TemplateScene::TooltipContent {
                each: params
                    .get("each")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            },
            other => anyhow::bail!("未知 scene：{other}"),
        };
        let is_tooltip = scene != TemplateScene::Comment;
        let sample_id = match params.get("sample") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.as_str()),
            Some(v) => anyhow::bail!("sample 须为字符串：{v}"),
        };
        let preset = wind_coordinator::template_preview::find_sample(sample_id)
            .ok_or_else(|| anyhow::anyhow!("未知样例：{}", sample_id.unwrap_or_default()))?;
        let sample = self.template_sample(template, &scene, preset);
        // 主题现取：与候选窗同一条搜索链、主题与明暗，每次从磁盘解析（低频操作，不挂缓存，设计 §11）。
        // 取不到时退化为兜底色预览并给一条总提示，不逐个把颜色名误报成「当前主题没有」。
        let loaded = wind_theme::load_resolved_dirs(
            &self.theme_search_dirs(),
            &self.current_theme_name(),
            self.current_theme_is_dark(),
        );
        let theme_ok = loaded.is_ok();
        let theme = loaded.unwrap_or_default();
        let default_node = wind_theme::RvNode::default();
        // 兜底色与渲染层（wind-ui 候选窗 / 气泡）取同一组常量。
        let (node, fallback): (&wind_theme::RvNode, Rgba) = if is_tooltip {
            (
                theme.views.tooltip.as_ref().unwrap_or(&default_node),
                theme.color("tooltip_text", fallback::TOOLTIP_TEXT),
            )
        } else {
            (&theme.views.comment, fallback::COMMENT_TEXT)
        };
        let runs_of = |state: TextState| -> (Value, Rgba) {
            let body = wind_theme::body_color(node, state, fallback);
            let runs: Vec<Value> = sample
                .text
                .spans()
                .iter()
                .filter_map(|sp| {
                    let c = wind_theme::span_color(
                        &theme,
                        node,
                        is_tooltip,
                        state,
                        fallback,
                        sp.role,
                        sp.in_title,
                        sp.separator,
                        sp.color.as_deref(),
                    );
                    (c != body).then(|| json!({"start": sp.start, "end": sp.end, "rgba": c}))
                })
                .collect();
            (Value::Array(runs), body)
        };
        let (runs, fg) = runs_of(TextState::Normal);
        let bg = if is_tooltip {
            // 同 wind-ui 气泡：调色板兜底，节点底色覆盖。
            node.bg_color
                .unwrap_or(theme.color("tooltip_bg", fallback::TOOLTIP_BG))
        } else {
            theme.views.window.bg_color.unwrap_or(fallback::WINDOW_BG)
        };
        let selected = (!is_tooltip).then(|| {
            let (runs, fg) = runs_of(TextState::Selected);
            let bg = theme
                .views
                .item
                .selected
                .as_ref()
                .and_then(|n| n.bg_color)
                .unwrap_or(fallback::SELECTED_BG);
            json!({"fg": fg, "bg": bg, "runs": runs})
        });

        // 问题：变量（主题无关，含场景外变量）+ 内联色（写法 / 当前主题查不查得到 / 全透明 /
        // 被忽略的 SPEC 片段）。
        let mut problems: Vec<Value> = sample
            .problems
            .iter()
            .map(|p| json!({"start": p.start, "end": p.end, "message": p.message}))
            .collect();
        if !theme_ok && !sample.colors.is_empty() {
            problems.push(json!({
                "start": 0, "end": 0,
                "message": "当前主题加载失败，预览按兜底色显示，颜色名无法核对",
            }));
        }
        // 名字在当前场景下查到的颜色：气泡先查 `tooltip_<名>`（与 span_color 同一顺序）。
        let lookup = |n: &str| -> Option<Rgba> {
            is_tooltip
                .then(|| theme.palette.get(&format!("tooltip_{n}")))
                .flatten()
                .or_else(|| theme.palette.get(n))
                .copied()
        };
        let transparent = "全透明的颜色看不见，按正文色显示".to_string();
        for c in &sample.colors {
            let mut msgs: Vec<String> = Vec::new();
            let mut refs = vec![&c.color.normal];
            match &c.color.selected {
                // `selected=` 只在候选注释里有意义：气泡没有选中态，引擎忽略它，不必查名字。
                Some(_) if is_tooltip => msgs.push("「selected=」在气泡里不生效，已忽略".into()),
                Some(sel) => refs.push(sel),
                None => {}
            }
            for r in refs {
                for atom in [&r.light, &r.dark] {
                    let m = match atom {
                        Atom::Invalid => Some(
                            "颜色写法不对：写 #RGB / #RRGGBB / #RRGGBBAA 或调色板颜色名"
                                .to_string(),
                        ),
                        Atom::Rgba(rgba) if rgba[3] == 0 => Some(transparent.clone()),
                        Atom::Rgba(_) => None,
                        Atom::Name(n) if &**n == "transparent" => Some(transparent.clone()),
                        // 主题没加载成功：上面已给总提示，不逐个误报。
                        Atom::Name(_) if !theme_ok => None,
                        Atom::Name(n) => match lookup(n) {
                            None => Some(format!("当前主题没有颜色「{n}」，按正文色显示")),
                            Some(rgba) if rgba[3] == 0 => {
                                Some(format!("「{n}」在当前主题里是全透明，按正文色显示"))
                            }
                            Some(_) => None,
                        },
                    };
                    if let Some(m) = m
                        && !msgs.contains(&m)
                    {
                        msgs.push(m);
                    }
                }
            }
            msgs.extend(c.notes.iter().cloned());
            for m in msgs {
                problems.push(json!({"start": c.start, "end": c.end, "message": m}));
            }
        }
        problems.sort_by_key(|p| p["start"].as_u64());

        let swatches: serde_json::Map<String, Value> = swatch_names
            .into_iter()
            .map(|name| {
                let c = wind_theme::InlineColor::parse(name);
                let rgba = wind_theme::span_color(
                    &theme,
                    node,
                    is_tooltip,
                    TextState::Normal,
                    fallback,
                    None,
                    false,
                    false,
                    Some(&c),
                );
                (name.to_string(), json!(rgba))
            })
            .collect();

        let samples: Vec<Value> = wind_coordinator::template_preview::SAMPLES
            .iter()
            .map(|s| json!({"id": s.id, "text": s.text, "lang": s.lang.as_str()}))
            .collect();
        Ok(json!({
            "candidate": preset.text,
            "sample": preset.id,
            "samples": samples,
            "text": sample.text.as_str(),
            "fg": fg,
            "bg": bg,
            "runs": runs,
            "selected": selected,
            "problems": problems,
            "swatches": swatches,
        }))
    }

    /// `theme.list`。`skin`（默认假）为真时才补 `skin` / `animated`：它们要对每个主题做一次
    /// 完整解析（占整条 RPC 的九成），而设置端启动时列表是必取项、这两项只有「主题信息」
    /// 才用。`id` 非空时只回那一项。
    fn web_theme_list(&self, params: &Value) -> anyhow::Result<Value> {
        let with_skin = params.get("skin").and_then(Value::as_bool).unwrap_or(false);
        let only = params
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        // 复用右键菜单的 list_themes_full 顺序，保证与菜单一致 (#5/主题)。
        let dirs = self.theme_search_dirs();
        let out: Vec<Value> = self
            .list_themes_full()
            .into_iter()
            .filter(|(id, _, _)| only.is_none_or(|o| o == id))
            .map(|(id, display, builtin)| {
                let meta = wind_theme::read_meta(&dirs, &id);
                let dir = wind_theme::theme::find_theme_dir(&dirs, &id);
                let base = dir
                    .as_ref()
                    .and_then(|d| {
                        std::fs::read_to_string(d.join(wind_transfer::theme::THEME_ENTRY_NAME)).ok()
                    })
                    .and_then(|t| t.parse::<toml::Value>().ok())
                    .and_then(|v| v.get("base")?.as_str().map(str::to_string))
                    .unwrap_or_default();
                let mut item = json!({
                    "name": id,
                    "display_name": display,
                    "author": meta.as_ref().map(|m| m.author.clone()).unwrap_or_default(),
                    "version": meta.as_ref().map(|m| m.version.clone()).unwrap_or_default(),
                    "builtin": builtin,
                    // 内置主题的目录是安装目录里的实现细节，只给用户主题。
                    "dir": dir
                        .filter(|_| !builtin)
                        .map(|d| d.display().to_string())
                        .unwrap_or_default(),
                    "base": base,
                });
                if with_skin {
                    let (skin, animated) = wind_theme::load_resolved_dirs(&dirs, &id, false)
                        .map(|t| theme_skin_summary(&t))
                        .unwrap_or_default();
                    item["skin"] = json!(skin);
                    item["animated"] = json!(animated);
                }
                item
            })
            .collect();
        Ok(json!(out))
    }
}

/// 主题是否带异形皮肤、皮肤是否会动：装饰写了事件状态机，或框图 / 装饰图（含 srcset 的各档）
/// 文件头是动画格式（[`wind_theme::skin::sniff_animated`]，只读头部不解码；GIF 一律按动图算，
/// 单帧 GIF 会误报）。给设置端「主题信息」看的概要。
fn theme_skin_summary(t: &wind_theme::Resolved) -> (bool, bool) {
    let Some(skin) = t.skin.as_deref() else {
        return (false, false);
    };
    let variants = [
        &skin.horizontal,
        &skin.vertical,
        &skin.horizontal_embedded,
        &skin.vertical_embedded,
    ];
    let variants = variants.into_iter().flatten();
    if variants
        .clone()
        .flat_map(|v| &v.decor)
        .any(|d| !d.anim.states.is_empty() || !d.anim.on.is_empty())
    {
        return (true, true);
    }
    let animated = variants
        .flat_map(|v| {
            let frame = std::iter::once(v.frame.image.as_str())
                .chain(v.frame.srcset.iter().map(|(p, _)| p.as_str()));
            let decor = v.decor.iter().map(|d| d.image.as_str());
            frame.chain(decor).collect::<Vec<_>>()
        })
        .any(|r| image_file_animated(t, r));
    (true, animated)
}

/// 读图片文件头（至多 64 KiB）判是不是动图；读不到（含 data: 内联图）算不是。图片引用的
/// 解析顺序同渲染端 `wind_scene::theme_assets::asset_path`：资源表 → 绝对路径 → base 链目录。
fn image_file_animated(t: &wind_theme::Resolved, reference: &str) -> bool {
    use std::io::Read as _;
    let path = match t.resources.get(reference) {
        Some(p) => std::path::PathBuf::from(p),
        None if std::path::Path::new(reference).is_absolute() => reference.into(),
        None => match t
            .asset_dirs
            .iter()
            .map(|d| d.join(reference))
            .find(|p| p.is_file())
        {
            Some(p) => p,
            None => return false,
        },
    };
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(64 * 1024).read_to_end(&mut head))
        .is_ok_and(|_| wind_theme::skin::sniff_animated(&head))
}

// 不用 blanket impl：`Arc<Coordinator>` 的方法解析会先命中 T=Arc 的 blanket 候选、
// bound 不满足即报错而不再 deref；具体 impl 才能让 Arc 自动 deref 到 &Coordinator。
impl WebDataRpc for wind_coordinator::Coordinator {}

/// 存储域 → 设置页显示域：真实文本投影为可编辑的转义形态（真换行→`\n`、制表→`\t`、
/// 反斜杠→`\\`）。
///
/// 编辑框里看不见的字符不可编辑，投影成转义形态后所见即所得（与 [`word_item`] 的 `code` 同一原则：
/// 存储域与显示域分开）。命令栏语法条目只投影换行/制表、反斜杠原样，免得双重转义，
/// 见 [`wind_store::wdict::escape_text_field`]。
fn ui_text(s: &str) -> String {
    wind_store::wdict::escape_text_field(s)
}

/// 影子规则行的显示文本：`candId` 形如 `phrase:{code}:{模板}` 时取模板原文，否则回落 `word`。
///
/// 与设置端 `pages::dict::spec::WordRow::display_text` 同一判据（那边定显示，这边定搜索/排序）。
/// 取第二个冒号之后的全部内容，不能按冒号切三段：模板自身常含冒号（`{time("HH:mm:ss")}`）。
fn shadow_display_text(row: &Value) -> String {
    let word = row.get("word").and_then(|v| v.as_str()).unwrap_or("");
    row.get("candId")
        .and_then(|v| v.as_str())
        .and_then(|id| id.strip_prefix("phrase:"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_code, tpl)| tpl)
        .filter(|tpl| !tpl.is_empty())
        .unwrap_or(word)
        .to_string()
}

/// 疑似笔误 → JSON。`kind` 供 UI 判定，`message` 是给人看的一句话。
fn hint_json(h: &wind_cmdbar::Hint) -> Value {
    use wind_cmdbar::Hint as H;
    match h {
        H::ControlCharInPath(f) => json!({
            "kind": "controlCharInPath",
            "func": f,
            "message": format!("{f} 的路径里出现了换行或制表符，通常是反斜杠只写了一个"),
        }),
    }
}

/// 设置页显示域 → 存储域：[`ui_text`] 的逆。
///
/// 凡是从设置页收 text 的 RPC 都必须先过它，包括拿 text 当 key 匹配的 `dict.remove`、
/// `freq.delete`、`temp.promote` 等，否则静默查不到。
fn store_text(s: &str) -> String {
    wind_store::wdict::unescape_text_field(s)
}

/// 用户词 → 设置页列表项（前端 UserWordItem）。
///
/// `code` 输出带空格的音节码（`ni hao`），与 `dict.encode` 同形；回传给 add/update/remove 时由
/// `user_words_api` 的 `normalize_add_code` / `remove` 拆回扁平码。无边界的码不含空格。
fn word_item(r: wind_store::user_words::UserWordRecord) -> Value {
    let code = user_words_api::display_code(&r);
    json!({ "code": code, "text": ui_text(&r.text), "weight": r.weight, "enabled": true })
}

/// `appearance.previewTemplate` 的模板长度上限（字节）。设置页输入框是单行，真实模板几十字节。
pub const PREVIEW_TEMPLATE_MAX: usize = 4096;
/// `appearance.previewTemplate` 一次最多求几个色块（设置页用 8 个）。
pub const PREVIEW_SWATCHES_MAX: usize = 32;

/// `getConfig` 附带的只读旁路字段，回传 `saveConfig` 时必须剥掉。
///
/// 它们是当前生效值的快照；设置页拿整份 getConfig 改几项原样回传，不剥的话 `json_diff` 会把
/// 快照写进 override，方案行为从此冻结在打开设置页那一刻。在服务端剥，任何客户端都受保护。
pub const READONLY_SIDECAR_FIELDS: &[&str] = &[
    "effectiveCodetable",
    // 取消覆盖后的回落值 + override 层原文（后者不剥会在 override 里留下无人读的一段）。
    "followedCodetable",
    "codetableOverride",
    // override 层的 `[punct]` 段原文，回答自定义标点表是方案作者写的还是用户改的。
    // 设置页尚未消费它（作者写了表时取消勾选回到的是作者的表而非全局）；要做那一档时直接用它。
    "punctOverride",
    "leadingCodeKeys",
    "keysOverview",
    // 「总览的全局层不完整」的说明（段级降级时非 null），与 `keysOverview` 同进同出。
    "keysOverviewDegraded",
    // 「跟随全局」时的实际值（布局/字体/辅助码）。
    "followedBehavior",
    // 方案文件自己的辅助码来源（设置页判「跟随方案」用）；不剥则方案作者后续改的推荐码表透不过来。
    "auxCodeBaseFiles",
];

/// [`WebDataRpc::followed_behavior`] 的纯函数内核：配置与主题字体由调用方给定。
///
/// - `layout`：与运行时同一裁决（`Orientation::from_layout_str`），只出 `horizontal`/`vertical`；
/// - `fontFamily`：全局 `ui.font.family`，空（= 跟随主题）则取主题字体，再空为 `""`；
/// - `auxEnabled` / `auxMaxPhraseLen` / `auxDirect` / `auxContinuous`：全局 `schema.pinyin.aux_code`。
fn followed_behavior_of(cfg: &wind_config::Config, theme_font: Option<String>) -> Value {
    let font = if cfg.ui.font.family.is_empty() {
        theme_font.unwrap_or_default()
    } else {
        cfg.ui.font.family.clone()
    };
    let aux = &cfg.schema.pinyin.aux_code;
    json!({
        "layout": wind_config::Orientation::from_layout_str(&cfg.ui.candidate.layout).layout_str(),
        "fontFamily": font,
        "perPage": cfg.ui.candidate.per_page, // fold-gate:allow 显示「跟随」的全局值
        "auxEnabled": aux.enabled,
        "auxMaxPhraseLen": aux.max_phrase_len,
        "auxDirect": aux.direct,
        "auxContinuous": aux.continuous,
    })
}

/// 按键总览：这个方案下每个绑过的键当前干什么、来自哪一层。
///
/// 由内核组装：组名展开（`page_keys = ["minus_equal"]` → `minus` / `equal`）住在
/// `KeysConfig::effective_session_actions`，设置端不另写一份。只读：反写就要决定写哪一层，
/// 设置页据此展示并跳转到对应编辑处（`docs/design/key-resolver-unification.md` §4.3）。
/// 动词给原值（`page_prev`）不翻译，翻译归 UI。
///
/// 段级降级时不可信的全局层不列，并经第二个返回值说明缺了哪张表、因哪些段降级；判据见
/// `docs/design/key-resolver-unification.md` 的「按键总览（`keysOverview`）的段级降级判据」。
/// 返回 `(总览, 不完整说明)`；后者无降级时为 `Value::Null`。
fn keys_overview(schema_cfg: &Value) -> (Vec<Value>, Value) {
    let cfg = match wind_config::Config::load(wind_config::Config::data_dir().as_deref()) {
        Ok(mut cfg) => {
            // 与 `ConfigBundle::build` 一样先 normalize：那里还有存量迁移（旧字段折算进
            // `key_actions` / `session_actions`），跳过它算出来的表与运行时不一致。
            cfg.normalize();
            cfg
        }
        // 读不到全局配置：与 `total_fallback` 合流，全局层一律不列，方案层照列（不受 `Config` 降级影响，
        // 整表不给会与「一个键都没绑」无从区分）。段级降级之后这条分支基本不可达，留作防御。
        Err(_) => {
            let mut cfg = wind_config::Config::default();
            cfg.degradation.total_fallback = true;
            cfg
        }
    };
    keys_overview_of(&cfg, schema_cfg)
}

/// [`keys_overview`] 的纯函数内核：配置由调用方给定，不碰磁盘也不碰环境变量。
///
/// 拆出来让降级判据能被普通单测覆盖：`WIND_INSTALL_ROOT` / `WIND_DATADIR_CONF` 经 OnceLock
/// 只认第一次，走 [`keys_overview`] 造降级配置不可靠；直接置 `Config::degradation` 即等价输入。
fn keys_overview_of(cfg: &wind_config::Config, schema_cfg: &Value) -> (Vec<Value>, Value) {
    let schema_table = |key: &str| -> std::collections::BTreeMap<String, String> {
        schema_cfg
            .get(key)
            .and_then(|x| x.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };

    let empty = std::collections::BTreeMap::new();
    let session = cfg.keys.effective_session_actions();
    let mut out = Vec::new();
    let mut tainted: Vec<(&str, &[&str])> = Vec::new();
    // 每张表：(总览表名, 方案文件里的字段名, 全部来源路径, 全局层的值)。
    // 来源路径是并集而非同名字段（坏段按直接子键定位）：给 `KeysConfig::effective_session_actions`
    // 加折算来源时这里要一起加，漏了无任何信号。`input` 段的跨段迁移刻意不进判据，
    // 见 docs/design/key-resolver-unification.md 的「按键总览（`keysOverview`）的段级降级判据」。
    for (table, field, sources, global) in [
        (
            "lead",
            "key_actions",
            &["keys.key_actions"][..],
            &cfg.keys.key_actions,
        ),
        (
            "session",
            "session_actions",
            &[
                "keys.session_actions",
                "keys.page_keys",
                "keys.select_char_keys",
                "keys.highlight_keys",
                "keys.select_key_groups",
            ][..],
            &session,
        ),
    ] {
        let global = if sources.iter().any(|p| cfg.degradation.taints(p)) {
            tainted.push((table, sources));
            &empty
        } else {
            global
        };
        push_overview_layer(&mut out, table, global, &schema_table(field));
    }

    let degraded = if tainted.is_empty() {
        // 字段恒在、无降级时显式 `null`：否则设置端分不清「无降级」与「这版 core 还没实现」。
        Value::Null
    } else {
        // 只列真正导致上面这些表被判不可信的降级段（如 `ui.font` 坏了不该出现在这里）。
        // 判定复用 `taints`（单段构造一份 `ConfigDegradation`），不重写一遍路径关系。
        let causes: Vec<&String> = cfg
            .degradation
            .sections
            .iter()
            .filter(|s| {
                let one = wind_config::config::ConfigDegradation {
                    sections: vec![(*s).clone()],
                    total_fallback: false,
                    unparsable: Vec::new(),
                };
                tainted
                    .iter()
                    .any(|(_, srcs)| srcs.iter().any(|p| one.taints(p)))
            })
            .collect();
        json!({
            // 这些表的全局层未列出；表名与 `keysOverview` 条目里的 `table` 同域。
            "tables": tainted.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            // 点分路径原样传出（`keys.page_keys`），与 `config.degradation` 同一份来源。
            // `totalFallback` 时本表为空——那种情形下 `sections` 本来就定位不到任何段。
            "sections": causes,
            "totalFallback": cfg.degradation.total_fallback,
        })
    };
    (out, degraded)
}

/// 一张表的两层仲裁：方案层表了态就用方案的，否则用全局的。
///
/// 方案层的显式 `none` 也是表态（本方案禁用），不回落全局，与内核
/// `Coordinator::session_action_for` / `bound_action_with_source` 一致。
///
/// `ineffective`：方案层 `[key_actions]` 的组合键没有消费通路（组合键只从全局 `keys.key_actions`
/// 编进热键表，方案层只按单键查）。补通路会破「C++ 吃键集 ⊆ Rust 出字集」，故只如实标出：
/// 这类条目带 `ineffective: true` 照常列出且不遮盖同名全局条目。判据用 core 的
/// `route_of_key_action`。字段恒在（其余条目为 `false`），设置端才分得清「没实现」。
fn push_overview_layer(
    out: &mut Vec<Value>,
    table: &str,
    global: &std::collections::BTreeMap<String, String>,
    schema: &std::collections::BTreeMap<String, String>,
) {
    let schema_ineffective = |k: &str| {
        table == "lead"
            && wind_config::hotkey::route_of_key_action(k)
                == Some(wind_config::hotkey::KeyActionRoute::Hotkey)
    };
    let row = |k: &str, action: &str, from: &str, ineffective: bool| {
        json!({
            "key": k,
            "table": table,
            "action": action,
            "from": from,
            "ineffective": ineffective,
        })
    };
    let mut keys: std::collections::BTreeSet<&String> = global.keys().collect();
    keys.extend(schema.keys());
    for k in keys {
        match schema.get(k) {
            Some(v) if schema_ineffective(k) => {
                // 不生效的方案条目不遮盖全局：全局那条先列（它才是实际生效的）。
                if let Some(g) = global.get(k) {
                    out.push(row(k, g, "global", false));
                }
                out.push(row(k, v, "schema", true));
            }
            Some(v) => out.push(row(k, v, "schema", false)),
            None => out.push(row(
                k,
                global.get(k).map(String::as_str).unwrap_or(""),
                "global",
                false,
            )),
        }
    }
}

fn strip_readonly_fields(cfg: &Value) -> Value {
    let Some(o) = cfg.as_object() else {
        return cfg.clone();
    };
    if !READONLY_SIDECAR_FIELDS.iter().any(|k| o.contains_key(*k)) {
        return cfg.clone();
    }
    let mut o = o.clone();
    for k in READONLY_SIDECAR_FIELDS {
        o.remove(*k);
    }
    Value::Object(o)
}

/// 稀疏 diff：返回 `cfg` 相对 `base` 的变化项（仅含改动的叶子/键）；无变化返回 None。
/// 对象逐键递归；数组/标量按整体比较（不同则取 cfg）。用于 schema override 最小化。
fn json_diff(base: &Value, cfg: &Value) -> Option<Value> {
    match (base, cfg) {
        (Value::Object(b), Value::Object(c)) => {
            let mut out = serde_json::Map::new();
            for (k, cv) in c {
                match b.get(k) {
                    // `custom_mappings` 必须作原子叶子，与 `merge_toml` 读侧的整体替换配对：逐键下钻的话
                    // override 只剩改动那一行，读侧整体替换后方案作者写的其余行全部丢失。
                    // 改合并语义（读侧）时必须同步查写侧。键空间是用户数据，本就不该按路径下钻。
                    Some(bv) if k == "custom_mappings" => {
                        if bv != cv {
                            out.insert(k.clone(), cv.clone());
                        }
                    }
                    Some(bv) => {
                        if let Some(d) = json_diff(bv, cv) {
                            out.insert(k.clone(), d);
                        }
                    }
                    None => {
                        out.insert(k.clone(), cv.clone());
                    }
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(Value::Object(out))
            }
        }
        _ => {
            if base == cfg {
                None
            } else {
                Some(cfg.clone())
            }
        }
    }
}

/// JSON → toml::Value（写 override 文件）。null 在对象中跳过（TOML 无 null）。
fn json_to_toml(v: &Value) -> toml::Value {
    match v {
        Value::Null => toml::Value::String(String::new()),
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else {
                toml::Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => toml::Value::String(s.clone()),
        // 跳过数组内 null（TOML 无 null），与对象分支语义一致，避免注入空串污染类型。
        Value::Array(a) => toml::Value::Array(
            a.iter()
                .filter(|x| !x.is_null())
                .map(json_to_toml)
                .collect(),
        ),
        Value::Object(o) => {
            let mut t = toml::map::Map::new();
            for (k, val) in o {
                if !val.is_null() {
                    t.insert(k.clone(), json_to_toml(val));
                }
            }
            toml::Value::Table(t)
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "phrase_shadowing_tests.rs"]
mod phrase_shadowing_tests;

/// 从 `.dict.yaml` 的 YAML 头读 `name:`，读不到返回空串（调用方回落文件名）。
///
/// 只读到头部结束（独占一行的 `...`）：正文可能有几百 MB，而这里只是给设置页的下拉
/// 取个显示名。
fn dict_yaml_name(path: &std::path::Path) -> String {
    use std::io::BufRead;
    let Ok(f) = std::fs::File::open(path) else {
        return String::new();
    };
    for line in std::io::BufReader::new(f).lines().take(200) {
        let Ok(line) = line else { return String::new() };
        if line.trim_end() == "..." {
            break;
        }
        // 顶格的 `name:` 才是词库名（缩进的同名键属于别的映射）。
        if line.starts_with("name:")
            && let Some(v) = line["name:".len()..].split('#').next()
        {
            return v.trim().trim_matches('"').trim_matches('\'').to_string();
        }
    }
    String::new()
}

#[cfg(test)]
#[path = "lib_schema_key_gate_wiring_tests.rs"]
mod schema_key_gate_wiring_tests;

#[cfg(test)]
#[path = "lib_followed_behavior_tests.rs"]
mod followed_behavior_tests;

#[cfg(test)]
#[path = "memory_stats_tests.rs"]
mod memory_stats_tests;

#[cfg(test)]
#[path = "lib_system_stats_tests.rs"]
mod system_stats_tests;

#[cfg(test)]
#[path = "sys_dict_categories_tests.rs"]
mod sys_dict_categories_tests;

#[cfg(test)]
#[path = "comment_library_tests.rs"]
mod comment_library_tests;

#[cfg(test)]
#[path = "lib_theme_list_tests.rs"]
mod theme_list_tests;

// 放在文件最末：`schema_key_gate_wiring_tests` 的源码守卫只扫第一个 `#[cfg(test)]` 之前的部分，
// 声明在文件头会把整个生产代码截没。
#[cfg(test)]
mod batch_tests;
