use super::*;
use crate::codetable::CodetableDict;

/// 两库各有独占码与共有码：归并后码升序、共有码的条目按库序拼成一组，层号对得上；
/// mmap 与内存两种形态混用时结果一致。
#[test]
fn code_groups_merge_layers_in_byte_order() {
    let mut a = CodetableDict::empty();
    a.merge_single("aa".into(), "甲".into(), 10, 0);
    a.merge_single("ab".into(), "乙".into(), 5, 0);
    a.merge_single("ab".into(), "丙".into(), 3, 0);
    let mut b = CodetableDict::empty();
    b.merge_single("a".into(), "丁".into(), 1, 0);
    b.merge_single("ab".into(), "戊".into(), 7, 0);
    b.merge_single("b".into(), "己".into(), 2, 0);

    let dir = std::env::temp_dir().join(format!("wind-groups-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut w = WdatWriter::new();
    a.export_to_wdat(&mut w);
    let wdat = dir.join("a.wdat");
    w.write(&wdat).unwrap();
    let mm = CachedDict::Mmap(Arc::new(WdatReader::open(&wdat).unwrap()));
    let mem_b = CachedDict::Memory(b);

    type Got = (String, Vec<(usize, String, i32, i32)>);
    let mut got: Vec<Got> = Vec::new();
    for_each_code_group(&[&mm, &mem_b], &mut |code, es| {
        got.push((
            code.to_string(),
            es.iter()
                .map(|e| (e.layer, e.text.to_string(), e.weight, e.order))
                .collect(),
        ));
    });
    let s = |x: &str| x.to_string();
    assert_eq!(
        got,
        vec![
            (s("a"), vec![(1, s("丁"), 1, 0)]),
            (s("aa"), vec![(0, s("甲"), 10, 0)]),
            (
                s("ab"),
                vec![(0, s("乙"), 5, 0), (0, s("丙"), 3, 1), (1, s("戊"), 7, 0)]
            ),
            (s("b"), vec![(1, s("己"), 2, 0)]),
        ]
    );
    drop(mm);
    std::fs::remove_dir_all(&dir).ok();
}

/// 单音节 code 的 boundary（只有起点位）。
fn b1() -> u64 {
    0b1
}

/// `shen|me` 这类两音节 code 的 boundary（起点 + 第二音节起始字节位）。
fn b2(second: usize) -> u64 {
    0b1 | (1 << second)
}

/// 合法音节输入：更长音节的条目必须被拒，下一个音节的补全必须放行。
///
/// 防「打 `shen` 出 `sheng` 的字」。两者音节数都是 1，[`prefix_syllable_keep`] 分不开，
/// 只能靠「输入末尾是不是候选的音节起点」。
#[test]
fn aligned_rejects_longer_syllable_but_keeps_next_syllable() {
    // 输入 shen（completed_len = 4）
    // sheng（生）：单音节，第 4 字节不是音节起点 ⇒ 拒
    assert!(!prefix_syllable_aligned(b1(), "sheng".len(), 4));
    // shenme（什么）：shen|me，第 4 字节正是「me」的起点 ⇒ 放行
    assert!(prefix_syllable_aligned(b2(4), "shenme".len(), 4));
    // shenmei（审美）：shen|mei，同样对齐 ⇒ 放行（它确实是 shen 的合法补全）
    assert!(prefix_syllable_aligned(b2(4), "shenmei".len(), 4));
    // 输入 fen（completed_len = 3）对 feng：拒
    assert!(!prefix_syllable_aligned(b1(), "feng".len(), 3));
    // 输入 ni（completed_len = 2）对 ning/niu/nian：全拒
    for code in ["ning", "niu", "nian", "niang", "niao", "nin", "nie"] {
        assert!(
            !prefix_syllable_aligned(b1(), code.len(), 2),
            "打 ni 不该召回单音节的 {code}"
        );
    }
    // 而 nihao（ni|hao）对齐 ⇒ 放行
    assert!(prefix_syllable_aligned(b2(2), "nihao".len(), 2));
}

/// `completed_len == 0`（输入不成音节：`fe`/`zh`/`m`）⇒ 判据整条让开。
///
/// 位 0 恒为音节起点，故这不是特例分支而是同一个判据的自然退化。主流全拼输入法
/// 正是这个行为：`fe` 不是合法拼音故走字符前缀匹配，`fen` 是合法音节故按音节匹配。
#[test]
fn illegal_syllable_falls_back_to_plain_prefix_match() {
    for code in ["fen", "feng", "fei", "fa"] {
        assert!(
            prefix_syllable_aligned(b1(), code.len(), 0),
            "输入不成音节时 {code} 必须放行，否则前缀匹配整条失效"
        );
    }
}

/// 两处放行：无边界信息、以及「不是补全」。
#[test]
fn aligned_passes_unknown_boundary_and_non_completions() {
    // boundary == 0：旧词典 / 用户手输码 / 超 64 字节长码 ⇒ 词库层判不了，放行
    assert!(prefix_syllable_aligned(0, "sheng".len(), 4));
    // code 不比已完成音节长 ⇒ 精确匹配或子短语，没有预测任何未输入内容
    assert!(prefix_syllable_aligned(b1(), 4, 4));
    assert!(prefix_syllable_aligned(b1(), 2, 4));
    // completed_len >= 64：bitmask 表达不了，与 boundary 的 64 字节天花板同款降级
    assert!(prefix_syllable_aligned(b1(), 100, 64));
    assert!(prefix_syllable_aligned(b1(), 100, 999));
}

/// 合成判据 = 音节数上限且边界对齐；两道各自独立，缺一不可。
#[test]
fn entry_keep_requires_both_predicates() {
    // 音节数合格但不对齐（sheng 对 shen）⇒ 拒
    assert!(!prefix_entry_keep(b1(), "sheng".len(), 1, 4));
    // 对齐但音节数超限（shenme 2 音节、上限 1）⇒ 拒
    assert!(!prefix_entry_keep(b2(4), "shenme".len(), 1, 4));
    // 两道都过 ⇒ 留
    assert!(prefix_entry_keep(b2(4), "shenme".len(), 2, 4));
}

#[test]
fn wdat_sibling_strips_the_whole_dict_yaml_suffix() {
    let p = Path::new("schemas/wubi86/wubi86_jidian.dict.yaml");
    assert_eq!(
        wdat_sibling(p).unwrap(),
        Path::new("schemas/wubi86/wubi86_jidian.wdat"),
        "须剥掉整个 .dict.yaml（与 Go 版 wdbOnlyInDir 一致）"
    );
    // 对照：with_extension 只换最后一级，会得到错误的名字——这正是不能用它的原因
    assert_eq!(
        p.with_extension("wdat"),
        Path::new("schemas/wubi86/wubi86_jidian.dict.wdat")
    );
    // 裸 .yaml 也支持
    assert_eq!(
        wdat_sibling(Path::new("a/b.yaml")).unwrap(),
        Path::new("a/b.wdat")
    );
    // 非 yaml 不推导
    assert!(wdat_sibling(Path::new("a/b.txt")).is_none());
    assert!(wdat_sibling(Path::new("a/b.wdat")).is_none());
}

fn wdat_only_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind-wdat-only-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// wdat-only 分发：方案目录里只有编译好的 .wdat，没有 .dict.yaml 源。
#[test]
fn loads_wdat_only_when_yaml_is_absent() {
    let dir = wdat_only_dir("load");
    let yaml = dir.join("x.dict.yaml"); // 故意不创建
    let wdat = dir.join("x.wdat");
    let mut d = CodetableDict::empty();
    d.merge_single("a".into(), "啊".into(), 1, 0);
    let mut w = WdatWriter::new();
    d.export_to_wdat(&mut w);
    w.write(&wdat).unwrap();

    // 缓存路径同样不存在——wdat-only 必须绕过整个指纹/缓存流程
    let cache = dir.join("cache").join("x.wdat");
    let loaded = CachedDict::load_at_with(&yaml, &cache, false)
        .expect("yaml 缺失但同名 wdat 在场时应走 wdat-only");
    assert_eq!(loaded.search("a").len(), 1);
    assert!(
        !cache.exists(),
        "wdat-only 不应生成缓存副本，应原位直接 mmap"
    );

    drop(loaded);
    std::fs::remove_dir_all(&dir).ok();
}

/// 无源可重建时必须明确报错，而不是静默降级成「整方案无引擎」。
#[test]
fn wdat_only_unreadable_file_errors_with_actionable_message() {
    let dir = wdat_only_dir("corrupt");
    let yaml = dir.join("y.dict.yaml"); // 不存在
    let wdat = dir.join("y.wdat");
    std::fs::write(&wdat, b"this is not a valid wdat").unwrap();

    let Err(err) = CachedDict::load_at_with(&yaml, &dir.join("cache/y.wdat"), false) else {
        panic!("损坏的 wdat-only 词库必须报错，不能静默降级");
    };
    let msg = err.to_string();
    assert!(msg.contains("wdat-only"), "须指明是 wdat-only 模式: {msg}");
    assert!(msg.contains("无法重建"), "须告知无源可重建: {msg}");

    std::fs::remove_dir_all(&dir).ok();
}

/// yaml 在场时不得被 wdat 抢走——正常方案的行为一步都不能变。
#[test]
fn yaml_present_still_takes_the_normal_path() {
    let dir = wdat_only_dir("yaml-wins");
    let yaml = dir.join("z.dict.yaml");
    std::fs::write(&yaml, "# comment\nzz\t再\t1\n").unwrap();
    // 同时放一个内容不同的 wdat，若被误用则查得到 "抢"
    let mut d = CodetableDict::empty();
    d.merge_single("qq".into(), "抢".into(), 1, 0);
    let mut w = WdatWriter::new();
    d.export_to_wdat(&mut w);
    w.write(dir.join("z.wdat")).unwrap();

    let loaded = CachedDict::load_at_with(&yaml, &dir.join("cache/z.wdat"), false).unwrap();
    assert!(
        loaded.search("qq").is_empty(),
        "yaml 存在时绝不能走 wdat-only 分支"
    );

    drop(loaded);
    std::fs::remove_dir_all(&dir).ok();
}

/// 全码判据①②：简码不能胜出（否则公式取第 2 码越界），且超过 max_code_length 的
/// 怪码被上限闸排除——这两条各自都足以让造词静默失效。
#[test]
fn single_char_full_code_prefers_longest_within_cap() {
    let mut d = CodetableDict::empty();
    // 「工」：一级简码 a + 全码 aaaa → 应取 aaaa（简码只有 1 位，取不到第 2 码）。
    d.merge_single("a".into(), "工".into(), 9999, 0);
    d.merge_single("aaaa".into(), "工".into(), 100, 1);
    // 「中」：全码 khk + 扩展库塞进来的 6 码怪码 → 上限闸(4) 排除怪码，取 khk。
    d.merge_single("khk".into(), "中".into(), 500, 2);
    d.merge_single("khkkhk".into(), "中".into(), 9999, 3);
    let cd = CachedDict::Memory(d);
    let idx = cd.build_single_char_full_codes(4);
    assert_eq!(
        idx.get(&'工').map(String::as_str),
        Some("aaaa"),
        "简码权重再高也不能当全码"
    );
    assert_eq!(
        idx.get(&'中').map(String::as_str),
        Some("khk"),
        "超 max_code_length 的怪码应被上限闸排除，即使它更长、权重更高"
    );
}

/// 上限闸关闭（0）时退化为纯「最长优先」——非定长码方案不应被误滤。
#[test]
fn single_char_full_code_cap_zero_disables_gate() {
    let mut d = CodetableDict::empty();
    d.merge_single("khk".into(), "中".into(), 500, 0);
    d.merge_single("khkkhk".into(), "中".into(), 100, 1);
    let cd = CachedDict::Memory(d);
    assert_eq!(
        cd.build_single_char_full_codes(0)
            .get(&'中')
            .map(String::as_str),
        Some("khkkhk"),
        "cap=0 应不设闸，取最长"
    );
}

/// 全码判据③④：同码长先比权重，权重相同再比码字典序（确定性兜底）。
#[test]
fn single_char_full_code_breaks_ties_by_weight_then_code() {
    let mut d = CodetableDict::empty();
    // 同为 2 码：权重高的 de 胜出。
    d.merge_single("dd".into(), "大".into(), 10, 0);
    d.merge_single("de".into(), "大".into(), 99, 1);
    // 同为 2 码且同权重：字典序小的 aa 胜出。
    d.merge_single("ab".into(), "式".into(), 50, 2);
    d.merge_single("aa".into(), "式".into(), 50, 3);
    let cd = CachedDict::Memory(d);
    let idx = cd.build_single_char_full_codes(4);
    assert_eq!(idx.get(&'大').map(String::as_str), Some("de"));
    assert_eq!(idx.get(&'式').map(String::as_str), Some("aa"));
}

/// 只收单字：多字词条不得进入全码表（否则「你好」会被当成一个"字"）。
#[test]
fn single_char_full_code_skips_multi_char_entries() {
    let mut d = CodetableDict::empty();
    d.merge_single("wqvb".into(), "你好".into(), 100, 0);
    d.merge_single("wqiy".into(), "你".into(), 100, 1);
    let cd = CachedDict::Memory(d);
    let idx = cd.build_single_char_full_codes(4);
    assert_eq!(idx.len(), 1, "多字词条应被跳过");
    assert!(idx.contains_key(&'你'));
}

/// 反查索引:同词收集全部码,码长升序 → 字典序升序,去重。
#[test]
fn reverse_index_collects_all_codes_sorted_by_len() {
    let mut d = CodetableDict::empty();
    // 「工」简码+全码 → 短码在前。
    d.merge_single("aaaa".into(), "工".into(), 100, 0);
    d.merge_single("a".into(), "工".into(), 100, 1);
    // 「中」唯一码 + 重复条目去重。
    d.merge_single("k".into(), "中".into(), 50, 2);
    d.merge_single("k".into(), "中".into(), 40, 3);
    // 「大」同长两码 → 字典序。
    d.merge_single("de".into(), "大".into(), 10, 4);
    d.merge_single("dd".into(), "大".into(), 99, 5);
    let cd = CachedDict::Memory(d);
    let idx = cd.build_reverse_index();
    assert_eq!(idx.len(), 3);
    assert_eq!(
        idx.codes_of("工").unwrap().iter().collect::<Vec<_>>(),
        vec!["a", "aaaa"]
    );
    assert_eq!(
        idx.codes_of("中").unwrap().iter().collect::<Vec<_>>(),
        vec!["k"],
        "重复条目应去重"
    );
    assert_eq!(
        idx.codes_of("大").unwrap().iter().collect::<Vec<_>>(),
        vec!["dd", "de"],
        "同长按字典序"
    );
    assert!(idx.codes_of("无").is_none());
    // 消费侧的两种取法
    assert_eq!(
        idx.codes_of("工").unwrap().last(),
        Some("aaaa"),
        "末位=全码"
    );
    assert_eq!(idx.codes_of("工").unwrap().join("/"), "a/aaaa");
}

#[test]
fn reverse_index_arena_boundaries_dont_bleed() {
    // arena 是连续拼接的,相邻词/码若边界算错会串味。用等长且互为前缀的样本压这个边界。
    let mut d = CodetableDict::empty();
    d.merge_single("aa".into(), "甲".into(), 1, 0);
    d.merge_single("aaa".into(), "甲".into(), 1, 1);
    d.merge_single("bb".into(), "乙".into(), 1, 2);
    d.merge_single("cc".into(), "丙".into(), 1, 3);
    let idx = CachedDict::Memory(d).build_reverse_index();
    assert_eq!(
        idx.codes_of("甲").unwrap().iter().collect::<Vec<_>>(),
        vec!["aa", "aaa"]
    );
    assert_eq!(
        idx.codes_of("乙").unwrap().iter().collect::<Vec<_>>(),
        vec!["bb"]
    );
    // 「丙」按字节序排首位,其编码起点必须是 0 而非前一条目的终点
    assert_eq!(
        idx.codes_of("丙").unwrap().iter().collect::<Vec<_>>(),
        vec!["cc"]
    );
    assert!(idx.codes_of("").is_none(), "空词不应命中");
}

/// 「逐库并集」与「先合并再构建」必须产出完全相同的索引，这是不建 `combined.wdat` 的前提。
///
/// 样本覆盖三种合并才会遇到的冲突形态，任一处理错两侧就会分叉：
///   ① 同 (text, code) 在两库权重不同  ② 同 text 不同 code 分散在两库
///   ③ 同 code 不同 text 分散在两库
#[test]
fn union_of_dicts_equals_merged_dict_for_reverse_index() {
    // 两个分库。
    let mut a = CodetableDict::empty();
    a.merge_single("aaaa".into(), "工".into(), 100, 0);
    a.merge_single("a".into(), "工".into(), 50, 1); // ② 同词另一个码
    a.merge_single("khk".into(), "中".into(), 10, 2); // ① 低权重那份
    a.merge_single("de".into(), "大".into(), 7, 3); // ③ 同码不同词
    let mut b = CodetableDict::empty();
    b.merge_single("khk".into(), "中".into(), 999, 0); // ① 高权重那份
    b.merge_single("de".into(), "太".into(), 8, 1); // ③ 同码另一个词
    b.merge_single("khkkhk".into(), "中".into(), 5, 2);

    // 合并版：按 combined 的语义（同 code 聚合，同 text 取更高权重）手工合成一份。
    let mut merged = CodetableDict::empty();
    merged.merge_single("aaaa".into(), "工".into(), 100, 0);
    merged.merge_single("a".into(), "工".into(), 50, 1);
    merged.merge_single("khk".into(), "中".into(), 999, 2); // 取 max
    merged.merge_single("de".into(), "大".into(), 7, 3);
    merged.merge_single("de".into(), "太".into(), 8, 4);
    merged.merge_single("khkkhk".into(), "中".into(), 5, 5);

    let union = build_reverse_index_from(&[CachedDict::Memory(a), CachedDict::Memory(b)]);
    let via_merged = CachedDict::Memory(merged).build_reverse_index();

    assert_eq!(union.len(), via_merged.len(), "词数必须一致");
    assert_eq!(
        union.image(),
        via_merged.image(),
        "两条路径的序列化镜像必须逐字节相同——这是「逐位等价」最直接的判据\
             （此前只比了总字节数，两份布局不同而大小恰好相等时会漏过）"
    );
    for text in ["工", "中", "大", "太"] {
        let u: Vec<_> = union.codes_of(text).unwrap().iter().collect();
        let m: Vec<_> = via_merged.codes_of(text).unwrap().iter().collect();
        assert_eq!(u, m, "{text} 的编码列表必须一致");
    }
    // 权重取 max 这一点单独验：它决定词语联想的排序，错了很难被上面几条发现。
    assert_eq!(
        union.texts_with_prefix("中", 9),
        via_merged.texts_with_prefix("中", 9),
        "联想排序（依赖每词的 max 权重）必须一致"
    );
}

/// 同上，针对单字全码表——它与反查索引共用同一条「摆脱 combined」的依据。
#[test]
fn union_of_dicts_equals_merged_dict_for_single_char_codes() {
    let mut a = CodetableDict::empty();
    a.merge_single("a".into(), "工".into(), 9999, 0); // 简码，权重极高但不该胜出
    a.merge_single("khk".into(), "中".into(), 10, 1);
    let mut b = CodetableDict::empty();
    b.merge_single("aaaa".into(), "工".into(), 1, 0); // 全码，应胜出
    b.merge_single("khk".into(), "中".into(), 999, 1);
    b.merge_single("khkkhk".into(), "中".into(), 9999, 2); // 超上限闸，应被排除

    let mut merged = CodetableDict::empty();
    merged.merge_single("a".into(), "工".into(), 9999, 0);
    merged.merge_single("aaaa".into(), "工".into(), 1, 1);
    merged.merge_single("khk".into(), "中".into(), 999, 2);
    merged.merge_single("khkkhk".into(), "中".into(), 9999, 3);

    let union =
        build_single_char_full_codes_from(&[CachedDict::Memory(a), CachedDict::Memory(b)], 4);
    let via_merged = CachedDict::Memory(merged).build_single_char_full_codes(4);
    assert_eq!(union, via_merged);
    assert_eq!(union.get(&'工').map(String::as_str), Some("aaaa"));
    assert_eq!(union.get(&'中').map(String::as_str), Some("khk"));
}

#[test]
fn reverse_index_empty_is_empty() {
    let idx = CachedDict::Memory(CodetableDict::empty()).build_reverse_index();
    assert!(idx.is_empty());
    assert_eq!(idx.len(), 0);
    assert!(idx.codes_of("任意").is_none());
}

/// 同一槽位、两份不同内容的源（用户目录覆盖了自带的同名词库）交替加载，第二轮起零重建——
/// 两份各有各的文件，不再互相顶掉。
#[test]
fn alternating_sources_on_one_slot_do_not_rebuild_each_other() {
    let dir = std::env::temp_dir().join(format!("wind-slot-pingpong-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let yaml = dir.join("x.dict.yaml");
    let slot = dir.join("x.wdat");
    let a = "---\nname: x\n...\n啊\ta\t1\n";
    let b = "---\nname: x\n...\n啊\ta\t1\n吧\tb\t1\n";
    let set_old_mtime = |secs: u64| {
        std::fs::File::options()
            .write(true)
            .open(&yaml)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    };
    let load = |body: &str, secs: u64| {
        std::fs::write(&yaml, body).unwrap();
        set_old_mtime(secs);
        let d = CachedDict::load_at_with(&yaml, &slot, false).unwrap();
        d.source_file().expect("应走 mmap").to_path_buf()
    };
    let pa = load(a, 1_700_000_000);
    let pb = load(b, 1_700_000_100);
    assert_ne!(pa, pb);
    let mtime = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
    let (ma, mb) = (mtime(&pa), mtime(&pb));

    // 第二轮：回到 A 再回到 B，两份文件都还在且都没被重写
    assert_eq!(load(a, 1_700_000_000), pa);
    assert_eq!(load(b, 1_700_000_100), pb);
    assert_eq!(mtime(&pa), ma, "A 不该被重建");
    assert_eq!(mtime(&pb), mb, "B 不该被重建");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 内容寻址下重建永远写新文件名，旧文件与它的 `.fp` 留到被 prune。本用例守的是：源内容一变，
/// `source_file()` 换成另一个路径，二级缓存拿到的摘要随之不同。
#[test]
fn a_rebuild_lands_on_a_new_content_addressed_file() {
    let dir = std::env::temp_dir().join(format!("wind-stale-fp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let yaml = dir.join("x.dict.yaml");
    let slot = dir.join("x.wdat");
    std::fs::write(&yaml, "---\nname: x\n...\n啊\ta\t1\n").unwrap();
    let d1 = CachedDict::load_at_with(&yaml, &slot, false).unwrap();
    let p1 = d1.source_file().expect("首建应走 mmap").to_path_buf();
    let before = crate::cache_fp::cache_digest(&p1);
    assert!(
        before.starts_with("fp:"),
        "前提：首建应写出 .fp，实际 {before}"
    );
    assert_ne!(p1, slot, "实际文件名应带指纹，而不是槽位本身");
    drop(d1);

    std::fs::write(&yaml, "---\nname: x\n...\n啊\ta\t1\n吧\tb\t1\n").unwrap();
    let d2 = CachedDict::load_at_with(&yaml, &slot, false).unwrap();
    assert_eq!(d2.search("b").len(), 1, "前提：wdat 确已按新 yaml 重建");
    let p2 = d2.source_file().unwrap().to_path_buf();
    assert_ne!(p1, p2, "内容变了必须落到新文件");
    assert!(p1.is_file(), "旧文件留给 prune（keep=2），不被覆盖");
    assert_ne!(
        crate::cache_fp::cache_digest(&p2),
        before,
        "新文件的摘要必须与旧的不同——否则派生缓存会静默复用旧产物"
    );
    drop(d2);
    let _ = std::fs::remove_dir_all(&dir);
}
/// 英文词库按名次表赋权：前缀补全从字母序变成常用度序；写了权重的条目不动；
/// 名次表改了缓存要重建（它是 wdat 的第二个源）。
#[test]
fn english_ranks_reorder_prefix_and_invalidate_cache() {
    let dir = wdat_only_dir("en-ranks");
    let yaml = dir.join("en.dict.yaml");
    std::fs::write(
        &yaml,
        "---\nname: en\n...\nproduce\tproduce\nproduced\tproduced\nproduct\tproduct\nproductive\tproductive\t5\n",
    )
    .unwrap();
    let ranks = dir.join("en_freq.txt");
    let slot = dir.join("cache/en.wdat");
    let texts = |d: &CachedDict| -> Vec<String> {
        d.search_prefix("produc", 10)
            .into_iter()
            .map(|h| h.1)
            .collect()
    };

    // 反向对照：名次表不在 ⇒ 无权重条目照旧按文件顺序，只有写了权重的 productive 在前。
    let plain = CachedDict::load_english_at(&yaml, &slot, &ranks).unwrap();
    assert_eq!(
        texts(&plain),
        ["productive", "produce", "produced", "product"]
    );
    drop(plain);

    // productive 名列第一：若名次覆盖了它写明的权重 5，它就会跑到最前。
    std::fs::write(&ranks, "# 注释\nproductive\nproduct\nproduce\nproduced\n").unwrap();
    let ranked = CachedDict::load_english_at(&yaml, &slot, &ranks).unwrap();
    let got = texts(&ranked);
    assert_eq!(
        got[..3],
        ["product", "produce", "produced"],
        "名次表出现后须重建缓存并按名次排，实际 {got:?}"
    );
    // productive 写了权重 5：不被名次覆盖，排在有名次的词之后。
    assert_eq!(got[3], "productive", "写了权重的条目不得被名次覆盖");
    drop(ranked);
    std::fs::remove_dir_all(&dir).ok();
}
