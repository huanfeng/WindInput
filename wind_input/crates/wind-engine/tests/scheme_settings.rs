//! 方案设置按方案取值：全拼与双拼各设一份互不影响；混输、英文、码表自动造词同理。
//! 设计见 `docs/design/scheme-settings-flatten.md`。只读方案定义，用仓库自带的 `data/` 即可。

use std::path::PathBuf;

use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data")
}

fn mgr(tag: &str, active: &str, overrides: &[(&str, &str)]) -> EngineManager {
    let root = std::env::temp_dir().join(format!("wind_pinyin_scheme_settings_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let ov = root.join("schema_overrides");
    std::fs::create_dir_all(&ov).unwrap();
    for (id, text) in overrides {
        std::fs::write(ov.join(format!("{id}.toml")), text).unwrap();
    }
    let mut cfg = Config::default();
    cfg.schema.available = ["pinyin", "shuangpin", "wubi86", "wubi86_pinyin"]
        .map(String::from)
        .to_vec();
    cfg.schema.active = active.to_string();
    cfg.schema.pinyin.frequency.enabled = true;
    EngineManager::with_store_override(&cfg, Some(&data_dir()), None, Some(ov))
}

#[test]
fn full_and_shuangpin_hold_separate_values() {
    let m = mgr(
        "separate",
        "pinyin",
        &[(
            "shuangpin",
            "[engine.pinyin]\nsentence_count = 3\nsingle_char = true\n\
             [engine.pinyin.frequency]\nenabled = false\n",
        )],
    );
    let full = m.pinyin_settings_of("pinyin");
    let sp = m.pinyin_settings_of("shuangpin");
    assert_eq!(full.sentence_count, 1);
    assert!(!full.single_char);
    assert!(full.frequency.enabled);
    assert_eq!(sp.sentence_count, 3);
    assert!(sp.single_char);
    assert!(!sp.frequency.enabled);
    // 调频走的是另一条取值路径，同样要按方案分。
    assert!(m.freq_settings_for("pinyin").enabled);
    assert!(!m.freq_settings_for("shuangpin").enabled);
}

/// 「此刻起作用的拼音方案」：拼音方案是自己，混输是它的拼音成员，码表是主拼音方案。
#[test]
fn scheme_in_effect_follows_active_scheme() {
    let ov = [("shuangpin", "[engine.pinyin]\nsentence_count = 4\n")];
    assert_eq!(
        mgr("eff_sp", "shuangpin", &ov)
            .pinyin_settings()
            .sentence_count,
        4
    );
    assert_eq!(
        mgr("eff_py", "pinyin", &ov)
            .pinyin_settings()
            .sentence_count,
        1
    );
    assert_eq!(
        mgr("eff_mix", "wubi86_pinyin", &ov).pinyin_scheme_in_effect(),
        "pinyin"
    );
    assert_eq!(
        mgr("eff_ct", "wubi86", &ov).pinyin_scheme_in_effect(),
        "pinyin"
    );
}

/// 方案失效后重新读盘：设置页保存方案设置走的就是这条路。
#[test]
fn invalidate_picks_up_new_scheme_settings() {
    let m = mgr("invalidate", "pinyin", &[]);
    assert_eq!(m.pinyin_settings_of("pinyin").sentence_count, 1);
    let ov = std::env::temp_dir().join("wind_pinyin_scheme_settings_invalidate/schema_overrides");
    std::fs::write(
        ov.join("pinyin.toml"),
        "[engine.pinyin]\nsentence_count = 2\n",
    )
    .unwrap();
    m.invalidate_schema("pinyin");
    assert_eq!(m.pinyin_settings_of("pinyin").sentence_count, 2);
}

#[test]
fn mixed_english_and_auto_phrase_settings_come_from_their_scheme() {
    let m = mgr(
        "others",
        "wubi86",
        &[
            (
                "wubi86_pinyin",
                "[engine.mixed]\nmin_pinyin_length = 5\nenable_english = true\n",
            ),
            (
                "english",
                "[engine.english]\ncommit_space = true\n[engine.english.frequency]\nenabled = true\n",
            ),
            (
                "wubi86",
                "[engine.codetable.auto_phrase]\nenabled = true\nmax_phrase_len = 3\n",
            ),
        ],
    );
    let mix = m.mix_settings_of("wubi86_pinyin");
    assert_eq!(mix.min_pinyin_length, 5);
    assert!(mix.enable_english);
    // 方案文件里的成员声明不受设置项影响。
    assert_eq!(
        m.mixed_secondary_schema("wubi86_pinyin").as_deref(),
        Some("pinyin")
    );

    let en = m.english_settings();
    assert!(en.commit_space);
    assert!(m.freq_settings_for("english").enabled);

    let ap = m.codetable_settings().auto_phrase;
    assert!(ap.enabled);
    assert_eq!(ap.max_phrase_len, 3);
    // 没写的项保持基线。
    assert_eq!(
        ap.min_phrase_len,
        Config::default()
            .schema
            .codetable
            .auto_phrase
            .min_phrase_len
    );
    // 笔画方案没写，不受五笔的设置影响。
    assert!(!m.effective_codetable("stroke").auto_phrase.enabled);
}
