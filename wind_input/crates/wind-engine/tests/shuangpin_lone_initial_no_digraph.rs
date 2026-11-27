//! 双拼落单的 z/c/s 只补全 z/c/s 声母音节，不带出 zh/ch/sh（它们在双拼里占 v/i/u 键）。
//! 全拼不受影响：`z` 后面还能接 `h`。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::fuzzy::FuzzyConfig;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter};
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

const ENTRIES: &[(&str, &str, i32)] = &[
    ("zai", "在", 9000),
    ("zhong", "中", 8000),
    ("cong", "从", 9000),
    ("chi", "吃", 8000),
    ("shi", "是", 9000),
    ("si", "四", 8000),
];

fn dict(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_sp_lone_initial_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    for (code, text, weight) in ENTRIES {
        w.add_with_boundary((*code).into(), vec![((*text).into(), *weight, 0, 0b1)]);
    }
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn sp_engine(tag: &str, fuzzy: Option<FuzzyConfig>) -> PinyinEngine {
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let e = PinyinEngine::new(PyConfig::default(), dict(tag));
    let e = match fuzzy {
        Some(f) => e.with_fuzzy(f),
        None => e,
    };
    e.with_shuangpin(ShuangpinConverter::new(layout))
}

fn texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    e.convert(input, 50)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

#[test]
fn lone_z_c_s_skip_digraph_words() {
    let e = sp_engine("skip", None);
    let z = texts(&e, "z");
    assert!(z.contains(&"在".to_string()), "z 应出 z 声母字: {z:?}");
    assert!(!z.contains(&"中".to_string()), "z 不该出 zh 字: {z:?}");
    let c = texts(&e, "c");
    assert!(
        c.contains(&"从".to_string()) && !c.contains(&"吃".to_string()),
        "{c:?}"
    );
    let s = texts(&e, "s");
    assert!(
        s.contains(&"四".to_string()) && !s.contains(&"是".to_string()),
        "{s:?}"
    );
}

#[test]
fn lone_z_keeps_digraph_when_fuzzy_on() {
    let fuzzy = FuzzyConfig {
        zh_z: true,
        ..FuzzyConfig::default()
    };
    let e = sp_engine("fuzzy", Some(fuzzy));
    let z = texts(&e, "z");
    assert!(
        z.contains(&"中".to_string()),
        "开 zh=z 模糊音时 z 应带出 zh 字: {z:?}"
    );
}

/// zh 字多到超过补全名额时，z 声母字仍要出来（不能先截断名额再过滤掉 zh）。
#[test]
fn lone_z_survives_when_digraph_words_crowd_the_quota() {
    const ZH: [&str; 6] = ["zha", "zhan", "zhao", "zhe", "zhi", "zhong"];
    let dir = std::env::temp_dir().join("wind_sp_lone_initial_crowd");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    for (ci, code) in ZH.iter().enumerate() {
        let hits: Vec<_> = (0..40u32)
            .map(|k| {
                let ch = char::from_u32(0x4e00 + ci as u32 * 100 + k).unwrap();
                (ch.to_string(), 100_000 + k as i32, k, 0b1)
            })
            .collect();
        w.add_with_boundary((*code).into(), hits);
    }
    w.add_with_boundary("zai".into(), vec![("在".into(), 50, 0, 0b1)]);
    w.write(&wdat).unwrap();
    let d = CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具");
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let e =
        PinyinEngine::new(PyConfig::default(), d).with_shuangpin(ShuangpinConverter::new(layout));
    let z = texts(&e, "z");
    assert!(
        z.contains(&"在".to_string()),
        "zh 字挤满名额时 z 字仍应出现: {z:?}"
    );
}
