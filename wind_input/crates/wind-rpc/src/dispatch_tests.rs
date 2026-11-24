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
        match method {
            "dict.stats" => Ok(json!([])),
            "compat.list" | "compat.schema" | "compat.export" => Ok(json!({ "ok": true })),
            "compat.upsert" | "compat.import" | "compat.reset" | "compat.reload" => {
                Ok(json!({ "ok": true }))
            }
            "compat.broken" => anyhow::bail!("写入失败"),
            "schema.setDictEnabled"
            | "schema.saveConfig"
            | "schema.getConfig"
            | "scheme.importPackage" => Ok(json!({ "ok": true })),
            // 两层错误：外层是人话主语，内层才是真正的原因。用来钉住出口不截断
            // （见 `dispatch` 的文档注释）。
            "test.layered" => Err(anyhow::anyhow!("底层真正的原因").context("外层的人话")),
            _ => anyhow::bail!("unknown method: {}", method),
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

/// `config.get` 传了 `key` 要当场报错，不能无视后照常返回整份配置——否则调用方读回执的
/// `value` 得到 `None`，功能静默失效。
#[test]
fn config_get_rejects_a_key_param_instead_of_silently_ignoring_it() {
    let e = dispatch(
        &state(),
        req("config.get", json!({ "key": "ui.theme.name" })),
    )
    .error
    .expect("传了 key 必须报错");
    assert!(e.contains("key"), "报错要点明是 key 这个参数的问题：{e}");

    // 正向对照：不传 key 照常返回整份配置。没有这条，上面那条在「config.get 整个
    // 坏掉」时也会绿。
    let ok = dispatch(&state(), req("config.get", json!({})));
    assert!(ok.error.is_none(), "不传 key 不该失败：{:?}", ok.error);
    assert!(
        ok.result.is_some_and(|v| v.get("ui").is_some()),
        "回执该是整份配置（含 ui 段）"
    );
}

/// 出口不许把错误的因果链截断：这是 core 向设置端与 CLI 传失败原因的唯一出口，
/// 写成 `e.to_string()` 会只剩最外层那句、抹掉真正的原因。断两层错误，因为单层下
/// `{:#}` 与 `{}` 输出相同。
///
/// `wind-webdata` 的 `theme_package_rpc_contract` 断言 `{:#}`，依赖本用例保证它等于客户端所见。
#[test]
fn error_response_keeps_the_whole_cause_chain() {
    let resp = dispatch(&state(), req("test.layered", json!({})));
    let e = resp.error.expect("该方法必然失败");
    assert!(
        e.contains("底层真正的原因"),
        "出口把内层原因截断了，客户端只会看到一句无用的话：{e}"
    );
    assert!(e.contains("外层的人话"), "外层主语也要在，别只剩原因：{e}");
}

#[test]
fn system_info_shape() {
    set_build_id("abc1234-dirty");
    let resp = dispatch(&state(), req("system.info", json!({})));
    assert_eq!(resp.id, 7);
    assert!(resp.error.is_none());
    let r = resp.result.unwrap();
    for k in ["version", "platform", "dataDir", "running"] {
        assert!(r.get(k).is_some(), "system.info 缺字段 {k}");
    }
    assert_eq!(r["build"], json!("abc1234-dirty"));
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
fn untrusted_caller_cannot_reach_sync_methods() {
    for m in ["sync.command", "sync.setSettings", "sync.status"] {
        let r = dispatch_as(&state(), req(m, json!({})), false);
        assert_eq!(r.error.as_deref(), Some(DENY_UNTRUSTED), "{m}");
    }
    // 对照：可信调用方放行（假 core 对未知方法报 unknown method，说明确实转发了）。
    let r = dispatch_as(&state(), req("sync.status", json!({})), true);
    assert!(r.error.unwrap().contains("unknown method"));
    // 非 sync 方法不受影响。
    let r = dispatch_as(&state(), req("dict.stats", json!({})), false);
    assert!(r.error.is_none());
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

// ── config.previewPatch / applyPatch 契约：只测只读与错误路径。applyPatch 的成功写路径
// 会真写用户层 config.toml，故不在此测；校验+展平已在 wind-config::patch 层覆盖。

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

/// 手改 config.toml 之后靠它让核心重读：只广播不重载的话，改动要等下次保存才生效。
#[test]
fn config_reload_applies_config_and_broadcasts() {
    let core = FakeCore::new();
    let st = DispatchState::new(core.clone(), "dev").expect("capabilities 应能加载");
    let resp = dispatch(&st, req("config.reload", json!({})));
    let r = resp.result.unwrap();
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["needsRestart"], json!(false));
    assert!(core.config_applied.load(Ordering::SeqCst), "应触发热重载");
}

/// 风险提示要过得了 RPC 面：`PatchEntry.warning` 序列化漏了的话设置端拿不到且无信号。
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

/// 只写 title 时 `info` 里没有 description 字段（逐字段 skip，不是整个 info 全有全无）。
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

/// 无 `[package]` 段（或段内两字段都缺省）→ 响应里没有 info 字段，
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

/// 说明非法 → preview 与 apply 都整体拒绝（同一套判据）。
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

// ── compat.changed：写方法成功后广播，只读 / dryRun / 失败都不广播 ──

fn state_with_events() -> (DispatchState, std::sync::mpsc::Receiver<Vec<u8>>) {
    let sink = crate::events::EventSink::new();
    let rx = sink.subscribe();
    let st =
        DispatchState::with_events(FakeCore::new(), "dev", sink).expect("capabilities 应能加载");
    (st, rx)
}

/// 取走已广播的事件名（帧 = 4 字节长度前缀 + JSON）。
fn drain_events(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<String> {
    rx.try_iter()
        .map(|b| {
            let v: Value = serde_json::from_slice(&b[4..]).expect("事件应是合法 JSON");
            v["event"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

#[test]
fn compat_write_method_emits_compat_changed_and_forwards_the_result() {
    let (st, rx) = state_with_events();
    for m in [
        "compat.upsert",
        "compat.reset",
        "compat.import",
        "compat.reload",
    ] {
        let resp = dispatch(&st, req(m, json!({})));
        assert!(resp.error.is_none(), "{m}");
        assert_eq!(resp.result.unwrap()["ok"], json!(true), "结果要原样透传");
    }
    assert_eq!(
        drain_events(&rx),
        vec!["compat.changed"; 4],
        "每次写入各广播一次"
    );
}

#[test]
fn compat_read_methods_and_dry_run_import_do_not_emit() {
    let (st, rx) = state_with_events();
    for m in ["compat.list", "compat.schema", "compat.export"] {
        assert!(dispatch(&st, req(m, json!({}))).error.is_none());
    }
    let dry = dispatch(&st, req("compat.import", json!({ "dryRun": true })));
    assert!(dry.error.is_none());
    assert!(drain_events(&rx).is_empty(), "只读与 dryRun 不应广播");
}

#[test]
fn compat_failed_write_does_not_emit() {
    let (st, rx) = state_with_events();
    let resp = dispatch(&st, req("compat.broken", json!({})));
    assert!(resp.error.is_some());
    assert!(drain_events(&rx).is_empty(), "失败的写入不应广播");
}

// ── schema.configChanged：改方案配置的写方法成功后广播，带方案 id ──

#[test]
fn schema_config_write_emits_with_id() {
    let (st, rx) = state_with_events();
    let p = json!({ "id": "wubi86", "dictId": "ext", "enabled": false });
    let resp = dispatch(&st, req("schema.setDictEnabled", p));
    assert_eq!(resp.result.unwrap()["ok"], json!(true), "结果要原样透传");
    assert!(
        dispatch(&st, req("schema.saveConfig", json!({ "id": "py" })))
            .error
            .is_none()
    );
    let got: Vec<(String, Value)> = rx
        .try_iter()
        .map(|b| {
            let v: Value = serde_json::from_slice(&b[4..]).unwrap();
            (
                v["event"].as_str().unwrap().to_string(),
                v["data"]["id"].clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("schema.configChanged".to_string(), json!("wubi86")),
            ("schema.configChanged".to_string(), json!("py")),
        ]
    );
}

/// 导入方案包可能动多个方案：载荷不带 id，订阅方按「全部」处理。
#[test]
fn multi_schema_write_emits_without_id() {
    let (st, rx) = state_with_events();
    let resp = dispatch(&st, req("scheme.importPackage", json!({ "path": "x.zip" })));
    assert!(resp.error.is_none());
    let data: Vec<Value> = rx
        .try_iter()
        .map(|b| serde_json::from_slice::<Value>(&b[4..]).unwrap())
        .map(|v| {
            assert_eq!(v["event"], json!("schema.configChanged"));
            v["data"].clone()
        })
        .collect();
    assert_eq!(data, vec![json!({ "method": "scheme.importPackage" })]);
}

#[test]
fn schema_config_write_methods_and_ids() {
    let p = json!({ "id": "a", "schemaId": "b" });
    for (m, want) in [
        ("schema.saveConfig", Some(Some("a"))),
        ("schema.resetConfig", Some(Some("a"))),
        ("schema.setDictEnabled", Some(Some("a"))),
        ("schema.delete", Some(Some("a"))),
        ("userDicts.setEnabled", Some(Some("b"))),
        ("userdata.purgeSchema", Some(Some("b"))),
        ("scheme.importPackage", Some(None)),
        ("scheme.importText", Some(None)),
        ("backup.restore", Some(None)),
        ("schema.reloadFiles", Some(None)),
        ("schema.getConfig", None),
        ("scheme.previewImport", None),
    ] {
        assert_eq!(schema_config_write_id(m, &p), want, "{m}");
    }
}

#[test]
fn schema_config_read_or_failed_write_does_not_emit() {
    let (st, rx) = state_with_events();
    assert!(
        dispatch(&st, req("schema.getConfig", json!({ "id": "a" })))
            .error
            .is_none()
    );
    assert!(
        dispatch(&st, req("schema.resetConfig", json!({ "id": "a" })))
            .error
            .is_some()
    );
    assert!(drain_events(&rx).is_empty());
}
