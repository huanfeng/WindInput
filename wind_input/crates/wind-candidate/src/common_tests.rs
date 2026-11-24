/// 与 [`CommonChars::from_base`] 配套的 registry：出厂 `common_han` 类的形态
/// ——作用域 = 汉字域、名单 = 这些字、名单内常用、域内名单外生僻。
///
/// 生产路径上这两者分别来自 `schemas/common_chars.txt` 与
/// `charsets/common_han.yaml`（后者的 `file:` 就指向前者），测试里一并造出来。
fn han_class(chars: impl IntoIterator<Item = char>) -> crate::CharsetRegistry {
    let (reg, dropped) = crate::CharsetRegistry::compile(vec![crate::ClassSpec {
        key: "common_han".into(),
        members: chars.into_iter().map(|c| c.to_string()).collect(),
        scope: Some(crate::Scope::Han),
        default_common: Some(true),
        outside_common: Some(false),
        ..Default::default()
    }]);
    assert!(dropped.is_empty());
    reg
}

/// 造「基表 + 与之配套的 registry」。判定要两份数据，测试里总是成对出现。
fn base(chars: impl IntoIterator<Item = char> + Clone) -> (CommonChars, crate::CharsetRegistry) {
    (CommonChars::from_base(chars.clone()), han_class(chars))
}

use super::*;

#[test]
fn test_string_common() {
    let (cc, cs) = base(['我', '们']);
    assert!(cc.is_string_common("我们", &cs)); // 全部常用
    assert!(!cc.is_string_common("我鬱", &cs)); // 含生僻
    assert!(!cc.is_string_common("", &cs)); // 空串
    assert!(cc.is_string_common("我!", &cs)); // 非汉字忽略
}

#[test]
fn test_cjk_punct_ignored() {
    // 判定域按语义而非 Unicode 块邻接：CJK 符号和标点不查表，否则含顿号的词条会被滤掉。
    let (cc, cs) = base(['我', '们']);

    assert!(cc.is_string_common("、", &cs)); // 顿号单条词条（本次上报的现象）
    assert!(cc.is_string_common("我、们", &cs)); // 混排：标点不再拖累整词判定
    for s in ["。", "《", "》", "「", "」", "〇", "；", "："] {
        assert!(cc.is_string_common(s, &cs), "CJK 标点应忽略: {s}");
    }
    // 判定自洽：同为中文标点，落不落进旧区间都该同一结果（旧实现下 、=false 而 ，=true）
    assert_eq!(
        cc.is_string_common("、", &cs),
        cc.is_string_common("，", &cs)
    );
    // 带圈/兼容符号与假名同属辅助字符，规范汉字表管不着
    assert!(cc.is_string_common("①", &cs));
    assert!(cc.is_string_common("℃", &cs));
    assert!(cc.is_string_common("あ", &cs));
    // 真汉字仍按表判定，未被本次放宽波及
    assert!(!cc.is_string_common("我鬱", &cs));
    assert!(!cc.is_string_common("、鬱", &cs)); // 标点忽略，但同串里的生僻字照旧拦下
}

#[test]
fn test_pua_not_common() {
    // 回归：五笔 dwi 下 U+E831（PUA）冒充生僻字混进常用字档。PUA 被本码表当汉字用，
    // 不在规范字表内即非常用；emoji/符号等真辅助字符仍忽略。
    let (cc, cs) = base(['仄']);
    assert!(cc.is_string_common("仄", &cs)); // 真汉字在表内
    assert!(!cc.is_string_common("\u{E831}", &cs)); // PUA 单字：非常用（正是 dwi 豆腐候选）
    assert!(!cc.is_string_common("仄\u{E831}", &cs)); // 含 PUA 的混合串亦非常用
    assert!(cc.is_string_common("仄😀", &cs)); // emoji（U+1F600）非汉字：忽略，不影响判定
}

/// 用户覆盖两个方向都要生效，且要能穿透到整串判定。
#[test]
fn overrides_win_over_base_in_both_directions() {
    let (mut cc, cs) = base(['我', '们']);
    assert!(cc.is_char_common('我', &cs));
    assert!(!cc.is_char_common('鬱', &cs));

    cc.set_overrides([("我".into(), false), ("鬱".into(), true)]);
    assert!(!cc.is_char_common('我', &cs), "常用字降级为生僻");
    assert!(cc.is_char_common('鬱', &cs), "生僻字升级为常用");
    // 整串判定走同一条路：降级后的字会拖累整个词。
    assert!(!cc.is_string_common("我们", &cs));
    assert!(cc.is_string_common("鬱", &cs));
    // 出厂判定不受覆盖影响——界面要靠它显示「出厂 → 现在」的对照。
    assert!(cc.is_base_common('我', &cs));
    assert!(!cc.is_base_common('鬱', &cs));
    assert_eq!(cc.override_of('我'), Some(false));
    assert_eq!(cc.override_of('们'), None);
}

/// `set_overrides` 是整体替换：撤销掉的那条必须真的消失。
///
/// 实现成增量合并的话，「恢复出厂」要到下次重灌才生效。
#[test]
fn set_overrides_replaces_rather_than_merges() {
    let (mut cc, cs) = base(['我']);
    cc.set_overrides([("我".into(), false), ("鬱".into(), true)]);
    assert!(!cc.is_char_common('我', &cs));

    // 用户撤销了「我」那条，store 全量读出来只剩「鬱」。
    cc.set_overrides([("鬱".into(), true)]);
    assert!(cc.is_char_common('我', &cs), "撤销后回到出厂判定");
    assert!(cc.is_char_common('鬱', &cs));
}

/// `is_empty` 只看出厂基表：它是上层「退化为不过滤」的判据。
///
/// 把覆盖也算进去的话，出厂表缺失而用户设过一个字时过滤照常进行，几乎所有字被判生僻。
#[test]
fn is_empty_ignores_overrides() {
    let (mut cc, _cs) = base([]);
    assert!(cc.is_empty());
    cc.set_overrides([("鬱".into(), true)]);
    assert!(cc.is_empty(), "有覆盖也仍算数据缺失");
}

/// 判定域自洽：`is_common_scope` 放行的字符，正是 `is_string_common` 会去查表的那些。
///
/// 域内未表态的字查表，域外未表态的字被忽略。写端准入是更宽的 `is_markable`，
/// 二者关系由 `markable_char_takes_effect` 钉。
#[test]
fn common_scope_matches_string_judgement() {
    let (cc, cs) = base([]);
    for ch in ['我', '鬱', '\u{E831}', '\u{20000}', '氵'] {
        assert!(is_common_scope(ch), "{ch} 应在默认字表管辖域内");
        // 域内且不在表里 ⇒ 整串判非常用。
        assert!(
            !cc.is_string_common(&ch.to_string(), &cs),
            "{ch} 应判非常用"
        );
    }
    for ch in ['、', '，', '①', '℃', 'あ', '😀', 'A', '7'] {
        assert!(!is_common_scope(ch), "{ch} 应在默认字表管辖域外");
        // 域外且用户没表过态 ⇒ 被忽略，不拖累判定。
        assert!(cc.is_string_common(&ch.to_string(), &cs), "{ch} 应被忽略");
    }
}

/// Unicode 17 的扩展 J（`U+323B0` 起）、扩展 I 与兼容汉字补充必须在管辖域内；
/// 落到域外会恒判常用，常用字档放行一批无字形的生僻字。
#[test]
fn supplementary_ideographic_planes_are_governed_wholesale() {
    let (cc, cs) = base([]);
    for (ch, what) in [
        ('\u{323B0}', "扩展 J 首字（Unicode 17 新增）"),
        ('\u{3347F}', "扩展 J 末字"),
        ('\u{2EBF0}', "扩展 I 首字"),
        ('\u{2F800}', "兼容汉字补充"),
        ('\u{3FFFF}', "平面 3 末尾（为将来的扩展 K/L 兜底）"),
    ] {
        assert!(is_common_scope(ch), "{what} 应在管辖域内");
        assert!(
            !cc.is_string_common(&ch.to_string(), &cs),
            "{what} 应判非常用"
        );
    }
}

/// 写端放行的字符，设成生僻后读端一定判非常用。
///
/// 写端准入宽于读端作用域，两者的关系是「写得进去就一定生效」，否则库里会躺着
/// 永不被查询的死记录。
#[test]
fn markable_char_takes_effect() {
    // 全都在 `is_common_scope` 之外，是用户要能单独设为生僻的几类。
    let outside = ['ㄅ', 'ㆠ', '⿰', 'あ', '、', '㈱', '😀', 'A'];
    for ch in outside {
        assert!(!is_common_scope(ch), "{ch} 本就在默认字表管辖域外");
        assert!(is_markable(ch), "{ch} 应可登记");

        let (mut cc, cs) = base([]);
        cc.set_overrides([(ch.to_string(), false)]);
        assert!(
            !cc.is_string_common(&ch.to_string(), &cs),
            "{ch} 设为生僻后必须判非常用，否则就是一条死记录"
        );
        assert!(cc.has_user_rare(&ch.to_string()), "{ch} 应认作用户显式降级");

        // 反向：设为常用同样被认，且不拖累整串。
        cc.set_overrides([(ch.to_string(), true)]);
        assert!(
            cc.is_string_common(&ch.to_string(), &cs),
            "{ch} 设为常用应放行"
        );
    }

    // 空白与控制字符是唯一的例外，理由是数据卫生而非作用域。
    for ch in [' ', '\t', '\u{3000}', '\u{0}'] {
        assert!(!is_markable(ch), "U+{:04X} 不该可登记", ch as u32);
    }
}

/// 用户眼里的「一个字符」= 一个字素簇，码位数可以是 1、2、3、5……
///
/// 词库里的 emoji 常带 FE0F（如 `⚽️`），按 `chars().count() == 1` 判会拒掉它。别自己列
/// 「跳过修饰码位」之类规则（`👨‍👩‍👧`、`🇨🇳`、`1️⃣` 仍会漏），用 UAX #29 字素簇。
#[test]
fn one_grapheme_is_one_char_regardless_of_code_points() {
    for (text, what) in [
        ("\u{26BD}\u{FE0F}", "变体选择符（用户报的 ⚽️）"),
        ("\u{26BD}", "裸形态"),
        ("\u{1F44D}\u{1F3FB}", "肤色修饰符"),
        (
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "ZWJ 家庭序列",
        ),
        ("\u{1F1E8}\u{1F1F3}", "区域指示符对（国旗）"),
        ("\u{0031}\u{FE0F}\u{20E3}", "keycap"),
        ("我", "普通汉字"),
        ("ㄅ", "注音"),
    ] {
        assert_eq!(
            single_markable_char(text),
            Some(text),
            "{what} 应算作一个字符，且键就是整簇"
        );
    }

    // 多个字素簇一律拒绝：常用性是单字属性，取首簇会让用户以为整词都标记了。
    for text in ["我们", "abc", "\u{1F468}\u{1F469}"] {
        assert_eq!(single_markable_char(text), None, "{text:?} 是多个字符");
    }
    assert_eq!(single_markable_char(""), None);
    assert_eq!(single_markable_char(" "), None, "空白不可登记");
}

/// 多码位簇登记后，读端必须对整簇生效。
///
/// 键是整簇，`⚽` 与 `⚽️` 是两个键；候选文本来自词库、形态固定，设与查不会错过。
#[test]
fn multi_code_point_cluster_override_takes_effect() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let (mut cc, cs) = base([]);
    cc.set_overrides([(family.to_string(), false)]);
    assert!(!cc.is_string_common(family, &cs), "整个 ZWJ 序列应判非常用");
    assert!(cc.has_user_rare(family));
    // 序列里的单个成员不受牵连：用户关的是这个组合，不是所有 👨。
    assert!(
        cc.is_string_common("\u{1F468}", &cs),
        "只登记了组合，单个成员不该跟着被判非常用"
    );

    // 混在句子里也认得出来。
    assert!(!cc.is_string_common(&format!("一家{family}"), &cs));
}

/// 按字判与按串判必须一致，域外字符尤其。
///
/// 两条路对同一个 `、` 给出相反答案的话，词库管理页显示「常用」而候选窗已把它滤掉。
/// 写端「与默认同向就删覆盖」也依赖这份一致（见 `is_base_common`）。
#[test]
fn char_and_string_judgements_agree() {
    let (mut cc, cs) = base(['我']);
    cc.set_overrides([("、".into(), false), ("鬱".into(), true)]);
    // 覆盖过的、没覆盖的、域内的、域外的各来一遍。
    for ch in [
        '我',
        '鬱',
        '、',
        '，',
        'あ',
        'ㄅ',
        '⿰',
        '😀',
        'A',
        '\u{323B0}',
    ] {
        assert_eq!(
            cc.is_char_common(ch, &cs),
            cc.is_string_common(&ch.to_string(), &cs),
            "{ch} 的按字判与按串判不一致"
        );
    }
}

/// 覆盖优先于默认字表——两个方向都要。
#[test]
fn override_wins_over_base_table() {
    let (mut cc, cs) = base(['我']);
    assert!(cc.is_string_common("我", &cs));
    cc.set_overrides([("我".into(), false)]);
    assert!(
        !cc.is_string_common("我", &cs),
        "默认常用的字被降级后须判非常用"
    );

    let (mut cc, cs) = base([]);
    cc.set_overrides([("鬱".into(), true)]);
    assert!(
        cc.is_string_common("鬱", &cs),
        "默认生僻的字被提升后须判常用"
    );
}

/// 全表列举：按字表原序，用户加的字追加在后。
///
/// `common_chars.txt` 按级别拼接，设置页分页浏览依赖这个顺序；从 `HashSet` 迭代会让每页内容随机变。
#[test]
fn list_all_keeps_table_order_and_appends_extras() {
    let (mut cc, cs) = base(['一', '乙', '二']);
    assert_eq!(
        cc.list_all(&cs),
        vec![
            ("一".to_string(), true, true),
            ("乙".to_string(), true, true),
            ("二".to_string(), true, true)
        ],
        "无覆盖时＝字表原序，且默认与现在一致"
    );

    // 一个降级（在表内）+ 一个新增（不在表内）。
    cc.set_overrides([("乙".into(), false), ("槮".into(), true)]);
    assert_eq!(
        cc.list_all(&cs),
        vec![
            ("一".to_string(), true, true),
            // 默认仍是常用，现在被改成生僻——两个值都要保留，界面靠差异显示对照。
            ("乙".to_string(), true, false),
            ("二".to_string(), true, true),
            // 表外字追加在最后，默认判定恒 false（＝出厂表里没有它）。
            ("槮".to_string(), false, true),
        ]
    );
}

/// 表外字按码位排序追加：`HashMap` 迭代序随机，直接追加会让这批字每次刷新都换位置。
#[test]
fn list_all_orders_extras_deterministically() {
    let (mut cc, cs) = base(['一']);
    cc.set_overrides([
        ("鬱".into(), true),
        ("槮".into(), true),
        ("乂".into(), true),
    ]);
    let extras: Vec<String> = cc
        .list_all(&cs)
        .into_iter()
        .skip(1)
        .map(|(c, _, _)| c)
        .collect();
    let mut sorted = extras.clone();
    sorted.sort_unstable();
    assert_eq!(extras, sorted, "表外字必须按码位升序，顺序才稳定");
}

/// 字表里的重复字只列一次——历史数据里难免有重复，列两行一模一样的字很怪。
#[test]
fn list_all_dedupes_repeated_chars() {
    let (cc, cs) = base(['一', '乙', '一']);
    assert_eq!(cc.list_all(&cs).len(), 2);
}

/// 域外字符的覆盖生效：这是写端全范围放开的读端前提。
#[test]
fn overrides_on_out_of_scope_chars_take_effect() {
    let (mut cc, cs) = base(['我']);
    cc.set_overrides([("、".into(), false), ("😀".into(), false)]);
    assert!(
        !cc.is_string_common("、", &cs),
        "用户把顿号设成生僻，就该判非常用"
    );
    assert!(
        !cc.is_string_common("我😀", &cs),
        "整串含一个被降级的字符即判非常用"
    );
    // 没被表过态的域外字符照旧忽略：零回归是放开的前提。
    assert!(cc.is_string_common("我，", &cs));
}

/// 行尾不挑食：带 `#` 注释头的 CR 文件若整份被当成注释，空表会让上层退化成不过滤。
///
/// 样本在代码里构造，不要改用仓库里的 fixture（git core.autocrlf 会改行尾）。
#[test]
fn line_endings_do_not_change_the_result() {
    let dir = std::env::temp_dir().join(format!("wind-common-eol-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for lf in ["李\n樱\n河\n", "# 我的常用字表\n李\n樱\n河\n"] {
        for (tag, text) in [
            ("LF", lf.to_string()),
            ("CRLF", lf.replace('\n', "\r\n")),
            ("CR", lf.replace('\n', "\r")),
        ] {
            let p = dir.join(format!("{tag}.txt"));
            std::fs::write(&p, &text).unwrap();
            let c = CommonChars::load(&p);
            let _ = std::fs::remove_file(&p);
            assert!(!c.is_empty(), "{tag}: 字表不该是空的（样本 {lf:?}）");
            for ch in ['李', '樱', '河'] {
                assert!(c.base.contains(&ch), "{tag}: 缺字 {ch}");
            }
        }
    }
    let _ = std::fs::remove_dir(&dir);
}
