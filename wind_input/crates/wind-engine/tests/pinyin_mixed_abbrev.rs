//! 混合简拼端到端：同一串里混用声母与完整音节（`nhao` = n + hao、`nih` = ni + h）。
//!
//! 设计文档 `docs/design/pinyin-mixed-abbrev.md`。
//!
//! **自带 wdat 夹具，不依赖 `build_dev/data`**：简拼索引只有 mmap 词典才有（内存词典返回
//! 空），而依赖真实词库的测试在该目录缺失时会**静默跳过、计数照常绿**（判据只有耗时）。
//! 走 `CachedDict::load_at` 的 wdat-only 模式，与 `pinyin_abbrev_index.rs` 同款。
//!
//! ⚠️ 夹具刻意全部采用**歧义切分码**：`maximum_match` 恰好猜对的样本（`cainiaoyizhan`
//! 那类）测不出任何东西，简拼路径的历史缺陷全部藏在「真值切分 ≠ 最大匹配」的缝里。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 最小 wdat：主表（带真值 boundary）+ 简拼二级索引（存全拼码）。
///
/// 关键夹具事实，逐条都是某个断言的支点：
///
/// | 码 | 词 | 真值切分 | `maximum_match` 会切成 | 简拼 |
/// |---|---|---|---|---|
/// | `nihao`    | 你好   | ni\|hao       | 同左（无歧义）  | nh |
/// | `nanhai`   | 南海   | nan\|hai      | 同左            | nh |
/// | `xianning` | 西安宁 | xi\|an\|ning  | **xian\|ning**  | xan |
/// | `xianning` | 先拧   | xian\|ning    | 同左            | xn |
///
/// 「你好」与「南海」共用简拼键 `nh` —— 混合简拼比纯简拼多出的那点信息量，全靠这一对
/// 才能被观测到（`nhao` 应只留你好）。
/// 「西安宁」与「先拧」共用**扁平码** `xianning` 而切分不同 —— 音节数过滤的支点。
fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_mixed_abbrev_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // (text, weight, order, boundary)  boundary = 各音节起始字节位
    //
    // ⚠️ **权重一律取 `cn_dicts` 里的真实值**，不要随手编。
    //
    // 节点打分是 `ln(weight / DICT_TOTAL)`，而 step 2b 的质量闸门
    // `MIXED_SENTENCE_MIN_LOGP_PER_CHAR`(-8.0) 是在**真实词库的权重分布**上标定的
    // （见其文档：`bzdhaobuhao`→不知道好不好 每字 -3.90）。编造的权重会让夹具落在闸门的
    // 另一侧，测出的是另一个世界的行为。
    //
    // 历史：这批权重原本是编的（不知道 7000 / 哈 4000 / 你好 9000），当时 `score_node` 对
    // **无 unigram** 的引擎走 `weight / 100_000` 的线性回退，与真实路径的对数量纲根本不在
    // 同一数轴上，夹具因此「碰巧」落在闸门通过侧。2026-08-08 unigram 并回 dict、量纲统一后
    // 这批编造值立即被闸门挡下 —— 那不是回归，是夹具一直在验证一条生产走不到的路径。
    w.add_with_boundary("nihao".into(), vec![("你好".into(), 5328, 0, 0b101)]);
    w.add_with_boundary("nanhai".into(), vec![("南海".into(), 991, 0, 0b1001)]);
    w.add_with_boundary("nihaoma".into(), vec![("你好吗".into(), 166, 0, 0b100101)]);
    // bu|zhi|dao —— 供「简拼族前缀回退」用例：`bzdha` 整串无词，须退到 `bzd`
    w.add_with_boundary(
        "buzhidao".into(),
        vec![("不知道".into(), 62492, 0, 0b100101)],
    );
    w.add_with_boundary("ha".into(), vec![("哈".into(), 16497, 0, 0b1)]);
    // biao|zhang|da|hui —— 简拼 `bzdh`，**权重刻意远低于「不知道」**。
    // 真机现场：打 `bzdhaobuhao` 时 `bzdh` 这个更长的切点把 `h` 抢走，首选成了「表彰大会」。
    // boundary = bit 0/4/9/11：biao(0..4) zhang(4..9) da(9..11) hui(11..14)
    w.add_with_boundary(
        "biaozhangdahui".into(),
        vec![("表彰大会".into(), 93, 0, 0b101000010001)],
    );
    w.add_with_boundary(
        "xianning".into(),
        vec![
            ("西安宁".into(), 3000, 0, 0b10101),  // xi|an|ning，3 音节
            ("先拧".into(), 500_000, 1, 0b10001), // xian|ning，2 音节，权重高得多
        ],
    );
    // 简拼索引：键是**完整声母串**，值是全拼码（v5）。混合简拼复用的正是这批键。
    w.add_abbrev(
        "nh".into(),
        vec![("nihao".into(), 9000), ("nanhai".into(), 8000)],
    );
    w.add_abbrev("nhm".into(), vec![("nihaoma".into(), 2000)]);
    w.add_abbrev("bzd".into(), vec![("buzhidao".into(), 7000)]);
    w.add_abbrev("bzdh".into(), vec![("biaozhangdahui".into(), 93)]);
    w.add_abbrev("xan".into(), vec![("xianning".into(), 3000)]);
    w.add_abbrev("xn".into(), vec![("xianning".into(), 500_000)]);
    w.write(&wdat).unwrap();

    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn engine(tag: &str) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), fixture(tag))
}

fn texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    e.convert(input, 30)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

/// 只取**整串消费**的候选（排除前缀回退的部分候选）。
fn full_texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    e.convert(input, 30)
        .map(|r| {
            r.candidates
                .into_iter()
                .filter(|c| !c.is_partial)
                .map(|c| c.text)
                .collect()
        })
        .unwrap_or_default()
}

/// **声母在前**：`nhao` = n(声母) + hao(全音节) → 你好。
///
/// 这是立项时的真机反馈本身。此前 `is_abbreviation` 对 `nhao` 判**真**、顺利进了简拼
/// 分支，却在召回处落空——索引里只有整串简拼 `nh`，拿 `nhao` 去点查什么都没有。
#[test]
fn initial_first_mixed_abbrev_hits() {
    let e = engine("initial_first");
    let t = texts(&e, "nhao");
    assert!(t.contains(&"你好".to_string()), "nhao 应出「你好」: {t:?}");
}

/// **声母在中间**：`nihm` = ni + h + m → 你好吗。这是混合式第二类无可替代的形态。
///
/// 前缀补全在这里彻底帮不上忙：`nihm` 不是 `nihaoma` 的字符串前缀，`search_prefix`
/// 一条都返回不了。而 `is_abbreviation("nihm")` 同样判假（`i` 不是任何音节首字母），
/// 纯简拼分支也进不去 —— 改动前这串必然候选为空。
#[test]
fn initial_in_the_middle_mixed_abbrev_hits() {
    let e = engine("middle");
    let t = texts(&e, "nihm");
    assert!(
        t.contains(&"你好吗".to_string()),
        "nihm 应出「你好吗」: {t:?}"
    );
}

/// **末尾声母（`nih`）这一半，前缀补全早就兜住了 —— 文档 §2「候选为空」的描述不准确。**
///
/// 凡「全拼在前、声母收尾」的混合式，必然是某个全拼码的字符串前缀（`nih` ⊂ `nihao`），
/// 于是 step4 的 `search_prefix` 直接命中，与简拼判据判不判真无关。实测：把
/// `enable_abbrev` 关掉，`nih` 照样出「你好」。
///
/// 记在这里是为了防止后人拿 `nih` 当混合简拼的验收用例 —— 那是一条**恒绿**的断言，
/// 改动前后都成立，什么也证明不了。真正只有混合式能做到的是 `nhao`（声母在前）与
/// `nihm`（声母在中间）两类。
#[test]
fn trailing_initial_form_is_already_covered_by_prefix_completion() {
    let cfg = PyConfig {
        enable_abbrev: false,
        ..PyConfig::default()
    };
    let e = PinyinEngine::new(cfg, fixture("trailing"));
    let r = e.convert("nih", 30).expect("应有候选");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("关掉简拼后 nih 仍应出你好——它来自前缀补全");
    assert!(c.is_prefix, "证据：这条候选是前缀补全层，不是简拼层: {c:?}");
}

/// **混合比纯简拼多出的信息量必须真的用上**：`nhao` 的**整串**候选只留你好，不带出南海。
///
/// 前缀回退会让「南海」以部分候选（`is_partial`，只消费 `nh`）排在整串候选之后，那是
/// 微软式逐级补全，不算混入；这里只查整串消费的候选。
///
/// 对照组是同一夹具下的 `nh`——它必须两个都出。少了这个对照，上面那条断言就可能是
/// 「夹具里压根没有南海」的假绿，而不是「过滤器真的挡住了它」。
#[test]
fn mixed_abbrev_filters_by_syllable_not_just_initial() {
    let e = engine("filter");

    let plain = texts(&e, "nh");
    assert!(
        plain.contains(&"你好".to_string()) && plain.contains(&"南海".to_string()),
        "对照组：纯简拼 nh 两个词都该出（否则下面的断言是假绿）: {plain:?}"
    );

    let mixed = full_texts(&e, "nhao");
    assert!(mixed.contains(&"你好".to_string()), "{mixed:?}");
    assert!(
        !mixed.contains(&"南海".to_string()),
        "nan|hai 的第二音节不是 hao，混合式必须挡掉它: {mixed:?}"
    );
}

/// **音节数过滤在混合形态下仍成立**（文档 §5 约束 3，口径改为「段数」）。
///
/// `xanning` = x(声母) + an + ning，3 段。扁平码 `xianning` 下挂着两个切分不同的词，
/// 回查主表会把它们一并捞出来：
/// - 「西安宁」xi\|an\|ning —— 3 音节，符合
/// - 「先拧」  xian\|ning  —— 2 音节，**权重高 166 倍**，不挡就直接占首位
///
/// 这也是本文件坚持用歧义切分码的理由：若拿 `maximum_match` 恰好猜对的码来测，
/// 真值与猜测重合，过滤器有没有生效根本看不出来。
#[test]
fn mixed_abbrev_rejects_wrong_syllable_count() {
    let e = engine("count");
    let t = texts(&e, "xanning");

    assert!(
        t.contains(&"西安宁".to_string()),
        "xi|an|ning 与 [x][an][ning] 三段吻合，应命中: {t:?}"
    );
    assert!(
        !t.contains(&"先拧".to_string()),
        "xian|ning 只有 2 音节，与 3 段不符，必须挡掉（即便权重高得多）: {t:?}"
    );
}

/// 混合简拼候选与纯简拼**同层**（`is_abbrev`），且不借用别的层级键（文档 §5 约束 2）。
///
/// 层级是硬闸门：两类同质候选分属两层，靠后那层怎么调词频都翻不过来。历史现场是
/// 系统词简拼借 `is_prefix=true` 沉底、用户词简拼用 `is_abbrev=true`（`ae2df59` 已统一）。
#[test]
fn mixed_abbrev_shares_the_plain_abbrev_layer() {
    let e = engine("layer");

    let mixed = e.convert("nhao", 30).expect("应有候选");
    let m = mixed
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("nhao 应命中你好");
    let plain = e.convert("nh", 30).expect("应有候选");
    let p = plain
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("nh 应命中你好");

    assert!(m.is_abbrev, "混合候选须标 is_abbrev");
    assert!(!m.is_prefix, "简拼不是前缀补全，不得借 is_prefix 沉底");
    assert!(!m.is_fuzzy, "更不是模糊命中");
    assert_eq!(
        wind_candidate::cmp_match_layers(m, p),
        std::cmp::Ordering::Equal,
        "混合简拼与纯简拼必须同层"
    );
}

/// 候选带**全拼码与真值边界**，与纯简拼一致 —— 词频记账走候选的 code，
/// 若这里落成击键串 `nhao`，同一个词在混合/纯简拼/全拼下就会走三份互不相认的计数。
#[test]
fn mixed_abbrev_candidate_carries_full_code_and_boundary() {
    let e = engine("code");
    let r = e.convert("nhao", 30).expect("应有候选");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("应命中");

    assert_eq!(c.code, "nihao", "须带全拼码，而非击键串 nhao");
    assert_eq!(c.boundary, 0b101, "边界随主表条目一并拿到");
    // 混合式消费整串击键（code 不是 query 的前缀 → 落「消费整串」分支）
    assert_eq!(c.consumed_length, 4, "nhao 四键全部消费");
}

/// **正常全拼输入零影响**：`nihao` 不因混合路径多出任何 is_abbrev 候选。
///
/// `nihao` 本身是有合法混合解释的（ni + h + ao），挡住它的是 step 5b 的 `mixed_covered`
/// 短路——整串已被音节完整覆盖就不进混合路径。这条断言守的就是那个短路：它一旦失效，
/// 绝大多数击键都会白跑一趟索引，且可能静默混入噪音候选。
#[test]
fn full_pinyin_input_is_untouched() {
    let e = engine("nomix");
    let r = e.convert("nihao", 30).expect("应有候选");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .expect("全拼应正常命中");
    assert!(!c.is_abbrev, "全拼命中不该被标成简拼: {c:?}");
    assert!(
        r.candidates.iter().all(|c| !c.is_abbrev),
        "全拼输入不该产生任何简拼层候选: {:?}",
        r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}

/// 关掉简拼（混输经 `schema.mix.enable_pinyin_abbrev` 注入）时混合式一并关掉 ——
/// 二者对「几乎任何字母串都可能是拼音」的放宽是同一件事，不能只关一半。
#[test]
fn disabling_abbrev_also_disables_mixed() {
    let cfg = PyConfig {
        enable_abbrev: false,
        ..PyConfig::default()
    };
    let e = PinyinEngine::new(cfg, fixture("off"));
    assert!(
        !texts(&e, "nhao").contains(&"你好".to_string()),
        "enable_abbrev=false 时不该出混合简拼候选"
    );
    assert!(
        !texts(&e, "nihm").contains(&"你好吗".to_string()),
        "声母在中间的形态同样要关掉"
    );
}

/// 用户/临时造词层的混合简拼：经声母索引取候选，再按各词自带的 boundary 逐段比对。
/// 索引本身的行为见 `pinyin_store_abbrev_index.rs`，此处只关心混合式判据。
///
/// 用户词「大菠萝哥」da|bo|luo|ge，混合式 `dbluoge` = d + b + luo + ge。
/// 注意 `is_abbreviation("dbluoge")` 判**假**（`u` 不是任何音节的首字母），
/// 故这条路径只可能由混合判据放行 —— 它同时验证了 step6 的入口条件确实取了「或」。
#[test]
fn user_word_mixed_abbrev_hits() {
    let dir = std::env::temp_dir().join("wind_mixed_abbrev_user");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("nihao".into(), vec![("你好".into(), 9000, 0, 0b101)]);
    w.add_abbrev("nh".into(), vec![("nihao".into(), 9000)]);
    w.write(&wdat).unwrap();
    let dict = CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("夹具");

    let p = std::env::temp_dir().join("wind_mixed_abbrev_user.redb");
    let _ = std::fs::remove_file(&p);
    let store = std::sync::Arc::new(wind_store::Store::open(&p).unwrap());
    store
        .add_user_word("pinyin", "daboluoge", "大菠萝哥", 500, 0b10010101)
        .unwrap();
    let dm = wind_dict::manager::DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    let e = PinyinEngine::new(PyConfig::default(), dict).with_store_layers(std::sync::Arc::new(dm));

    let r = e.convert("dbluoge", 30).expect("应有候选");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "大菠萝哥")
        .unwrap_or_else(|| {
            panic!(
                "dbluoge 应命中用户词: {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    assert!(c.is_abbrev, "用户词混合简拼同样落 is_abbrev 层");
    assert_eq!(c.code, "daboluoge", "保留全拼码，词频记账才认得同一个词");
    // 纯简拼不回归
    assert!(
        texts(&e, "dblg").contains(&"大菠萝哥".to_string()),
        "纯简拼 dblg 仍须命中"
    );
}

/// **preedit 按混合切分显示**：`nhao` → `n'hao`，不是原样 `nhao`，更不是 `ni'hao`。
///
/// 真机反馈（2026-07-30 实施当天）：候选对了但预编辑区没有分隔显示。根因是「跟随首选
/// 候选」那一支的条件 `top.code == completed` —— 简拼/混合的 code 是词的全拼码，与击键
/// 不同域，而 `nhao` 一个完整音节都切不出、`completed` 是空串，两边永远对不上。
///
/// ⚠️ 断言必须同时钉住「有分隔符」和「与击键同长」两件事。只断言含 `'` 的话，
/// 显示成 `ni'hao` 也能过——那才是更糟的错：用户敲 4 键看到 5 个字母，退格立刻错位。
#[test]
fn preedit_shows_mixed_segmentation_in_keystroke_domain() {
    let e = engine("preedit_mixed");
    let r = e.convert("nhao", 30).expect("应有候选");
    assert_eq!(r.candidates[0].text, "你好", "前提：首选是混合简拼候选");
    assert_eq!(r.preedit_display, "n'hao");
    assert_eq!(r.preedit_pinyin, "n'hao", "混输高亮取的是这一份");
    assert_eq!(
        r.preedit_display.replace('\'', "").len(),
        "nhao".len(),
        "去掉分隔符必须还原成击键串——显示域不得与击键域脱节"
    );
}

/// **纯简拼同样要分段显示**：`nh` → `n'h`（用户报的「简拼的状态也是如此」）。
///
/// 与混合式共用一套渲染：两者的候选都带全拼码 + 真值 boundary，切法完全相同。
#[test]
fn preedit_shows_plain_abbrev_segmentation() {
    let e = engine("preedit_plain");
    let r = e.convert("nh", 30).expect("应有候选");
    assert!(r.candidates[0].is_abbrev, "前提：首选是简拼候选");
    assert_eq!(r.preedit_display, "n'h");
}

/// 三段混合式：`xanning` → `x'an'ning`（声母段 + 两个音节段）。
#[test]
fn preedit_handles_three_segment_mixed_form() {
    let e = engine("preedit_three");
    let r = e.convert("xanning", 30).expect("应有候选");
    assert_eq!(r.candidates[0].text, "西安宁");
    assert_eq!(r.preedit_display, "x'an'ning");
}

/// 全拼 preedit 不回归：仍按音节切分显示，且走的是原有那条分支。
#[test]
fn preedit_of_full_pinyin_unchanged() {
    let e = engine("preedit_fp");
    assert_eq!(
        e.convert("nihao", 30).expect("应有候选").preedit_display,
        "ni'hao"
    );
    assert_eq!(
        e.convert("xianning", 30).expect("应有候选").preedit_display,
        "xian'ning",
        "首选「先拧」的真值切分是 xian|ning"
    );
}

/// **简拼族前缀回退**：`bzdha` 整串没有词，须退到 `bzd` 出「不知道」并只消费 3 键。
///
/// 真机现场（连打 `bzdnihaobuhao`）：输到 `bzdha` **整串空码**。`bzd` 明明能出「不知道」，
/// 但简拼族的召回一直是「全串或无」—— 索引按完整简拼串点查、混合模式要求覆盖整串。
/// 全拼下有 step3 子短语与 step4 前缀补全兜着，简拼下此前没有任何对应机制。
#[test]
fn abbrev_falls_back_to_longest_matching_prefix() {
    let e = engine("prefix_fallback");
    let r = e.convert("bzdha", 30).expect("不该空码");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "不知道")
        .unwrap_or_else(|| {
            panic!(
                "bzdha 应退到 bzd 出「不知道」: {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        c.consumed_length, 3,
        "只消费 bzd 三键，余下 ha 留给下一次输入；算成整串就会把 ha 一起吃掉"
    );
    assert!(c.is_abbrev && c.is_partial, "部分简拼须沉在完整匹配之后");
}

/// ★★ **多个切点的候选共存，同层按词频竞争 —— 不做「最长匹配」硬排除。**
///
/// 真机现场：打 `bzdhaobuhao`（想要「不知道」+「好不好」）首候选是**「表彰大会」**。
/// `bzdh` 恰好是它的简拼（biao·zhang·da·hui），首版「一有产出即停」就把 `h` 判给了它，
/// 更短的 `bzd`（→「不知道」，真实词库里**词频高 672 倍**）一条都进不来，
/// 剩下的 `aobuhao` 成了垃圾。
///
/// 把「切点长短」做成布尔式硬排除 = **惩罚 ∞**，而它只是**来源差异**、不是结构质量差异，
/// 只配走 weight（同款教训见模糊拼音 `is_fuzzy` 从层级键改惩罚的那一轮）。
#[test]
fn competing_cuts_coexist_and_are_ranked_by_frequency() {
    let e = engine("cut_competition");
    let r = e.convert("bzdhaobuhao", 30).expect("不该空码");

    assert_eq!(
        r.candidates[0].text,
        "不知道",
        "词频高得多的短切点必须拿到首选，不能被更长的切点整层排除: {:?}",
        r.candidates
            .iter()
            .take(5)
            .map(|c| (&c.text, c.weight, c.consumed_length))
            .collect::<Vec<_>>()
    );
    assert_eq!(r.candidates[0].consumed_length, 3, "只消费 bzd");

    // 更长的切点**仍在候选里**（用户真想要它也选得到），只是按词频沉在后面。
    let bzdh = r
        .candidates
        .iter()
        .position(|c| c.text == "表彰大会")
        .expect("bzdh 切点的候选不该被丢掉，只该被排后");
    assert!(bzdh > 0, "它的词频低得多，不该占首选");
    assert_eq!(r.candidates[bzdh].consumed_length, 4, "它消费 4 键");
}

/// 参与竞争的切点数有上限（`MAX_FALLBACK_CUTS`）—— 过短的切点不入场。
///
/// 若一路试到 `bz`，真实词库里「标准/帮助/保证」会凭 ~5 万的词频挤进前几位，
/// 而它们只解释了 11 键里的 2 键。取「最长 + 次长」两个切点即可覆盖真实的竞争场景。
#[test]
fn fallback_does_not_descend_to_very_short_cuts() {
    let e = engine("cut_limit");
    let r = e.convert("bzdhaobuhao", 30).expect("应有候选");
    let cuts: std::collections::BTreeSet<usize> = r
        .candidates
        .iter()
        .filter(|c| c.is_abbrev && c.is_partial)
        .map(|c| c.consumed_length)
        .collect();
    assert!(cuts.len() <= 2, "最多两个切点参与竞争，实际: {cuts:?}");
    assert!(!cuts.contains(&2), "不该退到只解释 2 键的切点: {cuts:?}");
}

/// **整串能命中时不降级**（零回归红线）：`bzd` 只出整串候选，不产生部分候选。
#[test]
fn full_stroke_hit_does_not_trigger_fallback() {
    let e = engine("no_fallback");
    let r = e.convert("bzd", 30).expect("应有候选");
    assert_eq!(r.candidates[0].text, "不知道");
    assert!(
        r.candidates.iter().all(|c| !c.is_partial),
        "整串命中就不该有任何部分候选: {:?}",
        r.candidates
            .iter()
            .map(|c| (&c.text, c.is_partial))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        r.candidates[0].consumed_length, 3,
        "整串简拼消费全部击键（走的是统一计算那条路，不是回退路径）"
    );
}

/// 连打场景：`bzdnihao` 同样退到 `bzd`，余下 `nihao` 留给下一次转换。
/// 这才是用户真实的连续输入形态（`bzdnihaobuhao`）。
#[test]
fn prefix_fallback_works_for_long_continuous_stroke() {
    let e = engine("prefix_long");
    let r = e.convert("bzdnihao", 30).expect("不该空码");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "不知道")
        .unwrap_or_else(|| {
            panic!(
                "bzdnihao 应退到 bzd: {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    assert_eq!(c.consumed_length, 3);
}

/// **长串不得被回退门槛挡在门外**（真机第三轮回归）。
///
/// `bzdnihaobuh` 最少需要 `[b][z][d][ni][hao][bu][h]` **七段**，超过 `MAX_SEGMENTS=6`
/// ⇒ 整串混合模式为空；`is_abbreviation` 又因 `i` 判假。若门槛问的是「整串本身是不是一条
/// 合法的简拼/混合模式」，这串就两头都不沾 ⇒ 回退不启动 ⇒ **连打到第 11 键彻底空码**。
///
/// 门槛该问的是「这串里**有成不了音节的部分**吗」（`!mixed_covered`）——逐前缀的严格判定
/// 在 `recall_abbrev_prefix` 内部各自进行，门槛只负责挡掉纯全拼。
#[test]
fn long_stroke_is_not_blocked_by_fallback_gate() {
    let e = engine("long_gate");
    // 逐长度连打，从 3 到全长都必须有候选（真机现场：n>=11 起空码）
    let full = "bzdnihaobuh";
    for n in 3..=full.len() {
        let r = e.convert(&full[..n], 30).expect("应有结果");
        assert!(
            !r.candidates.is_empty(),
            "连打到第 {n} 键（{}）不该空码",
            &full[..n]
        );
    }
    let c = e
        .convert(full, 30)
        .expect("应有候选")
        .candidates
        .into_iter()
        .find(|c| c.text == "不知道")
        .expect("应退到 bzd");
    assert_eq!(c.consumed_length, 3, "仍只消费 bzd 三键");
}

/// 部分匹配时**余下的击键要自己再切一遍**，不能整段甩到尾巴上。
///
/// `bzdnihaob` 选中「不知道」(消费 `bzd`) 后尾巴是 `nihaob`，其中 `ni`/`hao` 是完整音节、
/// `b` 是残码。整段追加会显示成 `b'z'd'nihaob` —— 该切的地方没切。
#[test]
fn partial_preedit_segments_the_remainder_too() {
    let e = engine("tail_seg");
    let r = e.convert("bzdnihaob", 30).expect("应有候选");
    assert_eq!(r.candidates[0].text, "不知道", "前提：首选是部分简拼候选");
    assert_eq!(r.preedit_display, "b'z'd'ni'hao'b");
    assert_eq!(
        r.preedit_display.replace('\'', "").len(),
        "bzdnihaob".len(),
        "去掉分隔符仍须还原击键串"
    );
}

/// **纯全拼输入绝不进前缀回退**（回退门槛的红线）。
///
/// `nihao` 是完整音节序列：`is_abbreviation` 判假（`i` 不是音节首字母）、`mixed_covered`
/// 短路又让混合模式为空 —— 简拼族对它「一无所获」。若回退只按「简拼族没命中」放行，
/// 它就会被拖进降级：退到 `niha` 后混合模式 `[ni][ha]` 之类会凭空捞出 `is_abbrev` 候选，
/// 破坏候选的层级次序（真实词库下 `meiyou` 因此多出「没有」的简拼副本，
/// `engine_manager::test_pinyin_trailing_partial_prefix_floats_above_exact` 当场变红）。
///
/// 门槛必须是「**整串本身**像简拼族形态」——「简拼族没命中」不等于「该做简拼降级」。
#[test]
fn full_pinyin_never_enters_prefix_fallback() {
    let e = engine("no_fallback_fp");
    for input in ["nihao", "xianning", "buzhidao", "ha"] {
        let r = e.convert(input, 30).expect("应有候选");
        assert!(
            r.candidates.iter().all(|c| !c.is_abbrev),
            "{input} 是全拼输入，不该出现任何简拼层候选（含前缀回退产出）: {:?}",
            r.candidates
                .iter()
                .filter(|c| c.is_abbrev)
                .map(|c| (&c.text, c.consumed_length))
                .collect::<Vec<_>>()
        );
    }
}

/// 部分匹配时 preedit 把余下击键作残码尾段。
///
/// 用 `bzdnih`：夹具里 `nih` 那段拼不出任何整句（`ni`+`h` 的 `h` 是残码），故首选仍是
/// 部分简拼候选「不知道」，走的是 `render_keystroke_preedit` 那一支。
/// （`bzdha` 已被 step 2b 的混合整句接管，见 `mixed_sentence_*` 用例。）
#[test]
fn preedit_shows_trailing_remainder_on_partial_abbrev() {
    let e = engine("preedit_partial");
    let r = e.convert("bzdnih", 30).expect("应有候选");
    assert_eq!(r.candidates[0].text, "不知道", "前提：首选是部分简拼候选");
    assert!(r.candidates[0].is_partial, "且是部分匹配");
    assert_eq!(r.preedit_display, "b'z'd'ni'h");
    assert_eq!(
        r.preedit_display.replace('\'', "").len(),
        "bzdnih".len(),
        "去掉分隔符仍须还原击键串"
    );
}

/// ★★ **混合整句**：`bzdha` = `bzd`(简拼→不知道) + `ha`(全拼→哈) 由 Viterbi 拼成一句。
///
/// 这是「智能组句」本身 —— 用户一次上屏即可，无需先选「不知道」再选「哈」。
/// 简拼节点与全拼节点在同一张词图里竞争（`LatticeNode` 是字节跨度、Viterbi 按字节推进），
/// 故这条路径不需要改解码器，只需让简拼跨度进得了图。
#[test]
fn mixed_sentence_combines_abbrev_and_full_pinyin() {
    let e = engine("mixed_sentence");
    let r = e.convert("bzdha", 30).expect("应有候选");
    let top = &r.candidates[0];
    assert_eq!(
        top.text,
        "不知道哈",
        "简拼段 + 全拼段应拼成整句: {:?}",
        r.candidates
            .iter()
            .take(4)
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
    assert!(top.is_sentence, "须标整句身份");
    // 与整串简拼词同层按权重竞争（`pinyin_mixed_sentence_layer.rs`）。它仍是首选：
    // 前缀回退的「不知道」带 `is_partial`，同层内排在完整匹配之后。
    assert!(top.is_abbrev, "②b 整句在简拼层");
    assert_eq!(top.consumed_length, 5, "整句消费全部击键");
}

/// 混合整句**不因质量闸门误伤正常解**。
///
/// 闸门（`MIXED_SENTENCE_MIN_LOGP_PER_CHAR`）挡的是「路径平均每字 log_prob 过低」的拼凑
/// 整句。取值定在零代价点：真实词库的受控对比里，它与不设闸门的整句命中率完全相同
/// （12.10%），只挡掉最离谱的那批。这条断言守住「正常的混合整句照常出」这一侧。
#[test]
fn mixed_sentence_survives_quality_gate() {
    let e = engine("gate");
    let r = e.convert("bzdha", 30).expect("应有候选");
    assert!(
        r.candidates[0].is_sentence,
        "夹具里 bzd+ha 是合理组合，不该被闸门挡掉: {:?}",
        r.candidates
            .iter()
            .take(3)
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
}

/// **单节点路径不包装成整句**（词频记账的红线）。
///
/// `dblg` 只解出「夺不了冠」一个词——那本质就是一条简拼候选。包装成整句会让它的 `code`
/// 变成击键串 `dblg`，而简拼候选的 `code` 是全拼码 `duobuliaoguan`：同一个词的词频就此
/// 记到两个互不相认的键上，用简拼练熟的词切回全拼一点不认。这正是 wdat v5 改「索引存码」
/// 时修掉的坑，不能从整句这条新路径上重新引进来。
#[test]
fn single_node_path_is_not_wrapped_as_sentence() {
    let e = engine("single_node");
    let r = e.convert("bzd", 30).expect("应有候选");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "不知道")
        .expect("应命中");
    assert!(!c.is_sentence, "一个词不算句: {c:?}");
    assert_eq!(c.code, "buzhidao", "须保留全拼码，词频才记在同一个键上");
}

/// 关掉简拼总开关时混合整句一并关掉 —— 它整条路径都建立在简拼节点上。
#[test]
fn disabling_abbrev_also_disables_mixed_sentence() {
    let cfg = PyConfig {
        enable_abbrev: false,
        ..PyConfig::default()
    };
    let e = PinyinEngine::new(cfg, fixture("gate_off"));
    let r = e.convert("bzdha", 30).expect("应有结果");
    assert!(
        !r.candidates.iter().any(|c| c.text == "不知道哈"),
        "enable_abbrev=false 时不该出混合整句: {:?}",
        r.candidates
            .iter()
            .take(3)
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
}

/// 混合整句的 preedit 同样与击键同域：`bzdha` → `b'z'd'ha`（简拼段每字母一位）。
///
/// 走不到常规整句那一支是因为 `completed` 对这类输入恒为空串——`bzdha` 从位置 0 就切不出
/// 完整音节，而那一支的条件是 `top.code == completed && !completed.is_empty()`。
#[test]
fn mixed_sentence_preedit_is_in_keystroke_domain() {
    let e = engine("mixed_sentence_preedit");
    let r = e.convert("bzdha", 30).expect("应有候选");
    assert_eq!(r.preedit_display, "b'z'd'ha");
    assert_eq!(
        r.preedit_display.replace('\'', "").len(),
        "bzdha".len(),
        "去掉分隔符仍须还原击键串"
    );
}

/// 夹具自检：确认走的是 mmap 路径。简拼索引只有 mmap 词典才有，夹具一旦退化成内存
/// 词典，本文件所有「应命中」的断言会变成「恒空」，而「不应命中」的断言会**全部假绿**。
#[test]
fn fixture_is_mmap_backed() {
    let d = fixture("selfcheck");
    assert!(
        !d.search_abbrev("nh", 10).is_empty(),
        "夹具必须是 mmap 词典，否则本文件的否定断言全部失去意义"
    );
}
