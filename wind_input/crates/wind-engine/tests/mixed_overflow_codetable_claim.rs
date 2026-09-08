//! 混输超码长归属：`pinyin_only_overflow`（⓪）的例外口 `codetable_owns_overflow`。
//!
//! 真机现象：五笔混输下 `yijg`（唯一全码「就是」）**再打任何一个字母**，候选里「就是」整条
//! 消失、只剩拼音「以」——且把上屏否决开关（①`auto_commit_block_on_pinyin` /
//! ②`block_commit_on_pinyin_word` / ③`auto_commit_block_on_english`）全关也无济于事。
//!
//! 根因是超码长走了另一套逻辑，由 ⓪ 独占裁决，卡死两个出口：
//! - `handle_top_code` 的 ⓪ 判据只看「拼音有没有候选」，不读任何否决开关；
//! - `convert_overflow` 里唯一能捞回码表候选的逃生口 `has_full_input_match(input) ||
//!   has_longer_code(input)` 问的是**整串**，而五笔 4 码封顶、不存在 5 码词条 ⇒ 恒假。
//!
//! 修法：两处共用 `codetable_owns_overflow` = 「前 N 码是精确全码」且「拼音主张不了整串」。
//!
//! ⚠️ 全部用例走**真 `PinyinEngine`**（不用 fake）：本判据的分水岭是
//! `is_possible_pinyin_sequence` 与候选 `consumed_length` 的真实取值，fake 引擎给什么就是什么，
//! 用 fake 锁不住真机行为（`youyo` 在 fake 下靠「消费整串」偶然通过，真机靠的是「还没打完」）。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::codetable::CodetableDict;
use wind_dict::{DictManager, SystemDictLayer};
use wind_engine::codetable::{CodeTableEngine, CommitOptions};
use wind_engine::english::EnglishEngine;
use wind_engine::mixed::{MixConfig, MixedEngine};
use wind_engine::pinyin::Config as PinyinConfig;
use wind_engine::{Engine, PinyinEngine};

/// 五笔侧（4 码封顶）：`yijg`=就是（本例主角）、`youy`=变凉（youyoud 回归对照）、
/// `gith`=不算（github 回归对照，取自真词库 `wubi86_jidian.dict.yaml` 的真实条目与权重）。
/// 三者都是**精确全码**，故「前缀是精确全码」这一条对所有用例同时成立 —— 变量只剩
/// 「拼音/英文主张不主张整串」。
fn wubi() -> Box<dyn Engine> {
    let mut d = CodetableDict::empty();
    d.merge_single("yijg".into(), "就是".into(), 5000, 0);
    d.merge_single("youy".into(), "变凉".into(), 864, 1);
    d.merge_single("gith".into(), "不算".into(), 1822, 2);
    // `word`：英文用例主角。选它是因为 `words` 的拼音**交得出候选**（`wo`→我）却**主张不了整串**
    // （`rds` 不成音节），三条判据里只有「英文主张」那条会否决 —— 唯一变量得以隔离。
    d.merge_single("word".into(), "叉".into(), 900, 3);
    // `cety`＝真机主角「通往」（`wubi86_jidian.dict.yaml:7013`，全库唯一该码）。
    // 选它是因为拼音侧 `ce` 是合法音节、`ty` 连音节前缀都不是 —— 与 `yijg` 同侧但更贴真机。
    d.merge_single("cety".into(), "通往".into(), 1442, 4);
    // `wang`：② 的隔离对照专用。它同时满足「前 N 码是精确全码」（例外口成立）与
    // 「前 N 码恰是 1 个完整拼音音节」（② 的判据 b 命中），是唯一能把 ② 单独测出来的形状。
    d.merge_single("wang".into(), "王".into(), 900, 5);
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "sys")));
    Box::new(CodeTableEngine::new(
        4,
        CommitOptions {
            top_code_commit: true,
            ..Default::default()
        },
        Arc::new(dm),
    ))
}

fn pinyin() -> PinyinEngine {
    let mut d = CodetableDict::empty();
    d.merge_single("yi".into(), "以".into(), 9000, 0);
    d.merge_single("you".into(), "悠".into(), 9000, 1);
    d.merge_single("ni".into(), "你".into(), 9000, 2);
    d.merge_single("wo".into(), "我".into(), 9000, 3);
    d.merge_single("ce".into(), "策".into(), 9000, 4);
    d.merge_single("wang".into(), "网".into(), 9000, 5);
    PinyinEngine::new(PinyinConfig::default(), CachedDict::Memory(d))
}

/// 「否决选项全关」＝ ①②③ 全关，⓪ 保持出厂 `true`（用户设置页里关得掉的就是 ①②③）。
fn all_vetoes_off() -> MixConfig {
    MixConfig {
        auto_commit_block_on_pinyin: false,
        block_commit_on_pinyin_word: false,
        auto_commit_block_on_english: false,
        pinyin_only_overflow: true,
        top_code_override_pinyin: false,
        ..Default::default()
    }
}

/// 英文侧（独立 `english` 方案，`type = "english"` 词库 → `CandidateSource::English`）：
/// - `words` 是**精确整串**词条（第三条判据的正例，选它是为了隔离变量，见该用例文档）；
/// - `yijgatron` 只让 `yijga` 命中**前缀**（反向锁：前缀不足以夺走归属）；
/// - `github` 供顶码分工用例（③ 开/关的对照）。
fn english() -> Box<dyn Engine> {
    let mut d = CodetableDict::empty();
    d.merge_single("words".into(), "words".into(), 500, 0);
    d.merge_single("wordsmith".into(), "wordsmith".into(), 100, 1);
    d.merge_single("yijgatron".into(), "Yijgatron".into(), 300, 2);
    d.merge_single("github".into(), "GitHub".into(), 500, 3);
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(CachedDict::Memory(d), "en")));
    let ct = CodeTableEngine::new(32, CommitOptions::default(), Arc::new(dm));
    Box::new(EnglishEngine::new(ct))
}

fn mixed(cfg: MixConfig) -> MixedEngine {
    MixedEngine::new(wubi(), Some(Box::new(pinyin())), None, cfg)
}

/// 带英文子引擎的混输（`schema.mix.enable_english` 开）。
///
/// ⚠️ 本文件其余用例一律 `english=None` —— 英文维度整个缺席，正是 `github` 回归漏网的原因。
fn mixed_en(cfg: MixConfig) -> MixedEngine {
    MixedEngine::new(wubi(), Some(Box::new(pinyin())), Some(english()), cfg)
}

fn texts(e: &MixedEngine, input: &str) -> Vec<String> {
    e.convert(input, 20)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

/// 前置事实：`yijg` 本身（未超码长）一切正常——五笔精确全码排第一。
/// 锁住「问题只发生在第 5 键」，免得后续有人误改等长合并分支。
#[test]
fn full_code_itself_is_unaffected() {
    let e = mixed(all_vetoes_off());
    assert_eq!(
        texts(&e, "yijg").first().map(String::as_str),
        Some("就是"),
        "4 码满码时五笔精确全码本就该排第一"
    );
}

/// 核心诉求：`yijg` + 字母 → 顶码放行，候选里「就是」回来且排第一。
#[test]
fn codetable_reclaims_overflow_when_pinyin_cannot_explain() {
    // 前提：本例确实落在「拼音主张不了整串」那一侧（否则下面测的是别的分支）。
    let py = pinyin();
    assert!(
        !py.is_possible_pinyin_sequence("yijga"),
        "前置：jg 不是音节前缀 ⇒ 拼音没在打这串"
    );
    assert_eq!(
        py.convert("yijga", 1).unwrap().candidates[0].consumed_length,
        2,
        "前置：拼音首选只解释 yi（2/5），够不上接管整串"
    );

    let e = mixed(all_vetoes_off());

    assert_eq!(
        e.handle_top_code("yijga"),
        Some(("就是".to_string(), "a".to_string())),
        "前 N 码是精确全码且拼音只解释得了 yi → ⓪ 不得再拦顶码"
    );

    let c = texts(&e, "yijga");
    assert_eq!(
        c.first().map(String::as_str),
        Some("就是"),
        "顶码关闭时也须由候选兜住：「就是」应排第一，实际 {c:?}"
    );
    assert!(c.iter().any(|t| t == "以"), "拼音候选仍应保留供选择：{c:?}");
}

/// 「+**任何**字母」都成立（用户原话）：拼音切分只认开头的 `yi`，与后续字母无关。
#[test]
fn holds_for_every_trailing_letter() {
    let e = mixed(all_vetoes_off());
    for ch in "abcdefghijklmnopqrstuvwxyz".chars() {
        let s = format!("yijg{ch}");
        let c = texts(&e, &s);
        assert_eq!(
            c.first().map(String::as_str),
            Some("就是"),
            "{s}: 首选应为「就是」，实际 {c:?}"
        );
    }
}

/// 反向锁（youyoud 回归）：`youyo` 与 `yijga` **只差一个变量**——两者前 N 码（`youy`/`yijg`）
/// 都是精确全码，但 `youyo` = you + `yo`，`yo` 是合法音节前缀 ⇒ 拼音还没打完、主张整串。
/// 此时 ⓪ 必须继续拦，否则第 5 键就把「悠悠的」顶成「变凉」。
#[test]
fn pinyin_still_owns_overflow_when_it_may_continue() {
    // 前提：拦截必须来自「拼音还没打完」这一位，而不是「前缀不是精确全码」——否则本例
    // 与 yijga 就不再是单一变量对照，youyoud 回归也就没被真正锁住。
    assert!(
        pinyin().is_possible_pinyin_sequence("youyo"),
        "前置：you + yo（合法音节前缀）⇒ 拼音还在打"
    );
    assert!(
        wubi().has_full_input_match("youy"),
        "前置：前 N 码 youy 确是精确全码（与 yijg 同侧），唯一变量只剩拼音主张与否"
    );

    let e = mixed(all_vetoes_off());
    assert_eq!(
        e.handle_top_code("youyo"),
        None,
        "拼音还没打完（you+yo）→ ⓪ 照常抑制顶码"
    );
    let c = texts(&e, "youyo");
    assert!(
        !c.iter().any(|t| t == "变凉"),
        "码表候选不得回捞，否则「悠悠的」被五笔截胡：{c:?}"
    );
    assert!(c.iter().any(|t| t == "悠"), "应保持纯拼音候选：{c:?}");
}

/// 反向锁（拼音打错字母）：`nihxo` 的拼音同样主张不了整串（`hx` 不是音节前缀），
/// 但前 N 码 `nihx` 在码表**没有精确全码** ⇒ 例外口不成立，仍归拼音。
/// 这一条保证「打错一个字母就被五笔顶码截胡」不会发生。
#[test]
fn no_reclaim_when_prefix_is_not_an_exact_code() {
    // 前提：拦截必须来自「前缀不是精确全码」这一位。若拼音其实主张得了这串，本例就退化成
    // 与上一例重复的测试，"前缀精确全码" 这一半判据将无人看守。
    let py = pinyin();
    assert!(
        !py.is_possible_pinyin_sequence("nihxo"),
        "前置：hx 不是音节前缀 ⇒ 拼音已打岔（与 yijga 同侧）"
    );
    assert!(
        py.convert("nihxo", 1).unwrap().candidates[0].consumed_length < 5,
        "前置：拼音首选解释不了整串"
    );
    assert!(
        !wubi().has_full_input_match("nihx"),
        "前置：前 N 码 nihx 在码表无精确全码 —— 这才是本例唯一的拦截理由"
    );

    let e = mixed(all_vetoes_off());
    assert_eq!(
        e.handle_top_code("nihxo"),
        None,
        "前缀非精确全码 → 码表没资格主张，顶码仍抑制"
    );
    assert!(
        texts(&e, "nihxo").iter().any(|t| t == "你"),
        "仍应交出拼音候选"
    );
}

/// 回捞的前缀候选不得被当成**本次输入**的精确匹配：`is_exact_code` 归一到完整输入。
/// 下游（协调器 `candidate_display_order` / `freq_rerank::freq_tier`）一律以完整输入为准，
/// 不归一会把只匹配前缀的候选提拔进精确档。
#[test]
fn reclaimed_prefix_candidate_is_not_marked_exact() {
    let e = mixed(all_vetoes_off());
    let r = e.convert("yijga", 20).unwrap();
    let jiushi = r
        .candidates
        .iter()
        .find(|c| c.text == "就是")
        .expect("前置：「就是」应已回捞");
    assert_eq!(jiushi.code, "yijg", "候选码是前 N 码");
    assert!(
        !jiushi.is_exact_code,
        "code(yijg) != input(yijga)，不得标为精确匹配"
    );
}

/// ★ 回捞的前缀候选必须**如实标注 `consumed_length`**（= 前 N 码），否则协调器
/// （`commit_selected` 的 `partial = consumed > 0 && consumed < total`）按「消费整串」处理，
/// 选中即把没解释的尾码一并吃掉 —— `yijga` 选「就是」，尾巴上的 `a` 凭空消失。
///
/// 这是码表候选带 `consumed_length` 的唯一出口（其余路径恒 0），协调器侧两处依赖
/// 「码表恒 0 ⇒ 永不部分匹配」的判据已随之对齐，见 `build_candidates` /
/// `learn_phrase_on_commit`。端到端对照见 `wind-coordinator` 的 `input_flow.rs`。
#[test]
fn reclaimed_prefix_candidate_reports_partial_consumption() {
    let e = mixed(all_vetoes_off());
    let r = e.convert("yijga", 20).unwrap();
    let jiushi = r
        .candidates
        .iter()
        .find(|c| c.text == "就是")
        .expect("前置：「就是」应已回捞");
    assert_eq!(
        jiushi.consumed_length, 4,
        "只解释得了前 4 码 yijg，尾码 a 须留在缓冲里"
    );

    // 对照：同一列表里的拼音候选按自己的解释长度标注，两者互不干扰。
    let yi = r
        .candidates
        .iter()
        .find(|c| c.text == "以")
        .expect("前置：拼音候选应在列");
    assert_eq!(yi.consumed_length, 2, "拼音只解释得了 yi");
}

/// 关掉 ⓪ 的老路径不受影响（混合 overflow：前 N 码 + 拼音整串竞争）。
#[test]
fn overflow_off_path_unchanged() {
    let e = mixed(MixConfig {
        pinyin_only_overflow: false,
        ..all_vetoes_off()
    });
    assert_eq!(
        e.handle_top_code("yijga"),
        Some(("就是".to_string(), "a".to_string()))
    );
    assert_eq!(texts(&e, "yijga").first().map(String::as_str), Some("就是"));
}

/// ★ 出厂默认下的完整表现：**候选回捞 + 顶码放行**。归属结论 ⓪① 共用后的新语义。
///
/// ⚠️⚠️ **本用例反转了一条旧决定，别当回归修回去。** 它原名
/// `default_guards_reclaim_candidates_but_still_block_commit`，断言的是「① 出厂即开 ⇒ 顶码
/// 仍被拦」，并在注释里把「⓪ 的例外口不是上屏放行口」写成红线。那条红线的依据是「默认安全」，
/// 但真机反馈证明这一格恰恰是缺陷：`cety`（唯一全码「通往」）+ 第 5 键在**出厂配置**下不顶码，
/// 而 `ty` 根本构不成音节 —— 顶码在默认配置下等于失效，用户在设置页里也关不掉 ⓪（`hidden`）。
///
/// 反转依据是语义而非「让五笔更爽快」：① 的含义是**让路给拼音**，让路的前提是拼音接得住
/// 这一串；`codetable_owns_overflow` 的四条判据已经判定它接不住（前 N 码是码表精确全码、
/// 拼音与英文都主张不了整串、拼音尚能交出候选）。同一次按键里候选侧已把「就是」回捞到首位，
/// 顶码侧却仍拦下 —— 那是两处对同一个归属问题给出相反处置，本就是缺陷形态。
///
/// ⚠️ 边界仍然由下面两条对照守住：拼音**主张得了**整串时 ① 照拦
/// （`pinyin_veto_still_applies_when_pinyin_claims_the_string`）、② 完全不受豁免
/// （`pinyin_word_veto_is_not_exempted_by_the_claim`）。
#[test]
fn default_guards_reclaim_and_now_also_commit() {
    let e = mixed(MixConfig::default());

    let c = texts(&e, "yijga");
    assert_eq!(
        c.first().map(String::as_str),
        Some("就是"),
        "候选装配只受 ⓪ 管，默认开关下同样应回捞：{c:?}"
    );
    assert!(c.iter().any(|t| t == "以"), "拼音候选仍在列：{c:?}");

    assert_eq!(
        e.handle_top_code("yijga"),
        Some(("就是".to_string(), "a".to_string())),
        "归属判给码表后 ① 不得再以「有拼音候选」为由否决顶码"
    );
}

/// 真机主角（内存复刻）：`cety` = 唯一全码「通往」，第 5 键应顶码上屏 —— **出厂配置下**。
///
/// 与 `yijga` 是同一条判据的两个实例，分开写是因为它们的拼音形状不同：`yijg` 的 `jg` 从第 3 键
/// 起就不成音节，而 `cety` 的 `ce` 是**完整音节**、`ty` 才断掉。后者更贴近用户直觉里的
/// 「前面像拼音、后面不像」，也是 ① 那个粗粒度 `has_pinyin` 最容易误判的形状。
#[test]
fn cety_top_commits_under_factory_defaults() {
    let py = pinyin();
    // 前置：确认本例真落在「拼音主张不了整串」那一侧，否则测的是别的分支。
    assert!(
        !py.is_possible_pinyin_sequence("cetya"),
        "前置：ty 不是音节前缀 ⇒ 拼音没在打这串"
    );
    assert_eq!(
        py.convert("cetya", 1).unwrap().candidates[0].consumed_length,
        2,
        "前置：拼音首选只解释 ce（2/5），够不上接管整串"
    );
    // 前置：`has_pinyin` 确实为真 —— 否则 ① 本就不触发，本用例退化成假绿。
    assert!(
        !py.convert("cetya", 1).unwrap().candidates.is_empty(),
        "前置：裸 convert 确有候选（① 的 has_pinyin 正是这么算的）"
    );

    let e = mixed(MixConfig::default());
    assert_eq!(
        e.handle_top_code("cetya"),
        Some(("通往".to_string(), "a".to_string())),
        "出厂配置下 cety + 第 5 键应顶出「通往」+ 余码 a"
    );
}

/// 反向锁 ①：拼音**主张得了**整串时，① 照旧否决顶码 —— 豁免只覆盖归属判给码表的那条窄带。
///
/// 单一变量：⓪ 显式关（否则拦截来自 ⓪，测不到 ①），① 显式开，串用 `youyo`
/// （`you` + `yo`，`yo` 是合法音节前缀 ⇒ 拼音还在打）。前 N 码 `youy` 同样是精确全码，
/// 与 `yijga` 只差「拼音主张与否」这一位。
#[test]
fn pinyin_veto_still_applies_when_pinyin_claims_the_string() {
    assert!(
        pinyin().is_possible_pinyin_sequence("youyo"),
        "前置：拼音还在打这串（与 yijga 的唯一差别）"
    );
    assert!(
        wubi().has_full_input_match("youy"),
        "前置：前 N 码是精确全码，例外口的第一条对两例同时成立"
    );

    let e = mixed(MixConfig {
        auto_commit_block_on_pinyin: true,
        block_commit_on_pinyin_word: false,
        pinyin_only_overflow: false,
        ..Default::default()
    });
    assert_eq!(
        e.handle_top_code("youyo"),
        None,
        "拼音主张整串 ⇒ 归属不成立 ⇒ ① 必须照拦（否则「悠悠的」被顶成「变凉」）"
    );
}

/// 反向锁 ②：`block_commit_on_pinyin_word` **不**享受豁免 —— 它判「整串是强拼音词」（词强度），
/// 与「归属」正交，只有 ① 那一半被削弱。
///
/// `wangjg` 是唯一能把 ② 单独测出来的形状：前 N 码 `wang` 既是码表精确全码（例外口第一条成立）
/// 又恰是 1 个完整拼音音节（② 的判据 b 命中），而尾巴 `jg` 不成音节前缀 ⇒ 拼音主张不了整串
/// （例外口第二条成立）。⓪ 与 ① 都会被豁免放行，拦下它的只可能是 ②。
///
/// ⚠️ 尾巴必须 ≥2 个不成前缀的字母：只多**一个**字母时（`wangb`）该字母恒是某音节的合法前缀，
/// 拼音就主张得了整串，例外口不成立，拦截来源退回 ⓪/① —— 那样本用例就变成假绿。
#[test]
fn pinyin_word_veto_is_not_exempted_by_the_claim() {
    let py = pinyin();
    assert!(
        py.is_whole_syllable_pinyin("wang") && py.completed_syllable_count("wang") == 1,
        "前置：前 N 码恰是 1 个完整音节 ⇒ ② 的判据 (b) 命中"
    );
    assert!(
        !py.is_possible_pinyin_sequence("wangjg"),
        "前置：jg 不成前缀 ⇒ 拼音主张不了整串，例外口成立、①被豁免"
    );
    assert!(
        wubi().has_full_input_match("wang"),
        "前置：前 N 码是码表精确全码，例外口第一条成立"
    );

    // ① 显式关：把变量收敛到 ② 一个。②（出厂即开）应独立拦下顶码。
    let e = mixed(MixConfig {
        auto_commit_block_on_pinyin: false,
        block_commit_on_pinyin_word: true,
        ..Default::default()
    });
    assert_eq!(
        e.handle_top_code("wangjg"),
        None,
        "② 与归属正交，不得被例外口豁免"
    );

    // 对照：把 ② 也关掉 ⇒ 顶码放行，证明上面那条拦截确实来自 ②。
    let off = mixed(MixConfig {
        auto_commit_block_on_pinyin: false,
        block_commit_on_pinyin_word: false,
        ..Default::default()
    });
    assert_eq!(
        off.handle_top_code("wangjg"),
        Some(("王".to_string(), "jg".to_string())),
        "② 关掉后应放行 —— 否则上一条断言的拦截来源另有其人"
    );
}

// ── 第三条判据：英文主张整串时，码表不得夺走归属 ──

/// ★ 英文词库开启时打 `words`：前 4 码 `word` 在码表是精确全码，码表精确 `+1e7` 会把英文精确
/// `+500K` 整层压掉，首选变成码表词、空格上屏还把尾码 `s` 一并吃掉。判据补上「英文主张整串」
/// 后归属回到英文。
///
/// ⚠️ 选 `words` 而非 `github` 是为了**隔离变量**：`github` 的拼音一条候选都交不出，会被第四条
/// 判据（拼音须交得出候选，见 `codetable_does_not_claim_when_pinyin_yields_nothing`）**提前**
/// 挡下，英文判据根本走不到 —— 那样本测试就退化成假绿。`words` 的拼音出得来「我」（`wo`）却
/// 主张不了整串（`rds` 不成音节），三条判据里只有英文那条会否决。
#[test]
fn english_owns_overflow_when_it_matches_whole_input() {
    // 前置：另外三条判据都**不**否决，否则测的是别的分支。
    assert!(
        wubi().has_full_input_match("word"),
        "前置：前 N 码 word 确是精确全码（与 yijg 同侧）"
    );
    let py = pinyin();
    assert!(
        !py.is_possible_pinyin_sequence("words"),
        "前置：rds 不是音节前缀 ⇒ 拼音主张不了整串（与 yijga 同侧）"
    );
    assert!(
        !py.convert("words", 1).unwrap().candidates.is_empty(),
        "前置：拼音交得出候选（wo→我）⇒ 第四条判据放行，唯一变量只剩英文"
    );
    assert!(
        english().has_full_input_match("words"),
        "前置：英文库有精确整串词条 words"
    );

    let e = mixed_en(all_vetoes_off());
    let c = texts(&e, "words");
    assert_eq!(
        c.first().map(String::as_str),
        Some("words"),
        "英文有精确整串词条 ⇒ 归属归英文，实际 {c:?}"
    );
    assert!(
        !c.iter().any(|t| t == "叉"),
        "码表前缀候选不得回捞：它只解释得了 word，选中会把 s 一并吃掉。实际 {c:?}"
    );
}

// ── 第四条判据：拼音一条候选都交不出的串，码表也不该主张 ──

/// ★ 真机回归（用户报告的原始现象）：**没开英文词库**时打 `github`，首选是五笔词「不算」，
/// 空格上屏把整个缓冲吃掉。
///
/// 前三条判据在此全部放行 —— 前 4 码 `gith` 是精确全码、`gi` 不成音节所以拼音主张不了整串、
/// 英文引擎压根不在场。可这串**连开头都不像中文**（拼音一条候选都交不出），把它判给码表并无
/// 依据：`yijga` 至少解释得出「以」，`github` 什么都解释不出。此时应保持候选为空，让用户按
/// 空格/回车直接上屏原码 `github` —— 这正是 249f486 之前的行为。
///
/// 与顶码 ⓪ 天然一致：⓪ 的判据本就是 `pinyin_only_overflow && has_pinyin && !ct_owns`，
/// `has_pinyin=false` 时它整条不成立，故本判据对顶码通路无影响（顶码另由 ③ 管，见
/// `topcode_on_english_word_is_still_governed_by_the_english_guard`）。
#[test]
fn codetable_does_not_claim_when_pinyin_yields_nothing() {
    // 前置：前三条判据确实都放行 —— 否则拦截来自别处，本判据无人看守。
    assert!(
        wubi().has_full_input_match("gith"),
        "前置：前 N 码 gith 是精确全码"
    );
    let py = pinyin();
    assert!(
        !py.is_possible_pinyin_sequence("github"),
        "前置：gi 不是音节前缀 ⇒ 拼音主张不了整串"
    );
    assert!(
        py.convert("github", 1).unwrap().candidates.is_empty(),
        "前置：拼音一条候选都交不出 —— 这才是本例唯一的拦截理由"
    );

    // english=None：用户没开英文词库的真实配置。
    let e = mixed(all_vetoes_off());
    let c = texts(&e, "github");
    assert!(
        c.is_empty(),
        "码表不得回捞「不算」：候选应为空，让用户直接上屏原码 github。实际 {c:?}"
    );
}

/// 反向锁：拼音**交得出**候选时照常回捞（`yijga` 的「以」）—— 第四条判据不得误伤主场景。
/// 与 `codetable_reclaims_overflow_when_pinyin_cannot_explain` 的分工：那条锁「拼音主张不了
/// 整串」，这条锁「拼音至少交得出点什么」。两个条件方向相反，必须同时成立。
#[test]
fn partial_pinyin_candidate_is_enough_to_keep_the_claim() {
    let py = pinyin();
    assert!(
        !py.convert("yijga", 1).unwrap().candidates.is_empty(),
        "前置：拼音交得出「以」（只解释 2/5）"
    );
    let e = mixed(all_vetoes_off());
    assert_eq!(
        texts(&e, "yijga").first().map(String::as_str),
        Some("就是"),
        "拼音哪怕只解释得了开头，也说明这串还在中文语境里 ⇒ 码表照常主张"
    );
}

/// 反向锁：英文只有**前缀**匹配（`yijgatron`）不足以夺走归属 —— 否则英文库 21918 条的前缀面
/// 会让一堆恰好撞上某英文词开头的五笔全码平白丢掉候选。
#[test]
fn english_prefix_alone_does_not_take_overflow() {
    // 前置：拦截必须来自「无精确整串」这一位，而不是「英文根本没候选」。
    assert!(
        !english().has_full_input_match("yijga"),
        "前置：yijga 在英文库无精确整串词条"
    );
    assert!(
        english().has_longer_code("yijga"),
        "前置：但确有更长后继 yijgatron ⇒ 英文交得出前缀候选，本例不是「英文空手」的假绿"
    );

    let e = mixed_en(all_vetoes_off());
    let c = texts(&e, "yijga");
    assert_eq!(
        c.first().map(String::as_str),
        Some("就是"),
        "英文仅前缀命中 ⇒ 归属仍归码表，「就是」应排第一，实际 {c:?}"
    );
    // ★ 前缀扩展**不再入列**（此前这里断言的是「仍应混入供选择」）。
    // 显示侧已收窄为只收精确命中（`MixedEngine::english_display_candidates`）：
    // `yijga` 在英文库无精确整串，故 `Yijgatron` 这条前缀候选不该出现在候选面上。
    // 归属判据（上面那条 assert_eq）不受影响——它问的是「谁主张这一串」，
    // 走的是 `english_claims_overflow` 的精确整串判据，与显示侧本就是两回事。
    assert!(
        !c.iter().any(|t| t == "Yijgatron"),
        "前缀扩展不得入列（只收精确命中）：{c:?}"
    );
}

/// 分工锁：本修复只改**候选归属**，顶码是另一条通路。
///
/// `github` 拼音一条候选都没有 ⇒ ⓪① 的 `has_pinyin` 前提不成立、② 的整串强拼音词也不成立，
/// 于是关掉 ③ 时顶码照旧顶出「不算」+ 余码 `ub`。**这是 249f486 之前就有的既有行为**
/// （见 memory 案例二），由 `auto_commit_block_on_english` 负责，不该误以为本次一并修了。
#[test]
fn topcode_on_english_word_is_still_governed_by_the_english_guard() {
    let off = mixed_en(all_vetoes_off());
    assert_eq!(
        off.handle_top_code("github"),
        Some(("不算".to_string(), "ub".to_string())),
        "③ 关 + 无拼音候选 ⇒ 顶码不受本修复影响（候选归属与上屏否决正交）"
    );

    let on = mixed_en(MixConfig {
        auto_commit_block_on_english: true,
        ..all_vetoes_off()
    });
    assert_eq!(
        on.handle_top_code("github"),
        None,
        "③ 开 ⇒ 顶码被英文守护拦下，这才是顶码侧的正解"
    );
}

/// 真实词库端到端验收（用户诉求的落点）：内存 fixture 测得了判据，测不出真机首选与真机词库。
///
/// 真机数据：`cety` 在 `wubi86_jidian.dict.yaml:7013` 是 `cety 通往 1442`、**全库唯一**该码；
/// 拼音侧 `ce` 有 6 个同音单字（侧/测/册/策/厕/…），`ty` 不成音节前缀。改动前第 5 键
/// `handle_top_code` 恒返回 `None`，改动后顶出「通往」+ 余码。
///
/// ⚠️ **简拼必须是关的**（`enable_pinyin_abbrev`，出厂已改为 `false`，这里仍显式写出以免
/// 出厂值再变时本用例静默换了被测分支）。简拼**开**时 `cetya` 的拼音首选是简拼整句
/// 「从而同样啊」（`consumed_length = 5` = 整串）⇒ `pinyin_claims_overflow` 成立 ⇒ 例外口第二条
/// 不成立 ⇒ **⓪ 独立拦下顶码，本修复够不着**。这是一个**已知且有意保留**的边界：简拼几乎能
/// 主张任何字母串，若让它不算「主张」，长简拼词（`zgrmghg` 之类）会被前 4 码的五笔全码顶码
/// 截胡 —— 取舍为「默认关简拼让顶码可用，需要简拼的用户显式打开并接受失去顶码」。
/// 勿把本用例的 `enable_pinyin_abbrev = false` 当成可有可无的样板照抄删掉。
///
/// ⚠️ 缺 `build_dev/data` 时静默跳过（全仓惯例）。判据是耗时与下方 eprintln。
#[test]
fn real_dict_cety_top_commits_on_the_fifth_key() {
    use std::path::PathBuf;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../build_dev/data")
        .canonicalize()
        .ok()
        .filter(|p| p.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists());
    let Some(dir) = dir else {
        eprintln!("跳过 real_dict_cety_top_commits_on_the_fifth_key：build_dev/data 不存在");
        return;
    };

    let mgr = |tag: &str| {
        let mut cfg = wind_config::Config::default();
        cfg.schema.available = vec![
            "wubi86".to_string(),
            "pinyin".to_string(),
            "wubi86_pinyin".to_string(),
        ];
        cfg.schema.active = "wubi86_pinyin".to_string();
        // 对齐出厂 `data/config.toml`：`Config::default()`（L1）的 top_code_commit 是 false，
        // 不覆写的话顶码通路整个不启用，全部断言都会退化成假绿。
        cfg.schema.codetable.top_code_commit = true;
        cfg.schema.mix.enable_pinyin_abbrev = false;
        let root = std::env::temp_dir().join(format!("wind_cety_topcode_{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        wind_engine::EngineManager::with_store_override(
            &cfg,
            Some(&dir),
            None,
            Some(root.join("overrides")),
        )
    };

    let m = mgr("default");
    // 前置：4 码本身不受影响，「通往」是首选。
    let c4 = m.convert("cety", 10);
    assert_eq!(
        c4.candidates.first().map(|c| c.text.as_str()),
        Some("通往"),
        "前置：满码时五笔精确全码应排第一，实际 {:?}",
        c4.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );

    // 前置：`has_pinyin` 在真实词库下确实为真（① 正是靠它否决的），否则本用例是假绿。
    let py_side = m.convert("cetya", 10);
    assert!(
        py_side.candidates.iter().any(|c| c.code == "ce"),
        "前置：拼音残码候选确实在场，实际 {:?}",
        py_side
            .candidates
            .iter()
            .map(|c| format!("{}({})", c.text, c.code))
            .collect::<Vec<_>>()
    );

    // 核心：出厂开关（① 与 ⓪ 均为 true）下第 5 键应顶码上屏。
    for tail in "abcdefghijklmnopqrstuvwxyz".chars() {
        let s = format!("cety{tail}");
        assert_eq!(
            m.handle_top_code(&s),
            Some(("通往".to_string(), tail.to_string())),
            "{s}: 第 5 键应顶出「通往」+ 余码 {tail}"
        );
    }
}

/// ★ 默认值守门（行为层）：**不碰任何 `schema.mix` 开关**，仅靠出厂默认，`cety` 第 5 键就该顶码。
///
/// 与上一条的分工：上一条显式关简拼、测的是**判据**；本条刻意**什么都不设**、测的是
/// **出厂默认值本身**（`enable_pinyin_abbrev` 出厂改为 `false` 正是为了让顶码可用）。
/// 把简拼默认值翻回 `true`，本用例即红——`wind-config` 那两条（`mix_pinyin_abbrev_defaults_off`
/// / `..._l1_and_l2_agree`）只钉取值，钉不住「这个取值换来了什么行为」。
///
/// ⚠️ `top_code_commit` 仍须显式对齐 L2：它在 `Config::default()`（L1）里是 `false`，
/// 不设的话顶码通路整个不启用，断言会退化成假绿。
#[test]
fn factory_defaults_alone_are_enough_for_cety_topcode() {
    use std::path::PathBuf;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../build_dev/data")
        .canonicalize()
        .ok()
        .filter(|p| p.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists());
    let Some(dir) = dir else {
        eprintln!("跳过 factory_defaults_alone_are_enough_for_cety_topcode：build_dev/data 不存在");
        return;
    };

    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec![
        "wubi86".to_string(),
        "pinyin".to_string(),
        "wubi86_pinyin".to_string(),
    ];
    cfg.schema.active = "wubi86_pinyin".to_string();
    cfg.schema.codetable.top_code_commit = true;
    // ↑ 到此为止。schema.mix 的任何一项都不设——本用例测的就是它们的出厂值。
    assert!(
        !cfg.schema.mix.enable_pinyin_abbrev,
        "前置：简拼出厂应为关，否则本用例测的是别的配置"
    );
    assert!(
        cfg.schema.mix.auto_commit_block_on_pinyin && cfg.schema.mix.pinyin_only_overflow,
        "前置：两道否决出厂都开着 —— 顶码能放行是靠归属例外口，不是靠守护没开"
    );

    let root = std::env::temp_dir().join("wind_cety_factory_defaults");
    let _ = std::fs::remove_dir_all(&root);
    let m = wind_engine::EngineManager::with_store_override(
        &cfg,
        Some(&dir),
        None,
        Some(root.join("overrides")),
    );
    assert_eq!(
        m.handle_top_code("cetya"),
        Some(("通往".to_string(), "a".to_string())),
        "出厂默认下 cety + 第 5 键应顶出「通往」"
    );
}
