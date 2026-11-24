//! 内置短语的可达性守门：系统短语必须打得出，不能被同码的码表候选压掉。
//!
//! 精确码短语与码表精确候选同属精确档，先后完全由权重裁决（见 `handle_candidate.rs` 的
//! `lookup` 分支）。因此内置数据有一条隐式契约：系统短语的权重必须高于同码码表词条，
//! 否则用户打 `datm` 得到的是「万花筒」。这条契约靠人守不住：
//! - 词库按词频重排时权重会整体变动（`wubi86_jidian.dict.yaml` 头部记录了重排历史）；
//! - 新增系统短语时作者不会想到去查这个码是否已有高频词；
//! - 失效时没有任何报错，只是首选悄悄换了一个。
//!
//! 这里检查的是数据，不是排序逻辑：排序侧的双向对照在
//! `input_flow.rs::phrase_and_codetable_exact_compete_by_weight`，两者缺一不可。

use std::collections::HashMap;
use std::path::PathBuf;

fn data_dir() -> PathBuf {
    // 三级：crates/wind-coordinator → crates → wind_input → 仓库根（build_dev 在仓库根）。
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 逐个内置纯码表方案检查：其全部词库里，是否存在权重不低于同码系统短语的精确条目。
///
/// 范围只有纯码表，因为「按权重竞争」只在那里发生：
/// - 拼音：候选 `is_exact_code` 恒 false，短语靠 `cmp_exact_first` 排在其上，不比权重；
/// - 混输：`source_tier` 档 0（码表精确）恒先于档 1（精确码短语），纳进来只会误报。
///
/// 过滤用 `is_pinyin()`/`is_mixed()` 而非 `engine.engine_type == "codetable"`：
/// `wubi86.schema.toml` 没写 engine_type，按字面值筛会把唯一要查的方案排空、测试静默通过。
/// 下方 `checked_dicts > 0` 兜底「过滤器把自己筛没了」。
#[test]
fn builtin_phrases_outweigh_same_code_codetable_entries() {
    let d = data_dir();
    let phrases_path = d.join("system.phrases.toml");
    let schemas_dir = d.join("schemas");
    if !phrases_path.is_file() || !schemas_dir.is_dir() {
        // 与 input_flow 同约定：无 build_dev/data 时跳过。判据是耗时 0.00s。
        return;
    }

    // 同码多条短语只留最高权重的那条：用户能否「打得出」取决于最强的一条能否排到前面。
    let mut phrase_max: HashMap<String, i32> = HashMap::new();
    for e in wind_phrase::PhraseLayer::parse_system_entries(&phrases_path) {
        phrase_max
            .entry(e.code.clone())
            .and_modify(|w| *w = (*w).max(e.weight))
            .or_insert(e.weight);
    }
    assert!(
        !phrase_max.is_empty(),
        "system.phrases.toml 解析出 0 条系统短语——解析失败会让本测试静默变成空操作"
    );

    let mut schemas: Vec<PathBuf> = std::fs::read_dir(&schemas_dir)
        .expect("读 schemas 目录")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".schema.toml"))
        .collect();
    schemas.sort();

    let mut checked_dicts = 0usize;
    let mut problems: Vec<String> = Vec::new();
    for sp in schemas {
        let Ok(text) = std::fs::read_to_string(&sp) else {
            continue;
        };
        let Ok(schema) = toml::from_str::<wind_config::Schema>(&text) else {
            continue;
        };
        // 拼音/双拼/混输不参与（理由见函数文档）。
        if schema.is_pinyin() || schema.is_mixed() {
            continue;
        }
        let sid = schema.schema.id.clone();
        for ds in schema.dictionaries.iter().filter(|d| !d.path.is_empty()) {
            let path = schemas_dir.join(&ds.path);
            if !path.is_file() {
                continue;
            }
            let Ok(dict) = wind_dict::cached::CachedDict::load(&path) else {
                continue;
            };
            checked_dicts += 1;
            // `default_weight` 会抹平整库权重，此时词条的实际权重是它而非文件里的值。
            for (code, &pw) in &phrase_max {
                for (text, w, _) in dict.search(code) {
                    let eff = ds.default_weight.unwrap_or(w);
                    if eff >= pw {
                        problems.push(format!(
                            "  短语码 {code}(w={pw}) ← {sid}/{} 的「{text}」(w={eff}) 压住或持平",
                            ds.path
                        ));
                    }
                }
            }
        }
    }
    assert!(
        checked_dicts > 0,
        "一个码表词库都没读到——本测试会静默通过，须先确认 build_dev/data 是完整的"
    );
    assert!(
        problems.is_empty(),
        "内置系统短语被同码码表词条压住，用户打这些码时首选不是短语：\n{}\n\
         修法是**调高该短语在 data/system.phrases.toml 里的权重**（值域 0~10000），\
         不是改排序规则——短语与码表精确候选按权重竞争是有意设计。",
        problems.join("\n")
    );
}
