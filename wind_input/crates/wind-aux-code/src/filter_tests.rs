use super::*;
use crate::table::AuxCodeTable;
use wind_candidate::CandidateSource;

/// 构造最简候选（只填过滤用到的 text/source，其余用默认值）
fn cand(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        source: CandidateSource::Pinyin,
        ..Default::default()
    }
}

/// 合并双码表：拆分（高优）+ 小鹤（低优）
fn sample_table() -> AuxCodeTable {
    let chaifen = AuxCodeTable::from_rows(vec![
        ('李', "mz"), // 木+子
        ('樱', "my"), // 木+婴
        ('林', "mm"), // 木+木
        ('河', "sk"), // 氵+可
        ('海', "sm"), // 氵+每
        ('花', "ch"), // 艹+化
        ('草', "cz"), // 艹+早
        ('厑', "ii"), // rime 示例字
    ]);
    let xiaohe = AuxCodeTable::from_rows(vec![
        ('李', "mz"), // 木子
        ('河', "dk"), // 氵口
        ('樱', "mn"), // 木女
        ('厑', "ib"), // rime 示例第二码
    ]);
    // 高优表放前面 → 先入列 = first-seen 优先
    AuxCodeTable::merge(vec![chaifen, xiaohe])
}

/// 场景 1：高优先级表直接命中
#[test]
fn prefix_match_on_high_priority_table() {
    let t = sample_table();
    let cands = vec![cand("李"), cand("樱"), cand("林"), cand("河"), cand("花")];
    let out = filter_by_aux_code(cands, &t, "m", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    // 木字旁三字（李、樱、林）在高优先级拆分表中以 m 开头
    assert_eq!(kept, vec!["李", "樱", "林"]);
    let filtered: Vec<_> = out.filtered.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(filtered, vec!["河", "花"]);
}

/// 场景 2：高优先级表没命中，但低优先级表命中（跨表合并后 dk 仍在码列表里）
#[test]
fn prefix_match_falls_back_to_lower_priority_table() {
    let t = sample_table();
    // 河：拆分表 sk（首字母 s，不匹配 d），小鹤表 dk（首字母 d → 匹配）
    let cands = vec![cand("李"), cand("樱"), cand("河"), cand("花")];
    let out = filter_by_aux_code(cands, &t, "d", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["河"], "d 前缀匹配合并后小鹤码表的 dk");
}

/// 场景 3：rime 示例「厑」有 ii + ib，都能独立命中
#[test]
fn rime_example_duplicate_codes_both_match() {
    let t = sample_table();
    // i 前缀：ii、ib 均以 i 开头
    assert!(t.any_code_starts_with('厑', "i"));
    // ib：第二个码精确前缀
    assert!(t.any_code_starts_with('厑', "ib"));
    // ii：第一个码精确前缀
    assert!(t.any_code_starts_with('厑', "ii"));
    // 过滤：打 "i" 厑应该被保留
    let cands = vec![cand("厑"), cand("李")];
    let out = filter_by_aux_code(cands, &t, "i", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["厑"]);
}

/// 默认 PerCharPrefix：辅助码短于词组字数 = 前缀态，词组保留（边打边缩）——
/// 打 `m` 时「李」「李子」都保留（李子 首字 李=mz 命中 m、输入尚未打满）。
#[test]
fn phrase_kept_when_aux_input_is_prefix() {
    let t = sample_table();
    let cands = vec![
        cand("李"),   // 单字匹配 m，应留
        cand("李子"), // 词组 2 字，aux "m" 仅 1 位 < 2：前缀态 → 留
        cand("木头"), // 首字 木 表未收录（无码）→ 滤
    ];
    let out = filter_by_aux_code(cands, &t, "m", &AuxCodeFilterOptions::default());
    let texts: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["李", "李子"], "位数不足 N 时词组前缀态保留");
}

/// 构造小鹤式词组测试表（每字一个多码位条目的首码）
fn xiaohe_like_table() -> AuxCodeTable {
    AuxCodeTable::from_rows(vec![
        ('魔', "gg"), // 广（部首 g 开头）
        ('法', "ds"), // 氵（三点水 d 开头）
        ('少', "xp"), // 小（x 开头）
        ('女', "va"), // 乙/折（v 开头）
        ('李', "mz"), // 木+子
        ('子', "va"), // 子（z 开头）
        ('树', "mc"), // 木+寸
    ])
}

/// 词组逐字首码匹配：每字的第一个辅助码按序对齐（「魔法少女」→ g/d/x/v）
#[test]
fn phrase_matches_each_char_first_code() {
    let t = xiaohe_like_table();
    let cands = vec![cand("魔法少女"), cand("李")];
    let out = filter_by_aux_code(cands, &t, "gdxv", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        kept,
        vec!["魔法少女"],
        "默认 PerCharPrefix：g/d/x/v 逐字首码命中"
    );
}

/// 词组：辅助码位数不足 N = 前缀态，词组保留（用户还没输全每字首码，边打边缩）
#[test]
fn phrase_partial_input_keeps_phrase() {
    let t = xiaohe_like_table();
    let cands = vec![cand("魔法少女")];
    let out = filter_by_aux_code(cands, &t, "gd", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["魔法少女"], "4 字词只打 2 位 gd：前缀态，保留");
}

/// 词组：辅助码位数超过 N → 词组不匹配。多出的位没有字可对应——否则 2 字词
/// 「魔法」配 3 位 gdx 会被静默保留，挤掉真正匹配的 3 字词。位数不足
/// N 是前缀态：3 字/4 字词都保留（边打边缩）。
#[test]
fn phrase_excess_input_is_filtered() {
    let t = xiaohe_like_table();
    let cands = vec![cand("魔法"), cand("魔法少女"), cand("魔法少")];
    let out = filter_by_aux_code(cands, &t, "gdx", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        kept,
        vec!["魔法少女", "魔法少"],
        "3 位 gdx：2 字词(超出)滤；4 字前缀态 魔法少女 与 3 字 魔法少 按原序保留"
    );
}

/// kept 保持输入原序：辅助码只筛不改序。
#[test]
fn kept_preserves_input_order() {
    let t = AuxCodeTable::from_rows(vec![('李', "mz"), ('子', "va"), ('鬼', "mvk")]);
    let cands = vec![cand("李子"), cand("鬼"), cand("李子精")];
    let out = filter_by_aux_code(cands, &t, "mv", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        kept,
        vec!["李子", "鬼", "李子精"],
        "三个候选都命中，保持输入原序"
    );
}

/// 词组：某一位首码不匹配 → 整词过滤（末位 w 而非 v：女 首码 v）
#[test]
fn phrase_wrong_char_is_filtered() {
    let t = xiaohe_like_table();
    let cands = vec![cand("魔法少女")];
    let out = filter_by_aux_code(cands, &t, "gdxw", &AuxCodeFilterOptions::default());
    assert!(out.kept.is_empty(), "女 首码 v，w 不命中 → 整词滤");
}

/// 词组：同表不同字的首码命中差异（李树 vs 李子）
#[test]
fn phrase_distinguishes_by_each_char() {
    let t = xiaohe_like_table();
    let cands = vec![cand("李树"), cand("李子")];
    // 李树：李(m) 树(m)，第二位要求 z → 树不命中；李子：李(m) 子(v) → 命中
    let out = filter_by_aux_code(cands, &t, "mv", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李子"]);
}

/// 用户场景：候选「时间」「实践」，小鹤码 时=oc 间=mo 实=bd 践=zj。
/// 输 `om`：时间（时 o + 间 m）命中；实践首字 实=bd 首码 b ≠ o → 整词滤。
#[test]
fn phrase_om_keeps_time_only_not_practice() {
    let t = AuxCodeTable::from_rows(vec![('时', "oc"), ('间', "mo"), ('实', "bd"), ('践', "zj")]);
    let cands = vec![cand("时间"), cand("实践")];
    let out = filter_by_aux_code(cands, &t, "om", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    let filtered: Vec<_> = out.filtered.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["时间"], "om 只命中 时间");
    assert_eq!(filtered, vec!["实践"], "实践被滤掉，不再出现在候选窗");
}

/// 用户场景：候选含单字「时」与词组「时间」，小鹤码 时=oc 间=mo 实=bd。
/// 打 `o`：单字 时（oc 以 o 开头）与词组 时间（首字 时 命中 o、输入尚未打满 =
/// 前缀态）都保留；实（bd 不以 o 开头）滤掉。
#[test]
fn prefix_input_keeps_single_and_phrase() {
    let t = AuxCodeTable::from_rows(vec![('时', "oc"), ('间', "mo"), ('实', "bd")]);
    let cands = vec![cand("时"), cand("时间"), cand("实")];
    let out = filter_by_aux_code(cands, &t, "o", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    let filtered: Vec<_> = out.filtered.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["时", "时间"], "打 o：时 和 时间 都保留");
    assert_eq!(filtered, vec!["实"]);
}

/// 词组某字符码表未收录（无论是否汉字，如 'a'/'1'/'木'）→ 该词过滤
#[test]
fn phrase_with_unlisted_char_is_filtered() {
    let t = xiaohe_like_table();
    // 完整匹配不中：李=mz 命中 m，但 'a'/'1'/木/，无码 → 逐字不中 → 滤
    let cands = vec![cand("李a"), cand("李1"), cand("木头"), cand("，")];
    let out = filter_by_aux_code(cands, &t, "mz", &AuxCodeFilterOptions::default());
    assert!(out.kept.is_empty(), "非汉字/未收录字 一律滤");
    // 前缀匹配不中：李=mz 命中 m，但 'a'/'1' 无码 → 逐字不中 → 滤
    let cands = vec![cand("李a"), cand("李1")];
    let out = filter_by_aux_code(cands, &t, "ma", &AuxCodeFilterOptions::default());
    assert!(out.kept.is_empty(), "非汉字无码 → 自然过滤，无需纯汉字判断");
}

/// 非汉字字符在码表里有码即可参与匹配（如「多啦A梦」A→a），PerCharPrefix 逐字命中
#[test]
fn phrase_with_coded_english_char_can_match() {
    let t = AuxCodeTable::from_rows(vec![('多', "xx"), ('啦', "kl"), ('梦', "mx"), ('A', "a")]);
    let cands = vec![cand("多啦A梦")];
    let out = filter_by_aux_code(cands.clone(), &t, "xkam", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["多啦A梦"], "A 有码 a → 逐字 x/k/a/m 命中");
    // 末位错 → 滤
    let out = filter_by_aux_code(cands, &t, "xkax", &AuxCodeFilterOptions::default());
    assert!(out.kept.is_empty());
}

/// 词组 kept 也保持原相对顺序（子序列不变量扩展到词组）
#[test]
fn phrase_kept_preserves_original_relative_order() {
    let t = xiaohe_like_table();
    let input = vec![cand("李子"), cand("魔法少女"), cand("李树")];
    let out = filter_by_aux_code(input, &t, "mv", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    // 李子 命中；魔法少女（需 gdxv）/李树（第二位 m≠v）滤掉 → kept 只含李子
    assert_eq!(kept, vec!["李子"]);
}

/// 码表未收录的单字：一律过滤（用户确认的规则）
#[test]
fn unlisted_char_is_filtered_out() {
    let t = sample_table();
    let cands = vec![
        cand("李"), // 收录 → 留
        cand("王"), // 所有表都没收录 → 滤
        cand("张"), // 未收录 → 滤
    ];
    let out = filter_by_aux_code(cands, &t, "m", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    let filtered: Vec<_> = out.filtered.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李"]);
    assert_eq!(filtered, vec!["王", "张"], "未收录字必须过滤");
}

/// 非汉字（英/数/标点）：码表无码 → 自然过滤（单字「，」查表无码被滤，
/// 多字符 hello/123 作为词组逐字匹配、首字无码被滤）
#[test]
fn non_hanzi_is_filtered() {
    let t = sample_table();
    let cands = vec![cand("李"), cand("hello"), cand("123"), cand("，")];
    let out = filter_by_aux_code(cands, &t, "m", &AuxCodeFilterOptions::default());
    let texts: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["李"]);
}

/// 非汉字单字在码表里有码即参与匹配（不做字集判断）：单字符 A→a，aux "a" 命中 A
#[test]
fn single_non_hanzi_char_with_code_can_match() {
    let t = AuxCodeTable::from_rows(vec![('李', "mz"), ('A', "a")]);
    let cands = vec![cand("李"), cand("A")];
    let out = filter_by_aux_code(cands.clone(), &t, "a", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["A"], "A 有码 a → 命中；李 mz 无 a → 滤");
    let out = filter_by_aux_code(cands, &t, "m", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李"], "李 mz 有 m → 命中；A 无 m → 滤");
}

/// 辅助码输入为空 → 不过滤（原样放行，防御语义）
#[test]
fn empty_aux_input_no_filter() {
    let t = sample_table();
    let cands = vec![cand("李"), cand("樱"), cand("李子")];
    let out = filter_by_aux_code(cands, &t, "", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李", "樱", "李子"], "空输入不筛选，原样放行");
    assert!(out.filtered.is_empty());
}

/// 空码表（未挂任何辅助码）→ 不过滤（原样放行）
#[test]
fn empty_store_no_filter() {
    let t = AuxCodeTable::new();
    let cands = vec![cand("李"), cand("樱"), cand("李子")];
    let out = filter_by_aux_code(cands, &t, "m", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李", "樱", "李子"], "空表不筛选，原样放行");
    assert!(out.filtered.is_empty());
}

/// 核心不变量：kept 顺序 == 原候选的子序列（相对顺序不被任何辅助码因素改变）
#[test]
fn kept_preserves_original_relative_order() {
    let t = sample_table();
    // 原顺序：李、樱、河、林、海、花、草
    let input = vec![
        cand("李"), // m 前缀命中 (1)
        cand("樱"), // m 前缀命中 (2)
        cand("河"), // m 不命中（s/d 前缀）
        cand("林"), // m 前缀命中 (3)
        cand("海"), // m 不命中
        cand("花"), // m 不命中
        cand("草"), // m 不命中
    ];
    let out = filter_by_aux_code(input, &t, "m", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    // 必须保持「李、樱、林」的原相对顺序，不能因为优先级而乱序
    assert_eq!(kept, vec!["李", "樱", "林"]);
}

/// kept + filtered == 输入集合（不重不漏）
#[test]
fn kept_and_filtered_partition_input() {
    let t = sample_table();
    let input = vec![
        cand("李"),
        cand("樱"),
        cand("河"),
        cand("花"),
        cand("李子"),
        cand("王"),
    ];
    let n = input.len();
    let out = filter_by_aux_code(input, &t, "m", &AuxCodeFilterOptions::default());
    assert_eq!(
        out.kept.len() + out.filtered.len(),
        n,
        "保留集 + 被滤集 必须等于原候选总数"
    );
}

/// 默认 max_phrase_len=0（不限）：所有词组均参与筛选，不再因长度自动排除。
#[test]
fn long_phrase_filtered_by_normal_logic_when_no_limit() {
    let t = xiaohe_like_table();
    let cands = vec![cand("魔法少女魔法少"), cand("魔法少女"), cand("李")];
    // max_phrase_len=0 不限：7 字词「魔法少女魔法少」首字魔=gg 不中 m → 常规滤；
    // 4 字词「魔法少女」同理；李 单字照常。
    let out = filter_by_aux_code(cands, &t, "m", &AuxCodeFilterOptions::default());
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    let filtered: Vec<_> = out.filtered.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李"], "单字恒参与；4 字词不中 m 照常滤");
    assert_eq!(
        filtered,
        vec!["魔法少女魔法少", "魔法少女"],
        "7 字词首字不匹配 m 被常规滤；4 字词同理"
    );
}

/// max_phrase_len=0 = 不限：长词组重新参与筛选。
#[test]
fn max_phrase_len_zero_disables_limit() {
    let t = xiaohe_like_table();
    let opts = AuxCodeFilterOptions { max_phrase_len: 0 };
    let cands = vec![cand("魔法少女团"), cand("李")];
    // 魔法少女团：首字魔=gg 不中 m → 常规滤（0 不限时不因长度直接排除）。
    let out = filter_by_aux_code(cands, &t, "m", &opts);
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李"]);
}

/// max_phrase_len=7：字数 >7 的词组被长度排除，不参与辅助码匹配。
/// 8 字词首字命中 m 也会被排除（长度优先于匹配逻辑）。
#[test]
fn max_phrase_len_7_excludes_long_phrases() {
    let t = xiaohe_like_table();
    let opts = AuxCodeFilterOptions { max_phrase_len: 7 };
    // 「李子树魔法少女团」8 字，首字李=m 命中 m → 但被长度排除（8 > 7）；
    // 「魔法少女」4 字，首字魔=gg 不中 m → 常规滤；「李」单字照常。
    let cands = vec![cand("李子树魔法少女团"), cand("魔法少女"), cand("李")];
    let out = filter_by_aux_code(cands, &t, "m", &opts);
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    let filtered: Vec<_> = out.filtered.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李"], "单字照常；8 字词即使首字命中也因长度排除");
    assert_eq!(
        filtered,
        vec!["李子树魔法少女团", "魔法少女"],
        "8 字词长度排除，4 字词常规滤"
    );
}

/// 自定义上限：如设 4 → 字数 >4 的词组排除、≤4 的词组按常规逻辑裁决。
#[test]
fn max_phrase_len_custom_threshold() {
    let t = xiaohe_like_table();
    let opts = AuxCodeFilterOptions { max_phrase_len: 4 };
    // 魔法少女=4 字，4 >4 为 false → 不被长度排除，但首字魔=gg 不中 m → 常规滤；
    // 李子=2 字首字李=m 命中 m（前缀态）→ 保留；李 单字照常。
    let cands = vec![cand("魔法少女"), cand("李子"), cand("李")];
    let out = filter_by_aux_code(cands, &t, "m", &opts);
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        kept,
        vec!["李子", "李"],
        "4 字词常规滤，短词/单字按原序保留"
    );
}

/// 长度边界三态：<上限保留，=上限保留，>上限排除。
///
/// 用 max_phrase_len=4，三组候选首字均为「李」(m) 命中输入 "m"，
/// 隔离长度逻辑——仅字数决定去留。
#[test]
fn max_phrase_len_boundary_lt_eq_gt() {
    let t = xiaohe_like_table();
    let opts = AuxCodeFilterOptions { max_phrase_len: 4 };
    let cands = vec![
        cand("李子树"),     // 3 字 < 4 → 保留
        cand("李子树法"),   // 4 字 = 4 → 保留（> 才排除）
        cand("李子树魔法"), // 5 字 > 4 → 排除
        cand("李"),         // 单字 → 保留
    ];
    let out = filter_by_aux_code(cands, &t, "m", &opts);
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    let filtered: Vec<_> = out.filtered.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        kept,
        vec!["李子树", "李子树法", "李"],
        "< 上限保留；= 上限保留；单字保留"
    );
    assert_eq!(filtered, vec!["李子树魔法"], "> 上限排除");
}

/// 长度排除不破坏子序列不变量：kept/filtered 各自的相对顺序仍是输入的子序列。
#[test]
fn max_len_exclusion_keeps_subsequence_invariant() {
    let t = xiaohe_like_table();
    let opts = AuxCodeFilterOptions { max_phrase_len: 7 };
    let input = vec![
        cand("李"),
        cand("李子树魔法少女团"),
        cand("李子"),
        cand("魔法少女"),
    ];
    // 8 字词被长度排除（8 > 7）；4 字词首字魔=gg 不中 m → 常规滤；李/李子保留。
    let out = filter_by_aux_code(input, &t, "m", &opts);
    let kept: Vec<_> = out.kept.iter().map(|c| c.text.as_str()).collect();
    let filtered: Vec<_> = out.filtered.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李", "李子"], "短词按原序保留");
    assert_eq!(
        filtered,
        vec!["李子树魔法少女团", "魔法少女"],
        "长度排除与常规滤各自按原序，相对顺序保持"
    );
}

/// aux_code_matches 谓词：单字前缀匹配
#[test]
fn aux_code_matches_single_char() {
    let t = sample_table();
    let opts = AuxCodeFilterOptions::default();
    assert!(aux_code_matches(&cand("李"), &t, "m", &opts));
    assert!(!aux_code_matches(&cand("李"), &t, "s", &opts));
    assert!(!aux_code_matches(&cand("王"), &t, "m", &opts));
}

/// aux_code_matches 谓词：词组逐字首码匹配
#[test]
fn aux_code_matches_phrase() {
    let t = xiaohe_like_table();
    let opts = AuxCodeFilterOptions::default();
    assert!(aux_code_matches(&cand("魔法少女"), &t, "gdxv", &opts));
    assert!(aux_code_matches(&cand("魔法少女"), &t, "gd", &opts));
    assert!(!aux_code_matches(&cand("魔法少女"), &t, "gdxw", &opts));
}

/// aux_code_matches 谓词：空输入/空表 = passthrough
#[test]
fn aux_code_matches_empty_passthrough() {
    let t = sample_table();
    let opts = AuxCodeFilterOptions::default();
    assert!(aux_code_matches(&cand("王"), &t, "", &opts));
    let empty = AuxCodeTable::new();
    assert!(aux_code_matches(&cand("王"), &empty, "m", &opts));
}
