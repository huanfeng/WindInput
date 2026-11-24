use super::*;
use std::io::Write;

/// `scan_weight_stats` 的画像：分位数、超范围计数、无权重条目分离统计。
/// `dict weight-check` 只是把它排版打印。
#[test]
fn scan_weight_stats_profiles_a_dict() {
    let path = std::env::temp_dir().join("wind_wscan_demo.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "---
name: x
columns:
  - code
  - text
  - weight
..."
        )
        .unwrap();
        for (c, t, w) in [
            ("a", "甲", "1000"),
            ("b", "乙", "5000"),
            ("c", "丙", "50000"),
            ("d", "丁", "500000"),
            ("e", "戊", "1000000"),
            ("f", "己", ""), // 空权重列：不计入分母
        ] {
            writeln!(f, "{c}	{t}	{w}").unwrap();
        }
    }
    let sc = scan_weight_stats(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(sc.weighted, 5, "空权重列不计入分母");
    assert_eq!(sc.zero, 1);
    assert_eq!(sc.min, 1000);
    assert_eq!(sc.median, 50000, "5 条取中间那条");
    assert_eq!(sc.max, 1_000_000);
    assert_eq!(sc.over_range, 3, "50000/500000/1000000 超 10000");
    assert!(!sc.is_compliant());
    assert!((sc.over_pct() - 60.0).abs() < 0.01);
}

/// 守约词库：`is_compliant` 为真，CLI 据此不给建议。
#[test]
fn scan_weight_stats_marks_compliant_dict() {
    let path = std::env::temp_dir().join("wind_wscan_ok.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "---
name: x
columns:
  - code
  - text
  - weight
..."
        )
        .unwrap();
        writeln!(f, "a	工	120").unwrap();
        writeln!(f, "b	弗	9950").unwrap();
        writeln!(f, "c	王	10000").unwrap();
    }
    let sc = scan_weight_stats(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(sc.is_compliant(), "10000 是边界内，不算超范围");
    assert_eq!(sc.over_range, 0);
    assert_eq!(sc.median, 9950);
}

/// 整库无权重列：`weighted == 0`，算守约（那是「退化为文件顺序」的有意设计）。
#[test]
fn scan_weight_stats_handles_dict_without_weights() {
    let path = std::env::temp_dir().join("wind_wscan_nw.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "---
name: x
columns:
  - code
  - text
..."
        )
        .unwrap();
        writeln!(f, "a	甲").unwrap();
        writeln!(f, "b	乙").unwrap();
    }
    let sc = scan_weight_stats(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(sc.weighted, 0);
    assert_eq!(sc.zero, 2);
    assert!(sc.is_compliant(), "无权重不是「超范围」");
}

/// 权重值域诊断的取数：超出 `WEIGHT_RANGE_MAX` 的条目计入 `over_range`，
/// 权重 0（无权重列/空列）不计入分母——那是「未定义」不是「超范围」。
/// `over_range/weighted` 会出现在告警里，0 权重进分母会让百分比虚低。
#[test]
fn weight_range_diagnostic_counts_only_real_weights() {
    let spec = ColumnSpec::from_layout(ColumnLayout::CodeFirst, false);
    let mut st = ParseStats::default();
    // 守约、超范围、恰好在边界、空权重、零权重
    for line in [
        "a	工	9999",
        "b	的	10359470",
        "c	一	10000",
        "d	是	",
        "e	不	0",
    ] {
        parse_rime_line(line, true, false, spec, &mut st);
    }
    assert_eq!(st.weighted, 3, "只有 9999/10359470/10000 三条算「有权重」");
    assert_eq!(st.over_range, 1, "只有 10359470 超范围；10000 是边界内");
    assert_eq!(st.max_weight, 10_359_470);
}

/// 反向锁：全部守约时诊断一声不吭（`over_range == 0`），干净的词库不刷屏。
#[test]
fn well_behaved_dict_triggers_no_weight_warning() {
    let spec = ColumnSpec::from_layout(ColumnLayout::CodeFirst, false);
    let mut st = ParseStats::default();
    for line in ["a	工	120", "aa	弗	9950", "aaa	工	9000"] {
        parse_rime_line(line, true, false, spec, &mut st);
    }
    assert_eq!(st.weighted, 3);
    assert_eq!(st.over_range, 0, "五笔量级的权重不得触发告警");
}

#[test]
fn syllable_boundary_mask_basics() {
    // 多音节：起始字节位 {0,2}（ni 占 0..2，hao 占 2..5）。
    assert_eq!(syllable_boundary_mask("ni hao"), 0b101);
    // 变长音节：zhuang(6B) 起始 0，ni 起始 6。
    assert_eq!(syllable_boundary_mask("zhuang ni"), 0b1000001);
    // 单音节：整串一个音节，起始 {0}。是真实信息，不是「未知」。
    assert_eq!(syllable_boundary_mask("ni"), 0b1);
    // 空码 → 无信息。
    assert_eq!(syllable_boundary_mask(""), 0);
    // 超长码（拼接 ≥64B）：bitmask 装不下 → 整体降级为 0，不给半截错误边界。
    let long = ["zhuang"; 12].join(" "); // 12*6 = 72B
    assert_eq!(syllable_boundary_mask(&long), 0);
}

/// 端到端：rime 源 → 解析 → wdat 落盘 → mmap 读回，边界必须原样穿过整条链路。
#[test]
fn boundary_survives_wdat_roundtrip() {
    let dir = std::env::temp_dir().join("wind_boundary_roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("py.dict.yaml");
    {
        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        writeln!(f, "你好\tni hao\t1200").unwrap();
        writeln!(f, "你\tni\t800").unwrap();
        // 同 code 不同切分：xi'an（西安，2 音节）vs xian（先，1 音节）。
        // 正是 DAG 无从分辨、必须靠词典真值的场景（两者覆盖字符数相同）。
        writeln!(f, "西安\txi an\t500").unwrap();
        writeln!(f, "先\txian\t900").unwrap();
    }
    let dict = CodetableDict::load(&src).unwrap();

    let mut w = crate::datformat::WdatWriter::new();
    dict.export_to_wdat(&mut w);
    let wdat = dir.join("py.wdat");
    w.write(&wdat).unwrap();

    let reader = crate::datformat::WdatReader::open(&wdat).unwrap();
    let find = |code: &str, text: &str| -> Option<u64> {
        reader
            .search(code)
            .into_iter()
            .find(|e| e.text == text)
            .map(|e| e.boundary)
    };

    assert_eq!(find("nihao", "你好"), Some(0b101), "ni|hao 边界应读回");
    assert_eq!(find("ni", "你"), Some(0b1));
    // 关键：同一 key "xian" 下两条候选各自带边界，据此可区分 xi|an 与 xian。
    assert_eq!(find("xian", "西安"), Some(0b101), "xi|an → 起始 {{0,2}}");
    assert_eq!(find("xian", "先"), Some(0b1), "xian → 单音节，起始 {{0}}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 英文词库格式 `word<TAB>word`（混合大小写）：load_lowercased 应小写化 code、
/// 保留 text 原样，使大小写不敏感前缀匹配生效。
#[test]
fn load_lowercased_english() {
    let path = std::env::temp_dir().join("wind_en_lowercase_test.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: en\n...").unwrap();
        writeln!(f, "# ab\tab").unwrap(); // 注释行跳过
        writeln!(f, "Aaron\tAaron").unwrap();
        writeln!(f, "abandon\tabandon").unwrap();
        writeln!(f, "ABC\tABC").unwrap();
    }
    let d = CodetableDict::load_lowercased(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    // 小写前缀 "aa" 命中 Aaron（原样大小写 text）
    let r = d.search_prefix("aa", 10);
    assert!(
        r.iter()
            .any(|(code, text, _, _)| code == "aaron" && text == "Aaron"),
        "应小写码命中、保留原样 text: {:?}",
        r
    );
    // 精确小写 "abc" 命中 ABC
    assert!(d.search("abc").iter().any(|(t, _, _)| t == "ABC"));
}

/// 取 fulls（(code, text, weight, boundary)）中某 text 的 (code, weight)。
fn collect(entries: &[(String, String, i32, u64)], text: &str) -> Vec<(String, i32)> {
    entries
        .iter()
        .filter(|(_, t, _, _)| t == text)
        .map(|(c, _, w, _)| (c.clone(), *w))
        .collect()
}

/// 取 abbrevs 中某全拼码对应的 `(简拼, weight)`。abbrevs 的第二个字段存的是全拼码
/// （AbbrevSection 是指向主键的二级索引，不复制词本身）。
fn collect_ab(entries: &[(String, String, i32)], full_code: &str) -> Vec<(String, i32)> {
    entries
        .iter()
        .filter(|(_, c, _)| c == full_code)
        .map(|(ab, _, w)| (ab.clone(), *w))
        .collect()
}

/// 取 fulls 中某 text 的 boundary。
fn boundary_of(entries: &[(String, String, i32, u64)], text: &str) -> Option<u64> {
    entries
        .iter()
        .find(|(_, t, _, _)| t == text)
        .map(|(_, _, _, b)| *b)
}

/// 并行解析：拼音格式（text\tcode\tweight，code 去空格）+ 注释/空行跳过，
/// 小文件走串行分支，结果应完整正确。
#[test]
fn parallel_parse_pinyin_format_small() {
    let path = std::env::temp_dir().join("wind_parrime_small.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // 声明 columns 才会取权重列（未声明时保守只认 text/code）
        writeln!(
            f,
            "---\nname: py\ncolumns:\n  - text\n  - code\n  - weight\n..."
        )
        .unwrap();
        writeln!(f, "# 注释跳过").unwrap();
        writeln!(f).unwrap(); // 空行跳过
        writeln!(f, "你好\tni hao\t1200").unwrap(); // code 去空格 -> nihao
        writeln!(f, "你\tni\t800").unwrap();
    }
    let (e, ab) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(e.len(), 2, "应解析 2 条，跳过注释/空行: {e:?}");
    assert_eq!(collect(&e, "你好"), vec![("nihao".to_string(), 1200)]);
    assert_eq!(collect(&e, "你"), vec![("ni".to_string(), 800)]);
    // 简拼：多音节 "ni hao"→"nh"；单音节 "ni" 无简拼。查询键是全拼码。
    assert_eq!(collect_ab(&ab, "nihao"), vec![("nh".to_string(), 1200)]);
    assert!(collect_ab(&ab, "ni").is_empty(), "单音节不产简拼");
    // 音节边界：源数据 "ni hao" 的空格是真值边界，不得随 code 拼平而丢弃。
    // "nihao" 音节 ni|hao → 起始字节 {0,2} → 0b101。
    assert_eq!(
        boundary_of(&e, "你好"),
        Some(0b101),
        "「你好」应记住 ni|hao 的边界"
    );
    // 单音节：整串一个音节 → 起始 {0} → 0b1（是真实信息，非「未知」）。
    assert_eq!(boundary_of(&e, "你"), Some(0b1));
}

/// 跨 1MB 阈值触发并行切块：构造大量行，断言总数与抽样正确、块边界不丢/不重行。
#[test]
fn parallel_parse_large_chunked_no_loss() {
    let path = std::env::temp_dir().join("wind_parrime_large.dict.yaml");
    let n = 60_000; // 每行约 20+ 字节 → 正文 > 1MB，触发并行分支
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        writeln!(
            f,
            "---\nname: big\ncolumns:\n  - code\n  - text\n  - weight\n..."
        )
        .unwrap();
        for i in 0..n {
            // 五笔格式 code\ttext\tweight，code 全 ASCII
            writeln!(f, "code{i}\t文{i}\t{i}").unwrap();
        }
    }
    let (e, _ab) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(e.len(), n, "并行切块不应丢行/重复");
    // 抽样首/中/尾
    assert_eq!(collect(&e, "文0"), vec![("code0".to_string(), 0)]);
    assert_eq!(
        collect(&e, "文59999"),
        vec![("code59999".to_string(), 59999)]
    );
    // 五笔码无音节概念 → boundary 恒 0（消费方据此降级，不会误当拼音边界）。
    assert_eq!(boundary_of(&e, "文0"), Some(0), "五笔码不应有音节边界");
    // 全部 code 唯一（边界未把某行切成两半）
    let mut codes: Vec<&str> = e.iter().map(|(c, _, _, _)| c.as_str()).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), n, "所有 code 应唯一");
}

/// 构造期望的 `Usable`（`has_syllables` 由 resolve_columns 采样填入，此处恒 false）。
fn usable(text_col: usize, code_col: usize, weight_col: Option<usize>) -> ColumnsDecl {
    ColumnsDecl::Usable(ColumnSpec {
        text_col,
        code_col,
        weight_col,
        has_syllables: false,
    })
}

/// 头部 `columns:` 声明是权威列规格，两种顺序都要认，且能穿过行内注释。
#[test]
fn columns_header_is_authoritative() {
    // flypy 风格：带行内注释
    let flypy = "---\nname: x\ncolumns:    # 码表格式\n  - text    # 文字\n  - code    # 输入码\n  - weight  # 权重\n...\n";
    assert_eq!(parse_columns_header(flypy), usable(0, 1, Some(2)));
    // wubi 风格：code 在前
    let wubi = "---\nname: y\nsort: by_weight\ncolumns:\n  - code\n  - text\n  - weight\n...\n";
    assert_eq!(parse_columns_header(wubi), usable(1, 0, Some(2)));
    // 只声明两列（用户为 12_kf 补的正是这种）→ 无权重列
    let two = "---\nname: kf\ncolumns:\n  - text\n  - code\n...\n";
    assert_eq!(parse_columns_header(two), usable(0, 1, None));
    // 无声明
    assert_eq!(
        parse_columns_header("---\nname: z\n...\n"),
        ColumnsDecl::Absent
    );
    // 声明里出现未支持的列名：占位并顺延后续列下标，不得错位
    let stem = "---\ncolumns:\n  - text\n  - code\n  - stem\n  - weight\n...\n";
    assert_eq!(
        parse_columns_header(stem),
        usable(0, 1, Some(3)),
        "stem 占一列，weight 应顺延到下标 3"
    );
    // columns 块后回到别的键，不应越界把后续键读成列名
    let trailing = "---\ncolumns:\n  - code\n  - text\nsort: by_weight\n...\n";
    assert_eq!(parse_columns_header(trailing), usable(1, 0, None));
}

/// `sort:` 是 librime 的库内同码排序键，本输入法不消费，取出仅为告警
/// （否则把 `by_weight` 改成 `original` 观察不到变化也拿不到诊断）。
#[test]
fn sort_header_is_detected_for_warning() {
    assert_eq!(
        parse_sort_header("---\nname: x\nsort: by_weight\n...\n").as_deref(),
        Some("by_weight")
    );
    assert_eq!(
        parse_sort_header("---\nsort: original\n...\n").as_deref(),
        Some("original")
    );
    // 行内注释剥离（district 库就带注释）
    assert_eq!(
        parse_sort_header("---\nsort: original  # 原始顺序\n...\n").as_deref(),
        Some("original")
    );
    // 无声明 → 不告警
    assert_eq!(
        parse_sort_header("---\nname: x\ncolumns: [text, code]\n...\n"),
        None
    );
    // 缩进的同名键属于别的映射，不算（与 columns: 同规则）
    assert_eq!(
        parse_sort_header("---\nfoo:\n  sort: by_weight\n...\n"),
        None
    );
    // 空值不触发
    assert_eq!(parse_sort_header("---\nsort:\n...\n"), None);
}

/// 流式序列必须支持：它是合法 YAML，且警告文案就建议用户这么写；只认块序列会让照建议改完
/// 的用户看到「改了没用」。
#[test]
fn columns_header_accepts_flow_sequence() {
    assert_eq!(
        parse_columns_header("---\nname: x\ncolumns: [text, code, weight]\n...\n"),
        usable(0, 1, Some(2))
    );
    // 紧凑写法 + 行内注释 + code 在前
    assert_eq!(
        parse_columns_header("---\ncolumns: [code,text,weight]  # 五笔\n...\n"),
        usable(1, 0, Some(2))
    );
    // 两列流式
    assert_eq!(
        parse_columns_header("---\ncolumns: [text, code]\n...\n"),
        usable(0, 1, None)
    );
}

/// 残缺声明要能被区分地诊断：把「声明不完整」误报成「未声明」，会让照日志排查的人
/// 看到头部声明确实存在，从而排除掉正确的线索。
#[test]
fn incomplete_columns_declaration_is_distinguished() {
    // 缺 code：librime 的自动编码词库形态（如 `columns: [text, weight]`）
    assert_eq!(
        parse_columns_header("---\ncolumns:\n  - text\n  - weight\n...\n"),
        ColumnsDecl::MissingCode
    );
    // 缺 text：librime 直接丢弃整个文件
    assert_eq!(
        parse_columns_header("---\ncolumns:\n  - code\n  - weight\n...\n"),
        ColumnsDecl::MissingText
    );
}

/// 声明缺 code 的词库整库跳过，而不是降级探测。
/// 降级探测会把权重列当成编码（`is_code_shape("100")` 为真），静默灌进一整库垃圾编码。
#[test]
fn missing_code_column_skips_whole_dict() {
    let path = std::env::temp_dir().join("wind_cols_no_code.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: auto\ncolumns:\n  - text\n  - weight\n...").unwrap();
        writeln!(f, "〇〇八\t100").unwrap();
        writeln!(f, "〇一二\t100").unwrap();
    }
    let (e, ab) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        e.is_empty() && ab.is_empty(),
        "无 code 列的词库应整库跳过，不得把权重当编码：{e:?}"
    );
}

/// 以 U+3000 全角空格为词条的行不得被 `str::trim()` 当缩进削掉（U+3000 属 Unicode
/// White_Space），否则整行字段左移一格。
#[test]
fn leading_fullwidth_space_text_is_preserved() {
    let path = std::env::temp_dir().join("wind_leading_ws.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: ws\n...").unwrap();
        writeln!(f, "\u{3000}\tcokg\t\t全角空格").unwrap(); // 四列，第 3 列空
        writeln!(f, "\u{3000}\tpwst").unwrap(); // 两列
        writeln!(f, "字\tabc\t5").unwrap();
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        collect(&e, "\u{3000}"),
        vec![("cokg".to_string(), 0), ("pwst".to_string(), 0)],
        "全角空格词条应完整保留，两条都在：{e:?}"
    );
    // 不得产出 code 为空串的垃圾条目（字段左移的副产物）
    assert!(
        !e.iter().any(|(c, _, _, _)| c.is_empty()),
        "不应出现空编码条目：{e:?}"
    );
    assert_eq!(collect(&e, "字"), vec![("abc".to_string(), 5)]);
}

/// 词条文本的转义序列须还原为真字符：`\n`→换行、`\t`→制表、`\\`→反斜杠。
///
/// 未知序列必须原样保留：`C:\Users` 里的 `\U` 不在转义表中，反转义后仍是 `\U`，
/// 存量词库里的 Windows 路径不会被静默改写。想要字面 `\n` 两字符的写 `\\n`。
#[test]
fn text_escape_sequences_are_unescaped() {
    let path = std::env::temp_dir().join("wind_text_escape.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: esc\n...").unwrap();
        writeln!(f, "甲\\n乙\tjy\t10").unwrap(); // \n → 换行
        writeln!(f, "丙\\t丁\tbd\t20").unwrap(); // \t → 制表（注意不是列分隔符）
        writeln!(f, "戊\\\\己\twj\t30").unwrap(); // \\ → 单个反斜杠
        writeln!(f, "C:\\Users\tcu\t40").unwrap(); // \U 未知 → 原样保留
        writeln!(f, "庚\\\\n辛\tgx\t50").unwrap(); // \\n → 字面 \n 两字符，不是换行
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(collect(&e, "甲\n乙"), vec![("jy".to_string(), 10)]);
    assert_eq!(collect(&e, "丙\t丁"), vec![("bd".to_string(), 20)]);
    assert_eq!(collect(&e, "戊\\己"), vec![("wj".to_string(), 30)]);
    assert_eq!(
        collect(&e, "C:\\Users"),
        vec![("cu".to_string(), 40)],
        "未知转义序列须原样保留，存量词库里的 Windows 路径不得被改写"
    );
    assert_eq!(
        collect(&e, "庚\\n辛"),
        vec![("gx".to_string(), 50)],
        "`\\\\n` 应还原为字面 \\n 两字符，而非换行"
    );
}

/// 末列 text 的尾随空白是词条内容，不是排版噪声。
///
/// 整行 `trim_end` 再切列会让 CodeFirst 布局（text 落末列）丢掉尾随空格，同一词条换个列序
/// 语义就变了；蒙古文 toli 词库整库以空格收尾，丢了会让连续上屏的词粘在一起。
/// 故逐字段处理：text 原样，code/weight 各自 `trim_end`。同时锁住转义序列仍然存活。
#[test]
fn trailing_whitespace_in_last_text_column_is_preserved() {
    let path = std::env::temp_dir().join("wind_trail_ws.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: t\ncolumns:\n  - code\n  - text\n...").unwrap();
        writeln!(f, "ka\t甲   ").unwrap(); // 末列尾随裸空格 → 属词条内容，须保留
        writeln!(f, "yi\t乙\\t").unwrap(); // 转义制表在末列 → 存活
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        collect(&e, "甲   "),
        vec![("ka".to_string(), 0)],
        "末列 text 的尾随空格是词条内容，必须原样保留"
    );
    assert!(
        collect(&e, "甲").is_empty(),
        "不得再产出被剥过空白的形态（那会与真·无空格词条撞成同一条）"
    );
    assert_eq!(
        collect(&e, "乙\t"),
        vec![("yi".to_string(), 0)],
        "转义序列在 trim 阶段是可见字符，必须活到反转义那一步"
    );
}

/// TextFirst 布局下末列是 code/weight，它们的尾随空白仍须剥掉（只可能是排版噪声）；
/// 否则会出现 `"abc "` 这样打不出来的编码、和 `"5 ".parse()` 失败退化成 0 的权重。
#[test]
fn trailing_whitespace_in_code_and_weight_still_trimmed() {
    let path = std::env::temp_dir().join("wind_trail_ws_code.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: t2\n...").unwrap(); // 无声明 → 默认 text/code/weight
        writeln!(f, "甲\tka  ").unwrap(); // code 落末列，带尾随空格
        writeln!(f, "乙\tyi\t5 ").unwrap(); // weight 落末列，带尾随空格
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        collect(&e, "甲"),
        vec![("ka".to_string(), 0)],
        "末列 code 的尾随空格须剥除，否则这条编码永远打不出来"
    );
    assert_eq!(
        collect(&e, "乙"),
        vec![("yi".to_string(), 5)],
        "末列 weight 的尾随空格须剥除，否则 parse 失败静默退化成 0"
    );
}

/// 蒙古文 toli 的真实形态：CodeFirst 两列、词条内部与末尾都有空格（蒙文的词间分隔），
/// 都必须原样保留。
#[test]
fn mongolian_two_column_dict_keeps_inner_and_trailing_spaces() {
    let path = std::env::temp_dir().join("wind_toli_shape.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "---\nname: toli\ncolumns:\n  - code\n  - text\n  - weight\n..."
        )
        .unwrap();
        writeln!(f, "a\t\u{e249} ").unwrap(); // 单字 + 尾随空格
        writeln!(f, "aabn\t\u{e226}\u{e2f4} \u{e341}\u{e2ca} ").unwrap(); // 词组：内部 + 尾随
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        collect(&e, "\u{e249} "),
        vec![("a".to_string(), 0)],
        "单字词条的尾随空格须保留"
    );
    assert_eq!(
        collect(&e, "\u{e226}\u{e2f4} \u{e341}\u{e2ca} "),
        vec![("aabn".to_string(), 0)],
        "词组的内部空格与尾随空格都是词间分隔，须整串原样保留"
    );
}

/// text 或 code 为空的行必须跳过：空 code 会让条目全挤进 `entries[""]`。
#[test]
fn empty_text_or_code_rows_are_skipped() {
    let path = std::env::temp_dir().join("wind_empty_fields.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: e\n...").unwrap();
        writeln!(f, "我\t").unwrap(); // code 空（行尾 trim 后仅一列，落列数门槛）
        writeln!(f, "\tabc").unwrap(); // text 空
        writeln!(f, "好\thao\t9").unwrap();
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(e.len(), 1, "只应留下完整的那一条：{e:?}");
    assert_eq!(collect(&e, "好"), vec![("hao".to_string(), 9)]);
}

/// 编码在前的拼音库不得被当成形码：librime 允许 `columns: [code, text, weight]`，只看列顺序
/// 会丢掉整张简拼表、音节边界全归零。空格是音节的正面证据，应胜过列顺序惯例。
#[test]
fn code_first_pinyin_dict_keeps_syllables() {
    let py = std::env::temp_dir().join("wind_syl_codefirst.dict.yaml");
    {
        let mut f = std::fs::File::create(&py).unwrap();
        writeln!(
            f,
            "---\nname: p\ncolumns:\n  - code\n  - text\n  - weight\n..."
        )
        .unwrap();
        writeln!(f, "ni hao\t你好\t1200").unwrap();
        writeln!(f, "ni\t你\t800").unwrap();
    }
    let (e, ab) = parse_rime_entries_parallel(&py, false).unwrap();
    let _ = std::fs::remove_file(&py);
    assert_eq!(collect(&e, "你好"), vec![("nihao".to_string(), 1200)]);
    assert_eq!(
        boundary_of(&e, "你好"),
        Some(0b101),
        "code 在前的拼音库同样应保留音节边界"
    );
    assert_eq!(
        collect_ab(&ab, "nihao"),
        vec![("nh".to_string(), 1200)],
        "code 在前的拼音库不应丢简拼表（键为全拼码，v5）"
    );
}

/// 编码在前 + 无空格 → 五笔类形码，boundary 恒 0（无边界信息，消费方降级 DAG）。
/// 对照：text 在前且无空格的单音节拼音词条（`好\thao`）必须保留 `0b1`，双拼真值校验要用它。
/// 无空格时数据本身区分不了这两者，故沿用列顺序惯例兜底。
#[test]
fn spaceless_code_first_is_form_code_but_text_first_keeps_single_syllable() {
    let wubi = std::env::temp_dir().join("wind_syl_wubi.dict.yaml");
    {
        let mut f = std::fs::File::create(&wubi).unwrap();
        writeln!(
            f,
            "---\nname: w\ncolumns:\n  - code\n  - text\n  - weight\n..."
        )
        .unwrap();
        writeln!(f, "aaaa\t工\t99").unwrap();
    }
    let (e, ab) = parse_rime_entries_parallel(&wubi, false).unwrap();
    let _ = std::fs::remove_file(&wubi);
    assert_eq!(boundary_of(&e, "工"), Some(0), "形码不应有音节边界");
    assert!(ab.is_empty(), "形码不应产出简拼");

    let py = std::env::temp_dir().join("wind_syl_single.dict.yaml");
    {
        let mut f = std::fs::File::create(&py).unwrap();
        writeln!(f, "---\nname: s\n...").unwrap(); // 无声明 → 探测得 text 在前
        writeln!(f, "好\thao\t2000").unwrap();
    }
    let (e2, _) = parse_rime_entries_parallel(&py, false).unwrap();
    let _ = std::fs::remove_file(&py);
    assert_eq!(
        boundary_of(&e2, "好"),
        Some(0b1),
        "单音节拼音词条的「整串一个音节」是真信息，不得归零"
    );
}

/// `# no comment` 是整行精确匹配：行中间出现、或带后缀的同名子串都不算。
#[test]
fn no_comment_directive_requires_exact_line() {
    assert_eq!(find_no_comment_directive("a\n# no comment\nb\n"), Some(2));
    assert_eq!(find_no_comment_directive("# no comment"), Some(0)); // 无行尾换行
    assert_eq!(find_no_comment_directive("x\n# no comment\r\ny"), Some(2)); // CRLF
    assert_eq!(
        find_no_comment_directive("# no commentX\n"),
        None,
        "带后缀不算"
    );
    // librime 先 trim_right 整行再比较，故尾随空白仍算指令（与数据行同一套行尾规则）
    assert_eq!(
        find_no_comment_directive("# no comment  \n"),
        Some(0),
        "尾随空格应算"
    );
    assert_eq!(
        find_no_comment_directive("a\n# no comment\t\r\nb"),
        Some(2),
        "尾随制表符应算"
    );
    assert_eq!(
        find_no_comment_directive("a\t# no comment\n"),
        None,
        "行中间不算"
    );
    assert_eq!(
        find_no_comment_directive("## no comment\n"),
        None,
        "前缀多一个#不算"
    );
    assert_eq!(find_no_comment_directive("abc\ndef\n"), None);
    // 首个合法出现处生效（前面有个不合法的干扰项）
    assert_eq!(
        find_no_comment_directive("# no commentX\n# no comment\n"),
        Some(14)
    );
}

/// `# no comment` 之后 `#` 是数据：这是 Rime 让 `#` 本身能当词条/编码的唯一途径。
#[test]
fn hash_becomes_data_after_no_comment_directive() {
    let path = std::env::temp_dir().join("wind_no_comment.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: kf\n...").unwrap();
        writeln!(f, "# 这行在指令之前，仍是注释").unwrap();
        writeln!(f, "＃\th").unwrap(); // 全角井号，正常词条
        writeln!(f, "# no comment").unwrap();
        writeln!(f, "#\tj").unwrap(); // 指令之后：半角 # 是词条
        writeln!(f, "##\tk").unwrap(); // 连 ## 也是数据了
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(collect(&e, "＃"), vec![("h".to_string(), 0)]);
    assert_eq!(
        collect(&e, "#"),
        vec![("j".to_string(), 0)],
        "指令之后半角 # 应作为词条被收下：{e:?}"
    );
    assert_eq!(collect(&e, "##"), vec![("k".to_string(), 0)]);
    // 指令之前的注释行与指令行自身都不该变成条目
    assert_eq!(e.len(), 3, "只应有 3 条：{e:?}");
}

/// 无指令时，`#` 一律仍是注释（保持既有行为，别把普通词库搞坏）。
#[test]
fn hash_stays_comment_without_directive() {
    let path = std::env::temp_dir().join("wind_no_directive.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: x\n...").unwrap();
        writeln!(f, "## 次选").unwrap(); // 第三方编辑器的分组名，仍按注释丢弃
        writeln!(f, "# 普通注释").unwrap();
        writeln!(f, "字\tabc\t5").unwrap();
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(e.len(), 1, "两行注释都应被丢弃：{e:?}");
    assert_eq!(collect(&e, "字"), vec![("abc".to_string(), 5)]);
}

/// 并行分块边界：指令在正文靠前，而后续 `#` 词条散落在各块。
/// 注释开关是跨行有状态的，若不预先求出全局偏移，后面的块会误判为「注释仍生效」。
#[test]
fn no_comment_directive_survives_parallel_chunking() {
    let path = std::env::temp_dir().join("wind_no_comment_parallel.dict.yaml");
    let n = 60_000; // 正文 > 1MB，触发并行分块
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        writeln!(
            f,
            "---\nname: big\ncolumns:\n  - text\n  - code\n  - weight\n..."
        )
        .unwrap();
        writeln!(f, "# no comment").unwrap();
        for i in 0..n {
            // 每条 text 都以 # 开头：若某块误判注释仍生效，该块条目会整批消失
            writeln!(f, "#{i}\tc{i}\t{i}").unwrap();
        }
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(e.len(), n, "分块后 # 词条不得丢失");
    assert_eq!(collect(&e, "#0"), vec![("c0".to_string(), 0)]);
    assert_eq!(collect(&e, "#59999"), vec![("c59999".to_string(), 59999)]);
}

/// 缺 `...` 分隔行 → 零条目（并有 warn）。
#[test]
fn missing_body_separator_yields_zero_entries() {
    let path = std::env::temp_dir().join("wind_no_sep.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: broken\n你好\tni hao\t1").unwrap(); // 没有 `...`
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(e.is_empty());
}

/// 未声明 `columns:` 时按 librime 默认取第 3 列作权重，第 4 列及以后一律不读。
/// 样本取自 `wubi86_jidian_extra_district.dict.yaml`：`text\tcode\t\t区号` 四列，
/// 第 3 列为空（→ 权重 0）、第 4 列是行政区划编号，不得被当成权重。
#[test]
fn undeclared_columns_ignore_everything_past_weight() {
    let path = std::env::temp_dir().join("wind_cols_undeclared_extra.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: district\n...").unwrap(); // 无 columns 声明
        writeln!(f, "北京市\tuyym\t\t110000").unwrap();
        writeln!(f, "东城区\tafaq\t\t110101").unwrap();
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        collect(&e, "北京市"),
        vec![("uyym".to_string(), 0)],
        "区号列不得被当作权重"
    );
    assert_eq!(collect(&e, "东城区"), vec![("afaq".to_string(), 0)]);
}

/// 未声明 `columns:` 时按 librime 默认 `[text, code, weight]` 取第 3 列作权重。
#[test]
fn undeclared_columns_follow_rime_default_weight() {
    let path = std::env::temp_dir().join("wind_cols_undeclared_w.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: x\n...").unwrap();
        writeln!(f, "你好\tni hao\t1200").unwrap();
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        collect(&e, "你好"),
        vec![("nihao".to_string(), 1200)],
        "未声明时应按 Rime 默认取第 3 列权重"
    );
}

/// 只有两列的词库（快符 `12_kf.dict.yaml` 全 26 行皆两列）不得因缺权重列被丢弃。
/// 权重是「有则取、无则 0」，不能进最低列数门槛。
#[test]
fn two_column_rows_survive_when_weight_column_absent() {
    let path = std::env::temp_dir().join("wind_cols_two_only.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: kf\n...").unwrap();
        writeln!(f, "、\ty").unwrap();
        writeln!(f, "@\tt").unwrap();
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(e.len(), 2, "两列行不得被整体丢弃: {e:?}");
    assert_eq!(collect(&e, "@"), vec![("t".to_string(), 0)]);
}

/// 声明与内容形态冲突时以声明为准：英文词库两列都是 ASCII，探测必然弃权，
/// 只有声明能救（如 wubi86_jidian_english 的 `abs\tABS\t100`）。
#[test]
fn declared_columns_win_over_ambiguous_content() {
    let path = std::env::temp_dir().join("wind_cols_declared.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "---\nname: en\ncolumns:\n  - code\n  - text\n  - weight\n..."
        )
        .unwrap();
        writeln!(f, "abs\tABS\t100").unwrap();
        writeln!(f, "adob\tAdobe\t20").unwrap();
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        collect(&e, "ABS"),
        vec![("abs".to_string(), 100)],
        "声明 code 在前 → text=ABS/code=abs"
    );
}

/// 纯 ASCII 词条（快符 `@`、ASCII 参数的 `$CC(...)`）不得被当成码列与编码对调、装出镜像
/// 垃圾词条。列序须由全文一次判定，不受单行影响。
#[test]
fn ascii_text_entries_not_mistaken_for_code_column() {
    let path = std::env::temp_dir().join("wind_kf_ascii.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // 无 columns: 声明 —— 正是 12_kf.dict.yaml 的情况，走探测
        writeln!(f, "---\nname: kf\n...").unwrap();
        writeln!(f, "｀\tq").unwrap(); // 全角，非 ASCII → 投 TextFirst
        writeln!(f, "、\ty").unwrap();
        writeln!(f, "@\tt").unwrap(); // 纯 ASCII 词条：曾被判反
        writeln!(f, "$CC(last(), type(last()))\tf").unwrap(); // 纯 ASCII 命令：曾被判反
        writeln!(f, r#"$CC("[End]", key.seq("End"))	n"#).unwrap();
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        collect(&e, "@"),
        vec![("t".to_string(), 0)],
        "敲 t 应出 @（此前反成 code=@ / text=t）"
    );
    assert_eq!(
        collect(&e, "$CC(last(), type(last()))"),
        vec![("f".to_string(), 0)],
        "纯 ASCII 的 $CC 命令同样不得反转"
    );
    assert_eq!(
        collect(&e, r#"$CC("[End]", key.seq("End"))"#),
        vec![("n".to_string(), 0)]
    );
    // 非 ASCII 词条保持正确（回归保护）
    assert_eq!(collect(&e, "｀"), vec![("q".to_string(), 0)]);
}

/// 无声明的 code-first 词库仍应探测正确（码列含数字，text 为汉字）。
#[test]
fn detects_code_first_without_declaration() {
    let body = "a\t工\t9999\nggg\t三\t100\ncode1\t文\t5\n";
    let (layout, tf, cf) = detect_layout(body, None);
    assert_eq!(layout, ColumnLayout::CodeFirst, "票数 text={tf} code={cf}");
    assert_eq!((tf, cf), (0, 3));
}

/// 无声明的 text-first 词库（含纯 ASCII 词条）探测应判 TextFirst，
/// 且纯 ASCII 行不干扰多数票。
#[test]
fn detects_text_first_without_declaration() {
    let body = "你好\tni hao\t1200\n@\tt\n、\ty\n";
    let (layout, tf, cf) = detect_layout(body, None);
    assert_eq!(layout, ColumnLayout::TextFirst, "票数 text={tf} code={cf}");
    assert_eq!((tf, cf), (3, 0), "`@\\tt` 也应投 TextFirst（@ 不是码形态）");
}

/// 两列都像码 / 都不像码 → 弃权，不瞎猜；零票时落到默认 TextFirst。
#[test]
fn ambiguous_lines_abstain_and_default_to_text_first() {
    assert_eq!(
        vote_layout("abandon\tabandon", true),
        None,
        "两列都像码 → 弃权"
    );
    assert_eq!(vote_layout("你好\t、", true), None, "两列都不像码 → 弃权");
    assert_eq!(vote_layout("# 注释\tx", true), None);
    assert_eq!(vote_layout("单列无tab", true), None);
    let (layout, tf, cf) = detect_layout("abandon\tabandon\nABC\tABC\n", None);
    assert_eq!(layout, ColumnLayout::TextFirst, "零票应落默认");
    assert_eq!((tf, cf), (0, 0));
}

/// 列序是文件级属性：同一文件内少数派行不得把自己那行翻过来。
#[test]
fn layout_is_file_level_not_per_line() {
    let path = std::env::temp_dir().join("wind_layout_filelevel.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: mixed\n...").unwrap();
        for i in 0..10 {
            writeln!(f, "字{i}\tcode{i}\t{i}").unwrap(); // 多数：TextFirst
        }
        writeln!(f, "~\tz").unwrap(); // 纯 ASCII 词条，仍须按 TextFirst 解
    }
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        collect(&e, "~"),
        vec![("z".to_string(), 0)],
        "少数派 ASCII 行须服从文件级列序"
    );
    assert_eq!(collect(&e, "字0"), vec![("code0".to_string(), 0)]);
}

/// 列序纠错只认编码列里的 `$SS(` 类 marker：单凭 `{` 不对调（反例）；
/// 真有 marker 时照旧对调（正例）。
#[test]
fn cmdbar_swap_requires_marker_not_bare_brace() {
    let path =
        std::env::temp_dir().join(format!("wind_cmdbar_swap_{}.dict.yaml", std::process::id()));
    std::fs::write(
        &path,
        "---\nname: t\ncolumns:\n  - text\n  - code\n...\n甲\t{ab}\n$SS(\"x\", \"y\")\tuu\n",
    )
    .unwrap();
    let (e, _) = parse_rime_entries_parallel(&path, false).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        collect(&e, "甲"),
        vec![("{ab}".to_string(), 0)],
        "{{ 不是对调依据"
    );
    assert_eq!(
        collect(&e, "$SS(\"x\", \"y\")"),
        vec![("uu".to_string(), 0)],
        "marker 在编码列 ⇒ 对调"
    );
}

/// BTreeMap 通配：以首个通配前的字面前缀 range 扫描 + 逐条过滤；等长档先取满。
#[test]
fn search_pattern_scans_literal_prefix_and_tiers_equal_length_first() {
    let mut d = CodetableDict::empty();
    for (code, text, w) in [
        ("ab", "甲", 10),
        ("ac", "乙", 20),
        ("abcd", "丙", 9999),
        ("bb", "丁", 30),
        ("abd", "戊", 5),
    ] {
        d.merge_single(code.into(), text.into(), w, 0);
    }
    let got = |p: &str, lim: usize, wp: bool| -> Vec<String> {
        d.search_pattern(p, '?', lim, wp)
            .into_iter()
            .map(|h| h.text)
            .collect()
    };
    assert_eq!(got("a?", 10, false), ["乙", "甲"]);
    assert_eq!(got("a?", 10, true), ["乙", "甲", "丙", "戊"]);
    assert_eq!(got("a?", 1, true), ["乙"], "名额先给等长档");
    assert_eq!(
        got("?b", 10, false),
        ["丁", "甲"],
        "首位通配：字面前缀为空，全表扫"
    );
    assert_eq!(got("??d", 10, false), ["戊"]);
    assert!(got("x?", 10, true).is_empty());
}

/// 对拍：`search_pattern` == `for_each_entry` 全遍历 + `pattern_matches` 过滤 + 同口径排序截断。
#[test]
fn search_pattern_matches_full_scan() {
    let alphabet = ['a', 'b', 'c'];
    let mut d = CodetableDict::empty();
    let mut i = 0i32;
    for len in 1..=4u32 {
        for k in 0..3usize.pow(len) {
            let mut code = String::new();
            let mut x = k;
            for _ in 0..len {
                code.push(alphabet[x % 3]);
                x /= 3;
            }
            i += 1;
            if i % 5 == 0 {
                continue;
            }
            let w = (i * 7919) % 7 * 100; // 权重大量重复，逼出 tie-break
            d.merge_single(code.clone(), format!("甲{i}"), w, 0);
            if i % 4 == 0 {
                d.merge_single(code, format!("乙{i}"), w, 0);
            }
        }
    }
    let reference =
        |pat: &str, limit: usize, with_prefix: bool| -> Vec<(String, String, i32, i32)> {
            let n = pat.chars().count();
            let mut seq = std::collections::HashMap::<String, i32>::new();
            let (mut exact, mut longer) = (Vec::new(), Vec::new());
            d.for_each_entry(&mut |code, text, weight| {
                // for_each_entry 按码内顺序给出，序号即 order
                let order = seq.entry(code.to_string()).or_insert(0);
                let o = *order;
                *order += 1;
                if !crate::layer::pattern_matches(pat, '?', code, with_prefix) {
                    return;
                }
                let row = (code.to_string(), text.to_string(), weight, o);
                if code.chars().count() == n {
                    exact.push(row);
                } else {
                    longer.push(row);
                }
            });
            let key = |a: &(String, String, i32, i32), b: &(String, String, i32, i32)| {
                b.2.cmp(&a.2)
                    .then(a.3.cmp(&b.3))
                    .then_with(|| a.0.cmp(&b.0))
            };
            exact.sort_by(key);
            longer.sort_by(key);
            exact.truncate(limit);
            longer.truncate(limit - exact.len());
            exact.extend(longer);
            exact
        };
    for pat in [
        "?", "a?", "?b", "??", "a?c", "??c?", "????", "c??a", "?a", "abc",
    ] {
        for with_prefix in [false, true] {
            for limit in [1usize, 3, 10, 1000] {
                let got: Vec<_> = d
                    .search_pattern(pat, '?', limit, with_prefix)
                    .into_iter()
                    .map(|h| (h.code, h.text, h.weight, h.order))
                    .collect();
                assert_eq!(
                    got,
                    reference(pat, limit, with_prefix),
                    "pat={pat} with_prefix={with_prefix} limit={limit}"
                );
            }
        }
    }
}
#[test]
fn rime_layout_exposes_body_columns_and_comment_cutoff() {
    let content = "---\nname: x\ncolumns:\n  - code\n  - text\n  - weight\n...\n# 注释\nni\t你\t1\n# no comment\n#hash\t井\t2\n";
    let l = rime_layout(content, std::path::Path::new("x.dict.yaml")).unwrap();
    assert!(content[l.body_offset..].starts_with("# 注"));
    assert_eq!((l.text_col, l.code_col, l.weight_col), (1, 0, Some(2)));
    let cut = l.comment_cutoff.unwrap();
    assert!(content[cut..].starts_with("# no comment"));
    assert!(rime_layout("no body marker", std::path::Path::new("y")).is_none());
    let d = rime_layout("---\n...\n你好\tnihao\n", std::path::Path::new("z")).unwrap();
    assert_eq!(
        (d.text_col, d.code_col, d.weight_col),
        (0, 1, Some(2)),
        "未声明 = librime 默认列序"
    );
}
