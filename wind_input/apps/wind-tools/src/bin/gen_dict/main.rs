//! gen_dict：五笔主词库生成与扩展词库拆分。
//!
//! 从极点五笔 rime-wubi86-jidian 原始词库出发，结合 unigram 真实词频重新赋权排序，
//! 输出 WindInput 所用的 rime YAML 词库。
//!
//! 用法：
//!   gen_dict --cache <.cache 目录> --out <词库输出目录> [--report <报告目录>]
//!            [--config <gen_dict.toml>] [--version-date YYYY-MM-DD]
//!
//! 由 WindInput-Go 的 `tools/dictgen` 移植而来。权重与排序的每个数值都会直接改变
//! 发行词库的候选顺序，改动前先读 `data/gen_dict/gen_dict.toml` 的头部说明。

mod boost;
mod config;
mod entry;
mod extra;
mod frost;
mod order_report;
mod parse;
mod reverse;
mod shortcode;
mod upstream_order;
mod weight;
mod writer;

use config::Config;
use entry::Entry;
use std::path::{Path, PathBuf};

struct Args {
    config: PathBuf,
    cache: PathBuf,
    out: PathBuf,
    report: Option<PathBuf>,
    version_date: Option<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let argv: Vec<String> = std::env::args().collect();
    let mut config = None;
    let mut cache = None;
    let mut out = None;
    let mut report = None;
    let mut version_date = None;

    let mut i = 1;
    while i < argv.len() {
        let need = |i: usize| -> anyhow::Result<String> {
            argv.get(i + 1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{} 缺少参数值", argv[i]))
        };
        match argv[i].as_str() {
            "--config" => {
                config = Some(PathBuf::from(need(i)?));
                i += 2;
            }
            "--cache" => {
                cache = Some(PathBuf::from(need(i)?));
                i += 2;
            }
            "--out" => {
                out = Some(PathBuf::from(need(i)?));
                i += 2;
            }
            "--report" => {
                report = Some(PathBuf::from(need(i)?));
                i += 2;
            }
            "--version-date" => {
                version_date = Some(need(i)?);
                i += 2;
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("未知参数: {other}（-h 查看用法）"),
        }
    }

    let config = config.unwrap_or_else(|| {
        // 相对 crate 源码位置的默认配置，便于直接 cargo run
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/gen_dict/gen_dict.toml")
    });
    Ok(Args {
        config,
        cache: cache.ok_or_else(|| anyhow::anyhow!("缺少 --cache <目录>（-h 查看用法）"))?,
        out: out.ok_or_else(|| anyhow::anyhow!("缺少 --out <目录>（-h 查看用法）"))?,
        report,
        version_date,
    })
}

fn print_usage() {
    eprintln!(
        "gen_dict：生成五笔主词库并拆分扩展词库\n\
         \n\
         用法:\n  \
           gen_dict --cache <dir> --out <dir> [选项]\n\
         \n\
         必需:\n  \
           --cache <dir>          源数据目录（含 rime-wubi/ 与 pinyin-frost/）\n  \
           --out <dir>            词库输出目录\n\
         \n\
         选项:\n  \
           --config <path>        配置文件，默认 data/gen_dict/gen_dict.toml\n  \
           --report <dir>         分析报告输出目录，省略则不写报告\n  \
           --version-date <date>  版本戳 YYYY-MM-DD，默认当前 UTC 日期\n"
    );
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let cfg = Config::load(&args.config)?;
    let config_dir = args.config.parent().unwrap_or(Path::new("."));
    let paths = cfg.resolve_paths(config_dir, &args.cache, &args.out, args.report.as_deref());
    let version = args.version_date.clone().unwrap_or_else(writer::today_utc);

    eprintln!("已加载配置: {}", args.config.display());
    eprintln!("  jidian   : {}", paths.jidian.display());
    eprintln!("  unigram  : {}", paths.unigram.display());
    eprintln!("  输出路径 : {}", paths.output.display());
    eprintln!("  版本戳   : {version}");
    eprintln!(
        "  权重归一化: 中位→{}  上限={}  下限={}  单字提权×{:.1}",
        cfg.target_median, cfg.weight_max, cfg.weight_min, cfg.char_boost_factor
    );
    if cfg.shortcodes.enabled {
        eprintln!(
            "  简码分层 : 一级={}  二级基={}  三级基={}  普通上限={}",
            cfg.shortcodes.level1_weight,
            cfg.shortcodes.level2_base_weight,
            cfg.shortcodes.level3_base_weight,
            cfg.regular_weight_max
        );
    }
    eprintln!();

    run(&cfg, &paths, &version, &args.cache)
}

fn run(cfg: &Config, paths: &config::Paths, version: &str, cache: &Path) -> anyhow::Result<()> {
    let mut log = |s: String| eprintln!("{s}");

    // ── 1. unigram ────────────────────────────────────
    let size_mb = std::fs::metadata(&paths.unigram)
        .map(|m| m.len() / 1024 / 1024)
        .unwrap_or(0);
    eprintln!("[1/4] 加载 unigram.txt ({size_mb} MB)...");
    let unigram = weight::load_unigram(&paths.unigram)?;
    eprintln!("      加载完成: {} 条词频记录", unigram.len());

    // ── 2. jidian ─────────────────────────────────────
    eprintln!("[2/4] 加载 jidian 词典...");
    let jidian = parse::parse_jidian(&paths.jidian)?;
    eprintln!("      {}: {} 条", paths.jidian.display(), jidian.len());
    if jidian.is_empty() {
        anyhow::bail!("jidian 词库为空，检查 --cache 路径与源数据是否完整");
    }

    // 单字→首选编码反查表：供自定义词反查与 extra 非法编码修正复用
    let char_codes = reverse::build_char_code_map(&jidian);

    // 上游候选序快照必须在过滤与赋权**之前**拍下：`Entry.weight` 一字两用，赋权会把
    // 上游的原始优先级原地覆盖掉（见 entry.rs），事后无从重建。
    let upstream_order_snapshot = order_report::Snapshot::capture(&jidian);

    // ── 3. 过滤 + 赋权 ────────────────────────────────
    eprintln!("[3/4] 过滤 + 补充词频...");
    let mut kept: Vec<Entry> = Vec::with_capacity(jidian.len());
    let mut dropped: Vec<(&'static str, Entry)> = Vec::new();
    let mut filter_stats: std::collections::BTreeMap<&'static str, usize> = Default::default();
    for e in jidian {
        match entry::should_keep(&e, cfg) {
            Ok(()) => kept.push(e),
            Err(reason) => {
                *filter_stats.entry(reason).or_default() += 1;
                dropped.push((reason, e));
            }
        }
    }
    eprintln!("      保留: {}  过滤: {}", kept.len(), dropped.len());
    let mut stats: Vec<_> = filter_stats.iter().collect();
    stats.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (reason, n) in stats {
        eprintln!("        - {reason}: {n}");
    }

    // 简码分层必须先于 unigram 赋权：赋权阶段靠 shortcode_level 跳过这些条目
    if cfg.shortcodes.enabled {
        shortcode::assign_shortcode_weights(&mut kept, cfg);
        eprintln!(
            "      简码分层: 一级={}  二级={}  三级={}",
            shortcode::count_level(&kept, 1),
            shortcode::count_level(&kept, 2),
            shortcode::count_level(&kept, 3)
        );
    }

    // 归一化基准只取 jidian 过滤后的词条，自定义词不参与（避免少量词拉偏中位）
    let median_raw = weight::median_raw_freq(&kept, &unigram);
    let log_median = (median_raw + 1.0).log10();
    let hit = kept
        .iter()
        .filter(|e| unigram.contains_key(&e.text))
        .count();
    eprintln!(
        "      unigram 命中: {hit} ({}%)  未命中: {}",
        hit * 100 / kept.len(),
        kept.len() - hit
    );
    eprintln!("      中位原始频次: {median_raw:.0}  (log10={log_median:.3})");

    if let Some(p) = &paths.custom_words
        && p.exists()
    {
        eprintln!("      加载自定义词表: {}", p.display());
        match reverse::load_custom_words(p, &char_codes, &unigram, log_median, cfg, &mut log) {
            Ok(v) => {
                eprintln!("      自定义词条: {} 条", v.len());
                kept.extend(v);
            }
            Err(e) => eprintln!("      [警告] 自定义词表加载失败: {e}"),
        }
    }

    let regular_max = cfg.regular_max();
    let mut weight_buckets: std::collections::BTreeMap<String, usize> = Default::default();
    for e in kept.iter_mut() {
        if e.shortcode_level > 0 {
            *weight_buckets.entry("简码".into()).or_default() += 1;
            continue;
        }
        // 上游编码约定保护：整个码跳过词频补权，权重稍后由 `apply_protected_codes` 统一赋。
        // 词频回答「这个词多常用」，码位约定回答「这个码谁该排第一」，两者不可通约——
        // 补权会把 `cccc` 的「又」(原始优先级 40，上游首选) 打成 3010 后再被降权成 1318。
        // 此处保持 weight 为解析出的原始优先级不动，它正是稍后赋权的输入。
        if cfg.is_protected_code(&e.code) {
            *weight_buckets.entry("保护码".into()).or_default() += 1;
            continue;
        }
        let is_char = e.is_single_char();
        // 先读原始优先级再覆写：读到已覆写的值会把生僻字全打成同一档
        let orig_priority = e.weight;
        match unigram.get(&e.text) {
            Some(&freq) => {
                let mut w = weight::compute_weight(freq, log_median, cfg);
                if is_char && cfg.char_boost_factor != 1.0 {
                    w = weight::clamp_weight(
                        (w as f64 * cfg.char_boost_factor).round() as i64,
                        cfg,
                    );
                }
                w = w.min(regular_max);
                e.weight = w;
                let lo = (w / 500) * 500;
                *weight_buckets
                    .entry(format!("{lo}-{}", lo + 499))
                    .or_default() += 1;
            }
            None => {
                e.weight = weight::fallback_weight(orig_priority, cfg);
                *weight_buckets.entry("<200(生僻)".into()).or_default() += 1;
            }
        }
    }

    let n_protected = apply_protected_codes(&mut kept, &unigram, cfg);
    if n_protected > 0 {
        eprintln!(
            "\n      编码约定保护: {n_protected} 条（{} 个码，跳过词频补权与简码降权）",
            cfg.protected_codes.codes.len()
        );
    }

    // 组内权重重排：把同码次序换回上游序，权重值集合不变（该码在权重轴上的高度守恒）。
    //
    // 位置在补权与保护码之后、降权之前：重排是「权重怎么分配」的收尾，而 demotion 是
    // 建立在权重之上的**策略**，须看到最终分配才判得准。
    let uo =
        upstream_order::reapply_upstream_order(&mut kept, &upstream_order_snapshot, &unigram, cfg);
    if uo.examined > 0 {
        eprintln!(
            "\n      上游序回归: {} 个码（判定 {}，护栏拦下：生僻 {} + 倍数>{} 共 {}）",
            uo.reordered,
            uo.examined,
            uo.held_unseen,
            cfg.upstream_order.max_freq_ratio,
            uo.held_unseen + uo.held_ratio
        );
    }

    eprintln!("\n      权重分布预览:");
    let mut buckets: Vec<(&String, &usize)> = weight_buckets.iter().collect();
    buckets.sort_by_key(|(k, _)| {
        if k.as_str() == "<200(生僻)" {
            -1
        } else if k.as_str() == "简码" {
            i64::MAX
        } else if k.as_str() == "保护码" {
            // 保护带就夹在普通带与简码带之间，预览里也照这个位置摆。
            i64::MAX - 1
        } else {
            k.split('-')
                .next()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0)
        }
    });
    for (k, cnt) in buckets {
        let bar = "█".repeat(cnt * 30 / kept.len());
        eprintln!("        {k:>15}: {cnt:>6}  {bar}");
    }

    // 降权前的冲突快照：降权后的权重已是调整结果，拿它调参会自我循环
    let pre_demotion = if cfg.shortcodes.enabled {
        shortcode::analyze_conflicts(&kept)
    } else {
        Vec::new()
    };
    // 顺序报告的成因列取自这两个集合，是**观测到的动作**而非从权重形状反推——
    // 降权结果是「第二候选 -1」，词频补权也能凑出同样的差值。
    let mut demoted_entries: std::collections::BTreeSet<(String, String)> = Default::default();
    let mut boosted_entries: std::collections::BTreeSet<(String, String)> = Default::default();

    if cfg.shortcodes.enabled && cfg.demotion.enabled {
        demoted_entries = shortcode::apply_demotion(&mut kept, cfg);
        if !demoted_entries.is_empty() {
            eprintln!(
                "\n      简码降权: {} 条简码字被降权（第二候选满足权重+gap条件）",
                demoted_entries.len()
            );
        } else {
            eprintln!("\n      简码降权: 无符合条件的降权条目");
        }
    }

    // 词序提升在自动权重与降权之后，是人工微调的最后一道闸
    if let Some(p) = &paths.boosts
        && p.exists()
    {
        eprintln!("\n      加载词序提升表: {}", p.display());
        match boost::load_boost_rules(p) {
            Ok(rules) if !rules.is_empty() => {
                let (applied, missing) = boost::apply_boost_rules(&mut kept, &rules, &mut log);
                eprintln!("      词序提升: {applied} 条生效，{missing} 条未匹配");
                // 未匹配的规则一并收进来无害：其 (code, text) 在产物里不存在，查不到
                boosted_entries = rules
                    .iter()
                    .map(|r| (r.code.clone(), r.text.clone()))
                    .collect();
            }
            Ok(_) => {}
            Err(e) => eprintln!("      [警告] boost 解析失败: {e}"),
        }
    }

    // 编码升序 → 同码权重降序。**不加 text 末级**：同码同权靠稳定排序保持 jidian 行序，
    // 加了会把大量同权条目按字典序重排。
    kept.sort_by(|a, b| a.code.cmp(&b.code).then_with(|| b.weight.cmp(&a.weight)));

    // ── 4. 写出 ───────────────────────────────────────
    eprintln!("\n[4/4] 写出到 {} ...", paths.output.display());
    writer::write_main_dict(&paths.output, &kept, cfg, version)?;
    let size_kb = std::fs::metadata(&paths.output)
        .map(|m| m.len() / 1024)
        .unwrap_or(0);
    eprintln!("      完成: {} 条，{size_kb} KB", kept.len());

    if cfg.extra.enabled {
        let input = paths
            .extra_input
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("extra.input_path 未配置"))?;
        if !input.is_file() {
            anyhow::bail!("五笔扩展词库不存在: {}", input.display());
        }
        process_extra(cfg, paths, &unigram, log_median, &char_codes, version)?;
        let extra_path = extra::extra_output_path(&paths.output, &cfg.output_name, "extra");
        let existing_extra = parse::parse_jidian(&extra_path)?;
        let source = cache
            .join("rime-frost")
            .join("cn_dicts")
            .join("base.dict.yaml");
        let supplement =
            frost::generate(&source, &kept, &existing_extra, cfg.fallback.priority_10)?;
        let supplement_path = paths.output.with_file_name("wubi86_frost.dict.yaml");
        writer::write_frost_dict(&supplement_path, &supplement, version)?;
        eprintln!(
            "      白霜补充词库: {} 条 → {}",
            supplement.len(),
            supplement_path.display()
        );
    }

    // 原样透传的词库：只清洗头部，条目顺序即数据，不得重排
    for (src, dst) in &paths.passthrough {
        if !src.exists() {
            eprintln!("      [警告] 透传源不存在，跳过: {}", src.display());
            continue;
        }
        match writer::passthrough_stripping_sort(src, dst) {
            Ok(stripped) => {
                let note = if stripped {
                    "，已清除 sort: 键"
                } else {
                    ""
                };
                eprintln!("      [透传] {} → {}{note}", src.display(), dst.display());
            }
            Err(e) => eprintln!("      [警告] 透传失败 {}: {e}", src.display()),
        }
    }

    if cfg.shortcodes.enabled {
        let conflicts = shortcode::analyze_conflicts(&kept);
        eprintln!("      简码避让冲突: 共 {} 处（降权后）", conflicts.len());
        if let Some(p) = &paths.conflict_report {
            match writer::write_conflict_report(p, &conflicts) {
                Ok(()) => eprintln!("      冲突报告: {}", p.display()),
                Err(e) => eprintln!("      [警告] 冲突报告写出失败: {e}"),
            }
        }
        if let Some(p) = &paths.demotion_report {
            let source = if pre_demotion.is_empty() {
                &conflicts
            } else {
                &pre_demotion
            };
            match writer::write_demotion_report(p, source) {
                Ok(()) => eprintln!("      降权报告: {}（降权前快照）", p.display()),
                Err(e) => eprintln!("      [警告] 降权报告写出失败: {e}"),
            }
        }
    }

    // 顺序变化报告：唯一回答「我们把上游的候选安排改成了什么样」的产物。
    // 摘要无条件打印——不传 --report 时也该看到规模，那是调参时最要紧的一个数。
    let (order_changes, order_summary) = order_report::diff(
        &upstream_order_snapshot,
        &kept,
        &unigram,
        cfg,
        &demoted_entries,
        &boosted_entries,
    );
    eprintln!(
        "\n      候选顺序变化: 可比码 {} / 顺序变化 {} / 首选变化 {}（其中违逆上游明确优先级 {}）",
        order_summary.comparable,
        order_summary.order_changed,
        order_summary.top_changed,
        order_summary.top_changed_against_upstream
    );
    if let Some(p) = &paths.order_report {
        match writer::write_order_report(p, &order_changes, &order_summary) {
            Ok(()) => eprintln!("      顺序变化报告: {}", p.display()),
            Err(e) => eprintln!("      [警告] 顺序变化报告写出失败: {e}"),
        }
    }

    if !dropped.is_empty()
        && let Some(p) = &paths.dropped
    {
        match writer::write_dropped(p, &dropped) {
            Ok(()) => eprintln!("      过滤条目已写出: {}", p.display()),
            Err(e) => eprintln!("      [警告] 过滤条目写出失败: {e}"),
        }
    }

    eprintln!("\n✓ 完成");
    Ok(())
}

/// 扩展词库：按字符类型拆成 4 个文件，各自赋权后写出。
/// 保护带内相邻档位的间距：留出空隙，便于日后人工在两档之间插值微调。
const PROTECTED_STEP: i64 = 10;

/// 给受保护码赋权：**极点原始优先级为主键，词频仅作并列裁决**。
///
/// ## 为什么词频不能当主键
///
/// 上游对这些码的排序表达的是「这个码位约定谁排第一」，而词频回答的是「这个词多常用」，
/// 两者不可通约。`cccc` 的「又」上游给 40（首选），补权按词频打成 3010 后又被
/// `apply_demotion` 降成 1318，首选变成「双双」——码位约定就此丢失。
///
/// ## 为什么词频又不能完全不用
///
/// 上游的「无权重列」与「显式最低档 10」在解析后**同形**（`parse.rs` 对空列返回 10）。
/// 于是 `qqqq` 的「狗狗」(无权重列) 与「金」(显式 10) 并列，仅靠文件序会让「狗狗」占了
/// 首选——而 `qqqq` 的键名汉字是「金」。上游自己按 `by_weight` 排，无权重列在 librime 语义下
/// 恒沉底，故「金 在前」才是上游原意。词频在这里正是区分两者的现成信息。
///
/// 判据顺序：原始优先级降 → unigram 词频降 → 原文件序升（三者皆同则保持上游行序）。
fn apply_protected_codes(entries: &mut [Entry], unigram: &weight::Unigram, cfg: &Config) -> usize {
    if !cfg.protected_codes.enabled || cfg.protected_codes.codes.is_empty() {
        return 0;
    }
    // 按码分组：保护是按**码**施加的，同码条目必须一起定序，否则组内会混入
    // 未受保护的补权条目并凭高权重反超（保护单条等于没保护）。
    let mut groups: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (i, e) in entries.iter().enumerate() {
        if cfg.is_protected_code(&e.code) {
            groups.entry(e.code.clone()).or_default().push(i);
        }
    }
    let mut total = 0usize;
    for idxs in groups.values() {
        let mut ranked: Vec<(usize, i64, i64, usize)> = idxs
            .iter()
            .map(|&i| {
                let e = &entries[i];
                let freq = unigram.get(&e.text).copied().unwrap_or(0);
                (i, e.weight, freq, e.orig_pos)
            })
            .collect();
        // 主键：上游优先级降；次键：词频降（拆并列）；末键：上游行序升（稳定兜底）。
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.3.cmp(&b.3)));
        let n = ranked.len() as i64;
        for (rank, (i, ..)) in ranked.iter().enumerate() {
            entries[*i].weight =
                cfg.protected_codes.base_weight + (n - rank as i64) * PROTECTED_STEP;
            total += 1;
        }
    }
    total
}

fn process_extra(
    cfg: &Config,
    paths: &config::Paths,
    unigram: &weight::Unigram,
    log_median: f64,
    char_codes: &reverse::CharCodes,
    version: &str,
) -> anyhow::Result<()> {
    let Some(input) = &paths.extra_input else {
        anyhow::bail!("extra.input_path 未配置");
    };
    if !input.exists() {
        eprintln!("\n[extra] 跳过：输入文件不存在 ({})", input.display());
        return Ok(());
    }

    eprintln!("\n[extra] 处理扩展词库: {}", input.display());
    let mut log = |s: String| eprintln!("{s}");
    let entries = parse::parse_extra_dict(input, char_codes, &mut log)?;
    eprintln!("      读取 {} 条原始条目", entries.len());

    let mut buckets: Vec<(extra::Category, Vec<Entry>)> = extra::Category::ALL
        .iter()
        .map(|c| (*c, Vec::new()))
        .collect();
    for e in entries {
        let cat = extra::classify(&e.text);
        let slot = buckets
            .iter_mut()
            .find(|(c, _)| *c == cat)
            .expect("四类已穷举");
        slot.1.push(e);
    }

    let (cjk_hit, cjk_total) = extra::assign_weights(&mut buckets, unigram, log_median, cfg);

    // 自定义 emoji 置于 emoji 桶最前：它们是手工维护的常用表情快捷入口
    match extra::load_custom_emoji(paths.custom_emoji.as_deref()) {
        Ok(custom) if !custom.is_empty() => {
            let n = custom.len();
            let slot = buckets
                .iter_mut()
                .find(|(c, _)| *c == extra::Category::Emoji)
                .unwrap();
            let mut merged = custom;
            merged.append(&mut slot.1);
            slot.1 = merged;
            eprintln!("      [custom_emoji] 注入 {n} 条 emoj 编码条目");
        }
        Ok(_) => {}
        Err(e) => eprintln!("      [custom_emoji] 加载失败，跳过: {e}"),
    }

    // CLDR 命名表：中文名反查五笔码，与上面的固定 `emoj` 码是两条独立通路。
    //
    // 与 custom_emoji 一样**直接注入 emoji 桶、不经 classify**：`has_emoji` 的区间表
    // 漏判 ⭐(U+2B50)、国旗(U+1F1E6..)、keycap(ASCII+U+20E3) 等 76 个字形，若改成走
    // classify，这些条目会落进 symbols/english 桶而从 emoji 库里消失。
    match extra::load_named_emoji(paths.custom_emoji_named.as_deref(), char_codes, &mut log) {
        Ok(named) if !named.is_empty() => {
            let n = named.len();
            let slot = buckets
                .iter_mut()
                .find(|(c, _)| *c == extra::Category::Emoji)
                .expect("emoji 桶必然存在");
            slot.1.extend(named);
            eprintln!("      [emoji_named] 注入 {n} 条五笔码 emoji 条目");
        }
        Ok(_) => {}
        Err(e) => eprintln!("      [emoji_named] 加载失败，跳过: {e}"),
    }

    // 去重放在所有注入之后：既清掉上游自带的重复，也合并 named 与上游的重合项
    if let Some((_, list)) = buckets
        .iter_mut()
        .find(|(c, _)| *c == extra::Category::Emoji)
    {
        let removed = extra::dedup_emoji_entries(list);
        if removed > 0 {
            eprintln!("      [emoji] 去重移除 {removed} 条（同码同 emoji，保留权重最高者）");
        }
    }

    for (cat, list) in buckets.iter_mut() {
        let name = format!("{}_{}", cfg.output_name, cat.suffix());
        let path = extra::extra_output_path(&paths.output, &cfg.output_name, cat.suffix());
        writer::write_extra_dict(&path, list, &name, *cat, version)?;
        eprintln!(
            "      [{}] {} 条 → {}",
            cat.suffix(),
            list.len(),
            path.display()
        );
    }

    if log_median > 0.0 && cjk_total > 0 {
        eprintln!(
            "      CJK unigram 命中率: {cjk_hit}/{cjk_total} ({:.1}%)",
            100.0 * cjk_hit as f64 / cjk_total as f64
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "main_protected_codes_tests.rs"]
mod protected_codes_tests;
