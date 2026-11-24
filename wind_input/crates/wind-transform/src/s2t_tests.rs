use super::*;
use std::path::PathBuf;

/// 真实 opencc 数据目录（`build_dev/data/opencc`）。
///
/// `build_dev` 在仓库根（三级 `../../../`）；只写两级会落到不存在的目录，所有依赖真实数据
/// 的用例静默走「跳过」分支照样绿。两处都试，取真的有数据的那一个。
fn opencc_dir() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = base.join("../../../build_dev/data/opencc");
    [root.clone(), base.join("../../build_dev/data/opencc")]
        .into_iter()
        .find(|d| d.join("STCharacters.octrie").is_file())
        .unwrap_or(root)
}

#[test]
fn test_s2t_standard_conversion() {
    let dir = opencc_dir();
    if !dir.join("STCharacters.octrie").exists() {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    let conv = Converter::load_variant(&dir, "s2t").expect("应加载 s2t 链");
    // 简体 → 繁体（字级）
    assert_eq!(conv.convert("汉字"), "漢字");
    assert_eq!(conv.convert("简体转换"), "簡體轉換");
    // 词级最长匹配（软件 → 軟件，标准 s2t 不转台湾习惯词）
    let r = conv.convert("计算机");
    assert!(r.chars().count() == 3, "长度应保持，实际: {}", r);
}

/// 繁 → 简（`input.t2s`「繁入简出」用的链）。
///
/// 判据刻意选繁简不同形的字：`人`/`工`/`一` 这类同形字两个方向都恒等，链没加载成功也照样绿。
#[test]
fn t2s_converts_traditional_to_simplified() {
    let dir = opencc_dir();
    if !dir.join("TSCharacters.octrie").exists() {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    let conv = Converter::load_variant(&dir, "t2s").expect("应加载 t2s 链");
    assert_eq!(conv.convert("漢字"), "汉字");
    assert_eq!(conv.convert("簡體轉換"), "简体转换");
    // 与 s2t 互为往返：简 → 繁 → 简 应回到原文（对这批无歧义字而言）。
    let s2t = Converter::load_variant(&dir, "s2t").expect("应加载 s2t 链");
    for word in ["汉字", "简体转换", "计算机"] {
        assert_eq!(conv.convert(&s2t.convert(word)), word, "往返不闭合: {word}");
    }
}

/// 1对多变体表是简→繁方向独有的资产：`STVariants.octrie` 在同一目录里，按名 load 在 t2s 下
/// 也会成功，`variants_of` 就会拿简体键查繁体输入、恒不命中。静默恒空比明确的 `None` 难查，
/// 故由链的方向挡住它。
#[test]
fn t2s_chain_has_no_variant_table() {
    let dir = opencc_dir();
    if !dir.join("TSCharacters.octrie").exists() {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    let s2t = Converter::load_variant(&dir, "s2t").expect("应加载 s2t 链");
    assert!(
        !s2t.variants_of("出").is_empty(),
        "探针失效：s2t 方向本该查得到「出」的变体，否则下一条断言恒绿"
    );
    let t2s = Converter::load_variant(&dir, "t2s").expect("应加载 t2s 链");
    assert!(t2s.variants.is_none(), "t2s 链不该带变体表");
    assert!(t2s.variants_of("出").is_empty());
}

/// `load` 改成分段读之后，必须与「整文件读入再 parse」逐字段等价。
/// 这是本次改动唯一可能出错的地方：偏移算错会让整张表静默错位。
#[test]
fn load_is_equivalent_to_read_then_parse() {
    let p = opencc_dir().join("STPhrases.octrie");
    if !p.exists() {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    let via_load = Dict::load(&p).expect("load 应成功");
    let via_parse = Dict::parse(&std::fs::read(&p).unwrap()).expect("parse 应成功");

    assert_eq!(via_load.max_key_len, via_parse.max_key_len);
    assert_eq!(
        via_load.strings, via_parse.strings,
        "字符串池必须逐字节一致"
    );
    assert_eq!(via_load.entries.len(), via_parse.entries.len());
    for (i, (a, b)) in via_load
        .entries
        .iter()
        .zip(via_parse.entries.iter())
        .enumerate()
    {
        assert_eq!(
            (a.key_off, a.key_len, a.val_off, a.val_len),
            (b.key_off, b.key_len, b.val_off, b.val_len),
            "第 {i} 条 entry 不一致"
        );
    }
}

/// 损坏的 count 不得据此分配巨量缓冲（分段读要先自己校验，不再有 parse 的
/// `entries_end > data.len()` 兜底）。
#[test]
fn load_rejects_corrupt_count() {
    let dir = std::env::temp_dir().join(format!("wind-s2t-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bad.octrie");

    let mut d = Vec::new();
    d.extend_from_slice(MAGIC);
    d.extend_from_slice(&1u32.to_le_bytes()); // version
    d.extend_from_slice(&u32::MAX.to_le_bytes()); // count：远超文件实际长度
    d.extend_from_slice(&4u16.to_le_bytes()); // max_key
    d.extend_from_slice(&[0u8; 2]); // 补齐 HEADER_SIZE
    assert_eq!(d.len(), HEADER_SIZE);
    std::fs::write(&p, &d).unwrap();

    assert!(
        Dict::load(&p).is_none(),
        "count 越界时应直接拒绝，而不是尝试分配 48GB"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 1对多变体表：多值字返回全部变体（定义序，首个=默认转换结果），单值字返回空。
#[test]
fn variants_of_returns_all_traditional_forms() {
    let dir = opencc_dir();
    if !dir.join("STVariants.octrie").exists() {
        eprintln!("跳过：缺少 STVariants 数据");
        return;
    }
    let conv = Converter::load_variant(&dir, "s2t").expect("应加载 s2t 链");
    // 「出」：默认不转（首值=自身），变体含「齣」。
    let v = conv.variants_of("出");
    assert_eq!(v, vec!["出", "齣"], "出 的变体应为 [出, 齣]");
    // 「发」：默认转「發」，变体另含「髮」。
    let v = conv.variants_of("发");
    assert!(v.contains(&"發".to_string()) && v.contains(&"髮".to_string()));
    assert_eq!(v[0], conv.convert("发"), "首个变体应与默认转换一致");
    // 单值字不在变体表：返回空（展开层据此跳过）。
    assert!(conv.variants_of("汉").is_empty());
    assert!(conv.variants_of("x").is_empty());
}

/// 造一份最小 .octrie（格式见文件头）：entries 按 key 字节序排，offset 相对字符串池起点。
fn build_octrie(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut rows: Vec<(&str, &str)> = pairs.to_vec();
    rows.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut strings: Vec<u8> = Vec::new();
    let mut entries: Vec<u8> = Vec::new();
    let mut max_key = 0usize;
    for (k, v) in &rows {
        let ko = strings.len() as u32;
        strings.extend_from_slice(k.as_bytes());
        let vo = strings.len() as u32;
        strings.extend_from_slice(v.as_bytes());
        entries.extend_from_slice(&ko.to_le_bytes());
        entries.extend_from_slice(&(k.len() as u16).to_le_bytes());
        entries.extend_from_slice(&vo.to_le_bytes());
        entries.extend_from_slice(&(v.len() as u16).to_le_bytes());
        max_key = max_key.max(k.len());
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    out.extend_from_slice(&(max_key as u16).to_le_bytes());
    out.extend_from_slice(&[0u8; 2]);
    out.extend_from_slice(&entries);
    out.extend_from_slice(&strings);
    out
}

/// 定制层只放一本 `STPhrases.octrie` 时，链的其余部分必须落回出厂那一层。
///
/// `load_variant` 的组内判据是「至少一本加载成功」，「先选中一个目录再整份加载」会拿只有
/// 词组表的残链当胜利，一个字都不转。夹具全部自造，故本用例不会因缺数据而跳过。
#[test]
fn resolved_chain_falls_back_per_file_not_per_directory() {
    let base = std::env::temp_dir().join(format!("wind-s2t-layers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data = base.join("data/opencc");
    let custom = base.join("custom/opencc");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&custom).unwrap();

    // data 层：字表 + 变体表（出厂自带的全套）
    std::fs::write(
        data.join("STCharacters.octrie"),
        build_octrie(&[
            ("简", "簡"),
            ("体", "體"),
            ("转", "轉"),
            ("换", "換"),
            ("测", "測"),
            ("试", "試"),
        ]),
    )
    .unwrap();
    std::fs::write(
        data.join("STVariants.octrie"),
        build_octrie(&[("发", "發 髮")]),
    )
    .unwrap();
    // data 层也有词组表，定制层要能盖住它（证明「命中靠前层」这一半也成立）
    std::fs::write(
        data.join("STPhrases.octrie"),
        build_octrie(&[("转换测试", "轉換測試")]),
    )
    .unwrap();
    // custom 层：**只放一本**词组表
    std::fs::write(
        custom.join("STPhrases.octrie"),
        build_octrie(&[("转换测试", "轉換測試〔定制〕")]),
    )
    .unwrap();

    let layers = [custom.clone(), data.clone()];
    let resolve = |file: &str| -> Option<PathBuf> {
        layers
            .iter()
            .map(|d| d.join(file))
            .find(|p| p.is_file())
            .or(None)
    };
    let conv = Converter::load_variant_resolved("s2t", resolve).expect("应能建出链");

    assert_eq!(
        conv.convert("简体字转换测试"),
        "簡體字轉換測試〔定制〕",
        "定制层的词组表要生效，而字表必须仍从 data 层取——整份胜出时这里会原样不转"
    );
    assert_eq!(
        conv.convert("简体"),
        "簡體",
        "定制层没有字表，不得因此丢掉出厂字表"
    );
    assert_eq!(
        conv.variants_of("发"),
        vec!["發", "髮"],
        "STVariants 只在 data 层，同样不得被定制层的存在挤掉"
    );

    // 反面对照：只看定制层（= 旧的「整份胜出」在这台机器上的实际效果），
    // 链只剩词组表，字表整个消失。
    let only_custom = Converter::load_variant(&custom, "s2t").expect("残链也能建出来");
    assert_eq!(
        only_custom.convert("简体"),
        "简体",
        "对照：只有 STPhrases 的残链一个字都不转——这正是本用例要挡住的现象"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn test_s2t_preserves_non_chinese() {
    let dir = opencc_dir();
    if !dir.join("STCharacters.octrie").exists() {
        return;
    }
    let conv = Converter::load_variant(&dir, "s2t").unwrap();
    assert_eq!(conv.convert("abc123"), "abc123");
    assert_eq!(conv.convert("hello 世界"), "hello 世界");
}
