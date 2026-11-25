//! 端到端守卫：韵母模糊组 `un ↔ ong`、`eng ↔ ong` 在真实词库下确实能打出来。
//!
//! 单元测试（`pinyin::fuzzy`）钉的是变体生成本身；本文件钉的是配置 → `EngineManager`
//! 映射 → 引擎召回整条链路，且每条都带「关着不出」的对照，证明命中来自模糊路径。
//!
//! 词库不存在时跳过（与 `pinyin_fuzzy_cross` 同惯例）。

use std::path::PathBuf;

use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("build_dev")
        .join("data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

/// 用指定模糊音开关建引擎，返回 `input` 的候选文本列表。
fn candidates(input: &str, apply: impl FnOnce(&mut Config)) -> Option<Vec<String>> {
    let dir = data_dir()?;
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    apply(&mut cfg);
    let mgr = EngineManager::new(&cfg, Some(&dir));
    Some(
        mgr.convert_with("pinyin", input, 50)
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect(),
    )
}

fn head(v: &[String]) -> &[String] {
    &v[..v.len().min(10)]
}

/// 断言：开关打开时 `input` 召回 `want`，出厂配置下不召回。
fn assert_gated(input: &str, want: &str, on: impl FnOnce(&mut Config)) {
    let Some(with_fuzzy) = candidates(input, |cfg| {
        cfg.input.fuzzy.enabled = true;
        on(cfg);
    }) else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    assert!(
        with_fuzzy.iter().any(|t| t == want),
        "开关打开后 {input} 须召回「{want}」，实际前 10 项: {:?}",
        head(&with_fuzzy)
    );
    let without = candidates(input, |_| {}).expect("词库已确认存在");
    assert!(
        !without.iter().any(|t| t == want),
        "出厂配置下 {input} 不该出「{want}」，否则本测试证明不了模糊路径生效，实际前 10 项: {:?}",
        head(&without)
    );
}

#[test]
fn un_ong_single_syllable_dun_recalls_dong() {
    assert_gated("dun", "东", |cfg| cfg.input.fuzzy.un_ong = true);
}

#[test]
fn un_ong_recalls_dongxi() {
    assert_gated("dunxi", "东西", |cfg| cfg.input.fuzzy.un_ong = true);
}

#[test]
fn un_ong_recalls_yong_from_yun() {
    assert_gated("yunyuan", "永远", |cfg| cfg.input.fuzzy.un_ong = true);
}

#[test]
fn eng_ong_recalls_zhongguo() {
    assert_gated("zhengguo", "中国", |cfg| cfg.input.fuzzy.eng_ong = true);
}

/// 总开关关着时，单项开关不生效（`manager.rs` 的 `enabled &&` 映射）。
#[test]
fn master_switch_gates_un_ong() {
    let Some(cands) = candidates("dunxi", |cfg| {
        cfg.input.fuzzy.enabled = false;
        cfg.input.fuzzy.un_ong = true;
    }) else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    assert!(
        !cands.iter().any(|t| t == "东西"),
        "总开关关着时 un_ong 不该生效，实际前 10 项: {:?}",
        head(&cands)
    );
}
