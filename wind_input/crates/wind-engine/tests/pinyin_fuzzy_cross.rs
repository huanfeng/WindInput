//! 端到端守卫：同一音节内**声母 + 韵母同时模糊**必须能召回。
//!
//! 现场问题：开启 `sh/s` 与 `en/eng` 后，`senxiao` 打不出「生肖」。
//! 根因在 `pinyin::fuzzy`——声母规则与韵母规则曾各自作用于原始音节、并列产出变体，
//! 于是 `sen` 只能得到 `shen`（改声母）与 `seng`（改韵母），独缺两处同时改的 `sheng`。
//! 单元测试钉的是变体生成本身，本文件钉的是「真实词库下用户确实能打出来」。
//!
//! 词库不存在时跳过（与 `pinyin_demote_risk` 等同惯例）。

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

#[test]
fn sh_s_plus_en_eng_recalls_shengxiao() {
    let Some(with_fuzzy) = candidates("senxiao", |cfg| {
        cfg.input.fuzzy.enabled = true;
        cfg.input.fuzzy.sh_s = true;
        cfg.input.fuzzy.en_eng = true;
    }) else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    assert!(
        with_fuzzy.iter().any(|t| t == "生肖"),
        "开启 sh/s + en/eng 后 senxiao 须能召回「生肖」，实际前 10 项: {:?}",
        &with_fuzzy[..with_fuzzy.len().min(10)]
    );

    // 对照组：不开模糊音时召不回 —— 证明命中确实来自模糊路径，而非词库里正好有个
    // 读作 senxiao 的词条把测试蒙过去。
    let without_fuzzy = candidates("senxiao", |_| {}).expect("词库已确认存在");
    assert!(
        !without_fuzzy.iter().any(|t| t == "生肖"),
        "关闭模糊音时不该出现「生肖」，否则本测试证明不了模糊路径生效"
    );
}

/// 另一组交叉：`sh/s` + `an/ang`，`sanhai` → 「上海」（san → shang，两处同时改）。
#[test]
fn sh_s_plus_an_ang_recalls_shanghai() {
    let Some(cands) = candidates("sanhai", |cfg| {
        cfg.input.fuzzy.enabled = true;
        cfg.input.fuzzy.sh_s = true;
        cfg.input.fuzzy.an_ang = true;
    }) else {
        eprintln!("跳过：build_dev 拼音词库不存在");
        return;
    };
    assert!(
        cands.iter().any(|t| t == "上海"),
        "sanhai 须能召回「上海」，实际前 10 项: {:?}",
        &cands[..cands.len().min(10)]
    );
}
