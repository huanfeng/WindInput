use super::*;
use wind_config::Config;

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

struct Fx {
    base: PathBuf,
    m: EngineManager,
}
impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

const PY: &str = "rime_pinyin";
const CT: &str = "rime_codetable";

/// 拼音式主库：本文件 3 条 + 子表 a（2 条，它自己又 import 了 c 与 a）+ 子表 b（2 条）
/// + 声明了但不存在的 gone。码表式 `ct/main`：本文件 2 条，也写了 import_tables。
fn fixture(tag: &str) -> Fx {
    let base = std::env::temp_dir().join(format!("wind_dict_view_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let s = base.join("schemas");
    write(
        &s.join("py.schema.toml"),
        "[schema]\nid = \"py\"\n[engine]\ntype = \"pinyin\"\n\
             [[dictionaries]]\nid = \"main\"\npath = \"py/main.dict.yaml\"\ntype = \"rime_pinyin\"\ndefault = true\n",
    );
    write(
        &s.join("py/main.dict.yaml"),
        "# Rime dictionary\n# encoding: utf-8\n#\n# 测试主库\n# 作者: 某人\n\n---\nname: main\n\
             version: \"2024-01-02\"\nsort: by_weight\nimport_tables:\n  - sub/a  # 甲表\n  - sub/gone\n  - sub/b\n...\n\
             大\tda\t10\n# 注释\n小\txiao\t5\n\n大小\tda xiao\t3\n",
    );
    write(
        &s.join("py/sub/a.dict.yaml"),
        "---\nname: a\nimport_tables:\n  - c\n  - a\n...\n你好\tni hao\t100\n换\\n行\thuan hang\t1\n",
    );
    write(
        &s.join("py/sub/c.dict.yaml"),
        "---\nname: c\n...\n嵌套\tqian tao\t1\n",
    );
    write(
        &s.join("py/sub/b.dict.yaml"),
        "\u{feff}---\nname: b\ncolumns: [code, text]\n...\nnh\t你会\nzz\t字\n",
    );
    write(
        &s.join("ct/main.dict.yaml"),
        "---\nname: ct\nimport_tables:\n  - ext\n...\naa\t工\t9\nbb\t子\t5\n",
    );
    write(
        &s.join("ct/ext.dict.yaml"),
        "---\nname: ext\n...\ncc\t又\t1\n",
    );
    let mut cfg = Config::default();
    cfg.schema.available = vec!["py".into()];
    cfg.schema.active = "py".into();
    let m = EngineManager::new(&cfg, Some(&base));
    Fx { base, m }
}

fn texts(p: &DictEntriesPage) -> Vec<&str> {
    p.rows.iter().map(|r| r.text.as_str()).collect()
}

fn eq(q: &str, mode: EntryMatch) -> EntryQuery {
    EntryQuery {
        query: q.into(),
        mode,
        ..Default::default()
    }
}

fn sorted(q: &str, sort: EntrySort, desc: bool) -> EntryQuery {
    EntryQuery {
        query: q.into(),
        sort: Some(sort),
        desc,
        ..Default::default()
    }
}

const MAIN: &str = "py/main.dict.yaml";

/// 与加载器同口径：拼音库展开子表一层（不进子表自己的 import_tables、缺的跳过），码表库
/// 不展开。
#[test]
fn count_expands_one_level_for_pinyin_only() {
    let f = fixture("count");
    assert_eq!(
        f.m.dict_file_entries(MAIN, PY),
        Some(7),
        "3 + a 2 + b 2，不含 c"
    );
    assert_eq!(f.m.dict_file_entries("py/sub/b.dict.yaml", PY), Some(2));
    assert_eq!(
        f.m.dict_file_entries("ct/main.dict.yaml", CT),
        Some(2),
        "码表库的 import_tables 加载器不读"
    );
    assert_eq!(f.m.dict_file_entries("py/none.dict.yaml", PY), None);
}

#[test]
fn header_lists_declared_imports_but_only_loaded_tables() {
    let f = fixture("header");
    let h = f.m.dict_header(MAIN, PY).unwrap();
    assert_eq!(h.name.as_deref(), Some("main"));
    assert_eq!(h.version.as_deref(), Some("2024-01-02"));
    assert_eq!(h.sort.as_deref(), Some("by_weight"));
    assert_eq!(
        h.import_tables,
        ["sub/a", "sub/gone", "sub/b"],
        "声明原样列"
    );
    assert!(h.columns.is_empty());
    assert_eq!(h.notes.as_deref(), Some("测试主库\n作者: 某人"));
    assert_eq!(h.count, Some(7));
    assert!(h.path.ends_with("py/main.dict.yaml"));
    assert!(h.mtime_ms.is_some());
    let t: Vec<(&str, Option<u64>)> = h
        .tables
        .iter()
        .map(|t| (t.name.as_str(), t.count))
        .collect();
    assert_eq!(t, [("sub/a", Some(2)), ("sub/b", Some(2))], "缺的不列");
    let c = f.m.dict_header("ct/main.dict.yaml", CT).unwrap();
    assert_eq!(
        (c.import_tables.len(), c.tables.len(), c.count),
        (1, 0, Some(2))
    );
    let b = f.m.dict_header("py/sub/b.dict.yaml", PY).unwrap();
    assert_eq!(b.columns, ["code", "text"]);
    assert_eq!(b.notes, None);
    assert!(f.m.dict_header("py/none.dict.yaml", PY).is_err());
}

#[test]
fn browse_walks_tables_in_order_with_columns_and_unescape() {
    let f = fixture("browse");
    let all =
        f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 0, 100)
            .unwrap();
    assert_eq!(all.total, 7);
    assert_eq!(
        texts(&all),
        ["大", "小", "大小", "你好", "换\n行", "你会", "字"]
    );
    assert_eq!(
        all.rows[2],
        DictEntryRow {
            code: "da xiao".into(),
            text: "大小".into(),
            weight: 3,
            table: "main".into()
        }
    );
    assert_eq!(all.rows[3].table, "sub/a");
    assert_eq!(
        (
            all.rows[5].code.as_str(),
            all.rows[5].weight,
            all.rows[5].table.as_str()
        ),
        ("nh", 0, "sub/b"),
        "columns: [code, text]：无权重列给 0，BOM 不影响"
    );
    assert_eq!(all.note, None);
    let p =
        f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 2, 3)
            .unwrap();
    assert_eq!(texts(&p), ["大小", "你好", "换\n行"], "跨表的页");
    let p =
        f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 6, 10)
            .unwrap();
    assert_eq!((p.total, texts(&p)), (7, vec!["字"]));
    let p =
        f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 99, 10)
            .unwrap();
    assert!(p.rows.is_empty());
    let p =
        f.m.dict_entries(MAIN, PY, &eq("", EntryMatch::Code), 0, 0)
            .unwrap();
    assert_eq!(p.rows.len(), 1, "limit 0 夹成 1");
    let c =
        f.m.dict_entries("ct/main.dict.yaml", CT, &eq("", EntryMatch::Code), 0, 9)
            .unwrap();
    assert_eq!(texts(&c), ["工", "子"], "码表库不混入 import_tables 的子表");
}

#[test]
fn search_by_code_prefix_and_text_substring() {
    let f = fixture("search");
    let p =
        f.m.dict_entries(MAIN, PY, &eq("DA", EntryMatch::Code), 0, 100)
            .unwrap();
    assert_eq!(texts(&p), ["大", "大小"], "前缀、忽略大小写");
    let p =
        f.m.dict_entries(MAIN, PY, &eq("da x", EntryMatch::Code), 0, 100)
            .unwrap();
    assert_eq!(texts(&p), ["大小"], "跨音节空格");
    let p =
        f.m.dict_entries(MAIN, PY, &eq("n", EntryMatch::Code), 0, 100)
            .unwrap();
    assert_eq!(texts(&p), ["你好", "你会"], "跨表");
    let p =
        f.m.dict_entries(MAIN, PY, &eq("n", EntryMatch::Code), 1, 100)
            .unwrap();
    assert_eq!((p.total, texts(&p)), (2, vec!["你会"]), "命中结果分页");
    let p =
        f.m.dict_entries(MAIN, PY, &eq("大", EntryMatch::Text), 0, 100)
            .unwrap();
    assert_eq!(texts(&p), ["大", "大小"]);
    let p =
        f.m.dict_entries(MAIN, PY, &eq("换\n", EntryMatch::Text), 0, 100)
            .unwrap();
    assert_eq!(texts(&p), ["换\n行"], "按反转义后的文本匹配");
    let p =
        f.m.dict_entries(MAIN, PY, &eq("da", EntryMatch::Text), 0, 100)
            .unwrap();
    assert_eq!(p.total, 0, "文本模式不看编码列");
    let p =
        f.m.dict_entries(MAIN, PY, &eq("qian", EntryMatch::Code), 0, 100)
            .unwrap();
    assert_eq!(p.total, 0, "子表自己的 import_tables 不展开");
}

/// 不指定方式时编码前缀与词子串取并。
#[test]
fn default_mode_matches_code_prefix_or_text_substring() {
    let f = fixture("any");
    assert_eq!(EntryQuery::default().mode, EntryMatch::Any);
    let get = |q: &str| {
        f.m.dict_entries(MAIN, PY, &eq(q, EntryMatch::Any), 0, 100)
            .unwrap()
    };
    assert_eq!(texts(&get("大")), ["大", "大小"], "词子串");
    assert_eq!(
        texts(&get("X")),
        ["小"],
        "编码前缀、忽略大小写：大小 的 da xiao 不以 x 打头"
    );
    assert_eq!(texts(&get("da")), ["大", "大小"], "编码前缀");
    assert_eq!(texts(&get("你")), ["你好", "你会"], "跨表");
    assert_eq!(texts(&get("  ")), texts(&get("")), "只有空白 = 不过滤");
}

/// 打字往后加字：在上一组命中里复核，结果与整扫一致。
#[test]
fn extending_a_query_narrows_the_previous_hits() {
    let f = fixture("narrow");
    let get = |q: &str| {
        f.m.dict_entries(MAIN, PY, &eq(q, EntryMatch::Any), 0, 100)
            .unwrap()
    };
    assert_eq!(texts(&get("n")), ["你好", "你会"]);
    assert_eq!(texts(&get("ni")), ["你好"]);
    assert_eq!(texts(&get("nih")), ["你好"]);
    assert_eq!(get("nix").total, 0);
    assert_eq!(texts(&get("n")), ["你好", "你会"], "退格回到缓存");
    let s = |q: &str| {
        f.m.dict_entries(MAIN, PY, &sorted(q, EntrySort::Weight, true), 0, 100)
            .unwrap()
    };
    assert_eq!(texts(&s("d")), ["大", "大小"]);
    assert_eq!(texts(&s("da")), ["大", "大小"], "复核保留排好的先后");
    // 缓存是进程级的，并行用例可能把上一组挤掉：直接比对复核与整扫
    let idx = f.m.dict_tables(MAIN, PY).unwrap();
    for (old, new, sort) in [
        ("n", "ni", None),
        ("d", "da x", Some(EntrySort::Text)),
        ("大", "大小", Some(EntrySort::Weight)),
    ] {
        let base = scan_hits(&idx, Some(&Needle::new(old, EntryMatch::Any)), sort, false).unwrap();
        let want = scan_hits(&idx, Some(&Needle::new(new, EntryMatch::Any)), sort, false).unwrap();
        let got = refine_hits(&idx, &base, Some(&Needle::new(new, EntryMatch::Any))).unwrap();
        assert!(!want.is_empty() && got == want, "{old} → {new}");
    }
}

/// 三列各自升降序、分页，以及搜索结果内排序。同键按文件顺序。
#[test]
fn sorts_by_each_column_and_pages() {
    let f = fixture("sort");
    let get =
        |q: EntryQuery, off: u64, lim: usize| f.m.dict_entries(MAIN, PY, &q, off, lim).unwrap();
    let w = get(sorted("", EntrySort::Weight, false), 0, 100);
    assert_eq!(w.total, 7);
    assert_eq!(
        texts(&w),
        ["你会", "字", "换\n行", "大小", "小", "大", "你好"],
        "权重升序，你会 / 字 同为 0 按文件顺序"
    );
    assert_eq!(
        texts(&get(sorted("", EntrySort::Weight, true), 0, 3)),
        ["你好", "大", "小"]
    );
    assert_eq!(
        texts(&get(sorted("", EntrySort::Weight, true), 5, 100)),
        ["你会", "字"],
        "降序时同键也按文件顺序"
    );
    assert_eq!(
        texts(&get(sorted("", EntrySort::Weight, false), 2, 3)),
        ["换\n行", "大小", "小"],
        "排序后分页"
    );
    assert_eq!(
        texts(&get(sorted("", EntrySort::Code, false), 0, 100)),
        ["大", "大小", "换\n行", "你会", "你好", "小", "字"],
        "编码按拼平后比：da < daxiao < huanhang < nh < nihao"
    );
    assert_eq!(
        texts(&get(sorted("", EntrySort::Text, false), 0, 100)),
        ["你会", "你好", "大", "大小", "字", "小", "换\n行"]
    );
    assert_eq!(
        texts(&get(sorted("", EntrySort::Text, true), 5, 100)),
        ["你好", "你会"],
        "降序的末页"
    );
    let s = get(sorted("da", EntrySort::Weight, false), 0, 100);
    assert_eq!((s.total, texts(&s)), (2, vec!["大小", "大"]), "命中内排序");
    let p = get(sorted("", EntrySort::Weight, false), 99, 10);
    assert!(p.rows.is_empty() && p.total == 7, "越界");
}

/// 排序结果按文件戳缓存：文件一变就重排。
#[test]
fn sort_cache_follows_file_changes() {
    let f = fixture("sortchange");
    let rel = "ct/main.dict.yaml";
    let get = || {
        f.m.dict_entries(rel, CT, &sorted("", EntrySort::Weight, true), 0, 10)
            .unwrap()
    };
    assert_eq!(texts(&get()), ["工", "子"]);
    assert_eq!(texts(&get()), ["工", "子"], "第二次走缓存");
    write(
        &f.base.join("schemas").join(rel),
        "---\nname: ct\n...\naa\t工\t1\nbb\t子\t5\ncc\t又\t9\n",
    );
    assert_eq!(texts(&get()), ["又", "子", "工"]);
}

fn hk(query: &str) -> HitsKey {
    HitsKey {
        tables: vec![(PathBuf::from("t"), (1, None))],
        lowercase: false,
        query: query.into(),
        mode: EntryMatch::Any,
        sort: None,
        desc: false,
    }
}

/// 单组超过行数上限：不缓存，也不挤掉已有的组。
#[test]
fn oversized_hits_are_not_cached() {
    let mut c = HitsCache::default();
    let now = Instant::now();
    c.put(hk("a"), Arc::new(vec![0; 10]), now);
    c.put(hk("b"), Arc::new(vec![0; HITS_CACHE_ROWS + 1]), now);
    assert!(c.get(&hk("b"), now).is_none());
    assert!(c.get(&hk("a"), now).is_some(), "已有的组还在");
    for q in ["c", "d", "e", "f"] {
        c.put(hk(q), Arc::new(vec![0; 10]), now);
    }
    assert_eq!(c.entries.len(), HITS_CACHE_SLOTS);
    assert!(c.get(&hk("a"), now).is_none(), "组数超了淘汰最久没用的");
}

/// 闲置超过时限，下次取放时整个清空；期间有访问就续期。
#[test]
fn hits_cache_clears_after_idle() {
    let mut c = HitsCache::default();
    let t0 = Instant::now();
    c.put(hk("a"), Arc::new(vec![1]), t0);
    let t1 = t0 + HITS_CACHE_IDLE - Duration::from_secs(1);
    assert!(c.get(&hk("a"), t1).is_some());
    let t2 = t1 + HITS_CACHE_IDLE - Duration::from_secs(1);
    assert!(c.get(&hk("a"), t2).is_some(), "访问续期");
    let t3 = t2 + HITS_CACHE_IDLE + Duration::from_secs(1);
    assert!(c.base(&hk("ab"), t3).is_none(), "闲置过期清空");
    assert_eq!(c.rows(), 0);
}

/// 英文库编码按小写读、码表按原样读：同一文件两种口径的排序结果各缓存一份，不串。
#[test]
fn sort_cache_keeps_lowercase_apart() {
    let f = fixture("sortcase");
    let rel = "ct/case.dict.yaml";
    write(
        &f.base.join("schemas").join(rel),
        "---\nname: case\n...\n乙\tB\t1\n甲\ta\t1\n",
    );
    let get = |ty: &str| {
        f.m.dict_entries(rel, ty, &sorted("", EntrySort::Code, false), 0, 10)
            .unwrap()
    };
    assert_eq!(texts(&get(CT)), ["乙", "甲"], "B < a");
    assert_eq!(texts(&get("english")), ["甲", "乙"], "a < b");
    assert_eq!(texts(&get(CT)), ["乙", "甲"]);
}

#[test]
fn search_follows_file_changes() {
    let f = fixture("change");
    let rel = "py/sub/b.dict.yaml";
    assert_eq!(
        f.m.dict_entries(rel, PY, &eq("z", EntryMatch::Code), 0, 10)
            .unwrap()
            .total,
        1
    );
    write(
        &f.base.join("schemas/py/sub/b.dict.yaml"),
        "---\nname: b\ncolumns: [code, text]\n...\nzz\t字\nzy\t自\nzx\t在\n",
    );
    assert_eq!(
        f.m.dict_entries(rel, PY, &eq("z", EntryMatch::Code), 0, 10)
            .unwrap()
            .total,
        3
    );
    assert_eq!(f.m.dict_file_entries(rel, PY), Some(3));
}

/// 加载器丢的行这里也不算：列数不足、编码或词为空；`$SS(` 落在编码列的行纠正列序。
#[test]
fn rows_the_loader_drops_are_not_counted() {
    let f = fixture("lines");
    let rel = "ct/lines.dict.yaml";
    write(
        &f.base.join("schemas").join(rel),
        "---\nname: l\ncolumns: [text, code, weight]\n...\n甲\ta\t1\n只有一列\n\tb\t1\n乙\t\t1\n\
             zzgo\t$SS(gogo)\t1\n丙\tc\t2\n",
    );
    assert_eq!(f.m.dict_file_entries(rel, CT), Some(3));
    let p =
        f.m.dict_entries(rel, CT, &eq("", EntryMatch::Code), 0, 10)
            .unwrap();
    assert_eq!(texts(&p), ["甲", "$SS(gogo)", "丙"]);
    assert_eq!(p.rows[1].code, "zzgo", "列序按行纠正");
    let q =
        f.m.dict_entries(rel, CT, &eq("zzg", EntryMatch::Code), 0, 10)
            .unwrap();
    assert_eq!(texts(&q), ["$SS(gogo)"], "纠正列序的行按纠正后的编码搜得到");
}

/// `columns:` 声明缺 code 列：加载器整库跳过 ⇒ 条数为空、词条空并给说明。
#[test]
fn broken_columns_declaration_is_not_loaded() {
    let f = fixture("cols");
    let rel = "ct/bad.dict.yaml";
    write(
        &f.base.join("schemas").join(rel),
        "---\nname: bad\ncolumns:\n  - text\n  - weight\n...\n甲\t1\n",
    );
    assert_eq!(f.m.dict_file_entries(rel, CT), None);
    let p =
        f.m.dict_entries(rel, CT, &eq("", EntryMatch::Code), 0, 10)
            .unwrap();
    assert_eq!(p.total, 0);
    assert!(
        p.note.as_deref().unwrap().contains("columns"),
        "{:?}",
        p.note
    );
    assert_eq!(f.m.dict_header(rel, CT).unwrap().count, None);
}

/// 多于一个检查点：任意偏移的页都与整读切片一致。
#[test]
fn pages_across_checkpoints_match_full_read() {
    let f = fixture("stride");
    let mut body = String::from("---\nname: big\n...\n");
    for i in 0..1000 {
        if i % 97 == 0 {
            body.push_str("# 注释\n\n");
        }
        body.push_str(&format!("词{i}\tc{i}\t{i}\r\n"));
    }
    write(&f.base.join("schemas/py/big.dict.yaml"), &body);
    let rel = "py/big.dict.yaml";
    let all =
        f.m.dict_entries(rel, PY, &eq("", EntryMatch::Code), 0, 500)
            .unwrap();
    assert_eq!(all.total, 1000);
    for off in [0u64, 255, 256, 257, 511, 700, 999] {
        let p =
            f.m.dict_entries(rel, PY, &eq("", EntryMatch::Code), off, 7)
                .unwrap();
        let want: Vec<String> = (off..(off + 7).min(1000))
            .map(|i| format!("词{i}"))
            .collect();
        assert_eq!(texts(&p), want, "offset {off}");
        assert_eq!(p.rows[0].weight as u64, off);
    }
}

/// 孤立 `\r` 换行的文件：偏移按规范化文本算，整读后照样分页。
#[test]
fn lone_cr_file_pages() {
    let f = fixture("cr");
    write(
        &f.base.join("schemas/py/cr.dict.yaml"),
        "---\rname: cr\r...\r甲\ta\r乙\tb\r丙\tc\r",
    );
    let p =
        f.m.dict_entries("py/cr.dict.yaml", PY, &eq("", EntryMatch::Code), 1, 5)
            .unwrap();
    assert_eq!((p.total, texts(&p)), (3, vec!["乙", "丙"]));
}

fn order_of(toml_text: &str) -> Vec<(String, bool)> {
    let schema: Schema = toml::from_str(toml_text).unwrap();
    EngineManager::ordered_system_dicts(&schema)
        .into_iter()
        .map(|(d, m)| (d.id.clone(), m))
        .collect()
}

/// 码表：按 base_order 稳定排序，主库也按自己的 base_order 排（可以不在首）。
#[test]
fn codetable_order_follows_base_order_even_for_main() {
    let ids = order_of(
        "[engine]\ntype = \"codetable\"\n\
             [[dictionaries]]\nid = \"x2\"\npath = \"x2\"\nbase_order = 2\n\
             [[dictionaries]]\nid = \"m\"\npath = \"m\"\ndefault = true\nbase_order = 5\n\
             [[dictionaries]]\nid = \"x1a\"\npath = \"x1a\"\nbase_order = 1\n\
             [[dictionaries]]\nid = \"nopath\"\n\
             [[dictionaries]]\nid = \"user:我的\"\npath = \"/abs/u.yaml\"\nbase_order = -3\n\
             [[dictionaries]]\nid = \"x1b\"\npath = \"x1b\"\nbase_order = 1\n",
    );
    let ids: Vec<(&str, bool)> = ids.iter().map(|(i, m)| (i.as_str(), *m)).collect();
    assert_eq!(
        ids,
        [("x1a", false), ("x1b", false), ("x2", false), ("m", true)]
    );
}

/// 拼音：按方案文件顺序叠层，base_order 不起作用。
#[test]
fn pinyin_order_is_file_order_not_base_order() {
    let ids = order_of(
        "[engine]\ntype = \"pinyin\"\n\
             [[dictionaries]]\nid = \"m\"\npath = \"m\"\ntype = \"rime_pinyin\"\ndefault = true\nbase_order = 0\n\
             [[dictionaries]]\nid = \"late\"\npath = \"a\"\ntype = \"rime_pinyin\"\nbase_order = 9\n\
             [[dictionaries]]\nid = \"early\"\npath = \"b\"\ntype = \"rime_pinyin\"\nbase_order = 1\n",
    );
    let ids: Vec<(&str, bool)> = ids.iter().map(|(i, m)| (i.as_str(), *m)).collect();
    assert_eq!(ids, [("m", true), ("late", false), ("early", false)]);
}

/// 真实拼音主库的翻页 / 搜索耗时（要 build_dev/data）：
/// `cargo test -p wind-engine --release -- --ignored real_pinyin_entries_timing --nocapture`。
#[test]
#[ignore = "需 build_dev/data"]
fn real_pinyin_entries_timing() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    assert!(
        data.join("schemas/pinyin.schema.toml").exists(),
        "缺 build_dev/data"
    );
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    let m = EngineManager::new(&cfg, Some(&data));
    let rel = "pinyin/rime_frost.dict.yaml";
    let t = std::time::Instant::now();
    let n = m.dict_file_entries(rel, PY).unwrap();
    eprintln!("首次计数（建索引）{n} 条: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let n2 = m.dict_file_entries(rel, PY).unwrap();
    eprintln!("再次计数（命中缓存）{n2} 条: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let p = m
        .dict_entries(rel, PY, &eq("", EntryMatch::Code), 0, 100)
        .unwrap();
    eprintln!("首页: {:?} ({} 行)", t.elapsed(), p.rows.len());
    for off in [n / 2, n - 100] {
        let t = std::time::Instant::now();
        let p = m
            .dict_entries(rel, PY, &eq("", EntryMatch::Code), off, 100)
            .unwrap();
        eprintln!("第 {off} 条起一页: {:?} ({} 行)", t.elapsed(), p.rows.len());
    }
    for (q, mode) in [("ni", EntryMatch::Code), ("中国", EntryMatch::Text)] {
        let t = std::time::Instant::now();
        let p = m.dict_entries(rel, PY, &eq(q, mode), 0, 100).unwrap();
        eprintln!("搜索 {q:?} 首页: {:?}，命中 {}", t.elapsed(), p.total);
        let t = std::time::Instant::now();
        let _ = m
            .dict_entries(rel, PY, &eq(q, mode), p.total / 2, 100)
            .unwrap();
        eprintln!("搜索 {q:?} 翻到中间: {:?}", t.elapsed());
    }
    for (sort, name) in [
        (EntrySort::Weight, "权重"),
        (EntrySort::Code, "编码"),
        (EntrySort::Text, "词条"),
    ] {
        let t = std::time::Instant::now();
        let p = m
            .dict_entries(rel, PY, &sorted("", sort, true), 0, 100)
            .unwrap();
        eprintln!(
            "整库按{name}排序首页（首次）: {:?}，{} 条",
            t.elapsed(),
            p.total
        );
        let t = std::time::Instant::now();
        let _ = m
            .dict_entries(rel, PY, &sorted("", sort, true), p.total / 2, 100)
            .unwrap();
        eprintln!("整库按{name}排序翻到中间（缓存）: {:?}", t.elapsed());
    }
    eprintln!(
        "命中缓存 {} 行（{} 字节）",
        cached_hit_rows(),
        cached_hit_rows() * 8
    );
    for q in ["z", "zh", "zho", "zhon", "zhong", "中"] {
        let t = std::time::Instant::now();
        let p = m
            .dict_entries(rel, PY, &eq(q, EntryMatch::Any), 0, 100)
            .unwrap();
        eprintln!("逐键搜索 {q:?}: {:?}，命中 {}", t.elapsed(), p.total);
    }
    let cps: usize = m
        .dict_tables(rel, PY)
        .unwrap()
        .iter()
        .map(|(_, _, i)| i.checkpoints.len())
        .sum();
    eprintln!("检查点 {cps} 个（{} 字节）", cps * 8);
    assert!(n > 600_000);
}
