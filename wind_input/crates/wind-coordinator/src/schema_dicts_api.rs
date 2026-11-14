//! 方案附加词库（扩展库 / 细胞词库）的列表与开关，桌面设置页与移动端门面共用。
//!
//! # 为什么在这里而不在 wind-webdata
//!
//! 开关的本体原是 wind-webdata 的 `schema.setDictEnabled`。移动端设置页要在「方案 → 拼音」
//! 等页逐个开关同一批库，但 **Android 闭包刻意不含 wind-webdata**（见本 crate Cargo.toml
//! 的 ⚠ 注释），wind-mobile 调不到它。照抄一份的话，「override 只落 `{id, enabled}` 稀疏项」
//! 与「先持久化再 live 翻、含混输扇出」这两条迟早两边各改各的——前者漂了会冻结整份词库
//! 定义，后者漂了就是「关了没反应」（见 `EngineManager::set_dict_enabled_live` 的说明）。
//! 它们只依赖 [`WebDataHost`] 窄面，故同 [`crate::user_words_api`] 下沉到这里，两端各自委托。
//!
//! # `user:` 用户分库也走这里
//!
//! 合并后的 `dictionaries` 里也登记着用户分库（`user:<名>`），「方案里有没有这个库」的校验
//! 对两类库是同一份；分流点放在校验之后，才能保住桌面对未知 id 的报错文案一字不变。用户分库
//! 那一支只用到 store 与 `set_dict_enabled_live`，同样在窄面之内，没有理由单独留在 webdata。
//!
//! # 不在这里做的
//!
//! - **扩展库的只读闸**：桌面扩展库开关从来不查只读模式（override 落在配置目录，不属于
//!   用户数据库），本层不另加闸以保桌面行为不变；移动端门面自己先查。用户分库那一支照旧查。
//! - **用户分库的列表**：它有自己的条目数 / 文件 / 损坏状态等字段，见 `userDicts.list`，
//!   [`list`] 只列方案文件（含发现目录）登记的附加库。

use serde_json::{Value, json};
use wind_config::schema::DictSpec;
use wind_engine::manager::{EngineManager, EntryMatch, EntryQuery, EntrySort};
use wind_store::user_dicts::ID_PREFIX;

use crate::web_host::WebDataHost;

/// 一个可开关的附加词库（设置页列表项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDictEntry {
    pub id: String,
    pub label: String,
    pub description: String,
    /// 当前生效的开关：用户覆盖 ?? 方案 `default_enabled` ?? 关。
    pub enabled: bool,
    /// 方案文件写的分组（`group`）；发现目录登记的库没有。
    pub group: Option<String>,
    /// 由 `[[dictionary_dirs]]` 自动登记（`dir:` 前缀，见 `EngineManager::inject_dir_dicts`），
    /// 不是方案文件手写的。
    pub from_dir: bool,
    /// 词库文件，相对 `schemas/`。
    pub path: String,
}

/// 列出 `schema_id` 的附加词库，按方案文件顺序（发现目录登记的排在声明的之后）。
///
/// 不含主库（`default = true`，关不掉）、用户分库与无 id 的项（开关按 id 落盘，无 id 的
/// 项翻了也存不住）。
pub fn list<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
) -> anyhow::Result<Vec<SchemaDictEntry>> {
    let merged = h
        .engine_mgr()
        .schema_merged(schema_id)
        .ok_or_else(|| anyhow::anyhow!("方案不存在: {schema_id}"))?;
    Ok(merged
        .dictionaries
        .iter()
        .filter(|d| !d.default && !d.id.is_empty() && !d.id.starts_with(ID_PREFIX))
        .map(|d| SchemaDictEntry {
            id: d.id.clone(),
            label: d.label.clone(),
            description: d.description.clone(),
            // `is_enabled` 对非主库即「enabled ?? default_enabled ?? false」，与引擎加载判据同一份
            enabled: d.is_enabled(),
            group: d.group.clone(),
            from_dir: d.id.starts_with(DIR_PREFIX),
            path: d.path.clone(),
        })
        .collect())
}

/// 发现目录登记的库的 id 前缀（`EngineManager::inject_dir_dicts` 生成）。
const DIR_PREFIX: &str = "dir:";

/// `sysDict.categories`：设置端词库窗「系统词库」一级表。含主词库、不含用户分库，按实际
/// 叠层先后排（`EngineManager::ordered_system_dicts`：拼音按方案文件顺序，码表 / 英文按
/// `base_order`），每项带 `order`（1 起）。
///
/// - `source`：`"main"` 主词库 / `"bundled"` 方案文件登记 / `"dir"` 发现目录登记。
/// - `togglable`：主库与写了 `default = true` 的库关不掉（`is_enabled` 恒真）。
/// - `count` 读词库文件计数（`EngineManager::dict_file_entries`，与加载器同口径：`rime_pinyin`
///   库加上一层 `import_tables` 子表，码表 / 英文只算本文件），停用的库也有；主文件不在或
///   `columns:` 声明残缺给 null。
/// - `label` 没写时取文件名（去 `.dict.yaml`）。
///
/// 非主库没有 id 的项不列（开关按 id 落盘，查看也按 id 寻址）。
pub fn categories<H: WebDataHost + ?Sized>(h: &H, schema_id: &str) -> anyhow::Result<Value> {
    let mgr = h.engine_mgr();
    let merged = mgr
        .schema_merged(schema_id)
        .ok_or_else(|| anyhow::anyhow!("方案不存在: {schema_id}"))?;
    let rows = EngineManager::ordered_system_dicts(&merged)
        .into_iter()
        .filter(|(d, main)| *main || !d.id.is_empty())
        .enumerate()
        .map(|(i, (d, main))| {
            json!({
                "id": view_id(d),
                "label": display_label(d),
                "description": d.description,
                "group": d.group,
                "enabled": d.is_enabled(),
                "source": if main { "main" } else if d.id.starts_with(DIR_PREFIX) { "dir" } else { "bundled" },
                "count": mgr.dict_file_entries(&d.path, d.effective_type()),
                "order": i + 1,
                "togglable": !main && !d.default,
            })
        })
        .collect();
    Ok(Value::Array(rows))
}

/// 查看用的库 id：方案写了 id 用 id；主库没写 id 时退到它的 `path`。
fn view_id(d: &DictSpec) -> String {
    if d.id.is_empty() {
        d.path.clone()
    } else {
        d.id.clone()
    }
}

/// 显示名：`label` → 文件名（去 `.dict.yaml`）→ id。
fn display_label(d: &DictSpec) -> String {
    if !d.label.is_empty() {
        return d.label.clone();
    }
    let file = d.path.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = file.strip_suffix(".dict.yaml").unwrap_or(file);
    if stem.is_empty() {
        d.id.clone()
    } else {
        stem.to_string()
    }
}

/// `sysDict.header` / `sysDict.entries` 的寻址：`dict_id` 须是 [`categories`] 列出的某一项。
fn find_system_dict<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    dict_id: &str,
) -> anyhow::Result<(DictSpec, bool)> {
    let merged = h
        .engine_mgr()
        .schema_merged(schema_id)
        .ok_or_else(|| anyhow::anyhow!("方案不存在: {schema_id}"))?;
    EngineManager::ordered_system_dicts(&merged)
        .into_iter()
        .find(|(d, main)| (*main || !d.id.is_empty()) && view_id(d) == dict_id)
        .map(|(d, main)| (d.clone(), main))
        .ok_or_else(|| anyhow::anyhow!("方案 {schema_id} 无系统词库 {dict_id}"))
}

/// `sysDict.header`：一本系统词库的头部信息卡片。字段见 `wind_engine::manager::DictHeader`，
/// 另带 `id` / `label` / `source`（同 [`categories`]）。
pub fn header<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    dict_id: &str,
) -> anyhow::Result<Value> {
    let (d, main) = find_system_dict(h, schema_id, dict_id)?;
    let head = h.engine_mgr().dict_header(&d.path, d.effective_type())?;
    let mut v = serde_json::to_value(head)?;
    if let Value::Object(o) = &mut v {
        o.insert("id".into(), json!(view_id(&d)));
        o.insert("label".into(), json!(display_label(&d)));
        let source = if main {
            "main"
        } else if d.id.starts_with(DIR_PREFIX) {
            "dir"
        } else {
            "bundled"
        };
        o.insert("source".into(), json!(source));
    }
    Ok(v)
}

/// `sysDict.entries`：只读词条分页，`{total, rows: [{code, text, weight, table}]}`。
/// `query` 空 = 不过滤；`mode` 缺省（`None`）= 编码前缀或词文本子串，`"code"` / `"text"` 只看
/// 一边。`sort` 为 `"code"` / `"text"` / `"weight"` 时按该列排（`order` 为 `"desc"` 降序，
/// 否则升序），缺省按文件顺序。读文本不持引擎锁，见 `wind_engine::manager::dict_view`。
#[allow(clippy::too_many_arguments)]
pub fn entries<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    dict_id: &str,
    query: &str,
    mode: Option<&str>,
    sort: Option<&str>,
    order: Option<&str>,
    offset: u64,
    limit: usize,
) -> anyhow::Result<Value> {
    let mode = match mode {
        None => EntryMatch::Any,
        Some("code") => EntryMatch::Code,
        Some("text") => EntryMatch::Text,
        Some(other) => anyhow::bail!("未知搜索方式: {other}"),
    };
    let sort = match sort {
        None => None,
        Some("code") => Some(EntrySort::Code),
        Some("text") => Some(EntrySort::Text),
        Some("weight") => Some(EntrySort::Weight),
        Some(other) => anyhow::bail!("未知排序列: {other}"),
    };
    let desc = match order {
        None | Some("asc") => false,
        Some("desc") => true,
        Some(other) => anyhow::bail!("未知排序方向: {other}"),
    };
    let q = EntryQuery {
        query: query.to_string(),
        mode,
        sort,
        desc,
    };
    let (d, _) = find_system_dict(h, schema_id, dict_id)?;
    let page = h
        .engine_mgr()
        .dict_entries(&d.path, d.effective_type(), &q, offset, limit)?;
    Ok(serde_json::to_value(page)?)
}

/// 设 `schema_id` 的附加词库 `dict_id` 启用与否：持久化后对已加载引擎即时生效。
///
/// 返回是否已对运行中的引擎生效（见 `EngineManager::set_dict_enabled_live`，目前恒 true）。
/// 未知方案 / 方案里没有这个库 → Err。
///
/// 启用一个文件缺失的库不是错误：开关照常落盘，重建时那一层加载失败只记 warning、主库
/// 照常出词（`load_codetable_layers` / `load_dictionary` 的扩展层都是「建不出来就少一层」）。
pub fn set_enabled<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    dict_id: &str,
    enabled: bool,
) -> anyhow::Result<bool> {
    let mut merged = h
        .engine_mgr()
        .schema_merged(schema_id)
        .ok_or_else(|| anyhow::anyhow!("方案不存在: {}", schema_id))?;
    let mut found = false;
    for d in merged.dictionaries.iter_mut() {
        if d.id == dict_id {
            d.enabled = Some(enabled);
            found = true;
        }
    }
    if !found {
        anyhow::bail!("方案 {} 无附加词库 {}", schema_id, dict_id);
    }
    // 用户分库的开关在 dicts.toml，不进 override
    if dict_id.starts_with(ID_PREFIX) {
        return set_user_dict_enabled(h, schema_id, dict_id, enabled);
    }
    let dicts_val = sparse_dict_overrides(&merged.dictionaries);
    let mut ov = h
        .engine_mgr()
        .get_schema_override(schema_id)
        .unwrap_or_else(|| toml::Value::Table(Default::default()));
    if !ov.is_table() {
        ov = toml::Value::Table(Default::default());
    }
    if let toml::Value::Table(t) = &mut ov {
        t.insert("dictionaries".to_string(), dicts_val);
    }
    // 持久化 override（不 invalidate），再对已加载引擎 live 翻该扩展层的 enabled 标志——
    // 扩展词库热插拔：无需重熔大词库即时生效；未加载方案下次构建按新 override 生效。
    h.engine_mgr().persist_schema_override(schema_id, &ov)?;
    Ok(h.engine_mgr()
        .set_dict_enabled_live(schema_id, dict_id, enabled))
}

/// `user:` 前缀的 id：写 `dicts.toml`，生效路径与扩展库相同。
fn set_user_dict_enabled<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    dict_id: &str,
    enabled: bool,
) -> anyhow::Result<bool> {
    if let Some(r) = h.storage_readonly_reason() {
        anyhow::bail!("{r}");
    }
    let name = dict_id.strip_prefix(ID_PREFIX).unwrap_or(dict_id);
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    let ds = h.engine_mgr().data_schema_id(schema_id);
    store.set_user_dict_enabled(&ds, schema_id, name, enabled)?;
    Ok(h.engine_mgr()
        .set_dict_enabled_live(schema_id, dict_id, enabled))
}

/// 构造 `dictionaries` 的 override 值：**每库只落 `{id, enabled}` 稀疏项**。
///
/// 词库的 path/label/base_order/顺序等结构定义始终以方案文件为准（合并侧见
/// `EngineManager::merge_dict_overrides`）。若在此写入完整数组，override 就会冻结整份
/// 词库定义——方案后续新增的库透不过来、改过的 path 仍指向旧文件、顺序也停在写快照那一刻。
///
/// 入参取合并后的 dictionaries：`enabled.is_some()` 即"该库有显式启用态"，逐条落盘以
/// 保留其它库已翻的开关。
fn sparse_dict_overrides(dicts: &[DictSpec]) -> toml::Value {
    toml::Value::Array(
        dicts
            .iter()
            .filter(|d| !d.id.is_empty() && !d.id.starts_with(ID_PREFIX))
            .filter_map(|d| {
                let mut t = toml::value::Table::new();
                t.insert("id".to_string(), toml::Value::String(d.id.clone()));
                t.insert("enabled".to_string(), toml::Value::Boolean(d.enabled?));
                Some(toml::Value::Table(t))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 落盘的 override 每库只含 `{id, enabled}`——绝不携带 path/label/base_order
    /// 等结构字段，否则 override 会冻结整份词库定义（方案升级后新增/改动的库透不过来）。
    #[test]
    fn sparse_dict_overrides_carries_only_id_and_enabled() {
        let dicts = vec![
            // 有显式启用态 → 落盘
            DictSpec {
                id: "ext1".into(),
                label: "分类词库".into(),
                path: "flypy/11_fl.dict.yaml".into(),
                base_order: 1,
                default_enabled: Some(true),
                enabled: Some(false),
                ..Default::default()
            },
            // 无显式启用态（用户没翻过）→ 不落盘，继承方案的 default_enabled
            DictSpec {
                id: "ext2".into(),
                path: "flypy/21_yj.dict.yaml".into(),
                default_enabled: Some(true),
                enabled: None,
                ..Default::default()
            },
            // 无 id 无法按 id 匹配回方案文件 → 丢弃
            DictSpec {
                id: String::new(),
                path: "flypy/31_fh.dict.yaml".into(),
                enabled: Some(true),
                ..Default::default()
            },
        ];

        let out = sparse_dict_overrides(&dicts);
        let arr = out.as_array().expect("应为数组");
        assert_eq!(arr.len(), 1, "只有 ext1 有显式启用态且带 id");

        let t = arr[0].as_table().unwrap();
        assert_eq!(t.get("id").unwrap().as_str(), Some("ext1"));
        assert_eq!(t.get("enabled").unwrap().as_bool(), Some(false));
        assert_eq!(
            t.len(),
            2,
            "除 id/enabled 外不得携带任何结构字段，实际: {:?}",
            t.keys().collect::<Vec<_>>()
        );
    }
}
