use super::*;
use crate::codetable::CodetableDict;
use std::io::Write;

/// 系统层须把词典的音节边界带进候选（精确 + 前缀两条路），不得在 `SystemDictLayer::search`
/// 处截断成三元组。
#[test]
fn system_layer_carries_boundary_into_candidates() {
    let path = std::env::temp_dir().join("wind_layer_boundary.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        writeln!(f, "你好\tni hao\t1200").unwrap();
        writeln!(f, "你\tni\t800").unwrap();
    }
    let d = CodetableDict::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let layer = SystemDictLayer::new(CachedDict::Memory(d), "py-system");

    let exact = layer.search("nihao", 10);
    let hao = exact.iter().find(|c| c.text == "你好").expect("应命中你好");
    assert_eq!(hao.boundary, 0b101, "候选应带 ni|hao 的真值边界");

    // 前缀补全（输入 ni → 补出「你好」）同样要带边界，供双拼校验。
    let pre = layer.search_prefix("ni", 10);
    let hao_p = pre
        .iter()
        .find(|c| c.text == "你好")
        .expect("前缀应补出你好");
    assert_eq!(hao_p.boundary, 0b101, "前缀候选也应带边界");
    assert_eq!(hao_p.code, "nihao", "前缀候选的 code 应是完整码");
}

#[test]
fn test_system_layer_via_manager() {
    // 用内存码表词典构造系统层（避免依赖文件）
    let mut d = CodetableDict::empty();
    d.merge_single("a".into(), "工".into(), 100, 0);
    d.merge_single("a".into(), "戈".into(), 50, 1);
    d.merge_single("aa".into(), "式".into(), 30, 0);
    let cached = CachedDict::Memory(d);

    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(cached, "codetable-system")));

    let exact = dm.search("a", 10);
    assert_eq!(exact.len(), 2);
    assert_eq!(exact[0].text, "工", "权重高者在前");
    // 前缀 a → a/aa
    assert!(dm.search_prefix("a", 10).len() >= 3);
}

#[test]
fn default_weight_overrides_all_entry_weights() {
    // 无权重附加库场景：default_weight 覆盖本库所有条目权重，同权后按 natural_order 排。
    let mut d = CodetableDict::empty();
    d.merge_single("a".into(), "甲".into(), 5, 0); // order 0
    d.merge_single("a".into(), "乙".into(), 999, 1); // order 1（权重更高，但会被覆盖）
    let layer = SystemDictLayer::new(CachedDict::Memory(d), "ext").with_default_weight(Some(100));
    let r = layer.search("a", 10);
    assert_eq!(r.len(), 2);
    assert!(
        r.iter().all(|c| c.weight == 100),
        "default_weight 应覆盖所有条目权重"
    );
    assert_eq!(r[0].text, "甲", "覆盖同权后按 natural_order 出现序");
    assert_eq!(r[1].text, "乙");
}

/// 反向对照的基线：不配 `default_weight` 时，扩展库的高权重如实参与跨库取最大值。
/// 有它才能说明下一条的「没生效」是 `default_weight` 造成的，而不是跨库合并本身没做。
#[test]
fn cross_layer_max_weight_with_real_system_layers() {
    let mut main = CodetableDict::empty();
    main.merge_single("a".into(), "工".into(), 800, 0);
    let mut ext = CodetableDict::empty();
    ext.merge_single("a".into(), "工".into(), 9999, 0);

    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::Memory(main),
        "main",
    )));
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::Memory(ext),
        "ext",
    )));

    let r = dm.search("a", 10);
    assert_eq!(r.len(), 1, "同 text 跨层合并成一条");
    assert_eq!(r[0].weight, 9999, "扩展库的高权重应如实生效");
    assert_eq!(
        r[0].meta.raw_weight, 800,
        "raw_weight 留的是首个出现层（主库）的库内原值，只有 weight 是跨层继承的——\
             排查时若只看 raw_weight 会误判成「扩展库权重没进来」"
    );
    assert_eq!(
        r[0].meta.weight_layer.as_deref(),
        Some("ext"),
        "权重来源须随权重一起换到扩展库（悬停调试段据此显示 `权 9999 ←ext`）"
    );
}

/// `default_weight` 先抹平本库权重，再参与跨库取最大值。
///
/// 库文件里写着 9999，但 `[[dictionaries]].default_weight` 把整库压成一个档位，参与比较的
/// 是那个档位——「扩展词库明明权重更高却没生效」的一条成因。
/// 与归一化的分工：`weight_spec` 是保序压缩（不颠倒库内高低），`default_weight` 是抹平。
#[test]
fn default_weight_flattens_before_cross_layer_max() {
    let mut main = CodetableDict::empty();
    main.merge_single("a".into(), "工".into(), 800, 0);
    let mut ext = CodetableDict::empty();
    ext.merge_single("a".into(), "工".into(), 9999, 0); // 库里权重远高于主库

    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::Memory(main),
        "main",
    )));
    dm.register_layer(Box::new(
        SystemDictLayer::new(CachedDict::Memory(ext), "ext").with_default_weight(Some(100)),
    ));

    let r = dm.search("a", 10);
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].weight, 800,
        "扩展库配了 default_weight=100，参与比较的是 100 而非词条原值 9999，\
             故主库的 800 胜出"
    );
    assert_eq!(
        r[0].meta.weight_layer.as_deref(),
        Some("main"),
        "主库胜出时来源标注须留在主库——标错会把排查引向扩展库"
    );
}

/// 禁用的扩展库完全不参与 max：关掉后回退到剩余启用库，开回来即恢复。
/// 层的 `enabled` 是它自己的原子标志，composite 的 mock 层验不到，这里用真实 `SystemDictLayer` 再验。
#[test]
fn disabled_system_layer_excluded_from_cross_layer_max() {
    let mut main = CodetableDict::empty();
    main.merge_single("a".into(), "工".into(), 800, 0);
    let mut ext = CodetableDict::empty();
    ext.merge_single("a".into(), "工".into(), 9999, 0);

    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::Memory(main),
        "main",
    )));
    // 扩展库初始即关闭（对齐方案配置 enabled=false 的加载路径）。
    dm.register_layer(Box::new(SystemDictLayer::with_enabled(
        CachedDict::Memory(ext),
        "ext",
        false,
    )));

    assert_eq!(
        dm.search("a", 10)[0].weight,
        800,
        "扩展库关闭时不得把 9999 带进来"
    );
    assert!(dm.set_layer_enabled("ext", true));
    assert_eq!(dm.search("a", 10)[0].weight, 9999, "开启后即时生效");
    assert!(dm.set_layer_enabled("ext", false));
    let back = dm.search("a", 10);
    assert_eq!(back[0].weight, 800, "再关掉应回退，不留残值");
    assert_eq!(
        back[0].meta.weight_layer.as_deref(),
        Some("main"),
        "来源标注须跟着回退，不得残留已关闭的 ext"
    );
}

/// 系统层把通配命中转成候选：带完整 code、更长码标 `is_prefix`、等长档在前。
#[test]
fn system_layer_answers_pattern_queries() {
    let mut d = CodetableDict::empty();
    d.merge_single("ab".into(), "甲".into(), 10, 0);
    d.merge_single("abcd".into(), "丙".into(), 9999, 0);
    let layer = SystemDictLayer::new(CachedDict::Memory(d), "sys");
    let r = layer.search_pattern(
        &format!("a{}", crate::WILDCARD_SLOT),
        crate::WILDCARD_SLOT,
        10,
        true,
    );
    let got: Vec<(&str, &str, bool)> = r
        .iter()
        .map(|c| (c.text.as_str(), c.code.as_str(), c.is_prefix))
        .collect();
    assert_eq!(got, [("甲", "ab", false), ("丙", "abcd", true)]);
}

/// `entries_digest` 必须随枚举结果的每个来源变：词库内容、文件（无 `.fp` 时的
/// `大小:mtime`）、两项权重换算。缺一项，英文词组索引就会在那一项变了之后读回旧文件。
///
/// 内存词库（wdat 写失败的降级形态）给 `None`：没有稳定产物可比 ⇒ 派生索引不落盘。
#[test]
fn entries_digest_tracks_content_file_and_weight_transform() {
    let dir = std::env::temp_dir().join(format!("wind_entries_digest_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let yaml = dir.join("en.dict.yaml");
    let wdat = dir.join("en.wdat");
    let write_yaml = |body: &str| {
        std::fs::write(&yaml, format!("---\nname: en\n...\n{body}")).unwrap();
    };
    let digest_of = |dw: Option<i32>| {
        let d = CachedDict::load_at_with(&yaml, &wdat, true).unwrap();
        SystemDictLayer::new(d, "en")
            .with_default_weight(dw)
            .entries_digest()
            .expect("mmap 词库应给得出摘要")
    };

    write_yaml("Buenos Aires\tbuenosaires\t60\n");
    let v1 = digest_of(None);
    assert_eq!(
        digest_of(None),
        v1,
        "什么都没变，摘要必须稳定（否则永远不命中）"
    );
    assert_ne!(
        digest_of(Some(5)),
        v1,
        "default_weight 改了，枚举出的权重就变了"
    );

    write_yaml("Buenos Aires\tbuenosaires\t60\nZulu Time\tzulu\t40\n");
    assert_ne!(digest_of(None), v1, "词库内容变了，摘要必须变");

    // 无 `.fp` sidecar（wdat-only 分发）时退到「大小:mtime」：只动 mtime 也得变。
    // `wdat` 是槽位，实际文件名带指纹——拿 reader 自报的路径。
    let v2 = digest_of(None);
    let real = CachedDict::load_at_with(&yaml, &wdat, true)
        .unwrap()
        .source_file()
        .expect("mmap 词库应有磁盘产物")
        .to_path_buf();
    let mut fp = real.clone().into_os_string();
    fp.push(".fp");
    std::fs::remove_file(&fp).unwrap();
    let d = CachedDict::Mmap(crate::reader_pool::open_wdat(&real).unwrap());
    let no_fp = SystemDictLayer::new(d, "en").entries_digest().unwrap();
    assert_ne!(no_fp, v2);
    let f = std::fs::File::options().write(true).open(&real).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    let d = CachedDict::Mmap(crate::reader_pool::open_wdat(&real).unwrap());
    assert_ne!(
        SystemDictLayer::new(d, "en").entries_digest().unwrap(),
        no_fp,
        "无 .fp 时 mtime 变了摘要必须变"
    );

    let mem = SystemDictLayer::new(CachedDict::Memory(CodetableDict::empty()), "m");
    assert!(mem.entries_digest().is_none(), "内存词库说不清摘要");
    let _ = std::fs::remove_dir_all(&dir);
}
