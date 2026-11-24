//! 拼音词条入库契约：`Engine::resolve_boundary` 的五种结果与其判据。
//!
//! 见 `docs/design/pinyin-entry-boundary-contract.md`。
//!
//! 自带 wdat 夹具，不依赖 build_dev/data（同 `pinyin_abbrev_index.rs`）：依赖真实词库的
//! 测试在数据缺失时会静默跳过，而本文件锁的是导入闸口的准入判据，不能容忍这种失效。
//!
//! 样本必须是歧义切分码：`maximum_match` 恰好猜对的串会让测试永远假绿。
//! [`fixture_really_is_ambiguous`] 专门锁住这个前提。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_engine::{BoundaryResolution, Engine};

/// 造一份最小 wdat。单字条目是必需的：`CharPinyinIndex` 靠它收集单字候选，
/// 读音验证与判据①都建立在它之上。
///
/// 夹具里几组关键事实：
/// - `xian` 同时是 2 音节码（西安）与 1 音节码（先）：字数约束的主场。
/// - `nanan` 有 `nan|an` 与 `na|nan` 两条 2 音节路径：靠读音表消歧。
/// - `angan` 有 `an|gan` 与 `ang|an` 两条且都能通过读音验证：Ambiguous 的样本。
///   为此给「安」虚构了读音 ang、给「甘」虚构了读音 an，锁的是多解分支行为而非汉语事实。
/// - 「重」代表读音是 zhong，但 `chongqing` 仍应切出 chong|qing：有 code 时多音字不构成问题。
/// - `xiai`→「喜爱」带真值 boundary 且故意不给单字读音：层 4 必然算不出，
///   返回 `Exact` 就证明走的是层 2 点查。
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
    // 两组字对分别钉住多解时的两种结局：
    // 「安甘」代表读音 an+gan 拼出恰好是 angan ⇒ 层 3 能收尾；
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
    // ── ü 拼写归一的素材（`xv`→`xu`、`nue`→`nve`）──
    // 词典侧只有正字法形态：手输的 `xv` 若不归一，层 2 与层 4 都落空 ⇒ Unresolvable。
    w.add_with_boundary("xu".into(), vec![("需".into(), 5000, 0, 0b1)]);
    w.add_with_boundary("yao".into(), vec![("要".into(), 6000, 0, 0b1)]);
    w.add_with_boundary("nve".into(), vec![("虐".into(), 5000, 0, 0b1)]);
    // 「女」**只挂在 `nv` 下**（刻意不给 `nu` 一份）：这让层 2 点查成为
    // 「`nv` 有没有被误归一成 `nu`」的判据——见 `umlaut_normalization_leaves_nl_v_alone`。
    w.add_with_boundary("nv".into(), vec![("女".into(), 5000, 0, 0b1)]);
    // ── 带真值边界的词条（层 2 的靶子；「喜」「爱」刻意无单字读音）──
    w.add_with_boundary("xiai".into(), vec![("喜爱".into(), 7800, 0, 0b101)]);

    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn engine(tag: &str) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), fixture(tag))
}

/// 前提自证：`angan` 必须真有两条都能通过读音验证的路径，否则
/// `resolves_ambiguous_by_reading_weight` 会因没有多解而假绿。判据取 `Ambiguous` 变体本身。
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
/// `maximum_match` 只看 code，做不到这一点。
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
        // 夹具里「先」带真值 boundary ⇒ **层 2 就命中**，走不到层 4。
        // 这不是缺陷：真实词典里单字词条同样带边界，层 2 本就该优先。
        BoundaryResolution::Exact(0b1),
        "xian"
    );
}

/// 三音节：`xian|ning`（2 音节）被字数约束直接排除，只剩 `xi|an|ning`。
/// 猜成 xian|ning 会让简拼算出 `xn`，打 `xan` 召不回。
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

/// 有 code 时多音字不构成问题：「重」代表读音是 zhong，但 code 已写定读音。
/// 这是与从字生成码（`generate_word_pinyin`）互补的核心论据（设计文档 §3.3）。
#[test]
fn code_pins_the_reading_for_polyphones() {
    let e = engine("chongqing");
    assert_eq!(
        e.resolve_boundary("chongqing", "重庆"),
        BoundaryResolution::Derived(0b100001), // chong|qing → 起点 0,5
    );
}

/// 层 3 在层 4 多解时接手，把结果从 `Ambiguous` 拉回 `Derived`：`angan` 两条路径都通过
/// 读音验证，而「安甘」逐字代表读音 flat 后恰好等于 code，切分确定。
#[test]
fn layer_three_rescues_layer_four_ambiguity() {
    let e = engine("angan");
    assert_eq!(
        e.resolve_boundary("angan", "安甘"),
        BoundaryResolution::Derived(0b101), // an|gan → 起点 0,2
    );
}

/// 层 3 也救不了时才落到 `Ambiguous`，按各字读音下标之和择优：「昂柑」拼出 `anggan` ≠ `angan`，
/// `an|gan`（an[1] + gan[0] = 1）胜过 `ang|an`（ang[0] + an[2] = 2）。
#[test]
fn resolves_ambiguous_by_reading_weight() {
    let e = engine("angan_amb");
    assert_eq!(
        e.resolve_boundary("angan", "昂柑"),
        BoundaryResolution::Ambiguous(0b101), // an|gan → 起点 0,2
        "两条都通过读音验证、层 3 又不命中 ⇒ 报 Ambiguous，但仍择代价低那条"
    );
}

/// 层 2（词典点查真值）优先于求解。「喜」「爱」无单字读音 ⇒ 层 4 只会给 `NoReading`，
/// 拿到 `Exact` 只可能是层 2 给的。
#[test]
fn dictionary_truth_wins_and_is_actually_layer_two() {
    let e = engine("xiai");
    assert_eq!(
        e.resolve_boundary("xiai", "喜爱"),
        BoundaryResolution::Exact(0b101),
    );
    // 反向对照：换一个词典里没有的码 ⇒ 层 2 落空。这里断的是变体不同（`NoReading` ≠ `Exact`），
    // 无读音该不该拒收见 `non_han_text_is_kept_with_solved_boundary`。
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

/// 判据①不满足 → `NoReading`（入库），不是 `Unresolvable`。符号在拼音词典里没有单字读音，
/// 但 `zuo ←` 的边界 `0b1` 是确定的；拒收会让加词放行、导出放行、导入却拒收。
/// 拦截「拿错文件」靠判据②（见 `illegal_code_is_unresolvable`）。
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

/// 码超 64 字节 → `NoInfo`（合法但无边界），不是 `Unresolvable`：bitmask 装不下时
/// 整体降级为 0（同 `wdict::split_spaced_code`），判成非法会拒掉合法的超长词条。
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

/// 非拼音方案（trait 默认实现）→ `NoInfo`。码表词组码没有音节语义，默认实现若改成
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
        // 带边界且入库：漏掉 `boundary()` 里的这一支，符号词条会以 boundary=0 落库，
        // 简拼索引对它们静默失效——比直接拒收更难查。
        (NoReading(0b1), 0b1, true),
        (NoInfo, 0, true),
        (Unresolvable, 0, false),
    ] {
        assert_eq!(r.boundary(), b, "{r:?}");
        assert_eq!(r.accepted(), ok, "{r:?}");
    }
}

/// 单音节码 + 单符号的导出/导入往返。单立一条：单音节码导出成无空格串（`boundary == 1`
/// 与 0 同形），导入端必须重解；多音节词条带空格走层 1，碰不到求解链，两条路须各测各的。
#[test]
fn single_syllable_symbol_entry_survives_reimport() {
    let e = engine("symbol");
    let r = e.resolve_boundary("zuo", "←");
    assert_eq!(r, BoundaryResolution::NoReading(0b1), "单音节边界是确定的");
    assert!(r.accepted(), "★ #97：导出得出来，就必须导得回去");
}

/// ü 拼写归一（`xv` → `xu`、`nue` → `nve`）：两种写法必须给出同一结果。断言写成互等而非
/// 写死 boundary，归一化改坏成别的映射也会红。
/// 这条守入库侧；查询侧在 `pinyin::tests` 另有用例，只归一一侧会加得进打不出。
#[test]
fn umlaut_spellings_resolve_same_as_orthographic_form() {
    let e = engine("umlaut");
    for (typed, orthographic, text) in [
        ("xv", "xu", "需"),
        ("nue", "nve", "虐"),
        ("xvyao", "xuyao", "需要"),
    ] {
        let a = e.resolve_boundary(typed, text);
        let b = e.resolve_boundary(orthographic, text);
        assert_eq!(a, b, "`{typed}` 与 `{orthographic}`（{text}）须同解");
        assert!(a.accepted(), "`{typed}`（{text}）须可入库，实际 {a:?}");
    }
}

/// 反向守卫：`nv`/`lv` 已是正字法形态，归一化不得动它们，否则「女」入不了库。
/// 判据取层 2 点查（「女」只挂在 `nv` 下）；比「`nv` 与 `nu` 不同解」有区分力，
/// 单字词层 4 一律给 `Derived(0b1)`。
#[test]
fn umlaut_normalization_leaves_nl_v_alone() {
    let e = engine("umlaut_nl");
    assert_eq!(
        e.resolve_boundary("nv", "女"),
        BoundaryResolution::Exact(0b1),
        "`nv` 不得被归一成 `nu`——那会让层 2 点查落空"
    );
}
