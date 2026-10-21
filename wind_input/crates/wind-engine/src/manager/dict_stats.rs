//! 系统词库统计（`dict.systemStats` 的引擎侧）：选库、逐库加载、字频与字集、结果缓存。
//!
//! 计算本体是 [`wind_dict::stats`] 的纯函数；这里只负责把「这个方案的系统词库」交给它：
//!
//! - **选库与加载照搬索引构建方**：启用集 = [`EngineManager::enabled_dict_specs`]，逐库加载 =
//!   [`EngineManager::load_dict_individually`]（反查索引走的同一条路），主库 / 扩展库的划分
//!   = [`EngineManager::split_codetable_dicts`] 的口径。另写一份就会出现「统计里有、候选里
//!   没有」的错位。各库经 reader_pool 共享 mmap，活跃方案的库几乎零成本。
//! - **停用的库只列不计**：为了统计去加载一个关着的库，会在缓存目录建出 wdat——那正是 t107
//!   刻意不再做的事。
//! - **字频**只用随包白霜拼音字表（`pinyin/cn_dicts/8105` 与 `41448` 的单字权重之和）：码表
//!   自己的权重量纲不一、常常全 0，不能当字频。字表缺失时加权指标整节不出。
//! - **缓存**：键 = 各库摘要（文件 + 内容指纹 + wdat 大小/mtime）+ 排序与换算参数 + 字频 /
//!   字集文件摘要 + 算法版本。词库一变键就变，不必接失效通知；任一库是内存形态（没有稳定
//!   产物）就不缓存、每次重算。只存结果，去重集合随 builder 一起释放。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use tracing::info;
use wind_config::Config;
use wind_config::schema::DictSpec;
use wind_dict::cached::{CachedDict, for_each_code_group};
use wind_dict::stats::{
    Charset, CharsetStats, CodeLenDist, DupStats, LayerCounts, PinyinExtra, RankOrder,
    STATS_ALGO_VERSION, ShortCodes, StatsBuilder, StatsKind, StatsLayer, StatsOptions, TotalCounts,
};

use super::{EngineManager, weight_norm_of};

/// 字频源：随包白霜拼音字表（相对 `schemas/`）。
const FREQ_SOURCES: [&str; 2] = [
    "pinyin/cn_dicts/8105.dict.yaml",
    "pinyin/cn_dicts/41448.dict.yaml",
];

/// 通规字表（分级见 [`wind_dict::stats::parse_common_han_levels`]）。
const COMMON_HAN: &str = "charsets/common_han.yaml";

/// 统计里的一行词库。停用 / 加载失败的库 `counts` 为 `None`。
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DictStatsLayer {
    pub id: String,
    pub label: String,
    /// `"main"` 主库 / `"extra"` 扩展与细胞库 / `"userLib"` 用户分库。
    pub kind: &'static str,
    pub enabled: bool,
    /// 启用了却没加载出来（文件缺失、解析失败）。
    pub failed: bool,
    #[serde(flatten)]
    pub counts: Option<LayerCounts>,
}

/// 一个方案的系统词库统计结果（`dict.systemStats` 回包的主体）。
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SystemDictStats {
    pub schema_id: String,
    pub schema_name: String,
    /// `"codetable"` / `"pinyin"` / `"english"`。
    pub engine_type: String,
    pub layers: Vec<DictStatsLayer>,
    pub total: TotalCounts,
    pub code_len: Option<CodeLenDist>,
    pub short_codes: Option<ShortCodes>,
    pub dup: DupStats,
    pub charsets: Vec<CharsetStats>,
    pub pinyin: Option<PinyinExtra>,
    /// 字频来源说明；无字频时 `None`（加权指标不出）。
    pub freq_source: Option<String>,
    /// 算出时刻（Unix 毫秒）与耗时。
    pub computed_at_ms: u64,
    pub elapsed_ms: u64,
}

/// 文件的「路径|大小:mtime」摘要；不存在记 absent。
fn file_stamp(p: &Path) -> String {
    let own = match std::fs::metadata(p) {
        Ok(m) => format!(
            "{}:{}",
            m.len(),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos())
        ),
        Err(_) => "absent".to_string(),
    };
    format!("{}|{own}", p.display())
}

/// 一个已加载词库的摘要（同 `SystemDictLayer::entries_digest` 的文件部分）；内存形态 `None`。
fn dict_digest(d: &CachedDict) -> Option<String> {
    let p = d.source_file()?;
    Some(format!(
        "{}|{}",
        file_stamp(p),
        wind_dict::cache_fp::cache_digest(p)
    ))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// 词库行的显示名：方案写的 label → 文件名（去 `.dict.yaml`）→ id。
fn layer_label(d: &DictSpec) -> String {
    if !d.label.is_empty() {
        return d.label.clone();
    }
    let file = d.path.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = file.strip_suffix(".dict.yaml").unwrap_or(file);
    if stem.is_empty() {
        d.id.clone()
    } else {
        stem.to_string()
    }
}

impl EngineManager {
    /// 统计 `schema_id` 的系统词库（启用的各库分行 + 合计 + 码表结构 / 重码 / 字集）。
    ///
    /// 返回 `(结果, 是否命中缓存)`。`refresh` 绕过缓存重算。混输方案没有自己的系统词库，
    /// 调用方按成员方案分别调（见 `wind_coordinator::dict_stats_api`）。
    ///
    /// ⚠️ O(全表)：拼音大词库在百毫秒级，只在设置页手动打开统计时调，**绝不能进按键链路**。
    pub fn dict_stats(
        &self,
        schema_id: &str,
        top_n: usize,
        refresh: bool,
    ) -> anyhow::Result<(Arc<SystemDictStats>, bool)> {
        // 同一方案 single-flight：设置页连点「重新统计」或两处入口同时打开时，后到的等前一个
        // 算完再查缓存，不并发扫两遍大词库（拼音一遍几十 MB 峰值）。不同方案互不阻塞。
        let gate = self
            .dict_stats_gates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(schema_id.to_string())
            .or_default()
            .clone();
        let _flight = gate.lock().unwrap_or_else(|e| e.into_inner());
        let data_dir = self
            .data_dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("没有数据目录"))?;
        let schema = self
            .schema_merged(schema_id)
            .ok_or_else(|| anyhow::anyhow!("方案不存在: {schema_id}"))?;
        let engine_type = schema.engine.engine_type.to_lowercase();
        let (kind, rank) = match engine_type.as_str() {
            "codetable" => (
                StatsKind::Codetable,
                match crate::codetable::BaseSort::parse(&schema.engine.codetable.base_sort) {
                    crate::codetable::BaseSort::Natural => RankOrder::Natural,
                    crate::codetable::BaseSort::Weight => RankOrder::Weight,
                },
            ),
            "pinyin" => (StatsKind::Pinyin, RankOrder::Weight),
            "english" => (StatsKind::English, RankOrder::Weight),
            "mixed" => anyhow::bail!("混输方案没有自己的系统词库，请按成员方案统计"),
            other => anyhow::bail!("方案 {schema_id} 的引擎类型 {other} 不支持统计"),
        };
        let max_code_length = if kind == StatsKind::Codetable {
            schema.engine.codetable.max_code_length
        } else {
            0
        };
        let schemas_dir = data_dir.join("schemas");
        let t0 = std::time::Instant::now();

        // 选库：行 = 方案登记的全部可用库（含停用的，只列不计）；加载 = 启用集。
        let enabled: Vec<&DictSpec> = Self::enabled_dict_specs(&schema);
        let main_path = Self::split_codetable_dicts(&schema).map(|(m, _)| m.path.clone());
        let mut layers: Vec<DictStatsLayer> = Vec::new();
        let mut loaded: Vec<(usize, CachedDict, StatsLayer)> = Vec::new();
        let norm = weight_norm_of(&schema);
        for d in schema.dictionaries.iter().filter(|d| !d.path.is_empty()) {
            let kind = if d.id.starts_with(wind_store::user_dicts::ID_PREFIX) {
                "userLib"
            } else if main_path.as_deref() == Some(d.path.as_str()) {
                "main"
            } else {
                "extra"
            };
            let on = enabled.iter().any(|e| std::ptr::eq(*e, d));
            let dict = if on {
                Self::load_dict_individually(d, &schemas_dir)
            } else {
                None
            };
            let row = layers.len();
            layers.push(DictStatsLayer {
                id: d.id.clone(),
                label: layer_label(d),
                kind,
                enabled: on,
                failed: on && dict.is_none(),
                counts: None,
            });
            if let Some(dict) = dict {
                loaded.push((
                    row,
                    dict,
                    StatsLayer {
                        base_order: d.base_order,
                        default_weight: d.default_weight,
                        weight_norm: norm,
                    },
                ));
            }
        }

        // 字频与字集（英文方案两者都不用）。
        let freq_paths: Vec<std::path::PathBuf> = if kind == StatsKind::English {
            Vec::new()
        } else {
            FREQ_SOURCES
                .iter()
                .map(|rel| Self::resolve_dict_file(rel, &schemas_dir))
                .filter(|p| p.is_file())
                .collect()
        };
        let charset_path = (kind != StatsKind::English)
            .then(|| Config::resolve_data_file(Some(data_dir), COMMON_HAN))
            .flatten();

        let key = {
            let mut k = format!(
                "v{STATS_ALGO_VERSION}|{engine_type}|{rank:?}|max={max_code_length}|top={top_n}|norm={norm:?}"
            );
            for (row, dict, sl) in &loaded {
                let Some(dg) = dict_digest(dict) else {
                    k.clear();
                    break;
                };
                let l = &layers[*row];
                k.push_str(&format!(
                    "|{}:{}:{}:{:?}:{dg}",
                    l.id, l.kind, sl.base_order, sl.default_weight
                ));
            }
            if !k.is_empty() {
                for l in &layers {
                    k.push_str(&format!("|row={}:{}", l.id, l.enabled));
                }
                for p in &freq_paths {
                    k.push_str(&format!("|freq={}", file_stamp(p)));
                }
                if let Some(p) = &charset_path {
                    k.push_str(&format!("|cs={}", file_stamp(p)));
                }
            }
            k
        };
        if !refresh && !key.is_empty() {
            let cache = self
                .dict_stats_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some((k, v)) = cache.get(schema_id)
                && *k == key
            {
                return Ok((v.clone(), true));
            }
        }

        let mut freq: HashMap<char, u64> = HashMap::new();
        for p in &freq_paths {
            if let Ok(text) = std::fs::read_to_string(p) {
                wind_dict::stats::add_char_freq(&text, &mut freq);
            }
        }
        let freq_source = (!freq.is_empty()).then(|| {
            let names: Vec<&str> = freq_paths
                .iter()
                .filter_map(|p| p.file_name()?.to_str()?.strip_suffix(".dict.yaml"))
                .collect();
            format!("白霜拼音字表（{}）", names.join(" + "))
        });
        let charsets = charset_path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| common_han_charsets(&t))
            .unwrap_or_default();

        let dicts: Vec<&CachedDict> = loaded.iter().map(|(_, d, _)| d).collect();
        let mut builder = StatsBuilder::new(StatsOptions {
            kind,
            rank,
            max_code_length,
            top_n,
            layers: loaded.iter().map(|(_, _, l)| *l).collect(),
            main_layer: loaded
                .iter()
                .position(|(row, _, _)| layers[*row].kind == "main"),
            freq: freq_source.as_ref().map(|_| &freq),
            charsets: &charsets,
        });
        builder.reserve(dicts.iter().map(|d| d.len()).sum());
        for_each_code_group(&dicts, &mut |code, es| builder.push_group(code, es));
        let stats = builder.finish();
        for ((row, _, _), counts) in loaded.iter().zip(stats.layers) {
            layers[*row].counts = Some(counts);
        }
        let elapsed = t0.elapsed();
        info!(
            "词库统计 {schema_id}: {} 条 / {} 库，耗时 {elapsed:?}{}",
            stats.total.entries,
            loaded.len(),
            if key.is_empty() {
                "（不缓存）"
            } else {
                ""
            }
        );
        let out = Arc::new(SystemDictStats {
            schema_id: schema_id.to_string(),
            schema_name: schema.schema.name.clone(),
            engine_type,
            layers,
            total: stats.total,
            code_len: stats.code_len,
            short_codes: stats.short_codes,
            dup: stats.dup,
            charsets: stats.charsets,
            pinyin: stats.pinyin,
            freq_source,
            computed_at_ms: now_ms(),
            elapsed_ms: elapsed.as_millis() as u64,
        });
        if !key.is_empty() {
            self.dict_stats_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(schema_id.to_string(), (key, out.clone()));
        }
        Ok((out, false))
    }
}

/// 通规三级 → 三个累进字集（一级 / 一二级 / 全表）。分节读不出三级时不出字集统计。
fn common_han_charsets(text: &str) -> Vec<Charset> {
    let levels = wind_dict::stats::parse_common_han_levels(text);
    if levels.len() != 3 {
        return Vec::new();
    }
    let mut acc: Vec<String> = Vec::new();
    [
        ("common_han_1", "通规一级"),
        ("common_han_12", "通规一二级"),
        ("common_han_123", "通规全表"),
    ]
    .into_iter()
    .zip(levels)
    .map(|((key, label), lv)| {
        acc.extend(lv);
        Charset {
            key: key.to_string(),
            label: label.to_string(),
            chars: acc.clone(),
        }
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    /// 夹具数据根：码表方案（主库 + 启用扩展 + 停用扩展）、拼音方案、字频字表、通规字表。
    fn fixture(tag: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("wind_dict_stats_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let s = base.join("schemas");
        write(
            &s.join("ct.schema.toml"),
            "[schema]\nid = \"ct\"\nname = \"测码\"\n[engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 4\n\
             [[dictionaries]]\nid = \"ct_main\"\nlabel = \"主表\"\npath = \"ct/main.dict.yaml\"\ndefault = true\n\
             [[dictionaries]]\nid = \"ct_ext\"\npath = \"ct/ext.dict.yaml\"\ndefault_enabled = true\nbase_order = 1\n\
             [[dictionaries]]\nid = \"ct_off\"\nlabel = \"关着的\"\npath = \"ct/off.dict.yaml\"\ndefault_enabled = false\n",
        );
        let head = "---\nname: x\nversion: \"1\"\n...\n";
        write(
            &s.join("ct/main.dict.yaml"),
            &format!(
                "{head}a\t工\t100\naaaa\t工\t50\naaaa\t式\t80\naaab\t工作\t70\nbbbb\t子\t40\nbbbb\t孑\t30\n"
            ),
        );
        write(
            &s.join("ct/ext.dict.yaml"),
            &format!("{head}aaaa\t戒\t60\nff\t😀\t1\n"),
        );
        write(&s.join("ct/off.dict.yaml"), &format!("{head}zz\t乙\t1\n"));
        write(
            &s.join("pinyin/cn_dicts/8105.dict.yaml"),
            &format!("{head}工\tgong\t100\n式\tshi\t50\n子\tzi\t30\n戒\tjie\t10\n孑\tjie\t5\n"),
        );
        write(
            &base.join("charsets/common_han.yaml"),
            "key: common_han\n...\n# 一级\n工\n式\n子\n# 二级\n戒\n孑\n丢\n# 三级\n乙\n",
        );
        base
    }

    fn mgr(base: &Path) -> EngineManager {
        let mut cfg = Config::default();
        cfg.schema.available = vec!["ct".into()];
        cfg.schema.active = "ct".into();
        EngineManager::new(&cfg, Some(base))
    }

    #[test]
    fn codetable_fixture_counts_layers_and_weighted_metrics() {
        let base = fixture("ct");
        let m = mgr(&base);
        let (s, cached) = m.dict_stats("ct", 10, false).unwrap();
        assert!(!cached);
        assert_eq!(s.engine_type, "codetable");
        assert_eq!(s.schema_name, "测码");
        let rows: Vec<(&str, &str, &str, bool, Option<u64>)> = s
            .layers
            .iter()
            .map(|l| {
                (
                    l.id.as_str(),
                    l.label.as_str(),
                    l.kind,
                    l.enabled,
                    l.counts.as_ref().map(|c| c.entries),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("ct_main", "主表", "main", true, Some(6)),
                ("ct_ext", "ext", "extra", true, Some(2)),
                ("ct_off", "关着的", "extra", false, None),
            ]
        );
        assert_eq!(s.total.entries, 8);
        assert_eq!(s.total.other, 1);
        // aaaa：式 80 > 戒 60 > 工 50；bbbb：子 40 > 孑 30
        assert_eq!(
            (s.dup.groups, s.dup.max, s.dup.max_code.as_str()),
            (2, 3, "aaaa")
        );
        assert_eq!(s.dup.top[0].texts, ["式", "戒", "工"]);
        assert_eq!(s.freq_source.as_deref(), Some("白霜拼音字表（8105）"));

        let one = &s.charsets[0]; // 一级：工 式 子
        assert_eq!(
            (one.key.as_str(), one.size, one.missing),
            ("common_han_1", 3, 0)
        );
        let two = &s.charsets[1]; // 一二级：+ 戒 孑 丢
        assert_eq!((two.size, two.missing, two.static_dup), (6, 1, Some(3)));
        // 非首选：戒 10（aaaa 第 2）+ 孑 5（bbbb 第 2）；工 走一简 a 首选
        assert_eq!(two.dyn_rate, Some(15.0 / 195.0));
        // 工 1 码、式/子/戒/孑 4 码
        assert_eq!(two.avg_code_len, Some((100.0 + 4.0 * 95.0) / 195.0));
        assert_eq!(s.charsets[2].missing_chars, ["丢", "乙"]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cache_hits_until_a_dict_changes_and_refresh_bypasses() {
        let base = fixture("cache");
        let m = mgr(&base);
        let (a, _) = m.dict_stats("ct", 10, false).unwrap();
        let (b, hit) = m.dict_stats("ct", 10, false).unwrap();
        assert!(hit && Arc::ptr_eq(&a, &b));
        let (_, hit) = m.dict_stats("ct", 10, true).unwrap();
        assert!(!hit, "refresh 绕过缓存");
        let (_, hit) = m.dict_stats("ct", 5, false).unwrap();
        assert!(!hit, "Top 条数不同不能复用");

        // 改扩展库：重建 wdat，摘要随之变化
        write(
            &base.join("schemas/ct/ext.dict.yaml"),
            "---\nname: x\nversion: \"2\"\n...\naaaa\t戒\t60\nff\t😀\t1\nzz\t丁\t1\n",
        );
        let (c, hit) = m.dict_stats("ct", 5, false).unwrap();
        assert!(!hit, "词库内容变了必须重算");
        assert_eq!(c.total.entries, 9);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_freq_source_hides_weighted_metrics_and_mixed_is_rejected() {
        let base = fixture("nofreq");
        std::fs::remove_file(base.join("schemas/pinyin/cn_dicts/8105.dict.yaml")).unwrap();
        write(
            &base.join("schemas/mx.schema.toml"),
            "[schema]\nid = \"mx\"\n[engine]\ntype = \"mixed\"\n",
        );
        let m = mgr(&base);
        let (s, _) = m.dict_stats("ct", 10, false).unwrap();
        assert_eq!(s.freq_source, None);
        assert!(
            s.charsets
                .iter()
                .all(|c| c.dyn_rate.is_none() && c.avg_code_len.is_none())
        );
        assert_eq!(s.charsets[1].static_dup, Some(3), "静重不依赖字频");
        assert!(m.dict_stats("mx", 10, false).is_err());
        assert!(m.dict_stats("nope", 10, false).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 真实出厂数据冒烟，数值与草案原型（Python 直接解析 yaml）对照。要 build_dev/data，
    /// 故默认 ignore（缺数据时静默通过就是假绿）：`cargo test -p wind-engine -- --ignored real_data`。
    #[test]
    #[ignore = "需 build_dev/data"]
    fn real_data_smoke_wubi86_and_pinyin() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        assert!(
            data.join("schemas/wubi86.schema.toml").exists(),
            "缺 build_dev/data：整份拷进来再跑"
        );
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
        cfg.schema.active = "wubi86".into();
        let m = EngineManager::new(&cfg, Some(&data));

        let (w, _) = m.dict_stats("wubi86", 50, true).unwrap();
        let main = w.layers.iter().find(|l| l.kind == "main").unwrap();
        eprintln!("wubi86 {}ms: {:?}", w.elapsed_ms, main);
        eprintln!("wubi86 total {:?}", w.total);
        eprintln!("wubi86 codeLen {:?} short {:?}", w.code_len, w.short_codes);
        eprintln!(
            "wubi86 dup groups={} entries={} max={} ({}) charFull={:?}",
            w.dup.groups, w.dup.entries, w.dup.max, w.dup.max_code, w.dup.char_full
        );
        for c in &w.charsets {
            eprintln!(
                "wubi86 {} size={} missing={} static={:?} groups={:?} dyn={:?} avg={:?}",
                c.label, c.size, c.missing, c.static_dup, c.dup_groups, c.dyn_rate, c.avg_code_len
            );
        }
        // 主库条目 = yaml 正文行数（各库分行不去重）。
        let yaml =
            std::fs::read_to_string(data.join("schemas/wubi86/wubi86_jidian.dict.yaml")).unwrap();
        let body = &yaml[wind_dict::rime_body_layout(&yaml).unwrap().0..];
        let rows = body
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#') && l.contains('\t'))
            .count() as u64;
        assert_eq!(main.counts.as_ref().unwrap().entries, rows);
        let sizes: Vec<u64> = w.charsets.iter().map(|c| c.size).collect();
        assert_eq!(sizes, [3500, 6500, 8105], "通规三级累进");
        assert_eq!(w.charsets[0].missing, 0, "五笔86 收全一级字");
        assert!(w.freq_source.is_some());
        let rate = w.charsets[2].dyn_rate.unwrap();
        assert!(rate > 0.0 && rate < 0.01, "{rate}");
        // 重码不计纯 emoji：最大组不再是 emoj，Top 成员都含汉字（数值以出厂词库为准）。
        assert_ne!(w.dup.max_code, "emoj");
        assert!(
            w.dup.top[0]
                .texts
                .iter()
                .all(|t| t.chars().any(wind_dict::stats::is_han)),
            "{:?}",
            w.dup.top[0]
        );
        assert_eq!((w.dup.max, w.dup.max_code.as_str()), (41, "cccc"));
        let md = w.dup.main.as_ref().unwrap();
        eprintln!("wubi86 main dup {md:?}");
        assert_eq!(md.max, 13, "主库最大重码");
        let cf = w.dup.char_full.as_ref().unwrap();
        assert_eq!(cf.top[0].size, cf.max, "单字全码 Top 与那一行同口径");

        let (p, _) = m.dict_stats("pinyin", 10, true).unwrap();
        eprintln!("pinyin {}ms total {:?}", p.elapsed_ms, p.total);
        eprintln!(
            "pinyin dup groups={} max={} ({}) extra={:?}",
            p.dup.groups, p.dup.max, p.dup.max_code, p.pinyin
        );
        for c in &p.charsets {
            eprintln!("pinyin {} missing={}", c.label, c.missing);
        }
        assert!(p.total.entries > 600_000);
        let syl = p.pinyin.as_ref().unwrap().syllables;
        assert!((400..=450).contains(&syl), "{syl}");
    }
}
