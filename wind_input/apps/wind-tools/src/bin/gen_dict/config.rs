//! gen_dict 配置：TOML 解析与路径解析。
//!
//! 数值默认值与 WindInput-Go 的 `tools/dictgen/config.go: defaultConfig()` 逐项对应。
//! 改动这些默认值会直接改变发行词库的候选顺序，动前先读 gen_dict.toml 的头部说明。

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    // 输入（相对 --cache）
    pub jidian_path: String,
    pub unigram_path: String,

    // 输出（相对 --out）
    pub output_path: String,
    pub output_name: String,

    // 人工维护数据（相对配置文件所在目录）
    pub custom_words_path: String,
    pub custom_emoji_path: String,
    /// emoji 中文命名表（由 gen_emoji_names 从 CLDR 生成）：中文名反查五笔码，
    /// 与 `custom_emoji_path` 的固定 `emoj` 码是两条互不影响的通路。
    pub custom_emoji_named_path: String,
    pub boosts_path: String,

    // 分析报告（相对 --report）
    pub dropped_path: String,
    pub conflict_report_path: String,
    pub demotion_report_path: String,
    /// 候选顺序变化报告：上游原序 vs 产物最终序。
    ///
    /// 与另外三份报告的视角不同——它们只看产物内部，这份是唯一能回答
    /// 「我们把上游的候选安排改成了什么样」的产物。
    pub order_report_path: String,

    // 权重归一化
    pub target_median: i64,
    pub weight_max: i64,
    pub weight_min: i64,
    pub char_boost_factor: f64,
    pub fallback: FallbackWeights,

    // 普通词条权重上限，须低于最低简码权重
    pub regular_weight_max: i64,

    pub shortcodes: ShortcodeConfig,
    pub demotion: DemotionConfig,
    pub upstream_order: UpstreamOrderConfig,
    pub extra: ExtraConfig,
    pub filter: FilterConfig,
    pub protected_codes: ProtectedCodesConfig,

    #[serde(rename = "drop_rules")]
    pub drop_rules: Vec<DropRule>,

    /// 写入主词库头部的 `import_tables` 段；当前发行方案为空
    /// （emoji/extra/district 都作为独立 `[[dictionaries]]` 声明）。
    pub import_tables: Vec<String>,

    /// 上游原样词库（相对 `--cache`）：不重排，只复制到输出目录并清洗头部的 `sort:` 键。
    ///
    /// 用于 district 这类**条目顺序本身即数据**的词库（按行政区划层级排列），
    /// 按词频重排会破坏其语义。
    pub passthrough: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FallbackWeights {
    pub priority_30: i64,
    pub priority_20: i64,
    pub priority_10: i64,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShortcodeConfig {
    pub enabled: bool,
    pub level1_weight: i64,
    pub level2_base_weight: i64,
    pub level3_base_weight: i64,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DemotionConfig {
    pub enabled: bool,
    pub filter_threshold: i64,
    pub single_char_promote_wt: i64,
    pub word_promote_wt: i64,
    pub max_gap_ratio_single: f64,
    pub max_gap_ratio_word: f64,
}

/// 组内权重重排：把同码次序换回极点上游序，同时不改变该码在权重轴上的高度。
///
/// 上游 `weight` 列是**码内局部优先级**（值域 5~890、86% 是最低档 10），跨码不可比；
/// 词频提供的才是跨码可比的高度。两者各答一半，本功能把它们分开使用。
/// 详见 [`crate::upstream_order`] 的模块文档。
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpstreamOrderConfig {
    pub enabled: bool,
    /// 护栏：`当前首选词频 / 上游首选词频` 超过此倍数则**不**回归上游序。
    ///
    /// 上游序里混着大量低频词，纯回归会把它们顶回首选。阈值 26 由样本夹定（见
    /// `upstream_order` 的测试）；24×～30× 区间很不敏感，调参先看 3×～10× 那一段。
    ///
    /// 另有一道无条件护栏：上游首选不在 unigram 里（生僻词）一律不回归。
    pub max_freq_ratio: i64,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExtraConfig {
    pub enabled: bool,
    pub input_path: String,
    pub default_weight: i64,
    /// 扩展库 CJK 桶的权重上限：补权后线性压缩到 `[weight_min, 本值]`。
    ///
    /// 必须低于主库最低档 `fallback.priority_10`（由 `validate` 强制）：这是「扩展库整库排在
    /// 主库之后」的唯一手段，引擎侧 `base_order` 排在 `weight` 之后，只在等权时起作用
    /// （见 `docs/design/dict-weight-normalization.md`）。
    ///
    /// 0 = 不压缩（保留补权原值）。
    pub weight_max: i64,
}

/// 上游特有的编码约定保护：这些码的条目跳过词频补权，权重改由极点原始优先级映射，
/// 保留上游设计的候选顺序。词频补权按「这个词有多常用」赋权，而上游对某些码（如四叠码
/// `aaaa`=工、`dddd`=大）表达的是「这个码位约定谁排第一」，两者不可通约。
///
/// 作用域只到词库权重：运行时出简让全不设这层豁免（见
/// `docs/design/codetable-short-code-yields-full.md`），开启后 `dddd` 首选是「大厦」而非「大」。
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProtectedCodesConfig {
    pub enabled: bool,
    /// 受保护的完整编码（精确匹配，非前缀）。
    pub codes: Vec<String>,
    /// 保护带基准权重：最终权重 = 本值 + 条目的极点原始优先级。
    ///
    /// 须高于扩展带与生僻字保底档、且加上原始优先级后仍低于简码带（由 `validate` 强制）。
    /// 不要求高于 `regular_weight_max`，理由见 `validate`。
    pub base_weight: i64,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilterConfig {
    pub drop_z_code: bool,
    pub drop_dollar: bool,
    pub drop_emoji: bool,
    pub drop_pure_latin: bool,
    pub drop_pua: bool,
    pub require_cjk: bool,
    pub max_code_len: usize,
    pub max_text_len: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct DropRule {
    pub code_prefix: String,
    pub code: String,
    pub reason: String,
    pub except_codes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            jidian_path: String::new(),
            unigram_path: String::new(),
            output_path: String::new(),
            output_name: "wubi86_jidian".into(),
            custom_words_path: String::new(),
            custom_emoji_path: String::new(),
            custom_emoji_named_path: String::new(),
            boosts_path: String::new(),
            dropped_path: String::new(),
            conflict_report_path: String::new(),
            demotion_report_path: String::new(),
            order_report_path: String::new(),
            target_median: 1000,
            weight_max: 9999,
            weight_min: 1,
            char_boost_factor: 1.3,
            fallback: FallbackWeights::default(),
            regular_weight_max: 8999,
            shortcodes: ShortcodeConfig::default(),
            demotion: DemotionConfig::default(),
            upstream_order: UpstreamOrderConfig::default(),
            extra: ExtraConfig::default(),
            filter: FilterConfig::default(),
            protected_codes: ProtectedCodesConfig::default(),
            drop_rules: Vec::new(),
            import_tables: Vec::new(),
            passthrough: Vec::new(),
        }
    }
}

impl Default for FallbackWeights {
    fn default() -> Self {
        Self {
            priority_30: 180,
            priority_20: 150,
            priority_10: 120,
        }
    }
}

impl Default for ShortcodeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level1_weight: 9999,
            level2_base_weight: 9950,
            level3_base_weight: 9000,
        }
    }
}

impl Default for DemotionConfig {
    fn default() -> Self {
        Self {
            // 已退役，取代者是运行时的出简让全（`schema.codetable.short_code_yield_level`）。
            // 默认值必须与 gen_dict.toml 同为 false：缺 `[demotion]` 段的配置会静默取到这里，
            // 留 true 会与运行时让位叠加，字被压到词后面两遍。
            enabled: false,
            // 以下参数仅为记录当初的标定值，enabled = false 时不参与计算。
            filter_threshold: 200,
            single_char_promote_wt: 1000,
            word_promote_wt: 800,
            max_gap_ratio_single: 0.60,
            max_gap_ratio_word: 0.65,
        }
    }
}

impl Default for UpstreamOrderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_freq_ratio: 26,
        }
    }
}

impl Default for ExtraConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            input_path: String::new(),
            default_weight: 100,
            // 119 = 主库最低档 priority_10(120) - 1：扩展库整库恒排在主库之后。
            weight_max: 119,
        }
    }
}

impl Default for ProtectedCodesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // 五笔 86 的 25 个键名汉字码（`zzzz` 在上游不存在，故为 25 而非 26）。
            // 保护的是**整个码**而非单条：同码内所有条目一起按上游原序落进保护带，
            // 否则未受保护的同码条目会带着补权后的高权重反超。
            codes: [
                "aaaa", "bbbb", "cccc", "dddd", "eeee", "ffff", "gggg", "hhhh", "iiii", "jjjj",
                "kkkk", "llll", "mmmm", "nnnn", "oooo", "pppp", "qqqq", "rrrr", "ssss", "tttt",
                "uuuu", "vvvv", "wwww", "xxxx", "yyyy",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            base_weight: 8000,
        }
    }
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            drop_z_code: true,
            drop_dollar: true,
            drop_emoji: true,
            drop_pure_latin: true,
            drop_pua: false,
            require_cjk: false,
            max_code_len: 4,
            max_text_len: 16,
        }
    }
}

/// 配置里的相对路径按各自的基准目录解析后的结果。
pub struct Paths {
    pub jidian: PathBuf,
    pub unigram: PathBuf,
    pub extra_input: Option<PathBuf>,
    pub output: PathBuf,
    /// (源文件, 输出文件) 对
    pub passthrough: Vec<(PathBuf, PathBuf)>,
    pub custom_words: Option<PathBuf>,
    pub custom_emoji: Option<PathBuf>,
    pub custom_emoji_named: Option<PathBuf>,
    pub boosts: Option<PathBuf>,
    pub dropped: Option<PathBuf>,
    pub conflict_report: Option<PathBuf>,
    pub demotion_report: Option<PathBuf>,
    pub order_report: Option<PathBuf>,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取配置失败 {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("解析配置失败 {}: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 拦截会静默产出错误词库的配置组合。
    ///
    /// 尤其是 `regular_weight_max >= level3_base_weight`：普通词条会窜进简码权重区间，
    /// 结果是简码不再稳定占首选——这种错误在输出里看不出来，只有实际打字才发现。
    fn validate(&self) -> anyhow::Result<()> {
        if self.jidian_path.is_empty() {
            anyhow::bail!("jidian_path 不可为空");
        }
        if self.unigram_path.is_empty() {
            anyhow::bail!("unigram_path 不可为空");
        }
        if self.output_path.is_empty() || self.output_name.is_empty() {
            anyhow::bail!("output_path / output_name 不可为空");
        }
        if self.weight_min > self.weight_max {
            anyhow::bail!(
                "weight_min({}) 不可大于 weight_max({})",
                self.weight_min,
                self.weight_max
            );
        }
        if self.shortcodes.enabled && self.regular_weight_max >= self.shortcodes.level3_base_weight
        {
            anyhow::bail!(
                "regular_weight_max({}) 必须低于 level3_base_weight({})，否则普通词条会挤进简码权重区间、简码不再稳定占首选",
                self.regular_weight_max,
                self.shortcodes.level3_base_weight
            );
        }
        if self.target_median <= 0 {
            anyhow::bail!("target_median 必须为正数");
        }
        // 0 或负数会让护栏恒真（任何词频都「超过」），表现为整个功能静默失效——
        // 而失效的样子恰好和「没启用」一模一样，不拦下就要靠翻产物才发现。
        if self.upstream_order.enabled && self.upstream_order.max_freq_ratio <= 0 {
            anyhow::bail!(
                "upstream_order.max_freq_ratio({}) 必须为正数，否则护栏恒拦、组内重排静默失效",
                self.upstream_order.max_freq_ratio
            );
        }
        // 扩展库带必须整体低于主库最低档，否则「扩展库排在主库之后」不成立。
        // 引擎侧的 base_order 救不了——它排在 weight 之后，只是等权 tiebreaker。
        if self.extra.enabled
            && self.extra.weight_max > 0
            && self.extra.weight_max >= self.fallback.priority_10
        {
            anyhow::bail!(
                "extra.weight_max({}) 必须低于 fallback.priority_10({})，否则扩展库词条会反超主库，而引擎侧 base_order 排在 weight 之后压不住它",
                self.extra.weight_max,
                self.fallback.priority_10
            );
        }
        // 保护带的两侧约束。
        //
        // 不要求高于 `regular_weight_max`：保护按码施加，同码内不存在未保护的主库条目，
        // 跨码引擎不比权重；唯一的跨库同码竞争来自扩展库，已由 `extra.weight_max` 压到下方。
        if self.protected_codes.enabled && !self.protected_codes.codes.is_empty() {
            let base = self.protected_codes.base_weight;
            if self.extra.enabled && self.extra.weight_max > 0 && base <= self.extra.weight_max {
                anyhow::bail!(
                    "protected_codes.base_weight({base}) 必须高于 extra.weight_max({})，否则扩展库词条会反超受保护码，保护形同虚设",
                    self.extra.weight_max
                );
            }
            if base <= self.fallback.priority_10 {
                anyhow::bail!(
                    "protected_codes.base_weight({base}) 必须高于 fallback.priority_10({})，否则受保护码会低于主库生僻字保底档",
                    self.fallback.priority_10
                );
            }
            // 上界留出原始优先级的加数空间：上游最大优先级为 60（kkkk 的「串口」）。
            const MAX_ORIG_PRIORITY: i64 = 100;
            if self.shortcodes.enabled
                && base + MAX_ORIG_PRIORITY >= self.shortcodes.level3_base_weight
            {
                anyhow::bail!(
                    "protected_codes.base_weight({base}) + 原始优先级上限({MAX_ORIG_PRIORITY}) 必须低于 level3_base_weight({})，否则受保护码会挤进简码权重区间、夺走简码首选",
                    self.shortcodes.level3_base_weight
                );
            }
        }
        Ok(())
    }

    /// 按 CLI 给的目录把配置里的相对路径展开。
    ///
    /// 三个基准各不相同：输入相对 `--cache`、输出相对 `--out`、人工数据相对配置文件
    /// 自身所在目录（它们与配置同属版本控制，跟着配置走才不会因调用位置不同而失效）。
    pub fn resolve_paths(
        &self,
        config_dir: &Path,
        cache_dir: &Path,
        out_dir: &Path,
        report_dir: Option<&Path>,
    ) -> Paths {
        let opt = |s: &str, base: &Path| -> Option<PathBuf> {
            if s.is_empty() {
                None
            } else {
                Some(base.join(s))
            }
        };
        Paths {
            jidian: cache_dir.join(&self.jidian_path),
            unigram: cache_dir.join(&self.unigram_path),
            extra_input: if self.extra.enabled {
                opt(&self.extra.input_path, cache_dir)
            } else {
                None
            },
            output: out_dir.join(&self.output_path),
            passthrough: self
                .passthrough
                .iter()
                .map(|rel| {
                    let src = cache_dir.join(rel);
                    // 输出保持与源同名：这些文件被方案按文件名引用
                    let name = Path::new(rel).file_name().unwrap_or_default();
                    (src, out_dir.join(name))
                })
                .collect(),
            custom_words: opt(&self.custom_words_path, config_dir),
            custom_emoji: opt(&self.custom_emoji_path, config_dir),
            custom_emoji_named: opt(&self.custom_emoji_named_path, config_dir),
            boosts: opt(&self.boosts_path, config_dir),
            dropped: report_dir.and_then(|d| opt(&self.dropped_path, d)),
            conflict_report: report_dir.and_then(|d| opt(&self.conflict_report_path, d)),
            demotion_report: report_dir.and_then(|d| opt(&self.demotion_report_path, d)),
            order_report: report_dir.and_then(|d| opt(&self.order_report_path, d)),
        }
    }

    /// 该编码是否受「上游编码约定保护」——受保护则跳过词频补权与简码降权。
    ///
    /// 精确匹配整个码，不做前缀匹配：保护的是具体码位的候选顺序，
    /// 前缀匹配会把 `cccc` 的保护误扩到 `ccc`/`cc` 等简码上（那些另有简码带管辖）。
    pub fn is_protected_code(&self, code: &str) -> bool {
        self.protected_codes.enabled && self.protected_codes.codes.iter().any(|c| c == code)
    }

    /// 普通词条的权重上限：启用简码分层时压到 `regular_weight_max` 之下。
    pub fn regular_max(&self) -> i64 {
        if self.shortcodes.enabled
            && self.regular_weight_max > 0
            && self.regular_weight_max < self.weight_max
        {
            self.regular_weight_max
        } else {
            self.weight_max
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
