use super::*;
use wind_candidate::Candidate;
use wind_dict::SystemDictLayer;
use wind_dict::cached::CachedDict;
use wind_dict::codetable::CodetableDict;

fn cand(code: &str, text: &str) -> Candidate {
    Candidate {
        code: code.to_string(),
        text: text.to_string(),
        ..Default::default()
    }
}

/// 护栏：检索范围放宽补回的候选不得影响满码自动上屏。
///
/// 部分自动上屏靠智能过滤滤掉同码生僻字才成立（见 `recheck_auto_commit_unique_after_filter`），
/// 放宽补回这些字时若不在计数时排除，满码即上屏会静默退化成要多按一次空格。
/// 设计见 docs/design/smart-filter-scope-relax.md §2.1。
#[test]
fn scope_filtered_candidates_do_not_block_auto_commit() {
    let mut relaxed = cand("hhnu", "愳");
    relaxed.is_scope_filtered = true; // 智能档下本应被滤，因放宽才在列表里
    let with_relaxed = [cand("hhnu", "X"), relaxed];
    assert_eq!(
        decide_auto_commit(true, 4, "hhnu", &with_relaxed, false).as_deref(),
        Some("X"),
        "放宽补回的同码生僻字不该否决满码自动上屏"
    );

    // 反向对照：同样两条候选、不带放宽标记 → 仍须照旧否决，防「无条件放行」的实现假绿。
    let both_normal = [cand("hhnu", "X"), cand("hhnu", "愳")];
    assert_eq!(
        decide_auto_commit(true, 4, "hhnu", &both_normal, false),
        None,
        "未经放宽的两个精确同码候选仍须否决上屏（放行了说明排除条件写宽了）"
    );

    // 边界：全部候选都是放宽补回的 → 无可上屏的正常候选，不上屏
    let mut only_relaxed = cand("hhnu", "愳");
    only_relaxed.is_scope_filtered = true;
    assert_eq!(
        decide_auto_commit(true, 4, "hhnu", &[only_relaxed], false),
        None,
        "只有放宽候选时不该拿它自动上屏"
    );
}

#[test]
fn decide_basic_unique_full() {
    let cands = [cand("aaaa", "工")];
    assert_eq!(
        decide_auto_commit(true, 4, "aaaa", &cands, false),
        Some("工".to_string())
    );
}

#[test]
fn decide_blocked_when_disabled_or_short() {
    let cands = [cand("aaaa", "工")];
    assert_eq!(decide_auto_commit(false, 4, "aaaa", &cands, false), None);
    // 码长不足 min_len
    assert_eq!(
        decide_auto_commit(true, 4, "aaa", &[cand("aaa", "x")], false),
        None
    );
}

#[test]
fn decide_blocked_when_ambiguous_or_has_longer() {
    // 两个精确匹配 → 不上屏
    let two = [cand("aaaa", "工"), cand("aaaa", "戈")];
    assert_eq!(decide_auto_commit(true, 4, "aaaa", &two, false), None);
    // 有更长后继 → 不上屏
    let one = [cand("aa", "式")];
    assert_eq!(decide_auto_commit(true, 2, "aa", &one, true), None);
}

fn engine_with(entries: &[(&str, &str, i32)], at_full: bool, min_len: usize) -> CodeTableEngine {
    engine_opts(
        entries,
        CommitOptions {
            auto_commit_at_full: at_full,
            auto_commit_min_len: min_len,
            ..Default::default()
        },
    )
}

/// 双词库夹具：主库 + 扩展库各一层，返回引擎与 `DictManager`（后者用于热启停扩展库）。
fn engine_two_dicts(
    main: &[(&str, &str, i32)],
    ext: &[(&str, &str, i32)],
) -> (CodeTableEngine, Arc<DictManager>) {
    let build = |entries: &[(&str, &str, i32)]| {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        CachedDict::Memory(d)
    };
    let dm = Arc::new(DictManager::new());
    dm.register_layer(Box::new(SystemDictLayer::new(build(main), "main")));
    dm.register_layer(Box::new(SystemDictLayer::new(build(ext), "ext")));
    let e = CodeTableEngine::new(4, CommitOptions::default(), dm.clone());
    (e, dm)
}

/// `set_dict_enabled` 必须先分辨「这个 id 是不是我的」，再谈处理。
///
/// `MixedEngine::set_dict_enabled` 把调用转发给 primary / secondary / english 三个子引擎并取
/// `a || b || c`，码表子引擎也会收到拼音库的 id。若它对任何 id 都回 true，或运算恒真，
/// 不支持热插拔的拼音子引擎所需的失效重建被吞掉，混输里会继续出已禁用库的候选直到重启。
///
/// 启用方向一律回 false：惰性加载后未启用的库没建层，必须交调用方失效重建，
/// 故调用方不能拿返回值给启用路径做路由。
#[test]
fn set_dict_enabled_only_claims_own_dicts() {
    let (e, _dm) = engine_two_dicts(&[("a", "工", 100)], &[("a", "工", 5000)]);
    let e = e.with_own_extra_dicts(["mine".to_string()]);

    assert!(
        e.set_dict_enabled("mine", false),
        "本方案声明过的库：禁用应认领（摘到与否都算达成 —— 惰性加载可能本就没装它）"
    );
    assert!(
        !e.set_dict_enabled("not-mine", false),
        "**不是**本方案的库：必须回 false，否则混输的 a||b||c 会恒真、掩盖真正承载方的重建需求"
    );
    assert!(
        !e.set_dict_enabled("mine", true),
        "启用方向一律回 false：层没建过，这里无从恢复，交调用方失效重建"
    );
    assert!(
        !e.set_dict_enabled("not-mine", true),
        "既不是我的、又是启用，更该回 false"
    );
}

/// 默认（未登记任何 own_extra_dicts）时对一切 id 回 false —— 退化成「失效重建」，
/// 可能多跑一次重建，但不会给出错误答案。既有调用点与测试因此无需改动。
#[test]
fn set_dict_enabled_defaults_to_disowning_everything() {
    let (e, _dm) = engine_two_dicts(&[("a", "工", 100)], &[("a", "工", 5000)]);
    assert!(!e.set_dict_enabled("ext", false));
    assert!(!e.set_dict_enabled("ext", true));
}

/// 跨词库同词条合并的主键是 `(code, text)`，不是 `text`。
///
/// ① 两库收录同一条（码相同）→ 按最高权重算，关掉出该权重的库即回退。
/// ② 两库里该词码不同 → 是两个词条，权重各归各的码位：简码条目若继承全码条目的高权重，
///    会直接改掉首选。
///
/// 两条必须并存：只有 ① 时「无条件跨码位取 max」照样全绿；只有 ② 时「永不继承」也全绿。
#[test]
fn cross_dict_weight_merges_by_code_and_text() {
    // ① 同码：主库 100 / 扩展库 5000 → 取 5000，来源标注指向扩展库。
    let (e, dm) = engine_two_dicts(&[("a", "工", 100)], &[("a", "工", 5000)]);
    let pick = |e: &CodeTableEngine| {
        e.convert("a", 50)
            .unwrap()
            .candidates
            .into_iter()
            .find(|c| c.text == "工")
            .expect("应有候选「工」")
    };
    let gong = pick(&e);
    assert_eq!(gong.weight, 5000, "同一词条被两库收录时按最高权重算");
    assert_eq!(
        gong.meta.weight_layer.as_deref(),
        Some("ext"),
        "权重来源须标为扩展库，否则调试段会把它记在主库头上"
    );

    // 关掉扩展库 → 回退到主库权重，来源标注一并回退（不得残留 ext）。
    assert!(dm.set_layer_enabled("ext", false));
    let gong_off = pick(&e);
    assert_eq!(gong_off.weight, 100, "扩展库关闭后回退到主库权重");
    assert_eq!(gong_off.meta.weight_layer.as_deref(), Some("main"));

    // ② 异码：主库简码 a(100)、扩展库全码 ab(5000)，打 `a`。
    let (e2, _) = engine_two_dicts(&[("a", "工", 100)], &[("ab", "工", 5000)]);
    let gong2 = pick(&e2);
    assert_eq!(gong2.code, "a", "打 a 命中的是简码那条");
    assert_eq!(
        gong2.weight, 100,
        "权重须是简码 `a` 自己的 100——`ab` 是另一个词条，它的 5000 不得漂过来"
    );
    assert_eq!(
        gong2.meta.weight_layer.as_deref(),
        Some("main"),
        "来源仍是主库：权重压根没换过"
    );
    assert!(
        gong2.merged_codes.iter().any(|c| c == "ab"),
        "被丢弃那条的**码位**仍要并入（检索范围过滤依赖它）——不继承的是权重，不是码位"
    );
}

fn engine_opts(entries: &[(&str, &str, i32)], opts: CommitOptions) -> CodeTableEngine {
    let mut d = CodetableDict::empty();
    for (i, (code, text, w)) in entries.iter().enumerate() {
        d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::Memory(d),
        "codetable-system",
    )));
    CodeTableEngine::new(4, opts, Arc::new(dm))
}

#[test]
fn clear_on_empty_at_full_len() {
    // 满码(4) 无候选 + clear_on_empty_max → should_clear
    let e = engine_opts(
        &[("aaaa", "工", 100)],
        CommitOptions {
            clear_on_empty_max: true,
            ..Default::default()
        },
    );
    let r = e.convert("zzzz", 50).unwrap();
    assert!(r.is_empty && r.should_clear, "满码空码应请求清空");
    // 未满码的空码不清空
    let r2 = e.convert("zz", 50).unwrap();
    assert!(r2.is_empty && !r2.should_clear, "未满码空码不应清空");
}

#[test]
fn top_code_commits_overflow_prefix() {
    // max=4，"aaaa"=工 唯一全码；输入 "aaaab"（>4，整串无匹配/无更长）→ 顶前4码"工"，余 "b"
    let e = engine_opts(
        &[("aaaa", "工", 100)],
        CommitOptions {
            top_code_commit: true,
            ..Default::default()
        },
    );
    let top = e.handle_top_code("aaaab");
    assert_eq!(top, Some(("工".to_string(), "b".to_string())));
    // 关闭开关 → None
    let e2 = engine_opts(&[("aaaa", "工", 100)], CommitOptions::default());
    assert_eq!(e2.handle_top_code("aaaab"), None);
}

#[test]
fn top_code_overflow_prefix_no_char_returns_empty_top() {
    // prefix 码表无字（短语专属码场景）：仍判定溢出该顶，返回 Some(("", 余码))——
    // 「顶什么」交上层用短语显示首选兜底。原 `first()?` 短路会让顶码整个不触发。
    let e = engine_opts(
        &[("aaaa", "工", 100)],
        CommitOptions {
            top_code_commit: true,
            ..Default::default()
        },
    );
    // "bbbb" 无字，"bbbbc"(>4，无匹配/无更长后继) → Some(("", "c"))
    assert_eq!(
        e.handle_top_code("bbbbc"),
        Some((String::new(), "c".to_string())),
        "prefix 码表无字应返回空 top + 余码，而非 None"
    );
}

#[test]
fn convert_sets_should_commit_for_unique_full_code() {
    // "aaaa" 唯一精确、无更长后继 → should_commit
    let e = engine_with(&[("aaaa", "工", 100)], true, 4);
    let r = e.convert("aaaa", 50).unwrap();
    assert!(r.should_commit, "唯一全码应自动上屏");
    assert_eq!(r.commit_text, "工");
}

#[test]
fn convert_no_commit_when_longer_code_exists() {
    // "aaa" 精确存在，但还有更长 "aaaa" → 不自动上屏
    let e = engine_with(&[("aaa", "甲", 100), ("aaaa", "工", 90)], true, 3);
    let r = e.convert("aaa", 50).unwrap();
    assert!(!r.should_commit, "存在更长后继编码时不应自动上屏");
}

#[test]
fn convert_no_commit_when_disabled() {
    let e = engine_with(&[("aaaa", "工", 100)], false, 4);
    let r = e.convert("aaaa", 50).unwrap();
    assert!(!r.should_commit);
}

#[test]
fn recheck_auto_commit_unique_after_filter() {
    // 同码两个精确候选（"hhnu"→X 常用 / 愳 生僻）：引擎按未过滤候选判不唯一 → 不上屏。
    let e = engine_with(&[("hhnu", "X", 100), ("hhnu", "愳", 1)], true, 4);
    let r = e.convert("hhnu", 50).unwrap();
    assert!(!r.should_commit, "两个精确同码候选不自动上屏");
    // 模拟智能过滤后仅剩一个精确全码候选 → 复评放行。
    let filtered = [cand("hhnu", "X")];
    assert_eq!(
        e.recheck_auto_commit("hhnu", &filtered),
        Some("X".to_string()),
        "过滤后唯一精确全码应复评放行"
    );
    // 满码上屏开关关闭时复评不放行。
    let e_off = engine_with(&[("hhnu", "X", 100), ("hhnu", "愳", 1)], false, 4);
    assert_eq!(e_off.recheck_auto_commit("hhnu", &filtered), None);
}

#[test]
fn single_code_input_disables_prefix() {
    // 词典：精确 "aa"→"式"，更长 "aab"→"想"。开启精确匹配后 "aa" 只应出 "式"。
    let e = engine_opts(
        &[("aa", "式", 100), ("aab", "想", 90)],
        CommitOptions {
            single_code_input: true,
            ..Default::default()
        },
    );
    let r = e.convert("aa", 50).unwrap();
    assert_eq!(r.candidates.len(), 1, "精确匹配模式不应含前缀候选");
    assert_eq!(r.candidates[0].text, "式");
}

#[test]
fn single_code_complete_fills_from_longer_code() {
    // 无 "ab" 精确项；补全池应按引擎序备好更长编码候选，首条为 "abc"→"你"。
    let e = engine_opts(
        &[("abc", "你", 100), ("abd", "他", 90)],
        CommitOptions {
            single_code_input: true,
            single_code_complete: true,
            show_code_hint: true,
            ..Default::default()
        },
    );
    let r = e.convert("ab", 50).unwrap();
    // 补全候选走 `completion_hints` 旁路而不入 `candidates`：该不该补取决于最终屏幕上有没有
    // 候选，引擎看不见协调器随后叠加的短语，无权就地拍板（见 ConvertResult 文档）。
    assert!(r.candidates.is_empty(), "补全候选不应入引擎候选列表");
    let hint = r.completion_hints.first().expect("应备好空码补全候选");
    assert_eq!(hint.text, "你", "空码补全首选取更长编码首条");
    assert_eq!(hint.comment, "c", "补全候选应标注剩余编码");
    // 备的是池不是单条：协调器在 shadow/检索范围过滤之后才择一，只备一条的话
    // 用户隐藏掉首条就无货可补。
    assert!(
        r.completion_hints.iter().any(|c| c.text == "他"),
        "补全池应含次条 abd→他，实际: {:?}",
        r.completion_hints
            .iter()
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
    assert!(!r.should_commit, "补全候选不应触发自动上屏");
}

/// 满码长空码、码表另有超码长词条 ⇒ 照样备补全（不被 `< max_code_length` 截断）。
/// 同开逆切分时，切分须让位给这条后续（见 `decode_split` 的后继判据）。
#[test]
fn single_code_complete_fills_at_full_length() {
    for split_input in [false, true] {
        let e = engine_opts(
            &[("abcde", "你", 100), ("ab", "甲", 90), ("cd", "乙", 90)],
            CommitOptions {
                single_code_input: true,
                single_code_complete: true,
                split_input,
                ..Default::default()
            },
        );
        let r = e.convert("abcd", 50).unwrap();
        assert!(r.candidates.is_empty(), "split={split_input}：不得切分");
        assert_eq!(
            r.completion_hints.first().map(|c| c.text.as_str()),
            Some("你"),
            "split={split_input}：满码长空码应备超码长后续"
        );
    }
}

/// 超码长空码（第 5 码起）仍有更长后续 ⇒ 照样备补全并标剩余编码，直到打全。
#[test]
fn single_code_complete_fills_beyond_full_length() {
    let e = engine_opts(
        &[("abcdef", "你", 100), ("abcdgh", "他", 90)],
        CommitOptions {
            single_code_input: true,
            single_code_complete: true,
            show_code_hint: true,
            ..Default::default()
        },
    );
    let r = e.convert("abcde", 50).unwrap();
    let hint = r.completion_hints.first().expect("超码长空码应备后续");
    assert_eq!((hint.text.as_str(), hint.comment.as_str()), ("你", "f"));
    assert!(
        r.completion_hints.iter().all(|c| c.text != "他"),
        "第 5 码已分岔，abcdgh 不是这串的后续"
    );
    assert!(e.convert("abcdx", 50).unwrap().completion_hints.is_empty());
}

#[test]
fn single_code_complete_hint_absent_without_longer_code() {
    // 无 "ab" 精确项、也无更长后继 → 无货可备。
    let e = engine_opts(
        &[("xy", "甲", 100)],
        CommitOptions {
            single_code_input: true,
            single_code_complete: true,
            ..Default::default()
        },
    );
    let r = e.convert("ab", 50).unwrap();
    assert!(r.candidates.is_empty());
    assert!(r.completion_hints.is_empty(), "无更长编码时不应备补全候选");
}

#[test]
fn exact_match_suppresses_completion_hint() {
    // 有 "ab" 精确项 → 不是空码，不该备补全（否则协调器侧判空虽拦得住，但白查一次前缀）。
    let e = engine_opts(
        &[("ab", "甲", 100), ("abc", "你", 90)],
        CommitOptions {
            single_code_input: true,
            single_code_complete: true,
            ..Default::default()
        },
    );
    let r = e.convert("ab", 50).unwrap();
    assert_eq!(r.candidates.len(), 1);
    assert!(r.completion_hints.is_empty(), "有精确候选时不备补全");
}

#[test]
fn exact_match_outranks_higher_weight_prefix_words() {
    // 真实现场（古精86五笔-深海词库）：简码 usr→「新」(11777)，前缀词组 usrq→「新的」(47487)、
    // usrt→「新手」(22229)。词组权重取自词频、单字取自字频，两套量纲不可比——纯按权重排会把
    // 简码「新」挤到第三位。精确匹配须恒居首，其后的前缀候选内部仍按权重降序。
    let e = engine_opts(
        &[
            ("usr", "新", 11777),
            ("usrq", "新的", 47487),
            ("usrt", "新手", 22229),
            ("usrp", "亲近", 1861),
        ],
        CommitOptions::default(),
    );
    let r = e.convert("usr", 50).unwrap();
    let order: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        order,
        vec!["新", "新的", "新手", "亲近"],
        "精确匹配应居首、其余按权重降序"
    );
    // 该层级必须落到字段上随候选流动：协调器合并短语后会无条件重排，只在引擎内排好而
    // 不标记，下游会按纯权重把结果推翻（本 bug 的原始成因）。
    assert!(
        r.candidates[0].is_exact_code,
        "精确候选须标记 is_exact_code 供协调器重排沿用"
    );
    assert!(
        r.candidates[1..].iter().all(|c| !c.is_exact_code),
        "前缀补全候选不应被标记为精确匹配"
    );
}

#[test]
fn truncate_protects_low_weight_exact_match() {
    // 精确全码 "aa"→式(权重 1) + 5 个高权重前缀词(code="aab".."aaf",权重 1000)。
    // max_candidates=3：纯按权重截断会把低权重精确「式」挤出配额丢失；分区保护须保留它。
    let e = engine_opts(
        &[
            ("aa", "式", 1),
            ("aab", "A", 1000),
            ("aac", "B", 1000),
            ("aad", "C", 1000),
            ("aae", "D", 1000),
            ("aaf", "E", 1000),
        ],
        CommitOptions::default(),
    );
    let r = e.convert("aa", 3).unwrap();
    assert_eq!(r.candidates.len(), 3, "应截断到 3 条");
    assert!(
        r.candidates.iter().any(|c| c.text == "式"),
        "低权重精确全码不应被高权重前缀词截断挤出，实际: {:?}",
        r.candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn single_code_complete_off_yields_empty() {
    let e = engine_opts(
        &[("abc", "你", 100)],
        CommitOptions {
            single_code_input: true,
            ..Default::default()
        },
    );
    let r = e.convert("ab", 50).unwrap();
    assert!(r.is_empty, "补全关闭时无精确匹配应为空");
}

#[test]
fn base_sort_natural_ignores_weight_uses_appearance_order() {
    // 同码 "aa" 两候选：低权重"低"先出现（order 0）、高权重"高"后出现（order 1）。
    let entries = &[("aa", "低", 1), ("aa", "高", 100)];
    // natural：忽略权重，按出现序 → 低、高。
    let e = engine_opts(
        entries,
        CommitOptions {
            base_sort: BaseSort::Natural,
            ..Default::default()
        },
    );
    let t: Vec<String> = e
        .convert("aa", 50)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert_eq!(t, vec!["低", "高"], "natural 应按出现序、忽略权重");
    // weight（默认）：高权重在前 → 高、低。
    let e2 = engine_opts(entries, CommitOptions::default());
    let t2: Vec<String> = e2
        .convert("aa", 50)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert_eq!(t2, vec!["高", "低"], "weight 应按权重降序");
}

#[test]
fn base_sort_parse_maps_strings() {
    assert_eq!(BaseSort::parse("natural"), BaseSort::Natural);
    assert_eq!(BaseSort::parse("Natural"), BaseSort::Natural);
    assert_eq!(BaseSort::parse("weight"), BaseSort::Weight);
    assert_eq!(BaseSort::parse(""), BaseSort::Weight);
    assert_eq!(BaseSort::parse("xyz"), BaseSort::Weight);
}

/// 构造双层码表引擎（贴近真实多词库方案）：
/// - 主库 `codetable-system`（base_order 0，带权重）：同码 "aa" 两条——"主低"(w10,出现序0)、
///   "主高"(w100,出现序1)，权重序与出现序相反（用于区分 weight/natural）。
/// - 扩展库 `codetable-extra-x`（base_order 1，无权重，default_weight=50）：同码 "aa" 一条 "扩"。
fn engine_two_layers(opts: CommitOptions) -> CodeTableEngine {
    let mut main = CodetableDict::empty();
    main.merge_single("aa".into(), "主低".into(), 10, 0);
    main.merge_single("aa".into(), "主高".into(), 100, 1);
    let mut ext = CodetableDict::empty();
    ext.merge_single("aa".into(), "扩".into(), 0, 0);

    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::Memory(main),
        "codetable-system",
    )));
    dm.register_layer(Box::new(
        SystemDictLayer::with_enabled(CachedDict::Memory(ext), "codetable-extra-x", true)
            .with_base_order(1)
            .with_default_weight(Some(50)),
    ));
    CodeTableEngine::new(4, opts, Arc::new(dm))
}

fn texts_of(e: &CodeTableEngine, input: &str) -> Vec<String> {
    e.convert(input, 50)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

#[test]
fn multi_layer_weight_mode_weight_primary_default_weight_places_ext() {
    // weight 模式（默认）：权重主导 → 主高(100) > 扩(50, 由 default_weight) > 主低(10)。
    // 证明：① 权重优先于 base_order（主低虽 base_order 0 却因低权重沉底）；
    //       ② default_weight 让无权重扩展库落在 50 档（介于 100 与 10 之间）。
    let e = engine_two_layers(CommitOptions::default());
    assert_eq!(
        texts_of(&e, "aa"),
        vec!["主高", "扩", "主低"],
        "weight 模式应权重主导 + default_weight 定档"
    );
}

#[test]
fn multi_layer_natural_mode_base_order_tiers_dicts_ignores_weight() {
    // natural 模式：忽略权重，按 base_order 档位分组、组内按出现序。
    // → 主库(base_order 0)整组在前：主低(出现序0)、主高(出现序1)；扩展库(base_order 1)在后：扩。
    // 证明：① base_order 分档把整个扩展库排到主库之后（与条目权重无关）；
    //       ② 组内忽略权重按出现序（主低虽权重低却因出现序靠前而在主高之前）。
    let e = engine_two_layers(CommitOptions {
        base_sort: BaseSort::Natural,
        ..Default::default()
    });
    assert_eq!(
        texts_of(&e, "aa"),
        vec!["主低", "主高", "扩"],
        "natural 模式应按 base_order 分档 + 组内出现序、忽略权重"
    );
}

// ───────────────────────── 整句输入（sentence_input） ─────────────────────────

/// 五笔结构的缩微模型（与 `sentence.rs` 单测同源）：一简 / 二简 / 3 码全码 /
/// 4 码全码 / 4 码词组俱全，权重照抄极点词库的层级带。
const SENTENCE_ENTRIES: &[(&str, &str, i32)] = &[
    ("a", "工", 9999),
    ("g", "一", 9999),
    ("w", "人", 9999),
    ("aa", "式", 9950),
    ("wt", "何", 9950),
    ("hci", "皮", 1200),
    ("aaaa", "工", 800),
    ("ggll", "一", 700),
    ("wtgf", "人", 600),
    ("aagg", "式", 500),
    ("aawt", "工作", 1241),
];

fn sentence_engine(extra: &[(&str, &str, i32)], opts: CommitOptions) -> CodeTableEngine {
    let mut entries: Vec<(&str, &str, i32)> = SENTENCE_ENTRIES.to_vec();
    entries.extend_from_slice(extra);
    engine_opts(&entries, opts)
}

/// 分隔符的字符串形式（测试拼串用）。
const SEPS: &str = "'";

fn sentence_opts() -> CommitOptions {
    CommitOptions {
        sentence_input: true,
        ..Default::default()
    }
}

#[test]
fn sentence_leads_when_input_exceeds_code_length() {
    // 超码长（8 > 4）且整串无精确解 → 整句候选置顶。
    let e = sentence_engine(&[], sentence_opts());
    let r = e.convert("aawtaawt", 50).unwrap();
    let first = r.candidates.first().expect("应有候选");
    assert_eq!(first.text, "工作工作");
    assert!(first.is_sentence, "整句候选须带 is_sentence 供顶部锚定");
}

#[test]
fn sentence_candidate_never_auto_commits() {
    // 整句候选是超码长串上唯一一条 `code == input`，满码上屏不得把它当唯一全码上屏。
    let opts = CommitOptions {
        auto_commit_at_full: true,
        ..sentence_opts()
    };
    let r = sentence_engine(&[], opts).convert("aawtaawt", 50).unwrap();
    assert!(r.candidates[0].is_sentence, "前提：应解出整句");
    assert!(!r.should_commit, "整句候选不得触发满码上屏");
}

#[test]
fn sentence_input_disables_full_auto_commit_and_clear() {
    // 两向对照：同一配置只差 `sentence_input`。`aawt` 唯一全码、`zzzz` 空码。
    let with = |sentence_input| CommitOptions {
        auto_commit_at_full: true,
        clear_on_empty_max: true,
        sentence_input,
        ..Default::default()
    };
    let off = sentence_engine(&[], with(false));
    assert!(
        off.convert("aawt", 50).unwrap().should_commit,
        "前提：关整句时唯一全码上屏"
    );
    assert!(
        off.convert("zzzz", 50).unwrap().should_clear,
        "前提：关整句时空码清空"
    );
    let cands = off.convert("aawt", 50).unwrap().candidates;
    assert!(off.should_auto_commit("aawt", &cands).is_some());

    let on = sentence_engine(&[], with(true));
    let r = on.convert("aawt", 50).unwrap();
    assert!(!r.should_commit, "开整句时满码上屏让位");
    assert!(on.should_auto_commit("aawt", &r.candidates).is_none());
    assert!(on.recheck_auto_commit("aawt", &r.candidates).is_none());
    assert!(
        !on.convert("zzzz", 50).unwrap().should_clear,
        "开整句时空码清空让位"
    );
}

#[test]
fn sentence_consumes_whole_input_and_marks_synthesized() {
    // 锁住「码表候选 consumed_length 恒 0」这条全仓约定：整句只在覆盖整串时产出。
    // 清空守护 / 词频记账 / 自动造词缓冲三处都依赖它，改成分段上屏须三处同改（见设计文档 §7.1）。
    let e = sentence_engine(&[], sentence_opts());
    let first = e.convert("aawtaawt", 50).unwrap().candidates.remove(0);
    assert_eq!(first.consumed_length, 0, "整句须消费整串");
    assert_eq!(first.code, "aawtaawt", "整句的 code 是整串输入");
    assert!(
        first.is_synthesized,
        "词库里没有这个词条，自动造词据此判值不值得学"
    );
    // boundary 是音节边界（拼音域）；码表码无音节语义，必须留 0，
    // 否则整句被学进用户词库时会带一份假的音节真值。
    assert_eq!(first.boundary, 0, "码表整句不得填 boundary");
}

#[test]
fn sentence_not_produced_within_code_length() {
    // 码长内的串本就是一个编码单元：`aaw` 本意是 `aawt`→「工作」，进整句路径会被读成
    // 「工工人」之类抢走首位（见 `mixed/engine.rs`）。
    let e = sentence_engine(&[], sentence_opts());
    let r = e.convert("aaw", 50).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.is_sentence),
        "码长内不得产出整句候选，实得: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

#[test]
fn sentence_nbest_shows_count_and_fills_pool() {
    // 露 2 算 3：候选最前是名次 1、2 两条整句，池子 3 条（够的话），各带自己的切法。
    let opts = CommitOptions {
        sentence_count: 2,
        sentence_max_count: 3,
        ..sentence_opts()
    };
    let r = sentence_engine(&[], opts).convert("aawtaawt", 50).unwrap();
    let ranks: Vec<u8> = r
        .candidates
        .iter()
        .filter(|c| c.is_sentence)
        .map(|c| c.sentence_rank)
        .collect();
    assert_eq!(ranks, vec![1, 2], "露两条，按名次");
    assert!(
        r.candidates[0].is_sentence && r.candidates[1].is_sentence,
        "整句块在最前"
    );
    assert!(r.sentence_pool.len() >= 2, "池子按算的条数填");
    assert!(r.sentence_pool.len() <= 3);
    let splits: Vec<_> = r.candidates[..2]
        .iter()
        .map(|c| c.meta.preedit_split.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(splits[0], "aawt'aawt");
    assert_eq!(r.preedit_codetable, splits[0], "组合区默认取名次 1 的切法");
    assert!(splits.iter().all(|s| !s.is_empty()), "每条整句都带切分");
}

#[test]
fn sentence_single_by_default_with_empty_pool() {
    // 出厂 1/1：只出一条、池子恒空——切换键据此不吃键。
    let r = sentence_engine(&[], sentence_opts())
        .convert("aawtaawt", 50)
        .unwrap();
    assert_eq!(r.candidates.iter().filter(|c| c.is_sentence).count(), 1);
    assert!(r.sentence_pool.is_empty());
    // 不带逐条切分：带了协调器就会随高亮移动重算组合区，1/1 下那是行为变化。
    assert!(r.candidates[0].meta.preedit_split.is_none());
    assert_eq!(r.preedit_codetable, "aawt'aawt", "组合区切分照旧由引擎给");
}

#[test]
fn sentence_off_by_default() {
    // 出厂关闭：同一串输入在默认配置下不该冒出整句。
    let e = sentence_engine(&[], CommitOptions::default());
    let r = e.convert("aawtaawt", 50).unwrap();
    assert!(!r.candidates.iter().any(|c| c.is_sentence));
}

#[test]
fn sentence_skipped_when_whole_input_has_exact_match() {
    // 对齐 librime `enable_sentence_ && !translation`：整串查得到词就不走整句，
    // 否则同一个词会以两种身份进列表。这里造一条 8 码的用户长词条。
    let e = sentence_engine(&[("aawtaawt", "工作工作", 300)], sentence_opts());
    let r = e.convert("aawtaawt", 50).unwrap();
    let first = r.candidates.first().expect("应有候选");
    assert!(first.is_exact_code, "整串精确匹配应居首");
    assert!(
        !r.candidates.iter().any(|c| c.is_sentence),
        "整串有精确解时不产整句"
    );
}

#[test]
fn top_code_yields_to_sentence() {
    // 顶码与整句抢同一个区间（超码长），且顶码是自动上屏、一触发用户就看不到整句。
    // 判据取「功能是否开启」，故即便本次解不出整句，顶码同样让位——
    // 否则同一串码上顶码会时灵时不灵。
    let opts = CommitOptions {
        top_code_commit: true,
        sentence_input: true,
        ..Default::default()
    };
    let e = sentence_engine(&[], opts);
    assert_eq!(e.handle_top_code("aawtaawt"), None, "整句开启时顶码让位");
    assert_eq!(e.handle_top_code("aawtzzzz"), None, "解不出整句也照样让位");

    // 对照：关掉整句，顶码恢复。
    let e2 = sentence_engine(
        &[],
        CommitOptions {
            top_code_commit: true,
            ..Default::default()
        },
    );
    let (top, rest) = e2.handle_top_code("aawtaawt").expect("顶码应触发");
    assert_eq!((top.as_str(), rest.as_str()), ("工作", "aawt"));
}

#[test]
fn sentence_fills_preedit_split() {
    // 组合区切分串：一长串码配一句话时，用户要看得见引擎把它切成了哪几段。
    let e = sentence_engine(&[], sentence_opts());
    let r = e.convert("aawtaawt", 50).unwrap();
    assert_eq!(r.preedit_codetable, "aawt@aawt".replace('@', SEPS));
}

#[test]
fn no_sentence_means_no_preedit_split() {
    // 码长内不产整句 ⇒ 切分串必须为空，否则组合区会显示一个不对应任何候选的切法。
    let e = sentence_engine(&[], sentence_opts());
    assert!(e.convert("aaw", 50).unwrap().preedit_codetable.is_empty());
    // 关闭整句时同理。
    let off = sentence_engine(&[], CommitOptions::default());
    assert!(
        off.convert("aawtaawt", 50)
            .unwrap()
            .preedit_codetable
            .is_empty()
    );
}

#[test]
fn manual_separator_end_to_end() {
    // 手动分隔符走完整 convert：`aa'wt` 强制两个二简字，且切分串原样保留分隔符。
    let e = sentence_engine(&[], sentence_opts());
    let input = format!("aa{SEPS}wt");
    let r = e.convert(&input, 50).unwrap();
    let first = r.candidates.first().expect("应有候选");
    assert!(first.is_sentence, "分隔符输入应产出整句候选");
    assert_eq!(first.text, "式何");
    assert_eq!(r.preedit_codetable, input);
}

#[test]
fn sentence_input_enabled_reports_state() {
    // 协调器据此放行分隔符键（见 `manual_separator_key`）。
    use crate::engine::Engine;
    assert!(sentence_engine(&[], sentence_opts()).sentence_input_enabled());
    assert!(!sentence_engine(&[], CommitOptions::default()).sentence_input_enabled());
}

#[test]
fn sentence_uses_three_code_full_entry_end_to_end() {
    // 3 码全码「皮 hci」必须能参与整句 —— 「整句只认 4 码单元」那条捷径的反例。
    let e = sentence_engine(&[], sentence_opts());
    let r = e.convert("hciaawt", 50).unwrap();
    assert_eq!(
        r.candidates.first().map(|c| c.text.as_str()),
        Some("皮工作")
    );
}

// ───────────────────────── 逆切分（split_input） ─────────────────────────

/// 音形结构的缩微模型：二简字/词 + 一个四码精确条目（对照组）。
/// 原例 `hf'kn` →「很可能」，后段 `kn` 故意留两个重码。
///
/// 不在这里放 `hfkn` 的五码扩展：那会给 `hfkn` 添一条前缀候选，默认档「有候选就不切」会挡掉
/// 所有基本盘用例。只有 `no_exact` 档那条用例需要，经 `extra` 加入（[`PREFIX_ONLY_ENTRY`]）。
const SPLIT_ENTRIES: &[(&str, &str, i32)] = &[
    ("hf", "很可", 900),
    ("kn", "能", 800),
    ("kn", "难", 500),
    ("xt", "学", 700),
    ("up", "双拼", 600),
    // 四码精确条目：默认档下「有候选就不切」的对照组。
    ("aaaa", "工", 100),
    // 一简：三码切分（2+1）的后段落点。
    ("a", "啊", 950),
    // 二简位被符号占着：有编码就切，符号照样参与。
    ("qw", "→", 700),
    // 二简位是特殊语法值：要到协调器才展开，拼进组合就是源码上屏 ⇒ 须剔除。
    ("zx", "$CC(ime.x)", 700),
    ("zy", "{date}", 700),
];

/// 让 `hfkn` 有前缀候选却仍无精确解——构造 `no_exact` 档场景的唯一条目。
const PREFIX_ONLY_ENTRY: (&str, &str, i32) = ("hfknq", "甲", 1000);

fn split_engine(extra: &[(&str, &str, i32)], opts: CommitOptions) -> CodeTableEngine {
    let mut entries: Vec<(&str, &str, i32)> = SPLIT_ENTRIES.to_vec();
    entries.extend_from_slice(extra);
    engine_opts(&entries, opts)
}

fn split_opts() -> CommitOptions {
    CommitOptions {
        split_input: true,
        ..Default::default()
    }
}

fn texts(r: &ConvertResult) -> Vec<&str> {
    r.candidates.iter().map(|c| c.text.as_str()).collect()
}

/// 基本盘：满码长空码 → 前段首选 × 后段列举，按后段权重序。
#[test]
fn split_composes_two_segments_on_full_length_empty_code() {
    let e = split_engine(&[], split_opts());
    let r = e.convert("hfkn", 50).unwrap();
    assert_eq!(
        texts(&r),
        vec!["很可能", "很可难"],
        "hf(很可) × kn(能/难)，后段按权重序"
    );
    assert!(
        r.candidates.iter().all(|c| c.is_split_composed),
        "组合候选须带 is_split_composed（协调器的组合区判据与沉底层都读它）"
    );
    assert!(!r.is_empty, "产出了候选，is_empty 必须为假");
}

/// 组合候选的四条字段约定，每一条都有下游依赖，改动即回归。
#[test]
fn split_candidate_field_contract() {
    let e = split_engine(&[], split_opts());
    let r = e.convert("hfkn", 50).unwrap();
    let c = r.candidates.first().expect("应有组合候选");

    assert_eq!(
        c.code, "hfkn",
        "code 须是整串 —— decide_auto_commit 据此认精确"
    );
    assert_eq!(
        c.consumed_length, 0,
        "消费整串：全仓「码表候选 consumed_length 恒 0」的约定不得由本功能打破"
    );
    assert_eq!(
        c.boundary, 0,
        "boundary 是音节边界（拼音域），码表无音节语义；填切分位会让自动造词学到假真值"
    );
    assert!(
        !c.is_exact_code,
        "它不是词库里的精确解，置位会让它混进 cmp_exact_first 的精确档"
    );
    assert!(
        c.is_synthesized,
        "词库无此整体词条 ⇒ 自动造词据此判「值得学」，打过一次下次直接有"
    );
    assert_eq!(c.source, CandidateSource::CodeTable);
}

/// 组合区显示 `hf'kn`，与整句、拼音音节共用同一个分隔符。
#[test]
fn split_fills_preedit_codetable() {
    let e = split_engine(&[], split_opts());
    let r = e.convert("hfkn", 50).unwrap();
    assert_eq!(r.preedit_codetable, format!("hf{SEPS}kn"));
    // 对照：没切分时不得留下切分串，否则组合区会显示一个不对应任何候选的切法。
    let r2 = e.convert("aaaa", 50).unwrap();
    assert!(r2.preedit_codetable.is_empty());
}

/// 关闭时零行为变化——这是本功能能默认关着发布的依据。
#[test]
fn split_disabled_changes_nothing() {
    let e = split_engine(&[], CommitOptions::default());
    let r = e.convert("hfkn", 50).unwrap();
    assert!(
        r.candidates.is_empty() && r.is_empty,
        "关闭时满码空码仍是空"
    );
    assert!(r.preedit_codetable.is_empty());
}

/// 闸门只占 `== max_code_length` 这一格：未满码、超码长都不切。
///
/// 超码长那一格归顶码与整句（两者的闸门都在 `>` 上）；未满码时还有更长后继可打，
/// 切分等于替用户提前认定「这串到此为止」。
#[test]
fn split_range_is_front_len_exclusive_to_full_length() {
    let e = split_engine(&[], split_opts());

    // 2 码 = 前段本身，后段无码可分。
    let two = e.convert("hf", 50).unwrap();
    assert!(
        !two.candidates.iter().any(|c| c.is_split_composed),
        "码长不长于前段时不得切分，实际: {:?}",
        texts(&two)
    );

    // 超码长归顶码与整句。
    let long = e.convert("hfknx", 50).unwrap();
    assert!(
        !long.candidates.iter().any(|c| c.is_split_composed),
        "5 码（超码长）不得切分，那是顶码与整句的区间，实际: {:?}",
        texts(&long)
    );
}

/// 三码空码 + 无更长后继 → 切成 2+1（`sm'a` →「什么啊」）：前段仍是二简，后段是剩下的一简，
/// 不是把三码对半分。
#[test]
fn split_three_code_when_no_longer_code() {
    let e = split_engine(&[], split_opts());
    // `hfa`：精确无、无 `hfa*` 后继 ⇒ 切成 hf(很可) + a(啊)。
    let r = e.convert("hfa", 50).unwrap();
    assert_eq!(texts(&r), vec!["很可啊"], "三码空码无后继时须切成 2+1");
    assert_eq!(r.preedit_codetable, format!("hf{SEPS}a"));
}

/// 反向对照：三码有更长后继时不得切分——用户还能继续打。防「未满码一律切」的实现让上一条变绿。
#[test]
fn split_three_code_yields_when_longer_code_exists() {
    // `hfk` 有 `hfknq` 这个更长后继。
    let e = split_engine(&[PREFIX_ONLY_ENTRY], split_opts());
    let r = e.convert("hfk", 50).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.is_split_composed),
        "三码有更长后继时不得切分，实际: {:?}",
        texts(&r)
    );
}

/// 精确匹配模式下满码长空码、但码表收了更长编码 ⇒ 默认档不切。
///
/// 精确模式关掉前缀枚举后列表是空的，只剩后继查询能认出「还没到头」。反向对照：同一串去掉
/// 更长条目就照切，证明拦下它的是后继而不是精确模式本身。
#[test]
fn split_full_length_yields_to_longer_code_in_exact_mode() {
    let exact = || CommitOptions {
        single_code_input: true,
        ..split_opts()
    };
    let e = split_engine(&[PREFIX_ONLY_ENTRY], exact());
    let r = e.convert("hfkn", 50).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.is_split_composed),
        "满码长有更长后继（hfknq）时不得切分，实际: {:?}",
        texts(&r)
    );
    assert!(r.preedit_codetable.is_empty(), "没切就不得留切分串");

    let control = split_engine(&[], exact());
    assert_eq!(
        texts(&control.convert("hfkn", 50).unwrap()),
        vec!["很可能", "很可难"],
        "★ 反向对照：无更长后继时精确模式照切"
    );
}

/// 段候选是符号也照样切：有实际编码就切，不论字、词、符。
#[test]
fn split_symbol_segment_participates() {
    let e = split_engine(&[], split_opts());
    // `qw` 只有符号「→」：有编码就切（`xkok` → 行👌 同形）。
    let r = e.convert("qwkn", 50).unwrap();
    assert_eq!(texts(&r), vec!["→能", "→难"]);
    let r = e.convert("hfqw", 50).unwrap();
    assert_eq!(texts(&r), vec!["很可→"], "后段是符号同样切");
}

/// 特殊语法值（`$CC` 命令 / `{..}` 模板）不参与：它们要到协调器才展开，拼进组合就是
/// 源码上屏。剔光的段视同空码 ⇒ 满码空码清空照常生效。
#[test]
fn split_skips_special_syntax_segment() {
    let e = split_engine(
        &[],
        CommitOptions {
            clear_on_empty_max: true,
            ..split_opts()
        },
    );
    for input in ["zxkn", "zykn", "hfzx"] {
        let r = e.convert(input, 50).unwrap();
        assert!(r.candidates.is_empty(), "{input}：实际 {:?}", texts(&r));
        assert!(
            r.is_empty && r.should_clear,
            "{input}：视同空码 ⇒ 满码空码清空"
        );
    }
}

/// 次选显示形态：默认 `Back`——同前段的每一行都带后段文本作显示，
/// 首条整串显示由协调器的显示出口判定（`cand_display_text`）；`Full` 一条都不填。
#[test]
fn split_alt_display_back_fills_back_text() {
    let e = split_engine(&[], split_opts());
    let r = e.convert("hfkn", 50).unwrap();
    let disp: Vec<&str> = r
        .candidates
        .iter()
        .map(|c| c.display_text.as_str())
        .collect();
    assert_eq!(disp, vec!["能", "难"]);
    assert_eq!(texts(&r), vec!["很可能", "很可难"], "上屏文本不变");

    let full = split_engine(
        &[],
        CommitOptions {
            split_alt_display: SplitAltDisplay::Full,
            ..split_opts()
        },
    );
    let r = full.convert("hfkn", 50).unwrap();
    assert!(r.candidates.iter().all(|c| c.display_text.is_empty()));
}

/// 前段多取时，前段非首选的行照样整串显示——否则两行显示同一个后段、分不出差别。
#[test]
fn split_alt_display_only_for_front_top() {
    let e = split_engine(
        &[("hf", "很", 800)],
        CommitOptions {
            split_front_candidates: 2,
            ..split_opts()
        },
    );
    let r = e.convert("hfkn", 50).unwrap();
    let rows: Vec<(&str, &str)> = r
        .candidates
        .iter()
        .map(|c| (c.text.as_str(), c.display_text.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("很可能", "能"),
            ("很可难", "难"),
            ("很能", ""),
            ("很难", "")
        ]
    );
    // 权重沿构造序单调不增：协调器按权重重排后同前段仍相邻（原始权重下
    // 「很能」800 会插到「很可难」500 前面，次选显示「难」就被读成「很难」）。
    let w: Vec<i32> = r.candidates.iter().map(|c| c.weight).collect();
    assert!(w.windows(2).all(|p| p[0] >= p[1]), "权重须单调不增：{w:?}");
}

#[test]
fn split_alt_display_parse() {
    assert_eq!(SplitAltDisplay::parse(""), SplitAltDisplay::Back);
    assert_eq!(SplitAltDisplay::parse("back"), SplitAltDisplay::Back);
    assert_eq!(SplitAltDisplay::parse("FULL"), SplitAltDisplay::Full);
    assert_eq!(SplitAltDisplay::parse("bogus"), SplitAltDisplay::Back);
}

/// 用例结束时删掉临时目录（Linux 上删已打开的 redb 文件无妨）。
struct TempRoot(std::path::PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 目录名带 pid：并发会话 / 并行的两次 `cargo test` 共用 temp 时不互相删库。
fn split_store(tag: &str) -> (TempRoot, Arc<wind_store::Store>) {
    let root = std::env::temp_dir().join(format!("wind_split_shadow_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(wind_store::Store::open(root.join("user_data.db")).unwrap());
    (TempRoot(root), store)
}

/// 候选调整里删掉的词不得被拼进组合（`n` 删「内容」后 `kpn` 不得出「恐怕内容」）。
#[test]
fn split_segment_respects_shadow_delete() {
    let (_dir, store) = split_store("del");
    store.delete_shadow("xh", "kn", "能").unwrap();
    let e = split_engine(&[], split_opts()).with_segment_shadow(store.clone(), "xh");
    assert_eq!(texts(&e.convert("hfkn", 50).unwrap()), vec!["很可难"]);

    // 删掉的恰是前段首选：由前段次选补位，而不是整个组合消失（先多取再截的理由）。
    store.delete_shadow("xh", "hf", "很可").unwrap();
    let e =
        split_engine(&[("hf", "很", 800)], split_opts()).with_segment_shadow(store.clone(), "xh");
    assert_eq!(texts(&e.convert("hfkn", 50).unwrap()), vec!["很难"]);

    // 反向对照：别的方案的规则不串味——证明生效的是按 (方案, 段码) 取的那条规则。
    let other = split_engine(&[], split_opts()).with_segment_shadow(store, "other");
    assert_eq!(
        texts(&other.convert("hfkn", 50).unwrap()),
        vec!["很可能", "很可难"]
    );
}

/// 段首选跟着候选调整的置顶走：用户在 `kn` 上把「难」置顶，组合首选随之变成「很可难」。
#[test]
fn split_segment_respects_shadow_pin() {
    let (_dir, store) = split_store("pin");
    store.pin_shadow("xh", "kn", "难", None, 0).unwrap();
    let e = split_engine(&[], split_opts()).with_segment_shadow(store, "xh");
    assert_eq!(
        texts(&e.convert("hfkn", 50).unwrap()),
        vec!["很可难", "很可能"]
    );
}

/// 逆切分与整句同判据：纯文本直通 `$CC(.., type("字"))` 取上屏文本参与切分，命令类仍剔除。
#[test]
fn split_segment_plain_type_entry_uses_typed_text() {
    let e = split_engine(
        &[
            ("ob", r#"$CC("横_一", type("一"))"#, 700),
            ("oc", r#"$CC("查", open("https://x/"))"#, 700),
        ],
        split_opts(),
    );
    assert!(texts(&e.convert("hfob", 50).unwrap()).contains(&"很可一"));
    assert!(
        !texts(&e.convert("hfoc", 50).unwrap())
            .iter()
            .any(|t| t.contains('$'))
    );
}

/// 某段被用户删光 ⇒ 视同空码，整体不产出。
#[test]
fn split_segment_deleted_empty_means_no_split() {
    let (_dir, store) = split_store("empty");
    store.delete_shadow("xh", "xt", "学").unwrap();
    let e = split_engine(&[], split_opts()).with_segment_shadow(store, "xh");
    assert!(e.convert("xtkn", 50).unwrap().candidates.is_empty());
}

/// 两段缺一不产：切一半的结果只会让用户以为词库缺条目。
#[test]
fn split_needs_both_segments() {
    let e = split_engine(&[], split_opts());
    for input in ["hfzz", "zzkn", "zzyy"] {
        let r = e.convert(input, 50).unwrap();
        assert!(
            r.candidates.is_empty(),
            "{input}：有一段查不到就整体不产出，实际: {:?}",
            texts(&r)
        );
    }
}

/// 默认档（`Empty`）不抢任何现有候选：有精确解时一条切分候选都不出。
#[test]
fn split_empty_trigger_yields_to_existing_candidates() {
    let e = split_engine(&[], split_opts());
    let r = e.convert("aaaa", 50).unwrap();
    assert_eq!(texts(&r), vec!["工"], "有精确候选时默认档不切分");
}

/// `no_exact` 档：无精确解即切，但切分候选排在既有候选之后。
/// 反向对照：同一串码在默认档下必须一条都不产，防「两档都照切」的实现假绿。
#[test]
fn split_no_exact_trigger_appends_after_existing() {
    // `hfkn` 无精确解，但 `hfknq`（甲）是它的前缀扩展 ⇒ 有候选、无精确。
    let e = split_engine(
        &[PREFIX_ONLY_ENTRY],
        CommitOptions {
            split_trigger: SplitTrigger::NoExact,
            ..split_opts()
        },
    );
    let r = e.convert("hfkn", 50).unwrap();
    assert_eq!(
        texts(&r),
        vec!["甲", "很可能", "很可难"],
        "no_exact 档：切分候选 append 在既有候选之后"
    );

    let default_trigger = split_engine(&[PREFIX_ONLY_ENTRY], split_opts());
    let r2 = default_trigger.convert("hfkn", 50).unwrap();
    assert_eq!(
        texts(&r2),
        vec!["甲"],
        "★ 反向对照：默认档下有候选就不切，一条组合都不该出现"
    );
}

/// 原帖规则一/二：前后两段都唯一（或前段有重码但恒取首选）→ 适配四码唯一自动上屏。
/// 靠既有 `decide_auto_commit` 的「恰一个 `code == input` 的候选」判据，本功能没有新增上屏判据。
#[test]
fn split_unique_composition_triggers_auto_commit() {
    let e = split_engine(
        &[],
        CommitOptions {
            auto_commit_at_full: true,
            ..split_opts()
        },
    );
    let r = e.convert("xtup", 50).unwrap();
    assert!(r.should_commit, "xt(学) × up(双拼) 唯一 ⇒ 应自动上屏");
    assert_eq!(r.commit_text, "学双拼");
}

/// 原帖规则三前半：后段有重码 → 不自动上屏，列出候选让用户选。
#[test]
fn split_ambiguous_back_segment_blocks_auto_commit() {
    let e = split_engine(
        &[],
        CommitOptions {
            auto_commit_at_full: true,
            ..split_opts()
        },
    );
    let r = e.convert("hfkn", 50).unwrap();
    assert!(!r.should_commit, "后段 kn 有重码 ⇒ 不得自动上屏");
    assert_eq!(texts(&r), vec!["很可能", "很可难"]);
}

/// 原帖规则三后半：「想要①就直接往后打」——第 5 个字母触发顶码，顶的是切分首选。
/// `handle_top_code` 取首选走 `self.convert(&prefix, 1)`，prefix 恰是满码长 ⇒ 切分照常触发，
/// 没有第四条上屏通路。
#[test]
fn split_top_code_commits_first_composition() {
    use crate::engine::Engine;
    let e = split_engine(
        &[],
        CommitOptions {
            top_code_commit: true,
            ..split_opts()
        },
    );
    assert_eq!(
        e.handle_top_code("hfknx"),
        Some(("很可能".to_string(), "x".to_string())),
        "顶码须顶出切分首选，余码 x 返回续打"
    );
}

/// `split_front_candidates = 2`：前段也列举，得到原帖配图那种效果。
///
/// 断言的是引擎内的构造序（前段外层、后段内层），屏幕序不一定是它：协调器的
/// `candidate_display_order` 会全量重排，实际按 `min(前,后)` 权重降序、按后段分组。取 2 以上
/// 两种序才会分叉，本夹具权重恰好让两者一致，测不出那个分叉（§4.2 记着的已知取舍）。
#[test]
fn split_front_candidates_expands_combinations() {
    let e = split_engine(
        &[("hf", "困", 300)],
        CommitOptions {
            split_front_candidates: 2,
            ..split_opts()
        },
    );
    let r = e.convert("hfkn", 50).unwrap();
    assert_eq!(
        texts(&r),
        vec!["很可能", "很可难", "困能", "困难"],
        "前段取 2 条时按「前段外层 × 后段内层」展开"
    );
}

/// 切点由 `max_code_length` 唯一决定，奇数/过短码长下功能整体关闭（构建期已告警）。
#[test]
fn split_disabled_on_unsupported_code_length() {
    let mut entries: Vec<(&str, &str, i32)> = SPLIT_ENTRIES.to_vec();
    entries.push(PREFIX_ONLY_ENTRY);
    let mut d = CodetableDict::empty();
    for (i, (code, text, w)) in entries.iter().enumerate() {
        d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(
        CachedDict::Memory(d),
        "codetable-system",
    )));
    // 码长 2：前段就占满了，后段无码可分 ⇒ `new` 就地关掉。
    // 别换成码长 5：恒切二简后 5 码是合法的（2+3），当反例测不到东西。
    let e = CodeTableEngine::new(2, split_opts(), Arc::new(dm));
    let r = e.convert("hf", 50).unwrap();
    // 只看「有没有切分候选」——列表里还会有词库自带的前缀候选，那与本判据无关。
    assert!(
        !r.candidates.iter().any(|c| c.is_split_composed),
        "码长不长于前段时功能整体关闭，实际: {:?}",
        texts(&r)
    );
}

/// `no_exact` 档不会自动上屏，挡住它的是 `has_longer` 而非「恰一个精确匹配」。
///
/// 切分候选的 `code` 就是整串，后段唯一时它恰好是唯一一条 `code == input`，所以真正挡住它的是
/// `decide_auto_commit` 最后那道 `has_longer`（本档能放行就意味着有前缀候选，必然有更长后继）。
/// 谁放宽了 `has_longer`，本档就会静默开始自动上屏。
#[test]
fn split_no_exact_never_auto_commits() {
    // 夹具要同时满足三条：`xtup` 无精确解、有前缀候选（否则 no_exact 退化成 empty 档）、
    // 后段 `up` 唯一。`xtupq` 一条把后两条一起给了：既是前缀候选，又是更长后继。
    let e = split_engine(
        &[("xtupq", "乙", 1000)],
        CommitOptions {
            split_trigger: SplitTrigger::NoExact,
            auto_commit_at_full: true,
            ..split_opts()
        },
    );
    let r = e.convert("xtup", 50).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.is_split_composed),
        "no_exact 档下应产出切分候选，实际: {:?}",
        texts(&r)
    );
    assert!(
        !r.should_commit,
        "no_exact 档不得自动上屏（挡它的是 has_longer）"
    );

    // 反向对照：同一串码在默认档下会自动上屏——证明「不上屏」是本档特有的。
    let dflt = split_engine(
        &[],
        CommitOptions {
            auto_commit_at_full: true,
            ..split_opts()
        },
    );
    let r2 = dflt.convert("xtup", 50).unwrap();
    assert!(r2.should_commit && r2.commit_text == "学双拼");
}

/// 与整句同开时，原帖规则三（继续打字母顶首选）会失效——整句让顶码整体让位。
///
/// 候选窗选词不受影响，构建时有 warn 说明。锁住它是因为调整 `handle_top_code` 的让位判据时，
/// 这条交互会跟着静默改变，而两个功能看起来毫不相干。
#[test]
fn split_top_code_yields_to_sentence_when_both_on() {
    use crate::engine::Engine;
    let mut entries: Vec<(&str, &str, i32)> = SPLIT_ENTRIES.to_vec();
    entries.extend_from_slice(SENTENCE_ENTRIES);

    let split_only = engine_opts(
        &entries,
        CommitOptions {
            split_input: true,
            top_code_commit: true,
            ..Default::default()
        },
    );
    assert_eq!(
        split_only.handle_top_code("hfknx"),
        Some(("很可能".to_string(), "x".to_string())),
        "只开逆切分：顶码顶出切分首选"
    );

    let both = engine_opts(
        &entries,
        CommitOptions {
            split_input: true,
            sentence_input: true,
            top_code_commit: true,
            ..Default::default()
        },
    );
    assert_eq!(
        both.handle_top_code("hfknx"),
        None,
        "同开整句：顶码整体让位 ⇒ 规则三落空（不是缺陷，是已知取舍）"
    );
}

/// 逆切分与整句占不同区间，可以同开、互不干扰。
#[test]
fn split_and_sentence_cover_different_ranges() {
    let mut entries: Vec<(&str, &str, i32)> = SPLIT_ENTRIES.to_vec();
    entries.extend_from_slice(SENTENCE_ENTRIES);
    let e = engine_opts(
        &entries,
        CommitOptions {
            split_input: true,
            sentence_input: true,
            ..Default::default()
        },
    );

    // 4 码（== 码长）→ 逆切分
    let r4 = e.convert("hfkn", 50).unwrap();
    let c4 = r4.candidates.first().expect("4 码应有组合候选");
    assert!(c4.is_split_composed && !c4.is_sentence, "4 码那格归逆切分");
    assert_eq!(c4.text, "很可能");

    // 8 码（> 码长）→ 整句
    let r8 = e.convert("aawtaawt", 50).unwrap();
    let c8 = r8.candidates.first().expect("8 码应有整句候选");
    assert!(c8.is_sentence && !c8.is_split_composed, "超码长那格归整句");
    assert_eq!(c8.text, "工作工作");
}

fn wildcard_opts(extra: CommitOptions) -> CommitOptions {
    CommitOptions {
        wildcard: Some('z'),
        ..extra
    }
}

fn slot_pattern(p: &str) -> String {
    p.replace('?', &wind_dict::WILDCARD_SLOT.to_string())
}

/// §3.1 / §3.2：等长优先（更长码权重再高也排后）、注释是完整编码、源标码表。
#[test]
fn wildcard_equal_length_first_and_full_code_comment() {
    let e = engine_opts(
        &[
            ("ab", "甲", 10),
            ("ac", "乙", 20),
            ("abcd", "丙", 9999),
            ("bb", "丁", 50),
        ],
        wildcard_opts(CommitOptions::default()),
    );
    assert_eq!(e.wildcard_key(), Some('z'));
    let r = e.convert_wildcard("az", &slot_pattern("a?"), 50).unwrap();
    let got: Vec<(&str, &str, &str, bool)> = r
        .candidates
        .iter()
        .map(|c| {
            (
                c.text.as_str(),
                c.code.as_str(),
                c.comment.as_str(),
                c.is_exact_code,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("乙", "ac", "ac", true),
            ("甲", "ab", "ab", true),
            ("丙", "abcd", "abcd", false),
        ]
    );
    assert!(
        r.candidates
            .iter()
            .all(|c| c.source == CandidateSource::CodeTable)
    );
    assert_eq!(
        r.preedit_display, "az",
        "组合区显示用户所打的原串，不是占位串"
    );
}

/// §3.2：通配下满码唯一也不自动上屏。反向对照：同一词库字面打全码照常上屏。
#[test]
fn wildcard_never_auto_commits() {
    let opts = CommitOptions {
        auto_commit_at_full: true,
        auto_commit_min_len: 4,
        ..Default::default()
    };
    let e = engine_opts(&[("aaaa", "工", 100)], wildcard_opts(opts));
    let r = e
        .convert_wildcard("azza", &slot_pattern("a??a"), 50)
        .unwrap();
    assert_eq!(r.candidates.len(), 1, "前置：通配唯一命中");
    assert!(
        !r.should_commit && r.commit_text.is_empty(),
        "通配结果不自动上屏"
    );
    let lit = e.convert("aaaa", 50).unwrap();
    assert!(lit.should_commit, "对照：字面全码仍自动上屏");
}

/// §3.2：通配满码无匹配也不请求清空。反向对照：字面满码空码照常清空。
#[test]
fn wildcard_never_requests_clear() {
    let opts = CommitOptions {
        clear_on_empty_max: true,
        ..Default::default()
    };
    let e = engine_opts(&[("aaaa", "工", 100)], wildcard_opts(opts));
    let r = e
        .convert_wildcard("bzzz", &slot_pattern("b???"), 50)
        .unwrap();
    assert!(r.is_empty && !r.should_clear, "通配空码不清空");
    let lit = e.convert("bbbb", 50).unwrap();
    assert!(
        lit.is_empty && lit.should_clear,
        "对照：字面满码空码照常清空"
    );
}

/// §3.1 / §11：精确匹配模式下不追加更长编码；条数跟随 max_candidates（不再写死 100）。
#[test]
fn wildcard_respects_single_code_input_and_max_candidates() {
    let e = engine_opts(
        &[("ab", "甲", 10), ("abcd", "丙", 9999)],
        wildcard_opts(CommitOptions {
            single_code_input: true,
            ..Default::default()
        }),
    );
    let r = e.convert_wildcard("az", &slot_pattern("a?"), 50).unwrap();
    let texts: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["甲"]);

    let many: Vec<(String, String, i32)> = (0..150u32)
        .map(|i| {
            let c1 = (b'a' + (i / 26) as u8) as char;
            let c2 = (b'a' + (i % 26) as u8) as char;
            (format!("q{c1}{c2}"), format!("字{i}"), 1)
        })
        .collect();
    let refs: Vec<(&str, &str, i32)> = many
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let e = engine_opts(&refs, wildcard_opts(CommitOptions::default()));
    let r = e
        .convert_wildcard("qzz", &slot_pattern("q??"), 1000)
        .unwrap();
    assert_eq!(
        r.candidates.len(),
        150,
        "要 1000 给全部 150 条（旧实现截在 100）"
    );
    let r = e
        .convert_wildcard("qzz", &slot_pattern("q??"), 120)
        .unwrap();
    assert_eq!(r.candidates.len(), 120, "按协调器给的 max_candidates 截");
}

/// 关闭通配时引擎不接通配请求；`convert` 永远是字面语义（它还被当活码探针用）。
#[test]
fn wildcard_off_means_no_wildcard_path_and_convert_stays_literal() {
    let e = engine_opts(&[("ab", "甲", 10)], CommitOptions::default());
    assert_eq!(e.wildcard_key(), None);
    assert!(e.convert_wildcard("az", &slot_pattern("a?"), 50).is_none());
    let on = engine_opts(&[("ab", "甲", 10)], wildcard_opts(CommitOptions::default()));
    assert!(
        on.convert("az", 50).unwrap().candidates.is_empty(),
        "开了通配，convert(\"az\") 仍按字面查——`z` 不是通配"
    );
}
/// `is_wildcard` 只由通配入口置位；字面 `convert` 的结果恒不带（`source_tier` 靠它区分）。
#[test]
fn wildcard_results_carry_is_wildcard_and_literal_does_not() {
    let e = engine_opts(
        &[("ab", "甲", 10), ("abcd", "丙", 5)],
        wildcard_opts(CommitOptions::default()),
    );
    let r = e.convert_wildcard("az", &slot_pattern("a?"), 50).unwrap();
    assert!(!r.candidates.is_empty(), "前置：通配有命中");
    assert!(r.candidates.iter().all(|c| c.is_wildcard));
    let lit = e.convert("ab", 50).unwrap();
    assert!(!lit.candidates.is_empty(), "前置：字面有命中");
    assert!(
        lit.candidates.iter().all(|c| !c.is_wildcard),
        "字面结果不带通配标记"
    );
}

/// 通配硬上限与翻页扩容上限是同一个常量；`max_candidates = 0` 回空，
/// 且不把 0 传给 `search_pattern`（Composite 把 0 当不限、各层把 0 当空）。
#[test]
fn wildcard_hard_cap_matches_expansion_cap_and_zero_is_empty() {
    assert_eq!(WILDCARD_RESULT_LIMIT, crate::engine::CANDIDATE_LIMIT_CAP);
    let n = WILDCARD_RESULT_LIMIT + 100;
    let many: Vec<(String, String, i32)> = (0..n as u32)
        .map(|i| {
            let c1 = (b'a' + (i / 676) as u8) as char;
            let c2 = (b'a' + (i / 26 % 26) as u8) as char;
            let c3 = (b'a' + (i % 26) as u8) as char;
            (format!("{c1}{c2}{c3}"), format!("字{i}"), 1)
        })
        .collect();
    let refs: Vec<(&str, &str, i32)> = many
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let e = engine_opts(&refs, wildcard_opts(CommitOptions::default()));
    let r = e
        .convert_wildcard("zzz", &slot_pattern("???"), usize::MAX)
        .unwrap();
    assert_eq!(r.candidates.len(), WILDCARD_RESULT_LIMIT, "硬上限兜底");
    let r0 = e.convert_wildcard("zzz", &slot_pattern("???"), 0).unwrap();
    assert!(r0.candidates.is_empty() && r0.is_empty, "max 0 ⇒ 空结果");
}

fn single_only_opts() -> CommitOptions {
    CommitOptions {
        wildcard: Some('z'),
        wildcard_single_only: true,
        ..Default::default()
    }
}

/// 先滤后截：`q??` 下 150 条高权重词组压着 120 条低权重单字：
/// 词库层按 limit 截出的前 100 条全是词组，事后过滤只剩 0 条。引擎须加倍重取到凑满。
#[test]
fn wildcard_single_only_filters_before_truncation() {
    let mut owned: Vec<(String, String, i32)> = Vec::new();
    for i in 0..150u32 {
        let c1 = (b'a' + (i / 26) as u8) as char;
        let c2 = (b'a' + (i % 26) as u8) as char;
        owned.push((format!("q{c1}{c2}"), format!("词组{i}"), 10_000 - i as i32));
    }
    for i in 0..120u32 {
        let c1 = (b'a' + (i / 26) as u8) as char;
        let c2 = (b'a' + (i % 26) as u8) as char;
        let ch = char::from_u32(0x4E00 + i).unwrap();
        owned.push((format!("q{c1}{c2}"), ch.to_string(), 1));
    }
    let refs: Vec<(&str, &str, i32)> = owned
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let off = engine_opts(&refs, wildcard_opts(CommitOptions::default()));
    let r = off
        .convert_wildcard("qzz", &slot_pattern("q??"), 100)
        .unwrap();
    assert!(
        r.candidates.iter().all(|c| c.text.starts_with("词组")),
        "前置：不过滤时前 100 条全是词组"
    );
    let on = engine_opts(&refs, single_only_opts());
    let r = on
        .convert_wildcard("qzz", &slot_pattern("q??"), 100)
        .unwrap();
    assert_eq!(r.candidates.len(), 100, "过滤后仍凑满 100 条");
    assert!(
        r.candidates
            .iter()
            .all(|c| wind_candidate::single_markable_char(&c.text).is_some())
    );
    let r = on
        .convert_wildcard("qzz", &slot_pattern("q??"), 500)
        .unwrap();
    assert_eq!(r.candidates.len(), 120, "词库取尽即停，不死循环");
}

/// 单字判据是字素簇（UAX #29），不是 `chars().count()`：ZWJ 序列、国旗算一个字。
#[test]
fn wildcard_single_only_counts_grapheme_clusters() {
    let e = engine_opts(
        &[
            ("qa", "👨‍👩‍👧", 40),
            ("qb", "🇨🇳", 30),
            ("qc", "工作", 20),
            ("qd", "工", 10),
        ],
        single_only_opts(),
    );
    let r = e.convert_wildcard("qz", &slot_pattern("q?"), 50).unwrap();
    let got: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(got, ["👨‍👩‍👧", "🇨🇳", "工"]);
}

fn disabled_mem(id: &str, entries: &[(&str, &str, i32)]) -> super::super::DisabledDictSource {
    let owned: Vec<(String, String, i32)> = entries
        .iter()
        .map(|(c, t, w)| (c.to_string(), t.to_string(), *w))
        .collect();
    super::super::DisabledDictSource {
        id: id.into(),
        base_order: 3,
        default_weight: None,
        load: Arc::new(move || {
            let mut d = CodetableDict::empty();
            for (i, (c, t, w)) in owned.iter().enumerate() {
                d.merge_single(c.clone(), t.clone(), *w, i as i32);
            }
            Ok(CachedDict::Memory(d))
        }),
    }
}

fn with_xz(e: CodeTableEngine) -> CodeTableEngine {
    e.with_disabled_dicts(super::super::DisabledDictLayers::new(
        vec![disabled_mem(
            "xz",
            &[("uuia", "门头沟区", 9999), ("uuia", "重码", 9999)],
        )],
        std::iter::empty(),
        None,
    ))
}

/// 普通 convert（精确 / 前缀 / 活码探针同一入口）永不读影子层，连加载都不触发。
#[test]
fn plain_convert_never_touches_disabled_layers() {
    let e = with_xz(engine_opts(
        &[("uuif", "立法", 10)],
        CommitOptions {
            reverse_key: Some('z'),
            ..wildcard_opts(CommitOptions::default())
        },
    ));
    for input in ["uuia", "uui", "u"] {
        let r = e.convert(input, 50).unwrap();
        assert!(r.candidates.iter().all(|c| c.text != "门头沟区"), "{input}");
    }
    assert_eq!(
        e.disabled_dicts().unwrap().load_count(),
        0,
        "普通 convert 不触发加载"
    );
    e.convert_reverse("uuiz", &slot_pattern("uui?"), 50)
        .unwrap();
    assert_eq!(e.disabled_dicts().unwrap().load_count(), 1);
    assert!(
        e.convert("uuia", 50)
            .unwrap()
            .candidates
            .iter()
            .all(|c| c.text != "门头沟区")
    );
}

/// 行内通配与反查都并查影子层，命中带 `from_disabled_dict` 且注释以 `*` 起头；已启用的不带。
#[test]
fn wildcard_and_reverse_both_read_disabled_layers_with_marker() {
    let e = with_xz(engine_opts(
        &[("uuif", "立法", 10)],
        CommitOptions {
            reverse_key: Some('z'),
            ..wildcard_opts(CommitOptions::default())
        },
    ));
    for r in [
        e.convert_wildcard("uuiz", &slot_pattern("uui?"), 50)
            .unwrap(),
        e.convert_reverse("uuiz", &slot_pattern("uui?"), 50)
            .unwrap(),
    ] {
        let xz = r.candidates.iter().find(|c| c.text == "门头沟区").unwrap();
        assert!(
            xz.from_disabled_dict && xz.comment.starts_with('*'),
            "{xz:?}"
        );
        let ok = r.candidates.iter().find(|c| c.text == "立法").unwrap();
        assert!(!ok.from_disabled_dict && !ok.comment.starts_with('*'));
    }
}

/// 仅单字 + 影子层。影子层的词组同样被滤掉；取尽判据是双边的：首轮影子层没取满（35 < 100）
/// 但主层取满了（100 条全是词组），必须再取一轮才能把主层被截掉的 10 个单字捞回来。
#[test]
fn reverse_single_only_filters_disabled_hits_and_exhausts_on_both_sides() {
    let code = |i: u32| {
        let c1 = (b'a' + (i / 26) as u8) as char;
        let c2 = (b'a' + (i % 26) as u8) as char;
        format!("q{c1}{c2}")
    };
    let mut owned: Vec<(String, String, i32)> = Vec::new();
    for i in 0..150u32 {
        owned.push((code(i), format!("词组{i}"), 10_000 - i as i32));
    }
    for i in 0..10u32 {
        owned.push((code(i), char::from_u32(0x4E00 + i).unwrap().to_string(), 1));
    }
    let refs: Vec<(&str, &str, i32)> = owned
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let mut xz: Vec<(String, String, i32)> = (0..30u32)
        .map(|i| (code(i), char::from_u32(0x5E00 + i).unwrap().to_string(), 1))
        .collect();
    xz.extend((0..5u32).map(|i| (code(i), format!("影子词组{i}"), 9999)));
    let xz_refs: Vec<(&str, &str, i32)> = xz
        .iter()
        .map(|(c, t, w)| (c.as_str(), t.as_str(), *w))
        .collect();
    let opts = CommitOptions {
        reverse_key: Some('z'),
        ..single_only_opts()
    };
    let e = engine_opts(&refs, opts).with_disabled_dicts(super::super::DisabledDictLayers::new(
        vec![disabled_mem("xz", &xz_refs)],
        std::iter::empty(),
        None,
    ));
    let r = e.convert_reverse("qzz", &slot_pattern("q??"), 100).unwrap();
    assert!(
        r.candidates
            .iter()
            .all(|c| wind_candidate::single_markable_char(&c.text).is_some()),
        "影子层词组同样被滤掉"
    );
    let flags: Vec<bool> = r.candidates.iter().map(|c| c.from_disabled_dict).collect();
    assert_eq!(
        flags.len(),
        40,
        "主层 10 个单字（须重取才见）+ 影子层 30 个"
    );
    assert!(flags[..10].iter().all(|f| !f) && flags[10..].iter().all(|f| *f));
}

/// spec §4.2：`(text, code)` 去重（同键留已启用那条），已启用排前，档内再按 base_sort。
#[test]
fn reverse_merges_disabled_after_enabled_dedup_by_text_code() {
    let e = with_xz(engine_opts(
        &[("uuif", "立法", 10), ("uuia", "重码", 1)],
        CommitOptions {
            reverse_key: Some('z'),
            ..wildcard_opts(CommitOptions::default())
        },
    ));
    let r = e
        .convert_reverse("uuiz", &slot_pattern("uui?"), 50)
        .unwrap();
    let got: Vec<(&str, &str, bool)> = r
        .candidates
        .iter()
        .map(|c| (c.text.as_str(), c.code.as_str(), c.from_disabled_dict))
        .collect();
    assert_eq!(
        got,
        [
            ("立法", "uuif", false),
            ("重码", "uuia", false),
            ("门头沟区", "uuia", true)
        ]
    );
}

/// 禁用一个已加载的扩展库 ⇒ 主 dm 摘层（普通候选消失），同时它进影子层（反查仍可见）；
/// 返回值语义不变（true = 目标态已达成）。
#[test]
fn set_dict_enabled_disable_moves_dict_into_disabled_layers() {
    let build = |entries: &[(&str, &str, i32)]| {
        let mut d = CodetableDict::empty();
        for (i, (code, text, w)) in entries.iter().enumerate() {
            d.merge_single(code.to_string(), text.to_string(), *w, i as i32);
        }
        CachedDict::Memory(d)
    };
    let dm = Arc::new(DictManager::new());
    dm.register_layer(Box::new(SystemDictLayer::new(
        build(&[("uuif", "立法", 10)]),
        "codetable-system",
    )));
    dm.register_layer(Box::new(SystemDictLayer::new(
        build(&[("aaae", "甘蓝菜", 50)]),
        "codetable-extra-ext",
    )));
    let opts = CommitOptions {
        reverse_key: Some('z'),
        ..wildcard_opts(CommitOptions::default())
    };
    let e = CodeTableEngine::new(4, opts, dm)
        .with_own_extra_dicts(["ext".to_string()])
        .with_disabled_dicts(super::super::DisabledDictLayers::new(
            vec![disabled_mem("ext", &[("aaae", "甘蓝菜", 50)])],
            ["ext".to_string()],
            None,
        ));
    assert!(e.set_dict_enabled("ext", false));
    assert!(
        e.convert("aaae", 20)
            .unwrap()
            .candidates
            .iter()
            .all(|c| c.text != "甘蓝菜")
    );
    let r = e
        .convert_reverse("aaaz", &slot_pattern("aaa?"), 20)
        .unwrap();
    assert!(
        r.candidates
            .iter()
            .any(|c| c.text == "甘蓝菜" && c.from_disabled_dict)
    );
}
/// spec §3.2：反查不受 wildcard 主开关约束；仅单字照样生效（同一内核）。
#[test]
fn convert_reverse_ignores_wildcard_switch() {
    let opts = CommitOptions {
        reverse_key: Some('z'),
        wildcard_single_only: true,
        ..Default::default()
    };
    let e = engine_opts(
        &[("ab", "甲", 10), ("ac", "乙丙", 20), ("bb", "丁", 5)],
        opts,
    );
    assert!(
        e.convert_wildcard("zb", &slot_pattern("?b"), 50).is_none(),
        "前置：主开关关"
    );
    let r = e.convert_reverse("zb", &slot_pattern("?b"), 50).unwrap();
    let got: Vec<(&str, &str, &str)> = r
        .candidates
        .iter()
        .map(|c| (c.text.as_str(), c.code.as_str(), c.comment.as_str()))
        .collect();
    assert_eq!(
        got,
        [("甲", "ab", "ab"), ("丁", "bb", "bb")],
        "首位通配、全码注释"
    );
    assert!(!r.should_commit && !r.should_clear);
}

#[test]
fn convert_reverse_absent_without_reverse_key() {
    let e = engine_opts(&[("ab", "甲", 10)], CommitOptions::default());
    assert!(e.convert_reverse("ab", "ab", 50).is_none());
}
