//! 传输无关的 JSON-RPC 分发：system.* / config.*（方法名与 web 前端 contract.ts 一致），
//! 未知/数据类方法转发到注入的 [`CoreRpc`]。
//!
//! 从 wind-webapi/rpc.rs 迁移，去掉 axum/WebState 依赖：改用 [`DispatchState`]
//! 持有 capabilities 缓存 + variant + 注入的 core 实现，返回 wind-ipc 的 [`Response`]。

use std::sync::Arc;

use serde_json::{Value, json};
use wind_config::Config;
use wind_ipc::rpc::{Request, Response};

// 产品版本取 build.rs 从 docs/VERSION 注入的 WIND_APP_VERSION，而非 workspace 的
// CARGO_PKG_VERSION。上报进 system_info.version / engine / appVersion。
pub(crate) const APP_VERSION: &str = env!("WIND_APP_VERSION");

/// 构建标识（git 短哈希[-dirty]），由宿主启动时注入一次，`system.info.build` 上报。
///
/// 由宿主注入而非本 crate 的 build.rs 自取：lib 增量构建时 build.rs 不随 HEAD 重跑，哈希会
/// 滞后；service 的 build.rs 每次强制重跑，且与主题预览缓存键同用 `WIND_GIT_HASH`。
static BUILD_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// 注入构建标识（仅首次生效）。未注入时 `system.info.build` 为 `null`。
pub fn set_build_id(id: impl Into<String>) {
    let _ = BUILD_ID.set(id.into());
}

/// 由宿主（service）注入的运行时状态来源（传输无关）。
///
/// 取代原 wind-webapi 的 `CoreStatus`：去掉浏览器授权相关（token/open_url），
/// 仅保留 dispatch 所需的状态查询 + 数据类 RPC 转发 + 字体枚举。
pub trait CoreRpc: Send + Sync {
    fn is_chinese_mode(&self) -> bool;
    fn active_schema_id(&self) -> String;
    /// config.setItems 落盘后重新加载并即时应用用户配置；返回是否仍需重启才能完全生效。
    /// 默认实现保守返回 true（未接入热重载的宿主，如测试 stub）。
    fn apply_config(&self) -> bool {
        true
    }
    /// 数据类 RPC（schema/dict/temp/freq/shadow/stats/theme/phrase）转发到宿主 core 实现。
    /// 默认未接入（测试 stub）：返回 unknown method 错误。
    fn data_rpc(&self, method: &str, _params: &Value) -> anyhow::Result<Value> {
        anyhow::bail!("unknown method: {}", method)
    }
    /// 本机字体枚举（system.fonts）：(family, display_name)。默认空表（无平台字体能力）。
    fn fonts(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// 分发器共享状态：能力清单缓存 + 变体 + 注入的 core 实现。
pub struct DispatchState {
    pub(crate) core: Arc<dyn CoreRpc>,
    pub(crate) variant: &'static str,
    pub(crate) capabilities: Value,
    /// 配置变更事件接收方（setItems/reload 后广播）。
    pub(crate) events: crate::events::EventSink,
}

impl DispatchState {
    pub fn new(core: Arc<dyn CoreRpc>, variant: &'static str) -> anyhow::Result<Self> {
        Self::with_events(core, variant, crate::events::EventSink::disconnected())
    }

    /// 构造并接入事件广播通道（config/dict 变更经此推送）。
    pub fn with_events(
        core: Arc<dyn CoreRpc>,
        variant: &'static str,
        events: crate::events::EventSink,
    ) -> anyhow::Result<Self> {
        let capabilities = crate::capabilities::generate(Config::data_dir().as_deref())?;
        Ok(Self {
            core,
            variant,
            capabilities,
            events,
        })
    }
}

/// 分发一条请求，返回 JSON-RPC 响应（成功/错误均为 200 等价的 Response）。
///
/// 错误用 `{e:#}` 展开整条 anyhow 链，不用 `to_string()`：这是 core 向设置端与 CLI 传递
/// 失败原因的唯一出口，`to_string()` 只给最外层 context，底下能指导用户的原因会被抹掉。
/// 代价是带 `source()` 的外部错误（如 `ZipError::Io`）会重复一句，不影响判读。
pub fn dispatch(state: &DispatchState, req: Request) -> Response {
    dispatch_as(state, req, true)
}

/// 拒绝文案：不可信调用方（AppContainer / 低完整性 / 他人 uid）碰机密相关方法时返回。
pub const DENY_UNTRUSTED: &str = "拒绝：调用方权限不足";

/// 需要「可信调用方」才能调用的方法：同步（含导出密钥、改后端）。
fn requires_trusted(method: &str) -> bool {
    method.starts_with("sync.")
}

/// 带调用方可信度的分发。`trusted=false` 来自传输层对连接对端的核验（见各 transport）；
/// 进程内调用（同步进程的受限请求通道）本身可信，走 [`dispatch`]。
pub fn dispatch_as(state: &DispatchState, req: Request, trusted: bool) -> Response {
    if !trusted && requires_trusted(&req.method) {
        return Response::error(req.id, DENY_UNTRUSTED.to_string());
    }
    match handle(state, &req.method, &req.params) {
        Ok(v) => Response::success(req.id, v),
        Err(e) => Response::error(req.id, format!("{e:#}")),
    }
}

fn handle(state: &DispatchState, method: &str, params: &Value) -> anyhow::Result<Value> {
    match method {
        "system.status" => Ok(json!({
            "running": true,
            "mode": if state.core.is_chinese_mode() { "chinese" } else { "english" },
        })),
        // 字段对齐 web 的 SystemInfo {version, platform, dataDir, running}；其余为附带字段（web 忽略）。
        "system.info" => Ok(json!({
            "version": APP_VERSION,
            "platform": platform_name(),
            "dataDir": Config::data_dir().map(|p| p.display().to_string()).unwrap_or_default(),
            "running": true,
            "engine": APP_VERSION,
            // 构建标识（git 短哈希[-dirty]），与主题预览构建标识同源；未注入为 null。
            "build": BUILD_ID.get(),
            "variant": state.variant,
            "activeSchema": state.core.active_schema_id(),
            // 定制版身份（`data_custom/custom.toml` 的 `[custom]`），非定制版为 `null`。
            // 放这里而不放 `system.capabilities`：同属「本机装的是什么」，capabilities 是
            // 构造时缓存的静态配置键清单；设置端启动本就拉 `system.info`，零额外往返。
            "customEdition": crate::custom_edition::identity_json(),
        })),
        "system.capabilities" => Ok(state.capabilities.clone()),
        // 段级降级记录：哪些段解析失败、已回落出厂值。
        //
        // 语义是「此刻盘上的配置试加载一遍会不会降级」（每次现读四层），不是运行中那份
        // 配置的快照，别拿它当后者用。形态恒为固定字段的对象。为何独立成方法见
        // `docs/design/data-custom-layer.md`「连带闸门」后的说明。
        "config.degradation" => {
            let d = Config::load(Config::data_dir().as_deref())?.degradation;
            Ok(json!({
                "degraded": d.is_degraded(),
                // 点分路径原样传出（`ui.font` 而不是 `ui`），别截成顶层段名丢掉精度。
                "sections": d.sections,
                "totalFallback": d.total_fallback,
                // 文件语法不合法（重复键、漏引号……）：发生在合并之前的单文件解析，与合并后
                // 类型检查的 `sections` 修法不同，设置端分开讲。
                "unparsable": d.unparsable.iter().map(|u| json!({
                    "layer": u.layer,
                    "path": u.path.display().to_string(),
                    "error": u.error,
                    // 1-based，就是用户在编辑器里看到的行号。
                    "skippedLines": u.skipped_lines,
                    "salvagedKeys": u.salvaged_keys,
                    // 编码不对（非 UTF-8）：设置端据此改讲「另存为 UTF-8」，而不是「修语法」。
                    "notUtf8": u.not_utf8,
                })).collect::<Vec<_>>(),
            }))
        }
        // 本机字体枚举（平台能力经 CoreRpc 注入；默认空表）。
        "system.fonts" => Ok(Value::Array(
            state
                .core
                .fonts()
                .into_iter()
                .map(|(family, display_name)| json!({ "family": family, "display_name": display_name }))
                .collect(),
        )),
        "system.notifyReload" => Ok(json!({ "ok": true })),
        // 返回整份配置（四层合并后），不按键取值。
        //
        // 多余的 `key` 参数当场拒掉而不是无视：无视会让读回执 `value` 的调用方静默拿到
        // None。取单个键用 [`get_item`]（`config.getItem`）。命令栏里同名的 `config.get(key)`
        // 函数是按 key 取值的（`wind-cmdbar::funcs::config`），同名不同源，易混。
        "config.get" => {
            if params.get("key").is_some() {
                anyhow::bail!(
                    "config.get 不接受 key 参数（它返回整份配置）；取单个已登记键请用 \
                     config.getItem {{\"key\": ...}}，它的回执正是 {{key, value}}"
                );
            }
            let cfg = Config::load(Config::data_dir().as_deref())?;
            Ok(cfg.to_json_value()?)
        }
        "config.getDefaults" => {
            // 出厂默认 = 系统预置（代码默认 L1 ⊕ data/config.toml L2 ⊕
            // data_custom/config.toml L2.5），与 capability 的 default 同源
            // （system_preset_value）。
            //
            // 定制版上「恢复默认」因此落到定制者设的值，这是预期语义。
            //
            // 不可用 toml::from_str("") 的纯 L1：顶码上屏/拼音自动学习等键出厂经 L2 置开、
            // L1 为关，用 L1 会让「恢复默认」把这些项误关。
            let v = Config::system_preset_value(Config::data_dir().as_deref())?;
            Ok(serde_json::to_value(v)?)
        }
        "config.setItems" => set_items(state, params),
        // 配置片段（TOML 文本）逐键预览：键/当前值/新值/错误，只读不落盘。
        "config.previewPatch" => preview_patch(params),
        // 配置片段应用：与 previewPatch 同一套校验，任何一条有错即整体拒绝（不做半应用）。
        "config.applyPatch" => apply_patch(state, params),
        // 配置字段注册表（key+type+enum options）：CLI/设置端据此校验与补全。
        "config.schema" => Ok(schema_json()),
        // 单字段当前值（含四层合并）：补 config.get 只能整份的缺口。
        "config.getItem" => get_item(params),
        "config.reload" => {
            // 手改 `config.toml` 或被别的工具改过之后，请求核心重读并应用（core 不监听文件）；
            // 随后广播变更事件，供订阅者（TSF/UI）刷新。
            let needs_restart = state.core.apply_config();
            state.events.emit_config_changed(
                json!({ "reason": "reload", "needsRestart": needs_restart }),
            );
            Ok(json!({ "ok": true, "needsRestart": needs_restart }))
        }
        // compat.*：转发到宿主；写方法成功后广播 compat.changed（右键菜单的写入不经这里，
        // 不广播，设置页靠重获焦点时重新拉取兜底）。
        m if m.starts_with("compat.") => {
            let out = state.core.data_rpc(m, params)?;
            if compat_method_writes(m, params) {
                state.events.emit_compat_changed(json!({ "method": m }));
            }
            Ok(out)
        }
        // schema/dict/temp/freq/shadow/stats/theme/phrase 等数据类 RPC 转发到宿主 core。
        // 改方案配置的写方法成功后广播 schema.configChanged，开着的设置页据此重拉该方案。
        _ => {
            let out = state.core.data_rpc(method, params)?;
            if let Some(id) = schema_config_write_id(method, params) {
                let data = match id {
                    Some(id) => json!({ "id": id, "method": method }),
                    None => json!({ "method": method }),
                };
                state.events.emit_schema_config_changed(data);
            }
            Ok(out)
        }
    }
}

/// 会改 `schema.getConfig` 结果的写方法：`Some(Some(id))` 改的是这一个方案，
/// `Some(None)` 可能动了多个方案（导入方案包、整机还原），载荷不带 id，订阅方按「全部」处理。
///
/// 方案配置只经 RPC 写（设置页、`wind_input schema …` 命令都走这里），故在 dispatch 广播即
/// 覆盖全部写入点。扩展词库开关有两处落点（`schema_overrides/<id>.toml` 与用户分库的
/// `dicts.toml`），按方法名判比在落盘处各挂一个钩子省事，也不会漏掉后者。
fn schema_config_write_id<'a>(method: &str, params: &'a Value) -> Option<Option<&'a str>> {
    let key = match method {
        "schema.saveConfig" | "schema.resetConfig" | "schema.setDictEnabled" | "schema.delete" => {
            "id"
        }
        "userDicts.setEnabled" | "userdata.purgeSchema" => "schemaId",
        "scheme.importPackage" | "scheme.importText" | "backup.restore" | "schema.reloadFiles" => {
            return Some(None);
        }
        _ => return None,
    };
    Some(params.get(key).and_then(Value::as_str))
}

/// `compat.*` 里哪些会改用户层文件：只读方法与 dryRun 导入不广播。
fn compat_method_writes(method: &str, params: &Value) -> bool {
    match method {
        "compat.list" | "compat.schema" | "compat.export" => false,
        "compat.import" => !params
            .get("dryRun")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => true,
    }
}

/// 平台名，对齐 web 约定（"windows" | "darwin" | "linux"）。
fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// 惰性取本次的降级记录（首次调用才真去 `Config::load()`）。
///
/// 只有遇到整值覆盖型键才付这份代价。
fn degradation_for_write_back(
    slot: &mut Option<wind_config::config::ConfigDegradation>,
) -> anyhow::Result<&wind_config::config::ConfigDegradation> {
    if slot.is_none() {
        *slot = Some(Config::load(Config::data_dir().as_deref())?.degradation);
    }
    Ok(slot.as_ref().expect("just filled"))
}

fn set_items(state: &DispatchState, params: &Value) -> anyhow::Result<Value> {
    let items = params
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("invalid_params: items missing"))?;
    // 第一遍：解析 + 按注册表校验。合法项收集待写；未知键/类型/枚举错的项跳过并记录，
    // 不让整批因一个旧字段失败（保护沿用旧字段的 webview）。malformed item（无 key）仍为硬错误。
    let mut writes: Vec<(String, toml::Value)> = Vec::with_capacity(items.len());
    let mut skipped: Vec<Value> = Vec::new();
    // 降级记录按需加载：绝大多数 setItems 没有整值覆盖型键，不该为此多读一遍四层文件。
    // `None` = 还没问过。
    let mut degradation: Option<wind_config::config::ConfigDegradation> = None;
    for it in items {
        let key = it
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("invalid_params: item.key missing"))?;
        let value = it.get("value").cloned().unwrap_or(Value::Null);
        let toml_val = match json_to_toml(&value) {
            Ok(v) => v,
            Err(e) => {
                skipped.push(json!({ "key": key, "reason": e.to_string() }));
                continue;
            }
        };
        if let Err(e) = wind_config::config_schema::validate(key, &toml_val) {
            skipped.push(json!({ "key": key, "reason": e.to_string() }));
            continue;
        }
        // 降级闸：只拦整值覆盖型键（Map 与数组，判据在 `FieldType::is_whole_value_leaf`，
        // 别在这里另写 matches）。设置端发的是「base ⊕ 本次编辑」的整份值，降级时 base 是
        // 出厂值，写回会抹掉用户原有的整张表。标量键不拦，否则降级期间整页无法保存。
        // 见 `docs/design/data-custom-layer.md`「连带闸门」。
        if wind_config::config_schema::field(key).is_some_and(|f| f.ty.is_whole_value_leaf())
            && degradation_for_write_back(&mut degradation)?.taints(key)
        {
            skipped.push(json!({
                "key": key,
                "reason": "本次配置加载中该键所在段解析失败并回落了出厂默认值，\
                           整份写回会抹掉已有内容，故跳过；请先修好报错的配置键再保存。",
            }));
            continue;
        }
        writes.push((key.to_string(), toml_val));
    }
    let applied = writes.len();
    if !skipped.is_empty() {
        tracing::warn!(
            "config.setItems 跳过 {} 个无效项（未登记/类型/枚举）: {:?}",
            skipped.len(),
            skipped
        );
    }
    // 第二遍：落盘合法项（IO 失败仍为硬错误）+ 热重载 + 事件广播。
    let needs_restart = apply_writes(state, writes, "setItems")?;
    Ok(json!({ "needsRestart": needs_restart, "applied": applied, "skipped": skipped }))
}

/// setItems / applyPatch 共用的落盘通路：逐键 `set_user_value`（继承「等出厂默认即删」
/// 的 prune 收口），随后即时热重载（轻量字段立即生效，引擎结构性变更则 needsRestart=true）
/// 并广播配置变更事件。返回 needsRestart。
fn apply_writes(
    state: &DispatchState,
    writes: Vec<(String, toml::Value)>,
    reason: &str,
) -> anyhow::Result<bool> {
    for (key, toml_val) in writes {
        let parts: Vec<&str> = key.split('.').collect();
        Config::set_user_value(&parts, toml_val)?;
    }
    let needs_restart = state.core.apply_config();
    state
        .events
        .emit_config_changed(json!({ "reason": reason, "needsRestart": needs_restart }));
    Ok(needs_restart)
}

/// 解析 + 展平 + 校验配置片段（previewPatch / applyPatch 共用）。
/// TOML 解析失败是整体错误；当前值与 `config.get` 同源（四层合并后的生效配置）。
///
/// 一并返回当前配置树：applyPatch 折算 Map 键的落盘整表要用它作合并种子，
/// 重新加载一次会引入「预览用 A 树、落盘用 B 树」的窗口。
///
/// 说明元信息（保留段 `[package]`）在此一并提取，非法即整体 Err（与 TOML 解析失败同级）。
/// 预览与应用共走本函数，判据只有一套。
fn patch_entries(
    params: &Value,
) -> anyhow::Result<(
    Vec<wind_config::patch::PatchEntry>,
    toml::Value,
    Option<wind_config::patch::PatchInfo>,
)> {
    let text = params
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("invalid_params: text missing"))?;
    let fragment = wind_config::patch::parse_fragment(text)
        .map_err(|e| anyhow::anyhow!("invalid_patch: {e}"))?;
    let info = wind_config::patch::extract_info(&fragment)
        .map_err(|e| anyhow::anyhow!("invalid_patch: {e}"))?;
    let cfg = Config::load(Config::data_dir().as_deref())?;
    // 降级记录必须在序列化成 `toml::Value` 之前留下来：`degradation` 是 `#[serde(skip)]`。
    let degradation = cfg.degradation.clone();
    let current = cfg.to_toml_value()?;
    let mut entries = wind_config::patch::preview(&fragment, &current);
    // 降级闸，preview 与 apply 共用（见 `mark_degraded_seeds`）：坏段的生效表是出厂值，
    // 当 Map 合并种子会抹掉用户条目。标成条目级 error 后 applyPatch 自然整体拒绝。
    wind_config::patch::mark_degraded_seeds(&mut entries, &degradation);
    Ok((entries, current, info))
}

/// `config.previewPatch { text }` →
/// `{ ok, entries: [{ key, mapEntry?, current?, next, error?, warning? }], info? }`，只读。
///
/// `info` = 片段 `[package]` 段的说明元信息，供导入对话框在预览列表上方显示；
/// 两字段都缺省时整个 `info` 不出现（前端不必区分「没写」与「写了空串」）。
///
/// `warning` = 该条可以应用但导入界面须显著提示（如会往工具栏放能启动程序的按钮）。
/// 不影响 `ok`：`ok` 只回答「全都合法吗」，是否因提示而不导入由用户决定。
fn preview_patch(params: &Value) -> anyhow::Result<Value> {
    let (entries, _, info) = patch_entries(params)?;
    let ok = entries.iter().all(|e| e.error.is_none());
    let mut out = json!({ "ok": ok, "entries": entries });
    if let Some(info) = info {
        out["info"] = serde_json::to_value(info)?;
    }
    Ok(out)
}

/// `config.applyPatch { text }`：先跑与 preview 相同的校验，任何一条有错 → 整体 Err、
/// 不做半应用；全部合法 → 走 setItems 的批量落盘通路（继承 prune 与生效通知）。
/// 0 条目视为成功 no-op（不落盘、不触发热重载）。
///
/// `written` = 落盘后的最终键值，Map 父键携带合并后的整表，供设置端回灌配置镜像（客户端
/// 没有 core 的当前表，拼不出整表）。`applied` 计片段条目数（Map 逐条目各计一条），与
/// preview 的 entries 条数对得上；落盘键数因 Map 合并而更少，两者刻意分开报。
///
/// 响应不回带 `info`（`[package]` 只给导入界面看，校验在 [`patch_entries`]）。
fn apply_patch(state: &DispatchState, params: &Value) -> anyhow::Result<Value> {
    let (entries, current, _) = patch_entries(params)?;
    let bad: Vec<String> = entries
        .iter()
        .filter_map(|e| e.error.as_ref().map(|err| format!("{}: {}", e.key, err)))
        .collect();
    if !bad.is_empty() {
        anyhow::bail!("invalid_patch: {}", bad.join("; "));
    }
    if entries.is_empty() {
        return Ok(json!({ "ok": true, "applied": 0, "needsRestart": false, "written": [] }));
    }
    let applied = entries.len();
    let writes = wind_config::patch::writes(&entries, &current);
    let written: Vec<Value> = writes
        .iter()
        .map(|(key, value)| -> anyhow::Result<Value> {
            Ok(json!({ "key": key, "value": serde_json::to_value(value)? }))
        })
        .collect::<anyhow::Result<_>>()?;
    let needs_restart = apply_writes(state, writes, "applyPatch")?;
    Ok(json!({
        "ok": true,
        "applied": applied,
        "needsRestart": needs_restart,
        "written": written,
    }))
}

/// 把 config_schema 注册表序列化为 JSON（`{ fields: [{key, type, options?}] }`）。
/// 供 `config.schema` RPC；CLI/设置端据此列出、补全、校验。
fn schema_json() -> Value {
    use wind_config::config_schema::{FieldType, registry};
    let fields: Vec<Value> = registry()
        .iter()
        .map(|f| {
            let (ty, options): (&str, Option<&[&str]>) = match f.ty {
                FieldType::Bool => ("bool", None),
                FieldType::Int => ("int", None),
                FieldType::Float => ("float", None),
                FieldType::Str => ("string", None),
                // 同 capabilities::type_name：分档枚举对外仍是 enum。
                FieldType::Enum(vs) | FieldType::LayoutEnum(vs) => ("enum", Some(vs)),
                FieldType::StrList => ("string[]", None),
                FieldType::Map(_) => ("map", None),
                FieldType::StructList => ("array", None),
            };
            let mut obj = json!({ "key": f.key, "type": ty });
            if let Some(vs) = options {
                obj["options"] = json!(vs);
            }
            obj
        })
        .collect();
    json!({ "fields": fields })
}

/// `config.getItem`：返回单个已登记键的当前值（四层合并后）。
fn get_item(params: &Value) -> anyhow::Result<Value> {
    let key = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("invalid_params: key missing"))?;
    if !wind_config::config_schema::is_known_key(key) {
        anyhow::bail!("invalid_config: 键 '{}' 未登记", key);
    }
    let cfg = Config::load(Config::data_dir().as_deref())?;
    let full = cfg.to_json_value()?;
    let mut cur = &full;
    for part in key.split('.') {
        cur = cur
            .get(part)
            .ok_or_else(|| anyhow::anyhow!("config 缺少键 {}", key))?;
    }
    Ok(json!({ "key": key, "value": cur.clone() }))
}

/// JSON 标量/容器 → toml::Value（用于写用户层配置）。
fn json_to_toml(v: &Value) -> anyhow::Result<toml::Value> {
    Ok(match v {
        Value::Null => anyhow::bail!("不支持 null 配置值"),
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                anyhow::bail!("不支持的数字 {}", n)
            }
        }
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for e in a {
                out.push(json_to_toml(e)?);
            }
            toml::Value::Array(out)
        }
        Value::Object(o) => {
            let mut t = toml::map::Map::new();
            for (k, val) in o {
                t.insert(k.clone(), json_to_toml(val)?);
            }
            toml::Value::Table(t)
        }
    })
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
