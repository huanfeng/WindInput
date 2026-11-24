//! gen_emoji_chars：把 Unicode `Emoji` 属性转成本仓的 emoji 字表
//!
//! 用法：`gen_emoji_chars --emoji-data <emoji-data.txt> --out <emoji_chars.txt>`
//!
//! 数据源：<https://www.unicode.org/Public/UCD/latest/ucd/emoji/emoji-data.txt>
//! 产物格式与 `common_chars.txt` 一致（一行一个字素簇），**入库并进发行版**。
//!
//! # 为什么是字表文件而不是内置判定函数
//!
//! 让判据可自定义：字表走 `resolve_schema_resource`，用户可整份替换，或用 `charset.toml`
//! 的 `add`/`remove` 稀疏调整。论证见 `docs/design/charset-classification.md` §5。
//!
//! # 属性选择：只用 `Emoji`
//!
//! 不用 `Emoji_Presentation`（会漏掉上游以裸码位存的 `❤ ☀ ✈` 等），绝不用
//! `Extended_Pictographic`（含未分配码位，是断簇用的）。也不能按 Unicode 块近似：块是显示域
//! 划分、emoji 是字符属性，两个方向都不准。数据与反例见上述文档「数据源与属性选择」一节。

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// keycap 序列（UTS #51 ED-13：`[0-9#*] FE0F? 20E3`）的基字符。
///
/// 必须排除：这 12 个 ASCII 在 `Emoji` 属性里为真，但独立时不是 emoji。不排除的话数字
/// 候选免词频、`0-9` 挤进生僻字候选、「整类设为生僻」会把数字一起判掉，且全程无报错。
const KEYCAP_BASES: &[char] = &['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '#', '*'];

/// 组合键帽符 `U+20E3`。
///
/// **要列进来**：`1️⃣` 这类 keycap 序列在逐字符的存在性判定里，基字符已被上面排除，
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

/// 写字符类文件：meta 头 + `...` + 列表体，与 `.dict.yaml` 同构。
///
/// 出厂 meta 只声明身份，不写任何判定字段：`default` / `no_freq` / `in_rare` 留空即不表态，
/// 本类只提供「这些字符属于 emoji」这个事实，不改变既有行为。
/// 别在出厂文件里替用户表态：写上 `default: rare` 会让升级用户的 emoji 候选突然消失。
fn write_table(out: &Path, chars: &BTreeSet<char>, version: &str) -> anyhow::Result<()> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(out)?);
    // 逐行 writeln! 而不是一个多行字面量：续行 `\` 的空白吞噬规则在这种带缩进的
    // 代码里极易出错（本文件就先错过一次，产物每行都带上了源码缩进）。
    for line in [
        "---".to_string(),
        "# 由 wind-tools/gen_emoji_chars 生成，请勿手改。".to_string(),
        format!("# 数据源: Unicode {version} emoji-data.txt 的 `Emoji` 属性。"),
        "# 已排除 keycap 基字符 0-9 # *（独立时不是 emoji），已补入 U+20E3。".to_string(),
        "#".to_string(),
        "# 要调整本类：**不要改本文件**（下次重新生成会覆盖）。在用户配置目录的".to_string(),
        "# charsets/ 下新建一个 .yaml，写 `key: emoji` 加你要改的字段即可，".to_string(),
        "# 未写的字段与整份列表都会从这里继承；列表增删写 `★` / `-★`。".to_string(),
        "key: emoji".to_string(),
        "name: 表情符号".to_string(),
        // 排在常用汉字(10)之后：那才是这套东西的主角，列表里该第一个看到。
        "order: 20".to_string(),
        "...".to_string(),
    ] {
        writeln!(f, "{line}")?;
    }
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

    /// 生成结果里 ASCII 区必须一个都不剩——keycap 基字符全部排除。
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
