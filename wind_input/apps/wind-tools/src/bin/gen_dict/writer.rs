//! 词库与分析报告的写出。
//!
//! 所有输出都走 `.tmp` + rename 的原子替换：生成过程被中断时，宁可留下旧词库，
//! 也不能留下半截文件——半截的 .dict.yaml 会被引擎当成完整词库加载。

use crate::config::Config;
use crate::entry::Entry;
use crate::extra::Category;
use crate::order_report::{Change, Summary};
use crate::shortcode::Conflict;
use std::io::{BufWriter, Write};
use std::path::Path;

/// 词库头部里不解析 `sort:` 的说明。
///
/// 那是 librime 的库内同码排序键，WindInput 从不读它。写出来只会让人以为改它有用——
/// 排序实际由方案 `wubi86.schema.toml` 的 `[[dictionaries]]` 决定。
const SORT_NOTE: &[&str] = &[
    "# 排序不由本文件决定，见方案 wubi86.schema.toml 的 [[dictionaries]]：",
    "# base_order 定库间分档，default_weight 抹平整库权重可退化为文件顺序。",
];

fn atomic_write(
    path: &Path,
    render: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let f = std::fs::File::create(&tmp)?;
        let mut bw = BufWriter::new(f);
        if let Err(e) = render(&mut bw) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        bw.flush()?;
    }
    // Windows 的 rename 不覆盖已存在目标，须先删
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn write_frost_dict(path: &Path, entries: &[Entry], version: &str) -> anyhow::Result<()> {
    atomic_write(path, |w| {
        writeln!(w, "# 白霜拼音基础词库生成的五笔补充词库，GPL-3.0")?;
        writeln!(w, "# 来源: https://github.com/gaboolic/rime-frost")?;
        writeln!(w, "---")?;
        writeln!(w, "name: wubi86_frost")?;
        writeln!(w, "version: \"{version}\"")?;
        writeln!(w, "columns:")?;
        writeln!(w, "  - code")?;
        writeln!(w, "  - text")?;
        writeln!(w, "  - weight")?;
        writeln!(w, "...")?;
        for e in entries {
            writeln!(w, "{}\t{}\t{}", e.code, e.text, e.weight)?;
        }
        Ok(())
    })
}

/// 写出五笔主词库。
pub fn write_main_dict(
    path: &Path,
    entries: &[Entry],
    cfg: &Config,
    version: &str,
) -> anyhow::Result<()> {
    atomic_write(path, |w| {
        writeln!(w, "# Rime dictionary")?;
        writeln!(w, "# encoding: utf-8")?;
        writeln!(w, "#")?;
        writeln!(w, "# WindInput 五笔86词库")?;
        writeln!(
            w,
            "# 来源: rime-wubi86-jidian (https://github.com/KyleBing/rime-wubi86-jidian)"
        )?;
        writeln!(
            w,
            "# 处理: 按 unigram 真实词频重新排序，单字提权（×{:.1}），生僻字保底权重",
            cfg.char_boost_factor
        )?;
        writeln!(w, "# 生成: wind-tools/gen_dict  版本: {version}")?;
        writeln!(w, "---")?;
        writeln!(w, "name: {}", cfg.output_name)?;
        writeln!(w, "version: \"{version}\"")?;
        for line in SORT_NOTE {
            writeln!(w, "{line}")?;
        }
        if !cfg.import_tables.is_empty() {
            writeln!(w, "import_tables:")?;
            for t in &cfg.import_tables {
                writeln!(w, "  - {t}")?;
            }
        }
        writeln!(w, "columns:")?;
        writeln!(w, "  - code")?;
        writeln!(w, "  - text")?;
        writeln!(w, "  - weight")?;
        writeln!(w, "...")?;
        for e in entries {
            writeln!(w, "{}\t{}\t{}", e.code, e.text, e.weight)?;
        }
        Ok(())
    })
}

/// 写出拆分后的扩展词库。
pub fn write_extra_dict(
    path: &Path,
    entries: &mut [Entry],
    name: &str,
    cat: Category,
    version: &str,
) -> anyhow::Result<()> {
    // 编码升序 → 同码权重降序 → 文本升序（末级保证同权时输出稳定）
    entries.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then_with(|| b.weight.cmp(&a.weight))
            .then_with(|| a.text.cmp(&b.text))
    });

    atomic_write(path, |w| {
        writeln!(
            w,
            "# Rime dictionary - WindInput 五笔扩展词库 ({})",
            cat.suffix()
        )?;
        writeln!(
            w,
            "# 来源: rime-wubi86-jidian extra，由 gen_dict 按字符类型拆分"
        )?;
        writeln!(w, "# 生成: {version}")?;
        writeln!(w, "---")?;
        writeln!(w, "name: {name}")?;
        writeln!(w, "version: \"{version}\"")?;
        for line in SORT_NOTE {
            writeln!(w, "{line}")?;
        }
        writeln!(w, "use_preset_vocabulary: false")?;
        writeln!(w, "columns:")?;
        writeln!(w, "  - code")?;
        writeln!(w, "  - text")?;
        writeln!(w, "  - weight")?;
        writeln!(w, "...")?;
        for e in entries.iter() {
            writeln!(w, "{}\t{}\t{}", e.code, e.text, e.weight)?;
        }
        Ok(())
    })
}

/// 写出被过滤条目清单（供人工复核过滤规则是否误伤）。
pub fn write_dropped(path: &Path, dropped: &[(&'static str, Entry)]) -> anyhow::Result<()> {
    let mut sorted: Vec<&(&str, Entry)> = dropped.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0).then_with(|| a.1.code.cmp(&b.1.code)));

    atomic_write(path, |w| {
        writeln!(w, "reason\tcode\ttext\torig_weight")?;
        for (reason, e) in sorted {
            // text 里的 TAB 会破坏 TSV 列对齐
            let text = e.text.replace('\t', " ");
            writeln!(w, "{reason}\t{}\t{text}\t{}", e.code, e.weight)?;
        }
        Ok(())
    })
}

pub fn write_conflict_report(path: &Path, conflicts: &[Conflict]) -> anyhow::Result<()> {
    atomic_write(path, |w| {
        writeln!(
            w,
            "conflict_type\tchar\tshort_code\tlong_code\tcount\ttop_candidates"
        )?;
        for c in conflicts {
            let top = if c.top_candidates.is_empty() {
                "-".to_string()
            } else {
                c.top_candidates
                    .iter()
                    .map(|t| format!("{}({})", t.text, t.weight))
                    .collect::<Vec<_>>()
                    .join(" > ")
            };
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{top}",
                c.kind, c.char_text, c.short_code, c.long_code, c.candidates_count
            )?;
        }
        Ok(())
    })
}

/// 写出降权待处理报告：仅含有竞争候选的 full4 冲突，带调参所需的评估列。
///
/// 传入的应是**降权前**的冲突快照——降权后的权重已经是调整结果，拿它调参会自我循环。
pub fn write_demotion_report(path: &Path, conflicts: &[Conflict]) -> anyhow::Result<()> {
    let demotions: Vec<&Conflict> = conflicts
        .iter()
        .filter(|c| {
            (c.kind == "level2_full4" || c.kind == "level3_full4") && c.candidates_count > 1
        })
        .collect();
    if demotions.is_empty() {
        return Ok(());
    }

    atomic_write(path, |w| {
        writeln!(
            w,
            "type\tchar\tshort\tlong\tchar_wt\t2nd\t2nd_wt\t2nd_is_char\tgap\tcount\ttop_candidates"
        )?;
        for c in demotions {
            if c.top_candidates.len() < 2 {
                continue;
            }
            let top = &c.top_candidates[0];
            let second = &c.top_candidates[1];
            let gap = top.weight - second.weight;
            let is_char = if second.text.chars().take(2).count() == 1 {
                "Y"
            } else {
                "N"
            };
            let tops = c
                .top_candidates
                .iter()
                .take(10)
                .map(|t| format!("{}({})", t.text, t.weight))
                .collect::<Vec<_>>()
                .join(" > ");
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{is_char}\t{gap}\t{}\t{tops}",
                c.kind,
                c.char_text,
                c.short_code,
                c.long_code,
                top.weight,
                second.text,
                second.weight,
                c.candidates_count
            )?;
        }
        Ok(())
    })
}

/// 写出候选顺序变化报告：上游原序 vs 产物最终序。
///
/// 第一列 `against_upstream` 是判读的入口——`Y` 表示上游用不同的优先级明确安排过次序
/// 而我们把它翻了过来；`N` 表示上游整组同档、没表过态，换首选无所谓对错。
/// 不分这两档就只剩一个「多少个首选变了」的大数字，指导不了任何修正。
pub fn write_order_report(
    path: &Path,
    changes: &[Change],
    summary: &Summary,
) -> anyhow::Result<()> {
    atomic_write(path, |w| {
        writeln!(w, "# 候选顺序变化: 上游 rime-wubi86-jidian → gen_dict 产物")?;
        writeln!(
            w,
            "# 可比码={} 顺序变化={} 首选变化={} 其中违逆上游明确优先级={}",
            summary.comparable,
            summary.order_changed,
            summary.top_changed,
            summary.top_changed_against_upstream
        )?;
        writeln!(
            w,
            "# up_order 括号内是上游原始优先级(10/20/…/60)，gen_order 括号内是产物最终权重"
        )?;
        writeln!(
            w,
            "against_upstream\ttop_changed\tcause\tcode\tn\tup_top\tup_prio\tgen_top\tgen_prio\tgen_wt\tup_freq\tgen_freq\tup_order\tgen_order"
        )?;
        for c in changes {
            let yn = |b: bool| if b { "Y" } else { "N" };
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                yn(c.upstream_had_opinion),
                yn(c.top_changed),
                c.cause,
                c.code,
                c.count,
                c.up_top,
                c.up_top_priority,
                c.gen_top,
                c.gen_top_priority,
                c.gen_top_weight,
                c.up_top_freq,
                c.gen_top_freq,
                c.up_order.join(" > "),
                c.gen_order.join(" > "),
            )?;
        }
        Ok(())
    })
}

/// 透传上游原样词库：内容一字不改，只清洗头部的 `sort:` 键。
///
/// 用于 district 这类无需重排的词库——它的条目顺序本身就是数据（按行政区划层级排列），
/// 重排反而破坏语义。但头部的 `sort:` 仍要清掉，否则加载时会触发 `parse_sort_header`
/// 告警，且与另外 5 个库的头部不一致。
///
/// 返回是否确实清掉了 `sort:` 键（false 说明上游已经没有这个键了）。
pub fn passthrough_stripping_sort(src: &Path, dst: &Path) -> anyhow::Result<bool> {
    let text = std::fs::read_to_string(src)
        .map_err(|e| anyhow::anyhow!("读取 {} 失败: {e}", src.display()))?;

    let mut out = String::with_capacity(text.len());
    let mut in_header = true;
    let mut stripped = false;
    // 上游在 `sort:` 下面跟了一行解释取值的注释，删了键就要一起删，否则留下孤儿注释
    let mut drop_next_sort_comment = false;

    // 用 split_inclusive 而非 lines()：后者会吃掉 CRLF 的 \r、并给无末尾换行的文件
    // 补上一个换行。透传的承诺是"内容一字不改"，这两处都会破坏它。
    for seg in text.split_inclusive('\n') {
        let content = seg.trim_end_matches(['\n', '\r']);
        let eol = &seg[content.len()..]; // 该行原本的行尾（可能为空）

        if in_header {
            let t = content.trim();
            if t == "..." {
                in_header = false;
                out.push_str(seg);
                continue;
            }
            if t.starts_with("sort:") {
                for note in SORT_NOTE {
                    out.push_str(note);
                    out.push_str(if eol.is_empty() { "\n" } else { eol });
                }
                stripped = true;
                drop_next_sort_comment = true;
                continue;
            }
            if drop_next_sort_comment {
                drop_next_sort_comment = false;
                if t.starts_with('#') && t.contains("排序方式") {
                    continue;
                }
            }
        }
        out.push_str(seg);
    }

    atomic_write(dst, |w| w.write_all(out.as_bytes()))?;
    Ok(stripped)
}

// ── 版本日期 ──────────────────────────────────────────

/// 当前 UTC 日期，`YYYY-MM-DD`。
///
/// 用 UTC 而非本地时区，是为了让同一份源数据在任何机器上都得到相同的版本戳——
/// 这个字符串会写进词库头部，跨时区的本地日期会造成无意义的产物差异。
/// 需要精确复现历史产物时用 `--version-date` 显式指定。
pub fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// days-since-epoch → (年, 月, 日)，Howard Hinnant 的 civil_from_days。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_conversion_matches_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1), "epoch");
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29), "闰年 2 月 29 日");
        assert_eq!(civil_from_days(-1), (1969, 12, 31), "epoch 之前");
    }

    #[test]
    fn today_is_well_formed() {
        let t = today_utc();
        assert_eq!(t.len(), 10, "YYYY-MM-DD: {t}");
        assert_eq!(t.matches('-').count(), 2);
        assert!(t.starts_with("20"), "本世纪内: {t}");
    }

    #[test]
    fn main_dict_header_declares_column_order() {
        let dir = std::env::temp_dir().join("gen_dict_writer_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("out.dict.yaml");
        let cfg = Config {
            jidian_path: "a".into(),
            unigram_path: "b".into(),
            output_path: "c".into(),
            output_name: "test_dict".into(),
            ..Default::default()
        };
        let entries = vec![Entry::new("中".into(), "khk".into(), 500, 0)];
        write_main_dict(&p, &entries, &cfg, "2026-01-01").unwrap();

        let s = std::fs::read_to_string(&p).unwrap();
        let (header, body) = s.split_once("\n...\n").expect("须有 ... 分隔");
        assert!(header.contains("name: test_dict"));
        assert!(header.contains("version: \"2026-01-01\""));
        assert!(!header.contains("sort:"), "不得写出 librime 的 sort: 键");
        assert!(
            header.contains("columns:\n  - code\n  - text\n  - weight"),
            "列序声明须与数据行一致"
        );
        assert_eq!(body, "khk\t中\t500\n", "数据行为 code\\ttext\\tweight");
    }

    #[test]
    fn extra_dict_sorted_by_code_then_weight_desc() {
        let dir = std::env::temp_dir().join("gen_dict_writer_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("extra.dict.yaml");
        let mut entries = vec![
            Entry::new("低".into(), "emoj".into(), 100, 0),
            Entry::new("高".into(), "emoj".into(), 200, 1),
            Entry::new("甲".into(), "aaaa".into(), 50, 2),
        ];
        write_extra_dict(&p, &mut entries, "t", Category::Emoji, "2026-01-01").unwrap();

        let s = std::fs::read_to_string(&p).unwrap();
        let body = s.split_once("\n...\n").unwrap().1;
        assert_eq!(
            body, "aaaa\t甲\t50\nemoj\t高\t200\nemoj\t低\t100\n",
            "编码升序，同码权重降序"
        );
    }

    #[test]
    fn passthrough_strips_sort_key_and_its_comment() {
        let dir = std::env::temp_dir().join("gen_dict_writer_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("district_src.yaml");
        let dst = dir.join("district_dst.yaml");
        // 复刻上游 district 的头部形状
        std::fs::write(
            &src,
            "---\nname: d\nversion: \"2022-12-16\"\ndict_grouped: true\n\
             sort: original\n# 码表的排序方式: by_weight 权重，original 原始顺序\n\
             \n# 内容格式说明\n...\n甲\taaaa\t1\n乙\tbbbb\t2\n",
        )
        .unwrap();

        assert!(passthrough_stripping_sort(&src, &dst).unwrap());
        let s = std::fs::read_to_string(&dst).unwrap();
        assert!(!s.contains("sort: original"), "sort: 键须被清掉");
        assert!(
            !s.contains("码表的排序方式"),
            "其解释注释须一并清掉，不留孤儿注释"
        );
        assert!(s.contains("base_order 定库间分档"), "须换上统一的说明注释");
        assert!(s.contains("# 内容格式说明"), "其余头部注释保持原样");
        assert_eq!(
            s.split_once("\n...\n").unwrap().1,
            "甲\taaaa\t1\n乙\tbbbb\t2\n",
            "数据行必须一字不改"
        );
    }

    #[test]
    fn passthrough_preserves_missing_trailing_newline() {
        // 上游 district 末尾没有换行；补一个就不再是"一字不改"
        let dir = std::env::temp_dir().join("gen_dict_writer_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("noeol_src.yaml");
        let dst = dir.join("noeol_dst.yaml");
        std::fs::write(&src, "---\nname: d\nsort: original\n...\n甲\taaaa\t1").unwrap();
        passthrough_stripping_sort(&src, &dst).unwrap();
        let s = std::fs::read_to_string(&dst).unwrap();
        assert!(s.ends_with("甲\taaaa\t1"), "末尾不得凭空多出换行: {s:?}");
    }

    #[test]
    fn passthrough_preserves_crlf_line_endings() {
        let dir = std::env::temp_dir().join("gen_dict_writer_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("crlf_src.yaml");
        let dst = dir.join("crlf_dst.yaml");
        std::fs::write(
            &src,
            "---\r\nname: d\r\nsort: original\r\n...\r\n甲\taaaa\t1\r\n",
        )
        .unwrap();
        passthrough_stripping_sort(&src, &dst).unwrap();
        let s = std::fs::read_to_string(&dst).unwrap();
        assert!(s.contains("甲\taaaa\t1\r\n"), "CRLF 须保留，不得静默转 LF");
        assert!(!s.contains("sort: original"));
    }

    #[test]
    fn passthrough_is_idempotent_when_no_sort_key() {
        let dir = std::env::temp_dir().join("gen_dict_writer_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("nosort_src.yaml");
        let dst = dir.join("nosort_dst.yaml");
        std::fs::write(&src, "---\nname: d\n...\n甲\taaaa\t1\n").unwrap();
        assert!(
            !passthrough_stripping_sort(&src, &dst).unwrap(),
            "本就没有 sort: 键"
        );
        assert_eq!(
            std::fs::read_to_string(&dst).unwrap(),
            "---\nname: d\n...\n甲\taaaa\t1\n"
        );
    }

    #[test]
    fn passthrough_ignores_sort_like_lines_in_body() {
        // 数据区里恰好有以 sort: 开头的词条时不得被当成头部键删掉
        let dir = std::env::temp_dir().join("gen_dict_writer_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("body_src.yaml");
        let dst = dir.join("body_dst.yaml");
        std::fs::write(&src, "---\nname: d\n...\nsort:x\tabcd\t1\n").unwrap();
        assert!(!passthrough_stripping_sort(&src, &dst).unwrap());
        assert!(
            std::fs::read_to_string(&dst)
                .unwrap()
                .contains("sort:x\tabcd\t1")
        );
    }

    #[test]
    fn atomic_write_leaves_no_tmp_behind() {
        let dir = std::env::temp_dir().join("gen_dict_writer_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("atomic.dict.yaml");
        let cfg = Config {
            jidian_path: "a".into(),
            unigram_path: "b".into(),
            output_path: "c".into(),
            ..Default::default()
        };
        write_main_dict(&p, &[], &cfg, "2026-01-01").unwrap();
        assert!(p.exists());
        assert!(
            !p.with_extension("tmp").exists(),
            "临时文件须已被 rename 消费"
        );
    }
}
