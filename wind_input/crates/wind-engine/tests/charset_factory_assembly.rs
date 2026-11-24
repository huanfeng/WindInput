//! 拿仓里真实的出厂目录（`data/charsets/` + `data/schemas/`）走一遍完整装配。
//!
//! 与 `charset_assembly.rs` 里用内联 yaml 的单元测试互补：那些证明装配器自洽，这里证明实际发出去的
//! 文件装配出来是对的（例如把 `default: rare` 误写进出厂 `emoji.yaml`，前者抓不住）。
//! 解析层的同款守门见 `wind-config/tests/charsets_factory_files.rs`。

use std::path::{Path, PathBuf};

use wind_engine::charset_assembly::{ExternalRefs, assemble};

/// 仓库根下的 `data/`。
///
/// 找不到就 panic 而不是 return：静默跳过会让计数照常显示「通过」，而这是唯一验真实出厂内容的测试。
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
    let defs = wind_config::charset_def::load_layered(Some(&d), None, Vec::new());
    assemble(&defs, Some(&d), ExternalRefs::default())
}

/// 出厂零回归的判据：不是「没人表态」，而是「表的态与现状一模一样」。
///
/// `common_han.yaml` 出厂就是表态的（`default: common` / `outside: rare`），复现的是 `CommonChars`
/// 的判定；不表态等于把常用字过滤整个关掉。所以逐码位与朴素参照实现对答案。
///
/// 参照物不能是 `CommonChars`：`is_base_common` 本身就是问 registry，拿它比对是自反断言，
/// 永远绿。参照物必须不经过被测代码——直接读名单建集合，按 `is_common_scope` 判。
#[test]
fn common_han_reproduces_the_pre_wiring_verdict_codepoint_by_codepoint() {
    let d = data_dir();
    let reg = factory_registry();

    // 朴素参照：名单直接从 `charsets/common_han.yaml` 的列表体读，不经过 `charset_def` 的解析
    // （参照物走被测代码会退化成自反断言）。列表体在 `...` 之后，一行连写多字。
    let text = std::fs::read_to_string(d.join("charsets").join("common_han.yaml"))
        .expect("读不出 common_han.yaml");
    // 按行找分隔符，别拿跨行的字节串去 split：工作区行尾随 autocrlf 可能是 CRLF，带换行的字节匹配
    // 会静默失配。`lines()` 两种行尾都吃。
    let body: Vec<&str> = text
        .lines()
        .skip_while(|l| l.trim() != "...")
        .skip(1)
        .collect();
    assert!(!body.is_empty(), "common_han.yaml 该有 `...` 分隔的列表体");
    let base: std::collections::HashSet<char> = body
        .iter()
        .copied()
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
        // 无人表态时调用方兜底判「常用」，与参照实现「域外一律放行」那一半同源。
        let expected = !wind_candidate::is_common_scope(ch) || base.contains(&ch);
        assert_eq!(
            reg.verdict_of_char(ch).unwrap_or(true),
            expected,
            "U+{c:04X} 的常用性判定与接线前不一致"
        );
    }
}

/// 除 `common_han` 之外，出厂没有任何类表态或开启并集属性。
///
/// emoji 类只定义「谁是 emoji」，不写判定字段；写上 `default: rare` 会让升级用户的 emoji 从候选里消失。
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
/// 少一个的后果是静默的：那一行被当成「未识别」跳过，功能不生效而无报错。`emoji` 组名不由内置
/// 块类提供，全靠出厂 `emoji.yaml` 在场，最容易掉。
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

/// 出厂 `blocks.yaml` 与代码里的区块表逐码位一致。
///
/// 区块类由 `gen_block_charsets` 从 `charblock::BLOCKS` 生成，改了一侧没同步另一侧时不会报错，
/// 只会让标签与 `exclude_blocks` 认的名字对不上、某段码位归错类。逐码位比对 registry 命中的区块类
/// 与 `block_of(ch).name`，同时钉住：块区间对、「其它」是补集、区块类 `order` 让具体的块排在
/// 「符号」之前（否则半个 BMP 会显示成「符号」）。
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
/// 读不到时 `load_member_file` 只 warn 并返回空表，这个类只剩 scope，「域内不在名单 ⇒ 生僻」
/// 就把所有汉字判成生僻。
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

/// emoji 类带着精确字表，且按块判定会漏的那批确实在里面。
///
/// 这几个字符所在的块不在块表里，或整块搬进来会连非 emoji 一起搬，`PRESET_EMOJI` 五个块的口径对它们都判错。
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
    // keycap 基字符独立时不是 emoji，生成器排除了这 12 个。
    for ch in ["0", "5", "9", "#", "*"] {
        assert!(!c.members.contains(ch), "keycap 基字符 {ch} 混进来了");
    }
}

/// emoji 类的判据与「五个块并集」在 `☰ ⌘ ⌥` 上故意不同：块口径多收了它们。
/// 写成断言，防有人「顺手补块」。
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
    for ch in ["♠", "☯"] {
        assert!(c.members.contains(ch), "{ch} 有 Emoji 属性，该收");
    }
}

/// 勾「符号」免词频后，`✓ ✗ ★ ☆ ☐ ⌘` 这批非 emoji 符号也免：它们所在的「装饰符号」「杂项符号」
/// 「杂项技术符号」三块扣掉 emoji 后并入了「符号」。只勾 `emoji` 管不到它们（不带 Emoji 属性）。
#[test]
fn symbol_group_covers_non_emoji_in_mixed_blocks() {
    let d = data_dir();
    let defs = wind_config::charset_def::load_layered(Some(&d), None, Vec::new());
    let sym = vec!["符号".to_string()];
    let reg = assemble(
        &defs,
        Some(&d),
        ExternalRefs {
            no_freq: &sym,
            in_rare: &[],
        },
    );
    for s in ["✓", "✗", "✘", "★", "☆", "☐", "⌘"] {
        assert!(reg.no_freq(s), "勾「符号」后 {s} 应免词频");
    }
    for s in ["我", "a", "✔", "☀"] {
        assert!(!reg.no_freq(s), "只勾「符号」时 {s} 不该免词频");
    }
}

/// 「符号」在三个混住块里与 emoji 按码位不相交：勾「符号」不顺带收进 emoji。
///
/// 只断言这三块：其余 17 块整块并入，内有 `▶ ™ ↔` 这类散落的 emoji，是既有口径。
#[test]
fn symbol_group_is_disjoint_from_emoji_in_mixed_blocks() {
    let reg = factory_registry();
    let sym = reg.class_by_key("符号").expect("缺「符号」类");
    let emoji = reg.class_by_key("emoji").expect("缺 emoji 类");
    let in_sym = |c: u32| sym.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi);
    let mut covered = 0;
    for (lo, hi) in [(0x2300u32, 0x23FFu32), (0x2600, 0x26FF), (0x2700, 0x27BF)] {
        for c in lo..=hi {
            let Some(ch) = char::from_u32(c) else {
                continue;
            };
            let is_emoji = emoji.members.contains(ch.to_string().as_str());
            assert_ne!(
                in_sym(c),
                is_emoji,
                "U+{c:04X}：三块内的码位应恰好属于「符号」与 emoji 之一"
            );
            covered += usize::from(in_sym(c));
        }
    }
    assert!(covered > 300, "三块并入「符号」的码位只有 {covered} 个");
}
