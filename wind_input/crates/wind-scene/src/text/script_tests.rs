use super::*;

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 键帽与 FE0F 序列必须整串落在 emoji 段里，连字才成得了；否则三格各归各家，
/// 画面上是普通的「1」加一个空框（基准字体自己有「1」，回退不触发）。
#[test]
fn keycap_and_fe0f_sequences_form_one_emoji_run() {
    let emoji = [ScriptClass::Emoji];
    let all_emoji = |runs: &[FontRun], len: usize| {
        runs.len() == 1 && runs[0].len == len && runs[0].class == Some(ScriptClass::Emoji)
    };
    // 1️⃣ = 0031 FE0F 20E3；不带 FE0F 的 1⃣ 同样成立。
    assert!(all_emoji(&font_runs(&w("1\u{FE0F}\u{20E3}"), &emoji), 3));
    assert!(all_emoji(&font_runs(&w("1\u{20E3}"), &emoji), 2));
    assert!(all_emoji(&font_runs(&w("#\u{FE0F}\u{20E3}"), &emoji), 3));
    // ▶️：文本码位（中性）+ FE0F 表情呈现选择符。
    assert!(all_emoji(&font_runs(&w("\u{25B6}\u{FE0F}"), &emoji), 2));
    // 夹在文字中间：只有序列本身切出来，前后段不受影响、不吞字。
    let runs = font_runs(&w("第1\u{FE0F}\u{20E3}名"), &emoji);
    let classes: Vec<_> = runs.iter().map(|r| r.class).collect();
    assert_eq!(classes, [None, Some(ScriptClass::Emoji), None]);
    assert_eq!(runs[1].start, 1);
    assert_eq!(runs[1].len, 3);
    // 同时声明了 digits：键帽仍赢——连字是比「这是个数字」更具体的事实。
    let runs = font_runs(
        &w("1\u{FE0F}\u{20E3}"),
        &[ScriptClass::Emoji, ScriptClass::Digits],
    );
    assert!(all_emoji(&runs, 3), "{runs:?}");
    // 裸数字不受影响：声明了 digits 就归 digits。
    let runs = font_runs(&w("12"), &[ScriptClass::Emoji, ScriptClass::Digits]);
    assert_eq!(runs[0].class, Some(ScriptClass::Digits));
}

/// 三条不该被改写的情形：FE0E 文本选择符、未声明 emoji、强脚本基字。
#[test]
fn emoji_rewrite_leaves_text_selector_and_strong_bases_alone() {
    let emoji = [ScriptClass::Emoji];
    // U+FE0E 是文本呈现选择符，语义与 FE0F 相反，不得提升。
    assert!(
        font_runs(&w("#\u{FE0E}"), &emoji)
            .iter()
            .all(|r| r.class.is_none())
    );
    // 未声明 emoji：一切照旧（键帽仍是三格各归各家，不切 emoji 段）。
    assert!(
        font_runs(&w("1\u{FE0F}\u{20E3}"), &[ScriptClass::Latin])
            .iter()
            .all(|r| r.class != Some(ScriptClass::Emoji))
    );
    // 汉字后面跟了个孤零零的 FE0F：不是任何 emoji 序列，汉字不许被搬走。
    let runs = font_runs(&w("文\u{FE0F}"), &[ScriptClass::Emoji, ScriptClass::Cjk]);
    assert_eq!(runs[0].class, Some(ScriptClass::Cjk), "{runs:?}");
}

/// 直立竖排切格：一个基字 + 粘着它的记号。
///
/// 第二条断言是重点：异体选择符若单独成格，那一格排出来是空的，前一格的字还会退回默认写法。
#[test]
fn upright_cells_keep_marks_with_their_base() {
    assert_eq!(upright_cells("你好世界"), ["你", "好", "世", "界"]);
    // 汉字异体选择符（U+E0101）必须并进前一格。
    assert_eq!(
        upright_cells("\u{8FB6}\u{E0101}好"),
        ["\u{8FB6}\u{E0101}", "好"]
    );
    // 日文浊点。
    assert_eq!(upright_cells("か\u{3099}な"), ["か\u{3099}", "な"]);
    // 拉丁组合音调。
    assert_eq!(upright_cells("e\u{0301}f"), ["e\u{0301}", "f"]);
    // 代理对（补充平面汉字）本就是一个 char，不会被切开。
    assert_eq!(upright_cells("\u{20000}字"), ["\u{20000}", "字"]);
    assert_eq!(upright_cells(""), Vec::<&str>::new());
    // 逐格拼回去必须还是原文——切分不得吞字。
    for s in [
        "你好",
        "a\u{0301}b\u{0302}",
        "\u{1F1E8}\u{1F1F3}x",
        "1. ᠮᠣᠩᠭᠣᠯ",
    ] {
        assert_eq!(upright_cells(s).concat(), s, "{s:?} 切分后拼不回原文");
    }
}

/// emoji 组合序列（ZWJ 连接、区域指示符成对）不得被拆散。
#[test]
fn upright_cells_keep_emoji_sequences_whole() {
    // 国旗 = 两个区域指示符。
    assert_eq!(upright_cells("\u{1F1E8}\u{1F1F3}"), ["\u{1F1E8}\u{1F1F3}"]);
    // 两面国旗相邻：必须切成两格，不能四个挤成一格。
    assert_eq!(
        upright_cells("\u{1F1E8}\u{1F1F3}\u{1F1EF}\u{1F1F5}"),
        ["\u{1F1E8}\u{1F1F3}", "\u{1F1EF}\u{1F1F5}"]
    );
    // ZWJ 序列（家庭 emoji 的一段）。
    assert_eq!(
        upright_cells("\u{1F468}\u{200D}\u{1F469}"),
        ["\u{1F468}\u{200D}\u{1F469}"]
    );
}

fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 取每段对应的原文与归属，方便断言。
fn seg(s: &str, declared: &[ScriptClass]) -> Vec<(String, Option<ScriptClass>)> {
    let w = u16s(s);
    font_runs(&w, declared)
        .into_iter()
        .map(|r| {
            (
                String::from_utf16_lossy(&w[r.start..r.start + r.len]),
                r.class,
            )
        })
        .collect()
}

/// 二分查找依赖有序且不相交；表是手写的，这条必须钉住。
#[test]
fn table_is_sorted_and_disjoint() {
    for w in TABLE.windows(2) {
        assert!(w[0].0 <= w[0].1, "区间自身颠倒: {:#X?}", w[0]);
        assert!(
            w[0].1 < w[1].0,
            "未按 start 升序或区间重叠: {:#X?} 与 {:#X?}",
            w[0],
            w[1]
        );
    }
    let last = TABLE[TABLE.len() - 1];
    assert!(last.0 <= last.1);
}

/// 段必须无缝、不重叠地覆盖整个输入，否则有文字没渲染或被渲染两次，且都不报错。
#[test]
fn runs_tile_the_input() {
    let samples = [
        "",
        "a",
        "中",
        "1. ᠮᠣᠩᠭᠣᠯ",
        "abc中文123 ᠮᠣᠩ🙂é",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "e\u{0301}cole",
        "。、！？",
    ];
    for s in samples {
        let w = u16s(s);
        for declared in [
            &[][..],
            &[ScriptClass::Latin][..],
            &[ScriptClass::Latin, ScriptClass::Digits, ScriptClass::Punct][..],
            ScriptClass::ALL,
        ] {
            let runs = font_runs(&w, declared);
            let mut pos = 0usize;
            for r in &runs {
                assert_eq!(r.start, pos, "段不连续: {s:?} / {declared:?} / {runs:?}");
                assert!(r.len > 0, "空段: {s:?} / {runs:?}");
                pos += r.len;
            }
            assert_eq!(pos, w.len(), "未覆盖到末尾: {s:?} / {runs:?}");
        }
    }
}

/// 零配置快路径：没声明任何指派就不该切段（每段一次 COM 调用，白切是纯开销）。
#[test]
fn no_declaration_yields_single_run() {
    assert_eq!(
        seg("abc中文123", &[]),
        vec![("abc中文123".to_string(), None)]
    );
}

/// 声明了 latin：拉丁字母切出去，汉字留默认。
#[test]
fn latin_is_split_out_when_declared() {
    assert_eq!(
        seg("中abc文", &[ScriptClass::Latin]),
        vec![
            ("中".to_string(), None),
            ("abc".to_string(), Some(ScriptClass::Latin)),
            ("文".to_string(), None),
        ]
    );
}

/// 未声明的具名类必须并入默认链，不能变成中性——变成中性就会被邻居传染。
/// 只声明 latin 时，汉字（Cjk 类）要老老实实待在默认链里。
#[test]
fn undeclared_class_joins_default_not_neutral() {
    // 若 Cjk 未声明时被当成中性，"中" 会继承前面的 "a" 而跟着拉丁字体跑。
    assert_eq!(
        seg("a中", &[ScriptClass::Latin]),
        vec![
            ("a".to_string(), Some(ScriptClass::Latin)),
            ("中".to_string(), None),
        ]
    );
}

/// 中性字符默认继承上下文：中文里的半角数字/标点不得跳到英文字体。
#[test]
fn neutrals_inherit_context_by_default() {
    assert_eq!(
        seg("中文123。", &[ScriptClass::Latin]),
        vec![("中文123。".to_string(), None)]
    );
    // 前向优先：拉丁后面的数字跟拉丁走。
    assert_eq!(
        seg("abc123", &[ScriptClass::Latin]),
        vec![("abc123".to_string(), Some(ScriptClass::Latin))]
    );
}

/// 显式声明 digits 才打破继承。
#[test]
fn declaring_digits_breaks_inheritance() {
    assert_eq!(
        seg("中文123", &[ScriptClass::Digits]),
        vec![
            ("中文".to_string(), None),
            ("123".to_string(), Some(ScriptClass::Digits)),
        ]
    );
}

/// 蒙古文数字 `U+1810..=U+1819` 绝不能被 `digits` 类切走——切走就是在蒙文词
/// 中间断开成形。这正是「分类轴必须是脚本、不能是字符功能」那条判据的落点。
#[test]
fn mongolian_digits_are_never_split_by_the_digits_class() {
    // ᠐᠑ 是蒙古文数字 0/1，夹在蒙古文字母之间。
    let s = "\u{1820}\u{1810}\u{1811}\u{1821}";
    assert_eq!(
        seg(s, &[ScriptClass::Digits, ScriptClass::Latin]),
        vec![(s.to_string(), None)],
        "蒙古文数字被切出去了——成形会断"
    );
}

/// 蒙古文整串（含自由变体选择符 FVS）是一个强归属段，不被拉丁指派打断。
#[test]
fn mongolian_run_stays_intact() {
    // ᠮᠣᠩᠭᠣᠯ + FVS1
    let s = "\u{182E}\u{1823}\u{1829}\u{182D}\u{1823}\u{182F}\u{180B}";
    assert_eq!(seg(s, &[ScriptClass::Latin]), vec![(s.to_string(), None)]);
}

/// 跨脚本的组合符必须跟随基字符：`e` + U+0301 不能被切成两段，否则重音会分裂。
#[test]
fn combining_mark_sticks_to_its_base() {
    let s = "\u{0065}\u{0301}中";
    assert_eq!(
        seg(s, &[ScriptClass::Latin]),
        vec![
            ("\u{0065}\u{0301}".to_string(), Some(ScriptClass::Latin)),
            ("中".to_string(), None),
        ]
    );
}

/// ZWJ 表情序列不得被切开（切开会退化成三个独立人像）。
#[test]
fn zwj_emoji_sequence_is_one_run() {
    let s = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    assert_eq!(
        seg(s, &[ScriptClass::Emoji, ScriptClass::Latin]),
        vec![(s.to_string(), Some(ScriptClass::Emoji))]
    );
}

/// 变体选择符跟随前一个码位（keycap `1️⃣` = 数字 + VS16 + U+20E3）。
#[test]
fn variation_selector_and_keycap_stick() {
    let s = "\u{0031}\u{FE0F}\u{20E3}";
    assert_eq!(
        seg(s, &[ScriptClass::Digits]),
        vec![(s.to_string(), Some(ScriptClass::Digits))]
    );
}

/// 代理对不得被切开：段边界只落在码位边界上。
#[test]
fn surrogate_pairs_are_never_split() {
    // U+20000 是扩展 B 汉字（代理对），两侧是拉丁。
    let s = "a\u{20000}b";
    let w = u16s(s);
    let runs = font_runs(&w, &[ScriptClass::Latin, ScriptClass::Cjk]);
    for r in &runs {
        let head = w[r.start];
        assert!(
            !(0xDC00..=0xDFFF).contains(&head),
            "段起点落在低位代理上: {runs:?}"
        );
    }
    assert_eq!(
        seg(s, &[ScriptClass::Latin, ScriptClass::Cjk]),
        vec![
            ("a".to_string(), Some(ScriptClass::Latin)),
            ("\u{20000}".to_string(), Some(ScriptClass::Cjk)),
            ("b".to_string(), Some(ScriptClass::Latin)),
        ]
    );
}

/// 整串都是中性时用默认链（没有可继承的强归属）。
///
/// 单独这一条分不出空格是中性还是强归属，故配一条对照：空格夹在已声明的类中间时必须被吸收进同一段。
#[test]
fn all_neutral_falls_back_to_default() {
    assert_eq!(
        seg("   ", &[ScriptClass::Latin]),
        vec![("   ".to_string(), None)]
    );
    assert_eq!(
        seg("a b", &[ScriptClass::Latin]),
        vec![("a b".to_string(), Some(ScriptClass::Latin))],
        "空格成了强归属：会把一段拉丁切成三段"
    );
}

/// 段首的中性字符没有前驱，回退到后向继承。
#[test]
fn leading_neutral_inherits_backward() {
    assert_eq!(
        seg("  abc", &[ScriptClass::Latin]),
        vec![("  abc".to_string(), Some(ScriptClass::Latin))]
    );
}

/// 硬换行 `\n` 必须中性——它若成为强归属就会把一行多行候选切成互不相干的段。
///
/// 样本两侧必须是已声明的类：两侧都在默认链的话，`\n` 无论中性还是强归属都并成单段，钉不住。
#[test]
fn newline_is_neutral() {
    assert_eq!(
        seg("a\nb", &[ScriptClass::Latin]),
        vec![("a\nb".to_string(), Some(ScriptClass::Latin))],
        "\\n 成了强归属：多行候选会被切成互不相干的段"
    );
}

/// 表外的强归属必须真的是强归属：未列脚本（蒙古文）与已声明类（拉丁）相邻时必须各自成段。
/// 守的是 `font_runs` 里 `Raw::StrongOther => Some(None)` 那一臂；整串同类的用例在它被改成
/// `Neutral(None)` 时照样全绿。
#[test]
fn undeclared_script_stays_strong_next_to_a_declared_class() {
    let mongolian = "\u{1820}\u{1821}";
    assert_eq!(
        seg(&format!("abc{mongolian}"), &[ScriptClass::Latin]),
        vec![
            ("abc".to_string(), Some(ScriptClass::Latin)),
            (mongolian.to_string(), None),
        ],
        "蒙古文被当成中性、跟着拉丁指派跑了"
    );
    // 反向（蒙文在前）同样成立——继承是前向优先，别让方向掩盖问题。
    assert_eq!(
        seg(&format!("{mongolian}abc"), &[ScriptClass::Latin]),
        vec![
            (mongolian.to_string(), None),
            ("abc".to_string(), Some(ScriptClass::Latin)),
        ]
    );
}

/// Common 符号块必须中性，否则 `abc→def` 会被切成三段、中间那段掉回默认链。
/// `第①条` 那种要声明 `cjk` 才暴露，故两种都测。
#[test]
fn common_symbols_are_neutral_and_do_not_split_runs() {
    // 箭头夹在拉丁中间：整串一段。
    assert_eq!(
        seg("abc→def", &[ScriptClass::Latin]),
        vec![("abc→def".to_string(), Some(ScriptClass::Latin))]
    );
    // 带圈数字夹在汉字中间：声明 cjk 时整串仍是一段。
    assert_eq!(
        seg("第①条", &[ScriptClass::Cjk, ScriptClass::Latin]),
        vec![("第①条".to_string(), Some(ScriptClass::Cjk))]
    );
    // 货币符号、℃、补充标点同理。
    assert_eq!(
        seg("€100", &[ScriptClass::Latin, ScriptClass::Digits]),
        vec![("€100".to_string(), Some(ScriptClass::Digits))]
    );
    assert_eq!(
        seg("25℃", &[ScriptClass::Cjk]),
        vec![("25℃".to_string(), None)]
    );
}

/// emoji tag 序列（英格兰旗）不得被切开——与 ZWJ 序列完全同型的失效。
#[test]
fn emoji_tag_sequence_is_one_run() {
    let flag = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
    assert_eq!(
        seg(flag, &[ScriptClass::Emoji, ScriptClass::Latin]),
        vec![(flag.to_string(), Some(ScriptClass::Emoji))],
        "tag 序列被切开：旗帜会退化成一面纯黑旗"
    );
}

/// 国旗由两个区域指示符组成（U+1F1E6..1F1FF），必须与其它 emoji 同属 Emoji 类，
/// 否则同一行里两种 emoji 用两种字体、度量不一致。
#[test]
fn regional_indicator_flags_belong_to_the_emoji_class() {
    let s = "\u{1F1E8}\u{1F1F3}\u{1F642}"; // 🇨🇳🙂
    assert_eq!(
        seg(s, &[ScriptClass::Emoji]),
        vec![(s.to_string(), Some(ScriptClass::Emoji))]
    );
}

/// U+FEFF（ZWNBSP/BOM）夹在词中间不得切一刀。
#[test]
fn zwnbsp_does_not_split_a_word() {
    assert_eq!(
        seg("ab\u{FEFF}cd", &[ScriptClass::Latin]),
        vec![("ab\u{FEFF}cd".to_string(), Some(ScriptClass::Latin))]
    );
}

/// 西里尔扩展 A 整块都是组合符，必须粘连——否则声明 cyrillic 时重音与基字符分裂。
/// 它证伪了「组合符总与基字符同脚本块」这个直觉（该块不在 U+0400..052F 内）。
#[test]
fn cyrillic_combining_extension_sticks() {
    let s = "\u{0430}\u{2DE0}"; // а + 西里尔组合字母
    assert_eq!(
        seg(s, &[ScriptClass::Cyrillic]),
        vec![(s.to_string(), Some(ScriptClass::Cyrillic))]
    );
}

/// 注音声调符（U+02C7 ˇ 等）是 Common 不是 Latin：声明 latin 时不得把它从注音串切走。
#[test]
fn bopomofo_tone_marks_are_not_latin() {
    assert_eq!(
        seg("\u{3123}\u{02C7}", &[ScriptClass::Latin, ScriptClass::Cjk]),
        vec![("\u{3123}\u{02C7}".to_string(), Some(ScriptClass::Cjk))]
    );
}

/// 落单代理走 `next_cp` 的兜底臂——`&str` 造不出这种输入，必须直接喂 `&[u16]`，
/// 否则那条兜底分支「文档承诺了、测试碰不到」。
#[test]
fn lone_surrogates_do_not_panic_or_misalign() {
    for w in [
        vec![0xD800u16, 0x0061],         // 落单高位代理 + 'a'
        vec![0x0061u16, 0xDC00],         // 'a' + 落单低位代理
        vec![0xDC00u16],                 // 只有一个低位代理
        vec![0xD800u16, 0xD800, 0x4E2D], // 连续两个高位代理 + 汉字
    ] {
        let runs = font_runs(&w, &[ScriptClass::Latin, ScriptClass::Cjk]);
        let total: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, w.len(), "落单代理下段长总和对不上: {runs:?}");
        assert_eq!(runs.first().map(|r| r.start), Some(0));
    }
}

/// 全角标点归 CJK 而不是 Punct：声明 punct 指派英文字体时，「。」不得跟着跑。
#[test]
fn fullwidth_punct_belongs_to_cjk() {
    assert_eq!(
        seg("中文。", &[ScriptClass::Punct, ScriptClass::Latin]),
        vec![("中文。".to_string(), None)]
    );
}

/// 私用区（拆字字根）落默认链——它随后会被 chaizi 的字体集覆盖，不该被脚本指派抢走。
#[test]
fn private_use_area_falls_to_default() {
    assert_eq!(
        seg("\u{E0E1}\u{E0E2}", &[ScriptClass::Latin, ScriptClass::Cjk]),
        vec![("\u{E0E1}\u{E0E2}".to_string(), None)]
    );
}

// ── FontPlan ──────────────────────────────────────────────────────────

fn chain(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// 归一化：空白字体名剔除、空链整条丢弃、同类后写胜出。
#[test]
fn plan_normalizes_input() {
    let p = FontPlan::new(
        chain(&["  Mongolian Baiti  ", "", "   "]),
        vec![
            (ScriptClass::Latin, chain(&["Consolas"])),
            (ScriptClass::Cjk, chain(&["", "  "])),
            (ScriptClass::Latin, chain(&["Segoe UI", "Arial"])),
        ],
    );
    assert_eq!(p.base_family(), Some("Mongolian Baiti"));
    assert_eq!(p.declared(), &[ScriptClass::Latin], "空链的 cjk 不该被声明");
    assert_eq!(
        p.chain_for(Some(ScriptClass::Latin)),
        ["Segoe UI", "Arial"],
        "同类重复声明应后写胜出"
    );
}

/// 书写顺序不同、内容相同的两份方案必须相等且同 hash：它要进测量缓存的键，顺序敏感会全 miss。
#[test]
fn plan_equality_is_write_order_independent() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let a = FontPlan::new(
        chain(&["A"]),
        vec![
            (ScriptClass::Punct, chain(&["P"])),
            (ScriptClass::Latin, chain(&["L"])),
        ],
    );
    let b = FontPlan::new(
        chain(&["A"]),
        vec![
            (ScriptClass::Latin, chain(&["L"])),
            (ScriptClass::Punct, chain(&["P"])),
        ],
    );
    assert_eq!(a, b);
    let h = |p: &FontPlan| {
        let mut s = DefaultHasher::new();
        p.hash(&mut s);
        s.finish()
    };
    assert_eq!(h(&a), h(&b));
}

/// 平凡方案（≤1 个默认字体、无指派）要能被识别出来——调用方据此完全走旧路径。
#[test]
fn trivial_plan_is_detected() {
    assert!(FontPlan::default().is_trivial());
    assert!(FontPlan::new(chain(&["A"]), vec![]).is_trivial());
    assert!(!FontPlan::new(chain(&["A", "B"]), vec![]).is_trivial());
    assert!(!FontPlan::new(chain(&["A"]), vec![(ScriptClass::Latin, chain(&["L"]))]).is_trivial());
}

/// 未指派的类回落默认链（不 panic），`None` 也取默认链。
#[test]
fn chain_for_falls_back_to_default() {
    let p = FontPlan::new(
        chain(&["A", "B"]),
        vec![(ScriptClass::Latin, chain(&["L"]))],
    );
    assert_eq!(p.chain_for(None), ["A", "B"]);
    assert_eq!(p.chain_for(Some(ScriptClass::Cjk)), ["A", "B"]);
    assert_eq!(p.chain_for(Some(ScriptClass::Latin)), ["L"]);
}

/// 只有真有回退项时才需要构造自定义 fallback：没有就走系统默认，零回归。
#[test]
fn needs_fallback_only_when_a_chain_has_more_than_one() {
    assert!(
        !FontPlan::new(chain(&["A"]), vec![(ScriptClass::Latin, chain(&["L"]))]).needs_fallback()
    );
    assert!(FontPlan::new(chain(&["A", "B"]), vec![]).needs_fallback());
    assert!(
        FontPlan::new(
            chain(&["A"]),
            vec![(ScriptClass::Latin, chain(&["L", "M"]))]
        )
        .needs_fallback()
    );
}

/// `chains()` 覆盖默认链与全部指派链，且不吐空链。
#[test]
fn chains_enumerates_default_first_then_assignments() {
    let p = FontPlan::new(
        chain(&["A"]),
        vec![(ScriptClass::Latin, chain(&["L", "M"]))],
    );
    let got: Vec<Vec<String>> = p.chains().map(|c| c.to_vec()).collect();
    assert_eq!(got, vec![chain(&["A"]), chain(&["L", "M"])]);
    // 默认链为空时不该吐出一条空链（它会被当成「base family 是空串」建映射）。
    let q = FontPlan::new(vec![], vec![(ScriptClass::Latin, chain(&["L"]))]);
    assert_eq!(q.chains().count(), 1);
}

/// 键名 round-trip：设置页与配置文件靠它对齐。
///
/// 遍历 `ALL` 测不出 `ALL` 漏项（match 有穷尽性检查，`ALL` 没有），故显式钉住条数，
/// 加变体时必须一并改这个数字。
#[test]
fn class_keys_round_trip() {
    assert_eq!(ScriptClass::ALL.len(), 7, "加了变体却没写进 ALL");
    for &c in ScriptClass::ALL {
        assert_eq!(ScriptClass::from_key(c.key()), Some(c));
    }
    assert_eq!(ScriptClass::from_key("LATIN"), Some(ScriptClass::Latin));
    assert_eq!(ScriptClass::from_key(" latin "), Some(ScriptClass::Latin));
    assert_eq!(ScriptClass::from_key("mongolian"), None);
}
