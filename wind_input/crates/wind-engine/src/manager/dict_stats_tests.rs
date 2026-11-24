use super::super::dict_view::count_dict_entries;
use super::*;

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// 夹具数据根：码表方案（主库 + 启用扩展 + 停用扩展）、拼音方案、字频字表、通规字表。
fn fixture(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("wind_dict_stats_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let s = base.join("schemas");
    write(
        &s.join("ct.schema.toml"),
        "[schema]\nid = \"ct\"\nname = \"测码\"\n[engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 4\n\
             [[dictionaries]]\nid = \"ct_main\"\nlabel = \"主表\"\npath = \"ct/main.dict.yaml\"\ndefault = true\n\
             [[dictionaries]]\nid = \"ct_ext\"\npath = \"ct/ext.dict.yaml\"\ndefault_enabled = true\nbase_order = 1\n\
             [[dictionaries]]\nid = \"ct_off\"\nlabel = \"关着的\"\npath = \"ct/off.dict.yaml\"\ndefault_enabled = false\n",
    );
    let head = "---\nname: x\nversion: \"1\"\n...\n";
    write(
        &s.join("ct/main.dict.yaml"),
        &format!(
            "{head}a\t工\t100\naaaa\t工\t50\naaaa\t式\t80\naaab\t工作\t70\nbbbb\t子\t40\nbbbb\t孑\t30\n"
        ),
    );
    write(
        &s.join("ct/ext.dict.yaml"),
        &format!("{head}aaaa\t戒\t60\nff\t😀\t1\n"),
    );
    write(&s.join("ct/off.dict.yaml"), &format!("{head}zz\t乙\t1\n"));
    write(
        &s.join("pinyin/cn_dicts/8105.dict.yaml"),
        &format!("{head}工\tgong\t100\n式\tshi\t50\n子\tzi\t30\n戒\tjie\t10\n孑\tjie\t5\n"),
    );
    write(
        &base.join("charsets/common_han.yaml"),
        "key: common_han\n...\n# 一级\n工\n式\n子\n# 二级\n戒\n孑\n丢\n# 三级\n乙\n",
    );
    base
}

fn mgr(base: &Path) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["ct".into()];
    cfg.schema.active = "ct".into();
    EngineManager::new(&cfg, Some(base))
}

#[test]
fn codetable_fixture_counts_layers_and_weighted_metrics() {
    let base = fixture("ct");
    let m = mgr(&base);
    let (s, cached) = m.dict_stats("ct", 10, false).unwrap();
    assert!(!cached);
    assert_eq!(s.engine_type, "codetable");
    assert_eq!(s.schema_name, "测码");
    let rows: Vec<(&str, &str, &str, bool, Option<u64>)> = s
        .layers
        .iter()
        .map(|l| {
            (
                l.id.as_str(),
                l.label.as_str(),
                l.kind,
                l.enabled,
                l.counts.as_ref().map(|c| c.entries),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("ct_main", "主表", "main", true, Some(6)),
            ("ct_ext", "ext", "extra", true, Some(2)),
            ("ct_off", "关着的", "extra", false, None),
        ]
    );
    assert_eq!(s.total.entries, 8);
    assert_eq!(s.total.other, 1);
    // aaaa：式 80 > 戒 60 > 工 50；bbbb：子 40 > 孑 30
    assert_eq!(
        (s.dup.groups, s.dup.max, s.dup.max_code.as_str()),
        (2, 3, "aaaa")
    );
    assert_eq!(s.dup.top[0].texts, ["式", "戒", "工"]);
    assert_eq!(s.freq_source.as_deref(), Some("白霜拼音字表（8105）"));

    let one = &s.charsets[0]; // 一级：工 式 子
    assert_eq!(
        (one.key.as_str(), one.size, one.missing),
        ("common_han_1", 3, 0)
    );
    let two = &s.charsets[1]; // 一二级：+ 戒 孑 丢
    assert_eq!((two.size, two.missing, two.static_dup), (6, 1, Some(3)));
    // 非首选：戒 10（aaaa 第 2）+ 孑 5（bbbb 第 2）；工 走一简 a 首选
    assert_eq!(two.dyn_rate, Some(15.0 / 195.0));
    // 工 1 码、式/子/戒/孑 4 码
    assert_eq!(two.avg_code_len, Some((100.0 + 4.0 * 95.0) / 195.0));
    assert_eq!(s.charsets[2].missing_chars, ["丢", "乙"]);

    let _ = std::fs::remove_dir_all(&base);
}

/// 停用的库也数得出；文件变了重数；文件不在给 None。
#[test]
fn file_entries_count_disabled_dicts_and_follow_changes() {
    let base = fixture("entries");
    let m = mgr(&base);
    assert_eq!(
        m.dict_file_entries("ct/main.dict.yaml", "rime_codetable"),
        Some(6)
    );
    assert_eq!(
        m.dict_file_entries("ct/off.dict.yaml", "rime_codetable"),
        Some(1),
        "停用库"
    );
    write(
        &base.join("schemas/ct/off.dict.yaml"),
        "---\nname: x\n...\nzz\t乙\t1\nzy\t丙\t1\n",
    );
    assert_eq!(
        m.dict_file_entries("ct/off.dict.yaml", "rime_codetable"),
        Some(2),
        "改了重数"
    );
    assert_eq!(
        m.dict_file_entries("ct/none.dict.yaml", "rime_codetable"),
        None
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 计数口径与加载器一致：BOM 不影响找 `...`，没有 `...` 就没有正文，`# no comment` 之后
/// 的 `#` 行是数据。
#[test]
fn count_follows_the_loader_layout() {
    assert_eq!(
        count_dict_entries("\u{feff}---\nname: x\n...\n甲\ta\n"),
        1,
        "BOM"
    );
    assert_eq!(count_dict_entries("甲\ta\n乙\tb\n"), 0, "没有 ... 即零条");
    assert_eq!(
        count_dict_entries("---\n...\n# 注释\n甲\ta\n# no comment\n#\tjing\n"),
        2,
        "# no comment 之后的 # 行算数据，指令行本身不算"
    );
}

/// 码表库写了 import_tables：加载器不展开（只 rime_pinyin 展开），条数只算本文件。
#[test]
fn codetable_wrapper_imports_are_not_counted() {
    let base = fixture("wrapper");
    write(
        &base.join("schemas/ct/wrap.dict.yaml"),
        "---\nname: wrap\nimport_tables:\n  - off\n  - ext\n...\n",
    );
    let m = mgr(&base);
    assert_eq!(
        m.dict_file_entries("ct/wrap.dict.yaml", "rime_codetable"),
        Some(0)
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn count_skips_head_comments_and_blank_lines() {
    let text = "# Rime dictionary\n\n---\nname: x\n# 头里的注释\n...\n甲\ta\n\n# 注释\n乙\tb\t1\n";
    assert_eq!(count_dict_entries(text), 2);
}

#[test]
fn cache_hits_until_a_dict_changes_and_refresh_bypasses() {
    let base = fixture("cache");
    let m = mgr(&base);
    let (a, _) = m.dict_stats("ct", 10, false).unwrap();
    let (b, hit) = m.dict_stats("ct", 10, false).unwrap();
    assert!(hit && Arc::ptr_eq(&a, &b));
    let (_, hit) = m.dict_stats("ct", 10, true).unwrap();
    assert!(!hit, "refresh 绕过缓存");
    let (_, hit) = m.dict_stats("ct", 5, false).unwrap();
    assert!(!hit, "Top 条数不同不能复用");

    // 改扩展库：重建 wdat，摘要随之变化
    write(
        &base.join("schemas/ct/ext.dict.yaml"),
        "---\nname: x\nversion: \"2\"\n...\naaaa\t戒\t60\nff\t😀\t1\nzz\t丁\t1\n",
    );
    let (c, hit) = m.dict_stats("ct", 5, false).unwrap();
    assert!(!hit, "词库内容变了必须重算");
    assert_eq!(c.total.entries, 9);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn missing_freq_source_hides_weighted_metrics_and_mixed_is_rejected() {
    let base = fixture("nofreq");
    std::fs::remove_file(base.join("schemas/pinyin/cn_dicts/8105.dict.yaml")).unwrap();
    write(
        &base.join("schemas/mx.schema.toml"),
        "[schema]\nid = \"mx\"\n[engine]\ntype = \"mixed\"\n",
    );
    let m = mgr(&base);
    let (s, _) = m.dict_stats("ct", 10, false).unwrap();
    assert_eq!(s.freq_source, None);
    assert!(
        s.charsets
            .iter()
            .all(|c| c.dyn_rate.is_none() && c.avg_code_len.is_none())
    );
    assert_eq!(s.charsets[1].static_dup, Some(3), "静重不依赖字频");
    assert!(m.dict_stats("mx", 10, false).is_err());
    assert!(m.dict_stats("nope", 10, false).is_err());
    let _ = std::fs::remove_dir_all(&base);
}

/// 真实出厂数据冒烟，数值与草案原型（Python 直接解析 yaml）对照。要 build_dev/data，
/// 故默认 ignore（缺数据时静默通过就是假绿）：`cargo test -p wind-engine -- --ignored real_data`。
#[test]
#[ignore = "需 build_dev/data"]
fn real_data_smoke_wubi86_and_pinyin() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    assert!(
        data.join("schemas/wubi86.schema.toml").exists(),
        "缺 build_dev/data：整份拷进来再跑"
    );
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    let m = EngineManager::new(&cfg, Some(&data));

    let (w, _) = m.dict_stats("wubi86", 50, true).unwrap();
    let main = w.layers.iter().find(|l| l.kind == "main").unwrap();
    eprintln!("wubi86 {}ms: {:?}", w.elapsed_ms, main);
    eprintln!("wubi86 total {:?}", w.total);
    eprintln!("wubi86 codeLen {:?} short {:?}", w.code_len, w.short_codes);
    eprintln!(
        "wubi86 dup groups={} entries={} max={} ({}) charFull={:?}",
        w.dup.groups, w.dup.entries, w.dup.max, w.dup.max_code, w.dup.char_full
    );
    for c in &w.charsets {
        eprintln!(
            "wubi86 {} size={} missing={} static={:?} groups={:?} dyn={:?} avg={:?}",
            c.label, c.size, c.missing, c.static_dup, c.dup_groups, c.dyn_rate, c.avg_code_len
        );
    }
    // 主库条目 = yaml 正文行数（各库分行不去重）。
    let yaml =
        std::fs::read_to_string(data.join("schemas/wubi86/wubi86_jidian.dict.yaml")).unwrap();
    let body = &yaml[wind_dict::rime_body_layout(&yaml).unwrap().0..];
    let rows = body
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#') && l.contains('\t'))
        .count() as u64;
    assert_eq!(main.counts.as_ref().unwrap().entries, rows);
    let sizes: Vec<u64> = w.charsets.iter().map(|c| c.size).collect();
    assert_eq!(sizes, [3500, 6500, 8105], "通规三级累进");
    assert_eq!(w.charsets[0].missing, 0, "五笔86 收全一级字");
    assert!(w.freq_source.is_some());
    let rate = w.charsets[2].dyn_rate.unwrap();
    assert!(rate > 0.0 && rate < 0.01, "{rate}");
    // 重码不计纯 emoji：最大组不再是 emoj，Top 成员都含汉字（数值以出厂词库为准）。
    assert_ne!(w.dup.max_code, "emoj");
    assert!(
        w.dup.top[0]
            .texts
            .iter()
            .all(|t| t.chars().any(wind_dict::stats::is_han)),
        "{:?}",
        w.dup.top[0]
    );
    assert_eq!((w.dup.max, w.dup.max_code.as_str()), (41, "cccc"));
    let md = w.dup.main.as_ref().unwrap();
    eprintln!("wubi86 main dup {md:?}");
    assert_eq!(md.max, 13, "主库最大重码");
    let cf = w.dup.char_full.as_ref().unwrap();
    assert_eq!(cf.top[0].size, cf.max, "单字全码 Top 与那一行同口径");

    let (p, _) = m.dict_stats("pinyin", 10, true).unwrap();
    eprintln!("pinyin {}ms total {:?}", p.elapsed_ms, p.total);
    eprintln!(
        "pinyin dup groups={} max={} ({}) extra={:?}",
        p.dup.groups, p.dup.max, p.dup.max_code, p.pinyin
    );
    for c in &p.charsets {
        eprintln!("pinyin {} missing={}", c.label, c.missing);
    }
    assert!(p.total.entries > 600_000);
    let syl = p.pinyin.as_ref().unwrap().syllables;
    assert!((400..=450).contains(&syl), "{syl}");
}
