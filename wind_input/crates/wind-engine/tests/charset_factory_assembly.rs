//! 拿**仓里真实的出厂目录**（`data/charsets/` + `data/schemas/`）走一遍完整装配。
//!
//! ★ 与 `charset_assembly.rs` 里那些用内联 yaml 的单元测试是两件事：那些证明装配器
//! 自洽，这个证明**我们实际发出去的文件**装配出来是对的。一个把 `default: rare` 写进
//! 出厂 `emoji.yaml` 的手滑，前者一个都抓不住。
//!
//! 同款守门测试见 `wind-config/tests/charsets_factory_files.rs`（那边验解析，这边验装配）。

use std::path::{Path, PathBuf};

use wind_engine::charset_assembly::{ExternalRefs, assemble};

/// 仓库根下的 `data/`。
///
/// ⚠️ 找不到就 **panic 而不是 return**：静默跳过会让这个文件在计数上照常显示「通过」，
/// 而它恰恰是唯一验真实出厂内容的一条（`build_dev/data` 缺失那次的教训）。
fn data_dir() -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("定位不到仓库根");
    let d = repo.join("data");
    assert!(
        d.join("charsets").is_dir(),
        "找不到出厂字符类目录 {}",
        d.join("charsets").display()
    );
    d
}

fn factory_registry() -> wind_candidate::CharsetRegistry {
    let d = data_dir();
    let defs = wind_config::charset_def::load_layered(Some(&d), None, None);
    assemble(&defs, Some(&d), ExternalRefs::default())
}

/// ★★ 出厂零回归的**正确判据**：不是「没人表态」，而是「表的态与现状一模一样」。
///
/// ⚠️ 本条最初写成「对任何输入都不表态」，被真实出厂文件当场证伪：`common_han.yaml`
/// 出厂**就是**表态的（`default: common` / `outside: rare`），它复现的正是现有
/// `CommonChars` 的判定；不表态等于把常用字过滤整个关掉。
///
/// ⇒ 逐码位与**朴素参照实现**对答案。这是把常用性判定切到 registry 的等价性凭据。
///
/// # ⛔ 参照物不能是 `CommonChars`
///
/// 接线之前它是独立实现（`!is_common_scope(ch) || base.contains(&ch)`），拿它当参照
/// 天经地义。接线之后 `is_base_common` **本身就是问 registry**，再拿它比对就成了
/// 自反断言——测试照常全绿，判别力却已经归零。
///
/// ⇒ 参照物必须是**不经过被测代码**的一份独立计算：直接读 `common_chars.txt` 建集合，
/// 按 `is_common_scope` 判。它复刻的正是接线前那一行。
#[test]
fn common_han_reproduces_the_pre_wiring_verdict_codepoint_by_codepoint() {
    let d = data_dir();
    let reg = factory_registry();

    // 朴素参照：接线前 `CommonChars` 里的那份实现，逐字复刻。
    //
    // ⚠️ 名单直接从 `charsets/common_han.yaml` 的**列表体**读，不经过 `charset_def`
    // 的解析——参照物一旦走被测代码，比对就退化成自反断言（本文件下面那条注释记着
    // 这个坑的另一面）。列表体在 `...` 之后，一行连写多字。
    let text = std::fs::read_to_string(d.join("charsets").join("common_han.yaml"))
        .expect("读不出 common_han.yaml");
    let body = text
        .split_once(
            "
...
",
        )
        .expect("common_han.yaml 该有 `...` 分隔的列表体")
        .1;
    let base: std::collections::HashSet<char> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .flat_map(|l| l.chars())
        .filter(|c| wind_candidate::is_markable(*c))
        .collect();
    assert!(
        base.len() > 6000,
        "常用字表只读到 {} 条，比对会全绿而无意义",
        base.len()
    );

    for c in 0u32..=0x10FFFF {
        let Some(ch) = char::from_u32(c) else {
            continue;
        };
        // 无人表态时调用方兜底判「常用」——与接线前「域外一律放行」那一半同源。
        let expected = !wind_candidate::is_common_scope(ch) || base.contains(&ch);
        assert_eq!(
            reg.verdict_of_char(ch).unwrap_or(true),
            expected,
            "U+{c:04X} 的常用性判定与接线前不一致"
        );
    }
}

/// 除 `common_han` 之外，出厂**没有任何类**表态或开启并集属性。
///
/// emoji 类那 1427 条只定义了「谁是 emoji」，一个判定字段都不写——写上 `default: rare`
/// 会让升级用户突然发现 emoji 从候选里消失，而他什么都没改。
#[test]
fn only_common_han_has_an_opinion() {
    let reg = factory_registry();
    for c in reg.classes() {
        if c.key == "common_han" {
            continue;
        }
        assert!(c.default_common.is_none(), "{} 表了 default", c.key);
        assert!(c.outside_common.is_none(), "{} 表了 outside", c.key);
        assert!(!c.no_freq, "{} 出厂开了 no_freq", c.key);
        assert!(!c.in_rare, "{} 出厂开了 in_rare", c.key);
    }
    // 并集属性一个都没开 ⇒ 免词频与生僻准入出厂恒假，与现状（配置为空）一致。
    for probe in ["我", "龘", "😀", "⭐", "★", "a"] {
        assert!(!reg.no_freq(probe), "{probe} 被判免词频");
        assert!(!reg.in_rare(probe), "{probe} 被判进生僻模式");
    }
}

/// `exclude_blocks` / `include_blocks` 里能写的每个名字，装配后都解析得出来。
///
/// ⚠️ 少一个的后果是静默的：那一行被当成「未识别」跳过，功能不生效而无报错。
/// 本条覆盖 `emoji` 这个组名——它**不由内置块类提供**，全靠出厂 `emoji.yaml` 在场，
/// 是这套接线里最容易掉的一环。
#[test]
fn every_configurable_name_resolves() {
    let reg = factory_registry();
    for name in ["emoji", "符号", "表情符号", "基本汉字", "其它"] {
        assert!(
            reg.class_by_key(name).is_some(),
            "配置里写得出的名字 {name} 在 registry 里没有落点"
        );
    }
}

/// ★★ 出厂 `blocks.yaml` 与代码里的区块表**逐码位一致**。
///
/// 区块类已经搬进配置（`gen_block_charsets` 从 `charblock::BLOCKS` 生成），于是多了一份
/// 会漂移的数据：有人改了代码里的块表却没重新生成，或反过来手改了 yaml。
///
/// ⚠️ 漂移的表现全都很轻：类型列的标签与 `exclude_blocks` 认的名字对不上、某一段码位
/// 归错类。没有一处会报错。
///
/// ⇒ 逐码位比对「registry 里命中的区块类」与 `block_of(ch).name`。这同时钉住了三件事：
/// 块的区间对、「其它」确实是补集、以及区块类的 `order` 让具体的块排在「符号」之前
/// （否则半个 BMP 会显示成「符号」）。
#[test]
fn factory_blocks_match_the_block_table_codepoint_by_codepoint() {
    let reg = factory_registry();
    // 只看区块类：emoji / common_han 也会命中，但它们不是「这个字属于哪个区块」的答案。
    // 判据是「有 ranges、且不表态」——那正是区块类的形状。
    for c in 0u32..=0x10FFFF {
        let Some(ch) = char::from_u32(c) else {
            continue;
        };
        let got = reg
            .classes()
            .iter()
            .find(|k| {
                k.default_common.is_none()
                    && !k.ranges.is_empty()
                    && k.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi)
            })
            .map(|k| k.key.as_str());
        assert_eq!(
            got,
            Some(wind_candidate::block_of(ch).name),
            "U+{c:04X} 的区块归属与代码里的块表不一致——blocks.yaml 该重新生成了"
        );
    }
}

/// `common_han` 的 `file:` 真的把 `schemas/common_chars.txt` 读进来了。
///
/// ⚠️ 读不到时 `load_member_file` 只 warn 并返回空表——那样这个类会**只剩 scope**，
/// 「域内不在名单 ⇒ 生僻」就把所有汉字判成生僻。测试不看这条，故障要到用户机器上
/// 才暴露。
#[test]
fn common_han_actually_loads_its_char_table() {
    let reg = factory_registry();
    let c = reg.class_by_key("common_han").expect("缺 common_han 类");
    assert!(
        c.members.len() > 6000,
        "常用字表只读到 {} 条，多半是路径没解析对",
        c.members.len()
    );
    for ch in ["我", "的", "一"] {
        assert!(c.members.contains(ch), "常用字 {ch} 不在表里");
    }
    assert!(!c.members.contains("龘"), "生僻字不该在常用字表里");
}

/// emoji 类带着那份精确字表，且**旧的块口径漏判的那批**确实在里面。
///
/// 这几个字符是 §5.5 那 182 条的代表：它们所在的块根本不在块表里，或整块搬进来会连
/// 非 emoji 一起搬——`PRESET_EMOJI` 那套五个块的口径对它们全都判错。
#[test]
fn emoji_class_carries_the_precise_table() {
    let reg = factory_registry();
    let c = reg.class_by_key("emoji").expect("缺 emoji 类");
    assert!(
        c.members.len() > 1400,
        "emoji 字表只有 {} 条",
        c.members.len()
    );

    for ch in [
        "⭐", "🀄", "🅰", "🈚", "©", "▶", "↔", "‼", "™", "⬅", "⬛", "⭕",
    ] {
        assert!(c.members.contains(ch), "旧口径漏判的 {ch} 不在 emoji 表里");
    }
    // ⛔ keycap 基字符独立时不是 emoji，生成器排除了这 12 个。
    for ch in ["0", "5", "9", "#", "*"] {
        assert!(!c.members.contains(ch), "keycap 基字符 {ch} 混进来了");
    }
}

/// ★ emoji 类的判据与旧的「五个块并集」在 `☰ ⌘ ⌥` 上**故意不同**——旧口径多收了它们。
///
/// 这条不是回归，是修正。把它写成断言，是为了下次有人「顺手补块」时立刻撞上。
#[test]
fn emoji_no_longer_over_collects_the_way_blocks_did() {
    let reg = factory_registry();
    let c = reg.class_by_key("emoji").expect("缺 emoji 类");
    for ch in ["☰", "⌘", "⌥"] {
        assert!(
            !c.members.contains(ch),
            "{ch} 不该算 emoji（旧块口径的多收）"
        );
    }
    // 对照：`♠ ☯` 在 emoji-data.txt 里确实带 Emoji 属性，收进来是对的。
    // 早期记录把它们也列进「多收」，是错的。
    for ch in ["♠", "☯"] {
        assert!(c.members.contains(ch), "{ch} 有 Emoji 属性，该收");
    }
}
