//! 传输无关的 JSON-RPC 分发：system.* / config.*（方法名与 web 前端 contract.ts 一致），
//! 未知/数据类方法转发到注入的 [`CoreRpc`]。
//!
//! 从 wind-webapi/rpc.rs 迁移，去掉 axum/WebState 依赖：改用 [`DispatchState`]
//! 持有 capabilities 缓存 + variant + 注入的 core 实现，返回 wind-ipc 的 [`Response`]。

use std::sync::Arc;

use serde_json::{Value, json};
use wind_config::Config;
use wind_ipc::rpc::{Request, Response};

// 产品版本取 build.rs 从 docs/VERSION 注入的 WIND_APP_VERSION（= 0.100.0），
// 而非 workspace 的 CARGO_PKG_VERSION（兜底 0.x）。上报进 system_info.version / engine / appVersion。
pub(crate) const APP_VERSION: &str = env!("WIND_APP_VERSION");

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
pub fn dispatch(state: &DispatchState, req: Request) -> Response {
    match handle(state, &req.method, &req.params) {
        Ok(v) => Response::success(req.id, v),
        Err(e) => Response::error(req.id, e.to_string()),
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
            "variant": state.variant,
            "activeSchema": state.core.active_schema_id(),
            // 定制版身份（`data_custom/custom.toml` 的 `[custom]`），非定制版为 `null`。
            //
            // ★ 落点选 `system.info` 而不是 `system.capabilities`，三条理由：
            // ① 语义同类——这里已经是「我这台机器装的是什么」（version/variant/dataDir），
            //    定制版身份正是同一个问题的一部分，而 capabilities 回答的是「配置键有哪些」；
            // ② capabilities 在 `DispatchState::new` 时**生成一次并缓存**，身份混进去会让
            //    「静态能力清单」与「本机装了什么」共用一个缓存，日后任一方要刷新都会牵连另一方；
            // ③ 设置端启动时本就并发拉 `system.info`（wind-setting `state.rs` 的 `h_info`），
            //    加字段是零额外往返，而新开一个 RPC 方法要在那个扇出里再加一条线程。
            //
            // 代价（不留白）：CLI 若只想问身份，也得拉一份完整 system.info。那份很小、
            // 且不做磁盘 IO，代价可忽略——这正是不把**降级**也塞进来的原因，见 `config.degradation`。
            "customEdition": crate::custom_edition::identity_json(),
        })),
        "system.capabilities" => Ok(state.capabilities.clone()),
        // 段级降级记录：哪些段解析失败、已回落出厂值。
        //
        // ★ 语义是「**此刻盘上**的配置试加载一遍会不会降级」，**不是**「正在跑的这份
        // 配置降没降级」。每次调用现读四层文件、现算一次，故用户把报错的键改好之后，
        // 下一次调用就返回 `degraded=false`——横幅能自己消失，不必等重启，这正是选它的
        // 理由。反过来说，**别拿它当运行时快照用**：服务当前生效的那份 `Config` 是启动
        // 时加载的，与这里的答案可以不同（用户刚把配置改坏、还没重载时最明显）。要那种
        // 语义得让协调器把自己那份 `degradation` 报上来，是另一件事。
        //
        // 不变量 6（`docs/design/data-custom-layer.md` §4）要求降级「必须 WARN 且在 UI 可见」。
        // 日志那一半 P0 已做，这里补上「可见」——没有它，用户看到的只是「我的按键设置怎么
        // 变回默认了」，而唯一的线索埋在日志文件里。
        //
        // ★ 为什么独立成方法，而不是并进 `system.info` 或 `config.get`：
        // - 并进 `system.info`：那个方法当前**不碰磁盘**，而降级记录只能由一次真实的
        //   `Config::load()`（读四层 TOML）产生。让一个轻量状态查询变成磁盘 IO，
        //   代价会落在每一个只想知道版本号的调用方头上。
        // - 并进 `config.get`：它返回的是 `Config` 本身的序列化结果，而 `degradation` 是
        //   `#[serde(skip)]` 的运行期元信息、**刻意**不出现在配置命名空间里（否则设置端
        //   diff 回传时它会被当成一个配置键写进 config.toml）。
        //
        // 形态恒为四个字段的对象（不是「没降级就不给」）：客户端据此可以无条件渲染，
        // 「字段缺失」在跨仓契约里与「这版 core 还没实现」无从区分。
        "config.degradation" => {
            let d = Config::load(Config::data_dir().as_deref())?.degradation;
            Ok(json!({
                "degraded": d.is_degraded(),
                // 点分路径**原样**传出（`ui.font` 而不是 `ui`）：降级粒度细到子表正是
                // 「缩小爆炸半径」的产物，在边界上截成顶层段名等于把这份精度扔掉，
                // 用户会以为整个界面设置都回了默认。
                "sections": d.sections,
                "totalFallback": d.total_fallback,
                // 文件**语法**不合法（重复键、漏引号……），与 `sections` 是两类故障：
                // 那个发生在四层合并**之后**的类型检查，这个发生在合并**之前**的单文件
                // 解析。设置端要分开讲——修法不同（改那一行 vs 改那个键的类型），
                // 混成一句会把用户支使到错误的地方。
                "unparsable": d.unparsable.iter().map(|u| json!({
                    "layer": u.layer,
                    "path": u.path.display().to_string(),
                    "error": u.error,
                    // 1-based，就是用户在编辑器里看到的行号。
                    "skippedLines": u.skipped_lines,
                    "salvagedKeys": u.salvaged_keys,
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
        "config.get" => {
            let cfg = Config::load(Config::data_dir().as_deref())?;
            Ok(serde_json::to_value(cfg)?)
        }
        "config.getDefaults" => {
            // 出厂默认 = 系统预置（代码默认 L1 ⊕ data/config.toml L2 ⊕
            // data_custom/config.toml L2.5），与 capability 的 default 同源
            // （system_preset_value）。
            //
            // ⚠️ 定制版上「出厂默认」因此是**定制默认值**，设置页的「恢复默认」会落到
            // 定制者设的值而不是原版值。这是正确语义（定制版用户要恢复的就是定制版的默认），
            // 不是 bug——见 `Config::system_preset_value` 的文档。
            //
            // 不可用 toml::from_str("") 的纯 L1：
            // 顶码上屏/拼音自动学习等键出厂经 L2 置开、L1 为关，二者分叉会让设置端
            // 「恢复默认」把这些项误关。
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
            // 变更通知：广播一个 config 变更事件，供订阅者（TSF/UI）刷新。
            state
                .events
                .emit_config_changed(json!({ "reason": "reload" }));
            Ok(json!({ "ok": true }))
        }
        // schema/dict/temp/freq/shadow/stats/theme/phrase 等数据类 RPC 转发到宿主 core。
        _ => state.core.data_rpc(method, params),
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
/// 单独抽出来是为了让「只有遇到 Map 键才付这份代价」这件事在调用点一眼可见，
/// 而不是散成一段 `if slot.is_none() { … }`。
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
    // 第一遍：解析 + 按注册表校验。合法项收集待写；未知键/类型/枚举错的项**跳过并记录**，
    // 不让整批因一个旧字段失败（保护沿用旧字段的 webview）。malformed item（无 key）仍为硬错误。
    let mut writes: Vec<(String, toml::Value)> = Vec::with_capacity(items.len());
    let mut skipped: Vec<Value> = Vec::new();
    // 降级记录**按需**加载：绝大多数 setItems 一个 Map 键都没有，不该为此多跑一次
    // `Config::load()`（它要读四层文件）。`None` = 还没问过。
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
        // ★ 降级闸（第四条同形状路径）。**只拦「整值覆盖」型键**，判据在
        // `FieldType::is_whole_value_leaf`：
        //
        // 设置端发来的是「base ⊕ 本次编辑」的**整份值**（wind-setting 的 `diff_config`
        // 把 map 作原子叶子整体发送；数组根本不是对象，走叶子分支同样整份发送），而
        // base 来自 `config.get` ⇒ 本次加载降级时 base 是出厂值 ⇒ 用户改一条自定义标点、
        // 或在工具栏里勾掉一格，就把他原有的整张表 / 整个数组抹掉，永久且无痕。
        // 这与 `applyPatch` 的 Map 合并种子是同一个失效模式，只是入口不同。
        //
        // ⚠️ 第一版这里只写了 `FieldType::Map(_)`，于是 `ui.toolbar.items`、
        // `ui.langbar.badges`、`ui.toolbar.buttons`、`schema.mix_modes` 这些数组型键全部
        // 漏在门外——它们与 Map 同为整值覆盖，失效形态一模一样。判据因此收进
        // `is_whole_value_leaf`，加新类型时去那里回答一次，别在这里重新展开 matches。
        //
        // 标量键不拦：它的落盘值是设置端发来的**显式单值**，与降级后的 base 无关，
        // 拦掉只会让降级期间整个设置页无法保存，代价远大于收益。
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
/// 说明元信息（保留段 `[package]`）在此一并提取：**非法即整体 Err**，与 TOML 解析失败
/// 同级，不是逐条 error。预览与应用共走本函数，判据只有一套——预览放行、应用才拒绝
/// （或反之）是分发者最难自查的一类不一致。
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
    // 降级记录必须在序列化成 `toml::Value` **之前**留下来：`degradation` 是 `#[serde(skip)]`，
    // 转成值树那一刻就没了，而下面的 Map 合并种子正是从这棵树上取的。
    let degradation = cfg.degradation.clone();
    let current = toml::Value::try_from(cfg)?;
    let mut entries = wind_config::patch::preview(&fragment, &current);
    // ★ 降级闸，preview 与 apply 共用这一处判据（见 `mark_degraded_seeds`）：坏段处的
    // 生效表是出厂值，拿它当 Map 合并种子会把用户已有条目整表抹掉。标成条目级 error 之后，
    // 预览直接显示原因、applyPatch 既有的「有错即整体拒绝」自动生效。
    wind_config::patch::mark_degraded_seeds(&mut entries, &degradation);
    Ok((entries, current, info))
}

/// `config.previewPatch { text }` →
/// `{ ok, entries: [{ key, mapEntry?, current?, next, error?, warning? }], info? }`，只读。
///
/// `info` = 片段 `[package]` 段的说明元信息，供导入对话框在预览列表上方显示；
/// 两字段都缺省时整个 `info` 不出现（前端不必区分「没写」与「写了空串」）。
///
/// `warning` = 该条**可以应用但导入界面须显著提示**（如会往工具栏放一个能启动程序的
/// 按钮）。与 `error` 是两回事，且**不影响 `ok`**——`ok` 只回答「全都合法吗」，
/// 要不要因为提示而不导入是用户的决定，不是校验结果。
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
/// `written` = **落盘后的最终键值**，Map 父键携带合并后的整表。设置端用它回灌配置镜像：
/// Map 合并后客户端无法从 entries 自行拼出整表（它手里没有 core 的当前表），必须由此回传。
/// `applied` 计的是**片段条目数**（Map 逐条目各计一条），与 preview 的 entries 条数对得上；
/// 落盘键数（`written.len()`）因 Map 合并而更少，两者刻意分开报。
///
/// 说明元信息（`[package]`）在 [`patch_entries`] 里与预览同判据地校验，非法即整体拒绝；
/// 响应本身**不**回带 `info`——它是给导入界面看的，落盘阶段没有消费者。
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
                FieldType::Enum(vs) => ("enum", Some(vs)),
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
    let full = serde_json::to_value(cfg)?;
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
mod tests {
    //! dispatch 单测：构造假 CoreRpc，发 system.info / config.get 等，断言 Response 形状。
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FakeCore {
        config_applied: AtomicBool,
    }
    impl FakeCore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                config_applied: AtomicBool::new(false),
            })
        }
    }
    impl CoreRpc for FakeCore {
        fn is_chinese_mode(&self) -> bool {
            true
        }
        fn active_schema_id(&self) -> String {
            "wubi86".to_string()
        }
        fn apply_config(&self) -> bool {
            self.config_applied.store(true, Ordering::SeqCst);
            false // needsRestart=false
        }
        fn data_rpc(&self, method: &str, _params: &Value) -> anyhow::Result<Value> {
            if method == "dict.stats" {
                Ok(json!([]))
            } else {
                anyhow::bail!("unknown method: {}", method)
            }
        }
        fn fonts(&self) -> Vec<(String, String)> {
            vec![("Sans".to_string(), "Sans".to_string())]
        }
    }

    fn state() -> DispatchState {
        DispatchState::new(FakeCore::new(), "dev").expect("capabilities 应能加载")
    }

    fn req(method: &str, params: Value) -> Request {
        Request {
            version: 1,
            id: 7,
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn system_info_shape() {
        let resp = dispatch(&state(), req("system.info", json!({})));
        assert_eq!(resp.id, 7);
        assert!(resp.error.is_none());
        let r = resp.result.unwrap();
        for k in ["version", "platform", "dataDir", "running"] {
            assert!(r.get(k).is_some(), "system.info 缺字段 {k}");
        }
        assert_eq!(r["variant"], json!("dev"));
        assert_eq!(r["activeSchema"], json!("wubi86"));
    }

    #[test]
    fn system_status_shape() {
        let resp = dispatch(&state(), req("system.status", json!({})));
        let r = resp.result.unwrap();
        assert_eq!(r["running"], json!(true));
        assert_eq!(r["mode"], json!("chinese"));
    }

    #[test]
    fn config_get_defaults_is_object() {
        let resp = dispatch(&state(), req("config.getDefaults", json!({})));
        let r = resp.result.unwrap();
        assert!(r.is_object());
        assert!(r["input"].is_object(), "默认配置应含 input 段");
    }

    #[test]
    fn capabilities_shape() {
        let resp = dispatch(&state(), req("system.capabilities", json!({})));
        let r = resp.result.expect("system.capabilities 应成功");
        assert!(
            r.get("configKeys").and_then(|v| v.as_array()).is_some(),
            "capabilities 应含 configKeys 数组"
        );
        assert!(
            r.get("appVersion").is_some(),
            "capabilities 应含 appVersion"
        );
    }

    #[test]
    fn fonts_shape() {
        let resp = dispatch(&state(), req("system.fonts", json!({})));
        let r = resp.result.unwrap();
        assert!(r.is_array());
        assert_eq!(r[0]["family"], json!("Sans"));
    }

    #[test]
    fn unknown_method_returns_error() {
        let resp = dispatch(&state(), req("bogus.method", json!({})));
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
    }

    #[test]
    fn data_rpc_forwarded() {
        let resp = dispatch(&state(), req("dict.stats", json!({})));
        assert!(resp.error.is_none());
        assert!(resp.result.unwrap().is_array());
    }

    // ── Stage 2/4: registry 校验 + config.schema / config.getItem ──
    // 容错策略：未知键/类型/枚举错的键被「跳过并在响应 skipped 里报告」，合法项照常应用，
    // 整批不因一个旧字段失败（保护沿用旧字段的 webview）。下列测试单项无合法键，故不写盘。

    /// 取响应里 skipped 数组中的 key 列表。
    fn skipped_keys(resp: &Response) -> Vec<String> {
        resp.result
            .as_ref()
            .and_then(|r| r.get("skipped"))
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|it| it.get("key").and_then(|k| k.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn set_items_skips_unknown_key() {
        let resp = dispatch(
            &state(),
            req(
                "config.setItems",
                json!({ "items": [{ "key": "ui.candidate.bogus", "value": 1 }] }),
            ),
        );
        assert!(resp.error.is_none(), "整批不应失败");
        let r = resp.result.clone().unwrap();
        assert_eq!(r["applied"], json!(0));
        assert!(skipped_keys(&resp).contains(&"ui.candidate.bogus".to_string()));
    }

    #[test]
    fn set_items_skips_enum_out_of_range() {
        let resp = dispatch(
            &state(),
            req(
                "config.setItems",
                json!({ "items": [{ "key": "ui.candidate.layout", "value": "diagonal" }] }),
            ),
        );
        assert!(resp.error.is_none());
        assert!(skipped_keys(&resp).contains(&"ui.candidate.layout".to_string()));
    }

    #[test]
    fn set_items_skips_type_mismatch() {
        let resp = dispatch(
            &state(),
            req(
                "config.setItems",
                json!({ "items": [{ "key": "ui.candidate.per_page", "value": "seven" }] }),
            ),
        );
        assert!(resp.error.is_none());
        assert!(skipped_keys(&resp).contains(&"ui.candidate.per_page".to_string()));
    }

    // ── config.previewPatch / applyPatch 契约（照 scheme.previewImport 先例：只测
    // 只读与错误路径）。applyPatch 的成功写路径**刻意不在此测**——它会真写用户层
    // config.toml（%APPDATA%），校验+展平的纯逻辑已在 wind-config::patch 层覆盖。

    #[test]
    fn preview_patch_reports_entries_readonly() {
        let core = FakeCore::new();
        let st = DispatchState::new(core.clone(), "dev").unwrap();
        let resp = dispatch(
            &st,
            req(
                "config.previewPatch",
                json!({ "text": "[ui.candidate]\nper_page = 9\n" }),
            ),
        );
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.unwrap();
        assert_eq!(r["ok"], json!(true));
        let entries = r["entries"].as_array().expect("entries 应为数组");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["key"], json!("ui.candidate.per_page"));
        assert_eq!(entries[0]["next"], json!(9));
        assert!(entries[0].get("error").is_none(), "合法条目不应有 error");
        assert!(entries[0].get("warning").is_none(), "寻常键不应有 warning");
        // 只读：不得触发热重载（也证明未走落盘通路）。
        assert!(!core.config_applied.load(Ordering::SeqCst));
    }

    /// 风险提示要真的过得了 RPC 面——`warning` 是 core 侧 `PatchEntry` 的新字段，
    /// 序列化漏了的话设置端拿不到，而「提示没出现」没有任何信号。
    ///
    /// 同时钉住 `ok` 与 `warning` 正交：有提示 ≠ 不合法，导入按钮不该因此被禁用。
    #[test]
    fn preview_patch_surfaces_risk_warning_without_blocking() {
        let core = FakeCore::new();
        let st = DispatchState::new(core.clone(), "dev").unwrap();
        let text =
            "[[ui.toolbar.buttons]]\nid = \"x\"\nlabel = \"符\"\naction = 'proc.run(\"x.exe\")'\n";
        let resp = dispatch(&st, req("config.previewPatch", json!({ "text": text })));
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.unwrap();
        assert_eq!(r["ok"], json!(true), "有风险提示不代表不合法");
        let entries = r["entries"].as_array().expect("entries 应为数组");
        let e = entries
            .iter()
            .find(|e| e["key"] == json!("ui.toolbar.buttons"))
            .expect("应含该键");
        let w = e["warning"].as_str().expect("危险键必须带 warning");
        assert!(w.contains("启动程序"), "提示要说清后果，实际：{w}");
    }

    /// Map 键在 RPC 面上逐条目呈现：`key` = 父 Map 键，条目名走 `mapEntry`（serde rename）。
    /// 设置端的确认对话框据此逐条列出「哪个绑定改成了什么」。
    #[test]
    fn preview_patch_reports_map_entries_with_map_entry_field() {
        let core = FakeCore::new();
        let st = DispatchState::new(core.clone(), "dev").unwrap();
        let resp = dispatch(
            &st,
            req(
                "config.previewPatch",
                json!({ "text": "[keys.key_actions]\nf4 = \"english\"\nf5 = \"半角\"\n" }),
            ),
        );
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.unwrap();
        assert_eq!(r["ok"], json!(true));
        let entries = r["entries"].as_array().expect("entries 应为数组");
        assert_eq!(entries.len(), 2, "Map 两个条目应各占一行: {entries:?}");
        for e in entries {
            assert_eq!(e["key"], json!("keys.key_actions"), "key 恒为父 Map 键");
            assert!(e.get("mapEntry").is_some(), "Map 条目须带 mapEntry: {e}");
        }
        assert_eq!(entries[0]["mapEntry"], json!("f4"));
        assert_eq!(entries[0]["next"], json!("english"));
        assert!(!core.config_applied.load(Ordering::SeqCst), "预览只读");
    }

    #[test]
    fn preview_patch_flags_unknown_and_invalid_values() {
        let resp = dispatch(
            &state(),
            req(
                "config.previewPatch",
                json!({ "text": "[ui.candidate]\nlayout = \"diagonal\"\n[input.foo]\nbar = 1\n" }),
            ),
        );
        assert!(resp.error.is_none(), "逐键错误不是 RPC 错误");
        let r = resp.result.unwrap();
        assert_eq!(r["ok"], json!(false));
        let entries = r["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries.iter().all(|e| e.get("error").is_some()),
            "两条都应带 error: {entries:?}"
        );
    }

    #[test]
    fn preview_patch_rejects_invalid_toml_as_whole() {
        let resp = dispatch(
            &state(),
            req("config.previewPatch", json!({ "text": "= not toml =" })),
        );
        assert!(resp.result.is_none());
        assert!(resp.error.is_some(), "整体解析失败应为 RPC 错误");
    }

    #[test]
    fn apply_patch_rejects_fragment_with_any_error() {
        let core = FakeCore::new();
        let st = DispatchState::new(core.clone(), "dev").unwrap();
        // 一条合法 + 一条未知键：整体拒绝，不做半应用。
        let resp = dispatch(
            &st,
            req(
                "config.applyPatch",
                json!({ "text": "[ui.candidate]\nper_page = 9\nbogus = 1\n" }),
            ),
        );
        assert!(resp.result.is_none());
        let err = resp.error.expect("应整体拒绝");
        assert!(err.contains("bogus"), "错误应点名出错的键: {err}");
        assert!(
            !core.config_applied.load(Ordering::SeqCst),
            "整体拒绝不得触发热重载（也证明未走落盘通路）"
        );
    }

    /// 片段自带的说明元信息随预览上浮：`info` 与 `entries` 各说各的——保留段既不产出
    /// 配置条目，也不影响真配置键的展平。
    #[test]
    fn preview_patch_carries_package_info() {
        let core = FakeCore::new();
        let st = DispatchState::new(core.clone(), "dev").unwrap();
        let resp = dispatch(
            &st,
            req(
                "config.previewPatch",
                json!({ "text": "[package]\ntitle = \"九列候选\"\ndescription = \"把候选窗改成每页 9 个。\"\n[ui.candidate]\nper_page = 9\n" }),
            ),
        );
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.unwrap();
        assert_eq!(r["ok"], json!(true));
        let entries = r["entries"].as_array().expect("entries 应为数组");
        assert_eq!(entries.len(), 1, "保留段不产出配置条目: {entries:?}");
        assert_eq!(entries[0]["key"], json!("ui.candidate.per_page"));
        assert_eq!(r["info"]["title"], json!("九列候选"));
        assert_eq!(r["info"]["description"], json!("把候选窗改成每页 9 个。"));
        assert!(!core.config_applied.load(Ordering::SeqCst), "预览只读");
    }

    /// 只写 title 时 `info` 里**没有** description 字段（逐字段 skip，不是整个 info 全有全无）。
    #[test]
    fn preview_patch_info_omits_unwritten_field() {
        let resp = dispatch(
            &state(),
            req(
                "config.previewPatch",
                json!({ "text": "[package]\ntitle = \"只有标题\"\n[ui.candidate]\nper_page = 9\n" }),
            ),
        );
        let r = resp.result.expect("应成功");
        assert_eq!(r["info"]["title"], json!("只有标题"));
        assert!(
            r["info"].get("description").is_none(),
            "没写的字段不输出: {}",
            r["info"]
        );
    }

    /// 无 `[package]` 段（或段内两字段都缺省）→ 响应里**没有** info 字段，
    /// 前端不必区分「没写」与「写了空串」。
    #[test]
    fn preview_patch_omits_info_when_absent() {
        for text in [
            "[ui.candidate]\nper_page = 9\n",
            "[package]\nkind = \"schema_text\"\n[ui.candidate]\nper_page = 9\n",
            "[package]\ntitle = \"   \"\n[ui.candidate]\nper_page = 9\n",
        ] {
            let resp = dispatch(
                &state(),
                req("config.previewPatch", json!({ "text": text })),
            );
            let r = resp.result.expect("应成功");
            assert!(
                r.get("info").is_none(),
                "不该出现 info 字段: {r} ({text:?})"
            );
        }
    }

    /// 说明非法 → preview 与 apply **都**整体拒绝（同一套判据）。
    /// 只在其中一处拒绝，分发者就会遇到「预览好好的、装的时候炸了」。
    #[test]
    fn invalid_package_info_rejects_preview_and_apply() {
        let bad = [
            // title 不许换行
            "[package]\ntitle = \"第一行\\n第二行\"\n[ui.candidate]\nper_page = 9\n",
            // 类型不对
            "[package]\ntitle = 5\n[ui.candidate]\nper_page = 9\n",
            // C0 控制字符
            "[package]\ndescription = \"说明\\u0007\"\n[ui.candidate]\nper_page = 9\n",
        ];
        for text in bad {
            let core = FakeCore::new();
            let st = DispatchState::new(core.clone(), "dev").unwrap();
            for method in ["config.previewPatch", "config.applyPatch"] {
                let resp = dispatch(&st, req(method, json!({ "text": text })));
                assert!(resp.result.is_none(), "{method} 应整体拒绝: {text:?}");
                let err = resp.error.expect("应有错误");
                assert!(err.contains("package."), "错误须点名字段: {err}");
            }
            assert!(
                !core.config_applied.load(Ordering::SeqCst),
                "整体拒绝不得触发热重载（也证明未走落盘通路）"
            );
        }
    }

    #[test]
    fn apply_patch_empty_fragment_is_noop_success() {
        let core = FakeCore::new();
        let st = DispatchState::new(core.clone(), "dev").unwrap();
        let resp = dispatch(&st, req("config.applyPatch", json!({ "text": "" })));
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["applied"], json!(0));
        // written 恒在场（空数组），设置端回灌逻辑不必分「有没有这个字段」。
        assert_eq!(r["written"], json!([]));
        assert!(
            !core.config_applied.load(Ordering::SeqCst),
            "no-op 不落盘也不热重载"
        );
    }

    #[test]
    fn config_schema_lists_registered_fields() {
        let resp = dispatch(&state(), req("config.schema", json!({})));
        assert!(resp.error.is_none());
        let r = resp.result.unwrap();
        let fields = r["fields"].as_array().expect("fields 应为数组");
        let layout = fields
            .iter()
            .find(|f| f["key"] == json!("ui.candidate.layout"))
            .expect("应含 ui.candidate.layout");
        assert_eq!(layout["type"], json!("enum"));
        assert!(
            layout["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == &json!("vertical")),
            "enum 应带 options"
        );
    }

    #[test]
    fn config_get_item_known_returns_value_unknown_errors() {
        let ok = dispatch(
            &state(),
            req("config.getItem", json!({ "key": "ui.candidate.per_page" })),
        );
        assert!(ok.error.is_none(), "已登记键应成功");
        assert!(ok.result.unwrap()["value"].is_number());

        let bad = dispatch(
            &state(),
            req("config.getItem", json!({ "key": "no.such.key" })),
        );
        assert!(bad.result.is_none());
        assert!(bad.error.is_some());
    }
}
