use super::*;
use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
use wind_dict::codetable::CodetableDict;
use wind_store::Store;

fn empty_engine() -> PinyinEngine {
    PinyinEngine::new(
        Config::default(),
        CachedDict::Memory(CodetableDict::empty()),
    )
}

/// 把补全召回门槛设回 2 / 3 的配置。
///
/// 出厂 `completion_min_syllables` 为 4：输入不足 4 个音节时上限收紧到 `started` 本身。用 2~3
/// 音节输入验证排序 / 上浮的用例会在召回层就拿不到样本，要验的逻辑一次都没执行到。凡是
/// 「已召回之后谁排前面」的用例都该用本函数；门槛自身由 `wind-coordinator` 的
/// `pinyin_completion_recall_gate` 守。
fn relaxed_completion_config() -> Config {
    Config {
        completion_min_syllables: 2,
        completion_max_extra_syllables: 3,
        ..Config::default()
    }
}

// ── 音节分析（顶码歧义裁决用；对齐 Go isPossiblePinyinSequence / isWholeSyllablePinyin /
//    hasNonInitialSingleLetterSyllable）。trie 为封闭标准音节集，不依赖词典。──

#[test]
fn possible_pinyin_sequence_matches_go_cases() {
    let e = empty_engine();
    // 完整音节 / 音节前缀 / 完整音节+尾部前缀 → true
    assert!(e.is_possible_pinyin_sequence("wang")); // 单完整音节
    assert!(e.is_possible_pinyin_sequence("zhon")); // zhong 的前缀
    assert!(e.is_possible_pinyin_sequence("yans")); // yan + 尾部前缀 s
    assert!(e.is_possible_pinyin_sequence("naap")); // na + a + 前缀 p
    // 非拼音 / 首音节单字母 → false
    assert!(!e.is_possible_pinyin_sequence("rcqn")); // 无完整音节也非前缀
    assert!(!e.is_possible_pinyin_sequence("gggg")); // g 非合法音节/前缀
    assert!(!e.is_possible_pinyin_sequence("abcd")); // 首音节 a 为单字母
}

#[test]
fn whole_syllable_pinyin_matches_go_cases() {
    let e = empty_engine();
    assert!(e.is_whole_syllable_pinyin("wang")); // 单完整音节
    assert!(e.is_whole_syllable_pinyin("aipu")); // ai+pu 恰好覆盖
    assert!(!e.is_whole_syllable_pinyin("zhon")); // 残缺前缀（非完整音节）
    assert!(!e.is_whole_syllable_pinyin("yans")); // yan + 残缺 s
    assert!(!e.is_whole_syllable_pinyin("abcd")); // 首音节单字母简拼
}

#[test]
fn non_initial_single_letter_syllable_matches_go_cases() {
    let e = empty_engine();
    assert!(e.has_non_initial_single_letter_syllable("naap")); // na + a（第二音节单字母）
    assert!(!e.has_non_initial_single_letter_syllable("yans")); // yan + 残缺 s（残缺不计）
    assert!(!e.has_non_initial_single_letter_syllable("aipu")); // ai + pu 皆双字母
    assert!(!e.has_non_initial_single_letter_syllable("abcd")); // 首位单字母不算「非首位」
}

/// 构造含指定 (code, text) 词条的最小拼音引擎（每条 weight=100，order 递增）。
fn engine_with_words(words: &[(&str, &str)]) -> PinyinEngine {
    let mut raw = CodetableDict::empty();
    for (i, (code, text)) in words.iter().enumerate() {
        raw.merge_single(code.to_string(), text.to_string(), 100, i as i32);
    }
    PinyinEngine::new(Config::default(), CachedDict::Memory(raw))
}

// ── ü 拼写归一化（`xv` → `xu`、`nue` → `nve`）。规则与安全性论证见 `spelling` 模块。──

/// jqxy 后的 `v` 打通到词典查询（`xv` 召回 `xu` 的词）。
///
/// 断言落在召回结果上而不是 `normalize_u_umlaut` 的返回值：后者在 `spelling` 单测里已验过，
/// 这里要证归一化确实接在了 `convert` 的链路上。
#[test]
fn umlaut_v_after_jqxy_recalls_u_words() {
    let e = engine_with_words(&[("xu", "需"), ("ju", "居"), ("qun", "群"), ("yuan", "元")]);
    for (stroke, want) in [("xv", "需"), ("jv", "居"), ("qvn", "群"), ("yvan", "元")] {
        let r = e.convert(stroke, 10).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == want),
            "`{stroke}` 应召回「{want}」，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }
}

/// n/l 后的 `ue` 是 üe 的另一种常见写法，同样打通。
#[test]
fn umlaut_ue_after_nl_recalls_ve_words() {
    let e = engine_with_words(&[("nve", "虐"), ("lve", "略")]);
    for (stroke, want) in [("nue", "虐"), ("lue", "略")] {
        let r = e.convert(stroke, 10).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == want),
            "`{stroke}` 应召回「{want}」，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }
}

/// `nv`/`lv` 已经是音节表形态，归一化不得动它们（回归防线：若误加
/// 「n/l 后 v→u」那一臂，`nv` 会变成 `nu`、「女」直接打不出）。
#[test]
fn umlaut_keeps_nl_v_spelling_intact() {
    let e = engine_with_words(&[("nv", "女"), ("lv", "绿"), ("nu", "怒"), ("lu", "路")]);
    let r = e.convert("nv", 10).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "女"),
        "`nv` 须仍出「女」，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    let r = e.convert("nu", 10).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "怒"),
        "`nu` 不该被归一化（后面没有 e），须仍出「怒」，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 归一化是等长替换 ⇒ 编码栏与 `consumed_length` 不得错位。
/// 用户打 `xv` 看到的是归一化形态 `xu`（与主流输入法一致）。
#[test]
fn umlaut_normalization_preserves_preedit_length() {
    let e = engine_with_words(&[("xu", "需"), ("xuyao", "需要")]);
    let r = e.convert("xvyao", 10).unwrap();
    let top = r
        .candidates
        .iter()
        .find(|c| c.text == "需要")
        .expect("应召回「需要」");
    assert_eq!(
        top.consumed_length,
        "xvyao".len(),
        "等长替换 ⇒ 消费整串，长度须按击键串算"
    );
    assert_eq!(
        r.preedit_display, "xu'yao",
        "编码栏显示归一化形态，音节边界照常"
    );
}

/// 合成候选：`p*` 普通、`a*` 简拼、`c*` 纠错，按给出的次序即已排好序。
fn quota_list(spec: &[(&str, usize)]) -> Vec<Candidate> {
    spec.iter()
        .flat_map(|&(kind, n)| (0..n).map(move |i| (kind, i)))
        .map(|(kind, i)| Candidate {
            text: format!("{kind}{i}"),
            is_abbrev: kind == "a",
            is_corrected: kind == "c",
            ..Default::default()
        })
        .collect()
}

fn quota_texts(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.text.as_str()).collect()
}

/// 两份保底同时不足：简拼腾位不吃纠错保底的那几席，两边都拿满。
#[test]
fn abbrev_and_correction_quotas_both_hold() {
    // 窗口 50：简拼保底 5、纠错保底 4；窗口里全是普通候选。
    let mut c = quota_list(&[("p", 60), ("c", 6), ("a", 8)]);
    truncate_with_quotas(&mut c, 50, true, &|c| c.is_corrected);
    assert_eq!(c.len(), 50);
    let t = quota_texts(&c);
    assert_eq!(t.iter().filter(|t| t.starts_with('c')).count(), 4, "{t:?}");
    assert_eq!(t.iter().filter(|t| t.starts_with('a')).count(), 5, "{t:?}");
    // 普通候选只被挤掉尾部、相对次序不变。
    let plain: Vec<&str> = t.iter().copied().filter(|t| t.starts_with('p')).collect();
    assert_eq!(plain, quota_texts(&quota_list(&[("p", 41)])));
    // 纠错按分取最前的 4 条。
    let corr: Vec<&str> = t.iter().copied().filter(|t| t.starts_with('c')).collect();
    assert_eq!(corr, ["c0", "c1", "c2", "c3"]);
}

/// 小窗口不给纠错保底：limit=1 首选不被挤掉，limit=9 也一席不占。
#[test]
fn correction_quota_never_displaces_small_window() {
    for max in [1, 8, 9] {
        let mut c = quota_list(&[("p", 20), ("c", 6)]);
        truncate_with_quotas(&mut c, max, false, &|c| c.is_corrected);
        assert_eq!(
            quota_texts(&c),
            quota_texts(&quota_list(&[("p", max)])),
            "{max}"
        );
    }
    assert_eq!(correction_quota(10), 1);
    assert_eq!(correction_quota(300), 4);
}

/// 关纠错（谓词恒假）时只有简拼保底：补 5 条简拼、挤掉尾部 5 条普通候选。
#[test]
fn quotas_without_correction_match_abbrev_only() {
    let mut a = quota_list(&[("p", 60), ("c", 6), ("a", 8)]);
    truncate_with_quotas(&mut a, 50, true, &|_| false);
    let plain = quota_list(&[("p", 45)]);
    let mut want = quota_texts(&plain);
    want.extend(["a0", "a1", "a2", "a3", "a4"]);
    assert_eq!(quota_texts(&a), want);
}

fn correction_fuzzy() -> FuzzyConfig {
    let mut c = wind_config::CorrectionConfig::default();
    c.rules.gn_ng = true; // 出厂关；用例是 gogn
    FuzzyConfig {
        correction: Arc::new(correction::CorrectionRules::from_config(&c)),
        ..Default::default()
    }
}

/// 换位纠错拼写成为可切的边：`gogn` 出「工」。
#[test]
fn correction_spelling_is_cuttable() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("gong".to_string(), "工".to_string(), 100, 0);
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw))
        .with_fuzzy(correction_fuzzy());
    let r = eng.convert("gogn", 10).unwrap();
    assert!(r.candidates.iter().any(|c| c.text == "工"), "{r:?}");
}

/// 双拼不启用换位纠错：两个 builder 谁先调都把纠错置空。
#[test]
fn shuangpin_drops_correction() {
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = || Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let dict = || CachedDict::Memory(CodetableDict::empty());
    let a = PinyinEngine::new(Config::default(), dict())
        .with_fuzzy(correction_fuzzy())
        .with_shuangpin(ShuangpinConverter::new(layout()));
    assert!(a.fuzzy_config.correction.is_empty());
    let b = PinyinEngine::new(Config::default(), dict())
        .with_shuangpin(ShuangpinConverter::new(layout()))
        .with_fuzzy(correction_fuzzy());
    assert!(b.fuzzy_config.correction.is_empty());
}

/// 双拼下 `v` 是布局键（小鹤 `v` = zh 声母），归一化绝不能触及。
/// 这条守的是 `convert_with_opts` 与 `normalize_gate_input` 两处的 `shuangpin` 短路。
#[test]
fn umlaut_normalization_never_touches_shuangpin() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("zhu".to_string(), "主".to_string(), 100, 0);
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw))
        .with_shuangpin(ShuangpinConverter::new(layout));
    // 小鹤 `vu` = zh + u → zhu。若 `v` 被误当成 ü 换成 `u`，这里会变成 `uu` 而全盘落空。
    let r = eng.convert("vu", 10).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "主"),
        "双拼 `vu` 须仍解析为 zhu，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    // 混输闸门那一路同样不得归一化。
    assert!(
        matches!(
            eng.normalize_gate_input("xv"),
            std::borrow::Cow::Borrowed(_)
        ),
        "双拼下闸门入参须原样借用"
    );
}

/// 手动分隔符是用户对切分的显式声明，归一化的相邻判据不得跨越它。
/// `nu'e` 明确要「nu + e」，不该被改写成 `nve`。
#[test]
fn umlaut_manual_separator_blocks_ue_rewrite() {
    let e = empty_engine();
    assert!(
        matches!(
            spelling::normalize_u_umlaut("nu'e"),
            std::borrow::Cow::Borrowed(_)
        ),
        "`'` 隔断相邻关系，`ue` 规则不触发"
    );
    // 闸门侧同样成立（走的是同一个函数）。
    let _ = e;
}

/// Task 8 Step 2：手动分隔符强制音节硬边界。
/// 词典含 "xian"→"先" 与 "xi"/"an" 单字；带分隔符 xi'an 强制切分 [xi,an]，
/// 跨界单音节词「先」(code=xian 却仅 1 字) 不得出现；preedit 保留手动 `'`。
#[test]
fn separator_forces_syllable_boundary() {
    let e = engine_with_words(&[("xian", "先"), ("xi", "西"), ("an", "安")]);
    let r = e.convert("xi'an", 50).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.text == "先"),
        "分隔符应阻止跨界音节 xian 匹配，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert!(
        r.preedit_display.contains('\''),
        "preedit 应保留手动分隔符，实际: {:?}",
        r.preedit_display
    );
    assert!(
        !r.candidates.is_empty(),
        "分隔符切分后仍应有候选（如「西」）"
    );
}

/// Task 8 Step 2：末尾分隔符必须立即显示，且不清空候选。
#[test]
fn trailing_separator_kept_in_preedit() {
    let e = engine_with_words(&[("ni", "你")]);
    let r = e.convert("ni'", 50).unwrap();
    assert!(
        r.preedit_display.ends_with('\''),
        "末尾分隔符必须立即显示，实际: {:?}",
        r.preedit_display
    );
    assert!(!r.candidates.is_empty(), "末尾分隔符不应清空候选");
}

/// Task 8 自审：五个边界（空段/开头 '/连续 ''/纯 '/末尾 '）均不 panic，
/// 且手动分隔符在 preedit 中原样保留。
#[test]
fn separator_edge_cases_no_panic() {
    let e = engine_with_words(&[("ni", "你"), ("hao", "好"), ("xi", "西"), ("an", "安")]);

    // 开头 '：xi 段仍应产候选，preedit 以 ' 起头
    let r = e.convert("'xi", 20).unwrap();
    assert!(r.preedit_display.starts_with('\''), "开头分隔符应保留");
    assert!(r.candidates.iter().any(|c| c.text == "西"));

    // 连续 ''：等价单边界，preedit 保留双分隔符
    let r = e.convert("ni''hao", 20).unwrap();
    assert!(r.preedit_display.contains("''"), "连续分隔符应原样保留");
    assert!(r.candidates.iter().any(|c| c.text == "你"));

    // 纯 ' / 连续纯 '：无拼音可查，无候选、仅回显分隔符，不 panic
    for pure in ["'", "''", "'''"] {
        let r = e.convert(pure, 20).unwrap();
        assert!(r.candidates.is_empty(), "纯分隔符输入 {pure:?} 不应有候选");
        assert_eq!(r.preedit_display, pure, "纯分隔符应原样回显");
    }
}

fn tmp_store(name: &str) -> Arc<Store> {
    let p = std::env::temp_dir().join(format!("wind_pinyin_{name}.redb"));
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

/// L 造词显现：挂上用户/临时层后，拼音造的词应进入候选（即便主词典为空）。
#[test]
fn store_layer_words_appear_in_candidates() {
    let store = tmp_store("layer_show");
    store
        .add_user_word("pinyin", "nihao", "你好", 500, 0)
        .unwrap();
    store
        .learn_temp_word("pinyin", "lanshou", "蓝瘦", 800, 0)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
        store.clone(),
        "pinyin",
    )));
    let engine = empty_engine().with_store_layers(Arc::new(dm));

    // 整串精确命中用户词
    let r = engine.convert("nihao", 20).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "你好"),
        "用户造词「你好」应出现在拼音候选"
    );
    // 临时词同样显现
    let r2 = engine.convert("lanshou", 20).unwrap();
    let shou = r2.candidates.iter().find(|c| c.text == "蓝瘦");
    assert!(shou.is_some(), "临时造词「蓝瘦」应出现在拼音候选");
    assert_eq!(
        shou.unwrap().source,
        CandidateSource::Pinyin,
        "来源应标为拼音"
    );
}

/// 同文合并不得抹掉来源归属。
///
/// 右键删除按 `is_user_dict`/`is_temp_dict` 选表、按码拼 key，`redb` 的 `remove` 对不存在的
/// key 静默成功——临时层的词与整句层同文时若被盖成 `is_user_dict=true`，删除就点多少次都无作用。
/// （此处用同文的系统词条代替整句候选，对合并分支而言二者等价。）
#[test]
fn merged_store_candidate_keeps_source_flags() {
    let store = tmp_store("merge_flags");
    store
        .learn_temp_word("pinyin", "zaiyebuhao", "再也不好", 800, 0)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
        store.clone(),
        "pinyin",
    )));
    let engine = engine_with_words(&[("zaiyebuhao", "再也不好")]).with_store_layers(Arc::new(dm));
    let r = engine.convert("zaiyebuhao", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "再也不好")
        .expect("同文候选应存在");
    assert!(
        c.meta.is_temp_dict,
        "临时词合并后必须仍标临时词——删除据此选 temp_words 表"
    );
    assert!(!c.meta.is_user_dict, "临时词不得被盖成用户词");
    assert_eq!(
        c.meta.store_code.as_deref(),
        Some("zaiyebuhao"),
        "存储码须随标记一起带出"
    );
}

/// 存储码 ≠ 候选码时（用户词经前缀补全命中），`store_code` 必须给出记录的那个码。
/// `code` 字段仍归已有候选（边界语义，见合并分支注释），故删除只能靠本字段。
#[test]
fn merged_store_candidate_carries_real_store_code() {
    let store = tmp_store("merge_store_code");
    store
        .add_user_word("pinyin", "nihaoa", "你好", 500, 0)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    let engine = PinyinEngine::new(
        relaxed_completion_config(),
        CachedDict::Memory({
            let mut raw = CodetableDict::empty();
            raw.merge_single("nihao".into(), "你好".into(), 100, 0);
            raw
        }),
    )
    .with_store_layers(Arc::new(dm));
    let r = engine.convert("nihao", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("同文候选应存在");
    assert!(c.meta.is_user_dict, "用户词合并后应标用户词");
    assert_eq!(c.code, "nihao", "候选码仍归已有候选（边界须同进同出）");
    assert_eq!(
        c.meta.store_code.as_deref(),
        Some("nihaoa"),
        "存储码须是用户词记录自身的码，否则删除拼出的 key 落空"
    );
}

/// 无 store 层时行为不变（不 panic、空词典无候选）。
#[test]
fn no_store_layer_is_inert() {
    let engine = empty_engine();
    let r = engine.convert("nihao", 20).unwrap();
    assert!(r.candidates.is_empty(), "空词典无 store 层应无候选");
}

/// 部分消费：用户词码是输入的前缀（nihao ⊂ nihaoma）→ consumed_length 标为前缀长度，
/// 选中后保留剩余拼音续转（分段上屏）。
#[test]
fn store_word_prefix_marks_partial_consumption() {
    let store = tmp_store("layer_partial");
    store
        .add_user_word("pinyin", "nihao", "你好", 500, 0)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    let engine = empty_engine().with_store_layers(Arc::new(dm));
    let r = engine.convert("nihaoma", 20).unwrap();
    let c = r.candidates.iter().find(|c| c.text == "你好");
    assert!(c.is_some(), "前缀用户词应作为分段候选出现");
    assert_eq!(
        c.unwrap().consumed_length,
        "nihao".len(),
        "应只消费前缀 nihao"
    );
}

// ── 模糊音召回用户/临时词。
//
// 用户词落库的是规范读音码（模糊命中造词走 `learn_code`，见 `pinyin_learn_code`），
// step 6 查 store 时须和系统词库的 `lookup_with_fuzzy` 一样展开模糊变体：开了 c=ch 后
// `chaijiuduolian` 要能打出自己加的「菜就多练 / caijiuduolian」。

/// 挂上只含给定用户词的 store 层，模糊音按 `fz` 配置。系统词典为空，
/// 故候选只可能来自用户层。
fn engine_with_user_words_fuzzy(
    tag: &str,
    words: &[(&str, &str, u64)],
    fz: FuzzyConfig,
) -> PinyinEngine {
    let store = tmp_store(tag);
    for (code, text, boundary) in words {
        store
            .add_user_word("pinyin", code, text, 500, *boundary)
            .unwrap();
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
        store.clone(),
        "pinyin",
    )));
    empty_engine()
        .with_fuzzy(fz)
        .with_store_layers(Arc::new(dm))
}

fn ch_c() -> FuzzyConfig {
    FuzzyConfig {
        ch_c: true,
        ..Default::default()
    }
}

/// 主诉求：c=ch 开启时，翘舌打法召回平舌码的用户词。
///
/// 候选对外的 `code` 留用户敲的那份（`consumed_length` 与分步上屏依赖它，同系统词模糊
/// 命中），规范码经 `store_code`（右键删除按它拼 key）与 `learn_code`（造词写库）带出。
#[test]
fn fuzzy_input_recalls_user_word_stored_under_canonical_code() {
    let eng =
        engine_with_user_words_fuzzy("fz_user_full", &[("caijiuduolian", "菜就多练", 0)], ch_c());
    let r = eng.convert("chaijiuduolian", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "菜就多练")
        .unwrap_or_else(|| {
            panic!(
                "c=ch 下 chaijiuduolian 应召回用户词「菜就多练」，实际: {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    assert!(c.meta.is_user_dict, "来源应标用户词");
    assert!(c.is_fuzzy, "应标模糊命中（据此施加权重折扣）");
    assert!(c.weight < 500, "模糊命中须打折扣，实际 weight={}", c.weight);
    assert_eq!(c.code, "chaijiuduolian", "候选码留用户敲的那份");
    assert_eq!(c.consumed_length, "chaijiuduolian".len(), "应消费整串");
    assert_eq!(
        c.meta.store_code.as_deref(),
        Some("caijiuduolian"),
        "删除要按记录自身的码拼 key"
    );
    assert_eq!(
        c.meta.learn_code.as_ref().map(|(code, _)| code.as_str()),
        Some("caijiuduolian"),
        "造词写库须用规范码，不得写入翘舌原码"
    );
}

/// step 6 的模糊变体同样按所打码的全部切分展开：`jinan` 的 maximum_match 是 `ji|nan`，
/// 用户词「静安」在 `jin|an → jing|an` 上。记录边界与变体边界同域，对不上的不收：只开
/// an=ang 时 `xi|an|fa → xi|ang|fa` 拼出的 `xiangfa` 不该借来 `xiang|fa` 的「想法」。
/// 关掉整句：那一路另有用户词模糊节点，会以整句身份先占住同文，不是本测试要钉的。
#[test]
fn store_fuzzy_expands_every_segmentation_and_checks_boundary() {
    let words = [("jingan", "静安", 0b1_0001), ("xiangfa", "想法", 0b10_0001)];
    let word_texts = |mut eng: PinyinEngine, input: &str| -> Vec<String> {
        eng.config.use_smart_compose = false;
        eng.convert(input, 50)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect()
    };
    let in_ing = FuzzyConfig {
        in_ing: true,
        ..Default::default()
    };
    let got = word_texts(
        engine_with_user_words_fuzzy("fz_user_allseg", &words, in_ing),
        "jinan",
    );
    assert!(
        got.iter().any(|t| t == "静安"),
        "jinan 应召回用户词「静安」，实际: {got:?}"
    );

    let an_ang = FuzzyConfig {
        an_ang: true,
        ..Default::default()
    };
    let got = word_texts(
        engine_with_user_words_fuzzy("fz_user_xseg", &words, an_ang),
        "xianfa",
    );
    assert!(
        !got.iter().any(|t| t == "想法"),
        "xianfa 不该借来「想法」，实际: {got:?}"
    );
}

/// 手输码的用户词没有边界（0）：按码现切一次（[`effective_boundary`]）再核对，不能一律放行。
/// 只开 an=ang 时 `xianfa` 的 `xi|an|fa → xi|ang|fa` 拼出 `xiangfa`，而手输的「想法」现切是
/// `xiang|fa`，切法对不上；整串与前缀子码（`xianfaxian` 的 `xianfa`）两路都不该借它。
/// 同一份现切也不能误杀：`zongguo` 的 `zhong|guo` 照样召回手输的「中国」。
#[test]
fn store_fuzzy_checks_derived_boundary_of_manual_words() {
    let words = [("xiangfa", "想法", 0), ("zhongguo", "中国", 0)];
    let word_texts = |mut eng: PinyinEngine, input: &str| -> Vec<String> {
        eng.config.use_smart_compose = false;
        eng.convert(input, 50)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect()
    };
    let an_ang = FuzzyConfig {
        an_ang: true,
        ..Default::default()
    };
    for input in ["xianfa", "xianfaxian"] {
        let got = word_texts(
            engine_with_user_words_fuzzy("fz_user_manual_x", &words, an_ang.clone()),
            input,
        );
        assert!(
            !got.iter().any(|t| t == "想法"),
            "{input} 不该借来手输的「想法」，实际: {got:?}"
        );
    }
    let zh_z = FuzzyConfig {
        zh_z: true,
        ..Default::default()
    };
    let got = word_texts(
        engine_with_user_words_fuzzy("fz_user_manual_z", &words, zh_z),
        "zongguo",
    );
    assert!(
        got.iter().any(|t| t == "中国"),
        "zongguo 应召回「中国」，实际: {got:?}"
    );
}

/// 词候选的整码模糊展开不受词图 `max_word_len`（10 音节）限制：11 音节的词典整词，
/// 模糊打法照样要召回。
#[test]
fn fuzzy_word_lookup_covers_codes_beyond_lattice_word_limit() {
    let code = "zhongguo".repeat(5) + "zhong";
    let mut d = CodetableDict::empty();
    d.merge_single(code.clone(), "长词".into(), 100, 0);
    let mut eng =
        PinyinEngine::new(Config::default(), CachedDict::Memory(d)).with_fuzzy(FuzzyConfig {
            zh_z: true,
            ..Default::default()
        });
    eng.config.use_smart_compose = false;
    let input = "zhongguo".repeat(5) + "zong";
    let got: Vec<String> = eng
        .convert(&input, 50)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert!(got.iter().any(|t| t == "长词"), "实际: {got:?}");
}

/// 双拼同键音节对用户词同样零罚：step 6 的 store 召回与整句词图的用户词模糊节点。
#[test]
fn shuangpin_shared_key_recalls_user_words_without_penalty() {
    let layout = r#"
[meta]
id = "shared"
name = "同键测试"
[finals]
q = ["iu", "ui"]
m = ["ian"]
i = ["i"]
"#;
    let conv = ShuangpinConverter::new(Layout::from_toml_str(layout).unwrap());
    let mut eng = engine_with_user_words_fuzzy(
        "sp_shared_user",
        // 边界：dui|lian = 位 0、3；ni = 位 0。整句节点要求有真值边界。
        &[("duilian", "对联", 0b1001), ("ni", "你", 0b1)],
        FuzzyConfig::default(),
    )
    .with_shuangpin(conv);
    // 先关整句：否则整句词图那条会先产出同文候选、盖住 step 6 召回的标记。
    eng.config.use_smart_compose = false;
    let r = eng.convert("dqlm", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "对联")
        .unwrap_or_else(|| {
            panic!(
                "dqlm 应召回用户词「对联」: {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    assert!(!c.is_fuzzy && c.weight >= 500, "零罚不标模糊: {c:?}");
    eng.config.use_smart_compose = true;
    let r = eng.convert("dqlmni", 20).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "对联你"),
        "整句须用上用户词的对端读音: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 手动分隔符、单音节、双音节、仅部分音节模糊，都应召回。
#[test]
fn fuzzy_user_word_recall_covers_separator_and_short_words() {
    let eng = engine_with_user_words_fuzzy(
        "fz_user_shapes",
        &[
            ("caijiuduolian", "菜就多练", 0),
            ("caidan", "菜单", 0),
            ("cai", "蔡", 0),
            ("cuchuan", "粗喘", 0),
        ],
        ch_c(),
    );
    for (input, want) in [
        ("chai'jiu'duo'lian", "菜就多练"),
        ("chaidan", "菜单"),
        ("chai", "蔡"),
        // 反向 + 仅部分音节模糊：c→ch 在首音节、ch 原样在次音节。
        ("chuchuan", "粗喘"),
        // 反向：平舌打法召回翘舌码。
        ("cucuan", "粗喘"),
    ] {
        let r = eng.convert(input, 20).unwrap();
        assert!(
            r.candidates.iter().any(|c| c.text == want),
            "{input} 应召回用户词「{want}」，实际: {:?}",
            r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }
}

/// 前缀子短语同样走模糊：`chaijiuduolian` 里的 `chaijiu` 召回用户词「菜就」，
/// 且只消费 `chaijiu`（分步上屏，剩余拼音续转）。
#[test]
fn fuzzy_user_word_recalled_as_subphrase() {
    let eng = engine_with_user_words_fuzzy("fz_user_sub", &[("caijiu", "菜就", 0)], ch_c());
    let r = eng.convert("chaijiuduolian", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "菜就")
        .expect("前缀子短语应模糊召回用户词「菜就」");
    assert!(c.is_fuzzy);
    assert_eq!(c.consumed_length, "chaijiu".len(), "应只消费 chaijiu");
}

/// 同文时精确命中优先：用户词里同时有精确码与模糊码的同一个词，候选须按精确那条算
/// （不打折、不标模糊）。
#[test]
fn exact_user_word_wins_over_fuzzy_duplicate() {
    let eng = engine_with_user_words_fuzzy(
        "fz_user_dup",
        &[("chaidan", "菜单", 0), ("caidan", "菜单", 0)],
        ch_c(),
    );
    let r = eng.convert("chaidan", 20).unwrap();
    let c = r.candidates.iter().find(|c| c.text == "菜单").unwrap();
    assert!(!c.is_fuzzy, "精确命中在先，不得被模糊那条盖掉");
    assert_eq!(c.weight, 500);
    // 存储码也须指向精确那条：否则右键删除删掉的是 `caidan`，`chaidan` 原样重现。
    assert!(
        c.meta.store_code.as_deref().is_none_or(|s| s == "chaidan"),
        "存储码不得被模糊那条改写成规范码，实际: {:?}",
        c.meta.store_code
    );
}

/// 简拼合并同 step 6 的守卫：同层同文两条记录（所打码精确那条 + 模糊键那条），模糊那条
/// 后到时不得改写存储码——否则右键删除删掉的是它，用户指向的精确记录原样留着。
/// n=l 下打 `lh`：`lihao` 的简拼键就是 `lh`（0 处改动），`nihao` 经变体键 `nh` 捞回（1 处）。
#[test]
fn abbrev_fuzzy_duplicate_does_not_steal_store_code() {
    let eng = engine_with_user_words_fuzzy(
        "fz_abbrev_dup",
        &[("lihao", "你好", 0b101), ("nihao", "你好", 0b101)],
        FuzzyConfig {
            n_l: true,
            ..Default::default()
        },
    );
    let r = eng.convert("lh", 20).unwrap();
    let c = r.candidates.iter().find(|c| c.text == "你好").unwrap();
    assert_eq!(
        c.meta.store_code.as_deref().unwrap_or(&c.code),
        "lihao",
        "存储码须指向精确那条记录"
    );
}

/// 同上，走简拼前缀回退（`recall_abbrev_prefix`）：`lhx` 整串无产出，退到切点 `lh`，
/// 尾部 `x` 留作残码。两条同文记录在那里合并，模糊那条同样不得改写存储码。
#[test]
fn abbrev_prefix_fallback_fuzzy_duplicate_does_not_steal_store_code() {
    let eng = engine_with_user_words_fuzzy(
        "fz_abbrev_prefix_dup",
        &[("lihao", "你好", 0b101), ("nihao", "你好", 0b101)],
        FuzzyConfig {
            n_l: true,
            ..Default::default()
        },
    );
    let r = eng.convert("lhx", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .unwrap_or_else(|| panic!("前缀回退应出「你好」，实际: {:?}", texts(&r)));
    assert_eq!(c.consumed_length, 2, "前提：走的是前缀回退（只消费 lh）");
    assert_eq!(
        c.meta.store_code.as_deref().unwrap_or(&c.code),
        "lihao",
        "存储码须指向精确那条记录"
    );
}

/// 关模糊音时行为不变：翘舌打法召不回平舌码的用户词，平舌打法照旧命中。
#[test]
fn fuzzy_off_user_word_recall_unchanged() {
    let eng = engine_with_user_words_fuzzy(
        "fz_user_off",
        &[("caijiuduolian", "菜就多练", 0), ("caijiu", "菜就", 0)],
        FuzzyConfig::default(),
    );
    let r = eng.convert("chaijiuduolian", 20).unwrap();
    assert!(
        r.candidates.is_empty(),
        "模糊音关闭时不应有任何召回，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    let r = eng.convert("caijiuduolian", 20).unwrap();
    let c = r.candidates.iter().find(|c| c.text == "菜就多练").unwrap();
    assert!(!c.is_fuzzy);
    assert_eq!(c.code, "caijiuduolian");
    assert_eq!(c.meta.learn_code, None, "精确命中的码即规范码，不另带");
}

// ── 整句建图 S2 的用户词节点同样走模糊（`lattice::add_store_nodes`）。
//
// 记录存的是规范码 + 规范码坐标下的边界；整句按所打码的跨度点查，不展开模糊的话翘舌打法
// `chaijiu` 撞不上 `caijiu`，整句里认不出自造的「菜就」。

/// [`engine_with_user_words_fuzzy`] + 打开 `sentence_uses_user_words`（`on`）。
fn sentence_engine_with_user_words_fuzzy(
    tag: &str,
    words: &[(&str, &str, u64)],
    fz: FuzzyConfig,
    on: bool,
) -> PinyinEngine {
    let mut eng = engine_with_user_words_fuzzy(tag, words, fz);
    eng.config.sentence_uses_user_words = on;
    eng
}

// ── 「屏蔽这个词」（拼音按文字删除）──

fn weighted_engine(words: &[(&str, &str, i32)]) -> PinyinEngine {
    let mut raw = CodetableDict::empty();
    for (i, (code, text, w)) in words.iter().enumerate() {
        raw.merge_single(code.to_string(), text.to_string(), *w, i as i32);
    }
    PinyinEngine::new(Config::default(), CachedDict::Memory(raw))
}

fn block(eng: &PinyinEngine, words: &[&str]) {
    eng.set_blocked_words(Arc::new(words.iter().map(|w| w.to_string()).collect()));
}

const SHIJIAN_WORDS: &[(&str, &str, i32)] = &[
    ("shi", "时", 9000),
    ("shi", "识", 10),
    ("jian", "间", 9000),
    ("jian", "见", 10),
    ("dao", "到", 5000),
    ("le", "了", 5000),
    ("daole", "到了", 5000),
];

/// 被屏蔽的词典整词不进候选、不进整句图；整句换成另一条覆盖整串的解读，不是半截。
#[test]
fn blocked_word_leaves_candidates_and_lattice() {
    let mut words = SHIJIAN_WORDS.to_vec();
    words.push(("shijian", "时间", 90000));
    words.push(("shijian", "实践", 1000));
    let eng = weighted_engine(&words);
    assert_eq!(sentence_of(&eng, "shijian").unwrap().text, "时间", "前提");
    block(&eng, &["时间"]);
    let r = eng.convert("shijian", 50).unwrap();
    assert!(r.candidates.iter().all(|c| c.text != "时间"));
    let s = sentence_of(&eng, "shijian").expect("整句仍在");
    assert_ne!(s.text, "时间");
    assert_eq!(s.code, "shijian", "整句覆盖整串");
    assert_eq!(r.candidates[0].text, s.text, "首选是覆盖整串的整句");
}

/// 由单字拼出的整句恰为被屏蔽的文字：N-best 取下一条（出厂只算 1 条时也要换）。
#[test]
fn sentence_equal_to_blocked_word_takes_next_best() {
    let eng = weighted_engine(SHIJIAN_WORDS);
    assert_eq!(
        sentence_of(&eng, "shijian").unwrap().text,
        "时间",
        "前提：单字拼出「时间」"
    );
    block(&eng, &["时间"]);
    let s = sentence_of(&eng, "shijian").expect("换下一条整句");
    assert_ne!(s.text, "时间");
    assert_eq!(s.code, "shijian");
    assert_eq!(s.text.chars().count(), 2);
}

/// 挂了语法模型时 `decode_nbest` 只回 1 条：撞上屏蔽词也要有替补整句，不能整条丢掉、首选退成半截。
#[test]
fn blocked_sentence_with_grammar_still_has_replacement() {
    let eng = weighted_engine(SHIJIAN_WORDS).with_grammar(Arc::new(grammar::NullGrammar));
    assert_eq!(sentence_of(&eng, "shijian").unwrap().text, "时间", "前提");
    block(&eng, &["时间"]);
    let s = sentence_of(&eng, "shijian").expect("挂语法模型时同样换下一条整句");
    assert_ne!(s.text, "时间");
    assert_eq!(s.code, "shijian");
}

/// 被屏蔽的文字作为更长整句的一段、由更小的块拼出，不拦。
#[test]
fn blocked_word_inside_longer_sentence_kept() {
    let mut words = SHIJIAN_WORDS.to_vec();
    words.push(("shijian", "时间", 90000));
    let eng = weighted_engine(&words);
    block(&eng, &["时间"]);
    assert_eq!(sentence_of(&eng, "shijiandaole").unwrap().text, "时间到了");
}

/// 空集合 = 无屏蔽：结果与不推一样。
#[test]
fn empty_blocked_set_changes_nothing() {
    let eng = weighted_engine(SHIJIAN_WORDS);
    let before: Vec<String> = eng
        .convert("shijian", 50)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect();
    block(&eng, &[]);
    let after: Vec<String> = eng
        .convert("shijian", 50)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert_eq!(before, after);
}

fn sentence_of(eng: &PinyinEngine, input: &str) -> Option<Candidate> {
    eng.convert(input, 50)
        .unwrap()
        .candidates
        .into_iter()
        .find(|c| c.is_sentence)
}

/// 「菜就」caijiu（cai|jiu → 位 0/3）、「多练」duolian（duo|lian → 位 0/3）。
const CAIJIU_DUOLIAN: &[(&str, &str, u64)] =
    &[("caijiu", "菜就", 0b1001), ("duolian", "多练", 0b1001)];

/// 主诉求：c=ch 下翘舌打法，整句由两个用户词拼出，且节点落在所打码域
/// （整句的码与消费长度都是用户敲的那串，分步上屏按它走）。
#[test]
fn fuzzy_sentence_joins_user_words_under_retroflex_typing() {
    let eng = sentence_engine_with_user_words_fuzzy("fz_s2_join", CAIJIU_DUOLIAN, ch_c(), true);
    let s = sentence_of(&eng, "chaijiuduolian").expect("应产出整句");
    assert_eq!(s.text, "菜就多练");
    assert_eq!(s.code, "chaijiuduolian", "整句码留用户敲的那份");
    assert_eq!(s.consumed_length, "chaijiuduolian".len());
    // 精确打法照旧（对照）。
    let s = sentence_of(&eng, "caijiuduolian").expect("精确打法应产出整句");
    assert_eq!(s.text, "菜就多练");
}

/// 记录边界必须逐音节对上变体码的切分：`ca|ijiu` 这种边界的记录不进图，
/// `boundary == 0` 的也不进（同精确路径的约束 2）。
#[test]
fn fuzzy_sentence_rejects_user_word_with_mismatched_boundary() {
    for (tag, boundary) in [("fz_s2_badmask", 0b101u64), ("fz_s2_nomask", 0)] {
        let eng = sentence_engine_with_user_words_fuzzy(
            tag,
            &[("caijiu", "菜就", boundary), ("duolian", "多练", 0b1001)],
            ch_c(),
            true,
        );
        let s = sentence_of(&eng, "chaijiuduolian");
        assert!(
            s.as_ref().is_none_or(|c| !c.text.contains("菜就")),
            "边界 {boundary:#b} 对不上变体码切分，不得进整句，实际: {:?}",
            s.map(|c| c.text)
        );
    }
}

/// 名次为 `rank` 的整句（N-best 下 1 = 最优解）。
fn sentence_ranked(eng: &PinyinEngine, input: &str, rank: u8) -> Option<String> {
    eng.convert(input, 50)
        .unwrap()
        .candidates
        .into_iter()
        .find(|c| c.is_sentence && c.sentence_rank == rank)
        .map(|c| c.text)
}

/// 精确优先：同一跨度上所打码精确命中「柴旧」、模糊命中「菜就」，同权重时整句取精确那条。
/// 开 N-best（露 2）看第二名：「菜就多练」必须在图里且输掉，才证明是罚分压住了它。
#[test]
fn fuzzy_sentence_prefers_exact_user_word() {
    let mut eng = sentence_engine_with_user_words_fuzzy(
        "fz_s2_exact",
        &[
            ("caijiu", "菜就", 0b1001),
            ("chaijiu", "柴旧", 0b10001),
            ("duolian", "多练", 0b1001),
        ],
        ch_c(),
        true,
    );
    eng.config.sentence_count = 2;
    eng.config.sentence_max_count = 2;
    assert_eq!(
        sentence_ranked(&eng, "chaijiuduolian", 1).as_deref(),
        Some("柴旧多练"),
        "精确命中不得被模糊那条压过"
    );
    assert_eq!(
        sentence_ranked(&eng, "chaijiuduolian", 2).as_deref(),
        Some("菜就多练"),
        "模糊那条应在图里、排第二"
    );
}

/// 记录边界对应的不是最短 / 最先遍历到的那条切分，模糊打法仍能进整句。
///
/// f=h 下打 `hanganhanganhangan`：每 6 字母有 `han|gan`、`hang|an` 两种切法，三段共 8 条切分；
/// 记录「方案方案方案」的边界 `fang|an` ×3 按图边升序是最后一条，先枚举路径再按条数截断会把它截掉。
#[test]
fn fuzzy_sentence_matches_record_on_non_first_segmentation() {
    let mut code = String::new();
    let mut boundary = 0u64;
    for syl in ["fang", "an", "fang", "an", "fang", "an"] {
        boundary |= 1 << code.len();
        code.push_str(syl);
    }
    let eng = sentence_engine_with_user_words_fuzzy(
        "fz_s2_multipath",
        &[(&code, "方案方案方案", boundary), ("ba", "吧", 0b1)],
        FuzzyConfig {
            f_h: true,
            ..Default::default()
        },
        true,
    );
    let s = sentence_of(&eng, "hanganhanganhanganba").expect("应产出整句");
    assert_eq!(s.text, "方案方案方案吧");
    // 整句边界落在所打码域、且是记录对应的那条切分：hang|an|hang|an|hang|an|ba
    let mut want = 0u64;
    let mut pos = 0;
    for syl in ["hang", "an", "hang", "an", "hang", "an", "ba"] {
        want |= 1 << pos;
        pos += syl.len();
    }
    assert_eq!(s.boundary, want);
}

/// 模糊用户节点赢下同词同跨度的系统词模糊节点时，切分一起换成用户记录对应的那条。
///
/// 系统词「西昂」码 `xiang`，ian=iang 下 `xian` 的系统模糊节点切分取 `any_path`（最少音节）
/// = 一个音节；用户记录「西昂」边界 `xi|ang`（所打码 `xi|an`）赢下后，整句边界应是 `xi|an|duo|lian`。
#[test]
fn fuzzy_user_node_win_replaces_segmentation() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("xiang".to_string(), "西昂".to_string(), 500, 0);
    let store = tmp_store("fz_s2_replace");
    for (code, text, boundary) in [("xiang", "西昂", 0b101u64), ("duolian", "多练", 0b1001)] {
        store
            .add_user_word("pinyin", code, text, 500, boundary)
            .unwrap();
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(store, "pinyin")));
    let eng = PinyinEngine::new(
        Config {
            sentence_uses_user_words: true,
            ..Default::default()
        },
        CachedDict::Memory(raw),
    )
    // ian=iang 让系统词在 `xian` 单音节上模糊命中「西昂」；an=ang 让用户记录经 `xi|an` 对上。
    .with_fuzzy(FuzzyConfig {
        an_ang: true,
        ian_iang: true,
        ..Default::default()
    })
    .with_store_layers(Arc::new(dm));
    let s = sentence_of(&eng, "xianduolian").expect("应产出整句");
    assert_eq!(s.text, "西昂多练");
    // xi|an|duo|lian → 位 0/2/4/7；系统节点那条 xian|duo|lian 是位 0/4/7。
    assert_eq!(s.boundary, 0b1001_0101, "应换成用户记录对应的切分");
    // 规范码随胜者一起换：用户记录 xi|ang，而非系统节点按变体推出的 xiang 单音节。
    assert_eq!(
        s.meta.learn_code,
        Some(code_and_mask(&["xi", "ang", "duo", "lian"]))
    );
}

/// 用户精确记录赢下同词的系统模糊节点时，规范码清回 `None`（所打码即规范码）。
///
/// 系统词「西昂」码 `xiang`，ian=iang 下以 `xian` 模糊命中；用户记录「西昂」就登记在 `xian`
/// 上、带加成赢下该节点。还挂着系统节点的 `xiang` 的话，学进去的是 `xiangduolian`。
#[test]
fn exact_user_node_win_clears_canon() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("xiang".to_string(), "西昂".to_string(), 500, 0);
    let store = tmp_store("s2_exact_clears_canon");
    for (code, text, boundary) in [("xian", "西昂", 0b101u64), ("duolian", "多练", 0b1001)] {
        store
            .add_user_word("pinyin", code, text, 500, boundary)
            .unwrap();
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(store, "pinyin")));
    let eng = PinyinEngine::new(
        Config {
            sentence_uses_user_words: true,
            ..Default::default()
        },
        CachedDict::Memory(raw),
    )
    .with_fuzzy(FuzzyConfig {
        ian_iang: true,
        ..Default::default()
    })
    .with_store_layers(Arc::new(dm));
    let s = sentence_of(&eng, "xianduolian").expect("应产出整句");
    assert_eq!(s.text, "西昂多练");
    assert_eq!(s.meta.learn_code, None);
}

/// 精确分支同理：用户精确记录赢下同词同跨度的系统词节点时，切分换成用户记录那条。
///
/// 系统词「西安」码 `xian` 无边界，`build` 取 `any_path` = 一个音节；用户记录「西安」边界
/// `xi|an` 赢下后，整句边界应是 `xi|an|duo|lian`。
#[test]
fn exact_user_node_win_replaces_segmentation() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("xian".to_string(), "西安".to_string(), 500, 0);
    let store = tmp_store("s2_exact_replace");
    for (code, text, boundary) in [("xian", "西安", 0b101u64), ("duolian", "多练", 0b1001)] {
        store
            .add_user_word("pinyin", code, text, 500, boundary)
            .unwrap();
    }
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(store, "pinyin")));
    let eng = PinyinEngine::new(
        Config {
            sentence_uses_user_words: true,
            ..Default::default()
        },
        CachedDict::Memory(raw),
    )
    .with_store_layers(Arc::new(dm));
    let s = sentence_of(&eng, "xianduolian").expect("应产出整句");
    assert_eq!(s.text, "西安多练");
    // xi|an|duo|lian → 位 0/2/4/7；系统节点那条 xian|duo|lian 是位 0/4/7。
    assert_eq!(s.boundary, 0b1001_0101, "应换成用户记录对应的切分");
}

// ── 模糊整句的造词码（`Candidate::meta.learn_code`）。
//
// 整句候选的 `code` 是所打码（`consumed_length` / 分步上屏绑在它上面），协调层
// `learn_phrase_on_commit` 优先取 `learn_code`。模糊整句若不带规范码，学进去的就是
// `chaijiuduolian` 这种非规范码，下次同样打法零罚精确命中它。

/// 由音节序列算 (码, 边界)。
fn code_and_mask(syls: &[&str]) -> (String, u64) {
    let mut code = String::new();
    let mut mask = 0u64;
    for s in syls {
        mask |= 1 << code.len();
        code.push_str(s);
    }
    (code, mask)
}

/// S2 用户词模糊节点 + 精确节点混排：造词码 = 各节点规范码拼接，边界换到规范码坐标。
#[test]
fn fuzzy_sentence_learn_code_is_canonical() {
    let eng = sentence_engine_with_user_words_fuzzy("fz_s2_learn", CAIJIU_DUOLIAN, ch_c(), true);
    let s = sentence_of(&eng, "chaijiuduolian").expect("应产出整句");
    assert_eq!(s.text, "菜就多练");
    assert_eq!(s.code, "chaijiuduolian", "候选码仍是所打码");
    assert_eq!(
        s.meta.learn_code,
        Some(code_and_mask(&["cai", "jiu", "duo", "lian"])),
        "造词码须为规范码（菜就走模糊、多练走精确）"
    );
    // 全精确：不带造词码（与候选码相同），行为逐位不变。
    let s = sentence_of(&eng, "caijiuduolian").expect("精确打法应产出整句");
    assert_eq!(s.meta.learn_code, None);
}

/// 系统词模糊节点（`build` 的模糊分支）同样带规范码；词条无边界时按变体音节推。
#[test]
fn system_fuzzy_sentence_learn_code_is_canonical() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("zhongguo".to_string(), "中国".to_string(), 5000, 0);
    raw.merge_single("renmin".to_string(), "人民".to_string(), 5000, 0);
    let eng =
        PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(FuzzyConfig {
            zh_z: true,
            ..Default::default()
        });
    let s = sentence_of(&eng, "zongguorenmin").expect("应产出整句");
    assert_eq!(s.text, "中国人民");
    assert_eq!(
        s.meta.learn_code,
        Some(code_and_mask(&["zhong", "guo", "ren", "min"]))
    );
    let s = sentence_of(&eng, "zhongguorenmin").expect("精确打法应产出整句");
    assert_eq!(s.meta.learn_code, None, "全精确不带造词码");
}

/// 关模糊音 / 关 `sentence_uses_user_words`：整句都不认翘舌打法下的「菜就」。
#[test]
fn fuzzy_sentence_user_word_needs_both_switches() {
    for (tag, fz, on) in [
        ("fz_s2_fzoff", FuzzyConfig::default(), true),
        ("fz_s2_s2off", ch_c(), false),
    ] {
        let eng = sentence_engine_with_user_words_fuzzy(tag, CAIJIU_DUOLIAN, fz, on);
        let s = sentence_of(&eng, "chaijiuduolian");
        assert!(
            s.as_ref().is_none_or(|c| !c.text.contains("菜就")),
            "[{tag}] 整句不应出现「菜就」，实际: {:?}",
            s.map(|c| c.text)
        );
    }
}

/// 构造「带 qing 同音字洪泛的系统词典 + 用户长词」的引擎（复用于长词上浮系列测试）。
///
/// - `boundary` 让同一夹具能造出「带音节真值」与「手输码(=0)」两种用户词，两者的上浮判据必须
///   完全一致（`user_long_word_promotion_ignores_missing_boundary` 的对称性断言）。
/// - `max_extra` 是唯一的门槛来源。
fn engine_with_qing_flood_and_user_word(
    store_name: &str,
    max_extra: u32,
    boundary: u64,
) -> PinyinEngine {
    let mut raw = CodetableDict::empty();
    for (i, ch) in ["清", "青", "情", "请", "轻", "晴", "倾", "氢", "卿", "顷"]
        .iter()
        .enumerate()
    {
        raw.merge_single(
            "qing".to_string(),
            ch.to_string(),
            1000 - i as i32,
            i as i32,
        );
    }
    raw.merge_single("feng".to_string(), "风".to_string(), 900, 0);
    raw.merge_single("qingfeng".to_string(), "清风".to_string(), 800, 0);

    let store = tmp_store(store_name);
    store
        .add_user_word("pinyin", "qingfengshurufa", "清风输入法", 5000, boundary)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(store, "pinyin")));
    // min_syllables 设回 2：本组用例用 `qingfengshu`(3 音节) 验用户长词上浮，
    // 出厂的 4 会挡掉样本。
    let cfg = Config {
        completion_max_extra_syllables: max_extra,
        ..relaxed_completion_config()
    };
    PinyinEngine::new(cfg, CachedDict::Memory(raw)).with_store_layers(Arc::new(dm))
}

/// 「清风输入法」qing|feng|shu|ru|fa 的音节起点位图（上面夹具那条用户词的真值）。
fn qingfengshurufa_boundary() -> u64 {
    let mut b: u64 = 0;
    let mut code = String::new();
    for syl in ["qing", "feng", "shu", "ru", "fa"] {
        b |= 1u64 << code.len();
        code.push_str(syl);
    }
    assert_eq!(code, "qingfengshurufa", "夹具自检：音节拼接须还原出该码");
    b
}

/// 【核心回归】用户长词「清风输入法」在打到第 3-4 音节时应上浮到同音子短语之上，
/// 而非被压到候选最底（本次修复的用户反馈现场：打到完整全拼才出现）。
#[test]
fn user_long_word_surfaces_at_partial_pinyin() {
    let engine = engine_with_qing_flood_and_user_word("long_word_surface", 3, 0);

    for input in ["qingfengshu", "qingfengshuruf"] {
        let r = engine.convert(input, 300).unwrap();
        let pos_word = r
            .candidates
            .iter()
            .position(|c| c.text == "清风输入法")
            .unwrap_or_else(|| panic!("{input}: 清风输入法 应在候选中"));
        let pos_qing = r
            .candidates
            .iter()
            .position(|c| c.text == "清")
            .expect("清 子短语应存在");
        assert!(
            pos_word < pos_qing,
            "{input}: 用户长词应上浮到同音子短语「清」之上，实际 word@{pos_word} qing@{pos_qing}: {:?}",
            r.candidates
                .iter()
                .take(5)
                .map(|c| &c.text)
                .collect::<Vec<_>>()
        );
        assert!(
            r.candidates[pos_word].is_promoted_completion,
            "{input}: 上浮的用户长词应标 is_promoted_completion"
        );
        // is_prefix 结构真值保持不变（码确实更长）。
        assert!(
            r.candidates[pos_word].is_prefix,
            "{input}: is_prefix 结构事实（码更长）不应被抹掉"
        );
    }

    // 完整全拼：精确命中，本就在首位（is_prefix=false，非提升）。
    let r_full = engine.convert("qingfengshurufa", 300).unwrap();
    assert_eq!(
        r_full.candidates[0].text, "清风输入法",
        "完整全拼应精确命中首位"
    );
    assert!(
        !r_full.candidates[0].is_prefix,
        "完整全拼是精确匹配，非补全"
    );
    assert!(
        !r_full.candidates[0].is_promoted_completion,
        "精确命中不经上浮通道"
    );
}

/// 边界守卫：短输入下上浮与否不看词条填没填 boundary，且上浮不等于夺位（精确词仍在前）。
///
/// `max_extra = 3`、用户词 5 音节：
/// - `qing`(started 1)：召回门 `cap = 1 + 3 = 4 < 5` ⇒ 词根本进不来；
/// - `qingfeng`(started 2)：`cap = 5` ⇒ 在场，剩 3 ≤ 3 ⇒ 上浮，但被 `promotion_cap` 封在精确词之后。
///
/// 别承诺「音节太少不上浮」：`remaining <= max_extra` 与召回门的 `word_syls <= started + max_extra`
/// 是同一个不等式，这件事在主路径上不可观测（见 [`should_promote_user_completion`]）；为保住它
/// 收紧 `max_extra` 只会让用户词被召回门删掉、上浮断言全部空过。
#[test]
fn user_long_word_promotion_ignores_missing_boundary() {
    for (tag, boundary) in [
        ("no_boundary", 0u64),
        ("with_boundary", qingfengshurufa_boundary()),
    ] {
        let engine =
            engine_with_qing_flood_and_user_word(&format!("long_word_guard_{tag}"), 3, boundary);

        // qing：started 1 ⇒ cap = 4 < 5 音节 ⇒ 召回门先一步挡掉。
        // 这一档验不了判据的 `started >= 2`（候选根本进不来），只能钉住「确实没进来」
        // —— 写成硬断言而非 `if let`，是为了让它哪天进得来时当场报红、而非静默空过。
        let r1 = engine.convert("qing", 300).unwrap();
        assert!(
            !r1.candidates.iter().any(|c| c.text == "清风输入法"),
            "[{tag}] qing(started 1，cap=4)下 5 音节用户词应被召回门挡在外面"
        );

        // qingfeng：started 2 ⇒ cap = 5 ⇒ 在场。本用例的要害在这一档。
        let r2 = engine.convert("qingfeng", 300).unwrap();
        let pw = r2
            .candidates
            .iter()
            .position(|c| c.text == "清风输入法")
            .unwrap_or_else(|| panic!("[{tag}] 前提不成立：qingfeng(cap=5)下用户长词应在候选里"));
        assert!(
            r2.candidates[pw].is_promoted_completion,
            "[{tag}] started 2、剩 3 ≤ max_extra 3 ⇒ 应上浮（与填没填 boundary 无关）"
        );

        let pqf = r2
            .candidates
            .iter()
            .position(|c| c.text == "清风")
            .unwrap_or_else(|| panic!("[{tag}] 前提不成立：精确词「清风」没进候选"));
        assert!(
            pqf < pw,
            "[{tag}] 上浮不等于夺位：`promotion_cap` 应把用户长词封在精确「清风」之后，\
                 实际 清风@{pqf} 长词@{pw}"
        );
    }
}

/// 上浮判据单测：距词尾 ≤ `max_extra`（音节数已知）/ 已打 ≥3 音节（算不出）才上浮。
/// 入参是算好的音节数，`boundary != 0` 的直接传 `count_ones()`；`boundary == 0` 的要先过
/// `effective_boundary`，见组内注释。
#[test]
fn promote_user_completion_thresholds() {
    let trie = SyllableTrie::new();
    // 5 音节词（boundary 五个音节起始位；此处只关心 count_ones()=5）。
    let b5: u64 = 0b11111; // 5 个置位（count_ones=5，模拟 5 音节词）
    assert_eq!(b5.count_ones(), 5);
    // 以 max_extra = 2 复核历史档位（本判据长期硬编码的那个值）。
    // 无残码：completed_syls 即 started。
    assert!(
        !should_promote_user_completion(2, false, b5.count_ones(), 2),
        "5 音节词打 2 音节剩 3 > 2，不上浮"
    );
    assert!(
        should_promote_user_completion(3, false, b5.count_ones(), 2),
        "5 音节词打 3 音节剩 2 = 2，上浮"
    );
    assert!(
        should_promote_user_completion(4, false, b5.count_ones(), 2),
        "5 音节词打 4 音节剩 1，上浮"
    );
    assert!(
        !should_promote_user_completion(1, false, b5.count_ones(), 2),
        "1 音节 < 2，无条件不上浮"
    );
    // 尾部残码算作已起头的一个音节：qingfengs = 2 完整音节 + 残码 → started 3 → 上浮。
    assert!(
        should_promote_user_completion(2, true, b5.count_ones(), 2),
        "2 完整音节 + 残码（started 3, 剩 2）应上浮"
    );
    assert!(
        !should_promote_user_completion(1, true, b5.count_ones(), 2),
        "1 完整音节 + 残码（started 2, 剩 3 > 2）不上浮"
    );
    // ===== boundary == 0 的两种处境，判据不同 =====

    // ① 码切得出音节（手输码用户词的常态）：DAG 现切即得音节数，与带边界的同码词走同一条路。
    //    端到端护栏在 `tests/pinyin_user_word_no_boundary_tier.rs`。
    let code5 = "qingfengshurufa"; // qing|feng|shu|ru|fa
    assert_eq!(
        effective_boundary(code5, 0, &trie).count_ones(),
        5,
        "夹具自检：该码必须能被 DAG 切成 5 个音节，否则下面测的是退化分支"
    );
    for started in 1..=5usize {
        let with_b = should_promote_user_completion(started, false, b5.count_ones(), 2);
        let no_b = should_promote_user_completion(
            started,
            false,
            effective_boundary(code5, 0, &trie).count_ones(),
            2,
        );
        assert_eq!(
            with_b, no_b,
            "started={started}：同码同音节数，带不带 boundary 字段结果必须一致"
        );
    }

    // ② 码切不出音节（空码 / 非法码）：确实算不出剩余，退化为 started >= 3，
    //    与 max_extra 无关。
    for max_extra in [0, 2, 10] {
        assert!(
            !should_promote_user_completion(2, false, 0, max_extra),
            "算不出音节数时 2 音节不上浮（max_extra={max_extra} 不参与）"
        );
        assert!(
            should_promote_user_completion(3, false, 0, max_extra),
            "算不出音节数时 3 音节上浮（max_extra={max_extra} 不参与）"
        );
        assert!(
            should_promote_user_completion(2, true, 0, max_extra),
            "算不出音节数时 2 音节 + 残码（started 3）上浮"
        );
    }
}

/// 距词尾上限跟着配置走：11 音节用户词、`max_extra_syllables = 10`，打 `qingfengshurufa`
/// （started 5，剩 6）必须上浮。
///
/// 不上浮不止「排在后面」：引擎 `sort_by` 紧跟 `truncate`，沉到最底在候选数超上限时等于被丢弃，
/// 所以这条断言守的是可见性，不是次序。
#[test]
fn promote_user_completion_follows_max_extra_config() {
    // 11 音节词（count_ones = 11）。
    let b11: u64 = (1 << 11) - 1;
    assert_eq!(b11.count_ones(), 11);

    // started = 5（qingfengshurufa），剩 6。
    assert!(
        !should_promote_user_completion(5, false, b11.count_ones(), 2),
        "max_extra=2：剩 6 > 2，不上浮（旧硬编码行为，报障现场）"
    );
    assert!(
        should_promote_user_completion(5, false, b11.count_ones(), 10),
        "max_extra=10：剩 6 ≤ 10，必须上浮 —— 用户把设置调宽就是这个意思"
    );
    assert!(
        should_promote_user_completion(5, false, b11.count_ones(), 6),
        "max_extra=6：剩 6 恰好等于上限，边界值取闭区间（与召回层同口径）"
    );
    assert!(
        !should_promote_user_completion(5, false, b11.count_ones(), 5),
        "max_extra=5：剩 6 > 5，不上浮"
    );

    // 与召回层同口径：召回上限是 word_syls ≤ started + max_extra，
    // 即 remaining ≤ max_extra。召回得进来的，就该上浮得起来。
    for started in 2..=11usize {
        let remaining = 11usize.saturating_sub(started);
        let max_extra = 10u32;
        let recalled = 11 <= started + max_extra as usize;
        let promoted = should_promote_user_completion(started, false, b11.count_ones(), max_extra);
        assert_eq!(
            promoted,
            recalled && started >= 2,
            "started={started} 剩 {remaining}：召回与上浮判据须同口径"
        );
    }
}

/// 补全折扣：每多一个未输入音节，权重减半（对齐 librime `kCompletionPenalty`
/// 与 fcitx5 `overLengthCost`，见 [`COMPLETION_WEIGHT_DISCOUNT`]）。
#[test]
fn completion_penalty_halves_per_extra_syllable() {
    // extra=0（候选音节数 == 已完成音节数，或 boundary=0 算不出）：原样不动。
    assert_eq!(completion_penalized(5328, 0), 5328);
    // 「你好」nih 下 extra=1。
    assert_eq!(completion_penalized(5328, 1), 2664);
    // 「你会发现」nih 下 extra=3：13330 × 0.125 = 1666.25 → 1666，
    // 于是落到「你好」2664 之后 —— 这正是本机制要达成的效果。
    assert_eq!(completion_penalized(13330, 3), 1666);
    // 饱和到 >=1：折扣不该把候选压成 0 而改变它与「无权重」候选的关系。
    assert_eq!(completion_penalized(4, 10), 1);
    // weight <= 1 短路，不因折扣变负/变 0。
    assert_eq!(completion_penalized(1, 5), 1);
    assert_eq!(completion_penalized(0, 5), 0);
}

/// 用户造词的简拼应可命中（现算，非离线索引）：用户新增「菜鸟驿站」（全拼
/// cainiaoyizhan），键入简拼 cnyz 应能查到；临时词同理。
#[test]
fn store_layer_words_match_abbreviation() {
    let store = tmp_store("layer_abbrev");
    store
        .add_user_word("pinyin", "cainiaoyizhan", "菜鸟驿站", 500, 0)
        .unwrap();
    store
        .learn_temp_word("pinyin", "lanshoubing", "蓝瘦蘑菇", 800, 0)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
        store.clone(),
        "pinyin",
    )));
    let engine = empty_engine().with_store_layers(Arc::new(dm));

    let r = engine.convert("cnyz", 20).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "菜鸟驿站"),
        "简拼 cnyz 应命中用户造词「菜鸟驿站」"
    );

    let r2 = engine.convert("lsb", 20).unwrap();
    assert!(
        r2.candidates.iter().any(|c| c.text == "蓝瘦蘑菇"),
        "简拼 lsb 应命中临时造词「蓝瘦蘑菇」"
    );

    // 全拼整串输入仍应正常命中（无回归）
    let r3 = engine.convert("cainiaoyizhan", 20).unwrap();
    assert!(r3.candidates.iter().any(|c| c.text == "菜鸟驿站"));
}

/// `enable_abbrev=false`（混输经 schema.mix.enable_pinyin_abbrev 注入）时不产简拼候选，
/// 但全拼一切照旧。与上一个用例同构，只翻转开关——用于锁住「关掉的是简拼、不是拼音」。
#[test]
fn abbrev_disabled_suppresses_abbrev_candidates_only() {
    let store = tmp_store("layer_abbrev_off");
    store
        .add_user_word("pinyin", "cainiaoyizhan", "菜鸟驿站", 500, 0)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    let engine = PinyinEngine::new(
        Config {
            enable_abbrev: false,
            ..Default::default()
        },
        CachedDict::Memory(CodetableDict::empty()),
    )
    .with_store_layers(Arc::new(dm));

    let r = engine.convert("cnyz", 20).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.text == "菜鸟驿站"),
        "关闭简拼后 cnyz 不应命中「菜鸟驿站」"
    );

    // 全拼不受影响——这一条是关键：开关关的是简拼，不是拼音本身。
    let r2 = engine.convert("cainiaoyizhan", 20).unwrap();
    assert!(
        r2.candidates.iter().any(|c| c.text == "菜鸟驿站"),
        "关闭简拼不得影响全拼命中"
    );
}

/// C1：query→原始输入空间的 consumed 回映射。无 `'` 恒等；边界紧跟 `'` 归入已消费侧；
/// 连续 `''` 一并吸收；越过分隔符时正确计数；nih'ao 段内残码边界不 panic。
/// Task 1.4 TDD：with_fuzzy builder 注入的配置应被引擎持有（探针验证）。
#[test]
fn engine_applies_fuzzy_config() {
    let dict = CachedDict::Memory(CodetableDict::empty());
    let fz = FuzzyConfig {
        zh_z: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), dict).with_fuzzy(fz);
    assert!(eng.fuzzy_zh_z(), "with_fuzzy 注入的 zh_z=true 应被引擎持有");
}

/// 同键韵母（`q = ["iu", "ui"]`）：`dq` 两个读音的字都出、都不罚不标模糊，
/// 用户一个模糊组都没开也生效；多音节里对端读音同样召回。全拼方案不产生这些对。
#[test]
fn shuangpin_shared_key_finals_recall_both_readings() {
    let layout_toml = r#"
[meta]
id = "shared"
name = "同键测试"
[finals]
q = ["iu", "ui"]
m = ["ian"]
i = ["i"]
"#;
    let dict = || {
        let mut raw = CodetableDict::empty();
        raw.merge_single("ni".to_string(), "你".to_string(), 300, 3);
        raw.merge_single("diu".to_string(), "丢".to_string(), 100, 0);
        raw.merge_single("dui".to_string(), "对".to_string(), 300, 1);
        raw.merge_single("duilian".to_string(), "对联".to_string(), 200, 2);
        CachedDict::Memory(raw)
    };
    let conv = ShuangpinConverter::new(Layout::from_toml_str(layout_toml).unwrap());
    // 先挂双拼再挂模糊音（与 manager 相反的顺序），同键音节不得被冲掉。
    let eng = PinyinEngine::new(Config::default(), dict())
        .with_shuangpin(conv)
        .with_fuzzy(FuzzyConfig::default());
    let r = eng.convert("dq", 10).unwrap();
    let find = |t: &str| r.candidates.iter().find(|c| c.text == t);
    let dui = find("对").expect("dq 应出 dui 的字");
    assert_eq!(dui.weight, 300, "同键对端零罚");
    assert!(!dui.is_fuzzy, "同键对端不标模糊");
    assert!(find("丢").is_some(), "首个读音照出");
    assert_eq!(r.candidates[0].text, "对", "两种读音按词频平等竞争");

    let r = eng.convert("dqlm", 10).unwrap();
    let dl = r.candidates.iter().find(|c| c.text == "对联");
    assert!(
        dl.is_some_and(|c| !c.is_fuzzy && c.consumed_length == 4),
        "多音节里对端读音也要召回: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    // 整句词图的模糊节点路径（`any_enabled` 把关）：没有整词，只能由「对」+「你」组句。
    let r = eng.convert("dqni", 10).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "对你"),
        "整句须用上对端读音: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );

    let full = PinyinEngine::new(Config::default(), dict());
    assert!(full.fuzzy_config.shared_key.is_empty(), "全拼不产生同键对");
    let r = full.convert("diu", 10).unwrap();
    assert!(!r.candidates.iter().any(|c| c.text == "对"));
}

/// Task 4.1 TDD Step 2：多音节双拼——consumed_length 必须回算为双拼键数，
/// compute_composition 不能对已是全拼的串再做一次双拼转换。
/// 输入小鹤双拼 "nihc"（ni+hc → 全拼 "nihao"），词典含「你好」。
#[test]
fn pinyin_engine_shuangpin_multisyllable_consumed_length() {
    // 构造含 "nihao"->"你好" 的最小词典
    let mut raw = CodetableDict::empty();
    raw.merge_single("nihao".to_string(), "你好".to_string(), 200, 0);
    raw.merge_single("ni".to_string(), "你".to_string(), 100, 1);
    let dict = CachedDict::Memory(raw);

    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let conv = ShuangpinConverter::new(layout);

    let eng = PinyinEngine::new(Config::default(), dict).with_shuangpin(conv);
    // 小鹤双拼 "nihc" → 全拼 "nihao"
    let r = eng.convert("nihc", 10).unwrap();

    // 1. 候选含「你好」
    assert!(
        r.candidates.iter().any(|c| c.text == "你好"),
        "双拼输入 \"nihc\" 应包含候选「你好」，实际候选: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );

    // 2. 「你好」的 consumed_length 必须是双拼键数 4，而非全拼字节数 5
    let nihao = r.candidates.iter().find(|c| c.text == "你好").unwrap();
    assert_eq!(
        nihao.consumed_length, 4,
        "「你好」consumed_length 应为双拼键数 4（\"nihc\" 的长度），实际为 {}",
        nihao.consumed_length
    );
}

/// `require_full_match` 在双拼下按击键数判「吃满整串」：`consumed_length` 已回映射到
/// 击键域（`nihc` 的「你好」= 4），基准若取转换后的全拼（`nihao` = 5）会把整串候选
/// 全部当成半截丢光。直接辅助码对前缀单独解码时依赖这一条（`docs/design/aux-code-direct.md`）。
#[test]
fn require_full_match_uses_keystroke_length_under_shuangpin() {
    let eng = sp_fp_engine(
        "rfm",
        &[
            ("你好", "ni hao", 1000),
            ("你", "ni", 900),
            ("拟", "ni", 800),
        ],
        false,
    );
    let opts = ConvertOptions {
        require_full_match: true,
        ..Default::default()
    };
    let r = eng.convert_with_opts("nihc", 50, opts).unwrap();
    let texts: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
    assert!(texts.contains(&"你好"), "吃满整串的词必须留下：{texts:?}");
    assert!(!texts.contains(&"你"), "只吃 2 键的单字应被丢弃：{texts:?}");
}

// ===== 全拼降级支路（双拼方案下允许全拼输入，`allow_full_pinyin`）=====

/// 构造「小鹤双拼 + 可控开关」的引擎。`entries` 为 `(文本, 空格分隔的音节码, 权重)`。
///
/// 必须走 rime 源格式，不可用 `merge_single`：后者造出的条目 `boundary` 恒 0，而双拼边界校验
/// 遇 0 一律放行，等于把本组用例要验的那道校验整个关掉。
/// `tag` 用于隔离临时文件名——多个用例并发跑，共用一个文件名会互相截断。
fn sp_fp_engine(tag: &str, entries: &[(&str, &str, i32)], allow: bool) -> PinyinEngine {
    sp_fp_engine_cfg(
        tag,
        entries,
        Config {
            allow_full_pinyin: allow,
            ..Config::default()
        },
    )
}

/// [`sp_fp_engine`]，给定整份引擎配置。
fn sp_fp_engine_cfg(tag: &str, entries: &[(&str, &str, i32)], cfg: Config) -> PinyinEngine {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("wind_fp_fallback_{tag}.dict.yaml"));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        for (text, code, w) in entries {
            writeln!(f, "{text}\t{code}\t{w}").unwrap();
        }
    }
    let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
    let _ = std::fs::remove_file(&path);

    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    PinyinEngine::new(cfg, dict).with_shuangpin(ShuangpinConverter::new(layout))
}

/// 击键图开关默认关：默认选项下结果与未引入开关时一致。
#[test]
fn sp_raw_lattice_off_by_default() {
    assert!(!ConvertOptions::default().sp_raw_lattice);
    let eng = sp_fp_engine("raw_lattice_off", &[("你好", "ni hao", 1000)], false);
    let r = eng.convert("nihc", 5).unwrap();
    let first = r.candidates.first().expect("应有候选");
    assert_eq!(first.text, "你好");
    assert_eq!(first.consumed_length, 4);
    let r2 = eng
        .convert_with_opts("nihc", 5, ConvertOptions::default())
        .unwrap();
    assert_eq!(texts(&r), texts(&r2));
}

fn texts(r: &ConvertResult) -> Vec<&str> {
    r.candidates.iter().map(|c| c.text.as_str()).collect()
}

/// 击键图整句终点没铺到末音节（`md` = mai 无词）时不出整句，同旧路径；否则整句的码、
/// 边界按全部音节写，消费数却只到 `nihc`，三者对不上。
#[test]
fn sp_raw_lattice_no_sentence_when_last_syllable_unreached() {
    let eng = sp_fp_engine(
        "raw_lattice_unreached",
        &[("你", "ni", 900), ("好", "hao", 800)],
        false,
    );
    let sentences = |raw: bool| -> Vec<(String, String, usize)> {
        let opts = ConvertOptions {
            sp_raw_lattice: raw,
            ..Default::default()
        };
        let r = eng.convert_with_opts("nihcmd", 20, opts).unwrap();
        r.candidates
            .iter()
            .filter(|c| c.is_sentence)
            .map(|c| (c.text.clone(), c.code.clone(), c.consumed_length))
            .collect()
    };
    assert!(sentences(false).is_empty(), "前提：旧路径不出整句");
    let on = sentences(true);
    assert!(on.is_empty(), "击键图同样不出整句：{on:?}");
}

/// 击键图开关开时的整句：返回 `(文本, 消费数, learn_code)`。
#[allow(clippy::type_complexity)]
fn raw_sentences(eng: &PinyinEngine, input: &str) -> Vec<(String, usize, Option<(String, u64)>)> {
    let opts = ConvertOptions {
        sp_raw_lattice: true,
        ..Default::default()
    };
    let r = eng.convert_with_opts(input, 20, opts).unwrap();
    r.candidates
        .iter()
        .filter(|c| c.is_sentence)
        .map(|c| (c.text.clone(), c.consumed_length, c.meta.learn_code.clone()))
        .collect()
}

fn raw_small_engine(tag: &str) -> PinyinEngine {
    sp_fp_engine(
        tag,
        &[("你", "ni", 900), ("好", "hao", 800), ("吗", "ma", 700)],
        false,
    )
}

/// 末尾 `'` 计入整句消费数（`nihc'` = 5）。
#[test]
fn sp_raw_lattice_trailing_apostrophe_consumed() {
    let eng = raw_small_engine("raw_wire_trail_sep");
    let s = raw_sentences(&eng, "nihc'");
    let top = s.iter().find(|x| x.0 == "你好").expect("应出整句：{s:?}");
    assert_eq!(top.1, 5, "{s:?}");
}

/// 音节中间的 `'` 同样计入（`ni'hc` = 5）。
#[test]
fn sp_raw_lattice_inner_apostrophe_consumed() {
    let eng = raw_small_engine("raw_wire_inner_sep");
    let s = raw_sentences(&eng, "ni'hc");
    let top = s.iter().find(|x| x.0 == "你好").expect("应出整句：{s:?}");
    assert_eq!(top.1, 5, "{s:?}");
}

/// 2c 残码整句：`nihcm` 的 `m` 作待定音节由词库补成「吗」，整句消费 5 键，
/// 全精确故 `learn_code` 为空。
#[test]
fn sp_raw_lattice_residual_code_sentence() {
    let eng = raw_small_engine("raw_wire_residual");
    let s = raw_sentences(&eng, "nihcm");
    let top = s.iter().find(|x| x.0 == "你好吗").expect("应出整句：{s:?}");
    assert_eq!(top.1, 5, "{s:?}");
    assert_eq!(top.2, None, "{s:?}");
}

/// 开模糊音时整句的 `learn_code` 是规范码（词库读音），不是击键读出的模糊音。
#[test]
fn sp_raw_lattice_fuzzy_learn_code_is_canonical() {
    let eng = sp_fp_engine(
        "raw_wire_fuzzy",
        &[("里", "li", 900), ("好", "hao", 800)],
        false,
    )
    .with_fuzzy(FuzzyConfig {
        n_l: true,
        ..Default::default()
    });
    let s = raw_sentences(&eng, "nihc");
    let top = s.iter().find(|x| x.0 == "里好").expect("应出整句：{s:?}");
    assert_eq!(top.1, 4, "{s:?}");
    assert_eq!(top.2.as_ref().map(|l| l.0.as_str()), Some("lihao"), "{s:?}");
}

struct FakeAux(Vec<(char, &'static str)>);

impl crate::engine::AuxCharLookup for FakeAux {
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        self.0
            .iter()
            .any(|(c, code)| *c == ch && code.starts_with(prefix))
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

type AuxSentence = (String, usize, Option<(String, u64)>);

/// 击键图 + 辅码表：`(文本, 消费数, learn_code)` 的整句。
fn aux_sentences(eng: &PinyinEngine, input: &str, aux: Option<FakeAux>) -> Vec<AuxSentence> {
    let opts = ConvertOptions {
        sp_raw_lattice: true,
        aux_continuous: aux.map(|a| Arc::new(a) as Arc<dyn crate::engine::AuxCharLookup>),
        ..Default::default()
    };
    let r = eng.convert_with_opts(input, 20, opts).unwrap();
    r.candidates
        .iter()
        .filter(|c| c.is_sentence)
        .map(|c| (c.text.clone(), c.consumed_length, c.meta.learn_code.clone()))
        .collect()
}

/// 句中辅码：`uiuunihc` 的 `uu` 是「是」的辅码，整句消费全部 8 键，造词码不含辅码。
#[test]
fn sp_raw_lattice_aux_sentence_consumes_aux_keys() {
    let eng = sp_fp_engine(
        "raw_wire_aux",
        &[("是", "shi", 900), ("你好", "ni hao", 1000)],
        false,
    );
    let s = aux_sentences(&eng, "uiuunihc", Some(FakeAux(vec![('是', "uu")])));
    let top = s
        .iter()
        .find(|x| x.0 == "是你好")
        .unwrap_or_else(|| panic!("应出整句：{s:?}"));
    assert_eq!(top.1, 8, "{s:?}");
    // 码按路径去掉辅码，造词码与它相同故为 None。
    assert_eq!(top.2, None, "{s:?}");
    let r = aux_run(&eng, "uiuunihc", vec![('是', "uu")], None);
    let c = cand_of(&r, "是你好", 8);
    assert_eq!((c.code.as_str(), c.boundary), ("shinihao", 0b101001));
    // 无辅码表 / 空表：`uu` 只能读 shu，词库没有，不出这条整句。
    for aux in [None, Some(FakeAux(vec![]))] {
        let s = aux_sentences(&eng, "uiuunihc", aux);
        assert!(s.iter().all(|x| x.0 != "是你好"), "{s:?}");
    }
}

/// 击键图首选 `(文本, 消费数, 造词码)`；造词码缺省时取候选码。
fn aux_top(eng: &PinyinEngine, input: &str, aux: Option<FakeAux>) -> (String, usize, String) {
    let opts = ConvertOptions {
        sp_raw_lattice: true,
        aux_continuous: aux.map(|a| Arc::new(a) as Arc<dyn crate::engine::AuxCharLookup>),
        ..Default::default()
    };
    let r = eng.convert_with_opts(input, 20, opts).unwrap();
    let c = r.candidates.first().expect("应有候选");
    let learn = c
        .meta
        .learn_code
        .as_ref()
        .map_or(c.code.clone(), |(code, _)| code.clone());
    (c.text.clone(), c.consumed_length, learn)
}

const NIHAO_AUX: &[(&str, &str, i32)] = &[
    ("你", "ni", 9000),
    ("好", "hao", 9000),
    ("你好", "ni hao", 50000),
    ("和", "he", 8000),
];

/// 辅码字母不参与的正常输入：辅码开 / 关首选相同。
#[test]
fn aux_penalty_keeps_plain_input_unchanged() {
    let eng = sp_fp_engine("aux_plain", NIHAO_AUX, false);
    let table = || Some(FakeAux(vec![('你', "hc"), ('好', "hn")]));
    let on = aux_top(&eng, "nihc", table());
    let off = aux_top(&eng, "nihc", None);
    assert_eq!(on, off);
    assert_eq!((on.0.as_str(), on.1), ("你好", 4));
    // 整句也不变：「你 + hc」（2 位辅码）扣 4.0 后不敌「你好」。
    assert_eq!(
        aux_sentences(&eng, "nihc", table()),
        aux_sentences(&eng, "nihc", None)
    );
}

/// 同文合并：整句用了辅码且消费整串时，合并后的候选取整句的消费数与造词码，同文只剩一条。
#[test]
fn aux_sentence_merge_takes_sentence_consumed() {
    let eng = sp_fp_engine("aux_merge", NIHAO_AUX, false);
    let opts = ConvertOptions {
        sp_raw_lattice: true,
        aux_continuous: Some(Arc::new(FakeAux(vec![('好', "hn")]))),
        ..Default::default()
    };
    let r = eng.convert_with_opts("nihchn", 20, opts).unwrap();
    let same: Vec<_> = r.candidates.iter().filter(|c| c.text == "你好").collect();
    assert_eq!(same.len(), 1, "{same:?}");
    let c = same[0];
    assert!(c.is_sentence);
    assert_eq!(c.consumed_length, 6);
    let learn = c.meta.learn_code.as_ref().map_or(c.code.as_str(), |l| &l.0);
    assert_eq!(learn, "nihao");
}

/// 同文合并：整句未用辅码时合并结果与不给辅码表时逐字段相同，消费数仍是 step 1 的。
/// `nihc'` 的整句消费整串 5 键，合并后照旧是 4（`'` 留缓冲）。
#[test]
fn plain_sentence_merge_keeps_consumed() {
    let eng = sp_fp_engine("aux_merge_plain", NIHAO_AUX, false);
    let run = |keys: &str, aux: Option<FakeAux>| {
        let opts = ConvertOptions {
            sp_raw_lattice: true,
            aux_continuous: aux.map(|a| Arc::new(a) as Arc<dyn crate::engine::AuxCharLookup>),
            ..Default::default()
        };
        eng.convert_with_opts(keys, 20, opts).unwrap()
    };
    let dump = |r: &ConvertResult| format!("{:?}\n{:?}", r.candidates, r.sentence_pool);
    for keys in ["nihc", "nihc'"] {
        let plain = run(keys, None);
        let table = run(keys, Some(FakeAux(vec![('好', "hn")])));
        assert_eq!(dump(&table), dump(&plain), "{keys}");
        let c = table.candidates.iter().find(|c| c.text == "你好").unwrap();
        assert!(c.is_sentence, "前提：整句与 step 1 同文合并（{keys}）");
        assert_eq!(c.consumed_length, 4, "{keys}");
    }
}

/// 变异：罚分置 0 时 `nihc` 的整句翻成「你 + 辅码 hc」（单字分高于「你好」），证明罚分在起作用。
/// 只看整句：引擎首选两种情况下都是词库精确的「你好」。
#[test]
fn aux_penalty_zero_flips_plain_input() {
    let eng = sp_fp_engine("aux_flip", NIHAO_AUX, false);
    let table = || Some(FakeAux(vec![('你', "hc"), ('好', "hn")]));
    let with = aux_sentences(&eng, "nihc", table());
    let old = raw_lattice::set_aux_letter_penalty(0.0);
    let without = aux_sentences(&eng, "nihc", table());
    raw_lattice::set_aux_letter_penalty(old);
    assert_eq!(with, vec![("你好".to_string(), 4, None)]);
    // 码按路径去掉辅码（`ni`），造词码与它相同故为 None。
    assert_eq!(
        without,
        vec![("你".to_string(), 4, None)],
        "λ=0 时辅码读法胜出"
    );
    let old = raw_lattice::set_aux_letter_penalty(0.0);
    let r = aux_run(&eng, "nihc", vec![('你', "hc"), ('好', "hn")], None);
    raw_lattice::set_aux_letter_penalty(old);
    let c = cand_of(&r, "你", 4);
    assert_eq!((c.code.as_str(), c.boundary), ("ni", 1));
    assert_eq!(c.meta.preedit_split.as_deref(), Some("ni hc"));
}

/// 辅码命中：`nihchn` 首选「你好」消费 6 键，造词码不含辅码字母。
#[test]
fn aux_applied_beats_plain_when_hit() {
    let eng = sp_fp_engine("aux_hit", NIHAO_AUX, false);
    let top = aux_top(&eng, "nihchn", Some(FakeAux(vec![('好', "hn")])));
    assert_eq!(top, ("你好".to_string(), 6, "nihao".to_string()));
}

const QIHOU_AUX: &[(&str, &str, i32)] = &[
    ("启", "qi", 9000),
    ("气", "qi", 9000),
    ("气候", "qi hou", 50000),
    ("和", "he", 8000),
    ("我", "wo", 9000),
];

/// 末键落单：「气候」（补全）压过「启 + 辅码 h」。
#[test]
fn tail_completion_beats_trailing_aux() {
    let eng = sp_fp_engine("aux_tail_qih", QIHOU_AUX, false);
    let table = || Some(FakeAux(vec![('启', "h")]));
    assert_eq!(aux_top(&eng, "qih", table()).0, "气候");
    // 两音节起整句：整串那条整句里补全节点压过辅码读法，没用辅码就不出，首选同不给表。
    let top = aux_top(&eng, "woqih", table());
    let off = aux_top(&eng, "woqih", None);
    assert_eq!((top.0.as_str(), top.1), (off.0.as_str(), off.1), "{top:?}");
    // 「我启 + 辅码 h」不出；末槽终点 4 的「我启」是 P1 那条整句，照出。
    let s = aux_sentences(&eng, "woqih", table());
    assert!(
        s.iter().all(|x| (x.0.as_str(), x.1) != ("我启", 5)),
        "{s:?}"
    );
}

/// `nikeyig` 型：「课 + 辅码 y」把键数凑成偶数拼出「程」（小鹤 `ig` = cheng），
/// 补全节点给出「你可以 + 一个」的对手。
#[test]
fn tail_completion_blocks_shifted_pair_reading() {
    let words: &[(&str, &str, i32)] = &[
        ("你", "ni", 9000),
        ("可以", "ke yi", 60000),
        ("你可以", "ni ke yi", 30000),
        ("课", "ke", 3000),
        ("程", "cheng", 3000),
        ("一个", "yi ge", 80000),
        ("个", "ge", 9000),
    ];
    let eng = sp_fp_engine("aux_tail_nikeyig", words, false);
    let table = || Some(FakeAux(vec![('课', "y")]));
    let off = aux_top(&eng, "nikeyig", None);
    let on = aux_top(&eng, "nikeyig", table());
    assert!(!on.0.contains('程'), "on={on:?} off={off:?}");
    let s = aux_sentences(&eng, "nikeyig", table());
    assert!(s.iter().all(|x| !x.0.contains('程')), "{s:?}");
}

/// 「漆雕」压到与对手「期待 + 残码 n」分差 2～3（真实词库约 1.8），闸门裕度要挡住这一档。
const QIDAI_AUX: &[(&str, &str, i32)] = &[
    ("期", "qi", 9000),
    ("漆", "qi", 6000),
    ("待", "dai", 9000),
    ("期待", "qi dai", 50000),
    ("漆雕", "qi diao", 3),
    ("雕", "diao", 3000),
    ("年", "nian", 9000),
];

/// `qiddn`（小鹤 dd = dai、dn = diao）：首选与不给辅码表时相同，「漆 + 辅码 d + 雕」不上首位。
/// 补全节点救不了：对手「期待 + 残码 n」起的是新字，分数不敌「漆雕」，靠比分闸门挡。
#[test]
fn tail_completion_qiddn_keeps_plain_top() {
    let eng = sp_fp_engine("aux_tail_qiddn", QIDAI_AUX, false);
    let off = aux_top(&eng, "qiddn", None);
    let on = aux_top(&eng, "qiddn", Some(FakeAux(vec![('漆', "d")])));
    assert_eq!(on.0, off.0, "on={on:?} off={off:?}");
}

/// 辅码模式下 step 2 仍解末槽终点的整句：`qiddn` 首选 `(期待, 4)` 同不给表，
/// 「漆 + 辅码 d + 雕」作整串整句另出。
#[test]
fn aux_tail_keeps_last_slot_sentence() {
    let eng = sp_fp_engine("aux_tail_last_slot", QIDAI_AUX, false);
    let table = || Some(FakeAux(vec![('漆', "d")]));
    let on = aux_top(&eng, "qiddn", table());
    let off = aux_top(&eng, "qiddn", None);
    assert_eq!((on.0.as_str(), on.1), ("期待", 4), "{on:?}");
    assert_eq!((on.0, on.1), (off.0, off.1));
    let s = aux_sentences(&eng, "qiddn", table());
    assert!(
        s.iter().any(|x| (x.0.as_str(), x.1) == ("期待", 4)),
        "{s:?}"
    );
    assert!(
        s.iter().any(|x| (x.0.as_str(), x.1) == ("漆雕", 5)),
        "{s:?}"
    );
}

const GATE_AUX: &[(&str, &str, i32)] = &[
    ("你", "ni", 9000),
    ("好", "hao", 9000),
    ("你好", "ni hao", 50000),
    ("行", "hang", 5000),
    ("藏", "cang", 3000),
    ("才", "cai", 3000),
];

/// 末槽整句闸门（出厂不设）：`wopdle` 的 `pd` 既是「拍」也是「我」的 2 位辅码。不设闸时
/// 「我了」居首；设闸且分差不够时对手「我拍了」居首（同不给表），「我了」仍在候选里、排在其后。
#[test]
fn aux_last_slot_gate_keeps_plain_top() {
    let words: &[(&str, &str, i32)] = &[("我", "wo", 9000), ("拍", "pai", 50), ("了", "le", 9000)];
    let eng = sp_fp_engine("aux_last_slot_gate", words, false);
    let table = || Some(FakeAux(vec![('我', "pd")]));
    let off = aux_top(&eng, "wopdle", None);
    assert_eq!((off.0.as_str(), off.1), ("我拍了", 6), "前提");
    assert_eq!(raw_lattice::AUX_SE_GATE_MARGIN, None);
    let on = aux_top(&eng, "wopdle", table());
    assert_eq!((on.0.as_str(), on.1), ("我了", 6), "前提：辅码读法分高");
    let old = raw_lattice::set_aux_se_gate_margin(Some(1e9));
    let gated = aux_top(&eng, "wopdle", table());
    let s = aux_sentences(&eng, "wopdle", table());
    raw_lattice::set_aux_se_gate_margin(old);
    assert_eq!((gated.0.as_str(), gated.1), ("我拍了", 6));
    let pos = |t: &str| s.iter().position(|x| x.0 == t && x.1 == 6);
    let (Some(plain), Some(aux)) = (pos("我拍了"), pos("我了")) else {
        panic!("两条整句都应在：{s:?}");
    };
    assert!(plain < aux, "{s:?}");
}

/// 闸门裕度分模型开 / 关两档：同一分差（约 7，对手是「你行藏」残码补全）模型关时放行，
/// 挂上语法模型（恒 0 分，不改分数）时按模型开那档不放行、首选同不给表。
#[test]
fn aux_gate_margin_follows_grammar() {
    let mut words = GATE_AUX.to_vec();
    words.push(("你行藏", "ni hang cang", 100));
    let table = || Some(FakeAux(vec![('你', "h")]));
    let eng = sp_fp_engine("aux_gate_gram_off", &words, false);
    raw_lattice::take_gate_gaps();
    let off = aux_top(&eng, "nihhc", table());
    let gap = raw_lattice::take_gate_gaps()
        .first()
        .and_then(raw_lattice::GateGap::gap)
        .expect("应比过分");
    assert!(
        gap > raw_lattice::AUX_GATE_MARGIN && gap < raw_lattice::AUX_GATE_MARGIN_GRAMMAR,
        "前提：分差 {gap}"
    );
    assert_eq!(off, ("你好".to_string(), 5, "nihao".to_string()));
    let eng = sp_fp_engine("aux_gate_gram_on", &words, false)
        .with_grammar(Arc::new(grammar::NullGrammar));
    let plain = aux_top(&eng, "nihhc", None);
    let on = aux_top(&eng, "nihhc", table());
    assert_eq!((on.0.as_str(), on.1), (plain.0.as_str(), plain.1), "{on:?}");
}

/// 比分闸门放行正当辅码：`nihhc`（你 + 辅码 h + 好）首选「你好」消费 5，压过末槽整句「你行」。
#[test]
fn aux_gate_passes_mid_aux() {
    let eng = sp_fp_engine("aux_gate_mid", GATE_AUX, false);
    let off = aux_top(&eng, "nihhc", None);
    assert_eq!((off.0.as_str(), off.1), ("你行", 4), "前提：{off:?}");
    let on = aux_top(&eng, "nihhc", Some(FakeAux(vec![('你', "h")])));
    assert_eq!(on, ("你好".to_string(), 5, "nihao".to_string()));
}

/// `nikeyig` 的真实形态：「你 + 课程（课后夹辅码 y）」与「你可以 + 残码」分差只有几分，
/// 闸门挡住，首选同不给表；裕度置 0 则「你课程」上首位。
#[test]
fn aux_gate_blocks_shifted_pair_reading() {
    let words: &[(&str, &str, i32)] = &[
        ("你", "ni", 9000),
        ("可以", "ke yi", 60000),
        ("你可以", "ni ke yi", 30000),
        ("课", "ke", 3000),
        ("程", "cheng", 3000),
        ("课程", "ke cheng", 3000),
        ("个", "ge", 9000),
    ];
    let eng = sp_fp_engine("aux_gate_nikeyig", words, false);
    let table = || Some(FakeAux(vec![('课', "y")]));
    let off = aux_top(&eng, "nikeyig", None);
    let on = aux_top(&eng, "nikeyig", table());
    assert_eq!((on.0.as_str(), on.1), (off.0.as_str(), off.1), "{on:?}");
    let old = raw_lattice::set_aux_gate_margin(Some(0.0));
    let loose = aux_top(&eng, "nikeyig", table());
    raw_lattice::set_aux_gate_margin(old);
    assert_eq!(
        (loose.0.as_str(), loose.1),
        ("你课程", 7),
        "前提：辅码读法本身分高"
    );
}

/// 辅码字母与下一键拼成非法键对（小鹤 `oh`、`oy`）：P1 的两键音节链在第 1 个音节后断掉，
/// 整句仍按击键图可达出「你好」，消费整串，造词码不含辅码。奇数长走 S_n，偶数长走末槽整句。
#[test]
fn aux_sentence_when_aux_breaks_pair_chain() {
    let eng = sp_fp_engine("aux_break_chain", NIHAO_AUX, false);
    let table = || Some(FakeAux(vec![('你', "oy")]));
    for (keys, n) in [("niohc", 5), ("nioyhc", 6)] {
        let on = aux_top(&eng, keys, table());
        assert_eq!(on, ("你好".to_string(), n, "nihao".to_string()), "{keys}");
        let s = aux_sentences(&eng, keys, table());
        let hit = s.iter().find(|x| x.0 == "你好").expect("应有整句");
        // 码按路径（`nihao`，见 `aux_candidate_consistent_chain_break`），造词码同码为 None。
        assert_eq!(hit.2, None, "{keys}: {s:?}");
        // 同文只出一条（整句并入精确词，不另插）。
        let opts = ConvertOptions {
            sp_raw_lattice: true,
            aux_continuous: table().map(|a| Arc::new(a) as Arc<dyn crate::engine::AuxCharLookup>),
            ..Default::default()
        };
        let r = eng.convert_with_opts(keys, 20, opts).unwrap();
        let same: Vec<_> = r.candidates.iter().filter(|c| c.text == "你好").collect();
        assert_eq!(same.len(), 1, "{keys}: {same:?}");
    }
}

/// 链断后只剩 1 个音节的读法（`nio`、`nioy`：你 + 辅码）不当整句出，同 P1「至少两个音节」。
#[test]
fn aux_chain_break_single_char_not_sentence() {
    let eng = sp_fp_engine("aux_break_single", NIHAO_AUX, false);
    for keys in ["nio", "nioy"] {
        let s = aux_sentences(&eng, keys, Some(FakeAux(vec![('你', "oy")])));
        assert!(s.is_empty(), "{keys}: {s:?}");
    }
}

/// 末键落单：`nihch` 的 `h` 在 P1 读法里是残码，当成「好」的 1 位辅码（末字 1 位辅码）。
/// 落单按 [`raw_lattice::AUX_DANGLING_MARGIN`] 比分：分差够就放行，界面首选「你好」消费 5；
/// 裕度抬到分差以上时同不给表（残码整句「你好好」）。
#[test]
fn aux_gate_dangling_key_passes_with_margin() {
    let eng = sp_fp_engine("aux_gate_dangling", NIHAO_AUX, false);
    let table = || Some(FakeAux(vec![('好', "h")]));
    raw_lattice::take_gate_gaps();
    let on = aux_ui(&eng, "nihch", table());
    let gaps = raw_lattice::take_gate_gaps();
    let gap = gaps
        .iter()
        .find(|g| g.kind == 'n' && g.dangling && g.sentence == "你好")
        .and_then(raw_lattice::GateGap::gap)
        .unwrap_or_else(|| panic!("前提：落单整句比过分：{gaps:?}"));
    assert!(gap > raw_lattice::AUX_DANGLING_MARGIN, "前提：分差 {gap}");
    assert_eq!(on[0], ("你好".to_string(), 5), "{on:?}");
    let off = aux_ui(&eng, "nihch", None);
    assert_ne!(off[0].0, "你好", "前提：{off:?}");
    let old = raw_lattice::set_aux_dangling_margin(Some(Some(gap + 0.1)));
    let blocked = aux_ui(&eng, "nihch", table());
    raw_lattice::set_aux_dangling_margin(old);
    assert_eq!(blocked[0], off[0], "{blocked:?}");
}

/// 落单的假读法：`nihch` 正往「你好和」打（`h` 是「和」的声母），「你好 + 辅码 h」与对手
/// 「你好和」分差约 7——过得了非落单的裕度，挡在落单裕度之下，界面首选同不给表。
#[test]
fn aux_gate_dangling_false_reading_blocked() {
    let mut words = NIHAO_AUX.to_vec();
    words.push(("你好和", "ni hao he", 100));
    let eng = sp_fp_engine("aux_gate_dangling_false", &words, false);
    let table = || Some(FakeAux(vec![('好', "h")]));
    raw_lattice::take_gate_gaps();
    let on = aux_ui(&eng, "nihch", table());
    let gaps = raw_lattice::take_gate_gaps();
    let gap = gaps
        .iter()
        .find(|g| g.kind == 'n' && g.dangling && g.sentence == "你好")
        .and_then(raw_lattice::GateGap::gap)
        .unwrap_or_else(|| panic!("前提：落单整句比过分：{gaps:?}"));
    assert!(
        gap > raw_lattice::AUX_GATE_MARGIN && gap <= raw_lattice::AUX_DANGLING_MARGIN - 4.0,
        "前提：分差 {gap}"
    );
    let off = aux_ui(&eng, "nihch", None);
    assert_eq!(on[0], off[0], "on={on:?} off={off:?}");
    assert_ne!(on[0], ("你好".to_string(), 5), "{on:?}");
}

/// 句尾 2 位辅码打在已对的字后：首选文字同不带辅码的输入，消费整串。
/// `nihchc` 末槽即整串（`hc` 也读 hao）；`nihhchn` 末槽止于 6、整串那条要过闸门。
#[test]
fn aux_gate_tail_two_letter_aux_on_correct_char() {
    let eng = sp_fp_engine("aux_gate_tail2", GATE_AUX, false);
    let table = || Some(FakeAux(vec![('你', "h"), ('好', "hc"), ('好', "hn")]));
    assert_eq!(aux_top(&eng, "nihc", table()).0, "你好");
    for (keys, n) in [("nihchc", 6), ("nihchn", 6), ("nihhchn", 7)] {
        let on = aux_top(&eng, keys, table());
        assert_eq!((on.0.as_str(), on.1), ("你好", n), "{keys}");
    }
}

/// 不放行的辅码整串整句即使自身分高，也排在末槽整句之后（裕度极大时恒不放行）。
#[test]
fn aux_gate_failed_sentence_stays_below_last_slot() {
    let words: &[(&str, &str, i32)] = &[
        ("你", "ni", 9000),
        ("可以", "ke yi", 60000),
        ("课", "ke", 3000),
        ("程", "cheng", 3000),
        ("课程", "ke cheng", 3000000),
        ("个", "ge", 9000),
    ];
    let eng = sp_fp_engine("aux_gate_cap", words, false);
    let table = || Some(FakeAux(vec![('课', "y")]));
    let pass = aux_top(&eng, "nikeyig", table());
    assert_eq!((pass.0.as_str(), pass.1), ("你课程", 7), "前提：放行时居首");
    let old = raw_lattice::set_aux_gate_margin(Some(1e9));
    let fail = aux_top(&eng, "nikeyig", table());
    let s = aux_sentences(&eng, "nikeyig", table());
    raw_lattice::set_aux_gate_margin(old);
    assert_eq!((fail.0.as_str(), fail.1), ("你可以", 6), "{s:?}");
    assert!(
        s.iter().any(|x| (x.0.as_str(), x.1) == ("你课程", 7)),
        "{s:?}"
    );
}

/// 同文普通词与整句并存：「你好」既是 `nihang` 的低频词条（普通候选），又是 `nihhc` 的辅码整串
/// 整句；末槽整句是另一个词「你行」。放行时并入那条普通词（取整串消费数）并居首；
/// 不放行时普通词原样（消费 4）、首位仍是末槽整句。
#[test]
fn aux_gate_same_text_plain_word_beside_sentence() {
    let mut words = GATE_AUX.to_vec();
    words.push(("你好", "ni hang", 100));
    words.push(("你行", "ni hang", 50000));
    let eng = sp_fp_engine("aux_gate_same_text", &words, false);
    let table = || Some(FakeAux(vec![('你', "h")]));
    let run = || {
        let opts = ConvertOptions {
            sp_raw_lattice: true,
            aux_continuous: table().map(|a| Arc::new(a) as Arc<dyn crate::engine::AuxCharLookup>),
            ..Default::default()
        };
        eng.convert_with_opts("nihhc", 20, opts).unwrap().candidates
    };
    let pass = run();
    assert_eq!(
        (pass[0].text.as_str(), pass[0].consumed_length),
        ("你好", 5)
    );
    assert_eq!(pass.iter().filter(|c| c.text == "你好").count(), 1);
    let old = raw_lattice::set_aux_gate_margin(Some(1e9));
    let fail = run();
    raw_lattice::set_aux_gate_margin(old);
    assert_eq!(
        (fail[0].text.as_str(), fail[0].consumed_length),
        ("你行", 4)
    );
    let plain: Vec<_> = fail.iter().filter(|c| c.text == "你好").collect();
    assert_eq!(plain.len(), 1, "{plain:?}");
    assert_eq!(plain[0].consumed_length, 4);
}

/// 末槽整句被屏蔽词滤光（没有排位基准）时，不放行的辅码整串整句不出，更不居首。
#[test]
fn aux_gate_failed_without_last_slot_sentence_not_emitted() {
    let words: &[(&str, &str, i32)] = &[
        ("期", "qi", 9000),
        ("待", "dai", 9000),
        ("期待", "qi dai", 50000),
        ("漆雕", "qi diao", 3_000_000),
        ("年", "nian", 9000),
    ];
    let eng = sp_fp_engine("aux_gate_no_top", words, false);
    eng.set_blocked_words(Arc::new(["期待".to_string()].into_iter().collect()));
    let table = || Some(FakeAux(vec![('漆', "d")]));
    let old = raw_lattice::set_aux_gate_margin(Some(1e9));
    let on = aux_top(&eng, "qiddn", table());
    let s = aux_sentences(&eng, "qiddn", table());
    raw_lattice::set_aux_gate_margin(old);
    let off = aux_top(&eng, "qiddn", None);
    assert_eq!((on.0.as_str(), on.1), (off.0.as_str(), off.1), "{s:?}");
    assert!(s.iter().all(|x| x.0 != "漆雕"), "{s:?}");
    // 放行时照常居首（证明上面挡住的是闸门，不是读法本身没出）。
    assert_eq!(aux_top(&eng, "qiddn", table()).0, "漆雕");
}

/// 句尾 2 位辅码、同终点有真实无辅码对手：`nihhchc` 末槽止于 6（你行藏），整串的无辅码路径是
/// 「你行藏 + 残码 c」，辅码读法「你（h）好（hc）」要比它高出裕度才居首。
#[test]
fn aux_gate_tail_two_letter_aux_with_plain_rival() {
    let eng = sp_fp_engine("aux_gate_tail2_rival", GATE_AUX, false);
    let table = || Some(FakeAux(vec![('你', "h"), ('好', "hc")]));
    let off = aux_top(&eng, "nihhchc", None);
    assert_eq!((off.0.as_str(), off.1), ("你行藏", 6), "前提：{off:?}");
    let s = aux_sentences(&eng, "nihhchc", table());
    assert!(
        s.iter().any(|x| x.1 == 7 && x.0 != "你好"),
        "前提：同终点有无辅码对手：{s:?}"
    );
    let on = aux_top(&eng, "nihhchc", table());
    assert_eq!((on.0.as_str(), on.1), ("你好", 7), "{s:?}");
}

/// 手动分隔：`'` 计入末槽 / 落单判据。
/// - `ni'hch`、`nihch'`：末槽之后只剩一个字母（可带 `'`），当末字 1 位辅码即落单，按落单裕度
///   比分（分差同 `nihch`，`nihch'` 无对手），放行、消费整串；一律不放行时首选同不给表；
/// - `nihhc'`：辅码在句中、末尾 `'` 并入末槽，放行，消费整串 6；
/// - `ni'hhc`：`'` 后的 `h` 不接在音节后，不是辅码，首选同不给表。
#[test]
fn aux_gate_with_manual_separator() {
    let eng = sp_fp_engine("aux_gate_sep", GATE_AUX, false);
    for keys in ["ni'hch", "nihch'"] {
        let table = || Some(FakeAux(vec![('好', "h")]));
        raw_lattice::take_gate_gaps();
        let on = aux_top(&eng, keys, table());
        assert!(
            raw_lattice::take_gate_gaps().iter().all(|g| g.dangling),
            "{keys}：应按落单比分"
        );
        assert_eq!((on.0.as_str(), on.1), ("你好", 6), "{keys}");
        let old = raw_lattice::set_aux_dangling_margin(Some(None));
        let strict = aux_top(&eng, keys, table());
        raw_lattice::set_aux_dangling_margin(old);
        let off = aux_top(&eng, keys, None);
        assert_eq!(
            (strict.0.as_str(), strict.1),
            (off.0.as_str(), off.1),
            "{keys}"
        );
    }
    let off = aux_top(&eng, "ni'hhc", None);
    let on = aux_top(&eng, "ni'hhc", Some(FakeAux(vec![('你', "h")])));
    assert_eq!((on.0.as_str(), on.1), (off.0.as_str(), off.1));
    let on = aux_top(&eng, "nihhc'", Some(FakeAux(vec![('你', "h")])));
    assert_eq!((on.0.as_str(), on.1), ("你好", 6));
}

/// 界面序 `(文字, 消费数)` 列表：协调器的排序键（`candidate_display_order`，消费数为首键），
/// 再按文字去重。引擎序首选不等于界面首选，闸门要在这一层验。
fn aux_ui(eng: &PinyinEngine, input: &str, aux: Option<FakeAux>) -> Vec<(String, usize)> {
    let opts = ConvertOptions {
        sp_raw_lattice: true,
        aux_continuous: aux.map(|a| Arc::new(a) as Arc<dyn crate::engine::AuxCharLookup>),
        ..Default::default()
    };
    let mut c = eng.convert_with_opts(input, 50, opts).unwrap().candidates;
    wind_candidate::drop_corrected_duplicates(&mut c);
    c.sort_by(|a, b| wind_candidate::candidate_display_order(a, b, false, false, input));
    wind_candidate::protect_top_from_corrected(&mut c, input.len());
    let mut seen = std::collections::HashSet::new();
    c.retain(|x| seen.insert(x.text.clone()));
    c.into_iter().map(|c| (c.text, c.consumed_length)).collect()
}

/// 不过闸的整串整句在界面序下不抢首选：`qiddn`（漆 + 辅码 d + 雕，与「期待 + 残码 n」
/// 等价歧义）、`nikeyig`（你 + 课程，课后夹辅码 y）。界面首选与不给辅码表时相同。
/// 引擎序里 6.5d 把它压到末槽整句（消费 e < n）之下；界面先按消费数分档，它与残码整句同在
/// 消费 n 那一档里比权重。
#[test]
fn ui_gate_blocks_failed_sn() {
    let nikeyig: &[(&str, &str, i32)] = &[
        ("你", "ni", 9000),
        ("可以", "ke yi", 60000),
        ("你可以", "ni ke yi", 30000),
        ("课", "ke", 3000),
        ("程", "cheng", 3000),
        ("课程", "ke cheng", 3000),
        ("个", "ge", 9000),
    ];
    // 「漆雕」比 `QIDAI_AUX` 常用些，与对手「期待 + 残码 n」的分差仍在裕度内（约 4.5）。
    let mut qidai = QIDAI_AUX.to_vec();
    qidai.retain(|w| w.0 != "漆雕");
    qidai.push(("漆雕", "qi diao", 15));
    // (击键, 词库, 辅码字, 辅码, 不过闸的辅码整句)
    type Case<'a> = (
        &'a str,
        &'a [(&'a str, &'a str, i32)],
        char,
        &'static str,
        &'a str,
    );
    let cases: [Case; 2] = [
        ("qiddn", &qidai, '漆', "d", "漆雕"),
        ("nikeyig", nikeyig, '课', "y", "你课程"),
    ];
    for (keys, words, ch, code, aux_text) in cases {
        let eng = sp_fp_engine(&format!("ui_gate_{keys}"), words, false);
        let table = || Some(FakeAux(vec![(ch, code)]));
        raw_lattice::take_gate_gaps();
        let on = aux_ui(&eng, keys, table());
        let gaps = raw_lattice::take_gate_gaps();
        assert!(
            gaps.iter().any(|g| g.kind == 'n'
                && !g.dangling
                && g.sentence == aux_text
                && g.gap()
                    .is_some_and(|v| v > 0.0 && v <= raw_lattice::AUX_GATE_MARGIN)),
            "前提：{keys} 的辅码整句比过分且不过闸：{gaps:?}"
        );
        assert!(
            on.iter()
                .any(|x| (x.0.as_str(), x.1) == (aux_text, keys.len())),
            "前提：不过闸的辅码整句仍在候选里（{keys}）：{on:?}"
        );
        let off = aux_ui(&eng, keys, None);
        assert_eq!(on[0], off[0], "{keys}: on={on:?} off={off:?}");
    }
}

/// 正当辅码在界面序下照样居首：`nihhc`（你 + 辅码 h + 好）界面首选「你好」消费 5。
#[test]
fn ui_gate_passes_real_aux() {
    let eng = sp_fp_engine("ui_gate_pass", GATE_AUX, false);
    let off = aux_ui(&eng, "nihhc", None);
    assert_ne!(off[0].0, "你好", "前提：不给辅码表时不是你好：{off:?}");
    let on = aux_ui(&eng, "nihhc", Some(FakeAux(vec![('你', "h")])));
    assert_eq!(on[0], ("你好".to_string(), 5), "{on:?}");
}

/// 整句池（`sentence_max_count > 1`）纳入过闸的整串整句：池里排第 1、消费整串，与界面一致；
/// 候选里那条带名次 1，原来的整句名次顺延（切换键按名次换位）。不过闸的不进池。
#[test]
fn ui_sn_in_pool() {
    let mut words = GATE_AUX.to_vec();
    words.push(("你航", "ni hang", 3000));
    let eng = sp_fp_engine_cfg(
        "ui_sn_pool",
        &words,
        Config {
            sentence_max_count: 3,
            ..Config::default()
        },
    );
    let run = || aux_run(&eng, "nihhc", vec![('你', "h")], None);
    let r = run();
    let pool: Vec<_> = r
        .sentence_pool
        .iter()
        .map(|c| (c.text.as_str(), c.consumed_length, c.sentence_rank))
        .collect();
    assert_eq!(pool.first(), Some(&("你好", 5, 1)), "{pool:?}");
    assert!(pool.len() >= 2, "前提：末槽整句另有 N-best：{pool:?}");
    for (i, p) in pool.iter().enumerate() {
        assert_eq!(p.2 as usize, i + 1, "名次连续：{pool:?}");
        assert_ne!(
            (p.0, p.1),
            ("你好", 4),
            "同文的末槽整句让给整串那条：{pool:?}"
        );
    }
    let top = cand_of(&r, "你好", 5);
    assert_eq!(top.sentence_rank, 1);
    assert!(top.meta.aux_keys.is_some());
    let ranked: Vec<_> = r
        .candidates
        .iter()
        .filter(|c| c.sentence_rank > 0)
        .map(|c| (c.text.as_str(), c.sentence_rank))
        .collect();
    for (t, k) in &ranked {
        assert_eq!(
            pool[*k as usize - 1].0,
            *t,
            "候选名次与池对得上：{ranked:?} {pool:?}"
        );
    }
    // 不过闸：不进池，池与不给辅码表时相同。
    let old = raw_lattice::set_aux_gate_margin(Some(1e9));
    let failed = run();
    raw_lattice::set_aux_gate_margin(old);
    let opts = ConvertOptions {
        sp_raw_lattice: true,
        ..Default::default()
    };
    let plain = eng.convert_with_opts("nihhc", 20, opts).unwrap();
    let texts = |p: &[Candidate]| -> Vec<(String, usize, u8)> {
        p.iter()
            .map(|c| (c.text.clone(), c.consumed_length, c.sentence_rank))
            .collect()
    };
    assert_eq!(texts(&failed.sentence_pool), texts(&plain.sentence_pool));
}

/// 过闸的整串整句进池后名次顺延，候选里带名次的整句仍不多于「露 N」：顶出去的新建整句只留池里，
/// 同文合并的普通候选名次清 0（文字仍在）。露 1 / 露 2 各测一次。
#[test]
fn ui_sn_in_pool_keeps_show_count() {
    let mut words = GATE_AUX.to_vec();
    words.push(("你航", "ni hang", 3000));
    for show in [1u8, 2] {
        let eng = sp_fp_engine_cfg(
            &format!("ui_sn_pool_show{show}"),
            &words,
            Config {
                sentence_count: show,
                sentence_max_count: 3,
                ..Config::default()
            },
        );
        let r = aux_run(&eng, "nihhc", vec![('你', "h")], None);
        let pool: Vec<_> = r.sentence_pool.iter().map(|c| c.text.as_str()).collect();
        assert!(pool.len() > show as usize, "前提：池比露的多：{pool:?}");
        let ranked: Vec<_> = r
            .candidates
            .iter()
            .filter(|c| c.sentence_rank > 0)
            .map(|c| (c.text.as_str(), c.sentence_rank))
            .collect();
        assert!(
            ranked.len() <= show as usize,
            "露 {show}：带名次 {ranked:?}，池 {pool:?}"
        );
        assert!(
            ranked.iter().all(|(_, k)| *k <= show),
            "露 {show}：{ranked:?}"
        );
        assert_eq!(cand_of(&r, "你好", 5).sentence_rank, 1, "露 {show}");
        // 词库里的「你航」是同文合并的普通候选：名次可清，候选不删。
        assert!(
            r.candidates.iter().any(|c| c.text == "你航"),
            "露 {show}：{:?}",
            r.candidates
        );
    }
}

fn tail_cand(text: &str, consumed: usize, weight: i32) -> Candidate {
    Candidate {
        text: text.into(),
        consumed_length: consumed,
        weight,
        ..Default::default()
    }
}

/// 6.5e 三个分支：同档有对手 → 压到最强者之下；同档无对手、也没有更靠前一档 → 不出；
/// 有更靠前一档的候选（这里是 `is_exact_code`）→ 本就不居首，原样不动。过闸的不碰。
#[test]
fn place_failed_aux_tail_branches() {
    let sn = || Candidate {
        is_sentence: true,
        ..tail_cand("你好", 5, 100)
    };
    let failed = [("你好".to_string(), 5, false)];
    let w_of = |c: &[Candidate]| c.iter().find(|x| x.text == "你好").map(|x| x.weight);
    // 同档对手（消费同为 5）。
    let mut c = vec![sn(), tail_cand("你好好", 5, 60), tail_cand("你好和", 5, 80)];
    place_failed_aux_tail(&mut c, &failed, "nihhc");
    assert_eq!(w_of(&c), Some(79));
    // 只有消费更少的候选：同档无对手 → 不出。
    let mut c = vec![sn(), tail_cand("你", 2, 500)];
    place_failed_aux_tail(&mut c, &failed, "nihhc");
    assert_eq!(w_of(&c), None, "{c:?}");
    // 更靠前一档（消费同为 5、精确码）且同档无对手 → 不动。
    let ahead = Candidate {
        is_exact_code: true,
        ..tail_cand("你好和", 5, 10)
    };
    let mut c = vec![sn(), ahead, tail_cand("你", 2, 500)];
    place_failed_aux_tail(&mut c, &failed, "nihhc");
    assert_eq!(w_of(&c), Some(100), "{c:?}");
    // 过闸的不碰。
    let mut c = vec![sn(), tail_cand("你", 2, 500)];
    place_failed_aux_tail(&mut c, &[("你好".to_string(), 5, true)], "nihhc");
    assert_eq!(w_of(&c), Some(100));
}

/// 击键图 + 辅码表跑一次，`sep` 覆写编码栏辅码分隔符。
fn aux_run(
    eng: &PinyinEngine,
    input: &str,
    table: Vec<(char, &'static str)>,
    sep: Option<&str>,
) -> ConvertResult {
    let opts = ConvertOptions {
        sp_raw_lattice: true,
        aux_continuous: Some(Arc::new(FakeAux(table))),
        aux_preedit_separator: sep.map(Arc::from),
        ..Default::default()
    };
    eng.convert_with_opts(input, 20, opts).unwrap()
}

/// `(文字, 消费数)` 那条候选。
fn cand_of<'a>(r: &'a ConvertResult, text: &str, consumed: usize) -> &'a Candidate {
    r.candidates
        .iter()
        .find(|c| c.text == text && c.consumed_length == consumed)
        .unwrap_or_else(|| panic!("缺 {text}/{consumed}：{:?}", r.candidates))
}

/// 带辅码候选的编码栏：音节之间 `'`、辅码前一个空格（默认分隔符），辅码位置在击键坐标。
/// 手打的 `'` 原样保留、不重复插入。
#[test]
fn aux_preedit_mid_and_tail() {
    let eng = sp_fp_engine("aux_preedit_shape", NIHAO_AUX, false);
    for (keys, table, split, aux) in [
        ("nihnhc", vec![('你', "hn")], "ni hn'hc", (2, 2)),
        ("nihchn", vec![('好', "hn")], "ni'hc hn", (4, 2)),
        ("nihn'hc", vec![('你', "hn")], "ni hn'hc", (2, 2)),
        ("ni'hchn", vec![('好', "hn")], "ni'hc hn", (5, 2)),
        ("nihhc", vec![('你', "h")], "ni h'hc", (2, 1)),
    ] {
        let r = aux_run(&eng, keys, table, None);
        let c = cand_of(&r, "你好", keys.len());
        assert_eq!(c.meta.preedit_split.as_deref(), Some(split), "{keys}");
        assert_eq!(c.meta.aux_keys.as_deref(), Some(&[aux][..]), "{keys}");
        assert!(!split.contains("''"), "{keys}");
        // 不带辅码的候选不带位置。
        assert!(
            r.candidates
                .iter()
                .filter(|x| x.text != "你好")
                .all(|x| x.meta.aux_keys.is_none()),
            "{keys}: {:?}",
            r.candidates
        );
    }
}

/// 编码栏辅码分隔符可覆写：空串时辅码直接贴在音节后，别的符号照插。
#[test]
fn aux_preedit_separator_override() {
    let eng = sp_fp_engine("aux_preedit_sep", NIHAO_AUX, false);
    for (sep, want) in [("", "nihn'hc"), ("·", "ni·hn'hc"), (" ", "ni hn'hc")] {
        let r = aux_run(&eng, "nihnhc", vec![('你', "hn")], Some(sep));
        let c = cand_of(&r, "你好", 6);
        assert_eq!(c.meta.preedit_split.as_deref(), Some(want), "{sep:?}");
    }
    let r = aux_run(&eng, "nihchn", vec![('好', "hn")], Some(""));
    assert_eq!(
        cand_of(&r, "你好", 6).meta.preedit_split.as_deref(),
        Some("ni'hchn")
    );
}

/// `disp` 是不是 `buf` 按序插入 `allowed` 里的字符得到的（编码栏光标换算的前提）。
fn is_insertion_of(buf: &str, disp: &str, allowed: &[char]) -> bool {
    let mut b = buf.chars().peekable();
    for d in disp.chars() {
        if b.peek() == Some(&d) {
            b.next();
        } else if !allowed.contains(&d) {
            return false;
        }
    }
    b.next().is_none()
}

/// 带辅码候选的五条一致性规则（`CandidateMeta::aux_keys`），`consumed` 为期望的消费数：
/// 1. 消费数 = 整句在击键串上的终点（含辅码与其后 `'`）；
/// 2. 码 / 边界 = 去掉辅码与 `'` 后的击键按双拼读出的全拼码（含末键残码）与音节边界；
/// 3. 造词码 `None`（= 码）或与码同字数同音节数、且与码不同；
/// 4. 辅码段在 `[0, consumed)` 内、升序不重叠、全是字母（不与音节键重叠由 2 保证）；
/// 5. 编码栏是整个缓冲按序插入 `'` 与空格，每段辅码前是空格。
fn assert_aux_candidate_consistent(
    eng: &PinyinEngine,
    c: &Candidate,
    input: &str,
    consumed: usize,
) {
    let tag = format!("{input} → {}", c.text);
    assert_eq!(c.consumed_length, consumed, "{tag}: 消费数");
    let aux = c
        .meta
        .aux_keys
        .as_deref()
        .unwrap_or_else(|| panic!("{tag}: 应带辅码位置"));
    assert!(!aux.is_empty(), "{tag}");
    let mut last = 0usize;
    for &(s, l) in aux {
        let (s, l) = (usize::from(s), usize::from(l));
        assert!(l > 0 && s >= last && s + l <= consumed, "{tag}: {aux:?}");
        assert!(
            input[s..s + l].bytes().all(|b| b.is_ascii_alphabetic()),
            "{tag}: {aux:?}"
        );
        last = s + l;
    }
    let in_aux = |i: usize| {
        aux.iter()
            .any(|&(s, l)| (usize::from(s)..usize::from(s) + usize::from(l)).contains(&i))
    };
    let syl_keys: String = input[..consumed]
        .char_indices()
        .filter(|&(i, ch)| ch != '\'' && !in_aux(i))
        .map(|(_, ch)| ch)
        .collect();
    let sp = eng.shuangpin.as_ref().unwrap().convert(&syl_keys);
    assert_eq!(c.code, sp.full_pinyin, "{tag}: 码（音节键 {syl_keys}）");
    let mut want = 0u64;
    for s in &sp.syllables {
        want |= 1 << s.fp_start;
    }
    if sp.has_partial {
        want |= 1 << sp.syllables.last().map_or(0, |s| s.fp_end);
    }
    assert_eq!(c.boundary, want, "{tag}: 边界");
    assert_eq!(
        c.boundary.count_ones() as usize,
        c.text.chars().count(),
        "{tag}: 一字一音节"
    );
    if let Some((lc, lb)) = &c.meta.learn_code {
        assert_eq!(lb.count_ones(), c.boundary.count_ones(), "{tag}: 造词码");
        assert!(
            !(lc == &c.code && *lb == c.boundary),
            "{tag}: 同码应为 None"
        );
    }
    let split = c
        .meta
        .preedit_split
        .as_deref()
        .unwrap_or_else(|| panic!("{tag}: 应带编码栏形态"));
    assert!(
        is_insertion_of(input, split, &['\'', ' ']),
        "{tag}: {split}"
    );
    for &(s, _) in aux {
        let d = disp_pos(input, split, usize::from(s));
        assert_eq!(&split[d - 1..d], " ", "{tag}: 辅码前应是空格：{split}");
    }
}

/// 缓冲第 `pos` 个字节在编码栏里的位置（贪心同步，同协调器 `map_buf_pos_to_display_pos`，
/// 只是停在该字节本身上）。
fn disp_pos(buf: &str, disp: &str, pos: usize) -> usize {
    let (b, d) = (buf.as_bytes(), disp.as_bytes());
    let (mut bi, mut di) = (0, 0);
    while di < d.len() {
        if d[di] == b[bi] {
            if bi == pos {
                return di;
            }
            bi += 1;
        }
        di += 1;
    }
    panic!("{pos} 不在 {disp} 里");
}

/// 末槽整句用了辅码、P1 的音节链没断（`tw` 自己也读 tei）：码按实际路径（`nihao`），不是
/// P1 的 `niteihao`。另含不过闸的末槽辅码整句（`wopdle`，测试设闸）。
#[test]
fn aux_candidate_consistent_step2() {
    let eng = sp_fp_engine("aux_cons_step2", NIHAO_AUX, false);
    let r = aux_run(&eng, "nitwhc", vec![('你', "tw")], None);
    let c = cand_of(&r, "你好", 6);
    assert_aux_candidate_consistent(&eng, c, "nitwhc", 6);
    assert_eq!((c.code.as_str(), c.boundary), ("nihao", 0b101));
    assert_eq!(c.meta.learn_code, None);
    assert_eq!(c.meta.preedit_split.as_deref(), Some("ni tw'hc"));

    let words: &[(&str, &str, i32)] = &[("我", "wo", 9000), ("拍", "pai", 50), ("了", "le", 9000)];
    let eng = sp_fp_engine("aux_cons_step2_gate", words, false);
    let old = raw_lattice::set_aux_se_gate_margin(Some(1e9));
    let r = aux_run(&eng, "wopdle", vec![('我', "pd")], None);
    raw_lattice::set_aux_se_gate_margin(old);
    let c = cand_of(&r, "我了", 6);
    assert_aux_candidate_consistent(&eng, c, "wopdle", 6);
    assert_eq!(c.code, "wole");
}

/// 链断入口（辅码与下一键拼不成音节，P1 只剩首音节）：码不再只有首音节 `ni`。
/// 偶数长走末槽整句、奇数长走 S_n。
#[test]
fn aux_candidate_consistent_chain_break() {
    let eng = sp_fp_engine("aux_cons_break", NIHAO_AUX, false);
    for (keys, split) in [("nioyhc", "ni oy'hc"), ("niohc", "ni o'hc")] {
        let r = aux_run(&eng, keys, vec![('你', "oy")], None);
        let c = cand_of(&r, "你好", keys.len());
        assert_aux_candidate_consistent(&eng, c, keys, keys.len());
        assert_eq!((c.code.as_str(), c.boundary), ("nihao", 0b101), "{keys}");
        assert_eq!(c.meta.preedit_split.as_deref(), Some(split), "{keys}");
    }
}

/// S_n 放行（奇数长、辅码在句中）：码是 `nihao`，不是 P1 读出的 `nihang`（`hh` = hang）。
#[test]
fn aux_candidate_consistent_tail_sn() {
    let eng = sp_fp_engine("aux_cons_sn", GATE_AUX, false);
    let r = aux_run(&eng, "nihhc", vec![('你', "h")], None);
    let c = cand_of(&r, "你好", 5);
    assert_aux_candidate_consistent(&eng, c, "nihhc", 5);
    assert_eq!(c.code, "nihao");
    assert_eq!(c.meta.preedit_split.as_deref(), Some("ni h'hc"));
}

/// 2c 残码整句里用了辅码（末键残码 `m` 补成「吗」）：码 = 路径音节 + 残码。
#[test]
fn aux_candidate_consistent_residual_2c() {
    let mut words = NIHAO_AUX.to_vec();
    words.push(("吗", "ma", 9000));
    let eng = sp_fp_engine("aux_cons_2c", &words, false);
    let r = aux_run(&eng, "nitwhcm", vec![('你', "tw")], None);
    let c = cand_of(&r, "你好吗", 7);
    assert_aux_candidate_consistent(&eng, c, "nitwhcm", 7);
    assert_eq!((c.code.as_str(), c.boundary), ("nihaom", 0b100101));
    assert_eq!(c.meta.preedit_split.as_deref(), Some("ni tw'hc'm"));
    // 同输入的末槽整句只消费到 6：剩余缓冲 `m` 接在编码栏后。
    let c = cand_of(&r, "你好", 6);
    assert_aux_candidate_consistent(&eng, c, "nitwhcm", 6);
    assert_eq!(c.meta.preedit_split.as_deref(), Some("ni tw'hc'm"));
}

/// 同文合并换整套身份：末槽整句并入 step 1 的「你好」（`nihchn`）、S_n 并入码为 `nihang` 的
/// 普通词「你好」（`nihhc`）。合并后只有一条，五条规则成立，码不留旧候选的。
#[test]
fn aux_candidate_consistent_merged() {
    let eng = sp_fp_engine("aux_cons_merge", NIHAO_AUX, false);
    let r = aux_run(&eng, "nihchn", vec![('好', "hn")], None);
    assert_eq!(r.candidates.iter().filter(|c| c.text == "你好").count(), 1);
    let c = cand_of(&r, "你好", 6);
    assert!(c.is_sentence && !c.is_synthesized, "前提：并入 step 1 词条");
    assert_aux_candidate_consistent(&eng, c, "nihchn", 6);
    assert_eq!(c.meta.preedit_split.as_deref(), Some("ni'hc hn"));

    let mut words = GATE_AUX.to_vec();
    words.push(("你好", "ni hang", 100));
    words.push(("你行", "ni hang", 50000));
    let eng = sp_fp_engine("aux_cons_merge_sn", &words, false);
    let r = aux_run(&eng, "nihhc", vec![('你', "h")], None);
    assert_eq!(r.candidates.iter().filter(|c| c.text == "你好").count(), 1);
    let c = cand_of(&r, "你好", 5);
    assert!(!c.is_synthesized, "前提：并入普通词");
    assert_aux_candidate_consistent(&eng, c, "nihhc", 5);
    assert_eq!(c.code, "nihao", "不留普通词的 nihang");
}

/// 2c 残码整句（用了辅码）与 step 1 同文合并：`nihchch` 的「你(hc) 好 + 残码 h → 好」与词条
/// 「你好好」（ni hao hao，消费 6）同文，合并后整套换成整句身份（消费 7、码 `nihaoh`）。
#[test]
fn aux_candidate_consistent_residual_2c_merged() {
    let mut words = NIHAO_AUX.to_vec();
    words.push(("你好好", "ni hao hao", 1));
    let eng = sp_fp_engine("aux_cons_2c_merge", &words, false);
    let r = aux_run(&eng, "nihchch", vec![('你', "hc")], None);
    assert_eq!(
        r.candidates.iter().filter(|c| c.text == "你好好").count(),
        1
    );
    let c = cand_of(&r, "你好好", 7);
    assert!(!c.is_synthesized, "前提：并入 step 1 词条");
    assert_aux_candidate_consistent(&eng, c, "nihchch", 7);
    assert_eq!((c.code.as_str(), c.boundary), ("nihaoh", 0b100101));
    assert_eq!(c.meta.preedit_split.as_deref(), Some("ni hc'hc'h"));
}

/// 不给辅码表（击键图开 / 关）、或给了表但没用上辅码：候选不带辅码位置，双拼候选的
/// `preedit_split` 照旧为空。
#[test]
fn aux_none_meta_untouched() {
    let eng = sp_fp_engine("aux_none_meta", NIHAO_AUX, false);
    for keys in ["nihc", "nihchn", "nihnhc", "nihcm", "ni'hc"] {
        for raw in [false, true] {
            let opts = ConvertOptions {
                sp_raw_lattice: raw,
                ..Default::default()
            };
            let r = eng.convert_with_opts(keys, 20, opts).unwrap();
            assert!(
                r.candidates
                    .iter()
                    .chain(&r.sentence_pool)
                    .all(|c| c.meta.aux_keys.is_none() && c.meta.preedit_split.is_none()),
                "{keys} raw={raw}: {:?}",
                r.candidates
            );
        }
    }
    let r = aux_run(&eng, "nihc", vec![('你', "hc"), ('好', "hn")], None);
    assert!(
        r.candidates
            .iter()
            .all(|c| c.meta.aux_keys.is_none() && c.meta.preedit_split.is_none()),
        "{:?}",
        r.candidates
    );
}

/// 全拼降级支路不吃双拼同键音节：全拼打 `diushi` 只出「丢失」，双拼 `dqui` 照出「对视」。
///
/// 布局让 `diushi` 在双拼流里读作 di|shao|hi，与 diu/dui 无关——「丢失」只能来自全拼
/// 支路（支路至少要两个音节），「对视」若出现也只能是支路误吃了同键对。
#[test]
fn full_pinyin_fallback_ignores_shared_key() {
    use std::io::Write;
    let path = std::env::temp_dir().join("wind_fp_fallback_shared_key.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        writeln!(f, "丢失\tdiu shi\t100").unwrap();
        writeln!(f, "对视\tdui shi\t300").unwrap();
    }
    let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
    let _ = std::fs::remove_file(&path);
    let layout = r#"
[meta]
id = "shared"
name = "同键测试"
[initials]
u = "sh"
[finals]
q = ["iu", "ui"]
i = ["i"]
s = ["ao"]
"#;
    let cfg = Config {
        allow_full_pinyin: true,
        ..Config::default()
    };
    let eng = PinyinEngine::new(cfg, dict).with_shuangpin(ShuangpinConverter::new(
        Layout::from_toml_str(layout).unwrap(),
    ));
    let r = eng.convert("diushi", 20).unwrap();
    assert!(
        texts(&r).contains(&"丢失"),
        "全拼支路应出「丢失」: {:?}",
        texts(&r)
    );
    assert!(
        !texts(&r).contains(&"对视"),
        "全拼支路不得吃同键对: {:?}",
        texts(&r)
    );
    let r = eng.convert("dqui", 20).unwrap();
    assert!(
        texts(&r).contains(&"对视"),
        "双拼流照出「对视」: {:?}",
        texts(&r)
    );
}

/// `add_store_abbrev_nodes` 赢下同词同跨度的既有节点时，切分与 `canon` 随胜者换
/// （与 `add_store_nodes` 的两个分支一致），不只换分数。
///
/// 直接在词图上验：预置一个同词、低分、带规范码与别种切分的节点，调用后应整体换成
/// 用户简拼节点的击键域切分与它自己的规范码。
#[test]
fn store_abbrev_node_win_replaces_segmentation_and_canon() {
    let store = tmp_store("abbrev_node_replace");
    // bai|cheng|xian → 位 0/3/8
    store
        .add_user_word("pinyin", "baichengxian", "拜城县", 800, 0b1_0000_1001)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(store, "pinyin")));
    let mut nodes: Vec<Vec<lattice::LatticeNode>> = vec![Vec::new(); 4];
    nodes[3].push(lattice::LatticeNode {
        start: 0,
        end: 3,
        word: "拜城县".to_string(),
        syllables: vec!["bcx".to_string()],
        syl_mask: 0b1,
        log_prob: -1000.0,
        // 预置一个不同的规范码，换没换一眼可辨。
        canon: Some(("stale".to_string(), 0b1)),
    });
    LatticeBuilder::new().add_store_abbrev_nodes("bcx", &dm, &mut nodes);
    assert_eq!(nodes[3].len(), 1, "同词同起点不新增");
    let n = &nodes[3][0];
    assert!(n.log_prob > -1000.0, "分数应换成用户节点的");
    assert_eq!(n.syllables, ["b", "c", "x"]);
    assert_eq!(n.syl_mask, 0b111);
    assert_eq!(
        n.canon,
        Some(("baichengxian".to_string(), 0b1_0000_1001)),
        "canon 换成用户简拼节点自己的规范码"
    );
}

/// 全拼降级支路的单词候选同样带造词码与来源（与主路径 step 1/3/6 同口径）：
/// 系统词模糊命中带词典码，用户词模糊命中带记录码、来源标记与存储码。
/// 丢了 meta 的话，分步上屏造出的是 `zongguo…` 这种模糊原码，用户词也删不掉。
#[test]
fn full_pinyin_fallback_fuzzy_words_carry_learn_code() {
    let store = tmp_store("sp_fp_fuzzy_words");
    store
        .add_user_word("pinyin", "caijiu", "菜就", 500, 0b1001)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(store, "pinyin")));
    let eng = sp_fp_engine("fuzzy_words", &[("中国", "zhong guo", 5000)], true)
        .with_fuzzy(FuzzyConfig {
            zh_z: true,
            ch_c: true,
            ..Default::default()
        })
        .with_store_layers(Arc::new(dm));

    let r = eng.convert("zongguo", 50).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "中国" && c.is_fullpinyin_fallback)
        .unwrap_or_else(|| panic!("应出降级支路的「中国」，实际: {:?}", texts(&r)));
    assert!(c.is_fuzzy);
    assert_eq!(c.code, "zongguo", "候选码仍是击键");
    assert_eq!(
        c.meta.learn_code.as_ref().map(|(code, _)| code.as_str()),
        Some("zhongguo"),
        "系统词模糊命中须带词典码"
    );

    let r = eng.convert("chaijiu", 50).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "菜就" && c.is_fullpinyin_fallback)
        .unwrap_or_else(|| panic!("应出降级支路的「菜就」，实际: {:?}", texts(&r)));
    assert_eq!(c.meta.learn_code, Some(("caijiu".to_string(), 0b1001)));
    assert!(c.meta.is_user_dict, "来源标记不得丢");
    assert_eq!(c.meta.store_code.as_deref(), Some("caijiu"), "删除按记录码");
}

/// 降级支路同文去重时，后到的 store 命中虽然整条让位，来源标记与存储码要并入
/// 先占位的那条（同主路径 step 6）：系统词「菜就」先在 ① 占位、用户也收录了它，
/// 否则右键删除找不到用户那条记录。
#[test]
fn full_pinyin_fallback_merges_store_origin_into_same_text() {
    let store = tmp_store("sp_fp_merge_origin");
    store
        .add_user_word("pinyin", "caijiu", "菜就", 500, 0b1001)
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(store, "pinyin")));
    let eng = sp_fp_engine("merge_origin", &[("菜就", "cai jiu", 5000)], true)
        .with_store_layers(Arc::new(dm));
    let r = eng.convert("caijiu", 50).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "菜就" && c.is_fullpinyin_fallback)
        .unwrap_or_else(|| panic!("应出降级支路的「菜就」，实际: {:?}", texts(&r)));
    assert!(c.meta.is_user_dict, "用户词的来源须并入");
    assert_eq!(c.meta.store_code.as_deref(), Some("caijiu"));
}

/// 全拼降级支路的整句同样带造词码：双拼方案下按全拼 + z=zh 打 `zongguorenmin`，
/// 整句「中国人民」的 `code` 是击键串，`learn_code` 是规范码 zhong|guo|ren|min。
#[test]
fn full_pinyin_fallback_fuzzy_sentence_learn_code() {
    let eng = sp_fp_engine(
        "fuzzy_learn",
        &[("中国", "zhong guo", 5000), ("人民", "ren min", 5000)],
        true,
    )
    .with_fuzzy(FuzzyConfig {
        zh_z: true,
        ..Default::default()
    });
    let r = eng.convert("zongguorenmin", 50).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.is_sentence && c.is_fullpinyin_fallback)
        .unwrap_or_else(|| panic!("应出降级支路整句，实际候选: {:?}", texts(&r)));
    assert_eq!(c.text, "中国人民");
    assert_eq!(c.code, "zongguorenmin");
    assert_eq!(
        c.meta.learn_code,
        Some(code_and_mask(&["zhong", "guo", "ren", "min"]))
    );
}

/// 核心用例，同时锁住支路的位置。
///
/// 双拼下按全拼打 `nihao`（5 键）应出「你好」。`nihao` 的双拼解释 ni|ha|o 与击键同形，step1
/// 会精确命中「你好」、随后被边界校验删掉；支路若排在校验之前，会因同文查重让位给那条注定被删
/// 的候选，最终一条都不剩，现象是「开关像没生效」。
#[test]
fn full_pinyin_recalls_word_that_shuangpin_boundary_check_rejects() {
    let eng = sp_fp_engine(
        "recall",
        &[("你好", "ni hao", 200), ("你", "ni", 100)],
        true,
    );
    let r = eng.convert("nihao", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .unwrap_or_else(|| panic!("应出「你好」，实际候选: {:?}", texts(&r)));
    assert!(c.is_fullpinyin_fallback, "应带全拼支路的来源标记");
    assert_eq!(
        c.consumed_length, 5,
        "consumed 必须是**击键域**的 5，不可再过 map_consumed_length 回映射成双拼键数"
    );
}

/// 反向对照：开关关闭时 `nihao` 不出全拼候选（正确的双拼打法是 `nihc`）。
#[test]
fn full_pinyin_off_keeps_shuangpin_only_behavior() {
    let eng = sp_fp_engine("off", &[("你好", "ni hao", 200), ("你", "ni", 100)], false);
    let r = eng.convert("nihao", 20).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.text == "你好"),
        "开关关闭时不该出「你好」，实际候选: {:?}",
        texts(&r)
    );
}

/// 闸门：双拼击键串不得触发支路——这是「双拼体验不被污染」的结构性保证。
#[test]
fn full_pinyin_gate_rejects_shuangpin_strokes() {
    let eng = sp_fp_engine("gate", &[("你好", "ni hao", 200)], true);
    // `nihc` = 「你好」的小鹤双拼码：ni + `hc`，而 `hc` 既不成音节也不是音节前缀
    // ⇒ 只切得出 1 个音节 ⇒ 不足 FULL_PINYIN_MIN_SYLLABLES。
    assert!(
        eng.full_pinyin_gate("nihc").is_none(),
        "nihc（双拼码）不该触发全拼支路"
    );
    assert!(
        eng.full_pinyin_gate("womf").is_none(),
        "womf（=women 的双拼码）不该触发"
    );
    // 尾部残码须是合法音节前缀：`m` 可以，`xyz` 不行。
    assert!(eng.full_pinyin_gate("nihaom").is_some(), "nihaom 应放行");
    assert!(
        eng.full_pinyin_gate("nihaoxyz").is_none(),
        "尾部 xyz 不是音节前缀，应拒绝"
    );
    // 真·全拼串放行。
    assert!(eng.full_pinyin_gate("nihao").is_some());
}

/// 支路不得干扰双拼流：`nihc` 走双拼正路，候选不带降级标志、consumed 仍是键数。
#[test]
fn full_pinyin_branch_does_not_disturb_shuangpin_path() {
    let eng = sp_fp_engine(
        "undisturbed",
        &[("你好", "ni hao", 200), ("你", "ni", 100)],
        true,
    );
    let r = eng.convert("nihc", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .unwrap_or_else(|| panic!("双拼正确打法应出「你好」，实际: {:?}", texts(&r)));
    assert!(
        !c.is_fullpinyin_fallback,
        "这条由双拼流产出，不该带降级标志"
    );
    assert_eq!(c.consumed_length, 4, "双拼流的 consumed 仍是键数 4");
}

/// 两条流都解释得出同一个词时，留下双拼那条（不带降级标志、不沉底）。
/// `nini`：双拼 n+i / n+i → "nini"，全拼 ni|ni 也是 "nini"，两域同形同码。
#[test]
fn shuangpin_candidate_wins_when_both_domains_agree() {
    let eng = sp_fp_engine("agree", &[("妮妮", "ni ni", 150)], true);
    let r = eng.convert("nini", 20).unwrap();
    let hits: Vec<_> = r.candidates.iter().filter(|c| c.text == "妮妮").collect();
    assert_eq!(hits.len(), 1, "同文候选不该重复，实际: {:?}", texts(&r));
    assert!(
        !hits[0].is_fullpinyin_fallback,
        "双拼也解释得出时，留下的必须是双拼那条"
    );
}

/// 层级：低置信全拼候选（前缀补全）沉底，高置信（精确整词 + 消费整串）不沉底。
///
/// 别改成「全拼候选一律排在双拼候选之后」：`zaijian` 的正解「再见」会被整层压到第 8 位。
/// 「双拼优先」由同文去重那一层保住（consumed 相同则留双拼那条），不靠层级键一刀切。
#[test]
fn full_pinyin_low_confidence_sinks_but_exact_word_does_not() {
    let eng = sp_fp_engine(
        "sink",
        &[
            ("你好", "ni hao", 200),
            ("你好吗", "ni hao ma", 5000),
            ("你", "ni", 100),
        ],
        true,
    );
    let r = eng.convert("nihao", 20).unwrap();
    let exact = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .unwrap_or_else(|| panic!("应出精确整词「你好」，实际: {:?}", texts(&r)));
    assert!(exact.is_fullpinyin_fallback, "来源标记：来自支路");
    assert!(
        !exact.is_prefix && !exact.is_partial,
        "精确整词＝高置信（cmp_match_layers 据此免除沉底）"
    );

    // 「你好吗」码更长＝前缀补全＝在预测用户还没打的音节 ⇒ 低置信，沉底。
    // 它的 weight(5000) 远高于「你好」(200)，若不沉底就会靠权重反超——正是要防的。
    if let Some(compl) = r.candidates.iter().find(|c| c.text == "你好吗") {
        assert!(
            compl.is_fullpinyin_fallback && compl.is_prefix,
            "前缀补全＝低置信，cmp_match_layers 据此沉底（否则高权重补全会盖过精确整词）"
        );
        let p_exact = r.candidates.iter().position(|c| c.text == "你好").unwrap();
        let p_compl = r
            .candidates
            .iter()
            .position(|c| c.text == "你好吗")
            .unwrap();
        assert!(
            p_exact < p_compl,
            "精确整词须先于前缀补全，实际: {:?}",
            texts(&r)
        );
    }
}

/// 同文去重取「解释得更多的那条」，而非无条件保留先到者。
///
/// `zaijian`：双拼流的简拼前缀回退先以 `consumed=4` 放入「再见」，支路随后以 `consumed=7`
/// 完整解释同一个词——若让位给先到者，用户选中后缓冲会凭空剩下 `ian`。
#[test]
fn full_pinyin_replaces_partial_duplicate_with_complete_one() {
    let eng = sp_fp_engine("dedup", &[("再见", "zai jian", 2837)], true);
    let r = eng.convert("zaijian", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "再见")
        .unwrap_or_else(|| panic!("应出「再见」，实际: {:?}", texts(&r)));
    assert_eq!(
        c.consumed_length,
        "zaijian".len(),
        "必须是完整解释（7 键）那条；留下半截解释会让上屏后残留余码"
    );
    assert_eq!(
        r.candidates.iter().filter(|c| c.text == "再见").count(),
        1,
        "同文不得重复"
    );
}

/// 模糊音：支路与双拼流吃同一套 `[schema.pinyin.fuzzy]` 设置——同一套配置在两条流下表现
/// 不一致本身就是缺陷。惩罚由 `lookup_with_fuzzy` 的 `fuzzy_penalized` 施加。
#[test]
fn full_pinyin_honors_fuzzy_settings() {
    let mut eng = sp_fp_engine("fuzzy", &[("中国", "zhong guo", 9000)], true);
    // zh↔z：打 `zongguo` 应经模糊命中「中国」。
    eng.fuzzy_config.zh_z = true;
    let r = eng.convert("zongguo", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "中国")
        .unwrap_or_else(|| panic!("开 zh_z 后 zongguo 应出「中国」，实际: {:?}", texts(&r)));
    assert!(c.is_fuzzy, "应标记为模糊命中（据此施加权重折扣）");
}

/// preedit 跟随首选：首选是全拼候选时，编码栏按全拼切分显示。
/// 词典只留 `nihao`，双拼流一条候选都产不出，支路的「你好」即首选。
#[test]
fn engine_exposes_both_preedit_forms() {
    let eng = sp_fp_engine("preedit", &[("你好", "ni hao", 200)], true);
    let r = eng.convert("nihao", 20).unwrap();
    assert_eq!(
        r.preedit_display, "ni'ha'o",
        "默认形态恒为**双拼自己的**切分，不因首选是谁而变"
    );
    assert_eq!(
        r.preedit_fullpinyin, "ni'hao",
        "另行交出全拼切分，供协调器在高亮到全拼候选时取用"
    );
}

/// 支路无产出时不给全拼形态——省得协调器凭空多一个可选项。
#[test]
fn engine_omits_full_pinyin_preedit_when_branch_silent() {
    let eng = sp_fp_engine("preedit_off", &[("你好", "ni hao", 200)], false);
    let r = eng.convert("nihao", 20).unwrap();
    assert!(
        r.preedit_fullpinyin.is_empty(),
        "开关关闭时不该有全拼形态，实际: {:?}",
        r.preedit_fullpinyin
    );
}

/// 整句解码（⑤）：全拼降级流也要能组句，否则「完整的全拼也能工作」只兑现一半——
/// 词典里没有的搭配全部落空。
#[test]
fn full_pinyin_composes_sentence() {
    let eng = sp_fp_engine(
        "sentence",
        &[
            ("我", "wo", 10000),
            ("今天", "jin tian", 5000),
            ("很", "hen", 3000),
            ("开心", "kai xin", 2000),
        ],
        true,
    );
    let r = eng.convert("wojintianhenkaixin", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "我今天很开心")
        .unwrap_or_else(|| panic!("全拼长串应组出整句，实际候选: {:?}", texts(&r)));
    assert!(c.is_sentence, "应标为整句");
    assert!(c.is_fullpinyin_fallback, "整句同样带支路来源标记");
    assert_eq!(
        c.consumed_length,
        "wojintianhenkaixin".len(),
        "整句消费完整音节段（本例无残码，即整串）"
    );
}

/// 整句让位于精确整词：支路自带一份判据，因为主路径 6.5/6.5b 跑在双拼边界校验之前、
/// 够不着本支路的候选。
///
/// `nihao` 下 Viterbi 拼出的「你好」会被同文查重挡掉，故改用词典没有的组合：`nihen` →「你很」
/// 由两个单字拼出，而 `ni hen` 码上另有精确词「妮痕」压着，整句须排在它之后。
#[test]
fn full_pinyin_sentence_yields_to_exact_word() {
    let eng = sp_fp_engine(
        "yield",
        &[
            ("妮痕", "ni hen", 9000),
            ("你", "ni", 500),
            ("很", "hen", 400),
        ],
        true,
    );
    let r = eng.convert("nihen", 20).unwrap();
    let exact = r.candidates.iter().find(|c| c.text == "妮痕");
    let sentence = r.candidates.iter().find(|c| c.text == "你很");
    if let (Some(e), Some(s)) = (exact, sentence) {
        assert!(
            s.weight < e.weight,
            "整句「你很」({}) 须让位于精确整词「妮痕」({})",
            s.weight,
            e.weight
        );
    } else {
        // 词图未拼出该整句时不强求（Viterbi 行为随词典权重浮动），但精确词必须在。
        assert!(
            exact.is_some(),
            "精确整词「妮痕」必须在，实际: {:?}",
            texts(&r)
        );
    }
}

/// 非双拼（纯全拼）方案下开关无效——全拼本就是主路径，支路会把同一批候选查两遍。
#[test]
fn full_pinyin_flag_is_noop_without_shuangpin() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("nihao".to_string(), "你好".to_string(), 200, 0);
    let cfg = Config {
        allow_full_pinyin: true,
        ..Config::default()
    };
    let eng = PinyinEngine::new(cfg, CachedDict::Memory(raw));
    let r = eng.convert("nihao", 20).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "你好"),
        "全拼方案照常出「你好」"
    );
    assert!(
        r.candidates.iter().all(|c| !c.is_fullpinyin_fallback),
        "全拼方案下不该有任何候选被标成降级候选"
    );
}

/// 边界相容判定：双拼定死音节边界，候选的词典边界须与之吻合。
#[test]
fn boundary_compatible_rules() {
    // 输入 nihao(5键) 双拼解释 ni|ha|o → 全拼 "nihao"，边界 {0,2,4}
    let sp = 0b10101u64;
    // 「你好」词典边界 ni|hao = {0,2}，与解释不符 → 拒绝（这正是 5 键出「你好」的病灶）
    assert!(
        !boundary_compatible(0b101, sp, 5, 5),
        "ni|hao 不该匹配 ni|ha|o"
    );
    // 「你」code=ni(2B) 边界 {0} → 只比前 2 字节窗口 → 相容
    assert!(boundary_compatible(0b1, sp, 2, 5));
    // 「你哈」code=niha(4B) 边界 {0,2} → 前 4 字节窗口相容
    assert!(boundary_compatible(0b101, sp, 4, 5));

    // 正确双拼 nihc(4键,小鹤) 解释 ni|hao → 全拼 "nihao"，边界 {0,2}
    let sp2 = 0b101u64;
    assert!(
        boundary_compatible(0b101, sp2, 5, 5),
        "ni|hao 应匹配 ni|hao"
    );

    // 前缀补全：输入 ni（全拼串仅 2B），候选「你好」code=nihao(5B) 边界 {0,2}
    // → 窗口取 min(5,2)=2，只比已输入部分 → 相容（补全部分尚未键入，无从校验）
    assert!(boundary_compatible(0b101, 0b1, 5, 2));

    // 任一侧无信息 → 放行（用户手输码/五笔/模糊变体/含回写段）
    assert!(boundary_compatible(0, sp, 5, 5));
    assert!(boundary_compatible(0b101, 0, 5, 5));
}

/// 双拼分段边界：音节、尾部 partial、以及无匹配键对回写段，各开一个段起点。
#[test]
fn sp_boundary_mask_rules() {
    use crate::pinyin::shuangpin::{SpConvertResult, SylSpan};
    let syl = |p: &str, fs, fe| SylSpan {
        pinyin: p.to_string(),
        raw_start: 0,
        raw_end: 0,
        fp_start: fs,
        fp_end: fe,
        literal: None,
    };
    // ni|ha + partial o → full "nihao"，边界 {0,2,4}
    // 注：has_partial 与 partial_initial 须同设——真实 convert 二者恒同时写入，
    // 只设其一是不可能出现的状态（fixture 造假会测出假结论）。
    let sp = SpConvertResult {
        syllables: vec![syl("ni", 0, 2), syl("ha", 2, 4)],
        has_partial: true,
        partial_initial: Some("o".into()),
        full_pinyin: "nihao".into(),
        ..Default::default()
    };
    assert_eq!(sp_boundary_mask(&sp), 0b10101);
    // ni|hao 恰好覆盖，无尾部 → {0,2}
    let sp2 = SpConvertResult {
        syllables: vec![syl("ni", 0, 2), syl("hao", 2, 5)],
        has_partial: false,
        full_pinyin: "nihao".into(),
        ..Default::default()
    };
    assert_eq!(sp_boundary_mask(&sp2), 0b101);
    // 回写段夹在音节中间（omni 的 om 占 0..2）：其起点同样是段边界 → {0,2}
    let sp3 = SpConvertResult {
        syllables: vec![syl("ni", 2, 4)],
        full_pinyin: "omni".into(),
        ..Default::default()
    };
    assert_eq!(
        sp_boundary_mask(&sp3),
        0b101,
        "回写段在中间时其起点也是边界"
    );
    // 回写段在尾部（nihaoya 的 oy+a 占 4..7）→ {0,2,4}。
    // 位 4 是关键：它让词典的 ni|hao|ya({0,2,5}) 失配；置 0 等于关掉校验，「你好呀」会从 step4 漏网。
    let sp4 = SpConvertResult {
        syllables: vec![syl("ni", 0, 2), syl("ha", 2, 4)],
        has_partial: true,
        partial_initial: Some("a".into()),
        full_pinyin: "nihaoya".into(),
        ..Default::default()
    };
    assert_eq!(
        sp_boundary_mask(&sp4),
        0b10101,
        "尾部回写段须标起点，否则校验失效"
    );
    assert!(
        !boundary_compatible(0b100101, 0b10101, 7, 7),
        "词典 ni|hao|ya 应与双拼 ni|ha|oy… 失配"
    );
    // 全是回写段（如 oy）→ 仅首段起点 {0}
    let sp5 = SpConvertResult {
        full_pinyin: "oy".into(),
        ..Default::default()
    };
    assert_eq!(sp_boundary_mask(&sp5), 0b1);
    // 空输入 → 无信息
    assert_eq!(sp_boundary_mask(&SpConvertResult::default()), 0);
}

/// 回归：无匹配键对（convert 的「原样回写」分支）既不进 syllables 也不置 has_partial，
/// build_raw_preedit 必须仍覆盖它们，否则编码被静默吞掉。
/// 真机现象（首道双拼）：nihaom → 显示 niha（om 消失），再按 a → ni'ha'oma 又复现。
#[test]
fn build_raw_preedit_covers_unmatched_pairs() {
    use crate::pinyin::shuangpin::{SpConvertResult, SylSpan};
    let syl = |raw_start, raw_end| SylSpan {
        pinyin: String::new(), // build_raw_preedit 只用 sp 区间切原始串，不读 pinyin
        raw_start,
        raw_end,
        fp_start: 0,
        fp_end: 0,
        literal: None,
    };

    // ① 尾部无匹配键对（om）：has_partial=false，早期实现漏掉尾巴 → "ni'ha"。
    let sp = SpConvertResult {
        syllables: vec![syl(0, 2), syl(2, 4)],
        has_partial: false,
        ..Default::default()
    };
    assert_eq!(
        build_raw_preedit("nihaom", &sp),
        "ni'ha'om",
        "尾部无匹配键对不得被吞"
    );

    // ② 尾部 partial 单键（o）：has_partial=true，行为与早期实现一致。
    let sp = SpConvertResult {
        syllables: vec![syl(0, 2), syl(2, 4)],
        has_partial: true,
        ..Default::default()
    };
    assert_eq!(build_raw_preedit("nihao", &sp), "ni'ha'o");

    // ③ 无匹配键对在中间（om 在前）：音节前的空隙也须原样保留。
    let sp = SpConvertResult {
        syllables: vec![syl(2, 4), syl(4, 6)],
        has_partial: true,
        ..Default::default()
    };
    assert_eq!(
        build_raw_preedit("omnihao", &sp),
        "om'ni'ha'o",
        "音节之间的无匹配段不得被吞"
    );

    // ④ 全无音节：原样返回。
    let sp = SpConvertResult::default();
    assert_eq!(build_raw_preedit("xq", &sp), "xq");
    assert_eq!(build_raw_preedit("", &sp), "");
}

/// 双拼真值边界校验：双拼把音节边界定死了，候选的词典边界必须吻合。
///
/// 双拼下打 5 键 `nihao` 被解释为 `ni|ha|o`，拼成 `full_pinyin="nihao"` 恰好撞上全拼，DAG 若重切成
/// `ni|hao` 就会查到「你好」；而「你好」的正确双拼是 4 键（`nihc`）。
/// 必须用 rime 源构造词典：`merge_single` 造的条目 boundary 恒 0（无信息→放行），测不出校验。
#[test]
fn shuangpin_rejects_mismatched_syllable_split() {
    use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
    use std::io::Write;
    let path = std::env::temp_dir().join("wind_sp_boundary_check.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        writeln!(f, "你好\tni hao\t2000").unwrap(); // 边界 ni|hao = {0,2}
        writeln!(f, "你\tni\t900").unwrap();
        writeln!(f, "哈\tha\t500").unwrap();
        writeln!(f, "哦\to\t300").unwrap();
    }
    let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
    let _ = std::fs::remove_file(&path);

    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let eng =
        PinyinEngine::new(Config::default(), dict).with_shuangpin(ShuangpinConverter::new(layout));

    // 5 键 nihao → 双拼 ni|ha|o（o 为 partial），与「你好」的 ni|hao 不符 → 拒绝。
    let r = eng.convert("nihao", 20).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    assert!(
        !texts.contains(&&"你好".to_string()),
        "5 键 nihao 解释为 ni|ha|o，不该出「你好」（其双拼是 4 键 nihc），实际: {texts:?}"
    );
    // 与解释相容的候选仍在：ni → 「你」
    assert!(
        texts.contains(&&"你".to_string()),
        "「你」(ni) 与 ni|ha|o 的首音节相容，应保留，实际: {texts:?}"
    );

    // 4 键 nihc → 双拼 ni|hao，与「你好」的词典边界一致 → 正常出。
    let r2 = eng.convert("nihc", 20).unwrap();
    assert!(
        r2.candidates.iter().any(|c| c.text == "你好"),
        "4 键 nihc 解释为 ni|hao，应出「你好」，实际: {:?}",
        r2.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 双拼下 `nihao` 选「你」后，剩余 `hao`(3键) 不得变成空候选。
///
/// 查询与校验必须用同一套切分：双拼解释 `hao` = `ha`+`o`(partial)，若查询仍按 DAG 重切的 `[hao]`
/// 只查「好」，会被真值 {0,2} 拒掉，而双拼该查的 `ha`（→「哈」）又没查。
#[test]
fn shuangpin_uses_own_split_for_lookup_not_dag() {
    use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
    use std::io::Write;
    let path = std::env::temp_dir().join("wind_sp_lookup_split.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        writeln!(f, "好\thao\t2000").unwrap(); // 单音节，边界 {0}
        writeln!(f, "哈\tha\t900").unwrap();
        writeln!(f, "哦\to\t300").unwrap();
    }
    let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
    let _ = std::fs::remove_file(&path);

    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let eng =
        PinyinEngine::new(Config::default(), dict).with_shuangpin(ShuangpinConverter::new(layout));

    // 3 键 hao → 双拼 ha|o：应按双拼自己的切分去查，出「哈」(ha)。
    let r = eng.convert("hao", 20).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    assert!(
        texts.contains(&&"哈".to_string()),
        "ha 是双拼解释出的完成音节，应查到「哈」，实际: {texts:?}（空候选=查询仍按 DAG 猜）"
    );
    // 「好」的双拼是 2 键 hc，3 键 h/a/o 不该出它。
    assert!(
        !texts.contains(&&"好".to_string()),
        "3 键 hao 解释为 ha|o，不该出「好」（其双拼是 hc），实际: {texts:?}"
    );

    // 2 键 hc → 双拼 hao 单音节 → 正常出「好」。
    let r2 = eng.convert("hc", 20).unwrap();
    assert!(
        r2.candidates.iter().any(|c| c.text == "好"),
        "2 键 hc 解释为 hao，应出「好」，实际: {:?}",
        r2.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 含「无匹配键对」时仍按双拼语义：取从 0 起连续的音节前缀，断裂处之后不解释。
///
/// `oy`（o 非声母，拼不出音节）是用户打错的无效键对，它及其后的内容不该产生候选。别把这里做成
/// 「整串降级回全拼 DAG」：那样 `nihaoya` 会出「你好呀」，与 `nihao`(5键) 不出「你好」自相矛盾。
/// `nh` 这种简拼由 AbbrevMatcher 兜底（走 query，不依赖 syllables），见 shuangpin_abbrev_still_works。
#[test]
fn shuangpin_keeps_own_semantics_with_unmatched_pair() {
    use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
    use std::io::Write;
    let path = std::env::temp_dir().join("wind_sp_writeback_strict.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        writeln!(f, "你好呀\tni hao ya\t2000").unwrap();
        writeln!(f, "你好\tni hao\t1500").unwrap();
        writeln!(f, "你\tni\t900").unwrap();
        writeln!(f, "哈\tha\t500").unwrap();
    }
    let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
    let _ = std::fs::remove_file(&path);

    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let conv = ShuangpinConverter::new(layout);

    // 前提：nihaoya 在小鹤下确有「无匹配键对」（oy）——即音节未覆盖到 full 末尾、
    // 且缺口大于 partial 声母。否则本测试没意义。
    let sp = conv.convert("nihaoya");
    let covered: usize = sp.syllables.last().map_or(0, |s| s.fp_end);
    let partial_len = sp.partial_initial.as_ref().map_or(0, |s| s.len());
    assert!(
        sp.full_pinyin.len() - covered > partial_len,
        "前提失效：nihaoya 应含无匹配回写段，实际 syllables={:?} full={:?}",
        sp.syllables.iter().map(|s| &s.pinyin).collect::<Vec<_>>(),
        sp.full_pinyin
    );

    let eng = PinyinEngine::new(Config::default(), dict).with_shuangpin(conv);
    let r = eng.convert("nihaoya", 20).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    // 断裂前的 ni|ha 照常出候选。
    assert!(
        texts.contains(&&"你".to_string()),
        "连续前缀 ni 应出「你」，实际: {texts:?}"
    );
    // 不得把整串当全拼——那会与「nihao 不出你好」自相矛盾。
    assert!(
        !texts.contains(&&"你好呀".to_string()) && !texts.contains(&&"你好".to_string()),
        "oy 是无效键对，不该整串降级成全拼解释，实际: {texts:?}"
    );
}

/// 无匹配键对原样回写进 full_pinyin（不产 SylSpan），输入不被吞——这是简拼兜底的前提：
/// AbbrevMatcher 走 `query`（即 full_pinyin），不看音节切分，故保住简拼不需要退回 DAG。
/// （简拼表只存在于 wdat AbbrevSection，端到端查询由 wind-dict 侧覆盖。）
#[test]
fn shuangpin_writeback_keeps_input_intact() {
    use crate::pinyin::shuangpin::{Layout, ShuangpinConverter};
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let conv = ShuangpinConverter::new(layout);
    // oy：o 非声母，拼不出合法音节 → 整对原样回写。
    let sp = conv.convert("oy");
    assert!(
        sp.syllables.is_empty(),
        "oy 拼不出音节，不该产出 SylSpan，实际 {:?}",
        sp.syllables.iter().map(|s| &s.pinyin).collect::<Vec<_>>()
    );
    assert_eq!(sp.full_pinyin, "oy", "无匹配键对须原样回写，输入不得被吞");
}

/// Fix A TDD：双拼 preedit 应显示用户实际输入的原始按键（按音节边界以 `'` 分隔，
/// 与全拼自动分词一致），而非转换后的全拼。输入小鹤 "nihc"（→全拼 nihao）应显示
/// "ni'hc"，候选仍含「你好」。
#[test]
fn shuangpin_preedit_shows_raw_keys() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("nihao".to_string(), "你好".to_string(), 200, 0);
    raw.merge_single("ni".to_string(), "你".to_string(), 100, 1);
    let dict = CachedDict::Memory(raw);

    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let conv = ShuangpinConverter::new(layout);
    let eng = PinyinEngine::new(Config::default(), dict).with_shuangpin(conv);

    // 完整双音节：preedit 为原始键按音节 ' 分隔 "ni'hc"（而非全拼 "ni'hao"）
    let r = eng.convert("nihc", 10).unwrap();
    assert_eq!(
        r.preedit_display, "ni'hc",
        "双拼 preedit 应显示原始按键并按音节 ' 分隔，实际: {:?}",
        r.preedit_display
    );
    // 候选仍走全拼语义，含「你好」
    assert!(
        r.candidates.iter().any(|c| c.text == "你好"),
        "候选仍应含「你好」，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );

    // 单音节：ni → "ni"
    let r2 = eng.convert("ni", 10).unwrap();
    assert_eq!(r2.preedit_display, "ni", "单音节 preedit 应为 \"ni\"");

    // 含 partial：nih（ni 完成 + h 未配对）→ "ni'h"
    let r3 = eng.convert("nih", 10).unwrap();
    assert_eq!(
        r3.preedit_display, "ni'h",
        "含 partial 的双拼 preedit 应为 \"ni'h\"，实际: {:?}",
        r3.preedit_display
    );
}

/// Fix B TDD：fuzzy 应接入精确/单音节查询。词典含 "shi"→"是"，
/// fuzzy sh_s=true 时，输入单音节全拼 "si" 应能命中「是」（sh↔s 模糊）。
#[test]
fn fuzzy_lookup_single_syllable() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("shi".to_string(), "是".to_string(), 100, 0);
    let dict = CachedDict::Memory(raw);
    let fz = FuzzyConfig {
        sh_s: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), dict).with_fuzzy(fz);

    let r = eng.convert("si", 10).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "是"),
        "fuzzy sh_s 开启时，单音节 \"si\" 应命中「是」，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );

    // 反向：词典 "si"→"四"，输入 "shi" 应命中「四」
    let mut raw2 = CodetableDict::empty();
    raw2.merge_single("si".to_string(), "四".to_string(), 100, 0);
    let fz2 = FuzzyConfig {
        sh_s: true,
        ..Default::default()
    };
    let eng2 = PinyinEngine::new(Config::default(), CachedDict::Memory(raw2)).with_fuzzy(fz2);
    let r2 = eng2.convert("shi", 10).unwrap();
    assert!(
        r2.candidates.iter().any(|c| c.text == "四"),
        "fuzzy sh_s 开启时，\"shi\" 应命中「四」，实际: {:?}",
        r2.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 模糊命中受 `×0.5` 折扣，但不是硬闸门：词频差距足够大时模糊仍可胜出。
///
/// 对齐 librime `kFuzzySpellingPenalty` / libime `fuzzyCost`（都是 `log(0.5)`）：折扣只表达
/// 「模糊召回的置信度低一档」，压不压得住由词频差距决定——「是」(1799848) 打 `si` 时排在
/// 「四」(22625) 前面。本用例守折扣本身存在且按音节累乘。
#[test]
fn fuzzy_hit_is_discounted_but_not_gated() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("si".to_string(), "四".to_string(), 100, 0);
    raw.merge_single("shi".to_string(), "是".to_string(), 9000, 0);
    let dict = CachedDict::Memory(raw);
    let fz = FuzzyConfig {
        sh_s: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), dict).with_fuzzy(fz);

    let r = eng.convert("si", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    let si = r.candidates.iter().find(|c| c.text == "四");
    let shi = r.candidates.iter().find(|c| c.text == "是");
    let si = si.unwrap_or_else(|| panic!("精确候选「四」应存在，实际: {texts:?}"));
    let shi = shi.unwrap_or_else(|| panic!("模糊候选「是」应存在，实际: {texts:?}"));

    assert!(!si.is_fuzzy, "「四」应为非模糊");
    assert!(shi.is_fuzzy, "「是」应为模糊命中");
    // 折扣确实施加了：9000 → 4500（单音节 ⇒ 0.5^1）。
    assert_eq!(
        shi.weight, 4500,
        "模糊命中须按 0.5^1 折扣，实际 {}",
        shi.weight
    );
    assert_eq!(si.weight, 100, "精确命中权重不受影响");
    // 90 倍的词频差距压过一档折扣 —— 折扣不是硬闸门。
    assert_eq!(r.candidates[0].text, "是", "实际: {texts:?}");
}

/// 配对：词频相当时折扣说了算，精确胜出。
/// 少了这条，「模糊完全不打折」也能让上一条通过。
#[test]
fn fuzzy_loses_to_exact_at_equal_weight() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("si".to_string(), "四".to_string(), 9000, 0);
    raw.merge_single("shi".to_string(), "是".to_string(), 9000, 0);
    let fz = FuzzyConfig {
        sh_s: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    let r = eng.convert("si", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    assert_eq!(
        r.candidates[0].text, "四",
        "同权重下模糊被折扣压住，精确胜出，实际: {texts:?}"
    );
}

/// 用户打出来的那一端本身不是合法音节时，模糊音必须仍然生效。
///
/// `tin` 不在标准音节表里，模糊变体又是在切分之后逐音节展开的；若切分不认 `tin`，`tinzhi` 的
/// DAG 只走到 `ti`、`nzhi` 全成残码，候选里压根没有「停止」。测试输入的形态是被测条件的一部分：
/// 只用「打的那端也是合法音节」的例子（`si`→`shi`）会整片绕开这个缺口。
#[test]
fn fuzzy_hits_when_typed_spelling_is_not_a_valid_syllable() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("tingzhi".to_string(), "停止".to_string(), 9000, 0);
    let fz = FuzzyConfig {
        in_ing: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    let r = eng.convert("tinzhi", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    let hit = r
        .candidates
        .iter()
        .find(|c| c.text == "停止")
        .unwrap_or_else(|| panic!("开 in_ing 后 `tinzhi` 应出「停止」，实际: {texts:?}"));
    assert!(hit.is_fuzzy, "应标记为模糊命中（据此施加 0.5^k 折扣）");
    assert_eq!(
        hit.consumed_length, 6,
        "整串已被解释，上屏须消费全部 6 键（修复前只切得出 `ti`）"
    );
}

/// 同一缺口的声母侧，且比韵母侧更常见：平翘舌不分的人打「装置」正是 `zuangzhi`
/// （`zuang` 不是合法音节，`zhuang` 才是）。
#[test]
fn fuzzy_hits_for_invalid_initial_spelling() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("zhuangzhi".to_string(), "装置".to_string(), 9000, 0);
    let fz = FuzzyConfig {
        zh_z: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    let r = eng.convert("zuangzhi", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    assert!(
        r.candidates.iter().any(|c| c.text == "装置"),
        "开 zh_z 后 `zuangzhi` 应出「装置」，实际: {texts:?}"
    );
}

/// 模糊音全关时，切分层不得有任何变化 —— 错音串照旧切不动。
///
/// 少了这条，「无条件把模糊拼写塞进 trie」也能让上面两条通过。
#[test]
fn fuzzy_spelling_layer_is_gated_by_config() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("tingzhi".to_string(), "停止".to_string(), 9000, 0);
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));

    let r = eng.convert("tinzhi", 10).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.text == "停止"),
        "没开 in_ing 就不该出「停止」，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 层级：精确词应优先于高频前缀补全词。"sikao"→"思考"(100,精确) 与 "sikaozhe"→"思考者"
/// (9000,补全)：输入 "sikao" 时「思考」必须排在「思考者」之前，即便后者词频高得多。
///
/// 刻意用 2 音节输入：单音节输入下 6.3 音节数闸门（[`STRICT_SYLLABLE_MATCH_MAX`]）让前缀补全
/// 整体不产出，那一侧由 `single_syllable_drops_longer_completions` 守卫。
#[test]
fn exact_ranks_above_prefix_completion() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("sikao".to_string(), "思考".to_string(), 100, 0);
    raw.merge_single("sikaozhe".to_string(), "思考者".to_string(), 9000, 0);
    let dict = CachedDict::Memory(raw);
    // 门槛设回 2：`sikao` 只有 2 音节，出厂的 4 会让「思考者」在召回层就没了。
    let eng = PinyinEngine::new(relaxed_completion_config(), dict);

    let r = eng.convert("sikao", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    let pos_kao = texts.iter().position(|t| *t == "思考");
    let pos_zhe = texts.iter().position(|t| *t == "思考者");
    assert!(pos_kao.is_some(), "精确「思考」应存在，实际: {texts:?}");
    assert!(pos_zhe.is_some(), "补全「思考者」应存在，实际: {texts:?}");
    assert!(
        pos_kao < pos_zhe,
        "精确「思考」应优先于高频前缀补全「思考者」，实际: {texts:?}"
    );
    assert!(
        !r.candidates[pos_kao.unwrap()].is_prefix,
        "「思考」应为精确(非前缀)"
    );
    assert!(
        r.candidates[pos_zhe.unwrap()].is_prefix,
        "「思考者」应为前缀补全"
    );
}

/// 6.3 音节数闸门：输入只表达 1 个音节时，音节数更多的前缀补全一律不产出。
///
/// `si` 只出单字「四」，不出 2 音节的「思考」——对齐主流拼音输入法（`dian` 不出「电话」）。
/// `merge_single` 造的词典 boundary 恒为 0，故本例同时覆盖 `effective_boundary` 的 DAG 兜底路径。
#[test]
fn single_syllable_drops_longer_completions() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("si".to_string(), "四".to_string(), 100, 0);
    raw.merge_single("sikao".to_string(), "思考".to_string(), 9000, 0);
    let dict = CachedDict::Memory(raw);
    let eng = PinyinEngine::new(Config::default(), dict);

    let texts: Vec<String> = eng
        .convert("si", 10)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert!(
        texts.iter().any(|t| t == "四"),
        "同音节数的精确候选必须保留，实际: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t == "思考"),
        "单音节输入下 2 音节补全词不该出现，实际: {texts:?}"
    );
}

/// 反向对照：多打一个字母（`sik`，残码使 used=2）后，同一个补全词必须回来。
/// 缺了这条，上面那个断言可以靠「把前缀补全整个废掉」来满足（`meiy`→「没有」会全灭）。
#[test]
fn trailing_partial_restores_completions() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("si".to_string(), "四".to_string(), 100, 0);
    raw.merge_single("sikao".to_string(), "思考".to_string(), 9000, 0);
    let dict = CachedDict::Memory(raw);
    let eng = PinyinEngine::new(Config::default(), dict);

    let texts: Vec<String> = eng
        .convert("sik", 10)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert!(
        texts.iter().any(|t| t == "思考"),
        "残码使输入占到 2 个音节，补全词必须回来，实际: {texts:?}"
    );
}

/// 完整匹配优先于子短语（对齐 Go coverage 分层）：输入完整音节 "nihao" 时，
/// 全长精确词「拟好」(code==nihao) 即便权重远低于子短语「你」(code=ni)，也应排在「你」之前。
/// 「你」只覆盖部分输入(ni)，是分段上屏候选(is_partial)，整体降到完整词之后；反过来的话
/// `baoan`→「报案」会被高频单字埋没。
#[test]
fn full_word_ranks_above_subphrase_singlechar() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("nihao".to_string(), "你好".to_string(), 200, 0);
    raw.merge_single("nihao".to_string(), "拟好".to_string(), 10, 1); // 罕见全长精确词
    raw.merge_single("ni".to_string(), "你".to_string(), 5000, 0); // 常用子短语
    let dict = CachedDict::Memory(raw);
    let eng = PinyinEngine::new(Config::default(), dict);

    let r = eng.convert("nihao", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    let pos_ni = texts.iter().position(|t| *t == "你");
    let pos_nihao_rare = texts.iter().position(|t| *t == "拟好");
    assert!(
        pos_ni.is_some() && pos_nihao_rare.is_some(),
        "候选缺失（子短语「你」仍应存在，供分段上屏）: {texts:?}"
    );
    assert!(
        pos_nihao_rare < pos_ni,
        "完整词「拟好」应优先于子短语单字「你」(对齐 Go coverage 分层)，实际: {texts:?}"
    );
    // 「你」是子短语(is_partial)，不是前缀补全(is_prefix)——分段上屏机制不受影响
    let ni = &r.candidates[pos_ni.unwrap()];
    assert!(!ni.is_prefix, "子短语「你」不应是前缀补全");
    assert!(ni.is_partial, "子短语「你」应标记 is_partial");
}

/// baoan 回归（用户报告场景）：输入 "baoan" 时，完整 bao'an 词「保安」「报案」必须
/// 聚集在前，不被高频子短语单字「报」(bao) 插开。修复前「报」(高权重) 会塞进
/// 「保安」「报案」之间，把「报案」挤到后面几页。
#[test]
fn baoan_full_words_group_above_subphrase() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("baoan".to_string(), "保安".to_string(), 3513, 0);
    raw.merge_single("baoan".to_string(), "报案".to_string(), 1374, 1);
    raw.merge_single("bao".to_string(), "报".to_string(), 9000, 0); // 高频单字，权重高于「报案」
    raw.merge_single("an".to_string(), "安".to_string(), 5000, 0);
    let dict = CachedDict::Memory(raw);
    let eng = PinyinEngine::new(Config::default(), dict);

    let r = eng.convert("baoan", 20).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    let pos = |t: &str| texts.iter().position(|x| *x == t);
    let p_baoan = pos("保安").expect("「保安」应存在");
    let p_baoan2 = pos("报案").expect("「报案」应存在");
    let p_bao = pos("报").expect("子短语「报」应存在（供分段上屏）");
    assert!(
        p_baoan < p_bao && p_baoan2 < p_bao,
        "完整词「保安」({p_baoan})「报案」({p_baoan2}) 都应排在高频子短语单字「报」({p_bao}) 之前，实际: {texts:?}"
    );
}

/// Fix B TDD：fuzzy 应接入多音节整串查询（fuzzy_codes 逐音节展开）。
/// 词典只存 eng 形式 "shengri"→"生日"，用户输入 en 形式 "shenri"（DAG 切分 shen+ri），
/// fuzzy en_eng=true 时应通过 fuzzy_codes 生成 "shengri" 反查命中「生日」。
#[test]
fn fuzzy_lookup_multi_syllable() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("shengri".to_string(), "生日".to_string(), 100, 0);
    let dict = CachedDict::Memory(raw);
    let fz = FuzzyConfig {
        en_eng: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), dict).with_fuzzy(fz);

    let r = eng.convert("shenri", 10).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "生日"),
        "fuzzy en_eng 开启时，\"shenri\" 应模糊命中「生日」(shengri)，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// Fix B TDD：fuzzy 全 false 时 lookup_with_fuzzy 退化为纯精确查找（不引入多余候选）。
#[test]
fn fuzzy_disabled_no_extra_candidates() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("shi".to_string(), "是".to_string(), 100, 0);
    let dict = CachedDict::Memory(raw);
    // 无 with_fuzzy → FuzzyConfig::default() 全 false
    let eng = PinyinEngine::new(Config::default(), dict);
    let r = eng.convert("si", 10).unwrap();
    assert!(
        !r.candidates.iter().any(|c| c.text == "是"),
        "fuzzy 关闭时 \"si\" 不应命中「是」，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 模糊命中的权重折扣：同一输入下，精确命中恒优先于同词频的模糊命中。
#[test]
fn fuzzy_penalty_keeps_exact_ahead_at_equal_weight() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("si".to_string(), "四".to_string(), 1000, 0);
    raw.merge_single("shi".to_string(), "是".to_string(), 1000, 1);
    let fz = FuzzyConfig {
        sh_s: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    let r = eng.convert("si", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    let pos_exact = texts.iter().position(|t| *t == "四").expect("「四」应存在");
    let pos_fuzzy = texts.iter().position(|t| *t == "是").expect("「是」应存在");
    assert!(
        pos_exact < pos_fuzzy,
        "同词频时精确命中须在模糊命中之前（折扣生效），实际: {texts:?}"
    );
}

/// 模糊命中不得被大量前缀补全挤出候选。
///
/// 别把 `is_fuzzy` 当 `cmp_match_layers` 的首要键：真实词库下 `si` 的前缀补全有 230 条，会把「是」
/// 顶到截断线外。此处用 40 条 `si*` 前缀补全模拟那堵墙、上限取 20——测试数据的规模本身就是
/// 被测条件的一部分，迷你词典测不出来。
#[test]
fn fuzzy_hit_survives_a_wall_of_prefix_completions() {
    let mut raw = CodetableDict::empty();
    // 一堵前缀补全的墙：码比输入长（is_prefix=true），非模糊，权重普通。
    for i in 0..40 {
        raw.merge_single(format!("si{i:02}"), format!("思{i:02}"), 500, i);
    }
    // 模糊命中：码 shi，经 s↔sh 由输入 si 召回；词频显著高于补全（真实词库中
    // 「是」正是高频字），折扣后仍应有竞争力。
    raw.merge_single("shi".to_string(), "是".to_string(), 900_000, 99);
    let fz = FuzzyConfig {
        sh_s: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    const LIMIT: usize = 20;
    let r = eng.convert("si", LIMIT).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    assert!(
        texts.iter().any(|t| *t == "是"),
        "模糊命中须能挤进前 {LIMIT} 条，不得被 40 条前缀补全整层压到截断线外，实际: {texts:?}"
    );
}

/// 多音节整词的模糊命中（step1 `lookup_with_fuzzy` 路径，逐音节 `fuzzy_codes`）：
/// `beijinsi` → 「北京市」(beijingshi) 需要第 2 音节 in→ing 且第 3 音节 s→sh。
/// 本例测不到 lattice 路径（step1 直接命中），那一路由 `fuzzy_non_initial_initial_via_lattice_sentence` 覆盖。
#[test]
fn fuzzy_hits_non_initial_syllables_via_lookup() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("beijingshi".to_string(), "北京市".to_string(), 5000, 0);
    let fz = FuzzyConfig {
        in_ing: true,
        sh_s: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    let r = eng.convert("beijinsi", 20).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "北京市"),
        "第 2、3 音节同时模糊时应命中「北京市」，实际: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// lattice 逐音节展开的专项守卫。
///
/// - 用非首音节的声母变体（`zou`→`zhou`，第 2 音节）：声母规则是 `starts_with`，整串调用只能改
///   首音节；韵母规则是 `find`，用它做判据会让整串调用也碰巧通过。
/// - 词典不含覆盖整串的词条，迫使候选只能由 Viterbi 多节点拼接产生，必经 lattice。
///
/// 把 lattice 改回对整串 `code` 求变体，本测试即挂。
#[test]
fn fuzzy_non_initial_initial_via_lattice_sentence() {
    let mut raw = CodetableDict::empty();
    // 覆盖前两音节的词（其码 zhongzhou 需由 zhong|zou 经第 2 音节 z→zh 得到）
    raw.merge_single("zhongzhou".to_string(), "中州".to_string(), 5000, 0);
    // 覆盖末音节的字，供 Viterbi 拼出整句
    raw.merge_single("ming".to_string(), "明".to_string(), 5000, 1);
    let fz = FuzzyConfig {
        zh_z: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    // zhong|zou|ming：词典无覆盖整串的词条 → 只能靠词图拼接
    let r = eng.convert("zhongzouming", 20).unwrap();
    // 必须断言整句（`is_sentence`），不能只断言「中州」出现：后者也会由 step3 的子短语查询命中，
    // 测不到 lattice。只有整句「中州明」需要「中州」先作为词图节点存在。
    let dump: Vec<String> = r
        .candidates
        .iter()
        .map(|c| format!("{}(sent={})", c.text, c.is_sentence))
        .collect();
    assert!(
        r.candidates
            .iter()
            .any(|c| c.is_sentence && c.text.contains("中州")),
        "第 2 音节 zou→zhou 须能进入词图，使 Viterbi 拼出整句「中州明」，实际: {dump:?}"
    );
}

/// 模糊命中的整句让位于精确整词（`is_sentence_demoted` 降级）。
#[test]
fn fuzzy_sentence_yields_to_exact_word() {
    let mut raw = CodetableDict::empty();
    // 精确整词（码 == 输入）
    raw.merge_single("sixiang".to_string(), "思想".to_string(), 26_000, 0);
    // 模糊命中的整词（码 shixiang，经 s↔sh 由 sixiang 召回）
    raw.merge_single("shixiang".to_string(), "是想".to_string(), 30_000, 1);
    let fz = FuzzyConfig {
        sh_s: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    let r = eng.convert("sixiang", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    let pos_exact = texts.iter().position(|t| *t == "思想");
    assert_eq!(
        pos_exact,
        Some(0),
        "存在精确整词时它必须居首，模糊整句让位，实际: {texts:?}"
    );
}

/// 反面：没有精确整词竞争时，模糊命中的整句照常居首——这正是模糊音要的效果
/// （`zongguo` → 「中国」）。与上一条共用同一判据，二者必须同时成立。
#[test]
fn fuzzy_sentence_leads_when_no_exact_word() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("zhongguo".to_string(), "中国".to_string(), 30_000, 0);
    // zongguo 下只有子短语单字，没有码 == zongguo 的精确整词
    raw.merge_single("zong".to_string(), "总".to_string(), 20_000, 1);
    let fz = FuzzyConfig {
        zh_z: true,
        ..Default::default()
    };
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw)).with_fuzzy(fz);

    let r = eng.convert("zongguo", 10).unwrap();
    let texts: Vec<&String> = r.candidates.iter().map(|c| &c.text).collect();
    assert_eq!(
        texts.first().map(|s| s.as_str()),
        Some("中国"),
        "无精确整词竞争时模糊整句应居首，实际: {texts:?}"
    );
}

/// Bug 复现：双拼下用户词（存储在 "pinyin" 共享 schema）应出现在候选中。
/// 小鹤双拼 "dabologe" → 全拼 "daboluoge"；store 中有该用户词时应能命中。
#[test]
fn shuangpin_store_user_word_appears_in_candidates() {
    let store = tmp_store("sp_userdict");
    store
        .add_user_word("pinyin", "daboluoge", "大菠萝哥", 0, 0)
        .unwrap();

    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));

    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let conv = ShuangpinConverter::new(layout);

    // 先确认转换正确：dabologe → daboluoge
    let sp_result = conv.convert("dabologe");
    assert_eq!(
        sp_result.full_pinyin(),
        "daboluoge",
        "小鹤双拼 dabologe 应转换为全拼 daboluoge，实际: {:?}",
        sp_result.full_pinyin()
    );

    let eng = empty_engine()
        .with_shuangpin(conv)
        .with_store_layers(Arc::new(dm));

    let r = eng.convert("dabologe", 20).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "大菠萝哥"),
        "双拼输入 \"dabologe\" 应命中用户词「大菠萝哥」，实际候选: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

// ── use_smart_compose 开关 ──────────────────────────────────────────────────

/// 构造带单字词典的拼音引擎（供整句/Viterbi 相关测试使用）：
/// 词典含 "ni"→"你"、"hao"→"好"，但无 "nihao"→"你好" 整词，任何 "你好" 候选只能来自整句路径。
/// 权重取 `cn_dicts` 的真实值（你 492791 / 好 124546），不要随手编：节点打分是
/// `ln(weight / DICT_TOTAL)`，闸门与惩罚常数都在真实权重分布上标定，编造的小值会让整句被质量闸门挡下。
fn engine_for_sentence_tests(config: Config) -> PinyinEngine {
    let mut raw = CodetableDict::empty();
    raw.merge_single("ni".to_string(), "你".to_string(), 492_791, 0);
    raw.merge_single("hao".to_string(), "好".to_string(), 124_546, 1);
    PinyinEngine::new(config, CachedDict::Memory(raw))
}

/// 判断候选是否为 Viterbi 合成整句（按来源标记，不看权重数值）。
fn is_viterbi_sentence(c: &Candidate) -> bool {
    c.is_sentence
}

// ── 词典整词被 Viterbi 选中时的同文合并 ─────────────────────────────────────

/// 词典整词恰好被 Viterbi 选为最优解时，经 step 2 的同文合并分支继承整句身份，
/// 且 weight 取 `max(词典 weight, W_eff)`。
///
/// 构造要点：给整词高权重、单字低权重，迫使 Viterbi 选「你好」这个单节点，从而走同文合并分支
/// （与 `demoted_sentence_falls_below_all_max_weight_exact_words` 的多节点合成正好相反）。
#[test]
fn dict_word_selected_by_viterbi_inherits_sentence_identity() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("ni".to_string(), "你".to_string(), 100, 0);
    raw.merge_single("hao".to_string(), "好".to_string(), 100, 1);
    raw.merge_single("nihao".to_string(), "你好".to_string(), 50_000, 2);
    raw.merge_single("nihao".to_string(), "拟好".to_string(), 200, 3);
    let e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
    let r = e.convert("nihao", 50).unwrap();

    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("候选中应有「你好」");
    assert!(c.is_sentence, "词典整词被 Viterbi 选中 → 继承整句身份");
    assert!(
        !c.is_sentence_demoted,
        "它本身即精确整词，无处可让，不该走 6.5 降级"
    );
    // 同文合并不得置 `is_synthesized`：这是词库里实打实的词条，只是被 Viterbi 选中。协调器的
    // 自动造词以该字段为准，置真的话打 `nihao` 选「你好」就会把系统词抄进临时词库
    // （见 `is_sentence` 字段文档）。
    assert!(
        !c.is_synthesized,
        "词库已有此词条 ⇒ 不是引擎新合成，不得置 is_synthesized"
    );
    // 同文合并取 `max(词典 weight, W_eff)`，50_000 是构造里「你好」的词典值。
    assert!(
        c.weight >= 50_000,
        "同文合并不得压低词典权重，实际 {}",
        c.weight
    );
    assert_eq!(r.candidates[0].text, "你好", "无词频记录时整句居首");
}

/// 对照组：同码没有别的精确整词时，整句身份照样成立。
/// 与上一条合看，说明「继承整句身份」取决于 Viterbi 选没选中它，与同码有无竞争者无关。
#[test]
fn dict_word_sentence_without_peer_still_is_sentence() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("ni".to_string(), "你".to_string(), 100, 0);
    raw.merge_single("hao".to_string(), "好".to_string(), 100, 1);
    raw.merge_single("nihao".to_string(), "你好".to_string(), 50_000, 2);
    let e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
    let r = e.convert("nihao", 50).unwrap();

    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("候选中应有「你好」");
    assert!(c.is_sentence, "仍是整句");
    assert!(c.weight >= 50_000, "同文合并同样取 max，实际 {}", c.weight);
}

// ── 整句让位于精确整词（step 6.5 降级）─────────────────────────────────────

fn nbest_engine(raw: CodetableDict) -> PinyinEngine {
    let mut e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
    e.config.sentence_count = 2;
    e.config.sentence_max_count = 2;
    e
}

fn nihao_nbest_dict() -> CodetableDict {
    let mut raw = CodetableDict::empty();
    raw.merge_single("ni".to_string(), "你".to_string(), 492_791, 0);
    raw.merge_single("ni".to_string(), "泥".to_string(), 30_000, 1);
    raw.merge_single("hao".to_string(), "好".to_string(), 124_546, 2);
    raw
}

/// N-best 下合成的第 2 名整句不算「精确整词」：第 1 名不得因它降级而名次倒挂。
#[test]
fn nbest_alternate_sentence_does_not_demote_top_sentence() {
    let e = nbest_engine(nihao_nbest_dict());
    let r = e.convert("nihao", 50).unwrap();
    let get = |rank: u8| {
        r.candidates
            .iter()
            .find(|c| c.is_sentence && c.sentence_rank == rank)
            .unwrap_or_else(|| panic!("缺第 {rank} 名整句: {:?}", r.candidates))
    };
    let (top, alt) = (get(1), get(2));
    assert_eq!((top.text.as_str(), alt.text.as_str()), ("你好", "泥好"));
    assert!(
        top.is_synthesized && alt.is_synthesized,
        "两条都应是合成整句"
    );
    assert!(
        !top.is_sentence_demoted,
        "第 2 名整句不是词条，不得让第 1 名降级"
    );
    assert!(
        top.weight > alt.weight,
        "第 1 名权重须高于第 2 名: {} vs {}",
        top.weight,
        alt.weight
    );
}

/// 学过的整句成了整串单节点后，第 2 名路径常是它的拆分版（同文）；
/// 备选名额不得被同文路径占掉。
#[test]
fn nbest_alternate_survives_after_top_sentence_is_learned() {
    let mut raw = CodetableDict::empty();
    raw.merge_single("xian".to_string(), "先".to_string(), 300_000, 0);
    raw.merge_single("xian".to_string(), "西安".to_string(), 60_000, 1);
    raw.merge_single("chuan".to_string(), "穿".to_string(), 30_000, 2);
    raw.merge_single("wazi".to_string(), "袜子".to_string(), 5_000, 3);
    raw.merge_single("chuanwazi".to_string(), "穿袜子".to_string(), 50, 4);
    let store = tmp_store("nbest_learned_top");
    // xi|an|chuan|wa|zi → 0/2/4/9/11
    store
        .learn_temp_word(
            "pinyin",
            "xianchuanwazi",
            "西安穿袜子",
            800,
            0b1010_0001_0101,
        )
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(store, "pinyin")));
    let e = nbest_engine(raw).with_store_layers(Arc::new(dm));
    assert_eq!(
        sentence_ranked(&e, "xianchuanwazi", 1).as_deref(),
        Some("西安穿袜子")
    );
    assert_eq!(
        sentence_ranked(&e, "xianchuanwazi", 2).as_deref(),
        Some("先穿袜子"),
        "第 2 名不得被「西安穿袜子」的拆分路径占掉"
    );
}

/// 对照：第 2 名与用户词同文时它是词条，第 1 名照旧让位（同只出 1 条时）。
/// 关掉整句用用户词：开着时用户词自己进图拿第 1，走不到这条例外。
#[test]
fn nbest_alternate_merged_with_user_word_still_demotes_top_sentence() {
    let store = tmp_store("nbest_alt_user");
    store
        .add_user_word("pinyin", "nihao", "泥好", 500, 0b101) // ni|hao → 0/2
        .unwrap();
    let dm = DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(store, "pinyin")));
    let mut e = nbest_engine(nihao_nbest_dict()).with_store_layers(Arc::new(dm));
    e.config.sentence_uses_user_words = false;
    let r = e.convert("nihao", 50).unwrap();
    let top = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .unwrap_or_else(|| panic!("缺「你好」: {:?}", r.candidates));
    let user = r.candidates.iter().find(|c| c.text == "泥好").unwrap();
    assert!(user.meta.is_user_dict, "「泥好」应带用户词标记");
    assert!(top.is_sentence_demoted, "存在精确用户词时第 1 名整句须让位");
    assert!(top.weight < user.weight);
}

/// 多个精确整词并列于最大权重时，整句排在它们全部之后。
///
/// `max - 1` 在算术上蕴含它，但并列走的是 `better`/`candidate_display_order` 的后续键
/// （base_order / natural_order），要真跑一遍才知道整句没混进并列组里。
#[test]
fn demoted_sentence_falls_below_all_max_weight_exact_words() {
    let mut raw = CodetableDict::empty();
    // 单字给高权重，确保 Viterbi 选「你+好」而非把某个 nihao 词条当单节点整句（那样走同文合并，
    // 不触发降级）。权重取真实量级：整句是概率连乘，编造值会让 Viterbi 直接选同码词、整句不产生。
    raw.merge_single("ni".to_string(), "你".to_string(), 492_791, 0);
    raw.merge_single("hao".to_string(), "好".to_string(), 124_546, 1);
    // 三个精确整词，权重并列且同为最大
    raw.merge_single("nihao".to_string(), "拟好".to_string(), 64, 2);
    raw.merge_single("nihao".to_string(), "泥好".to_string(), 64, 3);
    raw.merge_single("nihao".to_string(), "尼好".to_string(), 64, 4);
    let e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
    let r = e.convert("nihao", 50).unwrap();

    let pos = |t: &str| {
        r.candidates
            .iter()
            .position(|c| c.text == t)
            .unwrap_or_else(|| {
                panic!(
                    "候选中找不到 {t}，实际: {:?}",
                    r.candidates
                        .iter()
                        .map(|c| (&c.text, c.weight))
                        .collect::<Vec<_>>()
                )
            })
    };
    let sent = pos("你好");
    let sc = &r.candidates[sent];
    assert!(sc.is_sentence, "「你好」应是合成整句");
    assert!(sc.is_sentence_demoted, "存在精确整词时整句须降级");
    // 正向对照（与 `dict_word_selected_by_viterbi_inherits_sentence_identity` 的反向断言配对）：
    // 这里的「你好」由两个节点拼出、词库里没有，必须置 `is_synthesized`——否则引擎不再置该字段
    // （自动造词整个失效）时反向断言照样全绿。
    assert!(
        sc.is_synthesized,
        "多节点合成、词库无此词条 ⇒ 必须置 is_synthesized，否则自动造词全线失效"
    );
    // 64 = 上面三个同码精确整词的权重（取自 cn_dicts 的「拟好」真实值）。
    // 写成 `64 - 1` 而非硬编码 63：本断言要守的是「降到 max(同码词) - 1」这个关系，
    // 跟着构造走；写死数值会在下次调整夹具权重时变成一个无从追溯的魔数。
    assert_eq!(sc.weight, 64 - 1, "权重须为 max(同码精确整词) - 1");
    for w in ["拟好", "泥好", "尼好"] {
        assert!(
            pos(w) < sent,
            "并列于 max 的精确整词「{w}」(rank {}) 须排在整句(rank {sent})之前，实际: {:?}",
            pos(w),
            r.candidates
                .iter()
                .map(|c| (&c.text, c.weight))
                .collect::<Vec<_>>()
        );
    }
}

/// 降级整句仍须在普通候选之前，无论后者权重多高。
/// 位置靠 `cmp_match_layers` 的层级键保证而非权重数值，防 `max - 1` 的权重并列风险。
#[test]
fn demoted_sentence_still_precedes_ordinary_candidates() {
    let mut raw = CodetableDict::empty();
    // 单字与「拟好」取 cn_dicts 真实权重：整句是各词概率连乘，编造的 (你 1e5, 好 1e5, 拟好 5000)
    // 下 P(拟好) 远高于 P(你)·P(好)，Viterbi 直接选「拟好」、不产生「你好」整句；
    // 真实权重下 P(你)·P(好)=1.05e-6 > P(拟好)=2.6e-7，整句成立。
    raw.merge_single("ni".to_string(), "你".to_string(), 492_791, 0);
    raw.merge_single("hao".to_string(), "好".to_string(), 124_546, 1);
    raw.merge_single("nihao".to_string(), "拟好".to_string(), 64, 2);
    // 前缀补全（码比输入长）：权重顶到 2e9，仍应留在整句之后
    raw.merge_single(
        "nihaoma".to_string(),
        "你好吗".to_string(),
        2_000_000_000,
        3,
    );
    let e = PinyinEngine::new(Config::default(), CachedDict::Memory(raw));
    let r = e.convert("nihao", 50).unwrap();

    let sent = r.candidates.iter().position(|c| c.text == "你好").unwrap();
    assert!(r.candidates[sent].is_sentence_demoted, "整句须已降级");
    // 整句之前只允许出现精确整词（码 == 输入且不在下层）
    for (i, c) in r.candidates.iter().enumerate().take(sent) {
        assert!(
            c.code == "nihao" && !c.is_fuzzy && !c.is_prefix && !c.is_partial,
            "整句(rank {sent})之前只应有精确整词，却出现 {}(rank {i}, w={}, code={})",
            c.text,
            c.weight,
            c.code
        );
    }
}

/// TDD：use_smart_compose=false 时多音节输入不产生 Viterbi 合成整句候选。
#[test]
fn smart_compose_off_skips_viterbi_sentence() {
    let e = engine_for_sentence_tests(Config {
        use_smart_compose: false,
        ..Config::default()
    });
    let r = e.convert("nihao", 50).unwrap();
    assert!(
        !r.candidates.iter().any(|c| {
            c.text.chars().count() >= 2
                && c.source == CandidateSource::Pinyin
                && is_viterbi_sentence(c)
        }),
        "关闭智能组句后不应有 Viterbi 合成整句，实际候选: {:?}",
        r.candidates
            .iter()
            .map(|c| (&c.text, c.weight))
            .collect::<Vec<_>>()
    );
}

/// 回归：use_smart_compose=true（默认）时整句候选仍产生。
#[test]
fn smart_compose_on_produces_viterbi_sentence() {
    let e = engine_for_sentence_tests(Config::default()); // use_smart_compose 默认 true
    let r = e.convert("nihao", 50).unwrap();
    assert!(
        r.candidates.iter().any(|c| {
            c.text.chars().count() >= 2
                && c.source == CandidateSource::Pinyin
                && is_viterbi_sentence(c)
        }),
        "启用智能组句时应有 Viterbi 合成整句，实际候选: {:?}",
        r.candidates
            .iter()
            .map(|c| (&c.text, c.weight))
            .collect::<Vec<_>>()
    );
}

/// Task 4.1 TDD Step 1：双拼端到端——装配小鹤双拼 converter 后，
/// 输入双拼键 "ni" 应返回含「你」的候选。
#[test]
fn pinyin_engine_shuangpin_input() {
    // 构造含 "ni"->"你" 的最小词典
    let mut raw = CodetableDict::empty();
    raw.merge_single("ni".to_string(), "你".to_string(), 100, 0);
    let dict = CachedDict::Memory(raw);

    // 小鹤双拼：ni → ni（声母 n + 韵母 i=i，即全拼 "ni"，保持不变）
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let conv = ShuangpinConverter::new(layout);

    let eng = PinyinEngine::new(Config::default(), dict).with_shuangpin(conv);
    let r = eng.convert("ni", 10).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "你"),
        "双拼输入 \"ni\" 经转换后应返回含「你」的候选，实际候选: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 探针（临时）：双拼下手动分隔符。
#[test]
fn probe_shuangpin_separator_current() {
    let eng = sp_fp_engine(
        "probe_sep",
        &[
            ("你好", "ni hao", 200),
            ("你", "ni", 100),
            ("西安", "xi an", 150),
            ("好", "hao", 90),
        ],
        false,
    );
    for input in [
        "nhc", "n'hc", "nihc", "xiaj", "xi'aj", "ni'", "'ni", "n''hc",
    ] {
        let sp = eng.shuangpin.as_ref().unwrap().convert(input);
        let r = eng.convert(input, 10).unwrap();
        println!(
            "IN {:?} | full={:?} pre={:?} partial={:?} syl={:?}",
            input,
            sp.full_pinyin,
            sp.preedit_display,
            sp.partial_initial,
            sp.syllables
                .iter()
                .map(|s| (
                    s.pinyin.clone(),
                    s.raw_start,
                    s.raw_end,
                    s.fp_start,
                    s.fp_end
                ))
                .collect::<Vec<_>>(),
        );
        println!(
            "   -> eng.preedit={:?} disp={:?} cands={:?}",
            r.preedit_pinyin,
            r.preedit_display,
            r.candidates
                .iter()
                .take(4)
                .map(|c| (c.text.clone(), c.consumed_length))
                .collect::<Vec<_>>()
        );
    }
}

// ── 模糊音提示（`meta.pinyin_hints`）：引擎各路径按 learn_code 与用户音节对齐填提示。──

fn hint_cfg() -> FuzzyConfig {
    FuzzyConfig {
        in_ing: true,
        n_l: true,
        hint: fuzzy::HintSwitch::new(true),
        ..Default::default()
    }
}

fn hint_engine(fz: FuzzyConfig) -> PinyinEngine {
    engine_with_words(&[
        ("pingguo", "苹果"),
        ("pinguo", "拼过"),
        ("xinqing", "心情"),
        ("xingqing", "性情"),
        ("nan", "南"),
        ("lan", "蓝"),
        ("ping", "平"),
        ("lve", "略"),
    ])
    .with_fuzzy(fz)
}

fn fh(i: u16, s: &str) -> wind_candidate::PinyinHint {
    wind_candidate::PinyinHint {
        char_index: i,
        correct: s.into(),
        kind: wind_candidate::HintKind::Fuzzy,
    }
}

fn hints_of<'a>(r: &'a ConvertResult, text: &str) -> &'a [wind_candidate::PinyinHint] {
    &r.candidates
        .iter()
        .find(|c| c.text == text)
        .unwrap_or_else(|| panic!("应有候选「{text}」，实际: {:?}", texts(r)))
        .meta
        .pinyin_hints
}

#[test]
fn hint_fills_fuzzy_word() {
    let r = hint_engine(hint_cfg()).convert("pinguo", 20).unwrap();
    assert_eq!(hints_of(&r, "苹果"), &[fh(0, "ping")]);
    assert!(hints_of(&r, "拼过").is_empty(), "精确命中不标");
}

/// 整词正确音节随提示一并给出；词条码无边界（`boundary == 0`）时按 DAG 现切。
#[test]
fn hint_fills_word_syllables() {
    let r = hint_engine(hint_cfg()).convert("pinguo", 20).unwrap();
    let c = r.candidates.iter().find(|c| c.text == "苹果").unwrap();
    assert_eq!(
        c.meta.learn_code.as_ref().map(|l| l.1),
        Some(0),
        "前提：词条码无边界"
    );
    assert_eq!(c.meta.hint_syllables, ["ping", "guo"]);
    let exact = r.candidates.iter().find(|c| c.text == "拼过").unwrap();
    assert!(exact.meta.hint_syllables.is_empty(), "无提示不填");
}

/// 开关是共享的：同一引擎实例上翻转即生效，不必重建。
#[test]
fn hint_switch_flips_without_rebuild() {
    let fz = hint_cfg();
    let switch = fz.hint.clone();
    let eng = hint_engine(fz);
    switch.set(false);
    let r = eng.convert("pinguo", 20).unwrap();
    assert!(r.candidates.iter().all(|c| c.meta.pinyin_hints.is_empty()));
    switch.set(true);
    let r = eng.convert("pinguo", 20).unwrap();
    assert_eq!(hints_of(&r, "苹果"), &[fh(0, "ping")]);
}

#[test]
fn hint_off_leaves_empty() {
    let mut fz = hint_cfg();
    fz.hint = fuzzy::HintSwitch::new(false);
    let r = hint_engine(fz).convert("pinguo", 20).unwrap();
    assert!(r.candidates.iter().any(|c| c.text == "苹果"));
    assert!(r.candidates.iter().all(|c| c.meta.pinyin_hints.is_empty()));
}

#[test]
fn hint_skips_trailing_prefix() {
    let r = hint_engine(hint_cfg()).convert("pin", 20).unwrap();
    assert!(
        r.candidates.iter().any(|c| c.text == "平"),
        "{:?}",
        texts(&r)
    );
    assert!(
        r.candidates.iter().all(|c| c.meta.pinyin_hints.is_empty()),
        "末音节可能没打完，不标: {:?}",
        r.candidates
            .iter()
            .map(|c| (&c.text, &c.meta.pinyin_hints))
            .collect::<Vec<_>>()
    );
    // 对照：末音节是完整的另一读音（`lan` → 南）照标。
    let r = hint_engine(hint_cfg()).convert("lan", 20).unwrap();
    assert_eq!(hints_of(&r, "南"), &[fh(0, "nan")]);
    assert!(hints_of(&r, "蓝").is_empty());
}

/// fill 层的规范化：各路径当前交来的码已是规范形态、端到端构造不出（见
/// `normalized_typed` 文档），故直接验 helper；端到端只验入口归一化接上了提示。
#[test]
fn normalized_typed_strips_separator_and_umlaut() {
    assert_eq!(normalized_typed("pin'guo"), "pinguo");
    assert_eq!(normalized_typed("lue'lai"), "lvelai");
    assert_eq!(normalized_typed("nihao"), "nihao");
}

#[test]
fn hint_normalizes_typed_spelling() {
    // `nue` 经 ü 归一化成 `nve`，与词库 `lve` 在 n_l 下对齐。
    let r = hint_engine(hint_cfg()).convert("nue", 20).unwrap();
    assert_eq!(hints_of(&r, "略"), &[fh(0, "lve")]);
    // 手动分隔符不进对齐串。
    let r = hint_engine(hint_cfg()).convert("pin'guo", 20).unwrap();
    assert_eq!(hints_of(&r, "苹果"), &[fh(0, "ping")]);
}

#[test]
fn hint_fills_sentence() {
    let r = hint_engine(hint_cfg()).convert("pinguolan", 20).unwrap();
    let s = r
        .candidates
        .iter()
        .find(|c| c.is_sentence && c.text == "苹果蓝")
        .unwrap_or_else(|| panic!("应组出整句「苹果蓝」: {:?}", texts(&r)));
    assert_eq!(s.meta.pinyin_hints, vec![fh(0, "ping")], "只标模糊段");
}

#[test]
fn hint_fills_user_store_word() {
    let eng = engine_with_user_words_fuzzy(
        "fz_hint_user",
        // ping|guo：位 0、4。
        &[("pingguo", "苹果", 0b10001)],
        hint_cfg(),
    );
    let r = eng.convert("pinguo", 20).unwrap();
    assert_eq!(hints_of(&r, "苹果"), &[fh(0, "ping")]);
}

/// 用户词码无边界（`boundary == 0`）：整词音节按 DAG 现切，与提示同源。
#[test]
fn hint_user_word_without_boundary_fills_syllables() {
    let eng =
        engine_with_user_words_fuzzy("fz_hint_user_b0", &[("pingguo", "苹果", 0)], hint_cfg());
    let r = eng.convert("pinguo", 20).unwrap();
    let c = r.candidates.iter().find(|c| c.text == "苹果").unwrap();
    assert_eq!(c.meta.pinyin_hints, [fh(0, "ping")]);
    assert_eq!(c.meta.hint_syllables, ["ping", "guo"]);
}

fn sp_hint_engine(layout: &str, words: &[(&str, &str)], fz: FuzzyConfig) -> PinyinEngine {
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join(layout)).unwrap();
    let mut conv = ShuangpinConverter::new(layout);
    conv.set_fuzzy(&fz);
    engine_with_words(words).with_shuangpin(conv).with_fuzzy(fz)
}

/// 小鹤 `sx`：`s` + ia/ua 都不成音节，转换器取对偶声母读成 `shua`——用户本义是 `sua`。
#[test]
fn shuangpin_partner_marked() {
    let fz = FuzzyConfig {
        sh_s: true,
        hint: fuzzy::HintSwitch::new(true),
        ..Default::default()
    };
    let eng = sp_hint_engine("xiaohe.toml", &[("shua", "刷")], fz.clone());
    let r = eng.convert("sx", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "刷")
        .expect("sx 应出「刷」");
    assert!(!c.is_fuzzy, "只填提示，不改 is_fuzzy（排序不受 hint 影响）");
    assert_eq!(c.meta.pinyin_hints, vec![fh(0, "shua")]);
    // 对照：按 sh 键打（`ux`）是正读，不标。
    let r = eng.convert("ux", 20).unwrap();
    assert!(hints_of(&r, "刷").is_empty());
    // hint 关：不标。
    let off = FuzzyConfig {
        hint: fuzzy::HintSwitch::new(false),
        ..fz
    };
    let r = sp_hint_engine("xiaohe.toml", &[("shua", "刷")], off)
        .convert("sx", 20)
        .unwrap();
    let c = r.candidates.iter().find(|c| c.text == "刷").unwrap();
    assert!(!c.is_fuzzy && c.meta.pinyin_hints.is_empty());
}

/// hint 开关只影响显示：同一双拼对偶输入，开与关的候选顺序与模糊标记逐条一致。
#[test]
fn shuangpin_partner_hint_keeps_order() {
    let words = [("shua", "刷"), ("sa", "撒"), ("shuai", "帅"), ("su", "苏")];
    let run = |hint: bool| {
        let fz = FuzzyConfig {
            sh_s: true,
            hint: fuzzy::HintSwitch::new(hint),
            ..Default::default()
        };
        sp_hint_engine("xiaohe.toml", &words, fz)
            .convert("sxsk", 20)
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| (c.text, c.is_fuzzy, c.weight))
            .collect::<Vec<_>>()
    };
    let on = run(true);
    assert!(on.iter().any(|(t, ..)| t == "刷"), "{on:?}");
    assert_eq!(on, run(false));
}

/// 自然码 `dn` 无合法读音，走错音串垫底读成 `din`，模糊命中「丁 ding」：照标
/// （双拼两键即一个完整音节，不存在「末音节没打完」）。
#[test]
fn shuangpin_wrong_spelling_marked() {
    let fz = FuzzyConfig {
        in_ing: true,
        hint: fuzzy::HintSwitch::new(true),
        ..Default::default()
    };
    let eng = sp_hint_engine("ziranma.toml", &[("ding", "丁")], fz);
    let r = eng.convert("dn", 20).unwrap();
    assert_eq!(hints_of(&r, "丁"), &[fh(0, "ding")]);
}

/// 双拼开了「全拼也能打」：全拼降级支路召回的模糊命中同样带提示，末尾判据对整串击键
/// （`raw_input.len()`）——`xinqing` 的非末音节 xin 照标，`qingxin` 的末音节 xin 可能只是
/// xing 没打完，不标。
#[test]
fn full_pinyin_fallback_hint_uses_raw_input_for_trailing() {
    use std::io::Write;
    let path = std::env::temp_dir().join("wind_fp_fallback_hint.dict.yaml");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        writeln!(f, "性情\txing qing\t300").unwrap();
        writeln!(f, "情性\tqing xing\t300").unwrap();
    }
    let dict = CachedDict::Memory(CodetableDict::load(&path).unwrap());
    let _ = std::fs::remove_file(&path);
    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).unwrap();
    let fz = FuzzyConfig {
        in_ing: true,
        hint: fuzzy::HintSwitch::new(true),
        ..Default::default()
    };
    let cfg = Config {
        allow_full_pinyin: true,
        ..Config::default()
    };
    let mut conv = ShuangpinConverter::new(layout);
    conv.set_fuzzy(&fz);
    let eng = PinyinEngine::new(cfg, dict)
        .with_shuangpin(conv)
        .with_fuzzy(fz);

    let r = eng.convert("xinqing", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "性情")
        .unwrap_or_else(|| panic!("{:?}", texts(&r)));
    assert!(c.is_fullpinyin_fallback, "前提：来自全拼降级支路");
    assert_eq!(c.meta.pinyin_hints, vec![fh(0, "xing")]);

    let r = eng.convert("qingxin", 20).unwrap();
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "情性")
        .unwrap_or_else(|| panic!("{:?}", texts(&r)));
    assert!(c.is_fullpinyin_fallback, "前提：来自全拼降级支路");
    assert!(
        c.meta.pinyin_hints.is_empty(),
        "末音节 xin 可能没打完，不标"
    );
}

/// 双拼同键音节（自定义布局 `q = ["iu", "ui"]`）不算打错，不标。
#[test]
fn shuangpin_shared_key_not_marked() {
    let layout_toml = r#"
[meta]
id = "shared"
name = "同键测试"
[finals]
q = ["iu", "ui"]
"#;
    let mut raw = CodetableDict::empty();
    raw.merge_single("diu".to_string(), "丢".to_string(), 100, 0);
    raw.merge_single("dui".to_string(), "对".to_string(), 300, 1);
    let fz = FuzzyConfig {
        hint: fuzzy::HintSwitch::new(true),
        ..Default::default()
    };
    let mut conv = ShuangpinConverter::new(Layout::from_toml_str(layout_toml).unwrap());
    conv.set_fuzzy(&fz);
    let eng = PinyinEngine::new(Config::default(), CachedDict::Memory(raw))
        .with_shuangpin(conv)
        .with_fuzzy(fz);
    let r = eng.convert("dq", 10).unwrap();
    assert!(hints_of(&r, "对").is_empty());
    assert!(hints_of(&r, "丢").is_empty());
}
