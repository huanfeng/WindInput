//! 从 Unicode UTS #51 的 `emoji-data.txt` 生成 emoji 属性区间表，供 `wind-candidate`
//! 的 `charemoji` 判定「这个字符是不是 emoji」。
//!
//! 产出的 `charemoji_data.rs` 入库（可 review、可 diff），只含数据；判定逻辑与测试在
//! 手写的 `charemoji.rs` 里。**两个文件分开正是为了让本工具永远只覆盖数据那半边**——
//! 生成器与手写逻辑同处一个文件时，重新生成会连人写的取舍一起冲掉。
//!
//! # 取的是 `Emoji` 而不是 `Emoji_Presentation`
//!
//! `emoji-data.txt` 给了两条线，差集 219 个码位是「默认文本表现、加 VS16 才变彩色」的
//! 两栖字符（`© ® ™ ↔ ▶ ☀ ♠ ✈ ❤ 🕵` 都在其中）：
//!
//! | 属性 | 码位数 | 含义 |
//! |---|---|---|
//! | `Emoji` | 1438 | 这个字符**可以**被当 emoji 渲染 |
//! | `Emoji_Presentation` | 1219 | 这个字符**默认**就是彩色 emoji |
//!
//! 取 `Emoji`（宽档）的理由是**上游词库存的是裸码位**：实测用户的五笔 emoji 码表 1404 个
//! 字素簇里只有 10 个带 `U+FE0F`，`❤ ☀ ✈ 🕵` 全是裸的。按 `Emoji_Presentation` 判，这
//! 201 个（14%）真 emoji 会被判成非 emoji——而「漏掉真 emoji」正是 `charblock` 模块头
//! 那张表里标着**不安全**的那个方向。
//!
//! ⛔ **别改用 `Extended_Pictographic`**：它是给 UAX #29 断簇用的超集（2848 个码位），
//! 故意包含**尚未分配**的保留码位，好让未来的新 emoji 也能正确断簇。拿它问「这是不是
//! emoji」会得到大量假阳性，而假阳性在生僻字准入那条路上是静默的。
//!
//! # 用法
//!
//! ```text
//! curl -o .cache/unicode/emoji-data.txt \
//!   https://www.unicode.org/Public/UCD/latest/ucd/emoji/emoji-data.txt
//! cargo run -p wind-tools --bin gen_emoji_props -- \
//!   --emoji-data .cache/unicode/emoji-data.txt \
//!   --out wind_input/crates/wind-candidate/src/charemoji_data.rs
//! ```
//!
//! 数据许可证 Unicode-3.0（允许再分发与修改），见 NOTICE.md。

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;

struct Args {
    emoji_data: PathBuf,
    out: PathBuf,
}

fn parse_args() -> anyhow::Result<Args> {
    let (mut emoji_data, mut out) = (None, None);
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--emoji-data" => emoji_data = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("未知参数: {other}（--help 查看用法）"),
        }
    }
    Ok(Args {
        emoji_data: emoji_data.ok_or_else(|| anyhow::anyhow!("缺 --emoji-data <file>"))?,
        out: out.ok_or_else(|| anyhow::anyhow!("缺 --out <file>"))?,
    })
}

fn print_usage() {
    eprintln!(
        "用法: gen_emoji_props --emoji-data <file> --out <file>\n\
         \n\
         --emoji-data  Unicode 的 emoji-data.txt（UCD 的 emoji 子目录）\n\
         --out         生成的 Rust 数据源（charemoji_data.rs）"
    );
}

/// 从文件头的注释里抠出 `# Version: 17.0`。
///
/// 抠不到就报错而不是填「unknown」：版本号会写进生成物的注释，是日后回答「这张表是哪个
/// Unicode 版本」的唯一凭据。悄悄填一个占位符等于把那个问题永久变成无解。
fn parse_version(text: &str) -> anyhow::Result<String> {
    text.lines()
        .take(40)
        .find_map(|l| l.strip_prefix("# Version:"))
        .map(|v| v.trim().to_string())
        .ok_or_else(|| anyhow::anyhow!("emoji-data.txt 头部找不到 `# Version:` 行"))
}

/// 收集某个属性名下的全部码位。
///
/// 行格式为 `0023 ; Emoji # ...` 或 `1F300..1F320 ; Emoji_Presentation # ...`，
/// `#` 之后是注释。属性名**精确匹配**：`Emoji` 与 `Emoji_Presentation`、
/// `Emoji_Modifier_Base` 是前缀关系，用 `starts_with` 会把三者搅在一起。
fn collect(text: &str, want: &str) -> anyhow::Result<BTreeSet<u32>> {
    let mut set = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        let body = line.split('#').next().unwrap_or("").trim();
        if body.is_empty() {
            continue;
        }
        let Some((range, prop)) = body.split_once(';') else {
            anyhow::bail!("第 {} 行缺 `;`: {line}", n + 1);
        };
        if prop.trim() != want {
            continue;
        }
        let range = range.trim();
        let (a, b) = match range.split_once("..") {
            Some((a, b)) => (a, b),
            None => (range, range),
        };
        let a = u32::from_str_radix(a.trim(), 16)?;
        let b = u32::from_str_radix(b.trim(), 16)?;
        anyhow::ensure!(a <= b, "第 {} 行区间反了: {range}", n + 1);
        for c in a..=b {
            set.insert(c);
        }
    }
    anyhow::ensure!(!set.is_empty(), "属性 {want} 一个码位都没收到");
    Ok(set)
}

/// 把码位集合压成**相邻合并**的升序闭区间。
///
/// 合并是必须的而不是优化：`emoji-data.txt` 原文里 `1F300..1F320` 与 `1F321` 常常分行
/// （前者 `Emoji_Presentation`、后者只有 `Emoji`），照抄行会得到一张碎表，而判定端的
/// 二分只要求有序不要求最简——碎表不会算错，只是让「表有多大」这个数失去意义，
/// 也就看不出下次 Unicode 升级到底加了多少。
fn to_ranges(set: &BTreeSet<u32>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &c in set {
        match out.last_mut() {
            Some(last) if c == last.1 + 1 => last.1 = c,
            _ => out.push((c, c)),
        }
    }
    out
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let text = std::fs::read_to_string(&args.emoji_data)
        .map_err(|e| anyhow::anyhow!("读取 {} 失败: {e}", args.emoji_data.display()))?;
    let version = parse_version(&text)?;
    let emoji = collect(&text, "Emoji")?;
    let presentation = collect(&text, "Emoji_Presentation")?;

    // `Emoji_Presentation ⊂ Emoji` 是 UTS #51 的定义所保证的。这里断言它，是因为一旦
    // 上游改了口径（或本工具的属性名匹配写错了），下面那句「差集就是两栖字符」的注释
    // 会连同判据一起失去依据，而生成物照样能编译过。
    anyhow::ensure!(
        presentation.is_subset(&emoji),
        "Emoji_Presentation 不再是 Emoji 的子集，取值口径需重新确认"
    );

    let ranges = to_ranges(&emoji);
    let first = ranges.first().expect("非空").0;
    let last = ranges.last().expect("非空").1;
    // BMP 与增补面之间那个大空洞：汉字、假名、PUA、全角形式全落在里面，判定端靠一次
    // 比较就能整片排除。取自数据而不是手写常量——手写的那种迟早与表脱节。
    let bmp_last = ranges
        .iter()
        .rfind(|r| r.1 <= 0xFFFF)
        .expect("BMP 内必有 emoji")
        .1;
    let supp_first = ranges
        .iter()
        .find(|r| r.0 > 0xFFFF)
        .expect("增补面内必有 emoji")
        .0;

    let mut s = String::new();
    s.push_str(&format!(
        "//! Unicode emoji 属性区间表——**由 `wind-tools/gen_emoji_props` 生成，请勿手改**。\n\
         //!\n\
         //! 数据源: Unicode `emoji-data.txt`（UTS #51）版本 **{version}**，属性 `Emoji`。\n\
         //! 判定逻辑、取舍论证与测试在同目录的 `charemoji.rs`；本文件只有数据。\n\
         //!\n\
         //! 重新生成见 `gen_emoji_props` 的模块头。数据许可证 Unicode-3.0，见 NOTICE.md。\n\
         \n\
         /// 生成这张表所用的 Unicode Emoji 版本，供日志与「表有多旧」的排查。\n\
         pub const UNICODE_EMOJI_VERSION: &str = \"{version}\";\n\
         \n\
         /// `Emoji=Yes` 的码位，合并成升序、互不相邻的闭区间。共 {n_cp} 个码位、{n_rg} 段。\n\
         ///\n\
         /// 有序性是 `charemoji::is_emoji` 二分查找的正确性前提，由 `ranges_are_sorted`\n\
         /// 钉住——生成器本就按升序输出，那条测试防的是有人手改这张表。\n\
         pub const EMOJI_RANGES: &[(u32, u32)] = &[\n",
        n_cp = emoji.len(),
        n_rg = ranges.len(),
    ));
    for (a, b) in &ranges {
        s.push_str(&format!("    (0x{a:04X}, 0x{b:04X}),\n"));
    }
    s.push_str(&format!(
        "];\n\
         \n\
         /// 表内最小/最大码位，以及 BMP 末尾到增补面之间那个大空洞的两端。\n\
         ///\n\
         /// `is_emoji` 用这四个数做早退：ASCII 字母、汉字、假名、PUA、全角形式全都落在\n\
         /// 空洞里或表外，三次比较即可返回，不必进二分。全部由生成器从数据算出。\n\
         pub const EMOJI_FIRST: u32 = 0x{first:04X};\n\
         pub const EMOJI_LAST: u32 = 0x{last:04X};\n\
         pub const EMOJI_BMP_LAST: u32 = 0x{bmp_last:04X};\n\
         pub const EMOJI_SUPP_FIRST: u32 = 0x{supp_first:04X};\n"
    ));

    let mut f = std::fs::File::create(&args.out)
        .map_err(|e| anyhow::anyhow!("创建 {} 失败: {e}", args.out.display()))?;
    f.write_all(s.as_bytes())?;

    eprintln!(
        "Unicode Emoji {version}: Emoji={} 码位 / {} 段（Emoji_Presentation={}），\
         早退边界 {first:04X}..{bmp_last:04X} + {supp_first:04X}..{last:04X} → {}",
        emoji.len(),
        ranges.len(),
        presentation.len(),
        args.out.display()
    );
    Ok(())
}
