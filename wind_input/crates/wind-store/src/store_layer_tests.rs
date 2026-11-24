use super::*;
use wind_dict::CompositeDict;

fn store(name: &str) -> Arc<Store> {
    let p = std::env::temp_dir().join(format!("wind_storelayer_{name}.redb"));
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

/// 用户层 / 临时层回答通配查询：字面前缀扫描后过滤（不带 limit 进 redb——先截断再
/// 过滤会把匹配项整批截掉），等长档在前；草稿层恒不参与。
#[test]
fn store_layers_answer_pattern_queries_and_drafts_stay_out() {
    let s = store("pattern");
    s.add_user_word("wb", "ab", "甲", 10, 0).unwrap();
    s.add_user_word("wb", "ac", "乙", 20, 0).unwrap();
    s.add_user_word("wb", "abcd", "丙", 99, 0).unwrap();
    s.learn_temp_word("wb", "bb", "丁", 30, 0).unwrap();
    s.add_drafts("wb", &[("ad".to_string(), "草".to_string())])
        .unwrap();
    let slot = wind_dict::layer::WILDCARD_SLOT;
    let p = format!("a{slot}");

    let ul = StoreUserLayer::new(s.clone(), "wb");
    let got: Vec<(String, String, bool)> = ul
        .search_pattern(&p, slot, 10, true)
        .into_iter()
        .map(|c| (c.code, c.text, c.is_prefix))
        .collect();
    assert_eq!(
        got,
        [
            ("ac".to_string(), "乙".to_string(), false),
            ("ab".to_string(), "甲".to_string(), false),
            ("abcd".to_string(), "丙".to_string(), true),
        ]
    );
    let exact_only: Vec<String> = ul
        .search_pattern(&p, slot, 10, false)
        .into_iter()
        .map(|c| c.code)
        .collect();
    assert_eq!(exact_only, ["ac", "ab"]);
    assert_eq!(ul.search_pattern(&p, slot, 1, true).len(), 1, "limit 生效");

    let tl = StoreTempLayer::new(s.clone(), "wb");
    let lead: Vec<String> = tl
        .search_pattern(&format!("{slot}b"), slot, 10, false)
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert_eq!(lead, ["丁"], "首位通配：字面前缀为空，扫本方案全部临时词");

    let dl = StoreDraftLayer::new(s.clone(), "wb", 0);
    assert!(
        dl.search_pattern(&p, slot, 10, true).is_empty(),
        "草稿层不参与通配"
    );
}

/// 草稿绝不能进前缀召回：草稿层杂词率极高，参与前缀召回会让杂词在每一次按键上涌进候选。
/// 只精确命中是设计的一部分。判据钉在跨层合并的消费点 `CompositeDict::search_prefix`，
/// 防止层自己返回空、合并时却从别的路径捞回来。
#[test]
fn drafts_never_surface_through_prefix_queries() {
    let s = store("draft_prefix");
    s.add_drafts("wb", &[("wqvb".to_string(), "你好".to_string())])
        .unwrap();
    // 临时词做对照组：同样的前缀它必须照常召回，否则「前缀查询整个坏掉」也能变绿。
    s.learn_temp_word("wb", "wqvb", "拟好", 800, 0).unwrap();

    let composite = CompositeDict::new();
    composite.register_layer(Box::new(StoreDraftLayer::new(s.clone(), "wb", 0)));
    composite.register_layer(Box::new(StoreTempLayer::new(s.clone(), "wb")));

    let by_prefix = composite.search_prefix("wq", 50);
    let texts: Vec<&str> = by_prefix.iter().map(|c| c.text.as_str()).collect();
    assert!(
        texts.contains(&"拟好"),
        "对照组：临时词的前缀召回必须照常工作，否则本用例测不出东西：{texts:?}"
    );
    assert!(
        !texts.contains(&"你好"),
        "草稿从前缀查询里冒出来了 —— 每次按键都会涌进一堆杂词：{texts:?}"
    );
    assert!(
        by_prefix.iter().all(|c| !c.is_draft),
        "任何标着 is_draft 的候选都不该出现在前缀结果里"
    );

    // 打满完整码：草稿必须出得来，否则它永远没机会被用过、也就永远转不了正。
    let exact = composite.search("wqvb", 50);
    let texts: Vec<&str> = exact.iter().map(|c| c.text.as_str()).collect();
    assert!(
        texts.contains(&"你好"),
        "精确命中时草稿必须召回，这是它转正的唯一途径：{texts:?}"
    );
    assert!(
        exact.iter().any(|c| c.is_draft && c.text == "你好"),
        "草稿候选必须带上 is_draft 标记，否则排序层无从沉底"
    );
}

/// 边界必须穿过「记录 → 候选」这一层，不能在转候选时被 `..Default::default()` 吃掉
/// （否则双拼校验 / 长词上浮 / 自动造词沿用边界一并静默失效）。精确、前缀、临时层三条路径都要守。
#[test]
fn boundary_reaches_candidate_from_store() {
    let s = store("boundary_passthrough");
    // ni|hao → 起始 {0,2}；xi|an|ning → {0,2,4}
    s.add_user_word("pinyin", "nihao", "你好", 500, 0b101)
        .unwrap();
    s.learn_temp_word("pinyin", "xianning", "西安宁", 800, 0b10101)
        .unwrap();

    let ul = StoreUserLayer::new(s.clone(), "pinyin");
    assert_eq!(ul.search("nihao", 10)[0].boundary, 0b101, "精确查询");
    assert_eq!(
        ul.search_prefix("ni", 10)[0].boundary,
        0b101,
        "前缀补全（用户长词上浮判据吃这个值）"
    );

    let tl = StoreTempLayer::new(s.clone(), "pinyin");
    assert_eq!(tl.search("xianning", 10)[0].boundary, 0b10101, "临时层");

    // 无边界记录仍是 0（旧数据 / 手输码 → 消费方降级回 DAG），不得凭空造出边界
    s.add_user_word("pinyin", "abcd", "工作", 100, 0).unwrap();
    assert_eq!(ul.search("abcd", 10)[0].boundary, 0);
}

/// 简拼召回走索引，且仍经 merge_search 跨层合并：同一个词同时在用户层与临时层时只返回一条、
/// 权重取 max。
#[test]
fn abbrev_recall_goes_through_the_index_and_merges_across_layers() {
    let s = store("abbrev_recall");
    s.add_user_word("py", "nihao", "你好", 500, 0b101).unwrap();
    s.learn_temp_word("py", "nihao", "你好", 900, 0b101)
        .unwrap();
    // ni|hao|ma → 起始 {0,2,5}；zai|jian → 起始 {0,3}
    s.add_user_word("py", "nihaoma", "你好吗", 300, 0b100101)
        .unwrap();
    s.add_user_word("py", "zaijian", "再见", 700, 0b1001)
        .unwrap();

    let dm = wind_dict::DictManager::new();
    dm.register_layer(Box::new(StoreUserLayer::new(s.clone(), "py")));
    dm.register_layer(Box::new(StoreTempLayer::new(s.clone(), "py")));

    let nh = dm.search_abbrev("nh", 0);
    assert_eq!(nh.len(), 1, "nh 只该命中「你好」（nhm 是三音节，不同组）");
    assert_eq!(nh[0].text, "你好");
    assert_eq!(
        nh[0].weight, 900,
        "跨层同 text 须继承更高权重（临时层 900），与改用索引前一致"
    );
    assert_eq!(nh[0].code, "nihao", "保留全拼码，不得覆盖成简拼串");
    assert_eq!(nh[0].boundary, 0b101, "边界必须穿过来，双拼校验吃这个值");
    assert!(!nh[0].is_prefix, "简拼命中的是整词，不是前缀补全");

    assert_eq!(dm.search_abbrev("nhm", 0)[0].text, "你好吗");
    assert_eq!(dm.search_abbrev("zj", 0)[0].text, "再见");
    assert!(dm.search_abbrev("zg", 0).is_empty(), "无关声母串不该有产出");
}

/// 没有声母索引的层（系统词库层等）默认不产出，而非回退到全层枚举：
/// 召不回能在测试里立刻暴露，静默全表扫只会让简拼卡顿。
#[test]
fn layers_without_an_index_yield_nothing_rather_than_scanning() {
    use wind_dict::{DictLayer, LayerType};
    struct Bare;
    impl DictLayer for Bare {
        fn name(&self) -> &str {
            "bare"
        }
        fn layer_type(&self) -> LayerType {
            LayerType::System
        }
        fn search(&self, _: &str, _: usize) -> Vec<Candidate> {
            vec![Candidate {
                text: "不该出现".into(),
                ..Default::default()
            }]
        }
        fn search_prefix(&self, _: &str, _: usize) -> Vec<Candidate> {
            vec![Candidate {
                text: "更不该出现".into(),
                ..Default::default()
            }]
        }
    }
    let composite = CompositeDict::new();
    composite.register_layer(Box::new(Bare));
    assert!(composite.search_abbrev("nh", 0).is_empty());
}

#[test]
fn test_user_layer_search() {
    let s = store("user_search");
    s.add_user_word("wb", "a", "工", 100, 0).unwrap();
    s.add_user_word("wb", "abc", "啊吧次", 50, 0).unwrap();
    let layer = StoreUserLayer::new(s.clone(), "wb");
    assert_eq!(layer.layer_type(), LayerType::User);
    let exact = layer.search("a", 10);
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].text, "工");
    assert!(exact[0].meta.is_user_dict);
    // 前缀 "a" 命中 a / abc
    assert_eq!(layer.search_prefix("a", 10).len(), 2);
}

#[test]
fn test_is_prefix_flag_search_vs_search_prefix() {
    // TDD: 验证 search 返回 is_prefix=false，search_prefix 返回 is_prefix=true
    let s = store("is_prefix_flag");
    s.add_user_word("wb", "abc", "啊吧次", 50, 0).unwrap();
    let layer = StoreUserLayer::new(s.clone(), "wb");

    // 精确匹配：is_prefix 应为 false
    let exact = layer.search("abc", 10);
    assert_eq!(exact.len(), 1);
    assert!(
        !exact[0].is_prefix,
        "search() 返回的候选 is_prefix 应为 false"
    );

    // 前缀匹配：is_prefix 应为 true
    let prefix = layer.search_prefix("ab", 10);
    assert_eq!(prefix.len(), 1);
    assert!(
        prefix[0].is_prefix,
        "search_prefix() 返回的候选 is_prefix 应为 true"
    );
}

#[test]
fn test_temp_layer_is_prefix_flag() {
    // StoreTempLayer 的 search_prefix 同样应标记 is_prefix=true
    let s = store("temp_is_prefix_flag");
    s.learn_temp_word("wb", "xyz", "某词", 100, 0).unwrap();
    let layer = StoreTempLayer::new(s.clone(), "wb");

    let exact = layer.search("xyz", 10);
    assert_eq!(exact.len(), 1);
    assert!(!exact[0].is_prefix, "临时层 search() is_prefix 应为 false");

    let prefix = layer.search_prefix("xy", 10);
    assert_eq!(prefix.len(), 1);
    assert!(
        prefix[0].is_prefix,
        "临时层 search_prefix() is_prefix 应为 true"
    );
}

#[test]
fn test_temp_layer_and_composite() {
    let s = store("temp_composite");
    s.add_user_word("wb", "ni", "你", 100, 0).unwrap();
    s.learn_temp_word("wb", "ni", "拟", 800, 0).unwrap();
    let composite = CompositeDict::new();
    composite.register_layer(Box::new(StoreUserLayer::new(s.clone(), "wb")));
    composite.register_layer(Box::new(StoreTempLayer::new(s.clone(), "wb")));
    // composite 跨层查 "ni" → 你(user) + 拟(temp)
    let got = composite.search("ni", 10);
    assert_eq!(got.len(), 2);
    assert!(got.iter().any(|c| c.text == "你" && c.meta.is_user_dict));
    assert!(got.iter().any(|c| c.text == "拟" && c.meta.is_temp_dict));
}

/// 用户词层读引擎：精确、简拼（无边界词靠首字符兜底组）都经引擎分组索引。
#[test]
fn user_layer_reads_engine_and_text() {
    let p = std::env::temp_dir().join(format!("wind_sl_user_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let s = Arc::new(Store::open(&p).unwrap());
    s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
        .unwrap();
    s.add_user_word("pinyin", "xianning", "西安宁", 5, 0)
        .unwrap();
    let l = StoreUserLayer::new(s.clone(), "pinyin");
    let hit = l.search("nihao", 0);
    assert_eq!(hit[0].text, "你好");
    assert!(hit[0].meta.is_user_dict);
    assert_eq!(l.search_abbrev("nh", 0)[0].text, "你好");
    assert!(l.search_abbrev("xan", 0).iter().any(|c| c.text == "西安宁"));
    assert_eq!(
        s.search_user_words_by_abbrev_exact("pinyin", "nh", 0)
            .unwrap()[0]
            .text,
        "你好"
    );
}

/// 晋升：临时词进用户词（边界随词），临时词删掉，用户词落文本。
#[test]
fn promote_writes_user_word_through_engine() {
    let p = std::env::temp_dir().join(format!("wind_sl_promote_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let s = Store::open(&p).unwrap();
    s.learn_temp_word("pinyin", "nihao", "你好", 1, 0b101)
        .unwrap();
    assert!(s.promote_temp_word("pinyin", "nihao", "你好").unwrap());
    assert!(s.runtime().temp_word("pinyin", "nihao", "你好").is_none());
    assert_eq!(
        s.get_user_words("pinyin", "nihao").unwrap()[0].boundary,
        0b101
    );
    s.runtime()
        .data()
        .fold(wind_userdata::record::Target::User, "pinyin")
        .unwrap();
    assert!(
        std::fs::read_to_string(s.user_text_path("pinyin"))
            .unwrap()
            .contains("ni hao\t你好")
    );
}
