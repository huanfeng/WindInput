use super::*;
use wind_config::charset_def::{CharsetDef, parse_doc};

/// 字符类文件的说明行在 wind-config 渲染、在 wind-store 启动补写，两份文案必须一致。
#[test]
fn charset_banner_matches_store_backfill() {
    assert_eq!(
        wind_config::charset_def::STORED_DOC_BANNER,
        wind_store::banner::CHARSETS
    );
}

/// 造合并结果：先垫两个模拟出厂的区块类，再叠加测试自己那份。
///
/// 区块类在 `data/charsets/blocks.yaml`，单元测试不读它（测的是「叠加」本身，不是块表内容）；
/// 真实出厂文件由 `tests/charset_factory_assembly.rs` 验。
fn merged(text: &str) -> BTreeMap<String, MergedClass> {
    let mut m = BTreeMap::new();
    for stub in [
        "---
key: 表情符号
ranges:
  - U+1F300-U+1FAFF
order: 900
",
        "---
key: 基本汉字
ranges:
  - U+4E00-U+9FFF
order: 900
",
    ] {
        wind_config::charset_def::apply_doc(&mut m, parse_doc(stub).expect("垫片解析失败"));
    }
    wind_config::charset_def::apply_doc(&mut m, parse_doc(text).expect("解析失败"));
    m
}

/// 出厂零配置：装配出来的 registry **一个字符都不表态**，行为与本机制出现之前一致。
#[test]
fn no_config_means_no_opinion() {
    let reg = assemble(&BTreeMap::new(), None, ExternalRefs::default());
    for probe in ["我", "龘", "😀", "★", "a", "、"] {
        assert_eq!(reg.verdict_of(probe), None, "{probe} 不该有人表态");
        assert!(!reg.no_freq(probe), "{probe} 不该免词频");
        assert!(!reg.in_rare(probe), "{probe} 不该进生僻模式");
    }
}

/// 给内置块配一条属性时，块的 `ranges` 必须留着（叠加而非替换）；替换语义下这个类一个
/// 字符都命中不了，用户看到的是「配了 no_freq 没反应」。
#[test]
fn overlaying_a_builtin_block_keeps_its_ranges() {
    let defs = merged("---\nkey: 表情符号\nno_freq: true\n");
    let reg = assemble(&defs, None, ExternalRefs::default());
    assert!(reg.no_freq("😀"), "块的 ranges 被覆盖掉了");
    assert!(!reg.no_freq("我"), "免词频不该外溢到块外");
}

/// `enabled: false` 能把内置块类整个关掉。
#[test]
fn disabled_class_is_dropped() {
    let defs = merged("---\nkey: 表情符号\nenabled: false\n");
    let reg = assemble(&defs, None, ExternalRefs::default());
    assert!(reg.class_by_key("表情符号").is_none());
    // 关掉一个块不该动到别的块。
    assert!(reg.class_by_key("基本汉字").is_some());
}

/// 自定义类：yaml 里没有的 key 就新建，`order` 缺省走 `DEFAULT_ORDER`。
#[test]
fn a_custom_class_is_created_with_default_order() {
    let defs = merged("---\nkey: mine\nranges:\n  - U+2600-U+26FF\ndefault: rare\n");
    let reg = assemble(&defs, None, ExternalRefs::default());
    let c = reg.class_by_key("mine").expect("自定义类没进去");
    assert_eq!(c.order, DEFAULT_ORDER);
    assert_eq!(reg.verdict_of("☀"), Some(false));
}

/// 自定义类的 `order`（缺省 100）比内置块类（900）靠前 ⇒ 它表的态说了算。
/// 反过来的话用户新建的类会被不表态的内置块类挡住，怎么配都不生效。
#[test]
fn a_custom_class_outranks_the_builtin_blocks() {
    let defs = merged("---\nkey: mine\nranges:\n  - U+4E00-U+4EFF\ndefault: rare\n");
    let reg = assemble(&defs, None, ExternalRefs::default());
    assert_eq!(reg.verdict_of("一"), Some(false));
    assert_eq!(
        reg.class_of("一").map(|c| c.key.as_str()),
        Some("mine"),
        "仲裁赢家应当是表了态的那个"
    );
}

/// 列表体的 `-` 删除对内置块的 `ranges` 也生效（`excluded` 优先于一切）。
#[test]
fn removal_beats_a_builtin_range() {
    let defs = merged("---\nkey: 表情符号\nno_freq: true\n...\n-😀\n");
    let reg = assemble(&defs, None, ExternalRefs::default());
    assert!(!reg.no_freq("😀"), "被删掉的成员不该还命中");
    assert!(reg.no_freq("😁"), "同块的其余字符不受影响");
}

/// 字表读不到时，「域内名单外 ⇒ 生僻」这一半必须撤销，不能拿空名单去判；否则每个汉字都
/// 判生僻，候选被过滤到几乎不剩，而只有一条 warn。
#[test]
fn a_scoped_class_without_members_gives_up_its_verdict() {
    let defs = merged(concat!(
        "---
",
        "key: common_han
",
        "scope: han
",
        "file: 这个文件不存在.txt
",
        "default: common
",
        "outside: rare
",
    ));
    let reg = assemble(&defs, None, ExternalRefs::default());
    assert_eq!(reg.verdict_of("我"), None, "字表没读到就不该表态");
    assert_eq!(reg.verdict_of("龘"), None, "更不该把汉字全判成生僻");
}

/// 对照：名单读得到时，「域内名单外 ⇒ 生僻」照常工作——防线不该误伤正常情形。
#[test]
fn a_scoped_class_with_members_keeps_its_verdict() {
    let defs = merged(concat!(
        "---
",
        "key: common_han
",
        "scope: han
",
        "default: common
",
        "outside: rare
",
        "...
",
        "我
",
    ));
    let reg = assemble(&defs, None, ExternalRefs::default());
    assert_eq!(reg.verdict_of("我"), Some(true));
    assert_eq!(reg.verdict_of("龘"), Some(false), "域内名单外该判生僻");
}

/// 缺 `file:` 只让该类的外部成员为空，不影响它已有的其余定义。
#[test]
fn a_missing_member_file_does_not_kill_the_class() {
    let defs =
        merged("---\nkey: mine\nfile: nope.txt\nranges:\n  - U+2600-U+26FF\ndefault: rare\n");
    let reg = assemble(&defs, None, ExternalRefs::default());
    assert_eq!(reg.verdict_of("☀"), Some(false), "ranges 那半边该照常工作");
}

/// 装配对每个可叠加字段都真的写了一行——漏一行是静默的（配了没反应）。
///
/// 与 `CharsetDef::merge_covers_every_field` 同款：那条钉配置三层之间的合并，
/// 这条钉「配置 → 判定形态」这一跳。
#[test]
fn apply_covers_every_field() {
    let defs = merged(concat!(
        "---\n",
        "key: mine\n",
        "name: 我的类\n",
        "ranges:\n  - U+2600-U+26FF\n",
        "scope: pua\n",
        "default: rare\n",
        "outside: common\n",
        "order: 7\n",
        "no_freq: true\n",
        "in_rare: true\n",
        "...\n",
        "★\n",
        "-☀\n",
    ));
    let reg = assemble(&defs, None, ExternalRefs::default());
    let c = reg.class_by_key("mine").expect("类没进去");
    assert_eq!(c.name, "我的类");
    assert_eq!(c.ranges, vec![(0x2600, 0x26FF)]);
    assert_eq!(c.scope, Some(Scope::Pua));
    assert_eq!(c.default_common, Some(false));
    assert_eq!(c.outside_common, Some(true));
    assert_eq!(c.order, 7);
    assert!(c.no_freq);
    assert!(c.in_rare);
    assert!(c.members.contains("★"));
    assert!(c.excluded.contains("☀"));
    // `CharsetDef` 剩下的字段（key / file / enabled / replace）都不是「叠加到
    // ClassSpec 上的属性」：key 是主键，file 与列表体同归 members，enabled 在
    // 本函数之前就决定了这个类进不进来，replace 由配置层消化。
    let _ = CharsetDef::default();
}

/// 外部字表的行尾不挑食。
///
/// 这是 `CommonChars::load` 同一种文件的第二条路径（`file:` 存在的理由就是兼容
/// `schemas/common_chars.txt`），两处都得认孤立 `\r`：带 `#` 注释头的 CR 文件会被整份
/// 当成一条注释，一个成员都收不到且无报错。
/// 样本在代码里构造，不要改用仓库 fixture（git core.autocrlf 会按平台改行尾）。
#[test]
fn member_file_line_endings_do_not_change_the_result() {
    let dir = std::env::temp_dir().join(format!("wind-memberfile-eol-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("schemas")).unwrap();
    for lf in ["的一是\n了我不\n", "# 我的字表\n的一是\n了我不\n"] {
        for (tag, text) in [
            ("LF", lf.to_string()),
            ("CRLF", lf.replace('\n', "\r\n")),
            ("CR", lf.replace('\n', "\r")),
        ] {
            std::fs::write(dir.join("schemas/t.txt"), &text).unwrap();
            let got = load_member_file("测试类", "t.txt", Some(dir.as_path()));
            assert_eq!(
                got,
                vec!["的", "一", "是", "了", "我", "不"],
                "{tag}: 成员与 LF 版不同（样本 {lf:?}）"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
