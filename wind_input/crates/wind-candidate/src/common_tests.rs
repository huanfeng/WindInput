/// 与 [`CommonChars::from_base`] 配套的 registry：出厂 `common_han` 类的形态
/// ——作用域 = 汉字域、名单 = 这些字、名单内常用、**域内名单外生僻**。
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
    // 回归：用户词库里含中文顿号的词条在「常用字/智能」档被滤掉。
    // 根因＝判定域按 `0x2E80..=0x33FF` 整段圈定，把 CJK 符号和标点区当成必须查表的汉字，
    // 而字表里只有纯汉字。判据现按语义而非 Unicode 块邻接：符号一律忽略。
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
/// 若实现成增量合并，「恢复出厂」在下次重灌前不会生效——症状是点了没反应，
/// 重启后才对，属于最难查的一类（[[project_runtime_mirror_state_config_sync]]）。
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

/// ⚠️ `is_empty` 只看出厂基表：它是上层「退化为不过滤」的判据。
///
/// 把覆盖也算进去的话，出厂表缺失而用户设过一个字时它会返回 false，于是过滤照常
/// 进行——此刻几乎所有字都不在表里、全被判生僻，智能档会把候选滤到只剩那一个字。
#[test]
fn is_empty_ignores_overrides() {
    let (mut cc, _cs) = base([]);
    assert!(cc.is_empty());
    cc.set_overrides([("鬱".into(), true)]);
    assert!(cc.is_empty(), "有覆盖也仍算数据缺失");
}

/// 判定域自洽：`is_common_scope` 放行的字符，正是 `is_string_common` 会去查表的那些。
///
/// 上层拿 `is_common_scope` 做右键菜单的准入。两处一旦漂移，用户就能给一个读端
/// 根本不查的字符（`、`、emoji）存下覆盖，然后发现「设了完全没用」且毫无报错。
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
        // 域外**且用户没表过态** ⇒ 被忽略，不拖累判定。
        assert!(cc.is_string_common(&ch.to_string(), &cs), "{ch} 应被忽略");
    }
}

/// Unicode 17 的扩展 J（`U+323B0` 起）必须在管辖域内。
///
/// 原先 `is_han` 逐块列举到 `0x323AF`（扩展 H 末尾），**只差一个码位**：扩 J 落到域外
/// ⇒ 恒判常用 ⇒ 常用字档、智能档都放行。虎码跟进 Unicode 17 后，用户的常用字候选里
/// 冒出一批无字形的扩 J 生僻字（issue #83）。同一份列举还漏了扩展 I 与兼容汉字补充。
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

/// 写端放行的字符，设成生僻后读端**一定**判非常用。
///
/// 这是「词库管理全范围放开」后取代 `common_scope_matches_string_judgement` 那条
/// 同源性的钉子：准入不再等于读端作用域，两者的关系变成「写得进去就一定生效」。
/// 它防的还是同一件事——库里躺着一条用户以为生效、实际永不被查询的死记录，全程无报错。
#[test]
fn markable_char_takes_effect() {
    // 全都在 `is_common_scope` 之外，正是 issue #83 里用户点名要能关掉的那几类。
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

/// 用户眼里的「一个字符」= 一个**字素簇**，码位数可以是 1、2、3、5……
///
/// 用户报的 `⚽️` 就卡在旧判据上（`chars().count() == 1`），候选右键里根本没有
/// 「设为生僻字」这一项。词库里存的正是带 FE0F 的那个形态
/// （`wubi86_jidian_emoji.dict.yaml` 实测）。
///
/// ⛔ 中间版本改成「跳过修饰码位、数基础字符」——那仍是自己列规则，`👨‍👩‍👧`、`🇨🇳`、
/// `1️⃣` 照样被拒。现在直接用 UAX #29 的字素簇，一次覆盖全部形态。
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

/// 多码位簇登记后，读端必须对**整簇**生效。
///
/// ⚠️ 键是整簇，故 `⚽`(裸) 与 `⚽️`(带 FE0F) 是两个不同的键——用户在候选里点的是哪个
/// 形态就设哪个，而候选文本来自词库，形态是固定的，两者不会互相错过。
#[test]
fn multi_code_point_cluster_override_takes_effect() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let (mut cc, cs) = base([]);
    cc.set_overrides([(family.to_string(), false)]);
    assert!(!cc.is_string_common(family, &cs), "整个 ZWJ 序列应判非常用");
    assert!(cc.has_user_rare(family));
    // 序列里的**单个成员**不受牵连：用户关的是这个组合，不是所有 👨。
    assert!(
        cc.is_string_common("\u{1F468}", &cs),
        "只登记了组合，单个成员不该跟着被判非常用"
    );

    // 混在句子里也认得出来。
    assert!(!cc.is_string_common(&format!("一家{family}"), &cs));
}

/// 按字判与按串判**必须一致**，域外字符尤其。
///
/// 这条自洽性断言防的是一类特别难想到的账对不上：`is_char_common` 走
/// `is_base_common`、`is_string_common` 走「域外即忽略」，两条路对同一个 `、` 若给出
/// 相反答案，词库管理页会显示「常用」而候选窗里它已被滤掉——两边都不报错。
/// 写端的「与默认同向就删覆盖」也建立在这份一致上（见 `is_base_common`）。
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

/// 覆盖优先于默认字表——**两个方向都要**。
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

/// 全表列举：**按字表原序**，用户加的字追加在后。
///
/// 顺序是硬要求：`common_chars.txt` 按级别拼接（一级→二级→三级），设置页分页浏览
/// 全靠它。若从 `HashSet` 迭代，每次启动的顺序都不一样，第 2 页的内容会随机变。
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

/// 域外字符的覆盖**现在生效**（issue #83 放开写端准入的读端前提）。
///
/// 本条取代旧的 `overrides_on_out_of_scope_chars_are_inert`：那条钉的是相反的行为
/// ——域外覆盖形同虚设，因此写端必须按同一个作用域拒绝写入。用户要求词库管理全范围
/// 放开后，那个前提被替换成了「写得进去就一定生效」，两条断言必然互斥，故整条改写而
/// 不是加一条新的。
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

/// 行尾不挑食：带 `#` 注释头的 CR 文件，修之前整份被当成注释 → 空表 →
/// 上层退化成「不过滤」，用户的字表悄无声息地没生效。
///
/// ⚠️ 样本在代码里构造，不要改用仓库里的 fixture（git core.autocrlf 会改行尾）。
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
