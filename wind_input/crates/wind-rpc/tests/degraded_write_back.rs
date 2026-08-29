//! 降级写盘闸的端到端验证：`config.applyPatch` 与 `config.setItems` 在段级降级时
//! **不得**拿出厂残表覆盖用户的 Map 键。
//!
//! # 被测的失效模式
//!
//! 这两条路都以「当前生效配置」为 Map 键的合并/整表**种子**：
//!
//! - `applyPatch`：`patch::writes` 把片段条目并进当前表再整表写回；
//! - `setItems`：设置端把 map 型键作原子叶子**整表发送**（wind-setting 的 `diff_config`），
//!   其内容 = 它启动时经 `config.get` 读到的 base ⊕ 本次编辑。
//!
//! 段级降级之后，「当前生效配置」在坏段处是**出厂值**。于是「加一条自定义标点」这个
//! 动作会把用户已有的**整张**映射表抹掉，永久且无痕。P1 之后这条路还可由第三方触发：
//! 定制者在 `data_custom/config.toml` 里把该键写成错类型，该定制版的每个用户每次
//! `load()` 都降级。
//!
//! # 为什么必须是集成测试（独立进程）
//!
//! 闸要真的生效，测试必须让 `Config::load()` 读到一份**真的会降级**的配置，也就必须
//! 重定向用户目录（`WIND_DATADIR_CONF`）与安装根（`WIND_INSTALL_ROOT`）。这两个杠杆
//! 都经 OnceLock，同一进程只认第一次——放进 `#[cfg(test)]` 单测模块的话，同二进制里
//! 先跑的别的测试会把 OnceLock 定死，重定向**静默失效**，于是本测试转而去写用户真实的
//! `%APPDATA%\WindInput\config.toml`。本仓已有这类前科，故只能独立成一个测试二进制。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use wind_ipc::rpc::Request;
use wind_rpc::{CoreRpc, DispatchState, dispatch};

/// 最小 CoreRpc 桩：本测试只走 config.* 分支，其余走 trait 默认实现。
struct StubCore;

impl CoreRpc for StubCore {
    fn is_chinese_mode(&self) -> bool {
        true
    }
    fn active_schema_id(&self) -> String {
        "wubi86".into()
    }
    /// 落盘后的热重载：桩里什么都不做（本测试断言的是**磁盘内容**，不是生效状态）。
    fn apply_config(&self) -> bool {
        false
    }
}

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

fn call(state: &DispatchState, method: &str, params: Value) -> Result<Value, String> {
    let r = dispatch(
        state,
        Request {
            version: 1,
            id: 1,
            method: method.to_string(),
            params,
        },
    );
    match (r.result, r.error) {
        (Some(v), None) => Ok(v),
        (_, Some(e)) => Err(e),
        _ => panic!("既无 result 也无 error"),
    }
}

/// 一条自定义标点映射，代表「用户的真实数据」。
const USER_MAPPING: &str = "\"'1\" = [\"①\"]";

#[test]
fn degraded_section_blocks_map_write_back() {
    let tmp = std::env::temp_dir().join("wind_rpc_degraded_write_back_e2e");
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();
    write_at(&data, "config.toml", "[schema]\nactive = \"wubi86\"\n");

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        wind_config::Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会写真实 %APPDATA%"
    );

    // 用户层：一条真实映射 + **同段**的一个类型错误键。
    // 毒故意下在 `follow_mode`（Bool 收到字符串）而不是 `custom_mappings` 自己——
    // 要模拟的正是「用户数据完好无损，却因为同段另一个键而读不出来」这个形态。
    let poisoned = format!(
        "[input.punct]\nfollow_mode = \"not-a-bool\"\n\n[input.punct.custom_mappings]\n{USER_MAPPING}\n"
    );
    let file = write_at(&user, "config.toml", &poisoned);

    let state = DispatchState::new(Arc::new(StubCore), "dev").expect("DispatchState");

    // 前置确认：这份配置确实触发了 input.punct 段降级，且 custom_mappings 已变成空表。
    let cfg = wind_config::Config::load(wind_config::Config::data_dir().as_deref()).unwrap();
    assert!(
        cfg.degradation.taints("input.punct.custom_mappings"),
        "前置条件：本用例须真的触发该段降级，实际 sections={:?} total={}",
        cfg.degradation.sections,
        cfg.degradation.total_fallback
    );
    assert!(
        cfg.input.punct.custom_mappings.is_empty(),
        "前置条件：降级后生效表应为出厂空表——这正是不能拿它当种子的原因"
    );

    // ── applyPatch：预览就该报错，应用整体拒绝 ───────────────────────────────

    let patch_text = "[input.punct.custom_mappings]\n\"'2\" = [\"②\"]\n";
    let preview = call(&state, "config.previewPatch", json!({ "text": patch_text }))
        .expect("previewPatch 不应整体失败（错误是逐条目的）");
    assert_eq!(
        preview["ok"],
        json!(false),
        "★ 预览必须报不 ok——预览放行、应用才拒绝是最难自查的一类不一致\n{preview}"
    );
    assert!(
        preview["entries"][0]["error"].is_string(),
        "该条目须带 error 说明原因\n{preview}"
    );

    let err = call(&state, "config.applyPatch", json!({ "text": patch_text }))
        .expect_err("★ applyPatch 必须整体拒绝");
    assert!(
        err.contains("invalid_patch"),
        "拒绝理由应走既有的 invalid_patch 通道，实得: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        poisoned,
        "★ 用户 config.toml 必须一个字节都没变"
    );

    // ── setItems：Map 键跳过并记 skipped，标量键照常写入 ─────────────────────

    let out = call(
        &state,
        "config.setItems",
        json!({ "items": [
            { "key": "input.punct.custom_mappings", "value": { "'2": ["②"] } },
            { "key": "ui.candidate.per_page", "value": 9 },
        ]}),
    )
    .expect("setItems 不该整体失败");
    assert_eq!(
        out["applied"],
        json!(1),
        "★ Map 键须被跳过、标量键须照常写入\n{out}"
    );
    let skipped = out["skipped"].as_array().expect("skipped 应为数组");
    assert!(
        skipped
            .iter()
            .any(|s| s["key"] == json!("input.punct.custom_mappings")),
        "被跳过的键须出现在 skipped 里（否则设置端以为写成功了）\n{out}"
    );
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(
        after.contains("'1") && !after.contains("'2"),
        "★ 用户原有映射须完好、新的整表不得落盘，实际:\n{after}"
    );
    assert!(
        after.contains("per_page"),
        "标量键不该被降级闸牵连（拦掉它等于降级期间整个设置页无法保存）\n{after}"
    );

    // ── 正向对照：毒去掉之后两条路都照常工作 ─────────────────────────────────
    //
    // 没有这一步，上面的「拒绝」与「这套环境下本来就写不进去」无法区分。

    let clean = format!("[input.punct.custom_mappings]\n{USER_MAPPING}\n");
    std::fs::write(&file, &clean).unwrap();
    let cfg = wind_config::Config::load(wind_config::Config::data_dir().as_deref()).unwrap();
    assert!(!cfg.degradation.is_degraded(), "前置：毒已清除");

    let preview = call(&state, "config.previewPatch", json!({ "text": patch_text })).unwrap();
    assert_eq!(preview["ok"], json!(true), "无降级时预览须放行\n{preview}");
    call(&state, "config.applyPatch", json!({ "text": patch_text }))
        .expect("无降级时 applyPatch 须成功");
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(
        after.contains("'1") && after.contains("'2"),
        "★ 正常路径必须是**合并**：既有条目保留、新条目加入，实际:\n{after}"
    );

    let out = call(
        &state,
        "config.setItems",
        json!({ "items": [
            { "key": "input.punct.custom_mappings", "value": { "'1": ["①"], "'3": ["③"] } },
        ]}),
    )
    .unwrap();
    assert_eq!(out["applied"], json!(1), "无降级时 Map 键须照常写入\n{out}");
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(
        after.contains("'3"),
        "无降级时设置端发来的整表须落盘，实际:\n{after}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
