//! gen_aux_code：把上游辅助码原始档转成本仓的 `字=码` 辅助码表
//!
//! 用法：`gen_aux_code --cache <.cache 目录> --out <schemas/aux_code 目录> [--schema-out <schemas 目录>]`
//!
//! 给了 `--schema-out` 时另产出笔画**输入方案**的词库 `<schemas>/stroke/stroke.dict.yaml`
//! （方案文件 `stroke.schema.toml` 入库在 `data/schemas/`，本工具不写它）。见下方
//! 「笔画方案词库」一节。
//!
//! # 为什么需要这个工具
//!
//! 辅助码表全部来自第三方仓库，按 `NOTICE.md` 的政策不入版本库（`rime-stroke` 是
//! LGPL-3.0）：构建时下载、产物随发行版分发并适用原许可。本工具把 `.cache/aux-code/`
//! 下的上游原始档转成运行时格式。
//!
//! # 三张表的加工差异
//!
//! | 表 | 上游 | 加工 |
//! |---|---|---|
//! | `flypy_full.txt`（小鹤形码） | rime-lua-aux-code | **零转换**，只补元数据头 |
//! | `ZRM-wanxiang.txt`（自然码形码） | 同上 | **零转换**，只补元数据头 |
//! | `stroke.txt`（笔画） | rime-stroke `.dict.yaml` | 剥 YAML 头 + `\t`→`=` + **按字集裁剪** |
//!
//! 前两张已是 `字=码` 行格式，逐行与上游一致（本工具只在首部补
//! `# name/version/source/license`，供运行时显示码表名与追溯来源）。
//!
//! # stroke 的字集裁剪
//!
//! 上游笔画表覆盖约 11 万字，全量入内存对默认关闭的功能过重，故按常用字集裁剪。
//! 字集必须写在代码里、可复现（见 [`CHARSET_FILES`]）；背景见
//! `docs/design/aux-code-schema-source.md` 的「笔画改为码表方案」。
//!
//! # 笔画方案词库（`--schema-out`）
//!
//! 与 `stroke.txt` 出自同一份解析结果（[`parse_stroke`]），字集与码序逐一相同，
//! 引用 `schema:stroke` 与引用 `stroke.txt` 的筛选结果必须一致。`stroke.txt` 仍照旧产出：
//! 用户 override 里写死的 `aux_code/stroke.txt` 不能失效。
//!
//! 唯一多出的是权重：全 0 时既排不出序、wdat 前缀检索也无从剪枝。取 rime-frost 单字表
//! 字频（同字多音取最大），按 [`scale_weight`] 对数压进约定值域 `0~10000`。

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

/// stroke 裁剪用的字集文件（相对 `.cache/aux-code/charset/`，取并集）。
///
/// - `GB 18030-2000`：国标基本集，覆盖 CJK 基本区 + 扩展 A 的通行部分
/// - `《通用规范汉字表》（2013年）`：现代汉语规范字，补 GB18030 之外的规范字形
/// - `Unicode-CJK 〇`：`〇`（U+3007）不在任何汉字区块里，单列一张表
///
/// 要覆盖更多字（如日韩专用汉字）在此加表即可——加表只会让表变大，不改变已有字的码。
const CHARSET_FILES: &[&str] = &[
    "GB 18030-2000.txt",
    "《通用规范汉字表》（2013年）.txt",
    "Unicode-CJK 〇.txt",
];

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (cache, out, schema_out) = parse_args(&args)?;
    let src = cache.join("aux-code");
    std::fs::create_dir_all(&out)?;

    // 1. 已是 `字=码` 格式的两张：原样透传 + 补元数据头。
    for (file, name, source) in [
        (
            "flypy_full.txt",
            "小鹤",
            "https://github.com/HowcanoeWang/rime-lua-aux-code/blob/main/aux_code/flypy_full.txt",
        ),
        (
            "ZRM-wanxiang.txt",
            "自然码",
            "https://github.com/HowcanoeWang/rime-lua-aux-code/blob/main/aux_code/ZRM-wanxiang.txt",
        ),
    ] {
        let n = passthrough(&src.join(file), &out.join(file), name, source, "MIT")?;
        eprintln!("  {file} ({n} 条，零转换) → {}", out.join(file).display());
    }

    // 2. 笔画表：YAML → `字=码` + 字集裁剪。两份产物共用这一次解析。
    let rows = load_stroke_rows(&src)?;
    let n = write_stroke_txt(&rows, &out.join("stroke.txt"))?;
    eprintln!(
        "  stroke.txt ({n} 条) → {}",
        out.join("stroke.txt").display()
    );

    // 3. 笔画方案词库（可选）：同一份行 + rime-frost 字频。
    if let Some(schemas) = schema_out {
        let weights = load_frost_weights(&cache.join("rime-frost").join("cn_dicts"))?;
        let dst = schemas.join("stroke").join("stroke.dict.yaml");
        std::fs::create_dir_all(dst.parent().expect("dst 有父目录"))?;
        let (n, weighted) = write_stroke_dict(&rows, &weights, &dst)?;
        eprintln!(
            "  stroke.dict.yaml ({n} 条，{weighted} 字有字频) → {}",
            dst.display()
        );
    }

    eprintln!("gen_aux_code: 完成");
    Ok(())
}

/// 元数据头。运行时只读第 1 行的 `# name:`（见 wind-aux-code::loader），
/// 其余行是给人看的来源与许可追溯，程序不解析。
fn header(name: &str, source: &str, license: &str) -> String {
    format!("# name: {name}\n# version: 1.0\n# source: {source}\n# license: {license}\n\n")
}

/// 原样透传：上游已是 `字=码` 行格式，只在首部补元数据头。
///
/// 刻意不重新解析再序列化——那会引入「我们以为的格式」与上游实际格式的偏差；
/// 逐行透传使产物与上游**逐字节可比**（剥掉头部后 `diff` 应为空）。
fn passthrough(
    src: &Path,
    dst: &Path,
    name: &str,
    source: &str,
    license: &str,
) -> anyhow::Result<usize> {
    let content = std::fs::read_to_string(src)
        .map_err(|e| anyhow::anyhow!("读取 {} 失败: {e}（先跑 gen-data 下载）", src.display()))?;
    let body: Vec<&str> = content
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let mut f = std::fs::File::create(dst)?;
    f.write_all(header(name, source, license).as_bytes())?;
    for line in &body {
        writeln!(f, "{line}")?;
    }
    Ok(body.len())
}

/// 笔画表的解析结果：字 → 该字全部笔画码（上游行序）。`BTreeMap` 使产物按字排序、可复现。
type StrokeRows = BTreeMap<char, Vec<String>>;

/// 读 rime-stroke 原档并按 [`CHARSET_FILES`] 裁剪。
fn load_stroke_rows(src_dir: &Path) -> anyhow::Result<StrokeRows> {
    let charset = load_charset(&src_dir.join("charset"))?;
    let yaml_path = src_dir.join("stroke.dict.yaml");
    let content = std::fs::read_to_string(&yaml_path).map_err(|e| {
        anyhow::anyhow!(
            "读取 {} 失败: {e}（先跑 gen-data 下载）",
            yaml_path.display()
        )
    })?;
    Ok(parse_stroke(&content, &|ch| charset.contains(&ch)))
}

/// 笔画表：rime-stroke 的 `.dict.yaml`（`字\t笔画码`，YAML 头以 `...` 结束）→ 字 → 码，
/// 只留 `keep` 认可的字。
///
/// 同字多码保留上游行序（行序即优先级，见 wind-aux-code::table 的 first-seen 语义）。
fn parse_stroke(content: &str, keep: &dyn Fn(char) -> bool) -> StrokeRows {
    let mut rows = StrokeRows::new();
    for line in dict_body(content) {
        let mut cols = line.split('\t');
        let (Some(text), Some(code)) = (cols.next(), cols.next()) else {
            continue;
        };
        // 只取单字：max_phrase_length = 1 的表本就无词条，多字行是异常数据。
        let Some(ch) = single_char(text) else {
            continue;
        };
        if code.is_empty() || !keep(ch) {
            continue;
        }
        let codes = rows.entry(ch).or_default();
        if !codes.iter().any(|c| c == code) {
            codes.push(code.to_string());
        }
    }
    rows
}

/// librime `.dict.yaml` 的正文行：YAML front matter 以单独一行 `...` 结束；
/// 已剥行尾空白，跳过空行与 `#` 注释。
fn dict_body(content: &str) -> impl Iterator<Item = &str> {
    content
        .lines()
        .skip_while(|l| l.trim_end() != "...")
        .skip(1)
        .map(str::trim_end)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// 恰好一个字符时返回它。
fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => Some(ch),
        _ => None,
    }
}

/// 写 `stroke.txt`（`字=码`）。返回条目数。
///
/// 产物须与引入方案词库之前**逐字节相同**：用户 override 里写死的 `aux_code/stroke.txt`
/// 要继续可用，且改它会让「方案来源 vs 文件来源筛选一致」失去对照基准。
fn write_stroke_txt(rows: &StrokeRows, dst: &Path) -> anyhow::Result<usize> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(dst)?);
    f.write_all(
        header(
            "笔画",
            "https://github.com/rime/rime-stroke/blob/master/stroke.dict.yaml",
            "GNU Lesser General Public License v3.0",
        )
        .as_bytes(),
    )?;
    writeln!(
        f,
        "# 字集裁剪（gen_aux_code::CHARSET_FILES）：{}",
        CHARSET_FILES.join(" ∪ ")
    )?;
    writeln!(f, "# 字集来源：https://github.com/zispace/hanzi-chars\n")?;
    let mut total = 0usize;
    for (ch, codes) in rows {
        for code in codes {
            writeln!(f, "{ch}={code}")?;
            total += 1;
        }
    }
    f.flush()?;
    Ok(total)
}

/// 权重来源：rime-frost 的单字表（相对 `.cache/rime-frost/cn_dicts/`）。
/// 8105 是通用规范字（字频完整），41448 补其余字（绝大多数权重为 0，少数有值）。
const FROST_CHAR_FILES: &[&str] = &["8105.dict.yaml", "41448.dict.yaml"];

/// 读 [`FROST_CHAR_FILES`]，得字 → 字频。缺文件即报错（只有要产出方案词库时才读）。
fn load_frost_weights(dir: &Path) -> anyhow::Result<HashMap<char, u64>> {
    let mut weights = HashMap::new();
    for name in FROST_CHAR_FILES {
        let path = dir.join(name);
        let content = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "读取字频 {} 失败: {e}（先跑 gen-data 下载）",
                path.display()
            )
        })?;
        merge_frost_weights(&content, &mut weights);
    }
    anyhow::ensure!(
        weights.values().any(|w| *w > 0),
        "字频全为 0，检查 {} 下的单字表",
        dir.display()
    );
    Ok(weights)
}

/// 解析 rime-frost 单字表（`字\t拼音\t权重`）并入 `into`。多音字每个读音一行，
/// **取最大**——字形码不分读音，字的常用度看它最常用的那个读音。
/// 权重列缺失或非整数的行跳过（不当 0 覆盖别的读音）。
fn merge_frost_weights(content: &str, into: &mut HashMap<char, u64>) {
    for line in dict_body(content) {
        let mut cols = line.split('\t');
        let (Some(text), Some(_), Some(w)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let (Some(ch), Ok(w)) = (single_char(text), w.trim().parse::<u64>()) else {
            continue;
        };
        let slot = into.entry(ch).or_insert(0);
        *slot = (*slot).max(w);
    }
}

/// 词库权重的约定上界（`wind_dict::WEIGHT_RANGE_MAX`；本工具不依赖 wind-dict，故抄值）。
const WEIGHT_MAX: u64 = 10_000;

/// 字频 → 词库权重：`round(WEIGHT_MAX · ln(1+w) / ln(1+max))`，0 仍是 0。
///
/// 用对数而非线性：字频是长尾分布，线性压缩会把绝大多数字压成 0 或 1，
/// 同码重码之间就又排不出序了（同 `docs/design/dict-weight-normalization.md` §3.1）。
/// 单调映射，保序；只有原始字频极接近的头部字可能并列。
fn scale_weight(w: u64, max: u64) -> u64 {
    if w == 0 || max == 0 {
        return 0;
    }
    let v = (WEIGHT_MAX as f64 * (w as f64).ln_1p() / (max as f64).ln_1p()).round() as u64;
    v.clamp(1, WEIGHT_MAX)
}

/// 写笔画方案词库 `stroke.dict.yaml`（`字\t码\t权重`）。字与码序同 [`write_stroke_txt`]。
/// 返回 (条目数, 有字频的字数)。
fn write_stroke_dict(
    rows: &StrokeRows,
    weights: &HashMap<char, u64>,
    dst: &Path,
) -> anyhow::Result<(usize, usize)> {
    let raw = |ch: &char| weights.get(ch).copied().unwrap_or(0);
    let max = rows.keys().map(raw).max().unwrap_or(0);
    let mut f = std::io::BufWriter::new(std::fs::File::create(dst)?);
    write!(
        f,
        "# Rime dictionary\n\
         # encoding: utf-8\n\
         #\n\
         # 笔画输入方案词库（h/s/p/n/z = 横竖撇捺折），由 wind-tools/gen_aux_code 生成，勿手工编辑。\n\
         # 编码：rime-stroke https://github.com/rime/rime-stroke\n\
         #   许可：GNU Lesser General Public License v3.0；码未作改动，仅按字集裁剪\n\
         # 字集裁剪（gen_aux_code::CHARSET_FILES）：{}\n\
         # 字集来源：https://github.com/zispace/hanzi-chars\n\
         # 权重：rime-frost https://github.com/gaboolic/rime-frost 单字表（{}）的字频，\n\
         #   许可：GNU General Public License v3.0；同字多音取最大，按对数压进 0~{}，无字频为 0\n\
         ---\n\
         name: stroke\n\
         version: \"1.0\"\n\
         columns: [text, code, weight]\n\
         ...\n",
        CHARSET_FILES.join(" ∪ "),
        FROST_CHAR_FILES.join(" / "),
        WEIGHT_MAX
    )?;
    let (mut total, mut weighted) = (0usize, 0usize);
    for (ch, codes) in rows {
        let w = scale_weight(raw(ch), max);
        if w > 0 {
            weighted += 1;
        }
        for code in codes {
            writeln!(f, "{ch}\t{code}\t{w}")?;
            total += 1;
        }
    }
    f.flush()?;
    Ok((total, weighted))
}

/// 读字集目录下 [`CHARSET_FILES`] 列出的文件，取汉字并集。
///
/// 字表文件是「每行若干汉字 + `#` 注释」的自由格式，故按字符收集而非按行——
/// 只收 U+2E80 以上（CJK 相关区段起点），滤掉行内的 ASCII 序号与标点。
fn load_charset(dir: &Path) -> anyhow::Result<std::collections::HashSet<char>> {
    let mut set = std::collections::HashSet::new();
    for name in CHARSET_FILES {
        let path = dir.join(name);
        let content = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "读取字集 {} 失败: {e}（先跑 gen-data 下载）",
                path.display()
            )
        })?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            set.extend(line.chars().filter(|c| *c as u32 > 0x2E80));
        }
    }
    anyhow::ensure!(
        !set.is_empty(),
        "字集为空，检查 {} 下的字表文件",
        dir.display()
    );
    Ok(set)
}

fn parse_args(args: &[String]) -> anyhow::Result<(PathBuf, PathBuf, Option<PathBuf>)> {
    let mut cache = None;
    let mut out = None;
    let mut schema_out = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--cache" if i + 1 < args.len() => {
                cache = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--out" if i + 1 < args.len() => {
                out = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--schema-out" if i + 1 < args.len() => {
                schema_out = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            _ => i += 1,
        }
    }
    match (cache, out) {
        (Some(c), Some(o)) => Ok((c, o, schema_out)),
        _ => {
            anyhow::bail!(
                "用法: gen_aux_code --cache <.cache 目录> --out <schemas/aux_code 目录> \
                 [--schema-out <schemas 目录>]"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STROKE: &str = "# 头注释\n---\nname: stroke\n...\n\
        # 正文注释\n\
        一\th\n\
        丁\ths\n\
        丁\ths\n\
        丁\thsz\n\
        𠀀\thh\n\
        一二\thhh\n\
        乙\t\n\
        \n";

    #[test]
    fn parse_stroke_keeps_charset_single_chars_in_upstream_order() {
        let rows = parse_stroke(STROKE, &|c| c != '𠀀');
        let got: Vec<(char, Vec<&str>)> = rows
            .iter()
            .map(|(c, v)| (*c, v.iter().map(String::as_str).collect()))
            .collect();
        // 字集外（𠀀）、多字行、空码行都丢；同字重复码去重、异码保留行序。
        assert_eq!(got, vec![('一', vec!["h"]), ('丁', vec!["hs", "hsz"])]);
    }

    #[test]
    fn frost_weights_take_max_over_readings() {
        let mut w = HashMap::new();
        merge_frost_weights(
            "---\nname: 8105\n...\n### 字表\n的\tde\t500\n的\tdi\t0\n一\tyi\t9\n坏\tpi\n词语\tci yu\t7\n",
            &mut w,
        );
        // 第二个文件里同字更大的读音也要能抬高。
        merge_frost_weights("...\n一\tyi\t20\n的\tdi\t3\n", &mut w);
        assert_eq!(w.get(&'的'), Some(&500));
        assert_eq!(w.get(&'一'), Some(&20));
        assert_eq!(w.get(&'坏'), None, "缺权重列的行不入表");
        assert_eq!(w.get(&'词'), None, "多字行不入表");
    }

    #[test]
    fn scale_weight_is_monotonic_log_within_range() {
        let max = 15_378_475;
        assert_eq!(scale_weight(0, max), 0);
        assert_eq!(scale_weight(max, max), WEIGHT_MAX);
        assert_eq!(scale_weight(5, 0), 0);
        let (a, b, c) = (
            scale_weight(1, max),
            scale_weight(100, max),
            scale_weight(99_581, max),
        );
        assert!(0 < a && a < b && b < c && c < WEIGHT_MAX, "{a} {b} {c}");
    }

    /// 两份产物出自同一份行：stroke.txt 的 `字=码` 与方案词库的 `字\t码` 逐行对应。
    #[test]
    fn txt_and_dict_share_rows() {
        let dir = std::env::temp_dir().join(format!("gen_aux_code_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rows = parse_stroke(STROKE, &|_| true);
        let weights = HashMap::from([('一', 100u64), ('丁', 10)]);
        let txt = dir.join("stroke.txt");
        let dict = dir.join("stroke.dict.yaml");
        let n_txt = write_stroke_txt(&rows, &txt).unwrap();
        let (n_dict, weighted) = write_stroke_dict(&rows, &weights, &dict).unwrap();
        let txt = std::fs::read_to_string(&txt).unwrap();
        let dict = std::fs::read_to_string(&dict).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!((n_txt, n_dict, weighted), (4, 4, 2));
        let txt_rows: Vec<String> = txt
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect();
        let dict_rows: Vec<String> = dict_body(&dict)
            .map(|l| {
                let mut c = l.split('\t');
                format!("{}={}", c.next().unwrap(), c.next().unwrap())
            })
            .collect();
        assert_eq!(txt_rows, dict_rows);
        assert!(dict.contains("\ncolumns: [text, code, weight]\n...\n"));
        assert!(dict.contains("一\th\t10000\n"), "最高字频 → 值域上界");
        assert!(dict.contains("𠀀\thh\t0\n"), "无字频 → 0");
    }
}
