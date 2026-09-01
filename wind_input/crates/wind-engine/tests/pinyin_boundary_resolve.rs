//! 拼音词条入库契约：`Engine::resolve_boundary` 的五种结果与其判据。
//!
//! 见 `docs/design/pinyin-entry-boundary-contract.md`。
//!
//! **自带 wdat 夹具，不依赖 build_dev/data**（同 `pinyin_abbrev_index.rs`）：依赖真实词库的
//! 测试在 `build_dev/data` 缺失时会**静默跳过**（判据：耗时 0.00s），而本文件锁的是导入
//! 闸口的准入判据，不能容忍这种静默失效。
//!
//! ⚠️ **样本必须是歧义切分码**。用 `cainiaoyizhan` 这类 `maximum_match` 恰好猜对的串，
//! 测试会永远绿着却什么也没验证（同 `pinyin-code-domains.md` 记的假绿模式）。
//! 本文件的样本全部满足「同一个码有多条切分路径」，且 [`fixture_really_is_ambiguous`]
//! 专门锁住这个前提本身。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_engine::{BoundaryResolution, Engine};

/// 造一份最小 wdat。**单字条目是必需的**——`CharPinyinIndex` 遍历标准音节收集单字候选，
/// 读音验证与判据①都建立在它之上；没有单字条目的字会被判为「无读音」。
///
/// 夹具里几组关键事实：
/// - `xian` 同时是 2 音节码（xi|an → 西安）与 1 音节码（xian → 先）：**字数约束的主场**。
/// - `nanan` 有 `nan|an` 与 `na|nan` 两条 2 音节路径：**靠读音表消歧**。
/// - `angan` 有 `an|gan` 与 `ang|an` 两条，且**两条都能通过读音验证**：**Ambiguous 的样本**。
///   为此给「安」虚构了第二读音 ang、给「甘」虚构了第二读音 an——夹具是自造的，这里
///   要锁的是多解分支的行为，不是汉语事实。
/// - 「重」的代表读音是 zhong（权重更高），但 `chongqing` 仍应切出 chong|qing：
///   **有 code 时多音字不构成问题**。
/// - `xiai`→「喜爱」带真值 boundary，且**故意不给「喜」「爱」单字读音**——这样层 4 必然
///   算不出来，返回 `Exact` 就证明确实走的是层 2 点查。
fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_boundary_resolve_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");

    let mut w = WdatWriter::new();
    // ── 单字（供 CharPinyinIndex 建读音表）──
    w.add_with_boundary("xi".into(), vec![("西".into(), 5000, 0, 0b1)]);
    w.add_with_boundary("ning".into(), vec![("宁".into(), 4000, 0, 0b1)]);
    w.add_with_boundary("nan".into(), vec![("南".into(), 5000, 0, 0b1)]);
    w.add_with_boundary("na".into(), vec![("那".into(), 6000, 0, 0b1)]);
    w.add_with_boundary("gong".into(), vec![("工".into(), 5000, 0, 0b1)]);
    w.add_with_boundary("qing".into(), vec![("庆".into(), 4000, 0, 0b1)]);
    w.add_with_boundary(
        "gan".into(),
        vec![("甘".into(), 5000, 0, 0b1), ("柑".into(), 9000, 1, 0b1)],
    );
    // 两组字对，分别钐住多解时的两种结局（见各自的测试）：
    // 「安甘」代表读音 an+gan 拼出来恰好是 angan ⇒ 层 3 能收收尾。
    // 「昂柑」代表读音 ang+gan 拼出 anggan ≠ angan ⇒ 层 3 也救不了。
    w.add_with_boundary(
        "ang".into(),
        vec![("安".into(), 3000, 0, 0b1), ("昂".into(), 8000, 1, 0b1)],
    );
    // readings：安=[an(5000), ang(3000)]、昂=[ang(8000), an(5000)]
    //           甘=[gan(5000), an(1000)]、柑=[gan(9000), gang(7000), an(1000)]
    w.add_with_boundary(
        "an".into(),
        vec![
            ("安".into(), 5000, 0, 0b1),
            ("甘".into(), 1000, 1, 0b1),
            ("昂".into(), 5000, 2, 0b1),
            ("柑".into(), 1000, 3, 0b1),
        ],
    );
    w.add_with_boundary("gang".into(), vec![("柑".into(), 7000, 0, 0b1)]);
    // 「重」zhong(9000) > chong(3000) ⇒ 代表读音是 zhong，多音字消歧的反例素材
    w.add_with_boundary("zhong".into(), vec![("重".into(), 9000, 0, 0b1)]);
    w.add_with_boundary("chong".into(), vec![("重".into(), 3000, 0, 0b1)]);
    // `xian` 一码两切分：单字「先」(xian) 与词「西安」共用；此处只放单字。
    w.add_with_boundary("xian".into(), vec![("先".into(), 900_000, 0, 0b1)]);
    // ── 带真值边界的词条（层 2 的靶子；「喜」「爱」刻意无单字读音）──
    w.add_with_boundary("xiai".into(), vec![("喜爱".into(), 7800, 0, 0b101)]);

    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn engine(tag: &str) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), fixture(tag))
}

/// ★★ **前提自证**：`angan` 必须真有两条都能通过读音验证的路径，否则下面的
/// `resolves_ambiguous_by_reading_weight` 会因为「压根没有多解」而假绿。
///
/// 判据取 `Ambiguous` 这个**变体本身**——它只可能由「读音验证后仍 >1 条」产生。
#[test]
fn fixture_really_is_ambiguous() {
    let e = engine("amb_precondition");
    assert!(
        matches!(
            e.resolve_boundary("angan", "昂柑"),
            BoundaryResolution::Ambiguous(_)
        ),
        "夹具失去歧义性 ⇒ 多解分支的测试全部失去意义，先修夹具"
    );
}

/// 字数约束把切分从「猜」降为「解方程」：同一个 `xian`，2 字词与 1 字词各得唯一解。
///
/// 这正是 `maximum_match` 做不到的——它只看 code，`xi|an` 与 `xian` 覆盖字符数都是 4。
#[test]
fn char_count_constraint_resolves_xian() {
    let e = engine("xian");
    // 「西安」2 字 ⇒ 必须 2 音节 ⇒ xi|an
    assert_eq!(
        e.resolve_boundary("xian", "西安"),
        BoundaryResolution::Derived(0b101),
        "xi|an"
    );
    // 「先」1 字 ⇒ 必须 1 音节 ⇒ xian 整串
    assert_eq!(
        e.resolve_boundary("xian", "先"),
        // ⚠️ 夹具里「先」带真值 boundary ⇒ **层 2 就命中**，走不到层 4。
        // 这不是缺陷：真实词典里单字词条同样带边界，层 2 本就该优先。
        BoundaryResolution::Exact(0b1),
        "xian"
    );
}

/// 三音节：`xian|ning`（2 音节）被字数约束直接排除，只剩 `xi|an|ning`。
///
/// 这是 `abbrev_of_code` 在 `boundary == 0` 时踩过的原案：DAG 猜成 xian|ning ⇒ 简拼
/// 算出 `xn`，用户打 `xan` 召不回、打 `xn` 反而错误命中。
#[test]
fn char_count_constraint_resolves_three_syllables() {
    let e = engine("xianning");
    assert_eq!(
        e.resolve_boundary("xianning", "西安宁"),
        BoundaryResolution::Derived(0b10101), // xi|an|ning → 起点 0,2,4
    );
}

/// 多解时靠读音表消歧：`nanan` 的 `na|nan` 被「南」的读音表否掉（南只读 nan）。
#[test]
fn reading_table_disambiguates_nanan() {
    let e = engine("nanan");
    assert_eq!(
        e.resolve_boundary("nanan", "南安"),
        BoundaryResolution::Derived(0b1001), // nan|an → 起点 0,3
        "「南」不读 na ⇒ na|nan 出局，剩唯一解"
    );
}

/// ★ **有 code 时多音字不构成问题**：「重」的代表读音是 zhong，但 code 已经把读音写定了。
///
/// 从字生成码的那条路（`generate_word_pinyin`）在这里会给出 zhongqing；而本方法从码出发，
/// 根本不需要知道「重」是多音字。这是两个方向互补的核心论据（设计文档 §3.3）。
#[test]
fn code_pins_the_reading_for_polyphones() {
    let e = engine("chongqing");
    assert_eq!(
        e.resolve_boundary("chongqing", "重庆"),
        BoundaryResolution::Derived(0b100001), // chong|qing → 起点 0,5
    );
}

/// ★★ **层 3 在层 4 多解时接手**，把结果从 `Ambiguous` 拉回 `Derived`。
///
/// `angan` 有 `an|gan` 与 `ang|an` 两条路径且都通过读音验证 ⇒ 层 4 只能报多解。此时层 3
/// 出场：`generate_word_pinyin("安甘")` 词典查不到该词，兜底逐字代表读音得 `an gan`，
/// flat 后**恰好等于** code ⇒ 其切分是确定的，采信为 `Derived`。
///
/// 这条正是「层 4 先跑、多解才请层 3」这个层序的价值所在——它没有降低精度。
#[test]
fn layer_three_rescues_layer_four_ambiguity() {
    let e = engine("angan");
    assert_eq!(
        e.resolve_boundary("angan", "安甘"),
        BoundaryResolution::Derived(0b101), // an|gan → 起点 0,2
    );
}

/// 层 3 也救不了时才落到 `Ambiguous`，此时按「各字读音下标之和」择优。
///
/// 「昂柑」的代表读音拼出 `ang gan` → flat `anggan` ≠ `angan` ⇒ 层 3 不命中。
/// 于是比代价：`an|gan`（昂读 an[1] + 柑读 gan[0] = 1）胜过 `ang|an`（昂读 ang[0] + 柑读 an[2] = 2）。
#[test]
fn resolves_ambiguous_by_reading_weight() {
    let e = engine("angan_amb");
    assert_eq!(
        e.resolve_boundary("angan", "昂柑"),
        BoundaryResolution::Ambiguous(0b101), // an|gan → 起点 0,2
        "两条都通过读音验证、层 3 又不命中 ⇒ 报 Ambiguous，但仍择代价低那条"
    );
}

/// 层 2（词典点查真值）优先于求解，且**确实是层 2 在起作用**。
///
/// 判据可靠性来自夹具：「喜」「爱」没有单字读音 ⇒ 判据①标记 `no_reading` ⇒ 层 4 必然
/// 给出 `NoReading` 而非 `Exact`。因此拿到 `Exact` 只可能是层 2 给的。
#[test]
fn dictionary_truth_wins_and_is_actually_layer_two() {
    let e = engine("xiai");
    assert_eq!(
        e.resolve_boundary("xiai", "喜爱"),
        BoundaryResolution::Exact(0b101),
    );
    // 反向对照：同样的字、换一个词典里没有的码 ⇒ 层 2 落空，只剩层 4 的求解结果。
    // ⚠️ 这里断的是**变体不同**（`NoReading` ≠ `Exact`），层 2 是否起作用的判据在此，
    // 与「无读音该不该拒收」无关——后者见 `non_han_text_is_kept_with_solved_boundary`。
    assert_eq!(
        e.resolve_boundary("xiaiai", "喜爱爱"),
        BoundaryResolution::NoReading(0b10101),
        "无单字读音 ⇒ 判据①标记，切分 xi|ai|ai 仍成立"
    );
}

/// 判据②不满足 → `Unresolvable`（拒收）。五笔码切不出与字数相符的音节序列。
#[test]
fn illegal_code_is_unresolvable() {
    let e = engine("illegal");
    assert_eq!(
        e.resolve_boundary("wgkq", "工"),
        BoundaryResolution::Unresolvable,
        "「工」有读音（判据①过），但 wgkq 切不出 1 个音节 ⇒ 判据②拒"
    );
    // 对照组：同一个字、给合法的码 ⇒ 必须通过。**没有这一组，上面的断言可能因为
    // 「引擎压根没加载」这种完全错误的原因变绿。**
    assert_eq!(
        e.resolve_boundary("gong", "工"),
        BoundaryResolution::Exact(0b1),
    );
}

/// ★★ 判据①不满足 → `NoReading`（**入库**），绝不是 `Unresolvable`。
///
/// 这条曾断言拒收。改变来自 issue #97：用户在设置页加了 `zuo ←`（拼音码 → 符号候选，
/// 加词路径有意放行），导出后再导入却被判非法丢弃——同一条词条，加词放行、导出放行、
/// 导入拒收。符号在拼音词典里当然没有单字读音，可 `zuo` 是正经音节、`←` 恰好一个字符，
/// 边界 `0b1` 是确定的，根本不需要读音来定。
///
/// ⚠️ 拦截「拿错文件」的力量**不在这条**，在判据②（见 `illegal_code_is_unresolvable`）。
#[test]
fn non_han_text_is_kept_with_solved_boundary() {
    let e = engine("nonhan");
    let r = e.resolve_boundary("nana", "那a");
    assert_eq!(r, BoundaryResolution::NoReading(0b101), "切分 na|na 已定");
    assert!(
        r.accepted(),
        "★ 含符号/外文的词条必须入库，这正是 #97 的诉求"
    );
    assert!(!r.lacks_boundary(), "它有边界，不是 NoInfo 那一档");
}

/// ★★ 码超 64 字节 → `NoInfo`（**合法但无边界**），绝不是 `Unresolvable`。
///
/// bitmask 装不下更长的码，既定语义是整体降级为 0（同 `wdict::split_spaced_code`）。
/// 把它判成非法会拒掉合法的超长词条。
#[test]
fn overlong_code_is_no_info_not_illegal() {
    let e = engine("overlong");
    let code = "na".repeat(33); // 66 字节 > 64
    let text = "那".repeat(33);
    let r = e.resolve_boundary(&code, &text);
    assert_eq!(r, BoundaryResolution::NoInfo);
    assert!(r.accepted(), "超长码必须照常入库");
    assert!(r.lacks_boundary());
}

/// 非拼音方案（trait 默认实现）→ `NoInfo`。
///
/// ⚠️ 码表词组码本就没有音节语义，`boundary = 0` 是**正确语义**而非缺陷。默认实现若改成
/// `Unresolvable`，码表词库导入会被整体拒收。
#[test]
fn non_pinyin_engine_defaults_to_no_info() {
    struct Dummy;
    impl Engine for Dummy {
        fn convert(
            &self,
            _input: &str,
            _limit: usize,
        ) -> anyhow::Result<wind_engine::ConvertResult> {
            Ok(wind_engine::ConvertResult::default())
        }
        fn reset(&self) {}
        fn engine_type(&self) -> wind_engine::EngineType {
            wind_engine::EngineType::CodeTable
        }
    }
    let r = Dummy.resolve_boundary("wgkq", "工作");
    assert_eq!(r, BoundaryResolution::NoInfo);
    assert!(r.accepted(), "码表词条必须照常入库");
}

/// `accepted()` / `boundary()` 的取值表——六个变体里只有一个被拒。
#[test]
fn resolution_accessors() {
    use BoundaryResolution::*;
    for (r, b, ok) in [
        (Exact(0b101), 0b101, true),
        (Derived(0b11), 0b11, true),
        (Ambiguous(0b1), 0b1, true),
        // ★ 带边界且入库：漏掉 `boundary()` 里的这一支，符号词条会以 boundary=0 落库，
        // 简拼索引对它们静默失效——比直接拒收更难查。
        (NoReading(0b1), 0b1, true),
        (NoInfo, 0, true),
        (Unresolvable, 0, false),
    ] {
        assert_eq!(r.boundary(), b, "{r:?}");
        assert_eq!(r.accepted(), ok, "{r:?}");
    }
}

/// issue #97 的**端到端形状**：单音节码 + 单符号，正是导出/导入往返里被拒的那一条。
///
/// ★ 单独立一条而不是并进上面那个多字用例：`zuo ←` 走的是与 `nana 那a` 不同的路径——
/// 单音节码在 `join_code_by_boundary` 里导出成**无空格**串（`boundary == 1` 与
/// `boundary == 0` 同形），导入端 `split_spaced_code` 拿到 0、必须重解。多音节词条
/// 导出带空格、走「层 1 采信作者真值」，压根碰不到求解链。**用户观察到的「2 字及以上
/// 正常」就是这么来的**，两条路必须各测各的。
#[test]
fn single_syllable_symbol_entry_survives_reimport() {
    let e = engine("symbol");
    let r = e.resolve_boundary("zuo", "←");
    assert_eq!(r, BoundaryResolution::NoReading(0b1), "单音节边界是确定的");
    assert!(r.accepted(), "★ #97：导出得出来，就必须导得回去");
}
