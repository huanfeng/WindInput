//! gen_emoji_chars：把 Unicode `Emoji` 属性转成本仓的 emoji 字表
//!
//! 用法：`gen_emoji_chars --emoji-data <emoji-data.txt> --out <emoji_chars.txt>`
//!
//! 数据源：<https://www.unicode.org/Public/UCD/latest/ucd/emoji/emoji-data.txt>
//! 产物格式与 `common_chars.txt` 一致（一行一个字素簇），**入库并进发行版**。
//!
//! # ★ 为什么是字表文件而不是内置判定函数
//!
//! 判定「这个字符是不是 emoji」曾经用一张内置的 151 段区间表 + 一个判定函数。改成
//! 字表的理由是 `[charset]` 字符类系统的核心诉求——**让判据可自定义**：字表走
//! `resolve_schema_resource`，用户把同名文件放进配置目录即可整份换掉，还能用
//! `charset.toml` 的 `add`/`remove` 做稀疏调整；内置函数是唯一用户碰不到的形态。
//! 顺带查询更快（HashSet O(1) vs 二分 151 段）。论证见
//! `docs/design/charset-classification.md` §5。
//!
//! # ⛔ 属性选择：只用 `Emoji`
//!
//! | 属性 | 码位数 | 判断 |
//! |---|---|---|
//! | **`Emoji`** | 1438 | ✅ 本工具用它 |
//! | `Emoji_Presentation` | 1219 | ⛔ **误伤 201 个**（`❤ ☀ ✈ ♠ 🕵 🖥`）——上游词库存的是**裸码位**，实测 1404 簇里只有 10 个带 `FE0F` |
//! | `Extended_Pictographic` | 2848 | ⛔ **绝不用**：含**未分配码位**，是给 UAX #29 断簇用的，不是「是不是 emoji」的判据 |
//!
//! ★ Unicode 官方立场是 `© ® ™ ↔ ▶` **都是** emoji（RGI fully-qualified，只是归在
//! `Symbols` 组）。「这是符号不算 emoji」那条线属性表里没有，只在 `emoji-test.txt` 的
//! group 里；要它得引入 5225 条 RGI 全表，且会连 `✅ ❌ ⭕` 一起摘走——已否决。
//!
//! # ⛔ 为什么不能按 Unicode 块近似
//!
//! 实测一本五笔 emoji 码表（4132 条 / 1404 簇），旧的「五个块并集」口径**两个方向
//! 同时不准**：漏 182 条（`⬅ ⬛ ⭐ ⭕ 🀄 🃏 🅰 🆚 🈚 🉐 ⤴ ⤵` 所在的块不在块表里；
//! `▶ ◀ ↔ ↩ ‼ ™ ℹ ㊗ Ⓜ 〰 © ®` 所在的块大部分是非 emoji，整块搬会连 `← → ▲ ◆`
//! 一起搬），又多收「杂项符号」块内的 `☰` 与「杂项技术符号」块内的 `⌘ ⌥`。
//!
//! ⚠️ 早期记录把 `♠ ☯` 也算进「多收」，**是错的**：这两个本身就有 `Emoji` 属性
//! （`2660`/`262F` 在 emoji-data.txt 里明确列着），收进来是对的。2026-09-04 拿生成
//! 出来的字表实证后更正——结论转述了几轮都没人验证，直到有产物可查。
//!
//! ⇒ **补块补不齐**：块是显示域的划分、emoji 是字符属性，两者正交。

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// keycap 序列（UTS #51 ED-13：`[0-9#*] FE0F? 20E3`）的基字符。
///
/// # ★★★ 必须排除，否则四种后果全部静默
///
/// 这 12 个 ASCII 在 `Emoji` 属性里为真，但**独立时不是 emoji**——`1` 就是数字 1。
/// 不排除的话：数字候选免词频、`0-9` 挤进生僻字候选、一次「整类设为生僻」把十个数字
/// 一起判掉，而且全程无报错。
const KEYCAP_BASES: &[char] = &['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '#', '*'];

/// 组合键帽符 `U+20E3`。
///
/// ★ **要列进来**：`1️⃣` 这类 keycap 序列在逐字符的存在性判定里，基字符已被上面排除，
/// 唯一能命中的就是它。副作用是它单独也算 emoji，但组合符号不会单独成为候选，无害。
const COMBINING_KEYCAP: char = '\u{20E3}';

/// 目标属性名。**精确匹配**，不用 `starts_with`——`Emoji_Presentation` /
/// `Emoji_Modifier` / `Emoji_Component` 都以 `Emoji` 开头，前缀匹配会把它们一起收进来。
const WANT_PROPERTY: &str = "Emoji";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (data, out) = parse_args(&args)?;

    let text = std::fs::read_to_string(&data)
        .map_err(|e| anyhow::anyhow!("读取 {} 失败：{e}", data.display()))?;
    let version = parse_version(&text)?;
    let emoji = collect(&text, WANT_PROPERTY)?;

    // 子集断言：`Emoji_Presentation ⊂ Emoji` 是 UTS #51 的既定关系。它若不再成立，
    // 说明上游语义变了，本工具的属性选择论证（见模块头那张表）需要重新做一遍
    // ——让它在这里**响亮地失败**，而不是产出一份看似正常的字表。
    let presentation = collect(&text, "Emoji_Presentation")?;
    anyhow::ensure!(
        presentation.is_subset(&emoji),
        "上游语义变化：Emoji_Presentation 不再是 Emoji 的子集，请重新评估属性选择"
    );

    let mut chars: BTreeSet<char> = emoji
        .iter()
        .filter_map(|&c| char::from_u32(c))
        .filter(|c| !KEYCAP_BASES.contains(c))
        .collect();
    chars.insert(COMBINING_KEYCAP);

    write_table(&out, &chars, &version)?;
    eprintln!(
        "  emoji_chars.txt（Unicode {version}，{} 属性 {} 码位 − {} 个 keycap 基字符 + U+20E3 = {} 条）→ {}",
        WANT_PROPERTY,
        emoji.len(),
        KEYCAP_BASES.len(),
        chars.len(),
        out.display()
    );
    Ok(())
}

/// 从文件头 `# Version: 17.0` 取版本号。
///
/// 取不到就**报错**而不是填 "unknown"：版本号会写进产物首行供追溯，一个写着 unknown
/// 的字表在半年后没人说得清它是哪一版生成的。
fn parse_version(text: &str) -> anyhow::Result<String> {
    text.lines()
        .find_map(|l| l.trim_start_matches('#').trim().strip_prefix("Version:"))
        .map(|v| v.trim().to_string())
        .ok_or_else(|| anyhow::anyhow!("emoji-data.txt 里找不到 `# Version:` 行"))
}

/// 收集某个属性的全部码位。行格式：`0023          ; Emoji                # ...`
/// 或 `1F300..1F320 ; Emoji                # ...`。
fn collect(text: &str, want: &str) -> anyhow::Result<BTreeSet<u32>> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let body = line.split('#').next().unwrap_or("");
        let Some((range, prop)) = body.split_once(';') else {
            continue;
        };
        if prop.trim() != want {
            continue;
        }
        let range = range.trim();
        let (lo, hi) = match range.split_once("..") {
            Some((a, b)) => (a, b),
            None => (range, range),
        };
        let lo = u32::from_str_radix(lo.trim(), 16)
            .map_err(|_| anyhow::anyhow!("码位解析失败：{range}"))?;
        let hi = u32::from_str_radix(hi.trim(), 16)
            .map_err(|_| anyhow::anyhow!("码位解析失败：{range}"))?;
        out.extend(lo..=hi);
    }
    anyhow::ensure!(
        !out.is_empty(),
        "属性 {want} 一个码位都没收到，格式可能变了"
    );
    Ok(out)
}

/// 写字表：首行注释记录来源与版本，其后一行一个字符（与 `common_chars.txt` 同格式）。
fn write_table(out: &Path, chars: &BTreeSet<char>, version: &str) -> anyhow::Result<()> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(out)?);
    writeln!(
        f,
        "# emoji 字表 —— 由 wind-tools/gen_emoji_chars 生成，请勿手改"
    )?;
    writeln!(
        f,
        "# 数据源: Unicode {version} emoji-data.txt 的 `Emoji` 属性"
    )?;
    writeln!(
        f,
        "# 已排除 keycap 基字符 0-9 # *（独立时不是 emoji），已补入 U+20E3"
    )?;
    writeln!(
        f,
        "# 要自定义：把本文件放进用户配置目录的 schemas/ 下整份覆盖，"
    )?;
    writeln!(
        f,
        "# 或在 charset.toml 的 [charset.emoji] 里用 add / remove 稀疏调整。"
    )?;
    for c in chars {
        writeln!(f, "{c}")?;
    }
    f.flush()?;
    Ok(())
}

fn parse_args(args: &[String]) -> anyhow::Result<(PathBuf, PathBuf)> {
    let mut data = None;
    let mut out = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--emoji-data" if i + 1 < args.len() => {
                data = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--out" if i + 1 < args.len() => {
                out = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            _ => i += 1,
        }
    }
    match (data, out) {
        (Some(d), Some(o)) => Ok((d, o)),
        _ => anyhow::bail!(
            "用法: gen_emoji_chars --emoji-data <emoji-data.txt> --out <emoji_chars.txt>"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Version: 17.0
0023          ; Emoji                # 1.1  [1] (#️)       number sign
0030..0039    ; Emoji                # 1.1 [10] (0️..9️)    digit zero..nine
00A9          ; Emoji                # 1.1  [1] (©️)       copyright
1F600..1F602  ; Emoji                # 6.1  [3] (😀..😂)   grinning face
1F600..1F602  ; Emoji_Presentation   # 6.1  [3] (😀..😂)
2B50          ; Emoji                # 5.1  [1] (⭐)       star
";

    #[test]
    fn parses_version() {
        assert_eq!(parse_version(SAMPLE).unwrap(), "17.0");
    }

    #[test]
    fn missing_version_is_an_error() {
        assert!(parse_version("0023 ; Emoji\n").is_err());
    }

    /// 属性名必须精确匹配：`Emoji_Presentation` 以 `Emoji` 开头，前缀匹配会多收。
    #[test]
    fn property_match_is_exact_not_prefix() {
        let emoji = collect(SAMPLE, "Emoji").unwrap();
        let pres = collect(SAMPLE, "Emoji_Presentation").unwrap();
        assert_eq!(pres.len(), 3, "只该收到 1F600..1F602");
        assert!(emoji.len() > pres.len());
        assert!(emoji.contains(&0x0023) && emoji.contains(&0x2B50));
    }

    /// ★★★ 生成结果里 ASCII 区必须一个都不剩——keycap 基字符全部排除。
    /// 这条同时防住上游属性变化时的静默漂移。
    #[test]
    fn ascii_keycap_bases_are_all_excluded() {
        let emoji = collect(SAMPLE, "Emoji").unwrap();
        let kept: Vec<char> = emoji
            .iter()
            .filter_map(|&c| char::from_u32(c))
            .filter(|c| !KEYCAP_BASES.contains(c))
            .collect();
        assert!(
            kept.iter().all(|c| !c.is_ascii()),
            "ASCII 区还有残留：{kept:?}"
        );
        assert_eq!(KEYCAP_BASES.len(), 12, "keycap 基字符恰好 12 个");
    }

    /// 未收到任何码位要报错，不能产出一张空表——空表会让 emoji 类静默失效。
    #[test]
    fn empty_result_is_an_error() {
        assert!(collect(SAMPLE, "Emoji_Modifier").is_err());
    }
}
