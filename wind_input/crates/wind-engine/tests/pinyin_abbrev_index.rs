//! 简拼二级索引（wdat v5：AbbrevSection 存全拼码而非词）的装配语义。
//!
//! **自带 wdat 夹具，不依赖 build_dev/data**：简拼表只有 mmap 词典才有（内存词典返回空），
//! 而依赖真实词库的测试在 `build_dev/data` 缺失时会**静默跳过**（判据：耗时 0.00s）——
//! 本文件锁的是一个极隐蔽的缺陷，不能容忍这种静默失效。夹具走 `CachedDict::load_at`
//! 的 wdat-only 模式（yaml 不存在、同名 wdat 存在时直接 mmap）。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 造一份最小 wdat：全拼主表 + 简拼索引（索引存**码**）。
///
/// 关键夹具事实：`xian` 这一个扁平码下同时挂着
/// - 「西安」boundary=0b101（xi|an，**2 音节**）
/// - 「先」  boundary=0b1  （xian，**1 音节**）
///
/// 简拼 `xa` 指向的是前者的码。扁平码丢了音节切分，回查主表会把后者一并捞出来。
fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_abbrev_idx_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // (text, weight, order, boundary)
    w.add_with_boundary(
        "xian".into(),
        vec![
            ("西安".into(), 6000, 0, 0b101),
            ("先".into(), 900_000, 1, 0b1),
        ],
    );
    w.add_with_boundary("xiai".into(), vec![("喜爱".into(), 7800, 0, 0b101)]);
    // 简拼索引：存全拼码，不存词
    w.add_abbrev(
        "xa".into(),
        vec![("xian".into(), 6000), ("xiai".into(), 7800)],
    );
    w.write(&wdat).unwrap();

    // wdat-only 模式：yaml 路径不存在，同名 wdat 存在 → 直接 mmap
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn texts(e: &PinyinEngine, input: &str) -> Vec<String> {
    e.convert(input, 20)
        .map(|r| r.candidates.into_iter().map(|c| c.text).collect())
        .unwrap_or_default()
}

fn engine(dict: CachedDict) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), dict)
}

/// 简拼候选须带**全拼码与边界**——这正是索引改存主键换来的。
///
/// 此前 AbbrevSection 直接存词，候选只能把 code 设成简拼串 `xa`，于是同一个词在简拼与
/// 全拼下走两个互不相认的词频计数（词频记账取候选的 code）；boundary 也只能硬编码 0。
#[test]
fn abbrev_candidate_carries_full_code_and_boundary() {
    let e = engine(fixture("code"));
    let r = e.convert("xa", 20).expect("简拼应有结果");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == "西安")
        .expect("简拼 xa 应命中「西安」");

    assert_eq!(c.code, "xian", "候选须带全拼码，而非简拼串 xa");
    assert_eq!(c.boundary, 0b101, "边界随主表条目一并拿到，不再是 0");
}

/// **同码但音节数不符的词必须挡掉**。
///
/// 扁平码有损：`xian` 既是「西安」的 xi|an（2 音节），也是「先」的 xian（1 音节）。
/// 索引里 `xa` 指向的是前者的码，回查主表却会把后者一并捞出来——实测真实词库下
/// `xa` 出「先/线/弦/现/县」一串单字。这是「存词」改「存码」引入的新失效模式，
/// 存词时不会发生，故必须显式过滤：简拼字母数 == 音节数。
///
/// 注意「先」的权重（900000）远高于「西安」（6000），不过滤的话它会排在最前面。
#[test]
fn abbrev_rejects_same_code_with_wrong_syllable_count() {
    let e = engine(fixture("filter"));
    // 只看整串命中的候选：「先」会作为首声母单字（`is_partial`，只消费 `x`）排在后面，
    // 那是简拼逐级补全的单字档，不算“同码错音节数”混入。
    let t: Vec<String> = e
        .convert("xa", 30)
        .map(|r| {
            r.candidates
                .into_iter()
                .filter(|c| !c.is_partial)
                .map(|c| c.text)
                .collect()
        })
        .unwrap_or_default();

    assert!(
        t.contains(&"西安".to_string()),
        "2 音节的「西安」应命中: {t:?}"
    );
    assert!(
        t.contains(&"喜爱".to_string()),
        "另一个 2 音节词也应命中: {t:?}"
    );
    assert!(
        !t.contains(&"先".to_string()),
        "1 音节的「先」与 2 字母简拼不符，必须挡掉（即便权重高得多）: {t:?}"
    );
}

/// 全拼输入不受影响：同码的单音节词照常出，只有简拼路径按音节数过滤。
#[test]
fn full_pinyin_still_returns_same_code_single_syllable_word() {
    let e = engine(fixture("full"));
    let t = texts(&e, "xian");
    assert!(t.contains(&"先".to_string()), "全拼 xian 应出「先」: {t:?}");
    assert!(t.contains(&"西安".to_string()), "也应出「西安」: {t:?}");
}

/// `syllable_boundary_of` 是**点查取真值，不做推断** —— 这是它与
/// `generate_word_pinyin` 的分水岭。
///
/// 夹具里 `xian` 一个码下挂着两个切分不同的词（西安 xi|an、先 xian），只有按
/// `(code, text)` 精确定位才能给对答案；从词反推读音的那条路做不到这件事。
/// 词频列表靠它显示音节格式——词频表只有 `(code, text)`，自己不存 boundary。
#[test]
fn boundary_lookup_is_exact_not_inferred() {
    let e = engine(fixture("blookup"));

    assert_eq!(e.syllable_boundary_of("xian", "西安"), 0b101, "xi|an");
    assert_eq!(
        e.syllable_boundary_of("xian", "先"),
        0b1,
        "同一个码下的另一个词，切分不同，必须按 text 区分"
    );
    assert_eq!(e.syllable_boundary_of("xiai", "喜爱"), 0b101);
    // 查不到一律 0（= 无边界信息，消费方降级为扁平显示），不得瞎猜
    assert_eq!(e.syllable_boundary_of("xian", "不存在的词"), 0);
    assert_eq!(e.syllable_boundary_of("meiyouzhege", "西安"), 0);
}

/// **系统词简拼与用户词简拼必须同层**，否则用户词被整层压住、调频也翻不过来。
///
/// `cmp_match_layers` 的第一键就是 `is_abbrev`（`false < true`，标记者沉底）。此前
/// step5 的系统词简拼借 `is_prefix=true` 沉底、step6 的用户词简拼用 `is_abbrev=true`
/// ——分属两个层级，于是 `dblg` 下用户词「大菠萝哥」永远排在系统词「夺不了冠」之后，
/// 层级是硬闸门，怎么调频都翻不过来。
///
/// 本测试只断言**同层**（都标 `is_abbrev`），不断言谁在前——那由权重与词频决定。
#[test]
fn system_and_user_abbrev_share_one_layer() {
    let dir = std::env::temp_dir().join("wind_abbrev_layer");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // 系统词「夺不了冠」duo|bu|liao|guan → 简拼 dblg
    w.add_with_boundary(
        "duobuliaoguan".into(),
        vec![("夺不了冠".into(), 5000, 0, 0b1000010010001)],
    );
    w.add_abbrev("dblg".into(), vec![("duobuliaoguan".into(), 5000)]);
    w.write(&wdat).unwrap();
    let dict = CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("夹具");

    // 用户词「大菠萝哥」da|bo|luo|ge → 简拼同为 dblg
    let p = std::env::temp_dir().join("wind_abbrev_layer.redb");
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
    let r = e.convert("dblg", 20).expect("简拼应有结果");

    let sys = r
        .candidates
        .iter()
        .find(|c| c.text == "夺不了冠")
        .expect("系统词简拼应命中");
    let usr = r
        .candidates
        .iter()
        .find(|c| c.text == "大菠萝哥")
        .expect("用户词简拼应命中");

    assert!(
        sys.is_abbrev,
        "系统词简拼须标 is_abbrev（此前借的是 is_prefix）"
    );
    assert!(usr.is_abbrev, "用户词简拼须标 is_abbrev");
    assert!(
        !sys.is_prefix,
        "简拼不是前缀补全，层级由 is_abbrev 表达，不该再借 is_prefix"
    );
    assert_eq!(
        wind_candidate::cmp_match_layers(sys, usr),
        std::cmp::Ordering::Equal,
        "两侧必须同层，否则用户词被整层压住、词频翻不过硬闸门"
    );
}

/// 夹具自检：确认走的是 mmap 路径（简拼表只有 mmap 词典才有）。
/// 若哪天 wdat-only 模式变了、夹具退化成内存词典，简拼恒空、上面几个断言会变得没有意义。
#[test]
fn fixture_is_mmap_backed() {
    let d = fixture("selfcheck");
    assert!(
        !d.search_abbrev("xa", 10).is_empty(),
        "夹具必须是 mmap 词典，否则简拼表为空、本文件的断言全部失去意义"
    );
}
