//! 出厂 `data/charsets/*.yaml` 必须能被 `charset_def` 解析，且**不表态**。
//!
//! ★ 「不表态」这条是本测试的重点：出厂文件一旦写上 `default`，升级到该版的用户会
//! 突然发现 emoji 从候选里消失，而他们什么都没改。出厂只声明身份、不改变既有行为，
//! 变更出厂行为必须是一次单独的决策。
//!
//! ⚠️ 本测试读仓库里的真实出厂文件，不是夹具——夹具只能证明解析器自洽，证明不了
//! 我们实际发出去的那两个文件是对的。

use std::path::{Path, PathBuf};
use wind_config::charset_def;

fn charsets_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/wind_input/crates/wind-config
    // 上溯三级到仓库根：wind-config → crates → wind_input → <repo>
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("仓库根")
        .join("data")
        .join("charsets")
}

#[test]
fn factory_files_parse_and_stay_silent_on_verdicts() {
    let docs = charset_def::load_layer(&charsets_dir());
    let keys: Vec<&str> = docs.iter().map(|d| d.def.key.as_str()).collect();
    assert!(keys.contains(&"emoji"), "出厂应有 emoji 类，实得 {keys:?}");
    assert!(
        keys.contains(&"common_han"),
        "出厂应有 common_han 类，实得 {keys:?}"
    );

    for d in &docs {
        if d.def.key == "common_han" {
            continue; // 它刻意表态，见下一条测试
        }
        assert_eq!(
            d.def.default, None,
            "{} 不该在出厂替用户表态 default",
            d.def.key
        );
        assert_eq!(
            d.def.no_freq, None,
            "{} 不该在出厂替用户表态 no_freq",
            d.def.key
        );
        assert_eq!(
            d.def.in_rare, None,
            "{} 不该在出厂替用户表态 in_rare",
            d.def.key
        );
    }
}

/// emoji 类：1427 条，且 keycap 基字符零残留、`U+20E3` 在场。
#[test]
fn emoji_class_carries_the_generated_list() {
    let docs = charset_def::load_layer(&charsets_dir());
    let emoji = docs
        .iter()
        .find(|d| d.def.key == "emoji")
        .expect("emoji 类");

    // ⚠️ 刻意**不叫**「表情符号」——区块表里已有一个同名的块（`charblock::BLOCKS`），
    // 两者在设置页会并排列出，同名的话用户分不清哪个是精确字表、哪个是那一个区块。
    assert_eq!(emoji.def.display_name(), "Emoji 表情");
    assert_eq!(emoji.added.len(), 1427, "字表条数");
    assert!(emoji.removed.is_empty(), "出厂不该有移除项");

    for bad in ["0", "9", "#", "*"] {
        assert!(
            !emoji.added.iter().any(|s| s == bad),
            "keycap 基字符 {bad} 泄漏进字表"
        );
    }
    assert!(emoji.added.iter().any(|s| s == "\u{20E3}"), "U+20E3 应在场");

    // 旧块口径漏掉的那批的代表，逐个都要在。
    for c in [
        "\u{2B50}",
        "\u{1F004}",
        "\u{1F170}",
        "\u{1F21A}",
        "\u{00A9}",
        "\u{25B6}",
    ] {
        assert!(
            emoji.added.iter().any(|s| s == c),
            "U+{:04X} 应在字表里",
            c.chars().next().unwrap() as u32
        );
    }
}

/// common_han 复现既有判定：域内在名单→常用、域内不在名单→生僻、域外不表态。
#[test]
fn common_han_reproduces_the_existing_verdict_shape() {
    let docs = charset_def::load_layer(&charsets_dir());
    let han = docs
        .iter()
        .find(|d| d.def.key == "common_han")
        .expect("common_han 类");

    assert_eq!(han.def.scope, Some(charset_def::ScopeKind::Han));
    assert_eq!(han.def.default, Some(charset_def::Commonality::Common));
    assert_eq!(
        han.def.outside,
        Some(charset_def::Commonality::Rare),
        "★ 缺了 outside,「是汉字却不在名单里 ⇒ 生僻」就没有落点"
    );
    assert_eq!(
        han.def.file, None,
        "常用字表已搬进本文件的列表体——留着 file: 就成了两个数据源"
    );
    assert!(
        han.added.len() > 6000,
        "名单该在列表体里，实得 {} 条",
        han.added.len()
    );
    // ★ 一行连写多字：8104 个字挤在两百来行里，而不是 8104 行。
    // 顺序按《通用规范汉字表》的级别排（一级 → 二级 → 三级），设置页照它分页。
    for ch in ["一", "乙", "二"] {
        assert!(han.added.contains(&ch.to_string()), "名单里该有「{ch}」");
    }
    assert!(!han.added.contains(&"龘".to_string()), "生僻字不该在名单里");
}
