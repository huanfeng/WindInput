//! 引擎管理器
//!
//! 职责：
//! - 按需加载各方案的词典与引擎（Pinyin / CodeTable）
//! - 持有当前活跃方案，支持运行时切换 / 循环切换
//! - 将 `convert` 请求分发到当前引擎

use crate::codetable::CodeTableEngine;
use crate::encoder;
use crate::engine::{BoundaryResolution, ConvertOptions, ConvertResult, Engine, EngineType};
use crate::freq_rerank::ProtectPolicy;
use crate::pinyin::{Config as PinyinConfig, PinyinEngine};
pub use dict_stats::{DictStatsLayer, SystemDictStats};
pub use dict_view::{
    DictEntriesPage, DictEntryRow, DictHeader, DictTableInfo, ENTRIES_PAGE_MAX, EntryMatch,
    EntryQuery, EntrySort,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, warn};
use wind_candidate::CandidateSource;
use wind_config::Config;
use wind_config::schema::{DictSpec, Schema, merge_toml};
use wind_dict::cached::{CachedDict, ReverseIndex};

/// 拼音族共享数据归属命名空间：所有拼音引擎方案（全拼/双拼）的用户词/临时词/词频
/// 统一落此键空间（P2c）。区别于恰好同名的真实方案 id "pinyin"（如临时拼音默认目标）。
pub const PINYIN_DATA_SCHEMA: &str = "pinyin";

/// 内置英文方案 id（`data/schemas/english.schema.toml`）。
///
/// 作为 active 方案与被临时英文 overlay 引用时打的是同一份词库，用户数据也归同一个桶
/// （见 `Coordinator::effective_data_schema`）；混输 / 快捷输入融合英文候选时只借词库。
///
/// 进程里只建一份：所有入口都经 [`EngineManager::shared_english_engine`] 取同一个 `Arc`，
/// 别在 `build_engine` 里另建（词组分词索引随用户词库增长，重复一份内存很可观）。
pub const ENGLISH_SCHEMA: &str = "english";

/// 英文方案按屏蔽词多取候选的上限（见 `EngineManager::convert_unblocked`）：屏蔽集是用户一条条
/// 右键删出来的，同一前缀下几十条已是极端，再多就不值得为补满一页而放大查询。
const ENGLISH_BLOCK_EXTRA_CAP: usize = 64;

/// 英文词频名次表（相对 `schemas/`）：给英文词库里没写权重的词条按名次赋权，
/// 见 `wind_dict::freq_rank` 与 [`EngineManager::load_table_dict`]。
const ENGLISH_FREQ_RANKS: &str = "english/en_freq.txt";

/// 双拼方案未声明 `layout` 时的缺省布局。
///
/// 独立成常量而非各处写字面量：设置页要显示「当前选的是哪个」，那个判断与引擎实际
/// 加载哪份布局必须同源，否则会出现「设置页显示自然码、打起来是小鹤」。
pub const DEFAULT_SHUANGPIN_LAYOUT: &str = "xiaohe";

/// 码表词频应用策略（见 docs/redesign/frequency.md §3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreqStrategy {
    /// 一次到顶（MRU）：last_used 优先，最近选的置该档之首。
    Top,
    /// 逐次提升（默认）：count 优先，累积使用才爬升，抗误选。
    Step,
    /// 位次减半：候选每积累一次有效使用，目标位次除以 2，随半衰期回落
    /// （`docs/design/freq-rerank-model.md`）。
    ///
    /// `Top`/`Step` 是布尔 used-first（用过一次就整体跳到档内最前）；`Position` 让位次连续
    /// 表达强弱，适合前缀匹配为主的方案（英文），那里布尔闸门过于粗暴。
    Position,
}

/// 前缀补全参与词频位置提升的范围。判据是语义单元数
/// （[`wind_candidate::semantic_units`]：汉字逐字计、西文词整体计 1），不是字符数——
/// 按字符数，英文（几乎全是前缀匹配）会被「只提升单个」整体挡掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotePrefix {
    /// 前缀补全一律不参与提升。
    None,
    /// 只提升单个语义单元的候选（中文单字 / 西文单词）。默认。
    Single,
    /// 全部参与提升。
    All,
}

impl PromotePrefix {
    /// 解析配置字符串；未知值落回 [`Self::Single`]（默认档，最保守的可用行为）。
    pub fn parse(s: &str) -> Self {
        match s {
            "none" => Self::None,
            "all" => Self::All,
            _ => Self::Single,
        }
    }

    /// 该候选文本是否获准参与提升（调用方已确认它落在有效前缀层）。
    pub fn allows(&self, text: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Single => wind_candidate::semantic_units(text) <= 1,
        }
    }
}

/// 活跃方案的词频排序设置（apply_freq_rerank 用）。
/// 按方案解析后缓存，避免每键读盘（frequency.md §8）。字符类 registry 按 `Arc` 共享，克隆廉价。
#[derive(Debug, Clone)]
pub struct FreqSettings {
    /// 词频维度主开关（全局 schema.{codetable,pinyin}.frequency.enabled）；关则完全不重排。
    pub enabled: bool,
    /// 调频关闭时仍记录（`schema.*.frequency.record_always`）。只放开写端，读端一律
    /// 只认 [`Self::enabled`]；写端判据走 [`Self::records`]，不要直接读本字段。
    pub record_always: bool,
    /// used-first 内的排序策略（全局 schema.codetable.frequency.strategy；仅码表用）。
    pub strategy: FreqStrategy,
    /// 呈现层前 N 位保护（全局 `schema.codetable.frequency.protect_top_n*`；仅码表/混输用）。
    /// 重排前记录基础序前 N 个候选，重排后原序回填——优先级高于词频。
    /// N 按输入码长分级：简码位保住词库钦定首选，全码位放开调频（见 `ProtectPolicy`）。
    pub protect: ProtectPolicy,
    /// 前缀补全参与位置提升的范围（`schema.*.frequency.promote_prefix`）。
    pub promote_prefix: PromotePrefix,
    /// 英文候选记账码是否用输入码（`schema.english.frequency.code_scope == "input"`）。
    /// `false`（默认）= 用候选码，与拼音同侧。
    ///
    /// 本项按候选来源生效、不按当前方案，故 `freq_settings` 的三个分支取同一个值——
    /// 混输里的英文候选同样按英文口径记账。
    pub english_code_by_input: bool,
    /// 字符类判定结构。免词频取其中开了 `no_freq` 的那些类（并集语义）。
    ///
    /// 与 [`Self::english_code_by_input`] 一样跨引擎共用一份（「emoji 不参与词频」不该因引擎而异）。
    pub charsets: Arc<wind_candidate::CharsetRegistry>,
}

impl Default for FreqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            record_always: false,
            strategy: FreqStrategy::Step,
            protect: ProtectPolicy::NONE,
            promote_prefix: PromotePrefix::Single,
            english_code_by_input: false,
            // 空 registry 的 `no_freq` 恒假。
            charsets: Arc::default(),
        }
    }
}

impl FreqSettings {
    /// 选中时要不要记账：调频开着，或用户要求「只记次数、不调序」。
    ///
    /// 写端（`record_selection_in`）用它，读端（重排 / 英文热词召回 / 联想打分）用
    /// [`Self::enabled`]——两端开关刻意不同，这正是 `record_always` 的语义。
    pub fn records(&self) -> bool {
        self.enabled || self.record_always
    }

    /// 这条候选文本要不要整个绕开词频（既不记录、也不重排）。
    ///
    /// 写端 `record_selection_in` 与读端 `apply_freq_rerank_in` 必须用同一判据：只跳写端，
    /// 既有记录照旧生效；只跳读端，记录继续累积、关开关后突然全部浮上来。挂在两端都在读的
    /// [`FreqSettings`] 上即由类型保证同源。
    pub fn excluded_from_freq(&self, text: &str) -> bool {
        self.charsets.no_freq(text)
    }
}

/// 「作为混输辅助的拼音」才有的收敛项。
///
/// 混输的击键串同时是码表码，整串当拼音解读会抢走码表首位，故有些拼音行为只在混输下收紧。
/// 纯拼音方案不该受影响，所以由调用方按语境注入（`build_engine` 参数），不读全局配置。
/// 「是不是混输辅助」由 [`MixedRole`] 表达，本结构只承载取值。
#[derive(Clone, Copy)]
struct MixPinyinOpts {
    /// `schema.mix.enable_pinyin_abbrev`：是否产出简拼候选。
    abbrev: bool,
}

/// 码表引擎的整句开关最终取值。
///
/// 作为混输主引擎构建时取混输方案自己的声明，否则取本方案的。抽成纯函数以便单测
/// 「混输不继承 primary_schema 的整句声明」。
fn resolve_sentence_input(role: Option<MixedRole>, own: bool) -> bool {
    match role {
        Some(MixedRole::Primary { sentence_input, .. }) => sentence_input,
        _ => own,
    }
}

/// 逆切分开关的取值来源：独立方案用自己的声明，混输主引擎一律关闭。
///
/// 切分候选的 `code` 就是整串输入（自动上屏依赖这点），引擎靠不置 `is_exact_code` 避开
/// 精确档；但混输的 `source_tier` / `truncation_tier` 按 `code == input` 判档，切分候选会
/// 落进档 0 压过拼音精确候选。在混输档位体系里给它定档之前整体关闭（见
/// docs/design/codetable-split-input.md §5.6）。「写了却不生效」的告警在 `build_engine` 的混输分支里。
fn resolve_split_input(role: Option<MixedRole>, _own: bool) -> bool {
    match role {
        Some(MixedRole::Primary { .. }) => false,
        _ => _own,
    }
}

/// 递归构建混输子引擎时，告诉被构建方「你在给谁当零件」。`None` = 独立方案。
#[derive(Clone, Copy)]
enum MixedRole {
    /// 混输主（码表）。
    ///
    /// `sentence_input` 由混输方案自己的 `[engine.codetable]` 决定，不继承 primary_schema：
    /// 混输的超码长区间归拼音（`convert_overflow` 不经主引擎），继承过来只会「开着却不生效」。
    Primary { sentence_input: bool },
    /// 混输次（拼音），携带拼音侧的语境收敛。
    Secondary(MixPinyinOpts),
}

/// 一个待注册的码表词库层（[`EngineManager::load_codetable_layers`] 的产出）。
struct CodetableLayer {
    name: String,
    dict: CachedDict,
    enabled: bool,
    /// `[[dictionaries]].base_order`：库间硬分档。
    base_order: i32,
    /// `[[dictionaries]].default_weight`：抹平整库权重。
    default_weight: Option<i32>,
}

/// 方案级 `[weight_spec]` → [`wind_dict::WeightNorm`]，施加到本方案全部词库层。
///
/// 未配置 → `None`（守约方案的常态，不做任何换算）。
///
/// 必须是方案级、别改成按库：同一个单调映射保序，库间相对关系原样保留；不同库各用一个
/// 映射则没有保序保证，扩展库条目可能反超主库（`base_order` 救不回来，`better()` 里 weight
/// 在前）。见 `Schema::weight_spec`。参数不自洽时告警，不静默跳过。
fn weight_norm_of(schema: &Schema) -> Option<wind_dict::WeightNorm> {
    let ws = schema.weight_spec.as_ref()?;
    let norm = wind_dict::WeightNorm::from_parts(ws.median, ws.max, &ws.mode, ws.target);
    if norm.is_none() {
        warn!(
            "方案 {} 的 [weight_spec] 参数不自洽（median={} max={} target={}），归一化未生效。\
             要求 0 < median < max 且 0 < target < 10000。",
            schema.schema.id, ws.median, ws.max, ws.target
        );
    }
    norm
}

/// overlay 方案注册表的一条（见 [`EngineManager::overlay_modes`]）。
///
/// 实例身份 = 方案 id。见 `docs/redesign/overlay-mode-config.md`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayEntry {
    /// 被引用（即自身）的方案 id。
    pub schema_id: String,
    /// 显示名（`[schema] name`，空则回退 id）。
    pub name: String,
    /// 模式指示短称（`[schema] icon_label` 按显示宽度截断，可空）。
    pub icon_label: String,
    /// `[overlay]` 段本体。
    pub spec: wind_config::OverlaySpec,
}

/// 一条已解析的辅助码来源，见 `[engine.aux_code].files`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuxSource {
    /// 码表文件的绝对路径。
    File(std::path::PathBuf),
    /// 被引用的码表方案 id（已验证存在、是 codetable、不是自己）。
    Schema(String),
}

/// 活跃方案的辅助码生效设置（全局基线折叠方案覆盖后的结果）。
///
/// 由 [`EngineManager::aux_code_settings`] 一次读出，供协调器的进入门卫与筛选选项共用
/// ——三个值同源同一次 `read_schema`，不存在「开关读到 A、码表读到 B」的漂移。
#[derive(Debug, Clone, Default)]
pub struct AuxCodeSettings {
    /// 本方案最终是否启用辅助码（`[schema.pinyin.aux_code].enabled` 经方案 tri-state 折叠）。
    pub enabled: bool,
    /// 词组长度上限（0 = 不限）。
    pub max_phrase_len: usize,
    /// 直接辅助码最终是否生效：`enabled` 生效、`direct` 经方案 tri-state 折叠为真，且方案
    /// 是双拼（`[engine] type = "pinyin"` + `[engine.pinyin] scheme = "shuangpin"`）。
    /// 全拼写了 `direct = true` 这里为 false 并告警一次（设计 §7）。
    pub direct: bool,
    /// 直接辅助码走连续辅助码（击键图 + 辅码表）：`direct` 为真且方案没写内部逃生口
    /// `[engine.aux_code] continuous = false`。
    pub continuous: bool,
    /// 编码栏辅码分隔符（方案内部项 `[engine.aux_code] preedit_separator`，已校验）；`None` =
    /// [`crate::engine::AUX_PREEDIT_SEPARATOR`]。取值见 [`Self::preedit_separator`]。
    pub preedit_separator: Option<Arc<str>>,
    /// 已解析的来源，顺序即优先级。`enabled == false` 时恒空（关闭即不解析）。
    pub sources: Vec<AuxSource>,
}

impl AuxCodeSettings {
    /// 编码栏辅码前的分隔符：连续辅助码的辅码候选与串尾直接辅助共用这一处。
    pub fn preedit_separator(&self) -> &str {
        self.preedit_separator
            .as_deref()
            .unwrap_or(crate::engine::AUX_PREEDIT_SEPARATOR)
    }

    /// 其中被引用的方案 id。
    pub fn schema_sources(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().filter_map(|s| match s {
            AuxSource::Schema(id) => Some(id.as_str()),
            AuxSource::File(_) => None,
        })
    }
}

/// 设置页「辅助码来源」的可选项。
pub struct AuxCodeSourceOptions {
    /// (方案 id, 方案名)：已安装的码表方案，含隐藏、不看是否启用。
    pub schemas: Vec<(String, String)>,
    /// (相对 schemas 的路径, 绝对路径)：各层 `schemas/aux_code/*.txt`，靠前的层遮蔽同名者。
    pub files: Vec<(String, std::path::PathBuf)>,
}

/// 某个码位区间在词库里的命中情况，[`EngineManager::scan_chars_in_range`] 的产出。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RangeScan {
    /// 命中的字符，按码位升序、去重。
    pub chars: Vec<char>,
    /// 受影响的词条数（去重）：含有区间内任一字符的词条有多少条。
    ///
    /// 界面须显示这个而不只是 `chars.len()`：一个字符可能出现在数百条词条里，标为生僻会
    /// 连带它们一起判非常用。刻意不是「各字符命中数之和」，否则一条词被数多遍。
    pub entries: usize,
    /// 累加期间的去重集合（`finish` 后转进 `chars`）。
    seen: std::collections::BTreeSet<char>,
}

impl RangeScan {
    /// 吃一条词条的文本。
    fn tally(&mut self, text: &str, start: u32, end: u32) {
        let mut hit_this_entry = false;
        for ch in text.chars() {
            let c = ch as u32;
            // 空白与控制字符即便落在区间里也不收（`is_markable`），否则含空格的词条全判
            // 非常用，且该覆盖导出能写、导入被拒。挡在扫描层，预览与执行读同一份 `chars` 才一致。
            if start <= c && c <= end && wind_candidate::is_markable(ch) {
                self.seen.insert(ch);
                hit_this_entry = true;
            }
        }
        if hit_this_entry {
            self.entries += 1;
        }
    }

    /// 把去重集合落成有序的 `chars`。`BTreeSet` 已按码位有序，直接倒出即可。
    fn finish(&mut self) {
        self.chars = std::mem::take(&mut self.seen).into_iter().collect();
    }
}

/// 混输取英文子引擎的通道：由 [`EngineManager::ensure_loaded`] 注入，回调进
/// [`EngineManager::shared_english_engine`]。
///
/// 给回调而不是 `Arc`：`schema.mix.enable_english` 关着时不该建英文引擎；`build_engine`
/// 是关联函数，够不着那份缓存，只能由调用方注入。
type EnglishProvider<'a> = &'a dyn Fn() -> Option<Arc<dyn Engine>>;

/// 词语联想：临时词进「个人档」的使用次数门槛。新建即 count=1，达到 2 = 建出来之后又用过。
pub const ASSOC_TEMP_MIN_COUNT: u32 = 2;
/// 词语联想：每档先取 `limit ×` 本值作重排池。
pub const ASSOC_POOL_FACTOR: usize = 4;

/// [`LoadedEngine::last_used`] 的记号：校验缓存刚建好、还没人用过（见
/// [`EngineManager::refresh_schema_cache`]）。时钟读数从 0 往上走，碰不到它。
const UNTOUCHED: u64 = u64::MAX;

#[cfg(test)]
type BeforePublishHook = Arc<dyn Fn(&EngineManager, &str) + Send + Sync>;

/// `engines` 表里的一项：引擎 + 最后使用时刻（方案常驻策略的闲置淘汰用）。
///
/// 时刻存的是 [`EngineManager::use_clock`] 的读数（毫秒，粗粒度——淘汰拍子推进一次），
/// 不在取引擎时读系统时钟：按键热路径上的全部开销就是一次原子读 + 一次原子写。
struct LoadedEngine {
    engine: Arc<dyn Engine>,
    last_used: std::sync::atomic::AtomicU64,
}

thread_local! {
    /// 本线程调用 `read_schema_in`（读方案文件）的次数，见 [`schema_reads_on_current_thread`]。
    static SCHEMA_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// 当前线程累计读了几次方案文件。测试用它钉「输入流程零读盘」：预热后在同一线程上打字，
/// 计数不应再变（按线程计，并行测试互不干扰）。
#[doc(hidden)]
pub fn schema_reads_on_current_thread() -> u64 {
    SCHEMA_READS.with(|n| n.get())
}

/// 引擎管理器（懒加载：仅在需要时构建对应方案引擎，降低启动内存）。
pub struct EngineManager {
    /// schema_id -> 引擎实例（懒加载，Arc 便于无锁 convert）+ 最后使用时刻
    engines: Mutex<HashMap<String, LoadedEngine>>,
    /// `engines` 的失效代次：凡「表里的引擎已按旧配置建成、不能再用」的失效口
    /// （`invalidate_schema`、`reload_from_config`）都在持 `engines` 锁时 +1。
    /// `ensure_loaded` 锁外构建前记代次、写回前（同一把锁下）比对，变了就丢掉重建——否则
    /// 「建到一半被失效」的旧引擎会被插回表里，一直用到下次失效。共享英文引擎缓存同理。
    /// 闲置淘汰不推进它：被摘的引擎配置没变，正在建的那份照样能用。
    ///
    /// 全局一个代次是有意的取舍：无关方案的失效也会让在建的构建白建一次，换来不必维护
    /// 「谁依赖谁」的按方案代次。失效是低频事件，不会活锁。
    engines_gen: std::sync::atomic::AtomicU64,
    /// 淘汰时钟的零点（构造时刻）。
    clock_origin: std::time::Instant,
    /// 淘汰时钟：距 `clock_origin` 的毫秒数，只在淘汰拍子里推进（[`Self::evict_idle`]）。
    ///
    /// 取引擎时把读数记进 [`LoadedEngine::last_used`]，按键热路径上只多一次原子读写。
    /// 代价是精度只到一个拍子（生产 1 分钟），对 30 分钟的闲置判据无关紧要。
    use_clock: std::sync::atomic::AtomicU64,
    /// 当前活跃方案 ID
    active: Mutex<String>,
    /// 活跃方案的变更代际：每次 `active` 真正改变时 +1。
    ///
    /// 只比对 id 分不清「切走又切回来」与「从未变过」；消费者是协调器的方案往返键
    /// （`toggle_schema`），期间有任何切换就作废来源记录。递增与
    /// [`crate::active_hook::notify_active_changed`] 绑在 [`Self::on_active_changed`] 里，
    /// 不由各赋值点自己加：漏通知看得见，漏计数看不见，把后者搭在前者上。
    schema_generation: std::sync::atomic::AtomicU64,
    /// 可用方案列表（已过滤不支持的方案，用于循环切换）。
    /// Mutex 以支持配置热重载时原地更新（无需重建 EngineManager）。
    available: Mutex<Vec<String>>,
    /// 数据目录（懒加载时按需读取 schema）
    data_dir: Option<std::path::PathBuf>,
    /// redb 持久化存储（用户词/临时词层；None=无持久化，如纯测试/REPL）
    store: Option<Arc<wind_store::Store>>,
    /// 全局码表配置（公共基线；方案经 schema_overrides 的 [engine.codetable] 段逐字段覆盖）。
    /// Mutex 以支持热重载（变更后清空引擎缓存按新策略重建）。
    codetable: Mutex<wind_config::CodetableGlobal>,
    /// 全局混输配置（融合策略；全局唯一，无方案级 override）。Mutex 以支持热重载。
    mix: Mutex<wind_config::MixGlobal>,
    /// 全局英文配置（英文方案的行为与调频；全局唯一）。Mutex 以支持热重载。
    english: Mutex<wind_config::config::EnglishGlobal>,
    /// 共享英文引擎缓存——「英文候选混入」与混输的英文子引擎取的是同一份。
    ///
    /// `None` = 尚未尝试；`Some(None)` = 试过且不可用（英文词库缺失），不再重试——否则每键
    /// 重走一遍构建、失败、`warn!`。两处重置缺一不可：`reload_from_config`，以及
    /// `invalidate_schema(ENGLISH_SCHEMA)`（扩展词库启停会换掉 `engines` 里那份，不清就握着旧实例）。
    shared_english_engine: Mutex<Option<Option<Arc<dyn Engine>>>>,
    /// 全局临时拼音配置（码表方案下临时切拼音反查；全局唯一）。Mutex 以支持热重载。
    temp_pinyin: Mutex<wind_config::config::TempPinyinConfig>,
    /// 临时英文配置镜像，目前只为 `phrase_seg`：词组分词索引要不要预热取决于英文方案 /
    /// 临英 / 快捷输入英文三处开关，临英那份不在 `schema` 段里，`english` 镜像够不着。
    temp_english: Mutex<wind_config::config::TempEnglishConfig>,
    /// 全局 `input.reverse.lookup_disabled_dicts` 的镜像；只在 `new` 与 `reload_from_config` 写，
    /// `ensure_loaded` 读出交给 `build_engine` 决定挂不挂影子层（未启用扩展库，只给反查模式）。
    ///
    /// `reload_from_config` 里必须写在 `engines.clear()` 之前，否则末尾那次
    /// `ensure_loaded` 会拿旧值建引擎；协调器侧 `engine_reload_needed` 也必须收这个键。
    reverse_lookup_disabled: std::sync::atomic::AtomicBool,
    /// 字符类判定结构（免词频等），存解析结果，消费点在按键热路径上。跟 `codetable`/`english`
    /// 镜像一样在配置重载时刷新，漏了就是「设置页改了不生效、重启后才生效」。
    charsets: Mutex<Arc<wind_candidate::CharsetRegistry>>,
    /// 词频排序设置缓存（schema_id -> FreqSettings；按需解析、避免每键读盘）
    freq_cache: Mutex<HashMap<String, FreqSettings>>,
    /// 辅助码生效设置缓存（schema_id -> AuxCodeSettings），只供逐键路径
    /// （[`EngineManager::aux_code_settings_cached`]，直接辅助码每个双拼按键都要问）。与
    /// `freq_cache` 同生命周期：`invalidate_schema`（方案覆盖层写入 / 删除 / 重建都走它）与
    /// `reload_from_config` 时清。任一方案失效都整表清——`schema:` 来源会引用别的方案，
    /// 按 id 局部清会漏掉引用方。进入辅助码（`aux_code_settings_cached_of`）也走它。
    aux_settings_cache: Mutex<HashMap<String, AuxCodeSettings>>,
    /// `aux_settings_cache` 的失效代次：每次清表（持锁）+1。未命中时先记代次再算，回填前
    /// （同一把锁下）代次变了就不回填——否则「算到一半被清」的旧值会被插回去，一直留到下次失效。
    aux_settings_gen: std::sync::atomic::AtomicU64,
    /// 码表设置缓存（(schema_id, 是否按混输解析) → 生效设置），供 [`EngineManager::codetable_settings`]。
    ///
    /// 必须缓存：每键要取多次，每次都要读盘解析方案（混输再读主码表方案）。键里带「是否混输」：
    /// 它取决于引擎加载了没有，前后解析结果不同。失效同 `aux_settings_cache`（整表清，
    /// 混输引用主码表方案，按 id 局部清会漏）。
    codetable_settings_cache: Mutex<HashMap<(String, bool), wind_config::CodetableGlobal>>,
    /// `codetable_settings_cache` 的失效代次（同 `aux_settings_gen`）。
    codetable_settings_gen: std::sync::atomic::AtomicU64,
    /// 方案引擎类型缓存（`schema_engine_type`）。按 id 缓存，reload/invalidate 时清，
    /// 与 `freq_cache`/`name_cache` 同生命周期。按键路径每键调多次，不缓存会读盘卡顿；
    /// 缓存 `None` 也有意义，避免方案文件不存在时反复读盘刷警告。
    schema_type_cache: Mutex<HashMap<String, Option<String>>>,
    /// 方案定义缓存（schema_id → 合并 override 后的 `Schema`），供按键路径取方案级标量字段
    /// （混输成员、拼音分隔符策略），按键路径上不能有 `read_schema`。
    ///
    /// 词库清单这类随词库目录变化的字段不要从这里取：缓存只随方案失效与配置重载清，
    /// 不随词库目录变化清。失效同 `aux_settings_cache`（整表清 + 代次）。
    schema_def_cache: Mutex<HashMap<String, Option<Arc<Schema>>>>,
    /// `schema_def_cache` 的失效代次（同 `aux_settings_gen`）。
    schema_def_gen: std::sync::atomic::AtomicU64,
    /// 词库统计结果（方案 id → (缓存键, 结果)），见 [`Self::dict_stats`]。键含各库摘要，
    /// 词库一变自然失效，故不接 `invalidate_schema`；只存结果（KB 级），不存中间集合。
    dict_stats_cache: Mutex<HashMap<String, (String, Arc<dict_stats::SystemDictStats>)>>,
    /// 词库统计按方案 single-flight 的闸（方案 id → 锁），见 [`Self::dict_stats`]。
    dict_stats_gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// 已建引擎所用用户分库的指纹（方案 id → [(数据方案, 指纹)]），建引擎前记下。不带路径的
    /// `dict.reload` 据此判断分库变没变（[`Self::user_dicts_changed`]）。
    user_dicts_seen: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// 方案级 `[key_actions]` 表缓存（schema_id → 表）。
    ///
    /// 必须缓存：消费点 `Coordinator::bound_action_for` 在按键热路径上。值用 `Arc`，
    /// 命中时不必 clone 整张表。与 `schema_type_cache` 等同批失效（`invalidate_schema`）。
    key_actions_cache: Mutex<HashMap<String, Arc<std::collections::BTreeMap<String, String>>>>,
    /// 方案级 `[session_actions]` 表缓存（schema_id → 表）。
    ///
    /// 与 [`Self::key_actions_cache`] 同构、同批失效：两个失效点（`invalidate_schema`
    /// 按 id 移除、配置热重载整表清空）都要接，漏一处的表现是「设置页改了不生效」。
    session_actions_cache: Mutex<HashMap<String, Arc<std::collections::BTreeMap<String, String>>>>,
    /// 方案级行为覆盖缓存（schema_id → `[punct]` / `[candidate]` / `[phrases]` 三段快照）。
    ///
    /// 三段同源（一次 `read_schema`）、同批失效，故合成一个缓存条目。与
    /// [`Self::key_actions_cache`] 同批失效：`invalidate_schema` 按 id 移除、配置热重载整表清空。
    behavior_cache: Mutex<HashMap<String, Arc<wind_config::SchemaBehavior>>>,
    /// 方案显示名与图标短称缓存（schema_id -> (schema.name 缺则回退 id, 截断后的 icon_label)）。
    /// 两者出自同一次读方案文件，一并缓存、同批失效；按需读盘一次（方案列表每项都要取，
    /// 手机上每次弹键盘都会列一遍）。
    name_cache: Mutex<HashMap<String, (String, String)>>,
    /// overlay 方案注册表缓存（见 [`Self::overlay_modes`]）。
    ///
    /// 整表缓存而非 per-schema：任何方案的安装/删除/`[overlay]` 段变更都会改变它，
    /// 故 `invalidate_schema` 里直接整表置 `None`。
    overlay_cache: Mutex<Option<Vec<OverlayEntry>>>,
    /// 方案 override 层目录（schema_overrides/{id}.toml）；读 schema 时深合并到基础方案之上。
    /// None=不读 override（如纯测试）。设置页 saveConfig 写此目录。
    override_dir: Option<std::path::PathBuf>,
    /// 主码表方案 id(拼音反查码源):config.schema.primary_codetable 解析后(可空)。构造/重载时更新。
    primary_codetable: Mutex<String>,
    /// 主拼音方案 id(临时拼音目标):config.schema.primary_pinyin 原样(空=全拼 "pinyin",见
    /// temp_pinyin_target)。不同于 primary_codetable 的 available 扫描——空值定义为固定全拼,
    /// 避免调整 available 顺序静默改变临时拼音方案。构造/重载时更新。
    primary_pinyin: Mutex<String>,
    /// 码表反查索引缓存:方案 id → (汉字/词 → 全部编码,码长升序)。供拼音编码提示与悬停
    /// [编码] 段按词查实际码。懒建(首次需要时按方案词库全量构建),invalidate/reload 时清空。
    /// 内存护栏:每份索引可达数万词条,只缓存「本次请求方 + 主码表 + 在用集合」几份
    /// (见 `reverse_index_for` / `reverse_index_pins`)。
    reverse_index: Mutex<HashMap<String, Arc<ReverseIndex>>>,
    /// 词语联想的用户词 / 临时词文本索引（store 按码排，按文本前缀查要另建）。
    /// 见 [`crate::user_assoc`]；只留当前联想方案一份。
    user_assoc: crate::user_assoc::SharedSlot,
    /// 按词查编码的用户层，按方案分槽。见 [`crate::text_codes`]。
    user_text: crate::text_codes::SharedSlots,
    /// 用户层后台重建写回之后的回调（协调器借它重刷当前这屏候选，见
    /// [`Self::set_user_text_built_hook`]）。
    user_text_built: std::sync::OnceLock<crate::text_codes::BuiltHook>,
    /// 码表单字全码表缓存:方案 id → (汉字 → 全码)。供造词按 `[[encoder.rules]]` 组装
    /// 词组编码(见 `encode_word`)。与 `reverse_index`(按码长升序)分开:这份按权重挑全码。
    /// 只缓存一份:造词恒对活跃方案(混输则其主码表)进行,切方案即弃。
    single_char_codes: Mutex<Option<SingleCharCodeCache>>,
    /// 单字全码表的最后使用（[`Self::use_clock`] 读数），供闲置清扫（[`Self::evict_idle_caches`]）。
    single_char_used: std::sync::atomic::AtomicU64,
    /// 全局拼音配置（fuzzy/...）。Mutex 以支持热重载。
    pinyin: Mutex<wind_config::config::PinyinGlobalConfig>,
    /// 取配置时套哪层覆盖（`[mobile.*]`）。构造时定下（见
    /// [`Self::with_store_override_overlay`]），重载时继续读它。
    overlay: Mutex<wind_config::ConfigOverlay>,
    /// 纠错与提示配置（`[input.correction]`），与 `pinyin` 一同决定 [`pinyin_hint_on`]。
    correction_cfg: Mutex<wind_config::CorrectionConfig>,
    /// 换位纠错规则（`[input.correction]` 生成），注入全拼引擎的 `FuzzyConfig::correction`；
    /// 配置载入时生成一次，各引擎共享。
    correction: Mutex<Arc<crate::pinyin::correction::CorrectionRules>>,
    /// 误读表（内置 ⊕ 用户追加，见 [`crate::pinyin::misread`]）：构造与 `reload_from_config` 时读盘，
    /// 「误读」来源勾选时才注入拼音引擎（[`Self::misread_for_engines`]）。
    misread: Mutex<Arc<crate::pinyin::misread::MisreadTable>>,
    /// 有模板引用 `${correct_pinyin}`（协调器按全部模板位判定后告知，见
    /// [`Self::set_correct_pinyin_in_templates`]）：`hint` 关着也要让拼音引擎算正确拼音提示。
    correct_pinyin_in_templates: std::sync::atomic::AtomicBool,
    /// 拼音引擎「算不算正确拼音提示」的共享开关，注入每个拼音引擎与双拼转换器（见
    /// [`crate::pinyin::fuzzy::HintSwitch`]）。值 = [`pinyin_hint_on`]，由 `pinyin`、`correction_cfg` 与
    /// `correct_pinyin_in_templates` 共同决定（[`Self::refresh_pinyin_hint`]）；翻转不重建引擎。
    pinyin_hint: crate::pinyin::fuzzy::HintSwitch,
    /// 双拼韵母键集缓存：(已缓存的活跃方案 id, Option<HashSet<u8>>)。
    /// None = 当前活跃方案不是双拼；Some = 双拼布局的 finals 键集合。
    /// 活跃方案 id 变化时按需重建（惰性），避免每键读盘。
    shuangpin_finals_cache: Mutex<(String, Option<std::collections::HashSet<u8>>)>,
    /// 双拼反向表缓存：(已缓存的活跃方案 id, Option<Arc<ShuangpinReverse>>)。
    /// None = 该方案不是双拼；Some = 音节 → 击键表，供 `${code_schema}` 出编码。
    ///
    /// 与 `shuangpin_finals_cache` 分开：两者重建成本差两个数量级、键控各自独立。失效条件
    /// 相同，统一走 [`Self::invalidate_shuangpin_caches`]，不要在别处单独清其中一个。
    shuangpin_reverse_cache: Mutex<(
        String,
        Option<Arc<crate::pinyin::shuangpin::ShuangpinReverse>>,
    )>,
    /// 每方案构建锁（single-flight）：同一方案的引擎/缓存构建串行，避免后台预热与首次
    /// 切换并发时重复构建同一份大缓存；不同方案可并行构建（缓存在各自子目录，互不冲突）。
    build_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// 测试钩子：引擎建好、写回 `engines` 之前（参数为方案 id）、共享英文引擎写回缓存之前
    /// （`"@shared_english"`）、`invalidate_schema` 放开 `engines` 锁之后（`"@after_engines_bump"`）
    /// 调用。用来在锁外构建的窗口里确定性地插入一次失效。
    #[cfg(test)]
    before_publish_hook: Mutex<Option<BeforePublishHook>>,
    /// 反查索引的每方案构建锁。与 `build_locks` 刻意分开：索引构建（秒级、上百 MB）
    /// 不该和引擎构建互相阻塞，两者的等待方也不同（前者是后台线程，后者是切换方案的用户）。
    /// 兼作「是否正在建」的判据（`try_lock` 失败即在建），供打字线路决定要不要再 spawn。
    index_build_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// 已告警过的无效辅助码来源条目（`方案\0条目`），见 [`Self::first_aux_source_warn`]。
    aux_source_warned: Mutex<std::collections::HashSet<String>>,
    /// 反查索引被 build_guard 跳过（同一份输入已连续死在构建中）的方案。
    ///
    /// 跳过时不写 `reverse_index` 缓存（空表会被当真结果用），不另记一笔的话协调器每次重绘
    /// 都会「预热 → 再次跳过 → 通知重绘」无限循环；记在这里后预热/判在建直接短路。
    /// 与 `reverse_index` 同生命周期：凡清空那张表的地方一并清空，即给一次重试机会。
    reverse_index_skipped: Mutex<std::collections::HashSet<String>>,
    /// 测试钩子：[`Self::build_reverse_index_for`] 被调用的次数（证明短路生效）。
    #[cfg(test)]
    reverse_index_build_calls: std::sync::atomic::AtomicUsize,
    /// [`Self::active_data_facts`] 的缓存，按活跃方案 id 分键（随活跃方案变，切方案不必失效）。
    /// 协调器在按应用自动切方案（焦点驱动、高频）时也会问它。与 `overlay_cache` 同批失效：
    /// `invalidate_schema` / `reload_from_config`。
    active_facts_cache: Mutex<HashMap<String, ActiveDataFacts>>,
    /// 上面那份缓存的失效代次（同 `aux_settings_gen` 的做法）。
    data_facts_gen: std::sync::atomic::AtomicU64,
    /// 测试钩子：上面那份事实被真正重算的次数。
    #[cfg(test)]
    data_facts_builds: std::sync::atomic::AtomicUsize,
}

/// 诊断：[`EngineManager::memory_report`] 的结果。字节数都按容量估、不含分配器开销。
#[derive(Debug, Default, Clone)]
pub struct MemoryReport {
    /// 已加载引擎：(方案 id, 引擎类型, 引擎内已建懒建结构)。
    pub engines: Vec<(String, String, Vec<crate::engine::MemPart>)>,
    /// 反查索引：(方案 id, 常驻堆字节, 镜像总字节)；mmap 的常驻堆为 0。
    pub reverse_index: Vec<(String, usize, usize)>,
    /// 按词查编码用户层：(数据方案 id, 堆字节, 是否在重建中)。
    pub user_text: Vec<(String, usize, bool)>,
    /// 联想用户词索引：(数据方案 id, 堆字节)。
    pub user_assoc: Option<(String, usize)>,
    /// 单字全码表：(方案 id, 堆字节, 分配块数)。
    pub single_char: Option<(String, usize, usize)>,
    pub user_text_builds: crate::text_codes::BuildStatsSnapshot,
    pub user_assoc_builds: crate::text_codes::BuildStatsSnapshot,
    /// `CharPinyinIndex` 构建（每个拼音引擎一份、首次用到时在调用线程上同步建）。
    pub char_pinyin_builds: crate::text_codes::BuildStatsSnapshot,
}

/// 协调器 `DataNeeds` 要看的、随活跃方案变的事实，见 [`EngineManager::active_data_facts`]。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ActiveDataFacts {
    /// 辅助码当下引用了码表方案（[`EngineManager::aux_code_schemas_in_use`] 非空）。
    pub aux_code_in_use: bool,
    /// 自动造词开着（[`EngineManager::auto_phrase_enabled`]）。
    pub auto_phrase: bool,
}

/// 进程级缓存根目录（%LOCALAPPDATA%\WindInput\cache），EngineManager::new 设置一次。
static CACHE_DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

/// 多库合并缓存（`combined.wdat`）的指纹 tag。
const COMBINED_CACHE_TAG: &str = "combined/v1";
/// rime_pinyin 主表+import_tables 合并缓存（`merged.wdat`）的指纹 tag。
const MERGED_CACHE_TAG: &str = "merged/v1";

/// 反查索引「小到可以直接读进内存」的上限（3 MB），超过就 mmap。两条路查询行为完全相同，
/// 只影响性能。取值依据与不做成配置键的理由见 docs/architecture/rime-dict-loading.md 的「5.1 格式」。
pub const REVERSE_INDEX_RESIDENT_MAX: usize = 3 * 1024 * 1024;

/// 单字全码表缓存项：`(方案 id, 汉字 → 全码)`。见 `EngineManager::single_char_codes`。
type SingleCharCodeCache = (String, Arc<HashMap<char, String>>);

// schema ⊕ schema_overrides 的深合并在 wind_config::schema::merge_toml：方案包导出折叠
// override 时必须与引擎加载视图同源，合并实现只允许存在一份。

/// 当前进程匿名常驻内存（MB，RssAnon），只给构建打点用；非 Linux 返回 0。
fn linux_rss_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        // 取 RssAnon 而非 VmRSS：构建期间 mmap 着的段文件页也算 VmRSS，但那是文件页、
        // 可随时回收，不是本进程的私有内存（与 Windows 看 Private 不看 WS 同理）。
        if let Ok(s) = std::fs::read_to_string("/proc/self/status")
            && let Some(kb) = s
                .lines()
                .find_map(|l| l.strip_prefix("RssAnon:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        {
            return kb / 1024;
        }
    }
    0
}

/// 源文件 → 缓存路径：`<cache>/<相对 schemas 的目录链>/<文件名干>.<ext>`。
///
/// 用源文件在 schemas 下的目录链（不是父目录名）做命名空间，避免跨方案同名冲突，并把一个
/// 方案的全部缓存归拢一处，整方案失效 = 删一目录；推导见 [`wind_dict::cache_ns`]。文件名干
/// 剥掉 `.dict.yaml` 的 `.dict` 中缀。未设置缓存根时回退到源旁（测试/无 LOCALAPPDATA）。
/// 旧布局留下的孤儿有意不清，见 docs/architecture/rime-dict-loading.md 的「5.1 格式」。
fn cache_path(source: &Path, ext: &str) -> std::path::PathBuf {
    if let Some(Some(dir)) = CACHE_DIR.get() {
        let stem = wind_dict::cache_ns::cache_stem(source);
        return wind_dict::cache_ns::cache_path_in(dir, source, &format!("{stem}.{ext}"));
    }
    source.with_extension(ext)
}

/// 递归删除目录下的缓存产物（wdat 词库 / fp 指纹 / wdb unigram / wridx 反查索引 /
/// building 构建死亡记录 / 英文词组索引 `phrase-*.wphr` 及其写盘临时文件），best-effort：
/// 单个文件删除失败（如仍被 mmap 占用）计入 failed 继续。只认白名单（扩展名，外加词组
/// 索引的文件名形态——`.tmp` 只按扩展名认太宽），不触碰目录本身与其它文件（缓存根与
/// 用户数据同在 %LOCALAPPDATA% 命名空间下）。
fn purge_cache_files(dir: &Path, removed: &mut usize, failed: &mut usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        // DirEntry::file_type 不跟随符号链接：防缓存根下被植入指向别处的
        // junction/symlink 后递归删到外部目录。
        let ft = entry.file_type();
        if ft.as_ref().is_ok_and(|t| t.is_dir()) {
            purge_cache_files(&p, removed, failed);
            continue;
        }
        if !ft.is_ok_and(|t| t.is_file()) {
            continue;
        }
        let is_cache = p
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| matches!(s, "wdat" | "fp" | "st" | "wdb" | "wridx" | "building"))
            || p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(crate::english_phrase::is_phrase_cache_file);
        if !is_cache {
            continue;
        }
        match std::fs::remove_file(&p) {
            Ok(()) => *removed += 1,
            Err(e) => {
                warn!("删除缓存 {} 失败: {}", p.display(), e);
                *failed += 1;
            }
        }
    }
}

/// `dir` 里有没有 `.yaml` / `.yml`。目录不存在、读不了都算「没有」——这是给提示用的判据，
/// 不是给功能用的。
fn dir_has_yaml(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        let p = e.path();
        p.is_file()
            && p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("yaml") || x.eq_ignore_ascii_case("yml"))
    })
}

/// [`EngineManager::read_dict_head`] 只读这么多字节的头 —— 超出即认定正文早已开始。
const DICT_HEAD_SCAN_LIMIT: usize = 64 * 1024;

/// 词库 YAML 头里引擎要用的几项（见 [`EngineManager::read_dict_head_full`]）。
#[derive(Debug, Default, Clone)]
struct DictHead {
    /// `name:`
    name: String,
    /// `group:`（rime 不认这个键；本项目约定：设置端词库列表的分组名，只给发现目录的库用）
    group: String,
    /// `import_tables:`
    imports: Vec<String>,
}

/// rime 词库文件后缀（发现目录只认这一种）。
const DICT_YAML_SUFFIX: &str = ".dict.yaml";

/// 码表 / 拼音方案没声明 `[[dictionary_dirs]]` 时兜底的发现目录名（主词库所在目录下）。
const DEFAULT_DICT_DIR: &str = "ext";

/// 把 `sibling` 接到 `rel` 所在目录下：`pinyin/rime_frost.dict.yaml` +
/// `cn_dicts/corrections.dict.yaml` → `pinyin/cn_dicts/corrections.dict.yaml`。
///
/// 统一用 `/` 拼：结果要当稳定 id 用（见 `Coordinator::auto_comment_id`），
/// 两种分隔符混着写会让同一张表在不同写法下得到两个 id。
fn join_rel_dir(rel: &str, sibling: &str) -> String {
    match rel.rsplit_once(['/', '\\']) {
        Some((dir, _)) => format!("{dir}/{sibling}"),
        None => sibling.to_string(),
    }
}

/// 一个被方案声明的词库文件。见 [`EngineManager::declared_dict_files`]。
#[derive(Debug, Clone)]
pub struct SchemaDictFile {
    /// 方案文件里写的相对路径（如 `pinyin/rime_frost.dict.yaml`），稳定 id 的来源。
    pub rel: String,
    /// 按层序解析后的绝对路径（用户 > custom > 安装）。
    pub path: std::path::PathBuf,
    /// 词库显示名；可能为空（方案没写 `label`）。
    pub label: String,
    /// 声明了这份词库的全部方案 id。
    pub schemas: Vec<String>,
}

impl EngineManager {
    /// 从配置创建；仅构建活跃方案引擎，其余按需懒加载。
    pub fn new(config: &Config, data_dir: Option<&Path>) -> Self {
        Self::with_store(config, data_dir, None)
    }

    /// 同 [`new`]，但注入 redb 存储以注册用户词/临时词层（coordinator 用）。
    /// override 目录默认取 `Config::user_config_dir()/schema_overrides`（与用户 schema 覆盖同根）。
    pub fn with_store(
        config: &Config,
        data_dir: Option<&Path>,
        store: Option<Arc<wind_store::Store>>,
    ) -> Self {
        Self::with_store_override(config, data_dir, store, Self::default_override_dir())
    }

    /// 默认的方案 override 目录：用户配置目录下的 `schema_overrides`。
    pub fn default_override_dir() -> Option<std::path::PathBuf> {
        Config::user_config_dir().map(|d| d.join("schema_overrides"))
    }

    /// 同 [`with_store`]，但显式指定 override 目录（测试用，避免污染真实用户目录）。
    pub fn with_store_override(
        config: &Config,
        data_dir: Option<&Path>,
        store: Option<Arc<wind_store::Store>>,
        override_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self::with_store_override_overlay(
            config,
            data_dir,
            store,
            override_dir,
            wind_config::ConfigOverlay::Base,
        )
    }

    /// 真实构造入口：覆盖层（`[mobile.*]`）在构造时定下，引擎一次建成，不经重载。
    pub fn with_store_override_overlay(
        config: &Config,
        data_dir: Option<&Path>,
        store: Option<Arc<wind_store::Store>>,
        override_dir: Option<std::path::PathBuf>,
        overlay: wind_config::ConfigOverlay,
    ) -> Self {
        // 初始化缓存根（一次）：%LOCALAPPDATA%\WindInput\cache，提前建好目录
        CACHE_DIR.get_or_init(|| {
            let dir = Config::cache_dir();
            if let Some(d) = &dir {
                let _ = std::fs::create_dir_all(d);
            }
            dir
        });

        // 活跃方案被定制版 hide 掉时在此降级（不改写用户配置），见 resolve_active_schema_id。
        let ov = override_dir.as_deref();
        let cfg_active = config.active_schema().to_string();
        let active_id = Self::resolve_active_schema_id(config, data_dir, ov);
        let misread = Self::load_misread(data_dir, store.as_ref().map(|s| s.user_text_dir()));
        let mut available = config.schema.available.clone();
        if available.is_empty() {
            available.push(active_id.clone());
        }
        // 过滤不支持的方案（如双拼），但始终保留活跃方案
        available.retain(|sid| sid == &active_id || Self::schema_supported(sid, data_dir, ov));
        Self::ensure_fallback_listed(&mut available, &cfg_active, &active_id);
        // 主码表方案(拼音反查码源):config 显式 > available 首个 codetable 类型方案。
        let primary_codetable = Self::resolve_primary_codetable(
            &config.schema.primary_codetable,
            &available,
            data_dir,
            ov,
        );

        let mgr = Self {
            engines: Mutex::new(HashMap::new()),
            engines_gen: std::sync::atomic::AtomicU64::new(0),
            clock_origin: std::time::Instant::now(),
            use_clock: std::sync::atomic::AtomicU64::new(0),
            active: Mutex::new(active_id.clone()),
            schema_generation: std::sync::atomic::AtomicU64::new(0),
            available: Mutex::new(available),
            data_dir: data_dir.map(|d| d.to_path_buf()),
            codetable: Mutex::new(config.schema.codetable.clone()),
            mix: Mutex::new(config.schema.mix.clone()),
            english: Mutex::new(config.schema.english.clone()),
            shared_english_engine: Mutex::new(None),
            temp_pinyin: Mutex::new(config.input.temp_pinyin.clone()),
            temp_english: Mutex::new(config.input.temp_english.clone()),
            reverse_lookup_disabled: std::sync::atomic::AtomicBool::new(
                config.input.reverse.lookup_disabled_dicts,
            ),
            // 用户层在 store 里（`wind_store::charsets`），装配前先 `as_deref` 借用，
            // 下一行才把 `store` 本体 move 进结构体。
            charsets: Mutex::new(Arc::new(Self::build_charsets(
                config,
                data_dir,
                store.as_deref(),
            ))),
            store,
            freq_cache: Mutex::new(HashMap::new()),
            aux_settings_cache: Mutex::new(HashMap::new()),
            aux_settings_gen: std::sync::atomic::AtomicU64::new(0),
            codetable_settings_cache: Mutex::new(HashMap::new()),
            codetable_settings_gen: std::sync::atomic::AtomicU64::new(0),
            schema_type_cache: Mutex::new(HashMap::new()),
            schema_def_cache: Mutex::new(HashMap::new()),
            schema_def_gen: std::sync::atomic::AtomicU64::new(0),
            dict_stats_cache: Mutex::new(HashMap::new()),
            dict_stats_gates: Mutex::new(HashMap::new()),
            user_dicts_seen: Mutex::new(HashMap::new()),
            key_actions_cache: Mutex::new(HashMap::new()),
            session_actions_cache: Mutex::new(HashMap::new()),
            behavior_cache: Mutex::new(HashMap::new()),
            name_cache: Mutex::new(HashMap::new()),
            overlay_cache: Mutex::new(None),
            override_dir,
            primary_codetable: Mutex::new(primary_codetable),
            primary_pinyin: Mutex::new(config.schema.primary_pinyin.clone()),
            reverse_index: Mutex::new(HashMap::new()),
            user_assoc: Default::default(),
            user_text: Default::default(),
            user_text_built: std::sync::OnceLock::new(),
            single_char_codes: Mutex::new(None),
            single_char_used: std::sync::atomic::AtomicU64::new(0),
            pinyin: Mutex::new(config.pinyin_effective(overlay)),
            overlay: Mutex::new(overlay),
            correction_cfg: Mutex::new(config.input.correction.clone()),
            correction: Mutex::new(Arc::new(
                crate::pinyin::correction::CorrectionRules::from_config(&config.input.correction),
            )),
            misread: Mutex::new(Arc::new(misread)),
            correct_pinyin_in_templates: std::sync::atomic::AtomicBool::new(false),
            pinyin_hint: crate::pinyin::fuzzy::HintSwitch::new(pinyin_hint_on(
                &config.schema.pinyin.fuzzy,
                &config.input.correction,
                false,
            )),
            shuangpin_finals_cache: Mutex::new((String::new(), None)),
            shuangpin_reverse_cache: Mutex::new((String::new(), None)),
            build_locks: Mutex::new(HashMap::new()),
            #[cfg(test)]
            before_publish_hook: Mutex::new(None),
            index_build_locks: Mutex::new(HashMap::new()),
            aux_source_warned: Mutex::default(),
            reverse_index_skipped: Mutex::new(std::collections::HashSet::new()),
            #[cfg(test)]
            reverse_index_build_calls: std::sync::atomic::AtomicUsize::new(0),
            active_facts_cache: Mutex::new(HashMap::new()),
            data_facts_gen: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            data_facts_builds: std::sync::atomic::AtomicUsize::new(0),
        };
        // 仅同步构建活跃方案；其余方案由 Coordinator 启动后台预热（prewarm_schema）提前构建，
        // 避免首次切换时同步重熔大词库卡顿。单飞构建锁保证预热与切换不重复构建。
        mgr.ensure_loaded(&active_id);
        mgr
    }

    /// 当前活跃引擎是否开启整句输入（当前只有码表引擎会返回 true）。
    /// 供协调器判定手动分隔符键是否放行，见 `Engine::sentence_input_enabled`。
    pub fn sentence_input_enabled(&self) -> bool {
        self.sentence_input_enabled_of(&self.active_schema_id())
    }

    /// 指定方案是否开启整句输入。overlay 用这条，见 [`Self::engine_for`]。
    pub fn sentence_input_enabled_of(&self, schema_id: &str) -> bool {
        self.engine_for(schema_id)
            .is_some_and(|e| e.sentence_input_enabled())
    }

    /// 活跃方案生效的拼音分隔符模式（auto/quote/backtick/none）。
    /// 分隔符键的最终判定（含 auto 动态避让候选选择键）在协调器侧完成——
    /// 因「`'` 是否为选择键」需读 `select_key_groups`（协调器配置），引擎无该信息。
    pub fn pinyin_separator_mode(&self) -> String {
        self.pinyin_separator_mode_of(&self.active_schema_id())
    }

    /// 指定方案生效的分隔符模式：方案段 `[engine.pinyin].separator`（含
    /// `schema_overrides` 深合并）折叠全局基线 `[schema.pinyin].separator`，
    /// 与 `[engine.aux_code].enabled` 同构的 tri-state（`None` = 跟随全局）。
    ///
    /// 必须能按方案分：全拼与双拼的键位预算不同，双拼出厂把反引号给了辅助码
    /// （`shuangpin.schema.toml` 的 `backtick = "aux_code"`），全局唯一的 `separator`
    /// 一改就会夺走双拼的辅助码触发键。
    pub fn pinyin_separator_mode_of(&self, schema_id: &str) -> String {
        let global = self
            .pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .separator
            .clone();
        if schema_id.is_empty() {
            return global;
        }
        self.schema_def_cached(schema_id)
            .and_then(|s| s.engine.pinyin.separator.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or(global)
    }

    /// 当前活跃拼音方案是否为双拼（`engine.pinyin.scheme == "shuangpin"`）。
    /// 复用韵母键集缓存（Some 即双拼），与 `shuangpin_final_key` 同源、
    /// 方案切换/reload 自动失效。
    ///
    /// 别按「是不是双拼」去关某个拼音特性：拼音族的差异应落在方案级配置上（如双拼出厂
    /// `separator = "none"`，见 `docs/design/shuangpin-separator.md`），按引擎类型硬判会把
    /// 用户显式配的东西一并否掉。
    pub fn pinyin_is_shuangpin(&self) -> bool {
        self.pinyin_is_shuangpin_of(&self.active_schema_id())
    }

    /// 指定方案是否为双拼。overlay 用这条，见 [`Self::engine_for`]。
    ///
    /// 缓存是单槽（key = 方案 id）：交替询问不同方案会来回刷，每次代价一次 `read_schema`。
    pub fn pinyin_is_shuangpin_of(&self, schema_id: &str) -> bool {
        let id = schema_id.to_string();
        {
            let cache = self
                .shuangpin_finals_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cache.0 == id {
                return cache.1.is_some();
            }
        }
        let finals_set = self.build_shuangpin_finals(&id);
        let is_sp = finals_set.is_some();
        *self
            .shuangpin_finals_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (id, finals_set);
        is_sp
    }

    // 双拼韵母占用符号键（如 `;` = ing）由码元字符集单点仲裁：`PinyinEngine::input_chars`
    // 从布局推导，协调器 `try_code_char_gate` 抢在选词/引导键/标点之前接管，不在这里避让。

    /// 指定方案当前生效的双拼布局 id（已合并 `schema_overrides/<id>.toml`）；
    /// 非双拼方案、或方案读不出来返回 `None`。方案没写 `layout` 时给缺省的
    /// [`DEFAULT_SHUANGPIN_LAYOUT`]。设置页与韵母键集都走这里，保证显示与生效同源。
    ///
    /// 它不是唯一判据：`build_engine` 里的 `Layout::from_toml` 仍是第二份同构实现（带自己的
    /// 失败回退全拼与 `warn!`），两边只共享缺省常量。
    ///
    /// 混输方案恒返回 `None`，是取舍：混输下敲的是「主码表码 + 拼音码」混合流，给单一的
    /// 双拼击键串会误导（`code_source_schema` 对 `Mixed` 转发，是因为码表编码与输入方式无关）。
    fn shuangpin_layout_id_of(&self, schema_id: &str) -> Option<String> {
        let data_dir = self.data_dir.as_deref()?;
        let schema = Self::read_schema(schema_id, Some(data_dir), self.override_dir.as_deref())?;
        if !schema
            .engine
            .pinyin
            .scheme
            .eq_ignore_ascii_case("shuangpin")
        {
            return None;
        }
        Some(if schema.engine.pinyin.shuangpin.layout.is_empty() {
            DEFAULT_SHUANGPIN_LAYOUT.to_string()
        } else {
            schema.engine.pinyin.shuangpin.layout.clone()
        })
    }

    /// 加载指定方案当前生效的双拼布局；非双拼、或布局文件读不出来返回 `None`。
    ///
    /// 用户目录优先（`resolve_schema_file`）：`%APPDATA%/…/schemas/shuangpin/<id>.toml`
    /// 存在即覆盖安装目录，使用户自带/覆盖的双拼布局生效。
    fn load_shuangpin_layout(&self, schema_id: &str) -> Option<crate::pinyin::shuangpin::Layout> {
        let data_dir = self.data_dir.as_deref()?;
        let layout_id = self.shuangpin_layout_id_of(schema_id)?;
        let lp = Self::resolve_schema_file(&format!("shuangpin/{layout_id}.toml"), data_dir);
        crate::pinyin::shuangpin::Layout::from_toml(&lp).ok()
    }

    /// 内部辅助：为指定方案 id 构建双拼韵母键集（非双拼返回 None）。
    fn build_shuangpin_finals(&self, schema_id: &str) -> Option<std::collections::HashSet<u8>> {
        self.load_shuangpin_layout(schema_id)
            .map(|lay| lay.final_key_set())
    }

    /// 指定方案当前生效的双拼布局 id；非双拼方案或读不出来返回空串（供设置页）。
    pub fn shuangpin_layout_of(&self, schema_id: &str) -> String {
        self.shuangpin_layout_id_of(schema_id).unwrap_or_default()
    }

    /// 失效双拼布局的全部派生缓存（韵母键集 + 反向表）。两者失效条件相同，收在一处，
    /// 免得漏清反向表后切换布局仍按旧表出编码。
    fn invalidate_shuangpin_caches(&self) {
        *self
            .shuangpin_finals_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (String::new(), None);
        *self
            .shuangpin_reverse_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (String::new(), None);
    }

    /// 双拼编码（`${shuangpin}`）用哪个方案的布局。回退链，与
    /// [`Self::resolve_primary_codetable`] 同构（显式的优先，否则按 `available` 顺序找）：
    ///
    /// | 顺位 | 取谁 |
    /// |---|---|
    /// | 1 | 活跃方案本身是双拼 → 用它 |
    /// | 2 | `schema.primary_pinyin` 是双拼 → 用它 |
    /// | 3 | `available` 里首个双拼方案 |
    /// | — | 都不是 ⇒ `None`（没装双拼方案，无从谈起） |
    ///
    /// 全拼方案下也给值是刻意的：这个功能的主要受众是用全拼打字、正在往双拼迁移的人。
    ///
    /// 读盘（`read_schema` × 最多 available 条），故只在反向表缓存 miss 时走，
    /// 即切换方案后的第一次求值。
    fn shuangpin_hint_schema(&self) -> Option<String> {
        let active = self.active_schema_id();
        if self.shuangpin_layout_id_of(&active).is_some() {
            return Some(active);
        }
        let primary = self
            .primary_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !primary.is_empty() && self.shuangpin_layout_id_of(&primary).is_some() {
            return Some(primary);
        }
        // available 的顺序即用户的偏好顺序 —— 与 `resolve_primary_codetable` 同一判据。
        let available = self
            .available
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        available
            .into_iter()
            .find(|id| self.shuangpin_layout_id_of(id).is_some())
    }

    /// 双拼反向表（惰性建 + 单槽缓存）；没有可用的双拼布局时返回 `None`。
    ///
    /// 建表只发生在切方案后的第一次求值；命中时只是一次比较加一次 `Arc` 克隆。
    fn shuangpin_reverse(&self) -> Option<Arc<crate::pinyin::shuangpin::ShuangpinReverse>> {
        let id = self.active_schema_id();
        {
            let cache = self
                .shuangpin_reverse_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cache.0 == id {
                return cache.1.clone();
            }
        }
        // 布局来源不一定是活跃方案本身（全拼方案会回退到某个双拼方案），见
        // `shuangpin_hint_schema`。缓存仍以活跃方案 id 作键：来源是活跃方案的函数，
        // 活跃方案不变、来源就不会变。
        let table = self
            .shuangpin_hint_schema()
            .and_then(|src| self.load_shuangpin_layout(&src))
            .map(|lay| {
                Arc::new(crate::pinyin::shuangpin::ShuangpinConverter::new(lay).build_reverse())
            });
        *self
            .shuangpin_reverse_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (id, table.clone());
        table
    }

    /// 候选的全拼 `code` + `boundary` → 当前活跃方案的双拼击键串
    /// （`nihao` + `ni|hao` → 小鹤下 `nihc`）。供候选注释的 `${code_schema}`。
    ///
    /// 三种 `None` 都按「这一次不显示」处理：
    /// - 找不到任何双拼布局（回退链见 [`Self::shuangpin_hint_schema`]）；
    /// - `boundary == 0`：模糊音命中与用户手输码的词条切分不可靠，猜错会显示错的编码；
    ///   这道闸由 [`syllables_from_boundary`](crate::pinyin::mixed_abbrev::syllables_from_boundary) 把着；
    /// - 任一音节不在反向表里（半截击键串是错的答案，见 `ShuangpinReverse::encode_all`）。
    ///
    /// 在按键链路上且调用方持有 state 锁：命中无 IO，miss 只解析一份布局 TOML 再建表。
    pub fn shuangpin_code_of(&self, code: &str, boundary: u64) -> Option<String> {
        let syllables = crate::pinyin::mixed_abbrev::syllables_from_boundary(code, boundary)?;
        self.shuangpin_code_of_syllables(&syllables)
    }

    /// 音节序列 → 活跃方案的双拼击键串；非双拼方案、或任一音节查不到返回 `None`。
    ///
    /// 给没有候选身份的路径用（剪贴板反查 cmdbar `dict.rev` 只有裸文本，音节由
    /// [`Self::word_pinyin_syllables`] 按词消歧推断）。与 [`Self::shuangpin_code_of`] 汇到同一张
    /// 反向表，故 `${code_schema}` 在注释与反查两个入口同义。
    pub fn shuangpin_code_of_syllables(&self, syllables: &[&str]) -> Option<String> {
        self.shuangpin_reverse()?.encode_all(syllables)
    }

    /// 拼音方案编码提示:返回主码表中 `text` 实际对应的编码,不存在返回空。用反向索引取
    /// 实际码,不按字生成码(生成码常与码表实际码不一致)。
    /// 返回 `None` 表示反查索引尚未就绪，不是「查不到」——见 [`Self::word_codes_in`]
    /// 的三态说明。展示类调用方 `unwrap_or_default()` 即可。
    /// 含用户层：同长时系统码优先，用户码只在词库没有该长度的码时才出（见 `TextCodeView::hint_code`）。
    pub fn codetable_reverse_hint(&self, text: &str) -> Option<String> {
        let primary = self
            .primary_codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if primary.is_empty() {
            return Some(String::new()); // 没有主码表＝确定没有编码可显示，不是「没就绪」
        }
        let v = self.text_codes(&primary);
        if !v.system_ready() {
            return None;
        }
        Some(v.hint_code(text).map(str::to_string).unwrap_or_default())
    }

    /// 用主拼音方案为词推断带空格的音节码（`行长` → `hang zhang`），供候选注释的注音
    /// 消歧；方案不可加载或推不出返回空。
    ///
    /// 必须走 [`Self::generate_word_pinyin`]（枚举读音组合、取能在词典里查回该词的那个），
    /// 逐字取最常用读音在词组上系统性出错。服务对象是码表/短语等非拼音来源候选（拼音候选
    /// 自带 `code` + `boundary`，更可靠）。仅在模板含 `${pinyin}` 时求值，组合数有
    /// `MAX_READING_COMBOS` 护栏。
    pub fn word_pinyin_syllables(&self, text: &str) -> String {
        let target = {
            let p = self
                .primary_pinyin
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            // 空 = 固定全拼，与 resolve_temp_pinyin_target 同一约定（勿改成扫 available：
            // 那会让调整方案顺序静默改变本函数的结果）。
            if p.is_empty() {
                "pinyin".to_string()
            } else {
                p
            }
        };
        self.generate_word_pinyin(&target, text).unwrap_or_default()
    }

    /// 主码表方案 id（`schema.primary_codetable` 解析后，可空）：`${code_rev}` 反查的码源。
    pub fn primary_codetable_id(&self) -> String {
        self.primary_codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 编码/拆字的来源方案 id:码表方案=自身(其它方案的编码/拆字对本方案无意义);
    /// 混输=其主码表成员;拼音/其他=全局主码表方案。空=无来源(编码段/拆字不显示)。
    /// 按已加载引擎的内存类型判定,不读盘(此路径每次候选推送都会走)。
    pub fn code_source_schema(&self) -> String {
        let active = self.active_schema_id();
        match self.active_engine().map(|e| e.engine_type()) {
            Some(EngineType::CodeTable) => active,
            Some(EngineType::Mixed) => self.mixed_primary_schema(&active).unwrap_or_default(),
            _ => self
                .primary_codetable
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    /// 查 `schema_id` 方案词库中 `text` 的全部实际编码,按码长升序以 `/` 连接
    /// (如 `a/ab/abc`,供悬停 [编码] 段);方案 id 为空或词不在词库返回空——
    /// 不按取码规则生成,生成码常与词库实际码不一致。
    ///
    /// 三态返回：
    /// - `None` —— 反查索引尚未就绪（正在后台构建，或还没人触发构建）。
    /// - `Some("")` —— 索引已就绪，但这个词不在词库里。
    /// - `Some(codes)` —— 就绪且查到。
    ///
    /// 前两者不能混为一谈：加词去重（`handle_addword`）靠它判断系统词库里有没有，把「没就绪」
    /// 当「查不到」会写入重复条目并可能被固化进用户词库。展示类调用方 `unwrap_or_default()` 即可。
    ///
    /// 绝不阻塞。索引没就绪时调用方应后台预热（`ensure_reverse_index_async`）并本次放弃。
    pub fn word_codes_in(&self, schema_id: &str, text: &str) -> Option<String> {
        if schema_id.is_empty() {
            return Some(String::new()); // 没指定方案＝确定无从查起，不是「没就绪」
        }
        Some(
            self.reverse_index_if_ready(schema_id)?
                .codes_of(text)
                .map(|codes| codes.join("/"))
                .unwrap_or_default(),
        )
    }

    /// 「按词查编码」统一入口：系统层 + 用户层的一次快照，不阻塞。
    ///
    /// 用户层过期或缺失时，本次照用旧的（或没有），另起后台重建；系统层没就绪就是 `None`，
    /// 由调用方决定要不要派后台构建（打字链路用 `Coordinator::spawn_index_warm`）。
    /// 设计见 `docs/design/text-code-lookup.md`。
    pub fn text_codes(&self, schema_id: &str) -> crate::text_codes::TextCodeView {
        if schema_id.is_empty() {
            return Default::default();
        }
        let system = self.reverse_index_if_ready(schema_id);
        let user = self.store.as_ref().and_then(|s| {
            crate::text_codes::get_or_refresh(
                &self.user_text,
                s,
                &self.data_schema_id(schema_id),
                self.use_clock_now(),
                self.user_text_built.get().cloned(),
            )
        });
        crate::text_codes::TextCodeView { system, user }
    }

    /// 阻塞地把两层都建好（预热线程 / 测试用，不可进按键链路）。返回是否真的建了东西。
    pub fn prewarm_text_codes(&self, schema_id: &str) -> bool {
        if schema_id.is_empty() {
            return false;
        }
        let built_sys = self.prewarm_reverse_index(schema_id);
        let built_user = self.store.as_ref().is_some_and(|s| {
            crate::text_codes::prewarm(
                &self.user_text,
                s,
                &self.data_schema_id(schema_id),
                self.use_clock_now(),
            )
        });
        built_sys || built_user
    }

    /// 注册「用户层后台重建写回之后」的回调（一次性，重复注册忽略）。
    ///
    /// 用户层缺失 / 过期时查询本次没有用户码、另起后台重建（[`Self::text_codes`]）；没有这个
    /// 回调，建好之后要等用户下一次按键才看得到——闲置清扫释放之后回来的第一键尤其如此。
    /// 回调在重建线程上、出锁之后调；与反查索引后台建成后的重绘（协调器 `spawn_index_warm`）
    /// 同一个意思。
    pub fn set_user_text_built_hook(&self, hook: crate::text_codes::BuiltHook) {
        let _ = self.user_text_built.set(hook);
    }

    /// 清空「按词查编码」的用户层（全部方案槽）。配置不再有消费者时由协调器调，释放内存
    /// （每份约数 MB，见 `docs/design/memory-footprint.md` §4.2）。在建的后台重建写回时
    /// 找不到槽即作罢，不会把刚清掉的又填回去。之后若有运行期消费者（cmdbar `dict.rev`）
    /// 再用到，照常走首次使用时后台重建。
    pub fn clear_user_text(&self) {
        crate::text_codes::clear(&self.user_text);
    }

    /// 诊断：各懒建结构的自报大小与已加载引擎清单（`docs/design/memory-footprint.md` S5）。
    /// 只读：逐个取锁拷出数字，不触发任何构建。
    pub fn memory_report(&self) -> MemoryReport {
        let engines: Vec<(String, Arc<dyn Engine>)> = {
            let g = self.engines.lock().unwrap_or_else(|e| e.into_inner());
            let mut v: Vec<_> = g
                .iter()
                .map(|(k, l)| (k.clone(), l.engine.clone()))
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        let engines = engines
            .into_iter()
            .map(|(id, e)| (id, format!("{:?}", e.engine_type()), e.memory_parts()))
            .collect();
        let reverse_index = {
            let g = self.reverse_index.lock().unwrap_or_else(|e| e.into_inner());
            let mut v: Vec<_> = g
                .iter()
                .map(|(k, i)| (k.clone(), i.heap_bytes(), i.image_bytes()))
                .collect();
            v.sort();
            v
        };
        let single_char = self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(id, m)| {
                let mut bytes = m.capacity() * (size_of::<(char, String)>() + 1);
                for s in m.values() {
                    bytes += s.capacity();
                }
                (id.clone(), bytes, 1 + m.len())
            });
        MemoryReport {
            engines,
            reverse_index,
            user_text: crate::text_codes::report(&self.user_text),
            user_assoc: crate::user_assoc::report(&self.user_assoc),
            single_char,
            user_text_builds: crate::text_codes::BUILD_STATS.snapshot(),
            user_assoc_builds: crate::user_assoc::BUILD_STATS.snapshot(),
            char_pinyin_builds: crate::pinyin::generate::BUILD_STATS.snapshot(),
        }
    }

    /// 用户层当前在内存里的方案份数（诊断 / 测试用）。
    pub fn user_text_loaded(&self) -> usize {
        crate::text_codes::loaded(&self.user_text)
    }

    /// 展示用的「这个词怎么打」：系统层 + 用户层，`/` 连接、码长升序。
    ///
    /// 三态同 [`Self::word_codes_in`]：`None` = 系统层还没就绪。与它的区别只在含用户层——
    /// 加词去重要的是「系统词库里有没有」，那条口径刻意不变，仍用 `word_codes_in`。
    pub fn word_codes_display(&self, schema_id: &str, text: &str) -> Option<String> {
        if schema_id.is_empty() {
            return Some(String::new());
        }
        let v = self.text_codes(schema_id);
        if !v.system_ready() {
            return None;
        }
        Some(v.codes_of(text).join("/"))
    }

    /// 词语联想的词源方案：从哪本词库里捞「以上文为前缀的更长的词」。
    ///
    /// 混输必须解析到主码表成员：混输方案自己没有词库，拿它的 id 建反查索引得到空表，
    /// 联想静默失效。不复用 `code_source_schema`：它的拼音分支返回全局主码表，对编码提示
    /// 对、对联想错（纯拼音方案下会从五笔码表里捞词）。
    pub fn assoc_word_schema(&self) -> String {
        let active = self.active_schema_id();
        match self.active_engine().map(|e| e.engine_type()) {
            Some(EngineType::Mixed) => self
                .mixed_primary_schema(&active)
                .unwrap_or_else(|| active.clone()),
            _ => active,
        }
    }

    /// 词语联想取数：以 `prefix` 开头、且严格更长的词，分档合并、档内按使用重排后
    /// 取前 `limit` 条。返回 (整词, 档内排序分)，顺序即最终顺序（跨档分数量纲不同，
    /// 调用方不得再按分数重排）。
    ///
    /// 分档（同文本按档序先到先得）：
    /// 0. 用户词，排最前；
    /// 1. 常用临时词（`count ≥` [`ASSOC_TEMP_MIN_COUNT`]，建出来之后又用过），属个人档；
    /// 2. 系统词（`schema_id` 词库的反查索引）；
    /// 3. 其余临时词——只补位（自动造词的噪声词新建即 count=1）。
    ///
    /// 档内键 = (`p` 降序, 静态名次)。`p` 取 FREQ 表里该词被正常打字选中的记录
    /// （码表 = 输入码，拼音 = 候选码；码取自用户词 / 临时词记录与反查索引，同词多码合并：
    /// 次数求和、最近取大），打分沿用拼音那套 [`FreqProfile::pinyin_score`]
    /// （`log2(count+1)` × 半衰期衰减）。每档先取 `limit ×` [`ASSOC_POOL_FACTOR`] 条作池子，
    /// 让静态名次靠后但常用的词有机会浮上来。
    ///
    /// FREQ 查询是一次读事务的批量点查（几十个 key），在按键路径上。
    /// 两份索引都不在按键线程上构建：没就绪就这一层这次不出。
    pub fn assoc_prefix_words(
        &self,
        schema_id: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<(String, i32)> {
        if schema_id.is_empty() || prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let pool = limit.saturating_mul(ASSOC_POOL_FACTOR);
        let data_schema = self.data_schema_id(schema_id);
        let user_idx = self.store.as_ref().and_then(|store| {
            crate::user_assoc::get_or_refresh(&self.user_assoc, store, &data_schema)
        });
        // 索引没就绪就这次不联想系统词（不阻塞按键线程）。
        let sys_idx = self.reverse_index_if_ready(schema_id);

        // (档, 静态名次, 词, 查 FREQ 用的码)
        let mut items: Vec<(u8, usize, String, Vec<String>)> = Vec::new();
        let mut push = |tier: u8, rank: usize, text: &str, codes: Vec<String>| {
            if !items.iter().any(|(_, _, t, _)| t == text) {
                items.push((tier, rank, text.to_string(), codes));
            }
        };
        let sys_codes = |t: &str| -> Vec<String> {
            sys_idx
                .as_ref()
                .and_then(|i| i.codes_of(t))
                .map(|l| l.iter().map(str::to_string).collect())
                .unwrap_or_default()
        };
        let with_sys = |code: &str, t: &str| {
            let mut v = sys_codes(t);
            if !v.iter().any(|c| c == code) {
                v.push(code.to_string());
            }
            v
        };
        if let Some(u) = &user_idx {
            for (i, h) in u.user_with_prefix(prefix, pool).into_iter().enumerate() {
                push(0, i, h.text, with_sys(h.code, h.text));
            }
            let personal = |h: &crate::user_assoc::UserHit<'_>| h.count >= ASSOC_TEMP_MIN_COUNT;
            for (i, h) in u
                .temp_with_prefix(prefix, pool, personal)
                .into_iter()
                .enumerate()
            {
                push(1, i, h.text, with_sys(h.code, h.text));
            }
        }
        if let Some(idx) = &sys_idx {
            for (i, (t, _)) in idx.texts_with_prefix(prefix, pool).into_iter().enumerate() {
                push(2, i, t, sys_codes(t));
            }
        }
        if let Some(u) = &user_idx {
            let filler = |h: &crate::user_assoc::UserHit<'_>| h.count < ASSOC_TEMP_MIN_COUNT;
            for (i, h) in u
                .temp_with_prefix(prefix, pool, filler)
                .into_iter()
                .enumerate()
            {
                push(3, i, h.text, with_sys(h.code, h.text));
            }
        }

        // 选词次数只在调频开着时参与排序：`record_always`（只记不调）攒下的次数不得改联想顺序。
        // 开关按活跃方案取，不按 `schema_id`：混输下后者是主码表，而写端与主候选重排读的是
        // 混输自己的折叠值。
        let scores = if self.freq_settings_for(&self.active_schema_id()).enabled {
            self.assoc_freq_scores(&data_schema, &items)
        } else {
            vec![0.0; items.len()]
        };
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by(|&a, &b| {
            items[a]
                .0
                .cmp(&items[b].0)
                .then(scores[b].total_cmp(&scores[a]))
                .then(items[a].1.cmp(&items[b].1))
        });
        order
            .into_iter()
            .take(limit)
            .map(|i| (items[i].2.clone(), scores[i].round() as i32))
            .collect()
    }

    /// 词语联想里这个词还在不在（用户词 / 临时词 / 系统词任一有它）。
    ///
    /// 供联想历史（History）读端过滤：历史表只记「上文 → 词」，删词时不级联清理，
    /// 靠这里把已删掉的词挡在外面。两份索引任一没就绪就放行（判不了宁可照出，
    /// 不能把正常的历史全吞掉）；用户词索引可能略旧（后台重建中），删词后最多多出一轮。
    pub fn assoc_word_known(&self, schema_id: &str, text: &str) -> bool {
        let user = self.store.as_ref().and_then(|store| {
            crate::user_assoc::get_or_refresh(
                &self.user_assoc,
                store,
                &self.data_schema_id(schema_id),
            )
        });
        match (user, self.reverse_index_if_ready(schema_id)) {
            (Some(u), Some(sys)) => u.contains(text) || sys.codes_of(text).is_some(),
            _ => true,
        }
    }

    /// 联想候选的选词次数分（方案 A）：一次批量点查 FREQ，同词多码合并后按拼音衰减打分。
    /// 无 store / 查询失败 ⇒ 全 0（退化为静态序）。
    fn assoc_freq_scores(
        &self,
        data_schema: &str,
        items: &[(u8, usize, String, Vec<String>)],
    ) -> Vec<f64> {
        let Some(store) = &self.store else {
            return vec![0.0; items.len()];
        };
        let keys: Vec<(String, String, String)> = items
            .iter()
            .flat_map(|(_, _, t, codes)| {
                codes
                    .iter()
                    .map(move |c| (data_schema.to_string(), c.clone(), t.clone()))
            })
            .collect();
        let recs = match store.get_freq_batch(&keys) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("联想查词频失败（退化为静态序）: {e}");
                return vec![0.0; items.len()];
            }
        };
        let profile = self.pinyin_freq_profile();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let mut it = recs.into_iter();
        items
            .iter()
            .map(|(_, _, _, codes)| {
                let merged = it.by_ref().take(codes.len()).flatten().fold(
                    wind_store::freq::FreqRecord {
                        count: 0,
                        last_used: 0,
                    },
                    |acc, r| wind_store::freq::FreqRecord {
                        count: acc.count.saturating_add(r.count),
                        last_used: acc.last_used.max(r.last_used),
                    },
                );
                profile.pinyin_score(&merged, now)
            })
            .collect()
    }

    /// 阻塞地建好词语联想的用户词文本索引（预热线程 / 测试用，不可进按键链路）。
    /// 无 store 或已是最新时返回 false。
    pub fn prewarm_user_assoc(&self, schema_id: &str) -> bool {
        match &self.store {
            Some(store) if !schema_id.is_empty() => {
                crate::user_assoc::prewarm(&self.user_assoc, store, &self.data_schema_id(schema_id))
            }
            _ => false,
        }
    }

    /// 已建好的反查索引；没有就返回 `None`，绝不现建。
    ///
    /// 供打字链路上的展示类消费方（候选注释 `${code}`、悬停 [编码] 段、词语联想）使用：
    /// TSF→服务是同步 IPC，在这里等一次秒级构建就是整机卡顿。
    pub fn reverse_index_if_ready(&self, schema_id: &str) -> Option<Arc<ReverseIndex>> {
        self.reverse_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned()
    }

    /// 取 `schema_id` 的反查索引,缺则全量构建并缓存（会阻塞秒级）。
    ///
    /// 内存护栏:只保留「本次请求方 + 主码表 + 在用集合」（见 [`Self::reverse_index_pins`]），
    /// 在用的方案不会互相顶掉反复重建；方案切换的残留索引随下次构建清退。
    ///
    /// 只该在后台预热线程，或用户主动发起、本就预期要等的操作里调用（如加词去重校验——
    /// 那里返回空表会导致重复加词，不能降级）。打字链路一律用 [`Self::reverse_index_if_ready`]。
    ///
    /// 返回 `None` ＝ 本次构建被 build_guard 跳过（见 [`Self::build_reverse_index_for`]）。
    /// 此时不写缓存：否则一张空表会被当成真结果一直用到进程结束。
    ///
    /// 走 `index_build_locks` 单飞：并发调用只有一个真在建，其余等它建完后复查即返回。
    fn reverse_index_for(&self, schema_id: &str) -> Option<Arc<ReverseIndex>> {
        // primary 在 reverse_index 锁外取,避免嵌套锁。
        let primary = self
            .primary_codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // 快路径：已建好直接返回。
        if let Some(m) = self.reverse_index_if_ready(schema_id) {
            return Some(m);
        }
        // 构建不能握 `reverse_index` 锁（秒级、上百 MB），否则查别的方案的线程一起排队。
        // 只握本方案自己的构建锁。
        let lock = self.index_build_lock_for(schema_id);
        let _build = lock.lock().unwrap_or_else(|e| e.into_inner());
        // 抢到锁后复查：等待期间可能已被另一线程建好。
        if let Some(m) = self.reverse_index_if_ready(schema_id) {
            return Some(m);
        }
        if self.reverse_index_skipped(schema_id) {
            return None;
        }
        let Some(idx) = self.build_reverse_index_for(schema_id) else {
            self.reverse_index_skipped
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(schema_id.to_string());
            return None;
        };
        let m = Arc::new(idx);
        let pins = self.reverse_index_pins();
        let mut guard = self.reverse_index.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(schema_id.to_string(), m.clone());
        guard.retain(|k, _| reverse_index_keeps(k, schema_id, &primary, &pins));
        Some(m)
    }

    /// 辅助码当下可能用到的方案来源（`schema:<id>`），去重、保序：活跃方案的
    /// `[engine.aux_code]` ∪ 临拼目标方案的（五笔下从临拼进辅助码时，码表配在拼音方案里）。
    /// 辅助码关闭时对应方案的来源恒空（见 [`Self::aux_code_settings_of`]）。
    ///
    /// 供反查索引的内存护栏钉住它们、以及启动 / 切方案 / 改配置后的后台预热——两处用同一份
    /// 集合，否则「预热了却被护栏顶掉」或「钉住了却没人预热」。
    ///
    /// 临拼目标不走 [`Self::temp_pinyin_target`]：那个函数会 `ensure_loaded` 目标方案
    /// （可能秒级），而本函数在切方案 / 保存设置的路径上也会被调；这里只要 id。
    pub fn aux_code_schemas_in_use(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push_from = |schema_id: &str| {
            for id in self.aux_code_settings_of(schema_id).schema_sources() {
                if !out.iter().any(|s| s == id) {
                    out.push(id.to_string());
                }
            }
        };
        push_from(&self.active_schema_id());
        let enabled = self
            .temp_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enabled;
        let primary = self
            .primary_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(target) =
            Self::resolve_temp_pinyin_target(enabled, self.current_engine_type(), &primary)
        {
            push_from(&target);
        }
        out
    }

    /// 反查索引「在用集合」里除主码表外的方案：联想词方案 + 辅助码在用的方案来源。
    fn reverse_index_pins(&self) -> Vec<String> {
        let mut v = vec![self.assoc_word_schema()];
        v.extend(self.aux_code_schemas_in_use());
        v
    }

    /// 后台预热反查索引：把「首次使用时才建」提前到预热线程。
    ///
    /// 大词库下构建是秒级的，不能落在打字的同步链路上。
    ///
    /// 幂等；返回是否真的得到了索引（供调用方计时/记日志、决定要不要通知重绘）。
    /// 被 build_guard 跳过时返回 `false`，此后直到缓存失效都不再尝试
    /// （见 `reverse_index_skipped`）。
    ///
    /// 单飞在 [`Self::reverse_index_for`] 里（`index_build_locks`）。
    pub fn prewarm_reverse_index(&self, schema_id: &str) -> bool {
        if schema_id.is_empty()
            || self.reverse_index_if_ready(schema_id).is_some()
            || self.reverse_index_skipped(schema_id)
        {
            return false;
        }
        self.reverse_index_for(schema_id).is_some()
    }

    /// 测试用：把方案标成「反查索引已被放弃」，之后不会起后台构建，用来把「未就绪」状态钉死。
    #[doc(hidden)]
    pub fn mark_reverse_index_skipped_for_test(&self, schema_id: &str, skipped: bool) {
        let mut set = self
            .reverse_index_skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if skipped {
            set.insert(schema_id.to_string());
        } else {
            set.remove(schema_id);
        }
    }

    /// 该方案的反查索引本次进程是否已被 build_guard 放弃（直到缓存失效前不再尝试）。
    pub fn reverse_index_skipped(&self, schema_id: &str) -> bool {
        self.reverse_index_skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(schema_id)
    }

    /// 该方案的反查索引是否正在后台构建。打字线路据此决定要不要再起一个后台构建线程，
    /// 免得索引没建好期间每次按键都 spawn 一个。
    pub fn is_building_reverse_index(&self, schema_id: &str) -> bool {
        !schema_id.is_empty()
            && self.reverse_index_if_ready(schema_id).is_none()
            && self.index_build_lock_for(schema_id).try_lock().is_err()
    }

    /// 已加载引擎的影子层（未启用扩展词库）；引擎未加载、或没挂影子层（开关关 / 方案无扩展库）
    /// 时 `None`。只看已加载的、不触发构建——调用方在按键线程上也可能走到这里。
    fn loaded_disabled_dict_engine(&self, schema_id: &str) -> Option<Arc<dyn Engine>> {
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .filter(|l| l.engine.disabled_dict_layers().is_some())
            .map(|l| Arc::clone(&l.engine))
    }

    /// 主码表未启用扩展库里 `text` 的编码（码长升序，已带 `*` 前缀标明未启用）；
    /// 仅剪贴板反查用——注释 / 悬停 / 查重仍只认已启用词库。开关关或无影子层时为空。
    pub fn disabled_dict_codes(&self, text: &str) -> Vec<String> {
        let primary = self
            .primary_codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.loaded_disabled_dict_engine(&primary)
            .and_then(|e| e.disabled_dict_layers().map(|d| d.codes_of(text)))
            .unwrap_or_default()
            .into_iter()
            .map(|c| format!("*{c}"))
            .collect()
    }

    /// 阻塞地把 `schema_id` 已加载引擎的影子层建好（预热线程 / 测试用，不可进按键链路）。
    /// 返回是否真的建了；引擎未加载、没挂影子层或已建好时返回 false。
    pub fn prewarm_disabled_dicts(&self, schema_id: &str) -> bool {
        self.loaded_disabled_dict_engine(schema_id)
            .and_then(|e| e.disabled_dict_layers().map(|d| d.warm()))
            .unwrap_or(false)
    }

    /// 后台预热 `schema_id` 已加载引擎的影子层（未启用扩展词库，reverse-mode spec §4.2）。
    ///
    /// 影子层是懒加载的：没有这一步，首次反查模式查询会在按键线程上（持协调器 state 锁）
    /// 逐库 mmap、缺缓存时还要现建 wdat。
    ///
    /// - 开关关或方案没有扩展库时引擎不挂影子层，一查即返回；只看已加载的引擎，不触发构建。
    /// - 已建好或正有线程在建（`needs_warm` 用 `try_lock` 判）就不再起。
    /// - 加载持影子层内部的锁，按键线程撞上进行中的预热就等它建完复用。
    ///
    /// 调用点：活跃方案引擎（重）建好时（`ensure_loaded`）、活跃方案变更时（`on_active_changed`）、
    /// 热禁用扩展库之后（`set_dict_enabled_live`）。
    fn warm_disabled_dicts_async(&self, schema_id: &str) {
        let Some(engine) = self.loaded_disabled_dict_engine(schema_id) else {
            return;
        };
        if !engine
            .disabled_dict_layers()
            .is_some_and(|d| d.needs_warm())
        {
            return;
        }
        let sid = schema_id.to_string();
        let spawned = std::thread::Builder::new()
            .name("disabled-dicts-warm".into())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                if engine.disabled_dict_layers().is_some_and(|d| d.warm()) {
                    debug!("后台预热未启用扩展词库 {} 用时 {:?}", sid, t0.elapsed());
                }
            });
        if let Err(e) = spawned {
            warn!("无法启动未启用扩展词库预热线程: {e}（首次反查时再加载）");
        }
    }

    /// 影子层建过几遍（测试与诊断用）；引擎未加载或没挂影子层时 `None`。
    pub fn disabled_dicts_load_count(&self, schema_id: &str) -> Option<usize> {
        self.loaded_disabled_dict_engine(schema_id)
            .and_then(|e| e.disabled_dict_layers().map(|d| d.load_count()))
    }

    /// 单字全码表是否已就绪。它与反查索引同为惰性全量构建（同一批词库、同一量级），
    /// 因此同样不能在按键线程上现建。只缓存一份，故判据是「缓存的就是这个方案」。
    pub fn single_char_codes_ready(&self, schema_id: &str) -> bool {
        matches!(
            self.single_char_codes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref(),
            Some((id, _)) if id == schema_id
        )
    }

    /// 后台预热单字全码表（供自动造词取码）。幂等；返回是否真的建了。
    pub fn prewarm_single_char_codes(&self, schema_id: &str) -> bool {
        if schema_id.is_empty() || self.single_char_codes_ready(schema_id) {
            return false;
        }
        let _ = self.single_char_full_codes(schema_id);
        true
    }

    fn index_build_lock_for(&self, schema_id: &str) -> Arc<Mutex<()>> {
        self.index_build_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(schema_id.to_string())
            .or_default()
            .clone()
    }

    /// 反查索引缓存路径：`<cache>/<方案键>/<方案键>.wridx`。
    ///
    /// 整条路径（目录与文件名）只由方案 id 决定，不从主词库路径推：索引内容取决于方案启用的
    /// 整组词库，而共用主库的方案很常见（出厂 `shuangpin` 与 `pinyin`）；且调用点手里的主库
    /// 路径是缓存里的 `.wdat`，没有 `schemas` 段，推出来的命名空间会退化成父目录名。
    ///
    /// 无缓存根时返回 `None` = 本次不落盘，不像 `cache_path` 回退到源文件旁（安装目录常只读，
    /// 索引又是上百 MB 的产物）。
    fn reverse_index_cache_path(schema_id: &str) -> Option<std::path::PathBuf> {
        let dir = CACHE_DIR.get()?.as_ref()?;
        Self::reverse_index_cache_path_in(dir, schema_id)
    }

    /// [`reverse_index_cache_path`](Self::reverse_index_cache_path) 的纯函数内核：缓存根显式
    /// 传入。`CACHE_DIR` 是进程级 `OnceLock`，测试无从改写它，路径形态只能从这里断言。
    fn reverse_index_cache_path_in(
        cache_root: &Path,
        schema_id: &str,
    ) -> Option<std::path::PathBuf> {
        let key = Self::schema_cache_key(schema_id)?;
        Some(cache_root.join(&key).join(format!("{key}.wridx")))
    }

    /// 单字全码表缓存路径：`<cache>/<方案键>/<方案键>.wscc`。
    ///
    /// 与 [`reverse_index_cache_path`](Self::reverse_index_cache_path) 同键同目录、理由相同：
    /// 两者同源于该方案启用的整组词库，一起生、一起废。
    fn single_char_codes_cache_path(schema_id: &str) -> Option<std::path::PathBuf> {
        let dir = CACHE_DIR.get()?.as_ref()?;
        Self::single_char_codes_cache_path_in(dir, schema_id)
    }

    /// 纯函数内核，理由同 [`reverse_index_cache_path_in`](Self::reverse_index_cache_path_in)。
    fn single_char_codes_cache_path_in(
        cache_root: &Path,
        schema_id: &str,
    ) -> Option<std::path::PathBuf> {
        let key = Self::schema_cache_key(schema_id)?;
        Some(cache_root.join(&key).join(format!("{key}.wscc")))
    }

    /// 英文词组分词索引（`.wphr`）的落盘目录：`<cache>/<方案键>/`，文件名由索引自己按
    /// 启用词库集合的指纹取（`phrase-<指纹>.wphr`，见 [`crate::english_phrase::LazyPhraseIndex`]）。
    ///
    /// 与 [`reverse_index_cache_path`](Self::reverse_index_cache_path) 同目录、同键、同理由；
    /// 无缓存根时 `None` = 不落盘。
    fn english_phrase_cache_dir(schema_id: &str) -> Option<std::path::PathBuf> {
        let dir = CACHE_DIR.get()?.as_ref()?;
        Some(dir.join(Self::schema_cache_key(schema_id)?))
    }

    /// 方案 id → 可安全当目录名和文件名用的键；id 为空时 `None`。
    ///
    /// 只保留 ASCII 字母/数字/`-`/`_`，其余换成 `_`；转义真的发生时再附 id 的短哈希，否则
    /// 「五笔」与「拼音」会转成同一串下划线、互相顶掉缓存。ASCII id 不触发哈希。哈希用自带
    /// FNV-1a 而不是 `DefaultHasher`：后者不保证跨 Rust 版本稳定，而这串字符会进路径。
    fn schema_cache_key(schema_id: &str) -> Option<String> {
        let safe: String = schema_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if safe.is_empty() {
            return None;
        }
        if safe == schema_id {
            return Some(safe);
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in schema_id.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Some(format!("{safe}_{h:016x}"))
    }

    /// 各词库缓存产物的摘要列表，用作 `.wridx` 的二级指纹源（顺序即语义）。
    ///
    /// 任一词库处于内存模式（`source_file() == None`，wdat 写失败）就返回 `None`：没有稳定
    /// 的磁盘产物，以它为源的索引不该落盘，否则下次启动无从校验。
    fn reverse_index_source_digests(dicts: &[CachedDict]) -> Option<Vec<String>> {
        dicts
            .iter()
            .map(|d| {
                d.source_file().map(|p| {
                    // 路径一并入哈希：换了词库文件但新文件恰好同摘要（如都取到 absent）时，
                    // 只哈希摘要会认不出来。
                    format!("{}|{}", p.display(), wind_dict::cache_fp::cache_digest(p))
                })
            })
            .collect()
    }

    /// 扫全部已启用方案的启用词库，收出落在 `[start, end]` 码位区间内的字符。
    ///
    /// 供「按类型批量设常用/生僻」圈定范围。只收词库里真实出现过的字符，不按整个 Unicode 块
    /// 展开（否则是一堆读端永远查不到的死记录）。返回的条目数界面必须显示，见 [`RangeScan::entries`]。
    ///
    /// 扫全部方案而非活跃方案：常用字覆盖是全局作用域（键就是一个字，不带方案）。
    /// O(全表)，只在用户手动点菜单时调用，绝不能进按键链路。走 `load_dicts_individually`，
    /// 与反查索引构建同一条路径。
    pub fn scan_chars_in_range(&self, start: u32, end: u32) -> RangeScan {
        let mut scan = RangeScan::default();
        if start > end {
            return scan; // 空区间（如 charblock 的「其它」），不必开词库
        }
        let Some(data_dir) = self.data_dir.as_deref() else {
            return scan;
        };
        let t0 = std::time::Instant::now();
        let schemas_dir = data_dir.join("schemas");
        // 多个方案常共用同一份词库，按源文件去重，否则 `entries` 虚高。`CachedDict::Memory`
        // 没有源文件、去重对它失效，`entries` 会偏高；估影响面偏高比偏低安全，且那是异常路径。
        let mut seen_files = std::collections::HashSet::new();
        for schema_id in self.available_schemas() {
            let Some(schema) =
                Self::read_schema(&schema_id, Some(data_dir), self.override_dir.as_deref())
            else {
                continue;
            };
            for d in &Self::load_dicts_individually(&schema, &schemas_dir) {
                if let Some(p) = d.source_file()
                    && !seen_files.insert(p.to_path_buf())
                {
                    continue;
                }
                d.for_each_entry(&mut |_code, text, _weight| {
                    scan.tally(text, start, end);
                });
            }
        }
        scan.finish();
        // 全表遍历是本功能唯一的重活，留耗时便于排查「点了要等一下」。
        info!(
            "扫描码位区间 U+{:04X}-{:04X}: {} 个字符 / {} 条词条，{} 份词库，耗时 {:?}",
            start,
            end,
            scan.chars.len(),
            scan.entries,
            seen_files.len(),
            t0.elapsed()
        );
        scan
    }

    /// 按方案全量构建反查索引(汉字/词 → 全部编码,码长升序)。读不到方案/词库时返回空表。
    ///
    /// 落盘成 `.wridx` 后由 [`ReverseIndex::open`] 决定常驻还是 mmap（阈值
    /// [`REVERSE_INDEX_RESIDENT_MAX`]），大索引字节不进进程私有内存；不持久化则每次启动仍要
    /// 全量重建，故指纹校验是前提。指纹走二级通道（各词库 `.wdat` 摘要，
    /// [`wind_dict::cache_fp::cache_digest`]），复用命中不必读满 yaml。
    ///
    /// 返回 `None` 仅当构建被 build_guard 跳过（同一份输入已连续死在构建中）：
    /// 这不是「查无此词」，调用方不得把它当空表缓存或使用。
    fn build_reverse_index_for(&self, schema_id: &str) -> Option<ReverseIndex> {
        #[cfg(test)]
        self.reverse_index_build_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(data_dir) = self.data_dir.as_deref() else {
            return Some(ReverseIndex::default());
        };
        let schemas = data_dir.join("schemas");
        let Some(schema) = Self::read_schema_in(
            schema_id,
            Some(data_dir),
            self.override_dir.as_deref(),
            self.user_text_dir(),
        ) else {
            return Some(ReverseIndex::default());
        };
        let dicts = Self::load_dicts_individually(&schema, &schemas);
        if dicts.is_empty() {
            return Some(ReverseIndex::default());
        }
        // 顺手清掉旧版留下的合并缓存（本方案已不再需要它）。
        Self::purge_legacy_combined(&schema, &schemas);

        // 索引落盘与否只看缓存根；「某本词库处于内存模式」由下面的 digests 兜住（查全部词库）。
        let cache = Self::reverse_index_cache_path(schema_id);
        let digests = Self::reverse_index_source_digests(&dicts);

        // ① 复用：词库一个没变就直接开盘上的那份，连构建都不发生。
        if let (Some(c), Some(dg)) = (cache.as_deref(), digests.as_deref())
            && wind_dict::cache_fp::derived_cache_is_fresh(
                c,
                dg,
                wind_dict::cache_fp::REVERSE_INDEX_TAG,
            )
        {
            match ReverseIndex::open(c, REVERSE_INDEX_RESIDENT_MAX) {
                Ok(idx) => {
                    info!(
                        "Reused reverse index cache: {} ({} texts, {} dicts, {:.1} MB, 常驻 {:.1} MB)",
                        schema_id,
                        idx.len(),
                        dicts.len(),
                        idx.data_bytes() as f64 / 1024.0 / 1024.0,
                        idx.resident_bytes() as f64 / 1024.0 / 1024.0,
                    );
                    return Some(idx);
                }
                // 指纹说新鲜但打不开＝文件被截断/损坏。重建，但要留痕（「每次启动都慢」的线索）。
                Err(e) => warn!("反查索引缓存 {} 打不开（{e}），重建", c.display()),
            }
        }

        // ② 重建。峰值远高于成品，登记尝试以便连续死在这里时下次跳过（见 build_guard）。
        //    无缓存路径时无处记录，照旧直接建。
        let attempt = match (cache.as_deref(), digests.as_deref()) {
            (Some(c), Some(dg)) => match wind_dict::build_guard::begin(
                c,
                &wind_dict::cache_fp::derived_build_key(dg, wind_dict::cache_fp::REVERSE_INDEX_TAG),
            ) {
                wind_dict::build_guard::Gate::Proceed(a) => Some(a),
                wind_dict::build_guard::Gate::Skip { reason } => {
                    error!("方案 {schema_id} 的反查索引本次不可用：{reason}");
                    return None;
                }
            },
            _ => None,
        };
        let t0 = std::time::Instant::now();
        let image = wind_dict::cached::serialize_reverse_index_from(&dicts);
        let built = t0.elapsed();
        if let Some(a) = attempt {
            a.finish();
        }

        // ③ 落盘后从盘上重新打开——这一步才真正把索引字节移出进程私有内存。
        //    任一环失败都只是退回「常驻内存」这个旧行为，不影响正确性。
        if let (Some(c), Some(dg)) = (cache.as_deref(), digests.as_deref()) {
            match wind_dict::reverseidx::write_wridx(c, &image) {
                Ok(()) => {
                    wind_dict::cache_fp::write_derived_cache_fp(
                        c,
                        dg,
                        wind_dict::cache_fp::REVERSE_INDEX_TAG,
                    );
                    match ReverseIndex::open(c, REVERSE_INDEX_RESIDENT_MAX) {
                        Ok(idx) => {
                            info!(
                                "Built reverse index: {} ({} texts, {} dicts, {:.1} MB, 常驻 {:.1} MB, {:?})",
                                schema_id,
                                idx.len(),
                                dicts.len(),
                                idx.data_bytes() as f64 / 1024.0 / 1024.0,
                                idx.resident_bytes() as f64 / 1024.0 / 1024.0,
                                built
                            );
                            return Some(idx);
                        }
                        Err(e) => warn!("刚写好的反查索引 {} 打不开（{e}）", c.display()),
                    }
                }
                // Windows 上最常见的原因是旧文件仍被本进程映射着（rename 会 Access Denied）。
                Err(e) => warn!(
                    "反查索引写盘失败 {}（{e}）——本次退化为常驻内存，下次启动仍需重建",
                    c.display()
                ),
            }
        }
        let idx = ReverseIndex::from_bytes(image);
        info!(
            "Built reverse index (未落盘，常驻内存): {} ({} texts, {} dicts, {:.1} MB, {:?})",
            schema_id,
            idx.len(),
            dicts.len(),
            idx.data_bytes() as f64 / 1024.0 / 1024.0,
            built
        );
        Some(idx)
    }

    /// 取 `schema_id` 的单字全码表，缺则构建并缓存（只留一份，见字段注释）。
    fn single_char_full_codes(&self, schema_id: &str) -> Arc<HashMap<char, String>> {
        self.single_char_used
            .store(self.use_clock_now(), std::sync::atomic::Ordering::Relaxed);
        {
            let guard = self
                .single_char_codes
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some((id, m)) = guard.as_ref()
                && id == schema_id
            {
                return m.clone();
            }
        }
        let m = Arc::new(self.build_single_char_codes_for(schema_id));
        *self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((schema_id.to_string(), m.clone()));
        m
    }

    /// 按方案构建单字全码表。上限闸取该方案的 `engine.codetable.max_code_length`
    /// （0 = 不设闸，非定长码方案退化为纯「最长优先」）。读不到方案/词库时返回空表。
    fn build_single_char_codes_for(&self, schema_id: &str) -> HashMap<char, String> {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return HashMap::new();
        };
        let Some(schema) = Self::read_schema_in(
            schema_id,
            Some(data_dir),
            self.override_dir.as_deref(),
            self.user_text_dir(),
        ) else {
            return HashMap::new();
        };
        let cap = schema.engine.codetable.max_code_length;
        let dicts = Self::load_dicts_individually(&schema, &data_dir.join("schemas"));
        if dicts.is_empty() {
            return HashMap::new();
        }
        let cache = Self::single_char_codes_cache_path(schema_id);
        let digests = Self::single_char_codes_source_digests(&dicts, cap);

        // ① 复用：词库与 cap 都没变就直接读盘上那份，连扫描都不发生。
        if let (Some(c), Some(dg)) = (cache.as_deref(), digests.as_deref())
            && wind_dict::cache_fp::derived_cache_is_fresh(
                c,
                dg,
                wind_dict::cache_fp::SINGLE_CHAR_CODES_TAG,
            )
        {
            match wind_dict::charcodes::read(c) {
                Ok(m) => {
                    info!(
                        "Reused single-char full-code cache: {} ({} chars, cap={}, {} dicts)",
                        schema_id,
                        m.len(),
                        cap,
                        dicts.len()
                    );
                    return m;
                }
                // 指纹说新鲜但读不出＝文件被截断/损坏。重建，但要留痕（同反查索引那条）。
                Err(e) => warn!("单字全码表缓存 {} 读不出（{e}），重建", c.display()),
            }
        }

        // ② 重建。
        let t0 = std::time::Instant::now();
        let idx = wind_dict::cached::build_single_char_full_codes_from(&dicts, cap);
        let built = t0.elapsed();

        // ③ 落盘。不像反查索引那样写完再从盘打开：这张表是 MB 以内的常驻 HashMap，重读
        //    只白花解析时间。写失败也只是下次启动再建一遍。
        if let (Some(c), Some(dg)) = (cache.as_deref(), digests.as_deref()) {
            match wind_dict::charcodes::write(c, &wind_dict::charcodes::serialize(&idx)) {
                Ok(()) => wind_dict::cache_fp::write_derived_cache_fp(
                    c,
                    dg,
                    wind_dict::cache_fp::SINGLE_CHAR_CODES_TAG,
                ),
                // Windows 上最常见的原因是目标仍被占用（rename 会 Access Denied）。
                Err(e) => warn!(
                    "单字全码表写盘失败 {}（{e}）——本次照常可用，下次启动仍需重建",
                    c.display()
                ),
            }
        }
        info!(
            "Built single-char full-code table: {} ({} chars, cap={}, {} dicts, {:?})",
            schema_id,
            idx.len(),
            cap,
            dicts.len(),
            built
        );
        idx
    }

    /// `.wscc` 的指纹源：各词库摘要（同 `.wridx`）再加上 `max_code_length`。
    ///
    /// 单字全码表用 cap 当闸筛掉超长码，反查索引与 cap 无关；漏了 cap，改码长设置而词库
    /// 未动时会永久复用按旧 cap 筛出的表。
    fn single_char_codes_source_digests(dicts: &[CachedDict], cap: usize) -> Option<Vec<String>> {
        let mut dg = Self::reverse_index_source_digests(dicts)?;
        dg.push(format!("cap|{cap}"));
        Some(dg)
    }

    /// 按方案的 `[[encoder.rules]]` 为词计算码表词组编码（造词/加词统一入口）。
    ///
    /// 单字全码取自该方案的码表词库：码源必须与词库同源，造出来的词才打得出来（拆字表之类
    /// 的静态资源在用户换词库后可能造出打不出的码）。
    ///
    /// 文本口径见 [`encoder::encode_text`]：单字直取全码、不进词组公式；非汉字跳过
    /// （姓名+手机号只按姓名取码）；取不到码的汉字仍整词失败（见 [`encoder::EncodeError`]）。
    /// 与 [`Self::encode_words`]（设置页 / 导入用）必须同口径。
    pub fn encode_word(&self, schema_id: &str, word: &str) -> Result<String, encoder::EncodeError> {
        self.encode_word_with_hints(schema_id, word, &[])
    }

    /// 同 [`Self::encode_word`]，但优先按用户实际打的码取位（自动造词用）。
    ///
    /// `hints` 是「字 → 用户上屏它时选中的那条码」。单字全码表对多读音/多拆法的字只能按权重
    /// 挑一条，与用户打的码可能对不上，造出的词就召不回。有提示的字经 [`encoder::pick_hinted_code`] 在该字的全部码（反查
    /// 索引）里挑以提示开头、不超码长上限、且够公式取位的一条；挑不出（提示不是本方案的码，
    /// 或反查索引没就绪）回退全码表。无提示的字行为与 [`Self::encode_word`] 完全一致。
    pub fn encode_word_with_hints(
        &self,
        schema_id: &str,
        word: &str,
        hints: &[(char, String)],
    ) -> Result<String, encoder::EncodeError> {
        let schema = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        );
        let cap = schema
            .as_ref()
            .map_or(0, |s| s.engine.codetable.max_code_length);
        let spec = schema.and_then(|s| s.encoder).unwrap_or_default();
        let codes = self.single_char_full_codes(schema_id);
        // 反查索引只在真有提示时才取（不阻塞：没就绪就当没有提示）。
        let index = if hints.is_empty() {
            None
        } else {
            self.reverse_index_if_ready(schema_id)
        };
        encoder::encode_text(word, &spec, |c, need| {
            let full = codes.get(&c);
            if let Some(idx) = index.as_deref()
                && let Some((_, hint)) = hints.iter().find(|(h, _)| *h == c)
            {
                let key = c.to_string();
                let all = idx.codes_of(&key).into_iter().flat_map(|l| l.iter());
                if let Some(code) =
                    encoder::pick_hinted_code(hint, full.map(String::as_str), all, cap, need)
                {
                    return Some(code);
                }
            }
            full.cloned()
        })
    }

    /// 批量版 [`Self::encode_word`]：规则完全一致，但方案只读一次（`read_schema` 没有缓存，
    /// 逐词调在万级词表上就是万次文件解析）。
    ///
    /// 返回与 `words` 同序等长；取不到码的位置为空串——调用方靠下标把码配回词，
    /// 跳过失败项会让其后所有词错位配到别人的码上。
    /// 文本口径（单字直取、非汉字跳过）与单条版同一个 [`encoder::encode_text`]。
    pub fn encode_words(&self, schema_id: &str, words: &[&str]) -> Vec<String> {
        let spec = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
        .and_then(|s| s.encoder)
        .unwrap_or_default();
        let codes = self.single_char_full_codes(schema_id);
        let mut failed = 0usize;
        let out: Vec<String> = words
            .iter()
            .map(
                |w| match encoder::encode_text(w, &spec, |c, _| codes.get(&c).cloned()) {
                    Ok(code) => code,
                    Err(_) => {
                        failed += 1;
                        String::new()
                    }
                },
            )
            .collect();
        // 逐条打日志在万级批量下反而淹没有用信息，只汇总一行。
        if failed > 0 {
            tracing::debug!(
                "encode_words({}): {}/{} 个词取不到码",
                schema_id,
                failed,
                words.len()
            );
        }
        out
    }

    /// 解析主码表方案 id:config 显式指定优先;否则取 available 中首个 codetable 类型方案;都无返回空。
    fn resolve_primary_codetable(
        cfg_primary: &str,
        available: &[String],
        data_dir: Option<&Path>,
        override_dir: Option<&Path>,
    ) -> String {
        if !cfg_primary.is_empty() {
            return cfg_primary.to_string();
        }
        for id in available {
            if Self::read_schema(id, data_dir, override_dir)
                .map(|s| s.engine.engine_type.eq_ignore_ascii_case("codetable"))
                .unwrap_or(false)
            {
                return id.clone();
            }
        }
        String::new()
    }

    /// 解析活跃方案 id，并处理「`schema.active` 指向被定制版 hide 掉的方案」。
    ///
    /// 原版用户升级到定制版时，`schema.active` 可能指向已被删的方案，不处理就一个字都打不出。
    ///
    /// 降级只在内存里发生，绝不改写用户配置：定制层可被卸载，改写会让用户回到原版后丢掉
    /// 自己的选择（同契约 3「程序永不写 `data_custom`」）。
    ///
    /// 候选次序：先 `schema.available`（用户配的顺序），再各层 `schemas/` 目录（层序
    /// `user > custom > data`，见 [`Self::scan_layer_schema_ids`]），去重后按
    /// [`Self::active_fallback_rank`] 稳定排序取第一个。都没有则原样返回，让上游照旧报
    /// 「方案文件不存在」。判据必须是排序 + 类型，不能只看「受支持」或 `hidden`（英文方案
    /// 没标 hidden，会抢到首位）。
    fn resolve_active_schema_id(
        config: &Config,
        data_dir: Option<&Path>,
        override_dir: Option<&Path>,
    ) -> String {
        let active = config.active_schema().to_string();
        if !Config::custom_hides_schema(&active) {
            return active;
        }
        // 候选源：用户启用列表在前（他自己排过的顺序有意义），层序扫描在后。
        let mut cands: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let from_scan = data_dir
            .map(Self::scan_layer_schema_ids)
            .unwrap_or_default();
        for id in config.schema.available.iter().cloned().chain(from_scan) {
            if !Config::custom_hides_schema(&id) && seen.insert(id.clone()) {
                cands.push(id);
            }
        }
        // 先算好每个候选的档位（`None` = 不能当活跃方案），再稳定排序：同档位内
        // 上面那个「available 顺序 → 层序」原样保留，不被排序打乱。
        let mut ranked: Vec<(u8, String)> = cands
            .into_iter()
            .filter_map(|id| {
                Self::active_fallback_rank(&id, data_dir, override_dir).map(|r| (r, id))
            })
            .collect();
        ranked.sort_by_key(|(r, _)| *r);

        match ranked.into_iter().next() {
            Some((_, f)) => {
                warn!(
                    "活跃方案 {} 已被定制版清单 [schemas] hide，本次降级到 {}（用户配置未改写）",
                    active, f
                );
                f
            }
            None => {
                warn!(
                    "活跃方案 {} 已被定制版清单 [schemas] hide，且找不到任何可用方案可降级",
                    active
                );
                active
            }
        }
    }

    /// 降级落点必须出现在 `available` 里（构造与 `reload_from_config` 共用）。
    ///
    /// 不补的话单方案用户碰上定制版删掉该方案时，`retain` 的两条判据双双落空，`available`
    /// 变成空表（方案菜单空白、循环切换无反应）。只在确实降级过（`fallback != cfg_active`）
    /// 时补：「active 本来就不在 available 里」是合法状态，`cycle_schema` 专门处理过它。
    fn ensure_fallback_listed(available: &mut Vec<String>, cfg_active: &str, fallback: &str) {
        if fallback != cfg_active && !available.iter().any(|s| s == fallback) {
            // 插到表头：它就是此刻的活跃方案。
            available.insert(0, fallback.to_string());
        }
    }

    /// 一个方案作为降级落点的档位：`None` = 不能当活跃方案，`Some(r)` 越小越优先。
    ///
    /// 1. 读得出来且 `is_supported()`（`read_schema` 已顺带拦掉被 hide 的）；
    /// 2. 排除 `[overlay]` 方案：那是引导键瞬态进入的特殊模式，当常驻活跃方案等于打不了字；
    /// 3. english 排到最后而不是排除：盘上只剩它时，能打英文仍好过没有。
    fn active_fallback_rank(
        schema_id: &str,
        data_dir: Option<&Path>,
        override_dir: Option<&Path>,
    ) -> Option<u8> {
        let s = Self::read_schema(schema_id, data_dir, override_dir)?;
        if !s.is_supported() || s.overlay.is_some() {
            return None;
        }
        Some(u8::from(
            s.engine.engine_type.eq_ignore_ascii_case("english"),
        ))
    }

    /// 读取 schema 判断是否受支持（不构建引擎，仅解析 TOML）
    fn schema_supported(
        schema_id: &str,
        data_dir: Option<&Path>,
        override_dir: Option<&Path>,
    ) -> bool {
        match Self::read_schema(schema_id, data_dir, override_dir) {
            Some(s) => s.is_supported(),
            None => false,
        }
    }

    /// 读取 schema 的隐藏标志（[schema].hidden）：隐藏方案不在设置页「方案管理」列出。
    fn schema_hidden(
        schema_id: &str,
        data_dir: Option<&Path>,
        override_dir: Option<&Path>,
    ) -> bool {
        Self::read_schema(schema_id, data_dir, override_dir)
            .map(|s| s.schema.hidden)
            .unwrap_or(false)
    }

    /// 某方案是否标了 `[schema].hidden`（英文、快符这类不进常规方案列表的方案）。
    ///
    /// 供展示层过滤——[`Self::installed_schemas`] 返回全集，"设置页列不列出它"
    /// 是调用方的决定，不是"装没装"的一部分。
    pub fn schema_is_hidden(&self, schema_id: &str) -> bool {
        Self::schema_hidden(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
    }

    /// 某方案引擎是否已加载（已就绪，切换即时无构建）。
    pub fn is_loaded(&self, schema_id: &str) -> bool {
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(schema_id)
    }

    /// 某方案是否正在后台构建（未加载且构建锁被占）。供 UI 显示「准备中」用。
    pub fn is_building(&self, schema_id: &str) -> bool {
        if self.is_loaded(schema_id) {
            return false;
        }
        let lock = self
            .build_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned();
        // 锁存在且被占 = 有线程正在构建该方案。
        matches!(lock, Some(l) if l.try_lock().is_err())
    }

    /// 后台预热：构建某方案的引擎与缓存（阻塞，供后台线程调用）。返回是否成功。
    /// 与首次切换共享 single-flight 构建锁，竞争时只构建一次。
    pub fn prewarm_schema(&self, schema_id: &str) -> bool {
        self.ensure_loaded(schema_id)
    }

    /// 已加载就把「最后使用」记成当前淘汰时钟并返回 true；未加载返回 false。
    ///
    /// 记录与查表在同一把 `engines` 锁下（淘汰也持这把锁读「最后使用」再摘），故不会出现
    /// 「刚确认在表里、随即被摘」。
    fn touch_loaded(&self, schema_id: &str) -> bool {
        let g = self.engines.lock().unwrap_or_else(|e| e.into_inner());
        match g.get(schema_id) {
            Some(l) => {
                l.last_used.store(
                    self.use_clock.load(std::sync::atomic::Ordering::Relaxed),
                    std::sync::atomic::Ordering::Relaxed,
                );
                true
            }
            None => false,
        }
    }

    /// 淘汰时钟的当前读数（不推进）。
    fn use_clock_now(&self) -> u64 {
        self.use_clock.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 已加载引擎（不触发构建、不记使用）。
    fn loaded_engine(&self, schema_id: &str) -> Option<Arc<dyn Engine>> {
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .map(|l| Arc::clone(&l.engine))
    }

    /// 已加载引擎的方案 id（升序）。诊断与测试用。
    pub fn loaded_schemas(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }

    /// 方案常驻策略（`schema.keep_all_loaded = false`）下不摘的方案：
    ///
    /// - 当前方案；
    /// - 临拼目标（开关开着、当前方案适用时；按 id 判，不触发加载）；
    /// - `english`——临英候选开着，或共享英文引擎已被取走（别人握着同一个 `Arc`，摘了省不下
    ///   内存，下次还会再建一份）。
    ///
    /// 混输的成员不保护：混输在 `build_engine` 里自建成员子引擎，用不到表里那份。
    ///
    /// 启动常驻集合与闲置淘汰的保护名单是同一份，两处判据不会漂移。
    /// 正在构建的方案还不在 `engines` 表里，天然不会被摘。
    pub fn residency_protected(&self) -> std::collections::HashSet<String> {
        let active = self.active_schema_id();
        let mut keep = std::collections::HashSet::new();
        let temp_pinyin_on = self
            .temp_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enabled;
        let primary_pinyin = self
            .primary_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(t) = Self::resolve_temp_pinyin_target(
            temp_pinyin_on,
            self.loaded_engine_type(&active),
            &primary_pinyin,
        ) {
            keep.insert(t);
        }
        let temp_english_on = self
            .temp_english
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .show_candidates;
        let english_shared = matches!(
            *self
                .shared_english_engine
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            Some(Some(_))
        );
        if temp_english_on || english_shared {
            keep.insert(ENGLISH_SCHEMA.to_string());
        }
        keep.insert(active);
        keep
    }

    /// 闲置淘汰（方案常驻策略关时由协调器的清扫拍子调）：先推进淘汰时钟，再把
    /// 「`idle` 以上没用过、且不在 [`Self::residency_protected`] 里」的引擎从表里摘掉，
    /// 返回被摘的方案 id。`idle = 0` 即「保护名单外的一律摘」（运行期把常驻关掉时用）。
    ///
    /// 只摘引擎与只属于它的派生索引（[`Self::forget_unloaded`]），不动方案配置缓存：
    /// 配置没变，再切回来时按原样重建即可（派生缓存文件仍新鲜 ⇒ 只是 mmap + 建外壳）。
    pub fn evict_idle(&self, idle: std::time::Duration) -> Vec<String> {
        let now = self.advance_use_clock();
        let idle_ms = idle.as_millis() as u64;
        let keep = self.residency_protected();
        let mut dropped: Vec<(String, LoadedEngine)> = Vec::new();
        {
            let mut g = self.engines.lock().unwrap_or_else(|e| e.into_inner());
            let stale: Vec<String> = g
                .iter()
                .filter(|(id, l)| {
                    !keep.contains(id.as_str())
                        && now
                            .saturating_sub(l.last_used.load(std::sync::atomic::Ordering::Relaxed))
                            >= idle_ms
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in stale {
                if let Some(l) = g.remove(&id) {
                    dropped.push((id, l));
                }
            }
        }
        // 出锁再放引擎（解映射、释放外壳不该挡住取引擎的按键线程）。
        let ids: Vec<String> = dropped.into_iter().map(|(id, _)| id).collect();
        for id in &ids {
            self.forget_unloaded(id);
        }
        if !ids.is_empty() {
            info!("闲置淘汰方案引擎：{:?}", ids);
        }
        ids
    }

    /// 推进淘汰时钟到当前时刻并返回读数（只在清扫拍子里调，见 [`Self::use_clock`]）。
    fn advance_use_clock(&self) -> u64 {
        let now = self.clock_origin.elapsed().as_millis() as u64;
        self.use_clock
            .fetch_max(now, std::sync::atomic::Ordering::Relaxed);
        now
    }

    /// 懒建缓存的闲置清扫（`docs/design/memory-footprint.md` §7；协调器清扫拍子调，与方案
    /// 常驻策略无关、恒生效）：先推进淘汰时钟，再释放 `idle` 以上没用过的
    ///
    /// - 单字全码表。各消费方下次用到时就地重建（`.wscc` 新鲜时只是读一次盘），不丢活。
    /// - 按词查编码用户层的槽。释放后下一次查询没有用户层（只出系统层的码），同时起后台重建。
    ///
    /// 「最后使用」与引擎同一套打法：取用处一次原子写 / 已持锁时顺手写时钟读数。
    /// 返回释放了的项（`single_char:<方案>` / `user_text:<数据方案>`），供日志与测试。
    pub fn evict_idle_caches(&self, idle: std::time::Duration) -> Vec<String> {
        let now = self.advance_use_clock();
        let idle_ms = idle.as_millis() as u64;
        let mut released = Vec::new();
        // 摘下的表出锁再析构（2 万多个小块），别让取表的造词 / 加词路径陪等。
        let taken = {
            let mut sc = self
                .single_char_codes
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let used = self
                .single_char_used
                .load(std::sync::atomic::Ordering::Relaxed);
            if now.saturating_sub(used) >= idle_ms {
                sc.take()
            } else {
                None
            }
        };
        if let Some((id, table)) = taken {
            drop(table);
            released.push(format!("single_char:{id}"));
        }
        released.extend(
            crate::text_codes::evict_idle(&self.user_text, now, idle_ms)
                .into_iter()
                .map(|k| format!("user_text:{k}")),
        );
        if !released.is_empty() {
            info!("闲置释放懒建缓存：{:?}", released);
        }
        released
    }

    /// 引擎被摘后，清掉只属于它的派生索引。
    ///
    /// 摘引擎不改变方案定义，别的方案的索引照样有效（与 [`Self::invalidate_schema`] 不同）。
    /// 被摘方案自己的索引也要先过「在用」判据（同 `reverse_index_for` 的护栏）：例如拼音方案的
    /// 编码提示查的是主码表的反查索引，与主码表引擎在不在内存无关。
    fn forget_unloaded(&self, schema_id: &str) {
        let primary = self.primary_codetable_id();
        let mut in_use = self.reverse_index_pins();
        in_use.push(self.code_source_schema());
        let in_use_kept = reverse_index_keeps(schema_id, "", &primary, &in_use);
        if !in_use_kept {
            let removed = self
                .reverse_index
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(schema_id)
                .is_some();
            if removed {
                self.reverse_index_skipped
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(schema_id);
            }
        }
        // 单字全码表跟反查索引同一个「在用」判据：自动造词取码对的是编码来源方案
        // （混输则其主码表成员），与那个方案的独立引擎在不在内存无关。
        if in_use_kept {
            return;
        }
        // 按词查编码用户层：槽按数据方案分（拼音类方案共用 `pinyin` 一份），
        // 还有已加载引擎或在用方案落在同一数据方案上就留着。
        let data = self.data_schema_id(schema_id);
        let shared = {
            let loaded: Vec<String> = self
                .engines
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .cloned()
                .collect();
            loaded
                .iter()
                .chain(in_use.iter())
                .chain(std::iter::once(&primary))
                .any(|id| !id.is_empty() && self.data_schema_id(id) == data)
        };
        if !shared {
            crate::text_codes::remove(&self.user_text, &data);
        }
        let mut sc = self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if sc.as_ref().is_some_and(|(id, _)| id == schema_id) {
            *sc = None;
        }
    }

    /// 共享英文引擎缓存放手（[`Self::shared_english_engine`] 字段），条件是眼下没有消费者：
    /// 临英候选关着、且表里没有混输（混输握着同一个 `Arc`，放手会让下次再建一份）。
    /// 英文候选混入不算消费者——它每次现取，缓存空了会重新取回表里那一份。
    ///
    /// 放手后英文不再落进 [`Self::residency_protected`]，闲置清扫才能摘它。启动缓存校验
    /// 之后调（校验建混输时把英文取走了）。返回是否真的放了手。
    pub fn release_shared_english_if_unused(&self) -> bool {
        let temp_english_on = self
            .temp_english
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .show_candidates;
        let mixed_loaded = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|l| l.engine.engine_type() == EngineType::Mixed);
        if temp_english_on || mixed_loaded {
            return false;
        }
        self.shared_english_engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .is_some()
    }

    /// 校验某方案的派生缓存（`.wdat` 等，指纹不符或缺失即重建），建完不留引擎
    /// （除非它在 [`Self::residency_protected`] 里）。阻塞，只可在后台线程里调。
    ///
    /// 走与切换相同的 `ensure_loaded` 构建路径，不另抽「只确保缓存」的入口：缓存种类与指纹
    /// 判据都长在 `build_engine` 分支里，另写一份必然漂移。代价是多建一次引擎外壳。
    ///
    /// 已加载的方案直接返回 true（它的缓存在加载时已校验过）。返回是否构建成功。
    pub fn refresh_schema_cache(&self, schema_id: &str) -> bool {
        self.refresh_schema_cache_hooked(schema_id, || {})
    }

    /// [`Self::refresh_schema_cache`] 的测试缝：`after_build` 在「建好」与「摘除」之间跑，
    /// 用来确定性地复现「校验刚建完、用户恰好切过去」的竞态。生产只走上面那个入口。
    #[doc(hidden)]
    pub fn refresh_schema_cache_hooked(&self, schema_id: &str, after_build: impl FnOnce()) -> bool {
        if self.is_loaded(schema_id) {
            return true;
        }
        if !self.ensure_loaded(schema_id) {
            return false;
        }
        // 建完打一个「没人碰过」的记号：此后任何取引擎（`touch_loaded`）都会把它覆盖成
        // 时钟读数。不能拿时钟读数本身比对——它只在清扫拍子里推进，建完与随后的使用
        // 很可能读到同一个值。
        if let Some(l) = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
        {
            l.last_used
                .store(UNTOUCHED, std::sync::atomic::Ordering::Relaxed);
        }
        after_build();
        let protected = self.residency_protected().contains(schema_id);
        let dropped = {
            let mut g = self.engines.lock().unwrap_or_else(|e| e.into_inner());
            let untouched = g.get(schema_id).is_some_and(|l| {
                l.last_used.load(std::sync::atomic::Ordering::Relaxed) == UNTOUCHED
            });
            if untouched && !protected {
                g.remove(schema_id)
            } else {
                // 被用过（含切成了当前方案：切换先经 `ensure_loaded`）或受保护 ⇒ 留下；
                // 记号换回时钟读数，否则它永远不算闲置。
                if untouched && let Some(l) = g.get(schema_id) {
                    l.last_used
                        .store(self.use_clock_now(), std::sync::atomic::Ordering::Relaxed);
                }
                None
            }
        };
        drop(dropped);
        true
    }

    /// 跑一次 [`Self::before_publish_hook`]（拷出来、放锁后调：钩子里允许再取引擎，
    /// 也允许别的线程同时进钩子）。
    #[cfg(test)]
    fn run_before_publish_hook(&self, stage: &str) {
        let hook = self
            .before_publish_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(h) = hook {
            h(self, stage);
        }
    }

    /// 取引擎的公共入口：已加载就记一笔「用过」（闲置淘汰的判据），否则构建。
    ///
    /// 取引擎的路径（`engine_for` / `convert_with*` / `enumerate_with` / …）都先过这里，
    /// 故「最后使用」只在这一处打（[`Self::touch_loaded`]，不读系统时钟、不多拿锁）。
    fn ensure_loaded(&self, schema_id: &str) -> bool {
        if self.touch_loaded(schema_id) {
            return true;
        }
        // single-flight：取该方案的专用构建锁（不同方案各自一把，可并行构建）。
        let build_lock = {
            let mut locks = self.build_locks.lock().unwrap_or_else(|e| e.into_inner());
            locks.entry(schema_id.to_string()).or_default().clone()
        };
        let _build_guard = build_lock.lock().unwrap_or_else(|e| e.into_inner());
        // 构建期间被失效（`engines_gen` 变了）就丢掉这次结果、按新配置再建一轮，而不是返回
        // 「建好但没登记」：调用方一律 `ensure_loaded` 后再 `loaded_engine` 取，取不到就是
        // 丢一次按键 / 切方案失败。每轮重来都要求期间又发生一次失效，不会空转。
        loop {
            // 抢到锁后复查：等待期间可能已被另一线程（预热/切换）构建完成。
            if self.touch_loaded(schema_id) {
                return true;
            }
            // 先记代次、后读配置：读到的配置不会比代次旧。
            let generation = self.engines_gen.load(std::sync::atomic::Ordering::Acquire);
            let codetable_cfg = self
                .codetable
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let mix_cfg = self.mix.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let pinyin_cfg = self
                .pinyin
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let correction = self
                .correction
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let misread = self.misread_for_engines();
            // 词组分词：英文方案读 `schema.english.phrase_seg`，临英与快捷输入英文共读
            // `input.temp_english.phrase_seg`，两份开关相或。引擎只需知道「要不要具备
            // 这个能力」——按作用域把关是协调器的事，见 `build_engine` 的参数说明。
            let phrase_seg_anywhere = self
                .english
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .phrase_seg
                || self
                    .temp_english
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .phrase_seg;
            // 反查模式专属的「含未启用扩展库」开关（全局镜像），决定 `build_engine` 挂不挂影子层。
            let lookup_disabled_dicts = self
                .reverse_lookup_disabled
                .load(std::sync::atomic::Ordering::Relaxed);
            // 混输分支据此取共享英文引擎而不是另建一份；闭包在 `build_engine` 内部求值，
            // `enable_english` 关着时英文引擎不会被建出来。
            //
            // 构建英文方案自己时必须传 `None`：override 可把英文方案改成混输，回调会在同一线程
            // 再取 `build_locks["english"]`，`std::sync::Mutex` 不可重入，打字线路就此挂死。
            // 闭包先绑定再取引用，写成 `Some(&|| …)` 是临时值，活不到调用点。
            //
            // 先记指纹再建：建的过程中文件被改，下次 `dict.reload` 照样能发现
            let user_dicts_fp = self.user_dicts_fingerprints(schema_id);
            let borrow_shared_english = || self.shared_english_engine();
            let english_provider: Option<EnglishProvider<'_>> = if schema_id == ENGLISH_SCHEMA {
                None
            } else {
                Some(&borrow_shared_english)
            };
            match Self::build_engine(
                schema_id,
                self.data_dir.as_deref(),
                self.store.clone(),
                &codetable_cfg,
                &mix_cfg,
                self.override_dir.as_deref(),
                &pinyin_cfg,
                &self.pinyin_hint,
                &correction,
                misread.as_ref(),
                phrase_seg_anywhere,
                lookup_disabled_dicts,
                // 顶层入口：方案自身是拼音时不加约束（简拼开）。混输在其内部为 secondary 注入。
                None,
                english_provider,
            ) {
                Some(engine) => {
                    #[cfg(test)]
                    self.run_before_publish_hook(schema_id);
                    let engine_type = engine.engine_type();
                    {
                        let mut g = self.engines.lock().unwrap_or_else(|e| e.into_inner());
                        if self.engines_gen.load(std::sync::atomic::Ordering::Acquire) != generation
                        {
                            debug!("{schema_id} 构建期间被失效，丢弃本次结果重建");
                            continue;
                        }
                        // 与发布同在 `engines` 锁里登记指纹：先发布后登记的话，其间
                        // `user_dicts_changed` 会把这个刚建好的方案误报成「变了」。
                        self.user_dicts_seen
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(schema_id.to_string(), user_dicts_fp);
                        g.insert(
                            schema_id.to_string(),
                            LoadedEngine {
                                engine: Arc::from(engine),
                                // 刚建好也算「用过」，否则一个拍子之内就可能被判闲置。
                                last_used: std::sync::atomic::AtomicU64::new(self.use_clock_now()),
                            },
                        );
                    }
                    info!("Loaded engine: {} (type={:?})", schema_id, engine_type);
                    // 活跃方案的引擎（重）建好了：影子层后台预热。非活跃方案等切过去时再热
                    // （`on_active_changed`），不为用户未必会用的方案读盘。
                    if schema_id == self.active_schema_id() {
                        self.warm_disabled_dicts_async(schema_id);
                    }
                    return true;
                }
                None => {
                    warn!("Failed to build engine for schema: {}", schema_id);
                    return false;
                }
            }
        }
    }

    /// 取当前活跃引擎（必要时懒加载）
    fn active_engine(&self) -> Option<Arc<dyn Engine>> {
        self.engine_for(&self.active_schema_id())
    }

    /// 取指定方案的引擎（必要时懒加载）。
    ///
    /// overlay 模式（临拼/特殊模式）的判据必须走这条：`active_engine` 问的是主方案，
    /// 临拼在五笔方案下引用的是拼音方案，拿五笔的引擎类型去判会静默得出错误答案。
    fn engine_for(&self, schema_id: &str) -> Option<Arc<dyn Engine>> {
        if schema_id.is_empty() {
            return None;
        }
        self.ensure_loaded(schema_id);
        self.loaded_engine(schema_id)
    }

    /// 当前活跃方案 ID
    pub fn active_schema_id(&self) -> String {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 当前活跃引擎是否为拼音类型
    pub fn is_pinyin(&self) -> bool {
        self.is_pinyin_of(&self.active_schema_id())
    }

    /// 指定方案的引擎是否为拼音类型。overlay（临拼）用这条，见 [`Self::engine_for`]。
    pub fn is_pinyin_of(&self, schema_id: &str) -> bool {
        self.engine_for(schema_id)
            .map(|e| e.engine_type() == EngineType::Pinyin)
            .unwrap_or(false)
    }

    /// 活跃引擎下 `keys` 恰好切成完整双拼音节时的音节数；非双拼（全拼 / 码表 / 混输）、
    /// 切不完整时 `None`。见 [`Engine::shuangpin_full_syllable_count`]。纯内存，无 IO。
    pub fn shuangpin_full_syllable_count(&self, keys: &str) -> Option<usize> {
        self.active_engine()?.shuangpin_full_syllable_count(keys)
    }

    /// 活跃双拼引擎把击键串按击键对拆成的音节：`(全拼音节, 击键起, 击键止)`；非双拼 → 空。
    pub fn shuangpin_units(&self, keys: &str) -> Vec<(String, usize, usize)> {
        self.active_engine()
            .map(|e| e.shuangpin_units(keys))
            .unwrap_or_default()
    }

    /// 活跃方案是不是双拼（有布局）。
    pub fn is_shuangpin(&self) -> bool {
        !self
            .shuangpin_layout_of(&self.active_schema_id())
            .is_empty()
    }

    /// 活跃拼音引擎下，音节 `syllable` 按模糊音配置展开的等价读音（没开模糊音 / 非拼音 → 空）。
    pub fn fuzzy_siblings(&self, syllable: &str) -> Vec<String> {
        self.active_engine()
            .map(|e| e.fuzzy_siblings(syllable))
            .unwrap_or_default()
    }

    /// 当前活跃引擎是否为纯码表类型（混输 `Mixed` 不算——其拼音半边恒前缀匹配，
    /// 精确匹配语义只对纯码表方案自洽；供协调器判定短语是否随「精确匹配模式」抑制前缀枚举）。
    pub fn is_codetable(&self) -> bool {
        self.active_engine()
            .map(|e| e.engine_type() == EngineType::CodeTable)
            .unwrap_or(false)
    }

    /// 活跃引擎的码元字符集（`[engine.codetable].input_chars` / `.leading_chars`）。
    ///
    /// 未加载引擎、或引擎无「码元」概念（拼音等，`Engine::input_chars` 返回 `None`）时，
    /// 回落内置默认 `a-z`——绝不回落空集，那会让该方案一个字也打不出来。
    /// 对协调器而言「无码元集概念」与「默认码元集」行为相同，故此处合并为一个返回值。
    ///
    /// 混输取主码表子引擎的集合（见 `MixedEngine::input_chars`）。
    pub fn active_input_chars(&self) -> wind_config::CodeCharSet {
        self.input_chars_of(&self.active_schema_id())
    }

    /// 指定方案的码元字符集。语义同 [`Self::active_input_chars`]（含「绝不回落空集」）。
    ///
    /// 双拼布局把韵母塞进符号键（如 `;` = ing）时，这张集合是唯一的仲裁者。overlay 必须按
    /// 自己引用的方案取，否则临拼在五笔下拿到五笔的码元集。
    pub fn input_chars_of(&self, schema_id: &str) -> wind_config::CodeCharSet {
        self.engine_for(schema_id)
            .and_then(|e| e.input_chars().cloned())
            .unwrap_or_else(wind_config::CodeCharSet::default_alpha)
    }

    /// 该字符在活跃方案下是否为码元（可进输入缓冲）。按键热路径上每键调用，故不克隆整张位图。
    pub fn active_is_code_char(&self, ch: char) -> bool {
        match self.active_engine().as_ref().and_then(|e| e.input_chars()) {
            Some(cs) => cs.contains(ch),
            None => wind_config::CodeCharSet::default_contains(ch),
        }
    }

    /// 该字符在活跃方案下是否可作首码（缓冲为空时起头）。
    ///
    /// 典型用途：数字是码元（打得出 `Win10`）但不是首码——空缓冲下的数字键仍须是
    /// 选词/透传，否则用户既选不了「第 1 个候选」也拿不回原生数字输入。
    pub fn active_is_leading_char(&self, ch: char) -> bool {
        match self.active_engine().as_ref().and_then(|e| e.input_chars()) {
            Some(cs) => cs.contains_leading(ch),
            None => wind_config::CodeCharSet::default_contains(ch),
        }
    }

    /// 活跃方案的通配键（关闭 / 非码表 / 键非法时 `None`；混输取主码表的）。
    ///
    /// 与 [`Self::active_input_chars`] 同一归属理由：方案级引擎固定参数挂在引擎上，方案切换
    /// 自然跟着换。协调器只从这里取，不读 `codetable_settings()`——那份每次调用都重新折叠，
    /// 且非法键的告警只该在构建期出一次。
    pub fn active_wildcard_key(&self) -> Option<char> {
        self.active_engine().and_then(|e| e.wildcard_key())
    }

    /// 活跃方案反查模式内的通配键（非码表方案 `None`；混输取主码表的）。
    pub fn active_reverse_key(&self) -> Option<char> {
        self.active_engine().and_then(|e| e.reverse_wildcard_key())
    }

    /// 反查模式转换（透传活跃引擎；不看 `wildcard` 主开关，不叠英文 / 拼音）。
    pub fn convert_reverse(
        &self,
        input: &str,
        pattern: &str,
        max_candidates: usize,
    ) -> Option<ConvertResult> {
        self.active_engine()?
            .convert_reverse(input, pattern, max_candidates)
    }

    /// 活跃方案的满码长：码表取其码长，混输取主码表的，拼音 / 双拼为 0（无「满码」）。
    ///
    /// 与活跃引擎自报的 [`Engine::max_code_length`] 只差在混输：那个值对混输刻意为 0。短语自动上屏
    /// 的「是否参与」与最短码长都按本值判，混输才和它的主码表同口径。
    pub fn active_full_code_length(&self) -> usize {
        self.active_wildcard_code_length()
    }

    /// 活跃方案的通配码长（[`Engine::wildcard_code_length`]；混输取主码表的）。
    /// 协调器判「满码后通配键按字面」只从这里取，不用活跃引擎的 [`Engine::max_code_length`]
    /// ——混输那个值刻意为 0。
    pub fn active_wildcard_code_length(&self) -> usize {
        self.active_engine()
            .map(|e| e.wildcard_code_length())
            .unwrap_or(0)
    }

    /// 活跃方案的通配是否与拼音共用输入（五笔拼音混输且有拼音子引擎）。
    pub fn active_wildcard_mixes_pinyin(&self) -> bool {
        self.active_engine()
            .is_some_and(|e| e.wildcard_mixes_pinyin())
    }

    /// 通配转换（透传活跃引擎）。不叠英文混入：通配是查码，不是打英文。
    pub fn convert_wildcard(
        &self,
        input: &str,
        pattern: &str,
        max_candidates: usize,
    ) -> Option<ConvertResult> {
        self.active_engine()?
            .convert_wildcard(input, pattern, max_candidates)
    }

    /// 同 [`Self::active_is_code_char`]，但取指定方案（overlay 用，见 [`Self::engine_for`]）。
    ///
    /// 拼音引擎的码元集完全由双拼布局推导（`PinyinEngine::input_chars`）：全拼恒 `None` ⇒
    /// 回落 `a-z`，故调用点的非字母分支在全拼与纯字母双拼布局下恒不进入。
    pub fn is_code_char_of(&self, schema_id: &str, ch: char) -> bool {
        match self
            .engine_for(schema_id)
            .as_ref()
            .and_then(|e| e.input_chars())
        {
            Some(cs) => cs.contains(ch),
            None => wind_config::CodeCharSet::default_contains(ch),
        }
    }

    /// 同 [`Self::active_is_leading_char`]，但取指定方案。首码/全集之分见那边的文档。
    pub fn is_leading_char_of(&self, schema_id: &str, ch: char) -> bool {
        match self
            .engine_for(schema_id)
            .as_ref()
            .and_then(|e| e.input_chars())
        {
            Some(cs) => cs.contains_leading(ch),
            None => wind_config::CodeCharSet::default_contains(ch),
        }
    }

    /// 活跃引擎的候选排序是否忽略权重（`base_sort = "natural"`）。供协调器合并短语后按同一维度
    /// 重排（natural 模式丢弃 weight，对齐引擎 `candidate::by_natural`）；未加载/其余引擎为 false。
    pub fn active_base_sort_ignores_weight(&self) -> bool {
        self.active_engine()
            .map(|e| e.base_sort_ignores_weight())
            .unwrap_or(false)
    }

    /// 指定方案的基础排序是否忽略权重（`base_sort = "natural"`）。overlay 类模式（临时拼音、
    /// 快捷输入等）跑的是另一个方案的引擎，必须按目标方案取，不能用活跃方案的。
    pub fn base_sort_ignores_weight_of(&self, schema_id: &str) -> bool {
        if !self.ensure_loaded(schema_id) {
            return false;
        }
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .map(|e| e.engine.base_sort_ignores_weight())
            .unwrap_or(false)
    }

    /// 可用方案列表（快照拷贝）。
    ///
    /// 这里再滤一遍定制版 `[schemas] hide`（主拦截在 [`Self::read_schema`]）：构造与
    /// [`Self::reload_from_config`] 的 `retain` 会无条件保留活跃方案，而它恰是最可能被 hide
    /// 的。滤在读取侧，因为这是所有消费者的唯一出口。
    pub fn available_schemas(&self) -> Vec<String> {
        self.available
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|s| !Config::custom_hides_schema(s))
            .cloned()
            .collect()
    }

    /// 所有已安装且受支持的方案列表（目录扫描），含隐藏方案。
    ///
    /// 扫描 `data_dir/schemas/*.schema.toml`，对每个文件取去掉 `.schema.toml` 后缀的 id，
    /// 按 `is_supported()` 过滤掉不支持的方案，再并入当前 `available`（保证已启用方案即使
    /// 文件异常也在列），去重后按 id 字典序稳定排序返回。
    ///
    /// `data_dir` 为 None 时（纯测试）回退到 `available_schemas()`。
    ///
    /// 隐藏方案（`[schema].hidden`）也在返回值里：那是呈现层判断，`rebuild_all_caches` 与
    /// `delete_package` 的 keep 列表都需要全集。需要按 hidden 过滤的地方自己调
    /// [`Self::schema_is_hidden`]。
    ///
    /// 被定制版 `[schemas] hide` 删掉的方案不在返回值里——那不是「显示不显示」，
    /// 而是「装没装」，与 `[schema].hidden` 是两个正交的轴（见 [`Config::custom_hides_schema`]）。
    pub fn installed_schemas(&self) -> Vec<String> {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return self.available_schemas();
        };
        // 种子已由 available_schemas() 滤掉被 hide 的方案（双保险，见该方法）。
        let mut ids: Vec<String> = self.available_schemas();
        let ov = self.override_dir.as_deref();

        // 目录扫描（各层合并去重）与 hide 过滤都在 scan_layer_schema_ids 里。
        for id in Self::scan_layer_schema_ids(data_dir) {
            // 只按「受支持」过滤；`[schema].hidden` 与否留给调用方（见本函数文档）。
            if !ids.contains(&id) && Self::schema_supported(&id, Some(data_dir), ov) {
                ids.push(id);
            }
        }

        ids.sort();
        ids.dedup();
        ids
    }

    /// overlay 方案注册表：所有带 `[overlay]` 段的已安装方案，按 id 字典序。
    ///
    /// 「有哪些特殊模式」的唯一真相源，`ModeKind::Special(u8)` / `State.special_id` 的下标即
    /// 本表下标。枚举源是 [`Self::installed_schemas`] 而不是 `available`：overlay 方案不进
    /// `schema.available`。（`all_key_action_keys` 仍只遍历 `available`，要收 overlay 方案的
    /// `[key_actions]` 时那里也得换源。）
    ///
    /// 走 `read_schema`（含 override 深合并），设置页对 `[overlay]` 的覆盖自动生效。安装新
    /// overlay 方案会改变其后方案的下标，可接受（overlay 是瞬态的，`layout.rs` 处理越界回落）。
    /// 整表随 [`Self::invalidate_schema`] 失效。
    pub fn overlay_modes(&self) -> Vec<OverlayEntry> {
        if let Some(v) = self
            .overlay_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return v.clone();
        }
        let out: Vec<OverlayEntry> = self
            .installed_schemas()
            .into_iter()
            .filter_map(|id| {
                let s =
                    Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref())?;
                let spec = s.overlay?;
                let name = if s.schema.name.is_empty() {
                    id.clone()
                } else {
                    s.schema.name
                };
                // 与 `schema_icon_label` 走同一个函数，免得两处截断口径漂移。
                let icon_label = wind_config::icon_label_trunc(&s.schema.icon_label);
                Some(OverlayEntry {
                    schema_id: id,
                    name,
                    icon_label,
                    spec,
                })
            })
            // u8 下标上限：超出的条目进不了 `ModeKind::Special(u8)`，索性不入表——
            // 入表而无法激活比不入表更难排查（设置页列得出来、按键毫无反应）。
            .take(u8::MAX as usize + 1)
            .collect();
        *self.overlay_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(out.clone());
        out
    }

    /// 按方案 id 定位 overlay 注册表下标（供 `special:<id>` / `enter_special:<id>` 分发）。
    ///
    /// 未命中时若该 id 是被定制版 `[schemas] hide` 掉的，补一次 WARN：别的引用路径会经
    /// [`Self::read_schema`] 告警，唯独这里（注册表来自已滤掉 hide 的目录扫描）不会。
    /// 行为不变：分发点拿到 `None` 不吞键、落普通输入。
    pub fn overlay_index_of(&self, schema_id: &str) -> Option<u8> {
        let idx = self
            .overlay_modes()
            .iter()
            .position(|e| e.schema_id == schema_id)
            .map(|i| i as u8);
        if idx.is_none() && Config::custom_hides_schema(schema_id) {
            Self::warn_hidden_schema_once(schema_id);
        }
        idx
    }

    /// 枚举可选的双拼布局：按层序（`user > custom > data`）合并扫描各层的
    /// `schemas/shuangpin/*.toml`，靠前的层同名（按文件名 stem）覆盖靠后的层。
    ///
    /// 返回 `(id, 显示名)`：id 取文件名 stem（与加载路径 `{layout}.toml` 一致，
    /// 保证"能选=能加载"），显示名取布局 `[meta].name`；解析失败（如缺 `[finals]`）
    /// 的布局跳过并告警。供设置页"双拼布局"下拉动态取值，取代前端硬编码清单。
    ///
    /// `data_dir` 为 None（纯测试）时返回空。
    pub fn shuangpin_layouts(&self) -> Vec<(String, String)> {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return Vec::new();
        };
        let layers: Vec<wind_config::ResourceLayer> =
            Config::resource_layers_named_with(Some(data_dir))
                .into_iter()
                .map(|l| l.sub("schemas").sub("shuangpin"))
                .collect();
        Self::scan_shuangpin_layouts(&layers)
    }

    /// 纯扫描逻辑（可测）：按 `layers` 顺序扫描 `*.toml`，靠前的层优先，
    /// 以文件名 stem 去重；输出按 id 字典序稳定排序。
    ///
    /// 收带层名的 [`wind_config::ResourceLayer`] 而不是裸路径：覆盖日志要说清「胜出的是
    /// 哪一层」，只有路径就只能靠前缀去猜。
    fn scan_shuangpin_layouts(layers: &[wind_config::ResourceLayer]) -> Vec<(String, String)> {
        // 值是胜出文件的 (层名, 路径)，仅供被遮蔽方打覆盖日志时引用（见下）。
        let mut seen: std::collections::HashMap<String, (&'static str, std::path::PathBuf)> =
            std::collections::HashMap::new();
        let mut out: Vec<(String, String)> = Vec::new();
        for layer in layers {
            let dir = &layer.path;
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                // 只收普通文件：忽略名字恰好以 .toml 结尾的子目录等。
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let fname = entry.file_name();
                let fname_str = fname.to_string_lossy();
                let Some(stem) = fname_str.strip_suffix(".toml") else {
                    continue;
                };
                let id = stem.to_string();
                if let Some((winner_layer, winner)) = seen.get(&id) {
                    // 靠前的层已收录：本条是被遮蔽的同名布局。只有扫到被遮蔽方才确证有覆盖，
                    // 日志里给的是胜出那份的层名与路径。
                    Config::log_layer_override(
                        winner_layer,
                        "shuangpin",
                        &format!("shuangpin/{id}.toml"),
                        winner,
                        true,
                    );
                    continue;
                }
                seen.insert(id.clone(), (layer.name, entry.path()));
                match crate::pinyin::shuangpin::Layout::from_toml(&entry.path()) {
                    Ok(lay) => {
                        // id 以文件名 stem 为准（加载路径 {layout}.toml）；[meta].id 仅作校验。
                        if !lay.id.is_empty() && lay.id != id {
                            warn!(
                                "双拼布局文件名 {} 与 [meta].id=\"{}\" 不符，以文件名为准",
                                id, lay.id
                            );
                        }
                        let name = if lay.name.is_empty() {
                            id.clone()
                        } else {
                            lay.name
                        };
                        out.push((id, name));
                    }
                    Err(e) => {
                        warn!("双拼布局枚举跳过 {}: {}", entry.path().display(), e);
                    }
                }
            }
        }
        // 内置方案固定顺序（按流行度），其余按 id 字母序排到末尾。
        const BUILTIN_ORDER: &[&str] = &["xiaohe", "ziranma", "sogou", "mspy", "abc", "ziguang"];
        let rank = |id: &str| -> (usize, String) {
            let pos = BUILTIN_ORDER
                .iter()
                .position(|&s| s == id)
                .unwrap_or(usize::MAX);
            (pos, id.to_string())
        };
        out.sort_by_key(|a| rank(&a.0));
        out
    }

    /// 方案显示名（schema.name 优先，缺/读不到回退 id）。带缓存避免重复读盘。
    pub fn schema_name(&self, schema_id: &str) -> String {
        self.schema_labels(schema_id).0
    }

    /// (显示名, 图标短称)：读一次方案文件，进 `name_cache`。
    fn schema_labels(&self, schema_id: &str) -> (String, String) {
        if let Some(v) = self
            .name_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
        {
            return v.clone();
        }
        let schema = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        );
        let name = schema
            .as_ref()
            .map(|s| s.schema.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| schema_id.to_string());
        let icon = schema
            .map(|s| wind_config::icon_label_trunc(&s.schema.icon_label))
            .unwrap_or_default();
        let v = (name, icon);
        self.name_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(schema_id.to_string(), v.clone());
        v
    }

    /// 指定方案的图标短称（`schema.icon_label` 截断到 [`wind_config::ICON_LABEL_MAX_WIDTH`]）；
    /// 未配置返回空串。用于状态气泡/工具栏 short 模式。
    ///
    /// 截断口径见 [`wind_config::icon_label_trunc`]——上限由渲染宽度与 C++ 侧缓冲共同决定，
    /// 与"是不是方案"无关，故与非中文态标签（`[ui.labels]`）共用同一个函数。
    pub fn schema_icon_label(&self, schema_id: &str) -> String {
        self.schema_labels(schema_id).1
    }

    /// 指定方案的引擎类型字符串（小写，如 "pinyin"|"codetable"|"mixed"）；读不到返回 None。
    /// 不切换活跃方案（设置页 dict.encode 据此选拼音/五笔出码规则）。
    pub fn schema_engine_type(&self, schema_id: &str) -> Option<String> {
        {
            let cache = self
                .schema_type_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(v) = cache.get(schema_id) {
                return v.clone();
            }
        }
        let ty = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
        .map(|s| s.engine.engine_type.to_lowercase());
        self.schema_type_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(schema_id.to_string(), ty.clone());
        ty
    }

    /// 存储归属 id：拼音引擎方案统一为 "pinyin"（拼音/双拼共享一份用户词/临时/词频）；
    /// 其余方案（码表/混输/未知）用自身 id。仅影响存储键，不影响引擎行为。
    pub fn data_schema_id(&self, schema_id: &str) -> String {
        if self.schema_engine_type(schema_id).as_deref() == Some("pinyin") {
            PINYIN_DATA_SCHEMA.to_string()
        } else {
            schema_id.to_string()
        }
    }

    /// 混输方案的主码表方案 id（`[engine.mixed].primary_schema`）；非混输/未知/未配置返回 None。
    pub fn mixed_primary_schema(&self, schema_id: &str) -> Option<String> {
        if self.schema_engine_type(schema_id).as_deref() != Some("mixed") {
            return None;
        }
        self.schema_def_cached(schema_id)
            .map(|s| s.engine.mixed.primary_schema.clone())
            .filter(|p| !p.is_empty())
    }

    /// 混输方案的拼音子方案 id（`[engine.mixed].secondary_schema`）；非混输/未知/未配置返回 None。
    /// 按键路径上用：引擎类型与方案定义都走缓存。
    pub fn mixed_secondary_schema(&self, schema_id: &str) -> Option<String> {
        if self.schema_engine_type(schema_id).as_deref() != Some("mixed") {
            return None;
        }
        self.schema_def_cached(schema_id)
            .map(|s| s.engine.mixed.secondary_schema.clone())
            .filter(|p| !p.is_empty())
    }

    /// 写入/词频归属 id：非混输 = `data_schema_id(自身)`（source 无关，零回归）；
    /// 混输按候选来源分流（码表→主方案自身 id、拼音→"pinyin"）；
    /// 无法归因（None/English/Phrase 或 primary 缺失）返回 None，调用方跳过本次读写。
    pub fn write_data_schema_id(&self, schema_id: &str, source: CandidateSource) -> Option<String> {
        if self.schema_engine_type(schema_id).as_deref() != Some("mixed") {
            return Some(self.data_schema_id(schema_id));
        }
        match source {
            CandidateSource::CodeTable => self
                .mixed_primary_schema(schema_id)
                .map(|p| self.data_schema_id(&p)),
            CandidateSource::Pinyin => Some(PINYIN_DATA_SCHEMA.to_string()),
            _ => None,
        }
    }

    /// 方案基础定义（不含 override 层）——设置页计算 saveConfig 稀疏 diff 的基准。
    pub fn schema_base(&self, schema_id: &str) -> Option<Schema> {
        Self::read_schema(schema_id, self.data_dir.as_deref(), None)
    }

    /// 方案合并定义（基础 + override 层）——设置页 getConfig 返回。
    pub fn schema_merged(&self, schema_id: &str) -> Option<Schema> {
        Self::read_schema_in(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
            self.user_text_dir(),
        )
    }

    /// 用户数据目录（`<用户目录>/data`）；无 store 时 None（不登记用户分库）。
    fn user_text_dir(&self) -> Option<&Path> {
        self.store.as_ref().map(|s| s.user_text_dir())
    }

    /// 读取某方案 override 层（TOML 值，无则 None）。
    pub fn get_schema_override(&self, schema_id: &str) -> Option<toml::Value> {
        let dir = self.override_dir.as_deref()?;
        Self::read_override_value(schema_id, dir)
    }

    /// 某方案 override 文件的路径（目录未配置时 `None`；文件不一定存在）。
    /// 供读改写方区分「文件不存在」与「文件存在但解析失败」——[`Self::get_schema_override`]
    /// 两者都返回 `None`，拿空表起步写回会把坏文件里的其它内容整份冲掉。
    pub fn schema_override_path(&self, schema_id: &str) -> Option<std::path::PathBuf> {
        Some(
            self.override_dir
                .as_deref()?
                .join(format!("{schema_id}.toml")),
        )
    }

    /// 仅写入某方案 override 层（原子 tmp+rename），不使引擎缓存失效。
    /// 供「持久化 + 对已加载引擎 live 生效」的场景（如扩展词库热插拔）使用。
    pub fn persist_schema_override(
        &self,
        schema_id: &str,
        value: &toml::Value,
    ) -> anyhow::Result<()> {
        let dir = self
            .override_dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("无 override 目录"))?;
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{schema_id}.toml"));
        let out = toml::to_string_pretty(value)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// 写入某方案 override 层并使其引擎缓存失效（下次使用按新配置重建）。
    pub fn write_schema_override(
        &self,
        schema_id: &str,
        value: &toml::Value,
    ) -> anyhow::Result<()> {
        self.persist_schema_override(schema_id, value)?;
        self.invalidate_schema(schema_id);
        Ok(())
    }

    /// 已加载的混输方案里，以 `schema_id` 为成员（primary / secondary / english）的那些。
    ///
    /// 码表 / 拼音成员子引擎在 `build_engine` 里内联构造、不入 `engines` 表，同一份词库可能
    /// 有独立方案与混输内部两个副本。「对已加载引擎做点什么」的运行时操作都得把后者算上，
    /// 否则命中的是没人在用的那份。english 由全局 `schema.mix.enable_english` 决定建不建，
    /// 故对所有混输一律返回；它不是副本，混输持有的就是共享的那个 `Arc`（见 [`EnglishProvider`]）。
    fn loaded_mixed_dependents(&self, schema_id: &str) -> Vec<(String, Arc<dyn Engine>)> {
        let loaded: Vec<(String, Arc<dyn Engine>)> = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(id, _)| id.as_str() != schema_id)
            .map(|(id, l)| (id.clone(), Arc::clone(&l.engine)))
            .collect();
        loaded
            .into_iter()
            .filter(|(id, _)| {
                if self.schema_engine_type(id).as_deref() != Some("mixed") {
                    return false;
                }
                if schema_id == ENGLISH_SCHEMA {
                    return true;
                }
                // 沿 primary / secondary 传递：混输把另一个混输当成员时，叶子成员也被它捧着。
                self.mixed_members_closure(id)
                    .iter()
                    .any(|m| m == schema_id)
            })
            .collect()
    }

    /// 混输方案沿 primary / secondary 传递可达的全部成员（不含自身）；非混输为空。
    /// 已访问集合防环（成环的方案建不起来，但这里读的是方案文件，环照样会出现）。
    fn mixed_members_closure(&self, schema_id: &str) -> Vec<String> {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return Vec::new();
        };
        let mut seen = vec![schema_id.to_string()];
        let mut stack = vec![schema_id.to_string()];
        while let Some(id) = stack.pop() {
            for m in Self::mixed_edges_of(&id, data_dir, self.override_dir.as_deref()) {
                if !seen.contains(&m) {
                    seen.push(m.clone());
                    stack.push(m);
                }
            }
        }
        seen.remove(0);
        seen
    }

    /// 运行时启停某方案的扩展词库。禁用与启用不对称：
    ///
    /// - 禁用：对已加载引擎即时摘掉那一层（`Engine::set_dict_enabled`），无需重建。摘层同时
    ///   释放该词库的 `CachedDict`，这是 Windows 下被 mmap 的 wdat 能被删除的唯一途径。
    /// - 启用：未启用的词库没加载（惰性加载），一律失效整个方案、下次使用时重建。
    ///
    /// 拼音族不支持热翻，两个方向都走失效重建。未加载的方案此处不做事（下次构建生效）。
    /// 反查索引/单字全码表基于启用词库合并，一并失效。恒返回 true（每条路都无需重启即可生效）。
    /// 调用方须先 [`persist_schema_override`] 持久化，否则重启/重建后状态丢失。
    ///
    /// 必须扇出到把它当成员的混输方案：只翻独立方案那个实例，真正在服务的 `MixedEngine` 里那份
    /// 不变，表现为「关了没反应，顺手改别的设置又好了」。转发不到的依赖方直接失效待重建。
    pub fn set_dict_enabled_live(&self, schema_id: &str, dict_id: &str, enabled: bool) -> bool {
        // 禁用方向只对已加载引擎摘层，摸不到正在锁外构建的那份（它可能已按旧 override 读到
        // 这个库开着）：推进代次让它写回前作废重建（见 `engines_gen`）。启用方向走
        // `invalidate_schema`，自带推进。
        if !enabled {
            let _g = self.engines.lock().unwrap_or_else(|e| e.into_inner());
            self.engines_gen
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        let engine = self.loaded_engine(schema_id);
        // 独立方案与把它当成员的混输方案同一条规则：翻得动就翻，翻不动就失效待重建。
        // 只失效混输不失效独立方案的话，同一个拼音扩展库在混输里下次输入就生效、
        // 在独立拼音方案里却要重启，两个方案表现不一致。
        if enabled {
            // 启用一律失效重建，且扇出到每一个 mixed 依赖方，不看 `set_dict_enabled` 的返回值
            // （启用方向恒 false）。否则叠上下面禁用路径的 english 豁免，英文扩展库的启用在混输里
            // 永远不生效：混输捧着的是构造时拿到的那个 `Arc`。没建 english 子引擎时这趟失效多余，
            // 但多余只是一次重建。
            //
            // 依赖方先取后失效：`invalidate_schema(schema_id)` 自带扇出会把混输从 `engines` 摘掉，
            // 之后再问就恒为空。
            let dependents: Vec<String> = self
                .loaded_mixed_dependents(schema_id)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            self.invalidate_schema(schema_id);
            for mixed_id in dependents {
                info!("启用词库 {}：失效混输方案 {} 待重建", dict_id, mixed_id);
                self.invalidate_schema(&mixed_id);
            }
        } else if let Some(e) = engine
            && !e.set_dict_enabled(dict_id, false)
        {
            info!(
                "方案 {} 不支持即时翻转词库 {}，已失效待重建",
                schema_id, dict_id
            );
            self.invalidate_schema(schema_id);
        }
        // 引擎不在表里也算数：「从未加载」或「刚被本批次前一次调用失效掉」（设置页批量勾选时
        // 是常态），都是下次构建按已落盘的 override 生效。回 false 会让批量启用误报「没生效」。
        // 禁用方向才走转发：承载该库的那个子引擎摘层即释放，其余子引擎因
        // `own_extra_dicts` 不含这个 id 而明确回 false，不会冒充「我处理了」。
        for (mixed_id, e) in self.loaded_mixed_dependents(schema_id) {
            if !enabled && !e.set_dict_enabled(dict_id, false) && schema_id != ENGLISH_SCHEMA {
                // english 子引擎可能根本没建（enable_english 关着），转发不到是常态，不失效。
                info!(
                    "混输方案 {} 内的成员 {} 不支持即时翻转词库 {}，已失效待重建",
                    mixed_id, schema_id, dict_id
                );
                self.invalidate_schema(&mixed_id);
            }
        }
        // 反查索引依赖「启用词库合并」，启用集变了须失效（懒重建）。
        self.reverse_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.reverse_index_skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // 单字全码表同源于「启用词库合并」，与反查索引同生命周期，一并失效。
        *self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // 禁用方向把一个库挪进了影子集合、作废了已建的影子层：趁现在后台重建，别等首次反查
        // 在按键线程上读盘。启用方向走失效重建，新引擎建好时自会预热（见 `ensure_loaded`）。
        if !enabled {
            self.warm_disabled_dicts_async(&self.active_schema_id());
        }
        // 恒 true：上面每条路都无需重启即可生效。保留返回值给调用方（web 的 `live` 字段）。
        true
    }

    /// 方案引擎用到的用户分库所在的数据方案：混输取两个成员的，其余取自己的。
    fn user_dict_data_schemas(&self, schema_id: &str) -> Vec<String> {
        if self.schema_engine_type(schema_id).as_deref() != Some("mixed") {
            return vec![self.data_schema_id(schema_id)];
        }
        let mut out = Vec::new();
        for m in self.user_dict_mixed_members(schema_id) {
            let ds = self.data_schema_id(&m);
            if !out.contains(&ds) {
                out.push(ds);
            }
        }
        out
    }

    fn user_dicts_fingerprints(&self, schema_id: &str) -> Vec<(String, String)> {
        let Some(dir) = self.user_text_dir() else {
            return Vec::new();
        };
        self.user_dict_data_schemas(schema_id)
            .into_iter()
            .map(|ds| {
                let fp = wind_store::user_dicts::fingerprint(dir, &ds);
                (ds, fp)
            })
            .collect()
    }

    /// 已建引擎所用的用户分库里，文件或开关变了的数据方案（不带路径的 `dict.reload`）。
    /// 只比对，不失效：调用方对返回的每个数据方案调 [`Self::invalidate_user_dicts`]。
    pub fn user_dicts_changed(&self) -> Vec<String> {
        let Some(dir) = self.user_text_dir() else {
            return Vec::new();
        };
        let loaded: Vec<String> = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        let seen = self
            .user_dicts_seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut now: HashMap<String, String> = HashMap::new();
        let mut changed: Vec<String> = Vec::new();
        for id in loaded {
            for (ds, fp) in seen.get(&id).into_iter().flatten() {
                let cur = now
                    .entry(ds.clone())
                    .or_insert_with(|| wind_store::user_dicts::fingerprint(dir, ds));
                if cur != fp && !changed.contains(ds) {
                    changed.push(ds.clone());
                }
            }
        }
        changed.sort();
        changed
    }

    /// 某数据方案的用户分库变了（文件或开关）：失效用到它的全部方案（拼音族、作成员的混输）。
    /// 编译缓存按内容指纹，没变的分库不重编，代价是一次方案重建。
    pub fn invalidate_user_dicts(&self, ds: &str) {
        let mut ids: Vec<String> = self.available_schemas();
        let loaded: Vec<String> = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for id in loaded {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let mut hits: Vec<String> = Vec::new();
        for id in &ids {
            for m in self.user_dict_mixed_members(id) {
                if !ids.contains(&m) && !hits.contains(&m) && self.data_schema_id(&m) == ds {
                    hits.push(m);
                }
            }
            if self.schema_engine_type(id).as_deref() != Some("mixed")
                && self.data_schema_id(id) == ds
                && !hits.contains(id)
            {
                hits.push(id.clone());
            }
        }
        info!(
            "用户分库有变化：失效 {} 个方案（数据方案 {ds}）",
            hits.len()
        );
        // `invalidate_schema` 自带扇出到把它当成员的混输
        for id in hits {
            self.invalidate_schema(&id);
        }
        self.reverse_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.reverse_index_skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 混输方案的成员（primary / secondary，传递闭包）；非混输为空。
    fn user_dict_mixed_members(&self, schema_id: &str) -> Vec<String> {
        if self.schema_engine_type(schema_id).as_deref() != Some("mixed") {
            return Vec::new();
        }
        self.mixed_members_closure(schema_id)
    }

    /// 删除某方案 override 层并使其引擎缓存失效。返回是否删除了文件。
    pub fn delete_schema_override(&self, schema_id: &str) -> anyhow::Result<bool> {
        let removed = if let Some(dir) = self.override_dir.as_deref() {
            let path = dir.join(format!("{schema_id}.toml"));
            if path.exists() {
                std::fs::remove_file(&path)?;
                true
            } else {
                false
            }
        } else {
            false
        };
        self.invalidate_schema(schema_id);
        Ok(removed)
    }

    /// 是否为用户目录方案（%APPDATA%/…/schemas/{id}.schema.toml 存在）。
    /// 与 [`Self::delete_user_schema`] 的可删判定同源；仅安装目录的内置方案返回 false。
    pub fn is_user_schema(&self, schema_id: &str) -> bool {
        Config::user_config_dir()
            .map(|d| {
                d.join("schemas")
                    .join(format!("{schema_id}.schema.toml"))
                    .is_file()
            })
            .unwrap_or(false)
    }

    /// 删除用户自定义方案：仅当方案文件存在于用户目录（非内置 data 目录）时允许。
    /// 同时清除其 override 并从可用列表移除。返回是否删除。内置方案返回 Err。
    pub fn delete_user_schema(&self, schema_id: &str) -> anyhow::Result<bool> {
        let user_file = Config::user_config_dir()
            .map(|d| d.join("schemas").join(format!("{schema_id}.schema.toml")));
        match &user_file {
            Some(p) if p.is_file() => {
                std::fs::remove_file(p)?;
            }
            _ => anyhow::bail!("内置方案不可删除: {}", schema_id),
        }
        self.forget_deleted_schema(schema_id);
        Ok(true)
    }

    /// 方案文件已被外部删除(方案包级联删除)后的引擎侧收尾:
    /// 清 override、移出可用列表、失效解析/引擎缓存。不触碰文件系统里的方案文件。
    pub fn forget_deleted_schema(&self, schema_id: &str) {
        let _ = self.delete_schema_override(schema_id);
        self.available
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|s| s != schema_id);
        self.invalidate_schema(schema_id);
    }

    /// 使某方案的引擎与解析缓存失效（override/词典变更后，下次构建按新定义重建）。
    /// pub:方案包导入后由 RPC 层调用,失效已加载缓存(未加载时安全 no-op)。
    pub fn invalidate_schema(&self, schema_id: &str) {
        {
            let mut g = self.engines.lock().unwrap_or_else(|e| e.into_inner());
            // 与摘除同在一把锁下推进：见 `engines_gen`。下面扇出摘的混输也由这一次覆盖。
            self.engines_gen
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            g.remove(schema_id);
            // 英文方案被换掉时，共享英文引擎缓存也必须作废，否则混入与混输一直握着旧实例。
            // 必须与推进代次同在这把锁里（锁序 engines → 缓存）：放锁后再清，正等着写回的混输
            // 会按新代次重来、从还没清的缓存里取到旧实例并写回成功。
            if schema_id == ENGLISH_SCHEMA {
                *self
                    .shared_english_engine
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
        #[cfg(test)]
        self.run_before_publish_hook("@after_engines_bump");
        // 扇出到把它当成员的混输方案：混输在构建时就接过了成员子引擎的 `Arc`，从表里摘掉
        // 成员换不动它，不扇出则混输继续服务旧子引擎（且旧英文引擎被吊着、共享缓存又建一份）。
        // 凡失效必扇出，故放在这里而不是各调用方。
        //
        // 只摘 `engines` 条目、不递归走整个 `invalidate_schema`：混输自身配置没变，派生缓存
        // 仍有效，且递归在「混输套混输」下绕不出来。取 id 时就丢掉 `Arc`，让旧引擎尽早释放。
        //
        // english 那一支额外看 `enable_english`：`loaded_mixed_dependents` 对 english 一律返回
        // 全部混输，开关关着时混输手里根本没有英文子引擎，摘了只是白白重建。`mix` 只在
        // `reload_from_config` 里与 `engines.clear()` 一起更新，读到的就是在世混输构建时的值。
        let english_unused_by_mixed = schema_id == ENGLISH_SCHEMA
            && !self
                .mix
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .enable_english;
        let dependents: Vec<String> = if english_unused_by_mixed {
            Vec::new()
        } else {
            self.loaded_mixed_dependents(schema_id)
                .into_iter()
                .map(|(id, _)| id)
                .collect()
        };
        for mixed_id in dependents {
            debug!("失效 {schema_id}：连带失效混输方案 {mixed_id}（它捧着旧的成员子引擎）");
            self.engines
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&mixed_id);
        }
        self.freq_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(schema_id);
        self.invalidate_aux_settings_cache();
        self.invalidate_codetable_settings_cache();
        self.invalidate_schema_def_cache();
        self.invalidate_data_facts();
        self.schema_type_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(schema_id);
        self.key_actions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(schema_id);
        self.session_actions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(schema_id);
        self.behavior_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(schema_id);
        self.name_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(schema_id);
        // overlay 注册表是集合：装/删方案或改 [overlay] 段都会改变它的内容与下标，
        // 按 id 局部失效没有意义，整表置 None 由下次 overlay_modes() 重建。
        *self.overlay_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // 主码表(及其词库/override)可能变更:失效反查索引,下次按新内容重建。
        self.reverse_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.reverse_index_skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // 单字全码表同源于「启用词库合并」，与反查索引同生命周期，一并失效。
        *self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // 双拼布局可能变更：失效其全部派生缓存，下次按新布局重建。
        self.invalidate_shuangpin_caches();
    }

    /// 强制重建全部词库缓存：失效所有已装方案的引擎与解析缓存（释放 mmap
    /// reader 的强引用），再 best-effort 删除缓存根下的全部缓存产物。
    ///
    /// 指纹（`cache_fp`，已含 PARSE_SEMANTICS_VERSION）覆盖不到的场景用它兜底：
    /// 缓存文件损坏、解析语义修复未 bump 版本号、或需要立即生效不等下次校验。
    /// 返回 `(removed, failed)`；failed 通常是仍被短暂持有的 mmap（引擎 Arc 尚在
    /// 某次按键处理中），再次执行 rebuild 可清，不影响正确性。
    pub fn rebuild_all_caches(&self) -> (usize, usize) {
        for id in self.installed_schemas() {
            self.invalidate_schema(&id);
        }
        let Some(Some(dir)) = CACHE_DIR.get() else {
            return (0, 0);
        };
        let mut removed = 0;
        let mut failed = 0;
        purge_cache_files(dir, &mut removed, &mut failed);
        (removed, failed)
    }

    /// 活跃方案变更的统一收尾：记一代 + 通知上层。所有改 `active` 的地方都要调它。
    ///
    /// 必须在释放 active 锁之后调用——回调是上层代码（RPC 广播），不应在持锁期间执行。
    fn on_active_changed(&self, id: &str) {
        self.schema_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::active_hook::notify_active_changed(id);
        // 切到的方案若挂着影子层，趁现在后台预热（所有改 `active` 的路径都经过这里）。
        self.warm_disabled_dicts_async(id);
    }

    /// 活跃方案的变更代际，见 [`Self::schema_generation`] 字段说明。
    ///
    /// 用法是「记下当时的值，之后比对是否仍相等」，不要对差值或绝对值做判断。
    pub fn schema_generation(&self) -> u64 {
        self.schema_generation
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 切换到指定方案（必要时懒加载）；真的切换了返回 true，已是该方案或加载失败返回 false。
    pub fn switch_schema(&self, schema_id: &str) -> bool {
        if !self.ensure_loaded(schema_id) {
            return false;
        }
        {
            let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
            if *active == schema_id {
                return false;
            }
            info!("Switching schema: {} -> {}", *active, schema_id);
            *active = schema_id.to_string();
        }
        // 出锁后再通知：回调是上层代码（RPC 广播），不在持锁期间执行。
        self.on_active_changed(schema_id);
        true
    }

    /// 当前生效的全局拼音配置（已套覆盖层）。测试 / 诊断用。
    pub fn pinyin_global(&self) -> wind_config::config::PinyinGlobalConfig {
        self.pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 当前取配置时套用的覆盖层。
    pub fn config_overlay(&self) -> wind_config::ConfigOverlay {
        *self.overlay.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 按新配置热重载方案集（无需重建 EngineManager）：重算可用方案、更新上屏策略、
    /// 清空引擎/词频/名称缓存使其按新配置/词典重建，并切到新的活跃方案。
    /// 返回活跃方案是否发生变化（供上层决定是否清输入缓冲、刷新 UI）。
    pub fn reload_from_config(&self, config: &Config) -> bool {
        // 与构造同源：活跃方案被定制版 hide 掉时降级（不改写用户配置）。
        let cfg_active = config.active_schema().to_string();
        let new_active = Self::resolve_active_schema_id(
            config,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        );
        let mut available = config.schema.available.clone();
        if available.is_empty() {
            available.push(new_active.clone());
        }
        // 过滤不支持的方案，但始终保留活跃方案（与构造逻辑一致）。
        available.retain(|sid| {
            sid == &new_active
                || Self::schema_supported(
                    sid,
                    self.data_dir.as_deref(),
                    self.override_dir.as_deref(),
                )
        });
        Self::ensure_fallback_listed(&mut available, &cfg_active, &new_active);

        // 更新可变状态。
        // 重算主码表(拼音反查码源)。在 available 移入锁前用其引用解析。
        let primary = Self::resolve_primary_codetable(
            &config.schema.primary_codetable,
            &available,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        );
        *self
            .primary_codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = primary;
        // 主码表可能变更:失效反查索引,下次按新主码表重建。
        self.reverse_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.reverse_index_skipped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // 方案文件/override 可能随配置重载而变（设置页改完即热重载），按键功能表一并失效。
        // 两张表同批：会话态那张漏清的表现是「设置页改了翻页键，重启才生效」。
        self.key_actions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.session_actions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.behavior_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // 单字全码表同源于「启用词库合并」，与反查索引同生命周期，一并失效。
        *self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        *self.available.lock().unwrap_or_else(|e| e.into_inner()) = available;
        *self.codetable.lock().unwrap_or_else(|e| e.into_inner()) = config.schema.codetable.clone();
        *self.mix.lock().unwrap_or_else(|e| e.into_inner()) = config.schema.mix.clone();
        *self.english.lock().unwrap_or_else(|e| e.into_inner()) = config.schema.english.clone();
        *self.temp_pinyin.lock().unwrap_or_else(|e| e.into_inner()) =
            config.input.temp_pinyin.clone();
        // 同上：`phrase_seg` 变更要能让下方 `engines.clear()` 重建的引擎读到新值。
        *self.temp_english.lock().unwrap_or_else(|e| e.into_inner()) =
            config.input.temp_english.clone();
        // 反查模式「含未启用扩展库」开关：必须在下方 `engines.clear()` 之前写，末尾那次
        // `ensure_loaded` 才会按新值挂 / 摘影子层。
        self.reverse_lookup_disabled.store(
            config.input.reverse.lookup_disabled_dicts,
            std::sync::atomic::Ordering::Relaxed,
        );
        // 字符类：镜像存的是解析结果，要重新装配；漏掉则设置页改了要重启才生效。
        *self.charsets.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(Self::build_charsets(
            config,
            self.data_dir.as_deref(),
            self.store.as_deref(),
        ));
        *self
            .primary_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = config.schema.primary_pinyin.clone();
        // 全局拼音配置变更：更新缓存，引擎缓存随下方 clear() 一起失效，下次按新配置重建。
        *self.pinyin.lock().unwrap_or_else(|e| e.into_inner()) =
            config.pinyin_effective(self.config_overlay());
        *self
            .correction_cfg
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = config.input.correction.clone();
        *self.correction.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(
            crate::pinyin::correction::CorrectionRules::from_config(&config.input.correction),
        );
        *self.misread.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(Self::load_misread(
            self.data_dir.as_deref(),
            self.user_text_dir(),
        ));
        self.refresh_pinyin_hint();
        // 丢弃缓存：引擎按新上屏策略/词典重建，名称/词频按新方案重读。
        {
            let mut g = self.engines.lock().unwrap_or_else(|e| e.into_inner());
            // 与清表同在一把锁下推进（见 `engines_gen`），且在上面各份配置镜像写完之后。
            self.engines_gen
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            g.clear();
            // 连同共享英文引擎缓存一起重置：里面可能躺着上次的 `Some(None)`（词库当时缺失），
            // 不重置则补上词库、重载配置后仍不生效。与推进代次同在这把锁里，理由见
            // `invalidate_schema` 同一处。
            *self
                .shared_english_engine
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
        self.freq_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.invalidate_aux_settings_cache();
        self.invalidate_codetable_settings_cache();
        self.invalidate_schema_def_cache();
        self.invalidate_data_facts();
        self.schema_type_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.name_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // 双拼布局可能变更：失效其全部派生缓存，下次按新布局重建。
        self.invalidate_shuangpin_caches();

        // 切换活跃方案（即便 id 未变，引擎已被清空，这里立即重建避免首键延迟）。
        let changed = {
            let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
            let changed = *active != new_active;
            *active = new_active.clone();
            changed
        };
        self.ensure_loaded(&new_active);
        info!(
            "EngineManager reloaded from config (active={}, changed={})",
            new_active, changed
        );
        if changed {
            self.on_active_changed(&new_active);
        }
        changed
    }

    /// 循环切换到下一个可加载的方案；返回新方案 ID。
    /// 懒加载：在加载前不持 active 锁，避免首次加载（拼音合并/unigram）阻塞按键路径。
    pub fn cycle_schema(&self) -> Option<String> {
        let available = self.available_schemas();
        let n = available.len();
        let current = self.active_schema_id();
        // 判据是「有没有别的方案可去」，不是「列表里有几个」：直达热键能切到未启用方案
        // （如英文），此时 n == 1 意味着有一个地方可以回。
        if !available.iter().any(|s| s != &current) {
            return None;
        }
        // 起点：当前方案的下一个；当前方案不在 available 时从头开始（别当成第 0 个，
        // 那会跳过 available[0]）。
        let start = match available.iter().position(|s| s == &current) {
            Some(i) => i + 1,
            None => 0,
        };
        for step in 0..n {
            let cand = available[(start + step) % n].clone();
            if cand == current {
                continue;
            }
            if self.ensure_loaded(&cand) {
                {
                    let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
                    info!("Cycling schema: {} -> {}", *active, cand);
                    *active = cand.clone();
                }
                self.on_active_changed(&cand);
                return Some(cand);
            }
        }
        None
    }

    /// 把拼音「屏蔽这个词」集合（拼音数据方案候选调整里 `code = *` 的删除行）推给拼音引擎。
    /// 每次转换前调：集合由 store 按表内容缓存，规则写入 / 文件同步后自然换新，引擎侧同一份
    /// 只比一次指针。拼音型引擎与混输（转给它的拼音子引擎，混输拼音段与拼音方案共用屏蔽）。
    fn push_blocked_words(&self, engine: &dyn Engine) {
        if !matches!(engine.engine_type(), EngineType::Pinyin | EngineType::Mixed) {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        match store.blocked_words(PINYIN_DATA_SCHEMA) {
            Ok(set) => engine.set_blocked_words(set),
            Err(e) => debug!("读取拼音屏蔽词失败: {e}"),
        }
    }

    /// `engine.convert`，英文方案（`english`）再滤掉「屏蔽这个词」的词库词。
    ///
    /// 英文引擎是共享实例：拼音 / 码表 / 混输里混入的英文也用它（`shared_english_engine`
    /// 直调，不经这里），而那些混入的英文按宿主方案各自隐藏、不读英文屏蔽集（设计 §8 第 11 条），
    /// 故屏蔽集不推进引擎，只在按英文方案取候选（英文方案主路、临英、快捷输入英文成员）时在这里滤。
    /// 多取「可能出现在这次结果里」的屏蔽词条数再滤、截回 `max_candidates`，空出的名额由后面的词
    /// 补上。估数按文字以输入为前缀（忽略大小写）计、封顶 [`ENGLISH_BLOCK_EXTRA_CAP`]：英文词库
    /// 按码前缀查，码是小写化的词文；词组（码与文字不同形）漏估时只少补一格，不会多出屏蔽词。
    fn convert_unblocked(
        &self,
        engine: &dyn Engine,
        schema_id: &str,
        input: &str,
        max_candidates: usize,
    ) -> anyhow::Result<ConvertResult> {
        let blocked = (engine.engine_type() == EngineType::English && schema_id == ENGLISH_SCHEMA)
            .then(|| self.store.as_ref()?.blocked_words(ENGLISH_SCHEMA).ok())
            .flatten()
            .filter(|set| !set.is_empty());
        let Some(blocked) = blocked else {
            return engine.convert(input, max_candidates);
        };
        let lower = input.to_lowercase();
        let extra = blocked
            .iter()
            .filter(|w| {
                w.len() >= lower.len()
                    && w.is_char_boundary(lower.len())
                    && w[..lower.len()].eq_ignore_ascii_case(&lower)
            })
            .take(ENGLISH_BLOCK_EXTRA_CAP)
            .count();
        let mut r = engine.convert(input, max_candidates.saturating_add(extra))?;
        r.candidates.retain(|c| !blocked.contains(&c.text));
        r.candidates.truncate(max_candidates);
        r.is_empty = r.candidates.is_empty();
        Ok(r)
    }

    /// 转换输入为候选（分发到当前引擎），必要时混入英文候选。
    ///
    /// 英文混入接在这一层而不是各引擎内部：本管理器同时是 `convert` /
    /// `recheck_auto_commit` / `handle_top_code` 三条通路的收口点。新引擎类型要在
    /// [`Self::english_merge_cfg_for`] 里明确取哪份配置。详见 [`crate::english_merge`] 模块文档。
    pub fn convert(&self, input: &str, max_candidates: usize) -> ConvertResult {
        self.convert_active(input, max_candidates, None)
    }

    /// 同 [`Self::convert`]（含英文混入），但把 [`ConvertOptions`] 交给活跃引擎。
    /// 主输入路的连续辅助码用（击键图 + 辅码表）。
    pub fn convert_active_with_opts(
        &self,
        input: &str,
        max_candidates: usize,
        opts: ConvertOptions,
    ) -> ConvertResult {
        self.convert_active(input, max_candidates, Some(opts))
    }

    /// `opts = None` 走 `convert_unblocked`（与引入 opts 之前逐条一致）。
    fn convert_active(
        &self,
        input: &str,
        max_candidates: usize,
        opts: Option<ConvertOptions>,
    ) -> ConvertResult {
        let mut timer = crate::pinyin::ConvertStageTimer::new();
        let Some(engine) = self.active_engine() else {
            return ConvertResult::default();
        };
        self.push_blocked_words(engine.as_ref());
        let r = match opts {
            None => self.convert_unblocked(
                engine.as_ref(),
                &self.active_schema_id(),
                input,
                max_candidates,
            ),
            Some(opts) => engine.convert_with_opts(input, max_candidates, opts),
        };
        let mut r = r.unwrap_or_else(|e| {
            warn!("convert error: {}", e);
            ConvertResult::default()
        });
        timer.mark("engine");
        if let Some((eng, cfg)) = self.english_merge_ctx(&engine) {
            // 判据只有一个：中文侧有没有候选。不叠「有没有精确命中」，否则在「无精确→有精确」
            // 那一键上候选数骤变。有中文候选 ⇒ 只收精确（前缀词是噪音）；无中文候选 ⇒ 给前缀
            // 补全，精确那条由英文引擎自己排在最前。
            //
            // 例外是可选的前缀补全：`complete_min_length`（够长就补；拼音另要求整串不是完整
            // 拼音）。判据与条数上限都在 `english_merge::select`；双拼已在
            // `english_merge_cfg_for` 折掉。
            let english =
                crate::english_merge::select(eng.as_ref(), input, r.candidates.is_empty(), &cfg);
            // 通路①（满码自动上屏 / 满码空码清空）的英文守护，两级判据见
            // `english_merge::vetoes_commit`（与通路②③、混输同口径）。
            //
            // 前缀级不能只看列表：`gith` 时列表里还没有英文，满码就会把中文顶上屏，`github`
            // 永远敲不完；拦下后继续敲，完整单词必然露面。`should_clear` 一并否决（清空同样
            // 抹掉正在打的英文词）。只在确有可否决的动作时才多查一次英文。
            if (r.should_commit || r.should_clear)
                && crate::english_merge::vetoes_commit(eng.as_ref(), input, &english, &cfg)
            {
                r.should_commit = false;
                r.commit_text.clear();
                r.should_clear = false;
            }
            if !english.is_empty() {
                crate::english_merge::merge(&mut r.candidates, english, max_candidates);
                // 「有输入但无候选」的判据须随之更新：英文进来了就不再是空码，
                // 漏掉这行会让协调器按空码处置（吃键 / 清空），候选却是有的。
                r.is_empty = r.candidates.is_empty();
            }
        }
        timer.mark("english");
        r
    }

    /// 取「英文候选混入」的引擎与配置；关闭 / 不适用 / 词库缺失时 `None`。
    ///
    /// 混输与英文方案自身被排除：混输有自带的英文混入
    /// （`schema.mix.enable_english`，带三方档位仲裁），再叠一层会混两遍、档位与配额双重
    /// 失真；英文方案本身就在出英文候选。
    fn english_merge_ctx(
        &self,
        active: &Arc<dyn Engine>,
    ) -> Option<(Arc<dyn Engine>, crate::english_merge::Effective)> {
        let cfg = self.english_merge_cfg(active)?;
        if !cfg.enable {
            return None;
        }
        Some((self.shared_english_engine()?, cfg))
    }

    /// 取本次转换该用哪份英文混入配置的活跃方案版本；实现与详细说明见
    /// [`Self::english_merge_cfg_for`]。
    fn english_merge_cfg(
        &self,
        active: &Arc<dyn Engine>,
    ) -> Option<crate::english_merge::Effective> {
        self.english_merge_cfg_for(&self.active_schema_id(), active)
    }

    /// 取本次转换该用哪份英文混入配置——按引擎类型分流，码表与拼音两份互不共享取值
    /// （两者是否需要英文是独立的事，见 `wind_config::CodetableEnglishMerge`）。
    ///
    /// 码表侧走 [`Self::codetable_settings_of`]（按指定方案折叠了 `[engine.codetable.english_merge]`），
    /// 不读全局镜像，否则方案里写的覆盖没人读。拼音侧暂无方案级覆盖，直读全局镜像；将来补上
    /// 时这里要跟着换。
    ///
    /// 按方案而非活跃方案取值：overlay/特殊模式的自动上屏复评（[`Self::recheck_auto_commit_for`]）
    /// 引用的方案往往不是活跃方案。
    fn english_merge_cfg_for(
        &self,
        schema_id: &str,
        active: &Arc<dyn Engine>,
    ) -> Option<crate::english_merge::Effective> {
        match active.engine_type() {
            EngineType::CodeTable => {
                let c = self.codetable_settings_of(schema_id).english_merge;
                Some(crate::english_merge::Effective {
                    enable: c.enable,
                    min_length: c.min_length,
                    block_commit: c.block_commit,
                    block_commit_on_word: c.block_commit_on_word,
                    complete_min_length: c.complete_min_length,
                    complete_only_non_pinyin: false,
                    max_count: crate::english_merge::max_count_or_default(c.max_count),
                })
            }
            EngineType::Pinyin => {
                let p = self
                    .pinyin
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .english_merge
                    .clone();
                Some(crate::english_merge::Effective {
                    enable: p.enable,
                    min_length: p.min_length,
                    // 见 `Effective::block_commit`：拼音没有满码上屏 / 顶码可否决。
                    block_commit: false,
                    block_commit_on_word: false,
                    // 补全判据按全拼音节定义，双拼下不生效（双拼码两键一音节，几乎任何串都解得开）。
                    // 补全关着时短路，不问方案；开着时是一次按方案 id 的缓存命中。
                    complete_min_length: if p.complete_min_length > 0
                        && !self.pinyin_is_shuangpin_of(schema_id)
                    {
                        p.complete_min_length
                    } else {
                        0
                    },
                    complete_only_non_pinyin: true,
                    max_count: crate::english_merge::max_count_or_default(p.max_count),
                })
            }
            // 混输有自带的英文混入（`schema.mix.enable_english`，带三方档位仲裁），
            // 再叠一层会混两遍、档位与配额双重失真；英文方案本身就在出英文候选。
            EngineType::Mixed | EngineType::English => None,
        }
        // 刻意不写 `_ => None` 通配分支：新增引擎类型必须在这里明确用哪份配置（或不参与），
        // 由编译器提醒，而不是静默不支持英文混入。
    }

    /// 进程里那唯一一份英文词库引擎（懒加载一次，失败也记住，见
    /// [`Self::shared_english_engine`] 字段文档）。
    ///
    /// 两类调用方：英文候选混入（[`Self::english_merge_ctx`]）与混输的英文子引擎
    /// （经 [`EnglishProvider`] 注入 `build_engine`）。
    fn shared_english_engine(&self) -> Option<Arc<dyn Engine>> {
        {
            let cached = self
                .shared_english_engine
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(v) = cached.as_ref() {
                return v.clone();
            }
        }
        // 锁外构建：`ensure_loaded` 自带 single-flight 且可能秒级，持本缓存锁会堵住按键线路。
        // 混输构建期回调进来时外层持的是混输方案的 build_lock，与 `"english"` 那把不是同一把。
        //
        // 取前记 `engines_gen`、写回前比对：其间失效清掉的缓存不能被旧实例盖回去，变了就只交给
        // 本次调用方、不进缓存。混输构建拿到它也无妨：它自己的写回比对必失败、整份重来，
        // 而失效口已在同一段 `engines` 锁里清掉了缓存。
        let generation = self.engines_gen.load(std::sync::atomic::Ordering::Acquire);
        let built = if self.ensure_loaded(ENGLISH_SCHEMA) {
            self.loaded_engine(ENGLISH_SCHEMA)
        } else {
            None
        };
        #[cfg(test)]
        self.run_before_publish_hook("@shared_english");
        let mut cache = self
            .shared_english_engine
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.engines_gen.load(std::sync::atomic::Ordering::Acquire) == generation {
            *cache = Some(built.clone());
        }
        built
    }

    /// 存在英文候选时是否否决上屏（活跃方案版本）。三条通路共用这一个判据（`AGENTS.md`：
    /// 任何否决开关必须三处都接）；overlay/特殊模式的第四条通路见 [`Self::english_vetoes_commit_for`]。
    fn english_vetoes_commit(&self, active: &Arc<dyn Engine>, input: &str) -> bool {
        self.english_vetoes_commit_for(&self.active_schema_id(), active, input)
    }

    /// [`Self::english_vetoes_commit`] 的按方案版本：`english_merge_ctx` 内部走
    /// `english_merge_cfg`（活跃方案），故这里不能直接复用它，改走
    /// [`Self::english_merge_cfg_for`] + 共享英文引擎（后者本就是进程级单例，
    /// 与方案无关，不需要参数化）。
    fn english_vetoes_commit_for(
        &self,
        schema_id: &str,
        active: &Arc<dyn Engine>,
        input: &str,
    ) -> bool {
        let Some(cfg) = self.english_merge_cfg_for(schema_id, active) else {
            return false;
        };
        if !cfg.enable {
            return false;
        }
        let Some(eng) = self.shared_english_engine() else {
            return false;
        };
        crate::english_merge::vetoes_commit(eng.as_ref(), input, &[], &cfg)
    }

    /// 活跃方案里英文候选的位次下限 `(完整单词, 前缀补全)`（1 起），供协调器
    /// `place_english_after_common_exact` 定位。配置 0 回退各自出厂值（1 / 2）。
    ///
    /// 码表取方案级折叠后的 `english_merge.{exact,complete}_position`，混输取
    /// `schema.mix.english_{exact,complete}_position`，拼音取 `schema.pinyin.english_merge`
    /// 的同名两项（完整单词那项 0 = 按方案自动：全拼 1、双拼 2）；英文方案恒 `(1, 1)`
    /// （= 不额外限制）。
    pub fn active_english_positions(&self) -> (usize, usize) {
        let Some(engine) = self.active_engine() else {
            return (1, 1);
        };
        let (exact, complete) = match engine.engine_type() {
            EngineType::CodeTable => {
                let e = self
                    .codetable_settings_of(&self.active_schema_id())
                    .english_merge;
                (e.exact_position, e.complete_position)
            }
            EngineType::Mixed => {
                let m = self.mix.lock().unwrap_or_else(|e| e.into_inner());
                (m.english_exact_position, m.english_complete_position)
            }
            EngineType::Pinyin => {
                let e = self
                    .pinyin
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .english_merge
                    .clone();
                // 英文候选没开时列表里不会有英文，不必再问方案是不是双拼。
                if !e.enable {
                    return (1, 1);
                }
                // 双拼两键一音节，`the` / `good` 这类常用词几乎都解得出中文，英文不抢首选。
                let auto = if self.pinyin_is_shuangpin_of(&self.active_schema_id()) {
                    2
                } else {
                    1
                };
                let exact = if e.exact_position > 0 {
                    e.exact_position
                } else {
                    auto
                };
                (exact, e.complete_position)
            }
            EngineType::English => return (1, 1),
        };
        let or = |v: usize, d: usize| if v > 0 { v } else { d };
        (
            or(exact, wind_config::config::default_english_exact_position()),
            or(
                complete,
                wind_config::config::default_english_complete_position(),
            ),
        )
    }

    /// 当前活跃引擎类型（必要时懒加载）
    pub fn current_engine_type(&self) -> Option<EngineType> {
        self.active_engine().map(|e| e.engine_type())
    }

    /// 指定方案已加载引擎的类型（内存查，无 IO）。未加载返回 None。
    ///
    /// 与 [`schema_engine_type`](Self::schema_engine_type) 的区别是后者每次都
    /// 读文件 + 解析 TOML，不可放进逐键路径；本函数只查已加载引擎表。
    /// 供 overlay 类模式（临拼等）按目标方案类型分流取数策略。
    pub fn loaded_engine_type(&self, schema_id: &str) -> Option<EngineType> {
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .map(|e| e.engine.engine_type())
    }

    /// 确保指定方案可加载（懒加载）。用于 overlay 模式（特殊模式等）激活前的可用性校验。
    pub fn ensure_schema(&self, schema_id: &str) -> bool {
        self.ensure_loaded(schema_id)
    }

    /// 顶码上屏：超过满码长时取前 N 码首选上屏，返回 (上屏文本, 剩余编码)。
    /// 仅码表/混输引擎按 top_code_commit 实现，其余返回 None。
    pub fn handle_top_code(&self, input: &str) -> Option<(String, String)> {
        let engine = self.active_engine()?;
        let r = engine.handle_top_code(input)?;
        // 通路③：顶码。协调器让顶码先于候选刷新执行，漏接则 `block_commit` 对超码长输入失效。
        if self.english_vetoes_commit(&engine, input) {
            return None;
        }
        Some(r)
    }

    /// 满码自动上屏「显示态」复评（透传到活跃引擎）：据已过滤/重排/shadow 的显示候选复评，
    /// 引擎按未过滤候选因生僻同码字判不唯一而否决时，智能过滤后剩唯一精确全码则放行上屏。
    /// 活跃方案版本；overlay/特殊模式见 [`Self::recheck_auto_commit_for`]。
    pub fn recheck_auto_commit(
        &self,
        input: &str,
        candidates: &[wind_candidate::Candidate],
    ) -> Option<String> {
        self.recheck_auto_commit_for(&self.active_schema_id(), input, candidates)
    }

    /// [`Self::recheck_auto_commit`] 的 overlay/特殊模式版本：按指定方案取引擎而非
    /// `active_engine()`（后者问的是主方案）；活跃方案版本即本函数传活跃方案 id。
    ///
    /// `update_special_candidates` 靠它补齐与主路径对称的「shadow 之后回头复核唯一性」：引擎
    /// 首轮意向判在 shadow 生效之前的候选上，用户用自定义词条替换内置词条后会误判「不唯一」，
    /// 无人复评就永远上不了屏。
    ///
    /// 英文否决（`english_vetoes_commit_for`）必须接：它按 `input` 原串查英文引擎，overlay
    /// 引用的码表方案同样可能开了 `block_commit`。
    pub fn recheck_auto_commit_for(
        &self,
        schema_id: &str,
        input: &str,
        candidates: &[wind_candidate::Candidate],
    ) -> Option<String> {
        let engine = self.engine_for(schema_id)?;
        let r = engine.recheck_auto_commit(input, candidates)?;
        if self.english_vetoes_commit_for(schema_id, &engine, input) {
            return None;
        }
        Some(r)
    }

    /// 活跃引擎是否存在比 `input` 更长的后继编码（码表前缀扫描；拼音等默认 false）。
    /// 供短语自动上屏的「无更长后继」判据（码表侧），与短语层 `has_longer_code` 并用。
    pub fn has_longer_code(&self, input: &str) -> bool {
        self.active_engine()
            .map(|e| e.has_longer_code(input))
            .unwrap_or(false)
    }

    /// 临时拼音目标方案 id 的纯解析（不含可加载性门控，供单测）：
    /// 关闭 / 当前方案不适用 → None；否则取 primary_pinyin，空则全拼 "pinyin"。
    ///
    /// 方案适用范围仅码表/混输：拼音方案本身就在打拼音，再叠一层无意义，且会吞掉引导符
    /// （如 `）本该有的标点输出。混输保留——其拼音子方案由方案自身决定，未必等于主拼音方案，
    /// 「混输走全拼 + 临时拼音走双拼」是有效用法。
    fn resolve_temp_pinyin_target(
        enabled: bool,
        engine_type: Option<EngineType>,
        primary_pinyin: &str,
    ) -> Option<String> {
        if !enabled {
            return None;
        }
        if !matches!(
            engine_type,
            Some(EngineType::CodeTable) | Some(EngineType::Mixed)
        ) {
            return None;
        }
        Some(if primary_pinyin.is_empty() {
            wind_config::config::DEFAULT_PINYIN_SCHEMA.to_string()
        } else {
            primary_pinyin.to_string()
        })
    }

    /// 临时拼音目标方案 id：开关读 input.temp_pinyin.enabled，方案读 schema.primary_pinyin
    /// （空=全拼 "pinyin"），且仅码表/混输方案适用（见 resolve_temp_pinyin_target）。
    /// 启用、适用且目标方案可加载时返回 Some(target)，否则 None。
    ///
    /// 所有临时拼音进入点的公共门卫（引导键 / 字母触发 / 直达热键 / 顶屏进模式 /
    /// z-fallback 均先问它），故适用范围判据必须落在这里，放任一调用点都会漏网。
    pub fn temp_pinyin_target(&self) -> Option<String> {
        let enabled = self
            .temp_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enabled;
        let primary = self
            .primary_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let target =
            Self::resolve_temp_pinyin_target(enabled, self.current_engine_type(), &primary)?;
        if self.ensure_loaded(&target) {
            Some(target)
        } else {
            None
        }
    }

    /// 当前方案的按键功能表（`[key_actions]`，方案文件内联 + `schema_overrides` 已合并）。
    ///
    /// 与 [`Self::codetable_settings`] 不同，混输方案不下钻到 primary_schema：按键功能属于
    /// 方案自身的交互属性，混输方案想配就在自己的文件里配。
    ///
    /// 读不到方案时返回空表 = 不覆盖任何键，各键照常走全局链。返回 `Arc`，热路径上不复制整张表。
    pub fn active_key_actions(&self) -> Arc<std::collections::BTreeMap<String, String>> {
        self.key_actions_of(&self.active_schema_id())
    }

    /// 同 [`Self::active_key_actions`]，但取指定方案。
    ///
    /// 只服务「编码类」动词的查询（辅助码触发键、码元、分隔符，见
    /// docs/design/key-resolver-unification.md §4.4）；`special:*` / `temp_pinyin` 这类恒属主方案，
    /// 别拿它做通用分派。
    pub fn key_actions_of(
        &self,
        schema_id: &str,
    ) -> Arc<std::collections::BTreeMap<String, String>> {
        let id = schema_id.to_string();
        if let Some(cached) = self
            .key_actions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            return Arc::clone(cached);
        }
        let table = Arc::new(
            Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref())
                .map(|s| s.key_actions)
                .unwrap_or_default(),
        );
        self.key_actions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Arc::clone(&table));
        table
    }

    /// 当前方案的会话态按键表（`[session_actions]`，方案文件内联 + `schema_overrides`
    /// 已合并）。
    ///
    /// 与 [`Self::active_key_actions`] 的取表规则逐条相同（不下钻 primary_schema、读不到
    /// 方案返回空表 = 不覆盖任何键、返回 `Arc` 避免热路径 clone）。两张表分开而不是合成
    /// 一张带状态维度的表，理由见 `Schema::session_actions` 的文档。
    ///
    /// 空表 = 该方案不覆盖任何会话态键，各键照常走全局
    /// `KeysConfig::effective_session_actions()`。
    pub fn active_session_actions(&self) -> Arc<std::collections::BTreeMap<String, String>> {
        let id = self.active_schema_id();
        if let Some(cached) = self
            .session_actions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            return Arc::clone(cached);
        }
        let table = Arc::new(
            Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref())
                .map(|s| s.session_actions)
                .unwrap_or_default(),
        );
        self.session_actions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Arc::clone(&table));
        table
    }

    /// 告知「有模板引用 `${correct_pinyin}`」（协调器的 `DataNeeds::correct_pinyin`）。只翻共享开关，
    /// 不重建引擎：提示是 `convert` 收尾的后处理，不影响候选集合与排序。
    pub fn set_correct_pinyin_in_templates(&self, on: bool) {
        self.correct_pinyin_in_templates
            .store(on, std::sync::atomic::Ordering::Relaxed);
        self.refresh_pinyin_hint();
    }

    /// 读误读表：内置（`schemas/pinyin_misread.yaml`，经资源层 `user > custom > data` 取一份）
    /// ⊕ 用户追加（用户数据目录 `data/pinyin_misread.yaml`，同键覆盖内置）。文件缺失按空表。
    fn load_misread(
        data_dir: Option<&Path>,
        user_data_dir: Option<&Path>,
    ) -> crate::pinyin::misread::MisreadTable {
        use crate::pinyin::misread::{MISREAD_FILE, MisreadTable};
        let read = |path: Option<std::path::PathBuf>, origin: &str| {
            path.and_then(|p| std::fs::read_to_string(p).ok())
                .map(|src| MisreadTable::parse(&src, origin))
                .unwrap_or_default()
        };
        let builtin = read(
            Config::resolve_schema_resource(data_dir, MISREAD_FILE),
            "builtin",
        );
        let user = read(user_data_dir.map(|d| d.join(MISREAD_FILE)), "user");
        let table = MisreadTable::merge(builtin, user);
        info!("误读表 {} 条", table.len());
        table
    }

    /// 配置重载时同步 `[input.correction]` 与误读表（每次重载都重读两份表文件）。返回是否要
    /// 重建引擎（[`correction_rebuild_needed`]，或「误读」开着时表内容变了）——要就由调用方走
    /// `reload_from_config`；只改提示开关的，这里换掉镜像、翻 [`Self::pinyin_hint`] 即生效。
    pub fn sync_correction(&self, config: &Config) -> bool {
        let table = Self::load_misread(self.data_dir.as_deref(), self.user_text_dir());
        let new = &config.input.correction;
        let mut cfg = self
            .correction_cfg
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut cur = self.misread.lock().unwrap_or_else(|e| e.into_inner());
        let table_changed = (cfg.hint.misread || new.hint.misread) && **cur != table;
        let rebuild = correction_rebuild_needed(&cfg, new) || table_changed;
        *cfg = new.clone();
        *cur = Arc::new(table);
        drop((cfg, cur));
        self.refresh_pinyin_hint();
        rebuild
    }

    /// 注入拼音引擎的误读表：「误读」来源（`input.correction.hint.misread`）勾选且表非空时才给。
    fn misread_for_engines(&self) -> Option<Arc<crate::pinyin::misread::MisreadTable>> {
        let on = self
            .correction_cfg
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hint
            .misread;
        let table = self
            .misread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        (on && !table.is_empty()).then_some(table)
    }

    /// 按当前拼音配置与模板引用重算 [`Self::pinyin_hint`]。
    fn refresh_pinyin_hint(&self) {
        let tpl = self
            .correct_pinyin_in_templates
            .load(std::sync::atomic::Ordering::Relaxed);
        let pinyin = self.pinyin.lock().unwrap_or_else(|e| e.into_inner());
        let corr = self
            .correction_cfg
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.pinyin_hint
            .set(pinyin_hint_on(&pinyin.fuzzy, &corr, tpl));
    }

    /// 拼音引擎当前是否算正确拼音提示（诊断 / 测试用）。
    pub fn pinyin_hint_active(&self) -> bool {
        self.pinyin_hint.get()
    }

    /// 引擎表的失效代次：每次清表或摘除引擎都会推进（诊断 / 测试用，判断有没有重建）。
    pub fn engines_generation(&self) -> u64 {
        self.engines_gen.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 当前方案的行为覆盖三段（`[punct]` / `[candidate]` / `[phrases]`，方案文件内联 +
    /// `schema_overrides` 已合并）。
    ///
    /// 取表规则同 [`Self::active_key_actions`]：混输方案不下钻 `primary_schema`；读不到方案时
    /// 返回 default（一段都不覆盖）；返回 `Arc`（消费点在候选构建热路径上）。
    ///
    /// 见 `docs/design/schema-scoped-behavior.md` §6.5。
    pub fn active_behavior(&self) -> Arc<wind_config::SchemaBehavior> {
        self.behavior_for(&self.active_schema_id())
    }

    /// 指定方案的行为覆盖三段。
    ///
    /// 独立于 [`Self::active_behavior`] 暴露，是因为短语作用域的归属方案不一定是
    /// active——临时英文归 `english` 桶而主方案常是五笔/混输（见
    /// `effective_data_schema`）。按 active 取值会给临英套上主方案的短语开关。
    pub fn behavior_for(&self, schema_id: &str) -> Arc<wind_config::SchemaBehavior> {
        if let Some(cached) = self
            .behavior_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
        {
            return Arc::clone(cached);
        }
        let spec = Arc::new(
            Self::read_schema(
                schema_id,
                self.data_dir.as_deref(),
                self.override_dir.as_deref(),
            )
            .map(|s| s.behavior())
            .unwrap_or_default(),
        );
        self.behavior_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(schema_id.to_string(), Arc::clone(&spec));
        spec
    }

    /// 指定方案的码元字符集（按 id，非活跃方案也能查）。
    ///
    /// 与引擎构建走同一个构造器，区间语法、`leading_chars` 为空等于全集、非法回落全集这些
    /// 规则只有一处。
    ///
    /// 读不到方案时返回 `None`（而非默认 `a-z`）：调用方多半是要「据此提示用户」，
    /// 拿默认值去提示等于凭空报一个不存在的事实。
    pub fn schema_code_char_set(&self, schema_id: &str) -> Option<wind_config::CodeCharSet> {
        let schema = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )?;
        let ct = &schema.engine.codetable;
        Some(wind_config::CodeCharSet::new(
            &ct.input_chars,
            &ct.leading_chars,
            &format!("schema {schema_id}"),
        ))
    }

    /// [`Self::schema_code_char_set`] 加上该方案开着的符号通配键，一次读方案文件。
    ///
    /// 供协调器算「空闲时可作首码的符号」：符号通配键也能作首码，须同码元首码一样从 DLL
    /// 透传标点集里扣掉。纯配置读、不建引擎。
    ///
    /// 通配键只算纯码表方案：混输首位恒字面（扣了也收不到），拼音 / 英文方案虽会经折叠
    /// 继承全局 `wildcard` 开关，但根本不消费它——算进来只会让别的方案下这个符号被白吃。
    /// 非法键不在此告警（建引擎时 `wildcard_char` 已告警过一次）。
    pub fn schema_lead_inputs(
        &self,
        schema_id: &str,
    ) -> Option<(wind_config::CodeCharSet, Option<char>)> {
        let schema = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )?;
        let ct = &schema.engine.codetable;
        let charset = wind_config::CodeCharSet::new(
            &ct.input_chars,
            &ct.leading_chars,
            &format!("schema {schema_id}"),
        );
        let wildcard = if schema.engine.engine_type.eq_ignore_ascii_case("codetable") {
            let global = self
                .codetable
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let eff = Self::codetable_baseline(Some(&schema), &global).resolved(Some(ct));
            eff.wildcard
                .then(|| wind_config::config::parse_wildcard_key(&eff.wildcard_key))
                .flatten()
                .filter(|c| !c.is_ascii_alphabetic())
        } else {
            None
        };
        Some((charset, wildcard))
    }

    /// 所有已安装方案（含 overlay）的 `[key_actions]` 里出现过的键名（并集）。
    ///
    /// 与 [`Self::all_key_action_keys`] 只差枚举源：那个回答「要不要转发 keyup」，用
    /// `available` 足矣；本函数回答「谁占了这个键」，供从透传标点集里减去，减数漏一个就是
    /// 某方案的引导键被透传、那个模式再也进不去，故必须取更大的集合。
    pub fn installed_key_action_keys(&self) -> std::collections::BTreeSet<String> {
        let mut keys = std::collections::BTreeSet::new();
        for id in self.installed_schemas() {
            if let Some(s) =
                Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref())
            {
                keys.extend(s.key_actions.into_keys());
            }
        }
        keys
    }

    /// 所有可用方案的 `[key_actions]` 里出现过的键名（并集，去重）。
    ///
    /// 用于算「需要让 TSF 转发 keyup 的修饰键」。必须是并集：`CompiledHotkeys` 编译一次随
    /// activation 推给 C++，按活跃方案裁剪的话切方案后 C++ 手里是旧表。代价是方案 A 配的键在
    /// 方案 B 里也被转发，无害（纯修饰键 keydown 一律放行，服务端按活跃方案查表落空即不动作）。
    ///
    /// 不走 `key_actions_cache`：该缓存按方案 id 存，而这里要的是跨方案的并集。
    pub fn all_key_action_keys(&self) -> std::collections::BTreeSet<String> {
        self.all_action_keys().0
    }

    /// 所有可用方案的 `[session_actions]` 里出现过的键名（并集，去重）。转发集必须是并集，
    /// 理由同 [`Self::all_key_action_keys`]。
    ///
    /// 并集代价与 key_actions 那张不同：本表含可打印符号键，它们带 `FORWARD_ONLY` 进 keydown
    /// 表，无会话时必须放行给下游按标点处理，否则在没绑该键的方案下丢键。
    pub fn all_session_action_keys(&self) -> std::collections::BTreeSet<String> {
        self.all_action_keys().1
    }

    /// 两张表的键名并集，一次遍历同时收齐。
    ///
    /// 两者都要 `read_schema` 扫全部可用方案，合并免得同一批文件读两遍。
    fn all_action_keys(
        &self,
    ) -> (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
    ) {
        let ids = self
            .available
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut keys = std::collections::BTreeSet::new();
        let mut session = std::collections::BTreeSet::new();
        for id in ids {
            if let Some(s) =
                Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref())
            {
                keys.extend(s.key_actions.into_keys());
                // 会话态侧滤掉显式 `none`，`key_actions` 侧不滤：本集合要据此决定装不装
                // CapsLock 全局钩子，写了 none 就不该装；key_actions 那份多转发一个键宿主无感。
                //
                // 按方案独立判定：方案 A 绑了、方案 B 写 none，并集仍含该键（A 要用）。
                session.extend(
                    s.session_actions
                        .into_iter()
                        .filter(|(_, verb)| wind_config::SessionAction::parse(verb).is_enabled())
                        .map(|(name, _)| name),
                );
            }
        }
        (keys, session)
    }

    /// 活跃方案的有效码表行为配置：全局 `schema.codetable` 经该方案 `[engine.codetable]`
    /// （内联 + `schema_overrides` 合并后）折叠。供协调器读 punct_commit / z_key_repeat 等行为字段。
    pub fn codetable_settings(&self) -> wind_config::CodetableGlobal {
        self.codetable_settings_of(&self.active_schema_id())
    }

    /// 借用版的 [`Self::codetable_settings`]：缓存命中时不克隆整份设置。逐键路径只取一两个
    /// 字段时用它。
    pub fn with_codetable_settings<R>(
        &self,
        f: impl FnOnce(&wind_config::CodetableGlobal) -> R,
    ) -> R {
        self.with_codetable_settings_of(&self.active_schema_id(), f)
    }

    /// [`Self::codetable_settings`] 的按方案版本：供 overlay/特殊模式取指定方案的设置
    /// （见 `engine_for`）。判混输也按 `loaded_engine_type(schema_id)`，不问活跃方案。
    fn codetable_settings_of(&self, schema_id: &str) -> wind_config::CodetableGlobal {
        self.with_codetable_settings_of(schema_id, Clone::clone)
    }

    /// 借用版的 [`Self::codetable_settings_of`]。`f` 在缓存锁内执行，别在里面回调本管理器。
    pub fn with_codetable_settings_of<R>(
        &self,
        schema_id: &str,
        f: impl FnOnce(&wind_config::CodetableGlobal) -> R,
    ) -> R {
        let mixed = matches!(self.loaded_engine_type(schema_id), Some(EngineType::Mixed));
        let key = (schema_id.to_string(), mixed);
        if let Some(s) = self
            .codetable_settings_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return f(s);
        }
        let generation = self
            .codetable_settings_gen
            .load(std::sync::atomic::Ordering::Acquire);
        let s = self.resolve_codetable_settings(schema_id, mixed);
        let out = f(&s);
        self.cache_codetable_settings(key, s, generation);
        out
    }

    /// 清 `codetable_settings_cache` 并推进失效代次（持锁完成）。
    fn invalidate_codetable_settings_cache(&self) {
        let mut cache = self
            .codetable_settings_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.clear();
        self.codetable_settings_gen
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// 回填 `codetable_settings_cache`：算之前记下的代次已过期（期间被清过）就丢弃。
    fn cache_codetable_settings(
        &self,
        key: (String, bool),
        s: wind_config::CodetableGlobal,
        generation: u64,
    ) -> bool {
        let mut cache = self
            .codetable_settings_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self
            .codetable_settings_gen
            .load(std::sync::atomic::Ordering::Acquire)
            != generation
        {
            return false;
        }
        cache.insert(key, s);
        true
    }

    /// 不带缓存的解析（读方案文件）。
    fn resolve_codetable_settings(
        &self,
        schema_id: &str,
        mixed: bool,
    ) -> wind_config::CodetableGlobal {
        let global = self
            .codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // 混输方案自身无独立 codetable 配置，override 从其 primary_schema（主码表方案）读取
        let resolve_id = if mixed {
            Self::read_schema(
                schema_id,
                self.data_dir.as_deref(),
                self.override_dir.as_deref(),
            )
            .map(|s| s.engine.mixed.primary_schema)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| schema_id.to_string())
        } else {
            schema_id.to_string()
        };
        Self::resolve_codetable(
            &resolve_id,
            self.data_dir.as_deref(),
            &global,
            self.override_dir.as_deref(),
        )
    }

    /// 拼音自动造词配置（[schema.pinyin.auto_learn]）。
    pub fn auto_learn_settings(&self) -> wind_config::config::AutoLearnConfig {
        self.pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .auto_learn
            .clone()
    }

    /// 拼音词频衰减参数（用户配置；0 表示使用 store 默认值）。
    pub fn pinyin_freq_profile(&self) -> wind_store::freq::FreqProfile {
        let pf = self.pinyin.lock().unwrap_or_else(|e| e.into_inner());
        let def = wind_store::freq::FreqProfile::default();
        wind_store::freq::FreqProfile {
            base_scale: if pf.frequency.base_scale > 0.0 {
                pf.frequency.base_scale
            } else {
                def.base_scale
            },
            half_life_hours: if pf.frequency.half_life > 0.0 {
                pf.frequency.half_life
            } else {
                def.half_life_hours
            },
            recency_peak: pf.frequency.recency_peak.max(0.0),
        }
    }

    /// 当前方案是否为英文方案（`[engine] type = "english"`）。
    ///
    /// 判「当前方案」而非「候选来源是英文」：`CandidateSource::English` 在混输、快捷输入、
    /// 临时英文里都会出现，那些场景下用户正在写中文，按英文方案的规矩处理是错的。
    pub fn active_is_english(&self) -> bool {
        matches!(
            self.schema_engine_type(&self.active_schema_id()).as_deref(),
            Some("english")
        )
    }

    /// 当前方案的词频衰减参数：英文方案取英文段，其余取码表段。
    /// 拼音走 [`Self::pinyin_freq_profile`]，不经这里（消费点按引擎类型分派）。
    pub fn active_freq_profile(&self) -> wind_store::freq::FreqProfile {
        self.freq_profile_for(&self.active_schema_id())
    }

    /// 指定方案的词频衰减参数。与 [`Self::freq_settings_for`] 同一套分流，
    /// 两者必须一起改——一个按方案取 strategy、另一个仍按 active 取 half_life 的话，
    /// 特殊方案会用自己的策略配上主方案的衰减速度。
    pub fn freq_profile_for(&self, schema_id: &str) -> wind_store::freq::FreqProfile {
        if matches!(
            self.schema_engine_type(schema_id).as_deref(),
            Some("english")
        ) {
            return self.english_freq_profile();
        }
        let global = self
            .codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let ct = Self::resolve_codetable(
            schema_id,
            self.data_dir.as_deref(),
            &global,
            self.override_dir.as_deref(),
        );
        let def = wind_store::freq::FreqProfile::default();
        wind_store::freq::FreqProfile {
            half_life_hours: Self::resolve_half_life(ct.frequency.half_life, def.half_life_hours),
            ..def
        }
    }

    /// 英文方案的词频衰减参数。与码表/拼音三者互不相干，各读各的 `half_life`。
    pub fn english_freq_profile(&self) -> wind_store::freq::FreqProfile {
        let hl = self
            .english
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frequency
            .half_life;
        let def = wind_store::freq::FreqProfile::default();
        wind_store::freq::FreqProfile {
            half_life_hours: Self::resolve_half_life(hl, def.half_life_hours),
            ..def
        }
    }

    /// 码表/混输的词频衰减参数。与拼音完全独立，不读拼音段任何字段：设置页上是两个独立控件，
    /// 一个控件一个值，别做「码表为 0 时回落拼音」的回落链。`base_scale`/`recency_peak` 取 store
    /// 默认（位置提升模型不读它们）。
    ///
    /// 仅 `strategy = "position"` 时被消费；`top`/`step` 直接比 `count`/`last_used`。
    pub fn codetable_freq_profile(&self) -> wind_store::freq::FreqProfile {
        let ct = self
            .codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frequency
            .half_life;
        let def = wind_store::freq::FreqProfile::default();
        wind_store::freq::FreqProfile {
            half_life_hours: Self::resolve_half_life(ct, def.half_life_hours),
            ..def
        }
    }

    /// 半衰期回落（纯映射，便于单测）：`own > 0` 用自己的，否则用 `inherited`。
    fn resolve_half_life(own: f64, inherited: f64) -> f64 {
        if own > 0.0 { own } else { inherited }
    }

    /// 某方案当前生效的码表配置（基线 + 方案覆盖折叠后的全实值）。
    ///
    /// 设置页需要它作起点快照：方案里的行为字段是 `Option`，UI 得知道「不设置的话实际是什么」，
    /// 而基线分普通/特殊两种（见 [`Self::codetable_baseline`]），UI 侧算不出来。
    /// 与 `resolve_codetable` 只差取全局镜像的方式（那个是关联函数，供 `build_engine` 用）。
    pub fn effective_codetable(&self, schema_id: &str) -> wind_config::CodetableGlobal {
        let global = self
            .codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Self::resolve_codetable(
            schema_id,
            self.data_dir.as_deref(),
            &global,
            self.override_dir.as_deref(),
        )
    }

    /// 某方案不含 override 层的码表配置：全局基线 ⊕ 方案文件内联，折叠后的全实值。
    ///
    /// 设置页三态控件的「跟随值」（取消覆盖后会变成的值）；[`Self::effective_codetable`] 是
    /// 当前实际在跑的值。UI 自己算不出来，理由同那里。
    pub fn followed_codetable(&self, schema_id: &str) -> wind_config::CodetableGlobal {
        let global = self
            .codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // override_dir 传 None ＝ `read_schema` 不合并 override 层。
        Self::resolve_codetable(schema_id, self.data_dir.as_deref(), &global, None)
    }

    /// 解析某方案的有效码表配置：全局基线 + 方案 `[engine.codetable]` 行为（内联 + override
    /// 已在 `read_schema` 经 `merge_toml` 合并）逐字段折叠。读不到方案时原样返回全局基线。
    fn resolve_codetable(
        schema_id: &str,
        data_dir: Option<&Path>,
        global: &wind_config::CodetableGlobal,
        override_dir: Option<&Path>,
    ) -> wind_config::CodetableGlobal {
        let schema = Self::read_schema(schema_id, data_dir, override_dir);
        let base = Self::codetable_baseline(schema.as_ref(), global);
        base.resolved(schema.as_ref().map(|s| &s.engine.codetable))
    }

    /// 折叠方案行为时用哪份基线：普通方案用全局 `schema.codetable`，overlay 方案用内置默认值。
    ///
    /// overlay 方案（快符等）是几十条的小符号表，全局基线是按数万条的全码表调的；继承的话
    /// 用户改五笔的设置，快符会不知不觉跟着变。判据是 `[overlay]` 段存在，不是
    /// `[schema].hidden`（那只管列不列进切换列表；hidden 但非 overlay 的码表方案跟随全局）。
    fn codetable_baseline(
        schema: Option<&Schema>,
        global: &wind_config::CodetableGlobal,
    ) -> wind_config::CodetableGlobal {
        if schema.map(|s| s.overlay.is_some()).unwrap_or(false) {
            Self::special_schema_baseline()
        } else {
            global.clone()
        }
    }

    /// 特殊方案的折叠基线。
    ///
    /// 不是 `CodetableGlobal::default()`：那是结构体零值，大量测试依赖它（顶码/标点上屏都关），
    /// 两个概念别共用一处定义。
    ///
    /// 取值与 `data/config.toml` 出厂值一致，除 z 的两项（`z_key_repeat` / `z_key_action`）：
    /// `z` 在小符号表里多半是正经编码，不能被抢走。「不继承全局」的意义是用户改了全局后特殊
    /// 方案不跟着变，不是让出厂表现不同。
    fn special_schema_baseline() -> wind_config::CodetableGlobal {
        wind_config::CodetableGlobal {
            top_code_commit: true,
            punct_commit: true,
            single_code_complete: true,
            z_key_repeat: false,
            z_key_action: String::new(),
            ..Default::default()
        }
    }

    /// 拆字配置（`[engine.chaizi]`：db/font 路径 + DWrite 家族名）。来源方案与编码段
    /// 同源（`code_source_schema`）：码表方案只用自己的拆字配置——没配置就没有拆字，
    /// 不回落主码表（看其它方案的拆字对本方案无意义）；拼音回落全局主码表、混输取其
    /// 主码表成员。无配置返回 None。路径相对 `schemas/`（用户目录优先）。
    pub fn chaizi_spec(&self) -> Option<wind_config::schema::ChaiziSpec> {
        let id = self.code_source_schema();
        if id.is_empty() {
            return None;
        }
        let c = self.schema_def_cached(&id)?.engine.chaizi.clone();
        c.is_configured().then_some(c)
    }

    /// 预填按键路径要用的方案级缓存（活跃方案及其混输成员的定义、辅助码设置）。
    /// 在方案失效 / 配置重载 / 切方案之后、非按键线程上调，让失效后的第一键也不读盘。
    pub fn warm_hot_schema_fields(&self) {
        let active = self.active_schema_id();
        if active.is_empty() {
            return;
        }
        let _ = self.schema_def_cached(&active);
        for member in [
            self.mixed_primary_schema(&active),
            self.mixed_secondary_schema(&active),
        ]
        .into_iter()
        .flatten()
        {
            let _ = self.schema_def_cached(&member);
        }
        let _ = self.aux_code_settings_cached();
    }

    /// 活跃方案的辅助码生效设置：全局基线 `[schema.pinyin.aux_code]` 折叠方案段
    /// `[engine.aux_code]`（含 `schema_overrides` 深合并后的结果）。
    ///
    /// 一次 `read_schema` 出全部值（按键线程上不能多次读盘）。
    ///
    /// 与拆字不同：辅助码过滤的对象是拼音候选，触发时活跃方案即拼音方案本身，
    /// 故取 `active_schema_id()`（而非 `code_source_schema()`——那是码表字形反查的归属）。
    ///
    /// `files` 逐条解析，`schemas/` 基准、用户目录优先——与 `read_schema` 同源
    /// `self.data_dir`，故方案从哪读、码表也从哪解析；缺失逐条 warn，不中断其余文件。
    pub fn aux_code_settings(&self) -> AuxCodeSettings {
        self.aux_code_settings_of(&self.active_schema_id())
    }

    /// 同 [`Self::aux_code_settings`]，但走缓存：逐键路径专用（直接辅助码每个双拼按键都要
    /// 问一次，未开的用户也一样）。不带缓存的那个每次读盘 + 解析 TOML + 探测来源文件。
    ///
    /// 失效点见 `aux_settings_cache`：设置页写方案覆盖层、重载配置都会清；绕过这些入口直接改
    /// 磁盘文件（手改 `schema_overrides`）要等下一次重载才生效，与 `freq_settings_for` 同口径。
    pub fn aux_code_settings_cached(&self) -> AuxCodeSettings {
        self.aux_code_settings_cached_of(&self.active_schema_id())
    }

    /// 同 [`Self::aux_code_settings_of`]，但走缓存（失效点同 [`Self::aux_code_settings_cached`]）。
    /// 按辅助码触发键进入时用：那是按键路径，按来源方案取（临拼在五笔方案下引用拼音方案）。
    pub fn aux_code_settings_cached_of(&self, schema_id: &str) -> AuxCodeSettings {
        if let Some(s) = self
            .aux_settings_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
        {
            return s.clone();
        }
        let generation = self
            .aux_settings_gen
            .load(std::sync::atomic::Ordering::Acquire);
        let s = self.aux_code_settings_of(schema_id);
        self.cache_aux_settings(schema_id.to_string(), s.clone(), generation);
        s
    }

    /// 方案定义（合并 override 后），走 `schema_def_cache`。只取方案级标量字段，见该字段文档。
    fn schema_def_cached(&self, schema_id: &str) -> Option<Arc<Schema>> {
        if let Some(v) = self
            .schema_def_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
        {
            return v.clone();
        }
        let generation = self
            .schema_def_gen
            .load(std::sync::atomic::Ordering::Acquire);
        let def = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
        .map(Arc::new);
        let mut cache = self
            .schema_def_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self
            .schema_def_gen
            .load(std::sync::atomic::Ordering::Acquire)
            == generation
        {
            cache.insert(schema_id.to_string(), def.clone());
        }
        def
    }

    /// 清 `schema_def_cache` 并推进失效代次（持锁完成）。
    fn invalidate_schema_def_cache(&self) {
        let mut cache = self
            .schema_def_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.clear();
        self.schema_def_gen
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// 回填 `aux_settings_cache`：`generation` 是算之前记下的失效代次，期间被清过就丢弃。
    /// 比对与插入在同一把锁下，与 [`Self::invalidate_aux_settings_cache`] 互斥。
    fn cache_aux_settings(&self, id: String, s: AuxCodeSettings, generation: u64) -> bool {
        let mut cache = self
            .aux_settings_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self
            .aux_settings_gen
            .load(std::sync::atomic::Ordering::Acquire)
            != generation
        {
            return false;
        }
        cache.insert(id, s);
        true
    }

    /// 造词是否启用（码表/混输方案 + 开关开启）。拼音方案走 `[pinyin.auto_learn]` 的
    /// 选词即学路线，不进造词。
    pub fn auto_phrase_enabled(&self) -> bool {
        !self.is_pinyin() && self.codetable_settings().auto_phrase.enabled
    }

    /// 当前活跃方案下的辅助码 / 自动造词态。有缓存（按活跃方案 id），见 `active_facts_cache`。
    pub fn active_data_facts(&self) -> ActiveDataFacts {
        let active = self.active_schema_id();
        if let Some(f) = self
            .active_facts_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&active)
        {
            return *f;
        }
        // 锁外算：`is_pinyin` 可能懒加载活跃引擎，不能握着缓存锁等它。
        let generation = self
            .data_facts_gen
            .load(std::sync::atomic::Ordering::Acquire);
        #[cfg(test)]
        self.data_facts_builds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let facts = ActiveDataFacts {
            aux_code_in_use: !self.aux_code_schemas_in_use().is_empty(),
            auto_phrase: self.auto_phrase_enabled(),
        };
        let mut cache = self
            .active_facts_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self
            .data_facts_gen
            .load(std::sync::atomic::Ordering::Acquire)
            == generation
        {
            cache.insert(active, facts);
        }
        facts
    }

    /// 清事实缓存并推进失效代次（持锁完成，与回填互斥）。
    fn invalidate_data_facts(&self) {
        let mut active = self
            .active_facts_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        active.clear();
        self.data_facts_gen
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// 辅码设置缓存的失效代次：辅码来源 / 设置变了就推进，供外部缓存判断自己是否过期。
    pub fn aux_settings_generation(&self) -> u64 {
        self.aux_settings_gen
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// 清 `aux_settings_cache` 并推进失效代次（持锁完成，见 `aux_settings_gen`）。
    fn invalidate_aux_settings_cache(&self) {
        let mut cache = self
            .aux_settings_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.clear();
        self.aux_settings_gen
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// 同 [`Self::aux_code_settings`]，但取指定方案。
    ///
    /// 辅助码可以从临拼进入，而临拼在五笔方案下引用的是拼音方案——码表配在那个方案的
    /// `[engine.aux_code].files` 里。按活跃方案取的话 `files` 恒空，门卫静默拒绝进入。
    pub fn aux_code_settings_of(&self, schema_id: &str) -> AuxCodeSettings {
        let global = self
            .pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .aux_code
            .clone();
        let id = schema_id.to_string();
        let schema = (!id.is_empty())
            .then(|| Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref()))
            .flatten();
        let is_shuangpin = schema.as_ref().is_some_and(|s| {
            s.engine.engine_type.eq_ignore_ascii_case("pinyin")
                && s.engine.pinyin.scheme.eq_ignore_ascii_case("shuangpin")
        });
        let spec = schema.map(|s| s.engine.aux_code);
        let resolved = global.resolved(spec.as_ref());
        // 直接辅助码只做双拼（设计 §2：全拼 `lim` 既可能是 li+m 也可能是「厘米」简拼）。
        // 全拼写了 `direct = true` 不生效——这是配置错误，报一次足够定位（与来源告警同一节流）。
        let direct = resolved.enabled && resolved.direct;
        if direct && !is_shuangpin && self.first_aux_source_warn(schema_id, "\0direct") {
            tracing::warn!("直接辅助码只支持双拼方案，方案 {schema_id} 的 direct = true 不生效");
        }
        let direct = direct && is_shuangpin;
        let continuous = direct && spec.as_ref().and_then(|s| s.continuous) != Some(false);
        let preedit_separator = spec
            .as_ref()
            .and_then(|s| s.preedit_separator.as_deref())
            .and_then(|sep| {
                if crate::engine::aux_preedit_separator_valid(sep) {
                    return Some(Arc::from(sep));
                }
                if self.first_aux_source_warn(schema_id, "\0preedit_separator") {
                    tracing::warn!(
                        "方案 {schema_id} 的 preedit_separator = {sep:?} 不在可选字符（空格 · - _ |）内，改用空格"
                    );
                }
                None
            });
        // 关闭时不解析路径：省掉 N 次目录探测，也避免为一个用不上的功能刷 warn。
        let sources = if resolved.enabled {
            spec.map(|c| {
                c.files
                    .iter()
                    .filter_map(|entry| {
                        if let Some(id) =
                            entry.strip_prefix(wind_config::schema::AUX_SCHEMA_SOURCE_PREFIX)
                        {
                            let id = id.trim();
                            if id.is_empty() || id == schema_id {
                                if self.first_aux_source_warn(schema_id, entry) {
                                    tracing::warn!("辅助码来源无效（空或引用自己）: {entry}");
                                }
                                return None;
                            }
                            if self.schema_engine_type(id).as_deref() != Some("codetable") {
                                if self.first_aux_source_warn(schema_id, entry) {
                                    tracing::warn!("辅助码来源方案不存在或不是码表方案: {id}");
                                }
                                return None;
                            }
                            return Some(AuxSource::Schema(id.to_string()));
                        }
                        let p = wind_config::Config::resolve_schema_resource(
                            self.data_dir.as_deref(),
                            entry,
                        );
                        if p.is_none() && self.first_aux_source_warn(schema_id, entry) {
                            tracing::warn!(
                                "辅助码文件不存在（用户/系统 schemas 目录均未找到）: {entry}"
                            );
                        }
                        p.map(AuxSource::File)
                    })
                    .collect()
            })
            .unwrap_or_default()
        } else {
            Vec::new()
        };
        AuxCodeSettings {
            enabled: resolved.enabled,
            max_phrase_len: resolved.max_phrase_len,
            direct,
            continuous,
            preedit_separator,
            sources,
        }
    }

    /// 「辅助码来源条目无效」这条告警是否头一次报（按 方案 + 条目 记）。这是一次性的配置
    /// 错误，不节流就每按一次辅助码键都打一条；改了配置后条目文本变了，自然会再报。
    fn first_aux_source_warn(&self, schema_id: &str, entry: &str) -> bool {
        self.aux_source_warned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(format!("{schema_id}\0{entry}"))
    }

    /// 设置页「辅助码来源」的可选项：码表方案（排除 `exclude`，即请求方自己）+ 码表文件。
    pub fn aux_code_source_options(&self, exclude: &str) -> AuxCodeSourceOptions {
        let schemas = self
            .installed_schemas()
            .into_iter()
            .filter(|id| {
                id != exclude && self.schema_engine_type(id).as_deref() == Some("codetable")
            })
            .map(|id| {
                let name = self.schema_name(&id);
                (id, name)
            })
            .collect();
        let files = wind_config::Config::list_schema_resource_dir(
            self.data_dir.as_deref(),
            "aux_code",
            ".txt",
        );
        AuxCodeSourceOptions { schemas, files }
    }

    /// 活跃方案的词频排序设置。等价于 `freq_settings_for(active_schema_id())`。
    pub fn freq_settings(&self) -> FreqSettings {
        self.freq_settings_for(&self.active_schema_id())
    }

    /// 指定方案的词频排序设置（frequency.md §3/§8）。按引擎类型分：
    /// 英文取 `schema.english.frequency`，拼音取 `schema.pinyin.frequency`，
    /// 其余取该方案折叠后的码表调频段（全局基线 + 方案 `[engine.codetable.frequency]`）。
    ///
    /// 取 `schema_id` 而非恒用 active：特殊模式是 overlay，`active` 仍是主方案，而它引用的
    /// 方案有自己的词库与调频诉求（快符表要稳定顺序、生僻字表要学习），照 active 取会让
    /// 它跟着五笔走。
    ///
    /// 按 id 缓存（reload / invalidate 时清空），故每方案只解析一次方案文件。
    pub fn freq_settings_for(&self, schema_id: &str) -> FreqSettings {
        let id = schema_id.to_string();
        {
            let cache = self.freq_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = cache.get(&id) {
                return s.clone();
            }
        }
        // 英文记账口径按候选来源生效、不按当前方案（混输里的英文候选同样读它），故在分流
        // 之外取一次、三个分支共用。
        let english_code_by_input = {
            let en = self.english.lock().unwrap_or_else(|e| e.into_inner());
            en.frequency.code_scope == "input"
        };
        // 同上：跨引擎共用一个值。
        let charsets = Arc::clone(&self.charsets.lock().unwrap_or_else(|e| e.into_inner()));
        let engine_type = self.schema_engine_type(&id);
        let settings = match engine_type.as_deref() {
            Some("english") => {
                let en = self.english.lock().unwrap_or_else(|e| e.into_inner());
                FreqSettings {
                    enabled: en.frequency.enabled,
                    record_always: en.frequency.record_always,
                    strategy: Self::parse_freq_strategy(&en.frequency.strategy),
                    // 英文没有「简码位」：一个 a 后面跟的是几万个词而不是钦定首选，
                    // 套用码表那套按码长分级的首选保护只会锁死前几位不让调频。
                    protect: ProtectPolicy::NONE,
                    promote_prefix: PromotePrefix::parse(&en.frequency.promote_prefix),
                    english_code_by_input,
                    charsets: Arc::clone(&charsets),
                }
            }
            Some("pinyin") => {
                let pf = self.pinyin.lock().unwrap_or_else(|e| e.into_inner());
                // 拼音 strategy/protect 字段不参与（仅码表 used-first 排序用），取默认。
                FreqSettings {
                    enabled: pf.frequency.enabled,
                    record_always: pf.frequency.record_always,
                    strategy: FreqStrategy::Step,
                    protect: ProtectPolicy::NONE,
                    promote_prefix: PromotePrefix::parse(&pf.frequency.promote_prefix),
                    english_code_by_input,
                    charsets: Arc::clone(&charsets),
                }
            }
            _ => {
                // 按方案折叠后的调频段，不是全局镜像：基线（普通方案=全局、特殊方案=
                // 内置默认）叠加该方案 `[engine.codetable.frequency]` 的稀疏覆盖。
                let global = self
                    .codetable
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let ct = Self::resolve_codetable(
                    &id,
                    self.data_dir.as_deref(),
                    &global,
                    self.override_dir.as_deref(),
                );
                FreqSettings {
                    // 码表默认 `all`：其前缀补全已由 `freq_tier` 分到独立档位、跨不到精确档
                    // 之前，无需再按语义单元收窄。
                    promote_prefix: PromotePrefix::parse(&ct.frequency.promote_prefix),
                    english_code_by_input,
                    charsets: Arc::clone(&charsets),
                    enabled: ct.frequency.enabled,
                    record_always: ct.frequency.record_always,
                    strategy: Self::parse_freq_strategy(&ct.frequency.strategy),
                    protect: ProtectPolicy {
                        by_len: [
                            ct.frequency.protect_top_n_len1,
                            ct.frequency.protect_top_n_len2,
                            ct.frequency.protect_top_n_len3,
                        ],
                        fallback: ct.frequency.protect_top_n,
                    },
                }
            }
        };
        self.freq_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, settings.clone());
        settings
    }

    /// 装配字符类 registry：出厂两层目录 + store 里的用户层 + 两个外部引用。
    ///
    /// 全进程只装配这一份：生僻字模式准入（`input.rare_char.include_blocks`）也在这里吃进去，
    /// coordinator 经 [`Self::charsets`] 取同一份，免得免词频与生僻准入对同一个 key 解析出不同的类。
    ///
    /// `store` 为 `None`（无持久化的 headless）时用户层为空——与其它用户数据一致：
    /// 没有 store 就没有用户层，不另找目录兜底。
    fn build_charsets(
        config: &wind_config::Config,
        data_dir: Option<&Path>,
        store: Option<&wind_store::Store>,
    ) -> wind_candidate::CharsetRegistry {
        Self::warn_stale_charset_files();
        // 层序与 `resolve_schema_resource` 一致（后者是 user > custom > data，
        // 这里是靠后的覆盖靠前的）。
        let mut defs = wind_config::charset_def::load_factory(
            data_dir,
            wind_config::Config::custom_data_dir().as_deref(),
        );
        // 用户层按出厂校验（改出厂范围 / 整份替换被忽略并登记问题），再叠上去
        if let Some(store) = store {
            let docs = crate::charset_assembly::validated_user_docs(store, &defs);
            for doc in docs {
                wind_config::charset_def::apply_doc(&mut defs, doc);
            }
        }
        crate::charset_assembly::assemble(
            &defs,
            data_dir,
            crate::charset_assembly::ExternalRefs {
                no_freq: &config.schema.frequency.exclude_blocks,
                in_rare: &config.input.rare_char.include_blocks,
            },
        )
    }

    /// 只重装字符类 registry，不动引擎缓存。
    ///
    /// 必须独立于 `reload_from_config`：那个只在 schema 段变脏时调用，而
    /// `input.rare_char.include_blocks` 在 input 段，改它不会触发。每个无条件重建
    /// `ConfigBundle` 的地方都要调本方法。
    pub fn rebuild_charsets(&self, config: &wind_config::Config) {
        *self.charsets.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(Self::build_charsets(
            config,
            self.data_dir.as_deref(),
            self.store.as_deref(),
        ));
    }

    /// 出厂两层（data + custom）的字符类合并结果——编辑视图与 diff 的基准。
    ///
    /// 与装配用的是同一个 `load_factory`：基准若另算一份，diff 出来的「用户新增」
    /// 可能是定制层本来就有的，用户「恢复默认」就把定制层一并丢了。
    pub fn charset_factory(
        &self,
    ) -> std::collections::BTreeMap<String, wind_config::charset_def::MergedClass> {
        wind_config::charset_def::load_factory(
            self.data_dir.as_deref(),
            wind_config::Config::custom_data_dir().as_deref(),
        )
    }

    /// 直接换掉字符类 registry。
    ///
    /// 生产上由 [`Self::rebuild_charsets`] 从配置装配；本方法留给测试夹具与将来
    /// 「设置页改完 charsets/ 立即热载」那条路径——它们手里已经有现成的 registry，
    /// 不必再走一遍目录扫描。
    pub fn set_charsets(&self, reg: Arc<wind_candidate::CharsetRegistry>) {
        *self.charsets.lock().unwrap_or_else(|e| e.into_inner()) = reg;
    }

    /// 用户配置目录里还留着已不再被读取的旧落点时提醒一句（文件还在、程序不读，不提醒就是
    /// 静默失效）：
    ///
    /// | 旧落点 | 现在在哪 |
    /// |---|---|
    /// | `schemas/common_chars.txt` | `charsets/common_han.yaml` 的列表体 |
    /// | `charsets/*.yaml` | `data/charsets/*.yaml`（设置页「新建分类…」对话框里加载） |
    fn warn_stale_charset_files() {
        let Some(dir) = wind_config::Config::user_config_dir() else {
            return;
        };
        let stale = dir.join("schemas").join("common_chars.txt");
        if stale.is_file() {
            warn!(
                "{} 已不再生效——常用字表现在在 charsets/common_han.yaml 里。要沿用你的名单，\
                 请在设置页「字符集」→「新建分类…」里用「选择其它文件…」加载它",
                stale.display()
            );
        }
        // 目录还在但里面没有字表 ⇒ 没有可导入的东西，提示只会年年响。
        let stale_dir = dir.join(wind_config::charset_def::CHARSETS_DIR_NAME);
        if dir_has_yaml(&stale_dir) {
            warn!(
                "{} 已不再被读取——字符分类的个人调整现在在用户数据目录 data/charsets/ 里。\
                 里面的文件可在设置页「字符集」→「新建分类…」里用「选择其它文件…」逐个加载",
                stale_dir.display()
            );
        }
    }

    /// 当前的字符类 registry。coordinator 的生僻字准入与类型列都从这里取。
    pub fn charsets(&self) -> Arc<wind_candidate::CharsetRegistry> {
        Arc::clone(&self.charsets.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// 词频策略字符串 → 枚举（纯映射，便于单测）。
    fn parse_freq_strategy(s: &str) -> FreqStrategy {
        match s {
            "top" => FreqStrategy::Top,
            "position" => FreqStrategy::Position,
            _ => FreqStrategy::Step,
        }
    }

    /// 同 [`Self::convert_with`]，但把 [`ConvertOptions`] 交给引擎。
    ///
    /// 生僻字模式用它在截断之前筛掉常用字（`ConvertOptions::admit`）。不认这个选项的引擎
    /// 转发到 `convert`，调用方拿到结果后仍有一道最终过滤，下推只为够数与快。
    pub fn convert_with_opts(
        &self,
        schema_id: &str,
        input: &str,
        max_candidates: usize,
        opts: ConvertOptions,
    ) -> ConvertResult {
        if !self.ensure_loaded(schema_id) {
            return ConvertResult::default();
        }
        let engine = self.loaded_engine(schema_id);
        if let Some(e) = &engine {
            self.push_blocked_words(e.as_ref());
        }
        match engine {
            Some(e) => e
                .convert_with_opts(input, max_candidates, opts)
                .unwrap_or_else(|err| {
                    warn!("convert_with_opts error: {}", err);
                    ConvertResult::default()
                }),
            None => ConvertResult::default(),
        }
    }

    /// 用指定方案引擎转换（不改变当前活跃方案，必要时懒加载）。
    /// 用于临时拼音等 overlay：码表模式下临时借用拼音引擎。
    pub fn convert_with(
        &self,
        schema_id: &str,
        input: &str,
        max_candidates: usize,
    ) -> ConvertResult {
        let mut timer = crate::pinyin::ConvertStageTimer::new();
        if !self.ensure_loaded(schema_id) {
            return ConvertResult::default();
        }
        timer.mark(format!("cvwith:{schema_id}"));
        let engine = self.loaded_engine(schema_id);
        if let Some(e) = &engine {
            self.push_blocked_words(e.as_ref());
        }
        match engine {
            Some(e) => self
                .convert_unblocked(e.as_ref(), schema_id, input, max_candidates)
                .unwrap_or_else(|err| {
                    warn!("convert_with error: {}", err);
                    ConvertResult::default()
                }),
            None => ConvertResult::default(),
        }
    }

    /// 用指定方案的引擎枚举码表首 `limit` 条候选（特殊模式「进入即展示」浏览）。
    /// 方案未加载或引擎无浏览语义（如拼音）时返回空。
    pub fn enumerate_with(&self, schema_id: &str, limit: usize) -> Vec<wind_candidate::Candidate> {
        if !self.ensure_loaded(schema_id) {
            return Vec::new();
        }
        let engine = self.loaded_engine(schema_id);
        engine.map(|e| e.enumerate(limit)).unwrap_or_default()
    }

    /// 指定方案「进入即展示」浏览态的呈现上限（`Engine::browse_display_limit`）。
    /// 内存查已加载引擎表，无 IO（区别于 `effective_codetable`，那个每次都读 TOML）。
    /// 未加载 / 无浏览语义返回 None = 不限。
    pub fn browse_display_limit_of(&self, schema_id: &str) -> Option<usize> {
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .and_then(|e| e.engine.browse_display_limit())
    }

    /// 反查 `(code, text)` 在该方案词典里的音节边界；方案未加载/非拼音/查不到均返回 0。
    ///
    /// 不做推断（区别于 `generate_word_pinyin`）：拿现成的码点查取真值。
    /// 供词频列表显示音节格式——词频表本身不带 boundary。
    pub fn syllable_boundary_of(&self, schema_id: &str, code: &str, text: &str) -> u64 {
        if !self.ensure_loaded(schema_id) {
            return 0;
        }
        let engine = self.loaded_engine(schema_id);
        engine.map_or(0, |e| e.syllable_boundary_of(code, text))
    }

    /// 用指定方案的引擎为词语生成带空格的全拼音节码（造词反推、多音字消歧）。
    /// 方案非拼音类、未能加载或无法生成时返回 None（含非汉字的词必落此路）。调用方可回退
    /// `wind_reverse::ReverseLookup::gen_pinyin`——那条路同样产出带空格的音节码，落库前一样要
    /// `split_spaced_code` 拆成扁平 key，不可当扁平码直传。
    pub fn generate_word_pinyin(&self, schema_id: &str, text: &str) -> Option<String> {
        if !self.ensure_loaded(schema_id) {
            return None;
        }
        let engine = self.loaded_engine(schema_id)?;
        engine.generate_word_pinyin(text)
    }

    /// 批量求解 `(code, text)` 的音节边界，兼作拼音词条合法性判据（导入闸口用）。
    ///
    /// 引擎句柄只取一次——导入动辄上万行，逐行 `ensure_loaded` + 加锁不可接受
    /// （同 [`Self::generate_words_pinyin`] 的理由）。返回与 `pairs` 同序等长。
    ///
    /// 方案未加载 / 非拼音方案 → 整批 [`BoundaryResolution::NoInfo`]，即「合法但无边界」
    /// 而非「非法」：码表词库导入正是走这条路。
    pub fn resolve_boundaries(
        &self,
        schema_id: &str,
        pairs: &[(&str, &str)],
    ) -> Vec<BoundaryResolution> {
        if !self.ensure_loaded(schema_id) {
            return vec![BoundaryResolution::NoInfo; pairs.len()];
        }
        let engine = self.loaded_engine(schema_id);
        let Some(engine) = engine else {
            return vec![BoundaryResolution::NoInfo; pairs.len()];
        };
        pairs
            .iter()
            .map(|(code, text)| engine.resolve_boundary(code, text))
            .collect()
    }

    /// 批量版 [`Self::generate_word_pinyin`]：引擎句柄只取一次（`ensure_loaded` +
    /// 一次 `engines` 加锁），其余与逐个调用等价。
    ///
    /// 返回与 `texts` 同序等长；生成不出的位置为 None，由调用方决定回退还是留空。
    pub fn generate_words_pinyin(&self, schema_id: &str, texts: &[&str]) -> Vec<Option<String>> {
        if !self.ensure_loaded(schema_id) {
            return vec![None; texts.len()];
        }
        let engine = self.loaded_engine(schema_id);
        let Some(engine) = engine else {
            return vec![None; texts.len()];
        };
        texts
            .iter()
            .map(|t| engine.generate_word_pinyin(t))
            .collect()
    }

    /// 「被定制版 hide 掉的方案仍被引用」的告警，每个 id 每进程只打一次。
    ///
    /// [`Self::read_schema`] 在热路径上，不节流会刷爆日志；这是定制者的一次性配置错误
    /// （mix 成员 / 特殊模式 / `schema.active` 指向了被 hide 的方案），一次 WARN 足够定位。
    fn warn_hidden_schema_once(schema_id: &str) {
        static WARNED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
            std::sync::OnceLock::new();
        let first = WARNED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(schema_id.to_string());
        if first {
            warn!(
                "方案 {} 已被定制版清单 [schemas] hide，引用它的地方（mix 成员 / 特殊模式 / \
                 schema.active）将跳过它",
                schema_id
            );
        }
    }

    /// 扫描全部资源层（user / custom / data）的 `schemas/` 目录，收集 `*.schema.toml` 的 id。
    ///
    /// 返回值按层序（`user > custom > data`，层内按文件名序），同名 id 只留首次出现的那一层。
    /// 别改成整体 `sort()`：[`Self::resolve_active_schema_id`] 拿它挑降级目标，定制者自带的
    /// 方案必须排在出厂方案前面。
    ///
    /// 已剔除被定制版 hide 的 id，免得下游 `read_schema` 把正常的定制当配置错误告警。
    /// 未按 `is_supported()` 过滤——那要读文件，由调用方按需做。
    fn scan_layer_schema_ids(data_dir: &Path) -> Vec<String> {
        // 合并扫描各层：用户目录可新增第三方方案、定制层可自带方案（read_schema 走
        // resolve_schema_file，本就按同一层序，故都能被读出）。
        let mut dirs: Vec<std::path::PathBuf> = Config::resource_layers_with(Some(data_dir))
            .into_iter()
            .map(|d| d.join("schemas"))
            .collect();
        // 便携版可能把用户目录配成安装目录本身，去重免得同一目录扫两遍。
        let mut seen_dirs = std::collections::HashSet::new();
        dirs.retain(|d| seen_dirs.insert(d.clone()));

        let mut ids: Vec<String> = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();
        for dir in &dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            // 层内按文件名排序：`read_dir` 的返回序是文件系统的，换台机器就可能不同，
            // 而降级落点必须可复现。
            let mut layer_ids: Vec<String> = Vec::new();
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let fname_str = fname.to_string_lossy();
                if let Some(id) = fname_str.strip_suffix(".schema.toml") {
                    if Config::custom_hides_schema(id) {
                        continue;
                    }
                    layer_ids.push(id.to_string());
                }
            }
            layer_ids.sort();
            for id in layer_ids {
                if seen_ids.insert(id.clone()) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    // ───────────────────────── 词典加载 ─────────────────────────

    /// 按层序（`user > custom > data`）解析一个 schemas 相对文件路径，靠前的层胜出。
    ///
    /// 方案文件的唯一入口：`read_schema` 的 `{id}.schema.toml` 与双拼布局的
    /// `shuangpin/{id}.toml` 都走这里。
    ///
    /// 不能换成 `Config::resolve_schema_resource`：那个各层都没有时返回 None，本函数返回
    /// data 层的路径，由调用方 `exists()` 判定后报「方案文件不存在」，换过去缺文件就静默了。
    fn resolve_schema_file(rel: &str, data_dir: &Path) -> std::path::PathBuf {
        let layers = Config::resource_layers_named_with(Some(data_dir));
        for (i, layer) in layers.iter().enumerate() {
            let p = layer.path.join("schemas").join(rel);
            if !p.is_file() {
                continue;
            }
            // 遮蔽判定看全部后续层：用户层盖住 custom 层同样是「覆盖自带数据」。
            let shadowed = layers[i + 1..]
                .iter()
                .any(|b| b.path.join("schemas").join(rel).is_file());
            Config::log_layer_override(layer.name, "schema", rel, &p, shadowed);
            return p;
        }
        data_dir.join("schemas").join(rel)
    }

    /// 读取并解析 schema 文件（仅 TOML）。用户目录优先（见 resolve_schema_file）；
    /// 若 `override_dir/{id}.toml` 存在则深合并到基础方案之上（设置页 override 层 L3）。
    ///
    /// 定制版减法（`custom.toml` 的 `[schemas] hide`）的主拦截点在这里：方案文件读不出来，
    /// 下游各入口（[`Self::schema_supported`]、[`Self::build_engine`]、混输成员、`overlay_modes`）
    /// 自动失效，不必逐处补过滤。与 `[schema].hidden` 正交，见 [`Config::custom_hides_schema`]。
    fn read_schema(
        schema_id: &str,
        data_dir: Option<&Path>,
        override_dir: Option<&Path>,
    ) -> Option<Schema> {
        Self::read_schema_in(schema_id, data_dir, override_dir, None)
    }

    /// 同 [`Self::read_schema`]，另把用户分库（`<user_text_dir>/<数据方案>/dicts/`）登记进词库表。
    /// 只有用到词库的路径（建引擎、getConfig / setDictEnabled、反查索引、单字码表）需要它。
    fn read_schema_in(
        schema_id: &str,
        data_dir: Option<&Path>,
        override_dir: Option<&Path>,
        user_text_dir: Option<&Path>,
    ) -> Option<Schema> {
        SCHEMA_READS.with(|n| n.set(n.get() + 1));
        if Config::custom_hides_schema(schema_id) {
            Self::warn_hidden_schema_once(schema_id);
            return None;
        }
        let data_dir = data_dir?;
        let toml_path = Self::resolve_schema_file(&format!("{}.schema.toml", schema_id), data_dir);
        if !toml_path.exists() {
            warn!("Schema file not found: {}.schema.toml", schema_id);
            return None;
        }
        let content = std::fs::read_to_string(&toml_path).ok()?;
        let mut base: toml::Value = match content.parse() {
            Ok(v) => v,
            Err(e) => {
                warn!("Parse schema TOML failed {}: {}", toml_path.display(), e);
                return None;
            }
        };
        // 发现目录里的词库登记为扩展库。必须在合并 override 之前：override 的词库项只按 id
        // 匹配方案里已有的库（`merge_dict_overrides`），晚注入的库用户翻的开关永远对不上号。
        Self::inject_dir_dicts(
            &mut base,
            &|sub| Config::list_schema_resource_dir(Some(data_dir), sub, DICT_YAML_SUFFIX),
            &|rel| {
                Self::dict_head_resolved_full(&Self::resolve_dict_file(
                    rel,
                    &data_dir.join("schemas"),
                ))
            },
        );
        // 合并 override 层（存在才读；不存在则零影响）。
        if let Some(ov) = override_dir.and_then(|d| Self::read_override_value(schema_id, d)) {
            merge_toml(&mut base, ov);
        }
        // 用户分库在 override 之后登记：开关只认 dicts.toml，override 里同 id 的项不起作用。
        if let Some(dir) = user_text_dir {
            Self::inject_user_dicts(&mut base, schema_id, dir);
        }
        // 字段级容错（`tolerant_de`）的回落是静默的，不触发段级降级。清空→反序列化→取走，
        // 把这些回落也收进 `degraded_items` 告知用户。
        wind_config::tolerant_de::clear_fallbacks();
        let misplaced = misplaced_dict_font_keys(&base);
        let mut schema = match Schema::from_toml_value(base.clone()) {
            Ok(mut s) => {
                let fallbacks = wind_config::tolerant_de::take_fallbacks();
                if !fallbacks.is_empty() {
                    warn!(
                        "方案 {schema_id} 有 {} 项设置的值无法识别，已按默认处理：{}",
                        fallbacks.len(),
                        fallbacks.join("、")
                    );
                    s.degraded_items = fallbacks
                        .into_iter()
                        .map(|v| format!("值 \"{v}\""))
                        .collect();
                }
                Some(s)
            }
            // 不能直接 `None`：一个坏字段就让整个方案读不出来，几十个调用点一起归零且不报错。
            // 走段级降级：有毒的段回落出厂默认值，其余照常，降级清单挂在
            // `Schema::degraded_items` 上交给协调器 toast。
            Err(root_err) => Self::salvage_schema(schema_id, base, &root_err),
        }?;
        if !misplaced.is_empty() {
            Self::warn_misplaced_dict_font_once(schema_id, &misplaced);
            schema.degraded_items.extend(misplaced);
        }
        Some(schema)
    }

    /// `[[dictionaries]]` 下写了字体键时告警一次（`read_schema` 一次启动会被调用多次）。
    fn warn_misplaced_dict_font_once(schema_id: &str, items: &[String]) {
        static WARNED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
            std::sync::OnceLock::new();
        let first = WARNED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(schema_id.to_string());
        if first {
            warn!(
                "方案 {schema_id} 的 {} 不是受支持的键，已忽略——词库不带字体。\
                 方案的候选文字字体请在设置「方案管理 → 方案设置」里指定（落盘为 \
                 [candidate] font_family，引用已安装的字体）",
                items.join("、")
            );
        }
    }

    /// `read_schema` 的段级降级分支：把有毒的段回落出厂默认值，救回方案的其余部分。
    ///
    /// 救不回来（探不出毒、或回落后仍失败）才返回 `None`——那时方案确实无法使用，
    /// 与降级前的行为一致。
    fn salvage_schema(
        schema_id: &str,
        merged: toml::Value,
        root_err: &toml::de::Error,
    ) -> Option<Schema> {
        let default_v = match Schema::default().to_toml_value() {
            Ok(v) => v,
            // 默认方案自己序列化不了属于代码 bug，此处无从补救。
            Err(e) => {
                warn!("Schema default 不可序列化（{e}）；方案 {schema_id} 无法降级救回");
                return None;
            }
        };
        let wind_config::section_fallback::Patched { value, bad } =
            Schema::probe_and_patch(&merged, &default_v);
        if bad.is_empty() {
            warn!("Schema {schema_id} override 合并后解析失败且无法定位到具体段: {root_err}");
            return None;
        }
        match Schema::from_toml_value(value) {
            Ok(mut s) => {
                for (path, err) in &bad {
                    // WARN 而非 INFO：这是异常不是正常降级，压成 INFO 会掩盖真实的
                    // 值域收缩（改了枚举取值却没写迁移，会在这里悄悄「自愈」成出厂值）。
                    warn!(
                        "方案 {schema_id} 的 [{path}] 解析失败，已回落出厂默认值\
                         （该项在本方案里本次不生效）；原始错误：{err}"
                    );
                }
                s.degraded_items = bad
                    .into_iter()
                    .map(|(path, _)| format!("[{path}]"))
                    .collect();
                Some(s)
            }
            Err(e) => {
                warn!(
                    "Schema {schema_id} 段级降级后仍解析失败，方案不可用；\
                     原始错误：{root_err}；回落后仍失败：{e}"
                );
                None
            }
        }
    }

    /// 读取某方案的 override TOML 值（无则 None）。
    fn read_override_value(schema_id: &str, override_dir: &Path) -> Option<toml::Value> {
        let path = override_dir.join(format!("{schema_id}.toml"));
        let content = std::fs::read_to_string(&path).ok()?;
        content.parse().ok()
    }

    /// 混输引用图（primary / secondary 两条边）上从 `start` 出发可达的第一个环，
    /// 返回环路径（首尾同名，如 `["a", "b", "a"]`）；无环返回 `None`。
    ///
    /// `edges(id)` 给出 `id` 引用的子方案；非混输 / 读不到的方案给空。自引用、间接环、
    /// 以及「`start` 本身不在环上、但它引用的方案成环」都算——后者照样会让递归构建无底。
    fn find_mixed_cycle(start: &str, edges: &dyn Fn(&str) -> Vec<String>) -> Option<Vec<String>> {
        fn dfs(
            id: &str,
            edges: &dyn Fn(&str) -> Vec<String>,
            path: &mut Vec<String>,
            done: &mut std::collections::HashSet<String>,
        ) -> Option<Vec<String>> {
            if let Some(pos) = path.iter().position(|p| p == id) {
                let mut cycle = path[pos..].to_vec();
                cycle.push(id.to_string());
                return Some(cycle);
            }
            if done.contains(id) {
                return None;
            }
            path.push(id.to_string());
            for child in edges(id) {
                if let Some(c) = dfs(&child, edges, path, done) {
                    return Some(c);
                }
            }
            path.pop();
            done.insert(id.to_string());
            None
        }
        dfs(
            start,
            edges,
            &mut Vec::new(),
            &mut std::collections::HashSet::new(),
        )
    }

    /// 读方案（含 override）得到它在混输引用图上的出边；供 [`Self::find_mixed_cycle`] 用。
    fn mixed_edges_of(id: &str, data_dir: &Path, override_dir: Option<&Path>) -> Vec<String> {
        match Self::read_schema(id, Some(data_dir), override_dir) {
            Some(s) if s.engine.engine_type.eq_ignore_ascii_case("mixed") => {
                let m = &s.engine.mixed;
                [&m.primary_schema, &m.secondary_schema]
                    .into_iter()
                    .filter(|x| !x.is_empty())
                    .cloned()
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// 为指定 schema 构建引擎
    ///
    /// `mixed_role`：见 [`MixedRole`]。`None` = 独立方案（非混输成员），走各引擎默认。
    #[allow(clippy::too_many_arguments)]
    fn build_engine(
        schema_id: &str,
        data_dir: Option<&Path>,
        store: Option<Arc<wind_store::Store>>,
        codetable_cfg: &wind_config::CodetableGlobal,
        mix_cfg: &wind_config::MixGlobal,
        override_dir: Option<&Path>,
        pinyin_cfg: &wind_config::config::PinyinGlobalConfig,
        // `pinyin_hint`：管理器持有的提示开关，原样注入拼音引擎（见 `EngineManager::pinyin_hint`）。
        pinyin_hint: &crate::pinyin::fuzzy::HintSwitch,
        // `correction`：换位纠错规则（管理器生成一份共享）；双拼方案不用。
        correction: &Arc<crate::pinyin::correction::CorrectionRules>,
        // `misread`：误读表，`None` = 不召回不标（见 `EngineManager::misread_for_engines`）。
        misread: Option<&Arc<crate::pinyin::misread::MisreadTable>>,
        // `phrase_seg_anywhere`：英文方案 / 临英 / 快捷输入英文里是否有任何一个开着词组分词。
        // 英文引擎实例是三者共用的，分不开作用域；真正的闸门在协调器（`'` 进不了缓冲，分词
        // 路径不触发）。本参数只决定引擎要不要具备这个能力、要不要预热索引。
        phrase_seg_anywhere: bool,
        // `lookup_disabled_dicts`：全局 `input.reverse.lookup_disabled_dicts`（经
        // `EngineManager::reverse_lookup_disabled` 镜像传入）。开 ⇒ 码表引擎挂影子层（未启用扩展库，
        // 只给反查模式读）；混输递归构建子引擎时原样透传。
        lookup_disabled_dicts: bool,
        mixed_role: Option<MixedRole>,
        // `english_provider`：混输分支取英文子引擎的通道，见 [`EnglishProvider`]。
        // 非混输分支一概不碰它。
        english_provider: Option<EnglishProvider<'_>>,
    ) -> Option<Box<dyn Engine>> {
        let data_dir = data_dir?;
        let schemas = data_dir.join("schemas");
        let schema = Self::read_schema_in(
            schema_id,
            Some(data_dir),
            override_dir,
            store.as_ref().map(|s| s.user_text_dir()),
        )?;

        // 混输方案：递归构建主（码表）+ 次（拼音）子引擎，包装为 MixedEngine
        if schema.engine.engine_type.to_lowercase() == "mixed" {
            let m = &schema.engine.mixed;
            if m.primary_schema.is_empty() {
                warn!("mixed schema {} 缺少 primary_schema", schema_id);
                return None;
            }
            // 引用成环（自引用 / 互指 / 更长的环）时下面的递归构建无底，Rust 栈溢出直接
            // abort 整个进程。畸形 override 就能造出来（`merge_toml` 无字段白名单）。只在顶层
            // （`mixed_role == None`）查一次：它覆盖整张可达子图，子层再查是重复读盘。
            if mixed_role.is_none() {
                let edges = |id: &str| Self::mixed_edges_of(id, data_dir, override_dir);
                if let Some(cycle) = Self::find_mixed_cycle(schema_id, &edges) {
                    warn!(
                        "混输方案 {} 的 primary_schema/secondary_schema 引用成环：{}，\
                         该方案不可用。请检查方案文件或 schema_overrides 里的 [engine.mixed]。",
                        schema_id,
                        cycle.join(" → ")
                    );
                    return None;
                }
            }
            let primary = Self::build_engine(
                &m.primary_schema,
                Some(data_dir),
                store.clone(),
                codetable_cfg,
                mix_cfg,
                override_dir,
                pinyin_cfg,
                pinyin_hint,
                correction,
                misread,
                phrase_seg_anywhere,
                lookup_disabled_dicts,
                Some(MixedRole::Primary {
                    // 取混输方案自己声明的值，不继承 primary_schema 的（见 MixedRole::Primary）；
                    // 且只在整句真能触发（没配拼音子引擎）时才传真——整句开着会让满码上屏 /
                    // 满码空码清空 / 顶码让位，一个不触发的整句不该白白关掉它们。
                    sentence_input: schema.engine.codetable.sentence_input
                        && m.secondary_schema.is_empty(),
                }),
                english_provider,
            )?;
            // 「声明了整句、却配着拼音子引擎」不会生效（超码长区间归拼音），告警一句。
            if schema.engine.codetable.sentence_input && !m.secondary_schema.is_empty() {
                warn!(
                    "混输方案 {} 声明了 [engine.codetable] sentence_input，但它配有拼音子引擎 \
                     {}：超码长输入由拼音接管，码表整句不会触发，已忽略（顶码 / 满码上屏 / \
                     满码空码清空照常生效）。混输下的整句尚未接线。",
                    schema_id, m.secondary_schema
                );
            }
            // 逆切分在混输下整体关闭（见 `resolve_split_input`），写了却不生效要告警。
            if schema.engine.codetable.split_input {
                warn!(
                    "混输方案 {} 声明了 [engine.codetable] split_input，但逆切分在混输下尚未接线\
                     （切分候选会落进档位体系的档 0、压过拼音精确候选），已忽略。\
                     要用它请在纯码表方案上开。",
                    schema_id
                );
            }
            // secondary（拼音）是唯一注入 [`MixPinyinOpts`] 的地方：这些收敛只约束
            // 「作为混输辅助的拼音」，纯拼音方案走 build_engine 时仍传 None（行为不变）。
            let secondary = if m.secondary_schema.is_empty() {
                None
            } else {
                Self::build_engine(
                    &m.secondary_schema,
                    Some(data_dir),
                    store.clone(),
                    codetable_cfg,
                    mix_cfg,
                    override_dir,
                    pinyin_cfg,
                    pinyin_hint,
                    correction,
                    misread,
                    phrase_seg_anywhere,
                    lookup_disabled_dicts,
                    Some(MixedRole::Secondary(MixPinyinOpts {
                        abbrev: mix_cfg.enable_pinyin_abbrev,
                    })),
                    english_provider,
                )
            };
            // 融合策略走全局 schema.mix（无方案级 override）。
            let min_py = if mix_cfg.min_pinyin_length > 0 {
                mix_cfg.min_pinyin_length
            } else {
                2
            };
            let block_on_pinyin = mix_cfg.auto_commit_block_on_pinyin;
            // 英文候选（schema.mix.enable_english）：开启时经 `english_provider` 取共享的英文
            // 引擎（词库缺失则 None，退化为无英文），别在这里另建一份。开关热切换经
            // reload_from_config 的 engines.clear() 重建混输引擎生效。
            let english = if mix_cfg.enable_english {
                english_provider.and_then(|f| f())
            } else {
                None
            };
            // 英文最小触发长度（0=回退 3，即 2 字符以内不查英文）。
            let min_en = if mix_cfg.min_english_length > 0 {
                mix_cfg.min_english_length
            } else {
                3
            };
            info!(
                "Built mixed engine {} (primary={}, secondary={}, english={})",
                schema_id,
                m.primary_schema,
                m.secondary_schema,
                english.is_some()
            );
            let cfg = crate::mixed::MixConfig {
                min_pinyin_length: min_py,
                auto_commit_block_on_pinyin: block_on_pinyin,
                pinyin_only_overflow: mix_cfg.pinyin_only_overflow,
                top_code_override_pinyin: mix_cfg.top_code_override_pinyin,
                show_source_hint: mix_cfg.show_source_hint,
                min_english_length: min_en,
                auto_commit_block_on_english: mix_cfg.auto_commit_block_on_english,
                block_commit_on_english_word: mix_cfg.block_commit_on_english_word,
                english_complete_min_length: mix_cfg.english_complete_min_length,
                english_max_count: mix_cfg.english_max_count,
                block_commit_on_pinyin_word: mix_cfg.block_commit_on_pinyin_word,
                pinyin_word_min_weight: mix_cfg.pinyin_word_min_weight,
                pinyin_partial_candidates: mix_cfg.pinyin_partial_candidates,
                pinyin_partial_candidates_overflow: mix_cfg.pinyin_partial_candidates_overflow,
            };
            return Some(Box::new(crate::mixed::MixedEngine::new(
                primary, secondary, english, cfg,
            )));
        }

        // 英文方案：复用码表词典加载 + 前缀查询，但包成独立 EnglishEngine（EngineType::English）。
        // 关闭自动上屏 / 顶码 / 编码提示（英文词变长，无满码顶字语义）；词库以 type="english"
        // 声明（code 列小写化，大小写不敏感前缀匹配）。供临时英文 / 融合英文候选懒加载。
        if schema.engine.engine_type.eq_ignore_ascii_case("english") {
            let layers = Self::load_codetable_layers(&schema, &schemas);
            if layers.is_empty() {
                warn!("No usable english dictionary for schema {}", schema_id);
                return None;
            }
            let dm = wind_dict::DictManager::new();
            // 用户词层归属 `schema_id`（即 "english"），须与 `data_schema_id` 对英文的返回值
            // 一致，否则加进去的词查不出来。临时词层是英文自动造词
            // （`[schema.english.auto_learn]`）的暂存区；这是共享引擎，临英与混输里的英文成员
            // 同样看得到临时词（`english_merge::has_any` 的上屏否决刻意不算临时词）。
            // 注册顺序 User → Temp → System 与码表分支一致。
            if let Some(store) = &store {
                // 用户词托底到名次权重之上：系统词按词频名次赋了权，权重为 0（导入的词表
                // 没有权重列）或手填小值的用户词不托底就会沉到常用系统词之后。
                dm.register_layer(Box::new(
                    wind_store::StoreUserLayer::new(store.clone(), schema_id)
                        .with_min_weight(wind_dict::freq_rank::RANK_WEIGHT_MAX + 1),
                ));
                dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
                    store.clone(),
                    schema_id,
                )));
            }
            // 方案级归一化：构造一次、施加到全部词库层——同一个映射函数才保序，
            // 库间相对关系因此原样保留（见 `weight_norm_of`）。
            let wnorm = weight_norm_of(&schema);
            for l in layers {
                dm.register_layer(Box::new(
                    wind_dict::SystemDictLayer::with_enabled(l.dict, l.name, l.enabled)
                        .with_base_order(l.base_order)
                        .with_default_weight(l.default_weight)
                        .with_weight_norm(wnorm),
                ));
            }
            // 英文最大码长取词库最长词的安全上界（前缀匹配用，不触发顶码/自动上屏）。
            let mcl = if schema.engine.codetable.max_code_length > 0 {
                schema.engine.codetable.max_code_length
            } else {
                32
            };
            // 全 false：无自动上屏 / 顶码 / 编码提示，纯前缀查词。
            let commit_opts = crate::codetable::CommitOptions::default();
            info!("Built english engine {}", schema_id);
            // 词组分词：三个作用域任一开着就给引擎装上这个能力，分词符固定 `'`
            // （不可配的理由见 `EnglishGlobal::phrase_seg`）。全关时传 None，
            // `convert` 逐字节退回原路径、索引一次都不建。
            let seg = phrase_seg_anywhere.then_some(crate::english_phrase::PHRASE_SEPARATOR);
            return Some(Box::new(
                crate::english::EnglishEngine::new(
                    CodeTableEngine::new(mcl, commit_opts, Arc::new(dm))
                        .with_own_extra_dicts(Self::declared_extra_dict_ids(&schema)),
                )
                // 落盘目录须先于 `with_phrase_seg` 装上：后者会立刻起预热线程。
                .with_phrase_cache_dir(Self::english_phrase_cache_dir(schema_id))
                .with_phrase_seg(seg),
            ));
        }

        // 拼音分支只关心「是不是混输辅助拼音」这一面，先解出来，下面三处判据照旧。
        let mix_secondary = match mixed_role {
            Some(MixedRole::Secondary(o)) => Some(o),
            _ => None,
        };

        if schema.is_pinyin() {
            let dict = match Self::load_dictionary(&schema, &schemas) {
                Some(d) => d,
                None => {
                    warn!("load_dictionary returned None for schema {}", schema_id);
                    return None;
                }
            };
            // 从全局拼音配置构建引擎配置和模糊音。enabled 作总开关：未启用时所有模糊标志归零。
            let pg = pinyin_cfg;
            // 双拼不启用换位纠错（规则是全拼字母层的），纠错置空。
            let shuangpin = schema
                .engine
                .pinyin
                .scheme
                .eq_ignore_ascii_case("shuangpin");
            let empty_correction = Arc::default();
            let fuzzy = fuzzy_config_from_global(
                &pg.fuzzy,
                pinyin_hint,
                if shuangpin {
                    &empty_correction
                } else {
                    correction
                },
                misread,
            );
            let pcfg = PinyinConfig {
                use_smart_compose: pg.use_smart_compose,
                // 纯拼音方案看 `[schema.pinyin.abbrev] enabled`；混输看 `mix.enable_pinyin_abbrev`，
                // 两者取与——混输用户关掉全局简拼，混输里的拼音也不该还出简拼。
                enable_abbrev: pg.abbrev.enabled && mix_secondary.map(|o| o.abbrev).unwrap_or(true),
                // 残码整句只在非混输下启用，理由见 `PinyinConfig::enable_partial_final`。
                // 判据是「是不是混输辅助」本身，不是 `abbrev` 的取值（两者语义正交）。
                enable_partial_final: mix_secondary.is_none(),
                // 用户词进整句词图。全局项，混输的拼音子引擎同样适用。
                sentence_uses_user_words: pg.sentence_uses_user_words,
                // 整句 N-best（露几条 / 算几条），规整在引擎侧 `Config::sentence_counts`。
                // 混输的拼音子引擎钉成 1/1：count>1 时协调器把整句块摆到最前
                // （`place_sentence_block`），混输下会让拼音整句越过码表精确候选。
                sentence_count: if mix_secondary.is_some() {
                    1
                } else {
                    pg.sentence_count
                },
                sentence_max_count: if mix_secondary.is_some() {
                    1
                } else {
                    pg.sentence_max_count
                },
                abbrev_fallback: Default::default(),
                // 补全这两项刻意不按混输分流：「引擎敢预测多少没打的音节」是用户对候选面的
                // 统一取舍。见 `mixed_completion_config`。
                completion_min_syllables: pg.completion.min_syllables,
                completion_max_extra_syllables: pg.completion.max_extra_syllables,
                // 全拼降级输入（双拼下多人共用）。混输强制关闭：击键串同时是码表码，且混输
                // 本就不接双拼（`MixedEngine::pinyin_may_continue`）。非双拼方案下本项不生效
                // （见 `PinyinConfig::allow_full_pinyin`）。
                allow_full_pinyin: pg.shuangpin.allow_full_pinyin && mix_secondary.is_none(),
            };
            let mut engine = PinyinEngine::new(pcfg, dict).with_fuzzy(fuzzy.clone());
            // 上下文语言模型：开关开着，且 weight 与 model 都显式配置才启用（见
            // `PinyinGrammar::active`），否则根本不读文件。模型名默认空串，免得只改 weight 就
            // 静默启用一个评测上是负收益的模型（docs/design/language-model-integration.md §8）。
            // 模型数据不随安装包分发，缺失时降级为关闭而非报错（同文档 §5）。
            if pinyin_cfg.grammar.active() {
                let gram_path = schemas
                    .join("pinyin")
                    .join("grammar")
                    .join(&pinyin_cfg.grammar.model);
                let gcfg = crate::pinyin::octagram::OctagramConfig {
                    weight: pinyin_cfg.grammar.weight,
                    ..Default::default()
                };
                match crate::pinyin::octagram::OctagramGrammar::open(&gram_path, gcfg) {
                    Ok(g) => {
                        info!(
                            "Loaded grammar model {} ({} units, weight={})",
                            gram_path.display(),
                            g.unit_count(),
                            pinyin_cfg.grammar.weight
                        );
                        engine = engine.with_grammar(Arc::new(g));
                    }
                    Err(e) => warn!(
                        "Grammar model unavailable ({}), context scoring disabled: {e:#}",
                        gram_path.display()
                    ),
                }
            }
            // 双拼方案：按 layout 加载布局并注入 ShuangpinConverter
            if shuangpin {
                let layout_id = if schema.engine.pinyin.shuangpin.layout.is_empty() {
                    // 与 `shuangpin_layout_id_of` 同源，否则引擎与设置页显示的布局会分叉。
                    DEFAULT_SHUANGPIN_LAYOUT.to_string()
                } else {
                    schema.engine.pinyin.shuangpin.layout.clone()
                };
                // 用户目录优先（见 resolve_schema_file）：用户自带/覆盖布局生效。
                let lp =
                    Self::resolve_schema_file(&format!("shuangpin/{layout_id}.toml"), data_dir);
                match crate::pinyin::shuangpin::Layout::from_toml(&lp) {
                    Ok(layout) => {
                        let mut conv = crate::pinyin::shuangpin::ShuangpinConverter::new(layout);
                        conv.set_fuzzy(&fuzzy);
                        engine = engine.with_shuangpin(conv);
                    }
                    Err(e) => {
                        warn!("双拼布局 {} 加载失败，回退全拼: {}", layout_id, e);
                    }
                }
            }
            // 注入 redb Store 时挂用户词/临时词层（L 造词显现）：让拼音造的词进候选合并。
            // 仅含 User/Temp 层（系统词典仍由引擎自身的 CachedDict 承担 Viterbi/前缀）。
            // 存储归属统一为 "pinyin"，使全拼/双拼方案共享同一份用户词与临时词。
            if let Some(store) = &store {
                let dm = wind_dict::DictManager::new();
                dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
                    store.clone(),
                    PINYIN_DATA_SCHEMA,
                )));
                dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
                    store.clone(),
                    PINYIN_DATA_SCHEMA,
                )));
                engine = engine.with_store_layers(Arc::new(dm));
            }
            Some(Box::new(engine))
        } else {
            let mcl = if schema.engine.codetable.max_code_length > 0 {
                schema.engine.codetable.max_code_length
            } else {
                4
            };
            // 上屏策略：基线 + 该方案 [engine.codetable] 行为折叠。
            // schema 已在 read_schema 合并了 schema_overrides，此处直接取其 engine.codetable。
            // 基线按方案性质选（特殊方案用内置默认、不继承全局），见 codetable_baseline。
            // 与 resolve_codetable 是两个平行的折叠点，基线判据必须一致——
            // 一处改了另一处没改，会得到「引擎按 A 行为构建、协调器按 B 行为决策」。
            let eff = Self::codetable_baseline(Some(&schema), codetable_cfg)
                .resolved(Some(&schema.engine.codetable));
            let commit_opts = crate::codetable::CommitOptions {
                auto_commit_at_full: eff.auto_commit_at_full,
                auto_commit_min_len: eff.auto_commit_min_len,
                clear_on_empty_max: eff.clear_on_empty_max,
                top_code_commit: eff.top_code_commit,
                show_code_hint: eff.show_code_hint,
                single_code_input: eff.single_code_input,
                single_code_complete: eff.single_code_complete,
                // 基础排序：[engine.codetable].base_sort（"natural" → 纯出现序、忽略权重；默认 weight）。
                base_sort: crate::codetable::BaseSort::parse(&schema.engine.codetable.base_sort),
                // 整句：方案级引擎固定参数（同 max_code_length / base_sort），不走
                // `eff` 的行为折叠——它是「这张码表能不能整句」，不是用户偏好。
                //
                // 作为混输主引擎构建时改取混输方案自己的声明，理由见 `MixedRole::Primary`。
                sentence_input: resolve_sentence_input(
                    mixed_role,
                    schema.engine.codetable.sentence_input,
                ),
                // 整句 N-best 是用户偏好（走 `eff` 折叠）。混输下整句不触发（见
                // `MixedRole::Primary`），读到什么都不会被用到。
                sentence_count: eff.sentence_count,
                sentence_max_count: eff.sentence_max_count,
                // 逆切分：同为方案级引擎固定参数。切点有效性与「与整句同开」的告警在
                // `CodeTableEngine::new` 里（那里才同时握着 max_code_length 与两个开关）。
                //
                // 后三个旋钮取的是本次构建的 schema（混输下即 `primary_schema` 的值）；混输下
                // `split_input` 恒 false 故无影响。将来给混输接上逆切分，这三行必须一并经
                // `MixedRole` 收敛。
                split_input: resolve_split_input(mixed_role, schema.engine.codetable.split_input),
                split_front_candidates: schema.engine.codetable.split_front_candidates,
                split_trigger: crate::codetable::SplitTrigger::parse(
                    &schema.engine.codetable.split_trigger,
                ),
                split_alt_display: crate::codetable::SplitAltDisplay::parse(
                    &schema.engine.codetable.split_alt_display,
                ),
                // 通配：与上屏行为同源于 `eff`（全局基线 + 方案折叠；overlay 方案取内置基线，
                // 默认关）。非法键在此告警一次并视为关闭，协调器经 `active_wildcard_key` 取用。
                wildcard: eff.wildcard_char(schema_id),
                wildcard_single_only: eff.wildcard_single_only,
                // 反查模式通配键：不看主开关，非法回落 z（spec §3.2）。
                reverse_key: Some(eff.reverse_wildcard_char()),
            };
            // 码表引擎经 DictManager(CompositeDict) 查询：主库 + 每个启用的扩展各自一个 System 层，
            // 查询期由 composite 合并去重。禁用扩展可即时摘层，启用要重建（见 `set_dict_enabled_live`）。
            let layers = Self::load_codetable_layers(&schema, &schemas);
            if layers.is_empty() {
                warn!("No usable codetable dictionary for schema {}", schema_id);
                return None;
            }
            // 注入 redb Store 时，注册用户词/临时词层（按 schema 隔离），让用户词进候选合并。
            let dm = wind_dict::DictManager::new();
            if let Some(store) = &store {
                dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
                    store.clone(),
                    schema_id,
                )));
                dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
                    store.clone(),
                    schema_id,
                )));
                // 自动造词的草稿层（`docs/design/auto-phrase-draft-layer.md`）。只在开关打开时挂，
                // 关着的用户不为它在每次精确查询上多付一次 redb 读。TTL 在注册时固定，改配置
                // 靠重建引擎生效（方案切换与配置重载本就会重建）。
                if codetable_cfg.auto_phrase.enabled {
                    dm.register_layer(Box::new(wind_store::StoreDraftLayer::new(
                        store.clone(),
                        schema_id,
                        i64::from(codetable_cfg.auto_phrase.draft_ttl_hours) * 3600,
                    )));
                }
            }
            // 主库优先注册（在 load_codetable_layers 中已置首），扩展库其后。
            // base_order 决定等权/natural 排序的库间档位；default_weight 覆盖无权重库的权重档。
            // 方案级归一化：构造一次、施加到全部词库层——同一个映射函数才保序，
            // 库间相对关系因此原样保留（见 `weight_norm_of`）。
            let wnorm = weight_norm_of(&schema);
            // 影子层的启用集取「本次实际挂上的扩展层」，须在下面消费 `layers` 之前收集。
            let loaded_extra_ids: Vec<String> = layers
                .iter()
                .filter_map(|l| l.name.strip_prefix("codetable-extra-"))
                .map(str::to_string)
                .collect();
            for l in layers {
                dm.register_layer(Box::new(
                    wind_dict::SystemDictLayer::with_enabled(l.dict, l.name, l.enabled)
                        .with_base_order(l.base_order)
                        .with_default_weight(l.default_weight)
                        .with_weight_norm(wnorm),
                ));
            }
            // 码元字符集与上屏行为同源于 `eff`（全局基线 + 方案 [engine.codetable] 折叠），
            // 故方案里写 input_chars / leading_chars 与写 top_code_commit 走的是同一条路。
            let charset =
                wind_config::CodeCharSet::new(&eff.input_chars, &eff.leading_chars, schema_id);
            if !charset.is_default_alpha() {
                info!(
                    "schema {} 码元集：{:?}（首码 {:?}）",
                    schema_id,
                    charset.chars().into_iter().collect::<String>(),
                    charset.leading_chars().into_iter().collect::<String>()
                );
            }
            let mut engine = CodeTableEngine::new(mcl, commit_opts, Arc::new(dm))
                .with_charset(charset)
                .with_own_extra_dicts(Self::declared_extra_dict_ids(&schema));
            // 影子层（未启用扩展库，只给反查模式用，spec §4.2）：开关关或无扩展库时不挂。
            // 开关取全局 `input.reverse.lookup_disabled_dicts`（反查模式专属，不再是方案级）；
            // 混输的主码表子引擎同样由它决定；英文分支不接。
            if lookup_disabled_dicts
                && let Some(d) = Self::disabled_dict_layers(&schema, &schemas, loaded_extra_ids)
            {
                engine = engine.with_disabled_dicts(d);
            }
            // 逆切分的两段由引擎自己查，协调器的候选调整够不着，须另行注入。
            // 归属 id 取方案自身：码表的 `data_schema_id` 就是自身 id（只有拼音族折叠），
            // 混输下 `split_input` 恒为 false（见 `resolve_split_input`），不会注入。
            if commit_opts.split_input
                && let Some(store) = &store
            {
                engine = engine.with_segment_shadow(store.clone(), schema_id);
            }
            Some(Box::new(Self::attach_sentence_freq(
                engine,
                commit_opts.sentence_input,
                eff.sentence_grammar_weight,
                &schemas,
            )))
        }
    }

    /// 词库文件路径解析：按层序 `user > custom > data` 逐层，回退 schemas_dir（data 层，
    /// 与 read_schema 同语义）。词库路径解析的唯一入口，别在调用方另写一份。
    ///
    /// 同时支持 wdat-only 词库（用户只投放编译好的 `xxx.wdat`）：yaml 在各层都不存在时按同名
    /// wdat 同序再探，命中则返回该目录下的 yaml 路径，`CachedDict::load_at_with` 会据此推导同目录
    /// 的 wdat。不能只靠兜底：兜底恒指向安装目录，而用户投放的 wdat 一般在用户目录。
    fn resolve_dict_file(rel: &str, schemas_dir: &Path) -> std::path::PathBuf {
        Self::resolve_dict_file_in(rel, &Self::dict_layers(schemas_dir))
    }

    /// 词库解析的层序，元素是各层的 schemas 级目录，末位必须是 data 层。
    ///
    /// data 层直接取调用方传进来的 `schemas_dir`（便携版/测试会传自定义路径），不从它
    /// `parent()` 反推数据根；user/custom 两层与 data 目录参数无关，故取 `None` 那一支。
    fn dict_layers(schemas_dir: &Path) -> Vec<wind_config::ResourceLayer> {
        let mut layers: Vec<wind_config::ResourceLayer> = Config::resource_layers_named_with(None)
            .into_iter()
            .map(|l| l.sub("schemas"))
            .collect();
        layers.push(wind_config::ResourceLayer::new(
            "data",
            schemas_dir.to_path_buf(),
        ));
        layers
    }

    /// [`Self::resolve_dict_file`] 的纯函数内核：层序显式传入（元素为 schemas 级目录、
    /// 靠前者优先、末位是 data 层兼兜底），便于测试多层组合。
    ///
    /// 两趟，不是一趟：第 1 趟各层找 `.yaml`，全都没有才第 2 趟找有同名 `.wdat` 的。yaml 整体
    /// 优先于 wdat——用户层只投了 `.wdat`、data 层有 `.yaml` 时 data 层的 yaml 胜出。
    fn resolve_dict_file_in(
        rel: &str,
        layers: &[wind_config::ResourceLayer],
    ) -> std::path::PathBuf {
        // fn item 而非闭包：闭包会借住外部变量，与几处 return 的 move 冲突。
        fn has_wdat(p: &Path) -> bool {
            wind_dict::cached::wdat_sibling(p).is_some_and(|w| w.is_file())
        }
        // 绝对路径（用户分库）不走分层
        if Path::new(rel).is_absolute() {
            return std::path::PathBuf::from(rel);
        }
        // 兜底同原行为：返回 data 层（末位）路径，由调用方报加载失败。层序恒非空
        // （`dict_layers` 必压入 data 层），空表只可能来自测试，退化为相对路径。
        let fallback = layers
            .last()
            .map(|l| l.path.join(rel))
            .unwrap_or_else(|| std::path::PathBuf::from(rel));
        // 1) 第 1 趟：yaml 逐层
        for (i, layer) in layers.iter().enumerate() {
            let p = layer.path.join(rel);
            if !p.is_file() {
                continue;
            }
            // 遮蔽判定含 wdat：更靠后的层只投放了 wdat（无 yaml）时，本层的 yaml 同样是覆盖。
            let shadowed = layers[i + 1..].iter().any(|b| {
                let q = b.path.join(rel);
                q.is_file() || has_wdat(&q)
            });
            Config::log_layer_override(layer.name, "dict", rel, &p, shadowed);
            return p;
        }
        // 2) 各层都无 yaml → 按 wdat-only 同序再探
        for (i, layer) in layers.iter().enumerate() {
            let p = layer.path.join(rel);
            if !has_wdat(&p) {
                continue;
            }
            // 此时任何层都没有 yaml，遮蔽只可能来自更靠后层的 wdat。
            let shadowed = layers[i + 1..].iter().any(|b| has_wdat(&b.path.join(rel)));
            Config::log_layer_override(layer.name, "dict", rel, &p, shadowed);
            return p;
        }
        // 3) 都没有
        fallback
    }

    /// 方案声明过的扩展词库 id（含未启用的；主库不在内，它没有 `codetable-extra-` 层）。
    ///
    /// 供 `CodeTableEngine::set_dict_enabled` 分辨「这个 id 是不是我的」。必须包含未启用的
    /// —— 惰性加载后它们没有层，但禁用它们同样属于「本方案的事、且目标已达成」。
    fn declared_extra_dict_ids(schema: &Schema) -> Vec<String> {
        schema
            .dictionaries
            .iter()
            .filter(|d| !d.default && !d.path.is_empty())
            .map(|d| d.id.clone())
            .collect()
    }

    /// 码表方案的选库口径：可用 = path 非空；主库 = 首个 `default`，无则首个可用库；其余可用库
    /// 全是扩展库（不看 `is_enabled`）。无可用库 ⇒ `None`。
    ///
    /// [`Self::load_codetable_layers`] 与 [`Self::disabled_extra_sources`] 共用这一处：两边一旦
    /// 漂移，就会出现「某库既不在主层也不在影子层」或「主库进了影子层」。
    fn split_codetable_dicts(schema: &Schema) -> Option<(&DictSpec, Vec<&DictSpec>)> {
        let mut usable: Vec<&DictSpec> = schema
            .dictionaries
            .iter()
            .filter(|d| !d.path.is_empty())
            .collect();
        if usable.is_empty() {
            return None;
        }
        let main_idx = usable.iter().position(|d| d.default).unwrap_or(0);
        let main = usable.remove(main_idx);
        Some((main, usable))
    }

    /// 影子层（reverse-mode spec §4.2）的来源：本方案全部扩展库（主库除外，含已启用的——
    /// 它们随时可能被热禁用而进影子集合）。哪些真正进影子集合由
    /// `DisabledDictLayers` 按「声明 − 本次实际挂进主 dm 的」在查询时算，见
    /// [`Self::disabled_dict_layers`]。用户词库 / 临时词库不在 `[[dictionaries]]` 里，天然不在内。
    ///
    /// `load` 闭包与 `load_codetable_layers` 同一条读盘路径（`resolve_dict_file` + `cache_path` +
    /// `CachedDict::load_at_with`），只返回 `Result`、不 panic：失败由影子层 `warn!` 后跳过该库
    /// （spec §4.3）。它可能 mmap 或首建 wdat，大扩展库上是秒级读盘，故由后台线程预热
    /// （[`Self::warm_disabled_dicts_async`]），不留给首次反查模式查询的按键线程；预热没赶上时
    /// 按键线程等它建完或自己现建，之后复用，直到失效重建。开关关着时影子层不挂，从不调用。
    fn disabled_extra_sources(
        schema: &Schema,
        schemas_dir: &Path,
    ) -> Vec<crate::codetable::DisabledDictSource> {
        let Some((_, extras)) = Self::split_codetable_dicts(schema) else {
            return Vec::new();
        };
        extras
            .into_iter()
            .map(|e| {
                let full = Self::resolve_dict_file(&e.path, schemas_dir);
                let is_english = e.dict_type == "english";
                let schemas_dir = schemas_dir.to_path_buf();
                crate::codetable::DisabledDictSource {
                    id: e.id.clone(),
                    base_order: e.base_order,
                    default_weight: e.default_weight,
                    load: Arc::new(move || Self::load_table_dict(&full, is_english, &schemas_dir)),
                }
            })
            .collect()
    }

    /// 组装影子层：来源见 [`Self::disabled_extra_sources`]，启用集 = 本次 `load_codetable_layers`
    /// 实际挂上的扩展库 id（不是 `is_enabled()`：用户覆盖启用的已挂上、不进影子；启用但加载
    /// 失败的算未挂，影子层会再试一次、再失败也只 warn）。主库恒不在来源里。
    /// 无扩展库 ⇒ `None`，不挂：没有可查的未启用库，就不该为它付任何构造或预热开销。
    /// 开关（全局 `input.reverse.lookup_disabled_dicts`，影子层只给反查模式读）由调用方判。
    fn disabled_dict_layers(
        schema: &Schema,
        schemas_dir: &Path,
        loaded_extra_ids: Vec<String>,
    ) -> Option<crate::codetable::DisabledDictLayers> {
        let sources = Self::disabled_extra_sources(schema, schemas_dir);
        if sources.is_empty() {
            return None;
        }
        Some(crate::codetable::DisabledDictLayers::new(
            sources,
            loaded_extra_ids,
            weight_norm_of(schema),
        ))
    }

    /// 码表类词库（含英文）读盘的统一入口：英文词库额外叠英文词频名次表
    /// （[`ENGLISH_FREQ_RANKS`]，见 `wind_dict::freq_rank`）。
    ///
    /// 读英文词库的每一处都要走这里：名次表参与 wdat 指纹，有的带有的不带，同一槽位就
    /// 有两种指纹、两份缓存来回重建。名次表经 `resolve_dict_file` 定位，用户目录可整体替换。
    fn load_table_dict(
        full: &Path,
        english: bool,
        schemas_dir: &Path,
    ) -> anyhow::Result<CachedDict> {
        let wdat = cache_path(full, "wdat");
        if english {
            let ranks = Self::resolve_dict_file(ENGLISH_FREQ_RANKS, schemas_dir);
            CachedDict::load_english_at(full, &wdat, &ranks)
        } else {
            CachedDict::load_at_with(full, &wdat, false)
        }
    }

    /// 码表方案：主词库 + 每个启用的扩展词库各自加载为独立 system 层（主库 `codetable-system`
    /// 恒启用且排首位，扩展 `codetable-extra-<id>`），查询期由 CompositeDict 合并去重。
    fn load_codetable_layers(schema: &Schema, schemas_dir: &Path) -> Vec<CodetableLayer> {
        let resolve =
            |rel: &str| -> std::path::PathBuf { Self::resolve_dict_file(rel, schemas_dir) };
        let is_english =
            |e: &DictSpec| -> bool { !e.dict_type.is_empty() && e.dict_type == "english" };

        let Some((main, extras)) = Self::split_codetable_dicts(schema) else {
            return Vec::new();
        };

        let load_one = |e: &DictSpec| -> Option<CachedDict> {
            let full = resolve(&e.path);
            match Self::load_table_dict(&full, is_english(e), schemas_dir) {
                Ok(d) => Some(d),
                Err(err) => {
                    warn!("Failed to load codetable dict {}: {}", full.display(), err);
                    None
                }
            }
        };

        let mut out: Vec<CodetableLayer> = Vec::new();
        // 主库优先注册。加载失败 → 无系统层可用，放弃整方案（避免无候选）。
        match load_one(main) {
            Some(d) => {
                info!("  codetable main: {} ({} entries)", main.path, d.len());
                out.push(CodetableLayer {
                    name: "codetable-system".to_string(),
                    dict: d,
                    enabled: true,
                    base_order: main.base_order,
                    default_weight: main.default_weight,
                });
            }
            None => return Vec::new(),
        }
        // 扩展库只加载启用的：禁用的库不读、不建缓存、不映射（否则用户关掉后删不掉它的缓存
        // 文件）。启用时方案失效、下次使用重建才去加载（见 `set_dict_enabled_live`）。
        for e in extras {
            let enabled = e.is_enabled();
            if !enabled {
                info!(
                    "  codetable extra: {} (id={}, 未启用，跳过加载)",
                    e.path, e.id
                );
                continue;
            }
            if let Some(d) = load_one(e) {
                info!(
                    "  codetable extra: {} (id={}, enabled={}, {} entries)",
                    e.path,
                    e.id,
                    enabled,
                    d.len()
                );
                out.push(CodetableLayer {
                    name: format!("codetable-extra-{}", e.id),
                    dict: d,
                    enabled,
                    base_order: e.base_order,
                    default_weight: e.default_weight,
                });
            }
        }
        out
    }

    /// 方案实际参与构建的词库列表：启用的全部；一个都没启用时兜底取首个
    /// （否则整方案无候选，比"少一个扩展库"糟糕得多）。
    ///
    /// [`Self::load_dictionary`]（拼音引擎用）与 [`Self::load_dicts_individually`]（索引构建用）
    /// 必须选出同一批库，否则「候选里有的词，悬停却查不到编码」。
    fn enabled_dict_specs(schema: &Schema) -> Vec<&DictSpec> {
        let enabled: Vec<&DictSpec> = schema
            .dictionaries
            .iter()
            .filter(|d| d.is_enabled() && !d.path.is_empty())
            .collect();
        if !enabled.is_empty() {
            return enabled;
        }
        schema
            .dictionaries
            .iter()
            .filter(|d| !d.path.is_empty())
            .take(1)
            .collect()
    }

    /// 按方案加载全部启用词库，各自独立——不合并、不产出中间文件。
    ///
    /// 反查索引与单字全码表只需要全部条目，走这里而不是 [`Self::load_dictionary`]：后者多库时
    /// 要先合成大 `combined.wdat`，而两条路径产出的索引逐位相同（见
    /// [`wind_dict::cached::build_reverse_index_from`]）。选库规则经 [`Self::enabled_dict_specs`]
    /// 与 `load_dictionary` 同源。各库经 [`wind_dict::reader_pool`] 按路径共享 mmap，几乎零成本。
    fn load_dicts_individually(schema: &Schema, schemas_dir: &Path) -> Vec<CachedDict> {
        Self::enabled_dict_specs(schema)
            .iter()
            .filter_map(|e| Self::load_dict_individually(e, schemas_dir))
            .collect()
    }

    /// [`Self::load_dicts_individually`] 的单库那一步；词库统计要逐库对上 spec，单独开口。
    fn load_dict_individually(e: &DictSpec, schemas_dir: &Path) -> Option<CachedDict> {
        let full = Self::resolve_dict_file(&e.path, schemas_dir);
        let dtype = if e.dict_type.is_empty() {
            "rime_codetable"
        } else {
            e.dict_type.as_str()
        };
        match dtype {
            // rime 主表须经 import_tables 展开，否则只读到头部元数据。
            "rime_pinyin" => Self::load_rime_pinyin_dict(&full),
            // english：code 列小写化，与 load_codetable_layers 的缓存 tag 一致，
            // 否则会另建一份大小写不同的 wdat。
            t => Self::load_table_dict(&full, t == "english", schemas_dir).ok(),
        }
    }

    /// 全部已启用方案声明的已启用词库文件，按 `rel` 去重。
    ///
    /// 协调器拿它判断哪些词库自带 `comment` 列、派生注释源；那个判断属于 `wind-reverse`，
    /// 刻意不在这里做。
    ///
    /// 按 `rel` 分组、`schemas` 取并集：注释源按解析后路径去重挂载（`Coordinator::sync_comment_dicts`），
    /// 按 (方案, 词库) 各出一条的话第二条的方案 id 会被丢掉。`rel` 在用户/安装目录间不变，
    /// 适合当稳定 id。
    ///
    /// `import_tables` 的子表各自成条：rime 主表常只是一张清单（带 `comment` 列的子表藏在里面）。
    /// 子表路径相对主表所在目录且不带 `.dict.yaml`，`seen` 防环。扫全部已启用方案而非活跃
    /// 方案：注释源的挂载集合刻意不随方案抖动。
    pub fn declared_dict_files(&self) -> Vec<SchemaDictFile> {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return Vec::new();
        };
        let schemas_dir = data_dir.join("schemas");
        let mut out: Vec<SchemaDictFile> = Vec::new();
        for schema_id in self.available_schemas() {
            let Some(schema) =
                Self::read_schema(&schema_id, Some(data_dir), self.override_dir.as_deref())
            else {
                continue;
            };
            for e in Self::enabled_dict_specs(&schema) {
                // 待展开队列：`(rel, label)`。主表先入队，`import_tables` 的子表随后追加。
                let mut queue: Vec<(String, String)> = vec![(e.path.clone(), e.label.clone())];
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                while let Some((rel, label)) = queue.pop() {
                    if !seen.insert(rel.clone()) {
                        continue; // 防环：两张表互相 import，或多张表 import 同一张
                    }
                    let full = Self::resolve_dict_file(&rel, &schemas_dir);
                    let (name, imports) = Self::read_dict_head(&full);
                    for imp in imports {
                        // rime 的 `import_tables` 相对主表所在目录，且不带 `.dict.yaml`。
                        queue.push((
                            join_rel_dir(&rel, &format!("{imp}.dict.yaml")),
                            String::new(),
                        ));
                    }
                    let label = if label.is_empty() { name } else { label };
                    if let Some(hit) = out.iter_mut().find(|f| f.rel == rel) {
                        if !hit.schemas.contains(&schema_id) {
                            hit.schemas.push(schema_id.clone());
                        }
                        // 首个非空 label 胜出：先声明它的方案通常就是它的主人。
                        if hit.label.is_empty() {
                            hit.label = label;
                        }
                        continue;
                    }
                    out.push(SchemaDictFile {
                        rel,
                        path: full,
                        label,
                        schemas: vec![schema_id.clone()],
                    });
                }
            }
        }
        out
    }

    /// 把 `[[dictionary_dirs]]` 目录里的词库登记成扩展库，追加到 `base` 的 `dictionaries` 末尾。
    ///
    /// 生成项：`id = "dir:<相对路径去掉 .dict.yaml>"`（带前缀，不与方案手写的 id 撞；按完整
    /// 路径而非文件名，两个目录里的同名文件不会合成一项），`label` 取词库头的 `name:`、空则
    /// 文件名，出厂关（不写 `default_enabled`），码表的 `base_order` 排在已声明的库之后。
    ///
    /// 跳过两类文件：
    /// - 方案已手写声明的（同 path）——手写的有 label / base_order 等设计，优先；
    /// - 已被某个已声明词库的 `import_tables` 引用、且确实会被它加载的——它本就是那张
    ///   主表的一部分，再登记一次就是同一批词加载两遍。用户拿上游完整的 `rime_frost.dict.yaml`
    ///   连同 `cn_dicts_cell/` 一起放进用户目录时正是这样。「确实会加载」由 `head` 负责判定
    ///   （生产用 [`Self::dict_head_resolved`]）。
    ///
    /// `list_dir(sub)` 返回 `(相对 schemas/ 的路径, 绝对路径)`（生产用分层扫描），
    /// `head(rel)` 读该词库头部 → `(name, import_tables)`。两者都作为参数传入，单测不碰真实用户目录。
    fn inject_dir_dicts(
        base: &mut toml::Value,
        list_dir: &dyn Fn(&str) -> Vec<(String, std::path::PathBuf)>,
        head: &dyn Fn(&str) -> DictHead,
    ) {
        let Some(table) = base.as_table_mut() else {
            return;
        };
        Self::default_dict_dir(table);
        let dirs: Vec<(String, String)> = table
            .get("dictionary_dirs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| {
                        let path = d.get("path")?.as_str()?.trim_matches('/').to_string();
                        let ty = d.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        (!path.is_empty()).then(|| (path, ty.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if dirs.is_empty() {
            return;
        }
        let dicts = table
            .entry("dictionaries")
            .or_insert_with(|| toml::Value::Array(Vec::new()));
        let Some(arr) = dicts.as_array_mut() else {
            return;
        };

        let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut next_order: i64 = 0;
        for d in arr.iter() {
            next_order = next_order.max(
                d.get("base_order")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(0)
                    + 1,
            );
            let Some(rel) = d.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            taken.insert(rel.to_string());
            // import_tables 子表相对主表所在目录（同 `rime_source_paths`）
            let parent = rel.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            for imp in head(rel).imports {
                let sub = format!("{imp}{DICT_YAML_SUFFIX}");
                taken.insert(if parent.is_empty() {
                    sub
                } else {
                    format!("{parent}/{sub}")
                });
            }
        }

        for (dir, ty) in dirs {
            for (rel, _abs) in list_dir(&dir) {
                if !taken.insert(rel.clone()) {
                    continue;
                }
                let stem = rel.strip_suffix(DICT_YAML_SUFFIX).unwrap_or(&rel);
                let file = stem.rsplit_once('/').map(|(_, f)| f).unwrap_or(stem);
                let DictHead { name, group, .. } = head(&rel);
                let mut t = toml::map::Map::new();
                t.insert("id".into(), format!("dir:{stem}").into());
                t.insert(
                    "label".into(),
                    if name.is_empty() {
                        file.to_string()
                    } else {
                        name
                    }
                    .into(),
                );
                if !group.is_empty() {
                    t.insert("group".into(), group.into());
                }
                t.insert("description".into(), format!("自动识别：{rel}").into());
                t.insert("path".into(), rel.clone().into());
                t.insert("type".into(), ty.clone().into());
                t.insert("base_order".into(), next_order.into());
                next_order += 1;
                arr.push(toml::Value::Table(t));
            }
        }
    }

    /// 把用户分库登记成扩展库，追加到 `base` 的 `dictionaries` 末尾（设计
    /// `user-dict-libraries-and-external-edit.md` §3.2）。只给有自己词库的方案（码表 / 拼音 /
    /// 英文，同扩展词库的兜底范围）；混输的分库随成员方案走。
    ///
    /// 生成项：`id = "user:<名>"`、label 取词库头 `name:`（空则名字）、`path` 为绝对路径、类型同
    /// 主词库、`base_order = -3`（码表与用户词同档，排在系统库前；拼音不读它）、`enabled` 取
    /// `dicts.toml`（出厂开）。拼音族的数据方案折叠到 `pinyin`（与 [`Self::data_schema_id`] 同口径）。
    fn inject_user_dicts(base: &mut toml::Value, schema_id: &str, user_text_dir: &Path) {
        let Some(table) = base.as_table_mut() else {
            return;
        };
        let ty = table
            .get("engine")
            .and_then(|e| e.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_lowercase();
        if !["codetable", "pinyin", "english"].contains(&ty.as_str()) {
            return;
        }
        let ds = if ty == "pinyin" {
            PINYIN_DATA_SCHEMA
        } else {
            schema_id
        };
        let libs = wind_store::user_dicts::scan(user_text_dir, ds);
        if libs.is_empty() {
            return;
        }
        let Some(arr) = table.get_mut("dictionaries").and_then(|d| d.as_array_mut()) else {
            return;
        };
        let Some(main) = arr
            .iter()
            .find(|d| d.get("default").and_then(|v| v.as_bool()) == Some(true))
            .or_else(|| arr.first())
        else {
            return;
        };
        let dict_type = main
            .get("type")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or("rime_codetable")
            .to_string();
        let off = wind_store::user_dicts::off_names(user_text_dir, ds, schema_id);
        for l in libs {
            let label = wind_store::user_dicts::read_label(&l.path);
            let mut t = toml::map::Map::new();
            t.insert(
                "id".into(),
                format!("{}{}", wind_store::user_dicts::ID_PREFIX, l.name).into(),
            );
            t.insert(
                "label".into(),
                if label.is_empty() {
                    l.name.clone()
                } else {
                    label
                }
                .into(),
            );
            t.insert(
                "description".into(),
                format!("用户分库：{ds}/dicts/{}.dict.yaml", l.name).into(),
            );
            t.insert("path".into(), l.path.to_string_lossy().into_owned().into());
            t.insert("type".into(), dict_type.clone().into());
            t.insert("base_order".into(), (-3).into());
            t.insert("enabled".into(), (!off.contains(&l.name)).into());
            arr.push(toml::Value::Table(t));
        }
    }

    /// 码表 / 拼音 / 英文方案（`[engine] type = "codetable" | "pinyin" | "english"`）没写
    /// `dictionary_dirs` 键时，补一个兜底发现目录 `<主词库所在目录>/ext`，类型同主词库。出厂与
    /// 第三方方案不改文件就能放扩展词库。混输不兜底：它没有自己的词库。
    ///
    /// 补进方案本身而不是只在扫描时用：`schema.getConfig` 也就带上它，设置端的「打开扩展词库
    /// 目录」按钮不用另外推算。写了 `dictionary_dirs = []` 即显式关掉兜底。主词库在 `schemas/`
    /// 根下（路径无目录部分）时不兜底——那会去扫所有方案共用的 `schemas/ext`。
    fn default_dict_dir(table: &mut toml::map::Map<String, toml::Value>) {
        if table.contains_key("dictionary_dirs") {
            return;
        }
        let has_dict_dir = table
            .get("engine")
            .and_then(|e| e.get("type"))
            .and_then(|t| t.as_str())
            .is_some_and(|t| {
                ["codetable", "pinyin", "english"]
                    .iter()
                    .any(|k| t.eq_ignore_ascii_case(k))
            });
        if !has_dict_dir {
            return;
        }
        let Some(dicts) = table.get("dictionaries").and_then(|d| d.as_array()) else {
            return;
        };
        let Some(main) = dicts
            .iter()
            .find(|d| d.get("default").and_then(|v| v.as_bool()) == Some(true))
            .or_else(|| dicts.first())
        else {
            return;
        };
        let Some((parent, _)) = main
            .get("path")
            .and_then(|p| p.as_str())
            .and_then(|p| p.rsplit_once('/'))
        else {
            return;
        };
        let ty = main
            .get("type")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or("rime_codetable");
        let mut dir = toml::map::Map::new();
        dir.insert("path".into(), format!("{parent}/{DEFAULT_DICT_DIR}").into());
        dir.insert("type".into(), ty.into());
        table.insert(
            "dictionary_dirs".into(),
            toml::Value::Array(vec![toml::Value::Table(dir)]),
        );
    }

    /// [`Self::read_dict_head`]，但 `import_tables` 只留真能在主表目录找到文件的那些。
    ///
    /// 给发现目录的「已被主表引用就跳过」用：子表只按主表所在目录拼路径、不走分层
    /// （`rime_source_paths`），文件只在别的层时主表并不会加载它，只看清单就跳过会两头落空。
    #[cfg(test)]
    fn dict_head_resolved(path: &Path) -> (String, Vec<String>) {
        let h = Self::dict_head_resolved_full(path);
        (h.name, h.imports)
    }

    /// [`Self::dict_head_resolved`] 带 `group`。
    fn dict_head_resolved_full(path: &Path) -> DictHead {
        let mut h = Self::read_dict_head_full(path);
        let Some(dir) = path.parent() else {
            h.imports.clear();
            return h;
        };
        h.imports
            .retain(|imp| dir.join(format!("{imp}{DICT_YAML_SUFFIX}")).is_file());
        h
    }

    /// 读一个 `.dict.yaml` 的 YAML 头 → [`DictHead`]（`name` / `group` / `import_tables`）。
    ///
    /// 只读到独占一行的 `...`（正文起点）或 [`DICT_HEAD_SCAN_LIMIT`] 为止 —— 主词库正文
    /// 动辄几百 MB，而本函数每次启动对每张表都要跑一遍。几个字段来自同一次读。
    fn read_dict_head_full(path: &Path) -> DictHead {
        use std::io::BufRead;
        let Ok(f) = std::fs::File::open(path) else {
            return DictHead::default();
        };
        let mut name = String::new();
        let mut group = String::new();
        let mut imports: Vec<String> = Vec::new();
        let mut in_imports = false;
        let mut read = 0usize;
        for line in std::io::BufReader::new(f).lines() {
            let Ok(line) = line else {
                break; // 非 UTF-8 ⇒ 不是我们认的格式
            };
            read += line.len();
            if line.trim_end() == "..." || read > DICT_HEAD_SCAN_LIMIT {
                break;
            }
            // 剥行内注释（`  - cn_dicts/corrections # 错音错字提示`）
            let body = line.split('#').next().unwrap_or("");
            if in_imports {
                match body.trim().strip_prefix('-') {
                    Some(item) if !item.trim().is_empty() => {
                        imports.push(item.trim().to_string());
                        continue;
                    }
                    // 空行照旧留在块内（YAML 允许），回到非缩进键才算块结束
                    _ if body.trim().is_empty() => continue,
                    _ => in_imports = false,
                }
            }
            // 顶格的键才算数（缩进的同名键属于别的映射）
            if body.starts_with([' ', '\t']) {
                continue;
            }
            if let Some(v) = body.trim().strip_prefix("name:") {
                name = v.trim().trim_matches('"').trim_matches('\'').to_string();
            } else if let Some(v) = body.trim().strip_prefix("group:") {
                group = v.trim().trim_matches('"').trim_matches('\'').to_string();
            } else if body.trim() == "import_tables:" {
                in_imports = true;
            }
        }
        DictHead {
            name,
            group,
            imports,
        }
    }

    /// [`Self::read_dict_head_full`] 的 `(name, import_tables)` 视图。
    fn read_dict_head(path: &Path) -> (String, Vec<String>) {
        let h = Self::read_dict_head_full(path);
        (h.name, h.imports)
    }

    /// 清理本方案遗留的 `combined.wdat`（非拼音方案已无消费方，大方案上可达数百 MB）。
    ///
    /// 拼音方案的多库分支仍可能需要它（见 [`Self::load_dictionary`]），一律不动；判据取
    /// [`Schema::is_pinyin`]，它正确处理了 `engine.type` 缺省的方案。删不掉就算了，下次再试。
    fn purge_legacy_combined(schema: &Schema, schemas_dir: &Path) {
        if schema.is_pinyin() {
            return;
        }
        let Some(first) = Self::enabled_dict_specs(schema).first().copied() else {
            return;
        };
        let combined = cache_path(
            &Self::resolve_dict_file(&first.path, schemas_dir),
            "combined.wdat",
        );
        let Ok(meta) = std::fs::metadata(&combined) else {
            return; // 不存在＝已经清过或从未产生，正常路径
        };
        match std::fs::remove_file(&combined) {
            Ok(()) => {
                // `.fp` 与 `.st` 两个 sidecar 一起清
                wind_dict::cache_fp::remove_cache_fp(&combined);
                info!(
                    "已清理遗留的合并缓存 {}（释放 {:.1} MB，该文件已无消费方）",
                    combined.display(),
                    meta.len() as f64 / 1024.0 / 1024.0
                );
            }
            Err(e) => debug!(
                "遗留合并缓存 {} 暂时删不掉（{e}），不影响功能，下次再试",
                combined.display()
            ),
        }
    }

    /// 加载 schema 的词典：合并所有 enabled 词库（主词库 + default_enabled 附加库）。
    ///
    /// - 拼音（rime_pinyin）：单库经 import_tables 合并（load_rime_pinyin_dict）。
    /// - 多库 → `.combined.wdat`。
    ///
    /// 现存唯一的多库消费方是拼音引擎（`PinyinEngine` 只持有一个 `CachedDict`）。启用的库
    /// 全是 rime_pinyin 时，每个库各自走单库合并、组成 [`CachedDict::Layered`]：开关一张扩展库
    /// 只是多挂或少挂一层，主库的合并缓存不重建。只有混挂了别的类型才落到 `combined.wdat`，
    /// 那条路径按 `search_prefix` 枚举后重写，丢音节边界与简拼。
    ///
    /// 码表/英文的 live 查询走 `load_codetable_layers`；索引构建走 [`Self::load_dicts_individually`]。
    fn load_dictionary(schema: &Schema, schemas_dir: &Path) -> Option<CachedDict> {
        // 收集 enabled 词库（保持 schema 顺序：主库在前，扩展库在后）。
        // 与索引构建方共用同一份选取逻辑，见 enabled_dict_specs 的说明。
        let enabled = Self::enabled_dict_specs(schema);
        if enabled.is_empty() {
            warn!("No usable dictionary in schema");
            return None;
        }

        // 词典文件路径解析（含 wdat-only 探测）见 resolve_dict_file。
        let resolve =
            |rel: &str| -> std::path::PathBuf { Self::resolve_dict_file(rel, schemas_dir) };

        let dtype = |e: &DictSpec| {
            if e.dict_type.is_empty() {
                "rime_codetable".to_string()
            } else {
                e.dict_type.clone()
            }
        };

        // 拼音族多库：分层（见本函数文档）。某张扩展库建不出来只少一层，主库照常可用。
        if enabled.len() > 1 && enabled.iter().all(|e| dtype(e) == "rime_pinyin") {
            let main = resolve(&enabled[0].path);
            info!(
                "Loading dictionary: {} (type=rime_pinyin, 扩展库 {} 层)",
                main.display(),
                enabled.len() - 1
            );
            let mut layers = vec![Self::load_rime_pinyin_dict(&main)?];
            for e in &enabled[1..] {
                let full = resolve(&e.path);
                match Self::load_rime_pinyin_dict(&full) {
                    Some(d) => layers.push(d),
                    None => warn!("扩展词库 {} 加载失败，本次不挂这一层", full.display()),
                }
            }
            return Some(if layers.len() == 1 {
                layers.pop()?
            } else {
                CachedDict::Layered(layers)
            });
        }

        // 单库快路径
        if enabled.len() == 1 {
            let e = enabled[0];
            let full = resolve(&e.path);
            info!("Loading dictionary: {} (type={})", full.display(), dtype(e));
            return if dtype(e) == "rime_pinyin" {
                Self::load_rime_pinyin_dict(&full)
            } else {
                // 英文词库：code 列小写化（大小写不敏感前缀匹配，text 保留原样）。
                let lowercase = dtype(e) == "english";
                match Self::load_table_dict(&full, lowercase, schemas_dir) {
                    Ok(d) => {
                        info!("Dictionary loaded: {} entries", d.len());
                        Some(d)
                    }
                    Err(err) => {
                        warn!("Failed to load dictionary: {}", err);
                        None
                    }
                }
            };
        }

        // 多库：合并到 combined.wdat（缓存键 = 主词库路径 + .combined.wdat）
        let sources: Vec<(std::path::PathBuf, String)> = enabled
            .iter()
            .map(|e| (resolve(&e.path), dtype(e)))
            .collect();
        let combined = cache_path(sources[0].0.as_path(), "combined.wdat");
        Self::load_merged_dicts(&sources, &combined)
    }

    /// 把多个词库合并到一个 combined.wdat（按 code 聚合），并 mmap 打开。
    /// 每个源按其 dict_type 加载：rime_pinyin 先经 import_tables 展开。
    /// 缓存有效性：combined 比所有源都新则直接复用。
    fn load_merged_dicts(
        sources: &[(std::path::PathBuf, String)],
        combined: &Path,
    ) -> Option<CachedDict> {
        // 指纹须覆盖全部真实输入：rime_pinyin 源在构建时会展开 import_tables，
        // 只喂主表会让「改子表」无法使缓存失效。
        let expanded: Vec<std::path::PathBuf> = sources
            .iter()
            .flat_map(|(p, t)| Self::rime_source_paths(p, t))
            .collect();
        let paths: Vec<&Path> = expanded.iter().map(|p| p.as_path()).collect();
        // 按缓存文件 single-flight，理由见 reader_pool::file_lock（build_locks 的 key 是
        // schema_id，挡不住"两个方案指向同一个 combined.wdat"）。
        let build_lock = wind_dict::reader_pool::file_lock(combined);
        let _build_guard = build_lock.lock().unwrap_or_else(|e| e.into_inner());
        // 拿锁后复查。`combined` 是槽位，实际文件名带全部源的指纹（内容寻址）。
        // 复核快照先于算指纹取（理由见 load_rime_pinyin_dict）。
        let stamps_before = wind_dict::cache_fp::stamps_of(&paths);
        let content = wind_dict::cache_fp::resolve_slot_fp(combined, &paths, COMBINED_CACHE_TAG)
            .map(|fp| (wind_dict::cache_fp::slot_content_path(combined, &fp), fp));
        if let Some((path, _)) = &content
            && path.is_file()
            && let Ok(reader) = wind_dict::reader_pool::open_wdat(path)
        {
            info!(
                "Using combined cache: {} ({} keys)",
                path.display(),
                reader.key_count()
            );
            wind_dict::cache_fp::touch_cache(path);
            return Some(CachedDict::Mmap(reader));
        }

        // 构建期峰值远高于成品，理由与退避语义同 load_rime_pinyin_dict 里那段。
        let attempt = match wind_dict::build_guard::begin(
            combined,
            content
                .as_ref()
                .map(|(_, fp)| fp.as_str())
                .unwrap_or("unreadable"),
        ) {
            wind_dict::build_guard::Gate::Proceed(a) => a,
            wind_dict::build_guard::Gate::Skip { reason } => {
                error!("合并词库本次不可用：{reason}");
                return None;
            }
        };

        // 按 code 聚合所有源词库条目（前面的库优先级更高，先加入；同 text 取更高权重）
        let mut agg: HashMap<String, Vec<(String, i32)>> = HashMap::new();
        let mut total = 0usize;
        for (p, dict_type) in sources {
            // rime_pinyin 需经 import_tables 展开，否则只读到主文件元数据
            let loaded = if dict_type == "rime_pinyin" {
                Self::load_rime_pinyin_dict(p)
            } else {
                // 必须用与 load_codetable_layers 相同的缓存路径：源在只读安装目录，落在源旁
                // 会退化成内存模式、每次重解析；走 cache_path 则指纹命中即复用池中 mmap。
                CachedDict::load_at(p, &cache_path(p, "wdat")).ok()
            };
            match loaded {
                Some(d) => {
                    let n = d.len();
                    info!("  Merging {} entries from {}", n, p.display());
                    for (code, text, weight, _order) in d.search_prefix("", 5_000_000) {
                        let e = agg.entry(code).or_default();
                        if let Some(slot) = e.iter_mut().find(|(t, _)| t == &text) {
                            if weight > slot.1 {
                                slot.1 = weight; // 继承后续库中同词更高权重
                            }
                        } else {
                            e.push((text, weight));
                        }
                    }
                    total += n;
                }
                None => warn!("  Failed to load {}", p.display()),
            }
        }
        if total == 0 {
            attempt.finish();
            return None;
        }

        let mut writer = wind_dict::datformat::WdatWriter::new();
        for (code, mut entries) in agg {
            entries.sort_by_key(|e| std::cmp::Reverse(e.1));
            writer.add(code, entries);
        }
        // 源指纹算不出（读取失败）、或构建期间源被改写时，退回写到槽位本身：没有 `.fp`，
        // 派生缓存按大小:mtime 判，下次启动按新指纹重来（旧式文件由 prune 顺手清）。
        let content = if wind_dict::cache_fp::stamps_of(&paths).unchanged_since(&stamps_before) {
            content
        } else {
            warn!("合并词库的源在构建期间被改写，本次不落正式缓存");
            None
        };
        let target: std::path::PathBuf = content
            .as_ref()
            .map(|(p, _)| p.clone())
            .unwrap_or_else(|| combined.to_path_buf());
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let written = writer.write(&target);
        attempt.finish();
        match written {
            Ok(_) => {
                if let Some((p, fp)) = &content {
                    wind_dict::cache_fp::write_cache_fp_value(p, fp);
                    wind_dict::cache_fp::prune_slot(combined, p, 2);
                }
                match wind_dict::reader_pool::open_wdat(&target) {
                    Ok(reader) => {
                        info!(
                            "Wrote combined cache: {} ({} keys from {} dicts)",
                            target.display(),
                            reader.key_count(),
                            sources.len()
                        );
                        Some(CachedDict::Mmap(reader))
                    }
                    Err(e) => {
                        warn!("Failed to open combined cache: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("Failed to write combined cache: {}", e);
                None
            }
        }
    }

    /// 某个词库源实际参与构建的全部文件：主表本身，外加 `rime_pinyin` 的 `import_tables`
    /// 子表（子表不存在则跳过）。非 rime_pinyin 只有主表。
    ///
    /// merged 与 combined 两层缓存的指纹都用它：指纹漏掉任一真实输入，就是一条静默陈旧路径。
    fn rime_source_paths(dict_path: &Path, dict_type: &str) -> Vec<std::path::PathBuf> {
        let mut out = vec![dict_path.to_path_buf()];
        if dict_type != "rime_pinyin" {
            return out;
        }
        // 以下三条降级都会让 import_tables 整批读不到、拼音几乎无候选，必须留下日志。
        let content = match std::fs::read_to_string(dict_path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "词库 {} 读取失败（{e}），无法展开 import_tables，仅按主表处理。",
                    dict_path.display()
                );
                return out;
            }
        };
        // 已知局限：这里是朴素子串搜索，而非 codetable.rs 的
        // `rime_body_offset` 那种行精确匹配。若头部某个值里含字面量 `...`，会被提前截断。
        // 现存词库无一命中；要收紧须把行精确的分隔行定位提升为 wind-dict 的公开 API。
        let yaml_section = if let Some(start) = content.find("---") {
            let after = &content[start + 3..];
            after.find("...").map(|end| &after[..end]).unwrap_or(after)
        } else {
            &content
        };
        let yaml = match serde_yaml::from_str::<serde_yaml::Value>(yaml_section) {
            Ok(y) => y,
            Err(e) => {
                warn!(
                    "词库 {} 的 YAML 头部解析失败（{e}），无法展开 import_tables，仅按主表处理。",
                    dict_path.display()
                );
                return out;
            }
        };
        let Some(dir) = dict_path.parent() else {
            warn!(
                "词库路径 {} 没有父目录，无法定位 import_tables 子表。",
                dict_path.display()
            );
            return out;
        };
        if let Some(tables) = yaml.get("import_tables").and_then(|v| v.as_sequence()) {
            for t in tables {
                if let Some(name) = t.as_str() {
                    let sub = dir.join(format!("{}.dict.yaml", name));
                    if sub.exists() {
                        out.push(sub);
                    } else {
                        warn!(
                            "词库 {} 的 import_tables 声明了 {}，但 {} 不存在，已跳过。",
                            dict_path.display(),
                            name,
                            sub.display()
                        );
                    }
                }
            }
        }
        out
    }

    /// 给开启整句的码表引擎指明词频来源目录（见 `codetable::sentence::SentenceFreq`）。
    ///
    /// 词库走约定路径 `pinyin/rime_frost.dict.yaml`，不读拼音方案的 schema，免得码表行为随
    /// 另一个方案的配置漂移。只交路径、不在构建期同步加载（可能现场建 `merged.wdat`，切方案
    /// 会卡）；也不能懒到按键线程（首次解码数百毫秒），故交完路径立刻 `prewarm_sentence()`
    /// 推给后台线程。语法模型同理（权重 0 时根本不打开）。
    fn attach_sentence_freq(
        engine: CodeTableEngine,
        sentence_on: bool,
        grammar_weight: f64,
        schemas_dir: &Path,
    ) -> CodeTableEngine {
        if !sentence_on {
            return engine;
        }
        // 走 resolve_dict_file：同整句词频，模型同样可能被 custom / user 层替换。
        let gram = Self::resolve_dict_file(
            &format!(
                "pinyin/grammar/{}",
                crate::codetable::sentence::GRAMMAR_MODEL
            ),
            schemas_dir,
        );
        let engine = engine
            .with_sentence_schemas_dir(schemas_dir.to_path_buf())
            .with_sentence_grammar(gram, grammar_weight);
        engine.prewarm_sentence();
        engine
    }

    /// 加载整句词频用的拼音词库。由 `codetable::sentence` 的懒加载在首次解码时调用。
    pub(crate) fn load_sentence_freq_dict(schemas_dir: &Path) -> Option<CachedDict> {
        // 走 resolve_dict_file 而不是直接 join：这份词库同样可能被 custom / user 层替换。
        let path = Self::resolve_dict_file("pinyin/rime_frost.dict.yaml", schemas_dir);
        let d = Self::load_rime_pinyin_dict(&path)?;
        info!("码表整句：已接入拼音词库作为词频来源 {}", path.display());
        Some(d)
    }

    /// 一张 import_tables 子表的归并输入段：内容寻址缓存（改了哪张只重建哪张）。
    /// 返回 `(段, 若写在临时目录则其路径)`：落不了盘时写到临时目录当一次性段，调用方归并完
    /// 即删；连临时目录都写不了就放弃这张表（记 warn）。
    ///
    /// 源 yaml 不存在就跳过，不走 wdat-only 旁文件（那份 `.wdat` 不在 merged 的指纹里，更新后
    /// merged 不会失效）；空表也跳过。
    fn load_merge_segment(
        p: &Path,
    ) -> Option<(
        std::sync::Arc<wind_dict::datformat::WdatReader>,
        Option<std::path::PathBuf>,
    )> {
        if !p.is_file() {
            return None;
        }
        match CachedDict::load_at_with(p, &cache_path(p, "wdat"), false) {
            Ok(CachedDict::Mmap(r)) => {
                info!("  segment {} ({} keys)", p.display(), r.key_count());
                Some((r, None))
            }
            Ok(CachedDict::Memory(d)) => {
                if d.is_empty() {
                    return None;
                }
                static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let tmp = std::env::temp_dir().join(format!(
                    "wind-seg-{}-{}-{}.wdat",
                    std::process::id(),
                    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    wind_dict::cache_ns::cache_stem(p)
                ));
                let mut w = wind_dict::datformat::WdatWriter::new();
                d.export_to_wdat(&mut w);
                match w
                    .write(&tmp)
                    .and_then(|_| wind_dict::datformat::WdatReader::open(&tmp))
                {
                    Ok(r) => {
                        warn!(
                            "子表 {} 的段写不进缓存目录，本次用临时副本 {}",
                            p.display(),
                            tmp.display()
                        );
                        Some((std::sync::Arc::new(r), Some(tmp)))
                    }
                    Err(e) => {
                        warn!("  Failed to stage {}: {}", p.display(), e);
                        let _ = std::fs::remove_file(&tmp);
                        None
                    }
                }
            }
            Ok(CachedDict::Layered(_)) => None,
            Err(e) => {
                warn!("  Failed to load {}: {}", p.display(), e);
                None
            }
        }
    }

    /// 加载 rime_pinyin 词典（主表 + import_tables 子表归并到内容寻址的 `merged.wdat`）。
    fn load_rime_pinyin_dict(dict_path: &Path) -> Option<CachedDict> {
        // wdat-only：拼音是独立于 CachedDict::load_at_with 的第二条链路（要读 yaml 头展开
        // import_tables 再并行解析正文），源缺失时那两步全废，故须在此单独拦截。
        if !dict_path.is_file()
            && let Some(sidecar) = wind_dict::cached::wdat_sibling(dict_path)
            && sidecar.is_file()
        {
            return match wind_dict::reader_pool::open_wdat(&sidecar) {
                Ok(reader) => {
                    info!(
                        "以 wdat-only 模式加载拼音词库: {} ({} keys)",
                        sidecar.display(),
                        reader.key_count()
                    );
                    Some(CachedDict::Mmap(reader))
                }
                Err(e) => {
                    // 无源可重建，明确报错而非静默返回 None——后者会让整个拼音方案无引擎。
                    error!(
                        "wdat-only 拼音词库 {} 加载失败: {}。该词库无 yaml 源，无法重建，请更新词库文件。",
                        sidecar.display(),
                        e
                    );
                    None
                }
            };
        }
        // merged.wdat 写到可写缓存目录：安装目录通常只读，写在源旁会失败、拼音无候选。
        let merged_slot = cache_path(dict_path, "merged.wdat");
        // 按缓存文件 single-flight（不是按方案）：pinyin / shuangpin / 混输的拼音子引擎都指向
        // 同一个 merged.wdat。全部源（主表 + 子表）经 rime_source_paths 收集，与 combined 层共用。
        let build_lock = wind_dict::reader_pool::file_lock(&merged_slot);
        let _build_guard = build_lock.lock().unwrap_or_else(|e| e.into_inner());

        let sub_paths = Self::rime_source_paths(dict_path, "rime_pinyin");
        let src_refs: Vec<&Path> = sub_paths.iter().map(|p| p.as_path()).collect();
        // 复核快照要在算指纹之前取：否则「算完指纹、取快照之前」被改写的源，前后快照
        // 一致而指纹却对应旧内容。
        let stamps_before = wind_dict::cache_fp::stamps_of(&src_refs);

        // 内容寻址：槽位 + 全部源的指纹 → 实际文件名。任一源内容或源清单变化都换出另一个
        // 文件名，旧 merged 自然不会被复用。文件在，就是它。
        let content =
            wind_dict::cache_fp::resolve_slot_fp(&merged_slot, &src_refs, MERGED_CACHE_TAG).map(
                |fp| {
                    (
                        wind_dict::cache_fp::slot_content_path(&merged_slot, &fp),
                        fp,
                    )
                },
            );
        if let Some((path, _)) = &content
            && path.is_file()
        {
            match wind_dict::reader_pool::open_wdat(path) {
                Ok(reader) => {
                    info!(
                        "Using merged mmap cache: {} ({} keys)",
                        path.display(),
                        reader.key_count()
                    );
                    wind_dict::cache_fp::touch_cache(path);
                    return Some(CachedDict::Mmap(reader));
                }
                Err(e) => warn!("Stale merged cache ({}), regenerating", e),
            }
        }

        // 下面这段是构建峰值所在，内存不足时分配失败直接 abort、不可捕获（见 build_guard）。
        // 登记一次尝试：若连续死在这里，下次启动跳过而非再死。
        if content.is_none() {
            warn!(
                "拼音词库 {} 的源无法计算指纹（有源文件读取失败），本次 merged 只写临时目录",
                dict_path.display()
            );
        }
        let attempt = match wind_dict::build_guard::begin(
            &merged_slot,
            content
                .as_ref()
                .map(|(_, fp)| fp.as_str())
                .unwrap_or("unreadable"),
        ) {
            wind_dict::build_guard::Gate::Proceed(a) => a,
            wind_dict::build_guard::Gate::Skip { reason } => {
                error!("拼音词库 {} 本次不可用：{reason}", dict_path.display());
                return None;
            }
        };

        // ① 各子表各自的段：走 `load_at_with` 的内容寻址缓存，改一张子表只重建那一张。
        //    串行建段：并行会把大表的解析峰值叠在一起。段只做归并输入，不挂查询层。落不了盘
        //    的子表写到临时目录当一次性段，归并完即删。
        let t_segs = std::time::Instant::now();
        let mut temp_segments: Vec<std::path::PathBuf> = Vec::new();
        let readers: Vec<std::sync::Arc<wind_dict::datformat::WdatReader>> = sub_paths
            .iter()
            .filter_map(|p| {
                let (r, tmp) = Self::load_merge_segment(p)?;
                temp_segments.extend(tmp);
                Some(r)
            })
            .collect();
        if readers.is_empty() {
            attempt.finish();
            warn!("No entries loaded from pinyin dictionary");
            return None;
        }
        debug!(
            segments = readers.len(),
            segments_ms = t_segs.elapsed().as_millis() as u64,
            rss_mb = linux_rss_mb(),
            "merged: sub-table segments ready"
        );

        // ② k 路归并 → 流式写入器。各段的 `codes()` 游标按字节序递增；同 code 的候选按子表
        //    顺序拼接后按权重稳定降序、order 取序号（`merge_from_segments_matches_aggregate_build`
        //    锁定输出）。跨表同 (text, code) 不去重。
        let mut cursors: Vec<_> = readers.iter().map(|r| r.codes().peekable()).collect();
        let mut sw = wind_dict::datformat::WdatStreamWriter::new();
        // 简拼候选 `(简拼字节区间, code 序号, 权重)`：同 (简拼, code) 取最大权重，
        // 再按 (权重降序, code 升序) 排。
        let mut ab_buf: Vec<u8> = Vec::new();
        let mut ab_items: Vec<(u32, u16, u32, i32)> = Vec::new();
        let mut merged: Vec<(&str, i32, u64)> = Vec::new();
        let mut long_codes_without_abbrev = 0usize;
        while let Some(min_code) = cursors
            .iter_mut()
            .filter_map(|c| c.peek().map(|(k, _)| k.clone()))
            .min()
        {
            merged.clear();
            for (ti, c) in cursors.iter_mut().enumerate() {
                if c.peek().is_some_and(|(k, _)| *k == min_code) {
                    let (_, leaf) = c.next().expect("peeked");
                    for (t, w, _o, b) in readers[ti].leaf_entries(leaf) {
                        merged.push((t, w, b));
                    }
                }
            }
            merged.sort_by_key(|e| std::cmp::Reverse(e.1));
            let code_idx = sw.key_count() as u32;
            for (_, w, b) in &merged {
                if b.count_ones() >= 2 {
                    // 边界位是字节位置，但首字母要取整个 char：
                    // 音节以 ê / ü 之类非 ASCII 开头时只取一个字节会拼出非法 UTF-8。
                    let off = ab_buf.len() as u32;
                    for pos in 0..min_code.len().min(64) {
                        if b & (1u64 << pos) != 0
                            && let Some(ch) = min_code.get(pos..).and_then(|s| s.chars().next())
                        {
                            let mut tmp = [0u8; 4];
                            ab_buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                        }
                    }
                    let n = (ab_buf.len() as u32 - off) as u16;
                    ab_items.push((off, n, code_idx, *w));
                } else if *b == 0 && min_code.len() >= 64 {
                    // 音节起点 ≥64 的码没有边界位图（`syllable_boundary_mask`），推不出简拼；
                    // 这类超长词组极少，记数即可。
                    long_codes_without_abbrev += 1;
                }
            }
            sw.push_code(
                &min_code,
                merged
                    .iter()
                    .enumerate()
                    .map(|(i, (t, w, b))| (*t, *w, i as u32, *b)),
            );
        }
        drop(cursors);
        let total_entries = sw.entry_count();
        if long_codes_without_abbrev > 0 {
            info!(
                "  merged: {} 条 ≥64 字节的码没有边界位图，未生成简拼",
                long_codes_without_abbrev
            );
        }
        // 一条都没有（全部子表缺失或为空）就不落盘，返回 None
        if total_entries == 0 {
            attempt.finish();
            drop(readers);
            for t in &temp_segments {
                let _ = std::fs::remove_file(t);
            }
            warn!("No entries loaded from pinyin dictionary");
            return None;
        }

        let ab_of = |it: &(u32, u16, u32, i32)| -> &[u8] {
            &ab_buf[it.0 as usize..it.0 as usize + it.1 as usize]
        };
        ab_items.sort_by(|x, y| ab_of(x).cmp(ab_of(y)));
        let mut abbrev_count = 0usize;
        let mut i = 0usize;
        while i < ab_items.len() {
            let ab = ab_of(&ab_items[i]).to_vec();
            let mut group: Vec<(u32, i32)> = Vec::new();
            while i < ab_items.len() && ab_of(&ab_items[i]) == ab.as_slice() {
                group.push((ab_items[i].2, ab_items[i].3));
                i += 1;
            }
            // 同 (简拼, code) 只留权重最高的那条：先按 (code, 权重降序) 排让同 code 相邻、
            // dedup 留首条，再按 (权重降序, code 字典序) 排成写入序。
            group.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
            group.dedup_by_key(|e| e.0);
            let mut group: Vec<(&str, i32)> = group
                .into_iter()
                .map(|(ci, w)| (sw.code_at(ci as usize), w))
                .collect();
            group.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            let ab_str = std::str::from_utf8(&ab).unwrap_or("");
            let owned: Vec<(String, i32)> =
                group.iter().map(|(c, w)| (c.to_string(), *w)).collect();
            drop(group);
            sw.push_abbrev_sorted(ab_str, owned.iter().map(|(c, w)| (c.as_str(), *w)));
            abbrev_count += 1;
        }
        drop((ab_items, ab_buf));
        info!(
            "  merged pinyin: {} codes + {} abbrevs",
            sw.key_count(),
            abbrev_count
        );
        debug!(rss_mb = linux_rss_mb(), "merged: writer filled");

        info!("Writing merged .wdat cache ({} entries)...", total_entries);
        let temp_fallback = std::env::temp_dir().join(
            merged_slot
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("rime.merged.wdat")),
        );
        let blob = sw.build();
        drop(readers);
        for t in &temp_segments {
            let _ = std::fs::remove_file(t); // Windows 上若仍被映射删不掉，留给系统临时目录
        }
        // 指纹是建段之前算的；期间任一源被改写，按旧指纹命名的 merged 就会装着新内容。
        // 这时只写临时副本（不进正式路径、不写 .fp），下次启动按新指纹重来。
        let content = if wind_dict::cache_fp::stamps_of(&src_refs).unchanged_since(&stamps_before) {
            content
        } else {
            warn!(
                "拼音词库 {} 的源在构建期间被改写，本次不落正式缓存，下次启动重建",
                dict_path.display()
            );
            None
        };
        attempt.finish();
        // 正式目标是内容寻址的新文件（源指纹算不出时没有正式目标，只剩临时回退）
        let official: Option<std::path::PathBuf> = content.as_ref().map(|(p, _)| p.clone());
        let targets: Vec<&Path> = official
            .iter()
            .map(|p| p.as_path())
            .chain(std::iter::once(temp_fallback.as_path()))
            .collect();
        for target in targets {
            let is_fallback = target == temp_fallback.as_path();
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = blob.emit(target) {
                if is_fallback {
                    warn!("Failed to write merged cache {}: {}", target.display(), e);
                } else {
                    // 正式缓存写不进去是降级的起点，记 error。别归因于「文件被 mmap 占用」：
                    // Windows 上 rename 覆盖正被映射的文件会成功（见 reader_pool 的回归测试）。
                    error!(
                        "写入 merged 缓存失败 {}: {}。常见原因是缓存目录不可写、磁盘空间不足\
                         或权限受限。接下来会退到临时目录副本。",
                        target.display(),
                        e
                    );
                }
                continue;
            }
            // 正式路径：`.fp` 写在实际文件旁供派生缓存取摘要（指纹已在文件名里，不重算），
            // 并把同槽位的旧份数收到 2 份。
            if !is_fallback && let Some((p, fp)) = &content {
                wind_dict::cache_fp::write_cache_fp_value(p, fp);
                wind_dict::cache_fp::prune_slot(&merged_slot, p, 2);
            }
            match wind_dict::reader_pool::open_wdat(target) {
                Ok(reader) => {
                    if is_fallback {
                        // 功能可用但这是降级：副本无法与正式缓存共享映射，且下次启动会原样再走一遍。
                        error!(
                            "拼音词库已降级为临时目录副本: {} ({} keys)。功能可用，但该副本\
                             无法与正式缓存共享映射（同一份词库被映射两次），且指纹只写正式\
                             路径——下次启动会原样再走一遍。请检查缓存目录是否可写。",
                            target.display(),
                            reader.key_count()
                        );
                    } else {
                        info!(
                            "Using merged mmap cache: {} ({} keys)",
                            target.display(),
                            reader.key_count()
                        );
                    }
                    return Some(CachedDict::Mmap(reader));
                }
                Err(e) => warn!("Failed to open merged cache {}: {}", target.display(), e),
            }
        }
        error!("merged 缓存的正式路径与临时回退路径均写入失败，拼音词库不可用（该方案将无候选）");
        None
    }
}

/// 反查索引的保留判据：本次请求的、主码表、以及当前在用集合里的（联想方案、辅助码引用的方案）。
///
/// 在用的都要留，否则辅助码引用的方案、联想方案与主码表会互相顶掉、反复秒级重建。
fn reverse_index_keeps(k: &str, requested: &str, primary: &str, pins: &[String]) -> bool {
    k == requested || k == primary || pins.iter().any(|p| p == k)
}

/// `[[dictionaries]]` 条目里写了 `font_path` / `font_family` 的，逐项返回描述。
///
/// 这两个键不是 `DictSpec` 的字段，serde 会静默丢弃；但 `[engine.chaizi]` 有同名键，方案作者
/// 容易照样写进词库段。方案字体的正路是 `[candidate] font_family`。收进 `degraded_items`
/// 走现成的 toast 通路，不给 `DictSpec` 加两个没人读的字段。
fn misplaced_dict_font_keys(schema: &toml::Value) -> Vec<String> {
    let Some(dicts) = schema.get("dictionaries").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    dicts
        .iter()
        .filter_map(|d| d.as_table())
        .flat_map(|d| {
            let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            ["font_path", "font_family"]
                .into_iter()
                .filter(|k| d.contains_key(*k))
                .map(move |k| format!("[[dictionaries]] {id}.{k}（请改用方案设置里的候选字体）"))
        })
        .collect()
}

mod dict_stats;
mod dict_view;

/// 全局模糊音配置 → 引擎 `FuzzyConfig`：`enabled` 作总开关，未启用时所有模糊标志归零。
/// 双拼同键音节不来自用户配置，由 `with_shuangpin` 从布局推出。`hint` 是管理器的共享开关
/// （值见 [`pinyin_hint_on`]），运行时翻转、不随这里固化。换位纠错规则（`[input.correction]`
/// 生成）不受模糊音 `enabled` 管，原样挂上；双拼由调用方传空。
fn fuzzy_config_from_global(
    f: &wind_config::PinyinFuzzy,
    hint: &crate::pinyin::fuzzy::HintSwitch,
    correction: &Arc<crate::pinyin::correction::CorrectionRules>,
    misread: Option<&Arc<crate::pinyin::misread::MisreadTable>>,
) -> crate::pinyin::fuzzy::FuzzyConfig {
    let on = f.enabled;
    crate::pinyin::fuzzy::FuzzyConfig {
        zh_z: on && f.zh_z,
        ch_c: on && f.ch_c,
        sh_s: on && f.sh_s,
        n_l: on && f.n_l,
        f_h: on && f.f_h,
        r_l: on && f.r_l,
        an_ang: on && f.an_ang,
        en_eng: on && f.en_eng,
        in_ing: on && f.in_ing,
        ian_iang: on && f.ian_iang,
        uan_uang: on && f.uan_uang,
        un_ong: on && f.un_ong,
        eng_ong: on && f.eng_ong,
        hint: hint.clone(),
        correction: correction.clone(),
        misread: misread.cloned(),
        ..Default::default()
    }
}

/// `[input.correction]` 的变化要不要重建拼音引擎：纠错规则（`enabled` / `rules` / `custom`）
/// 在建引擎时并进音节表，「误读」勾选决定建引擎时挂不挂误读表。其余（标记、提示的显示与
/// 来源）运行时读，不用重建。
fn correction_rebuild_needed(
    old: &wind_config::CorrectionConfig,
    new: &wind_config::CorrectionConfig,
) -> bool {
    old.enabled != new.enabled
        || old.rules != new.rules
        || old.custom != new.custom
        || old.hint.misread != new.hint.misread
}

/// 引擎算不算正确拼音提示：正文内嵌（`hint.enabled`）或模板引用 `${correct_pinyin}`（`correct_pinyin_tpl`）
/// 任一要它，且至少一个勾选的来源能产出（模糊来源要模糊音开、纠错来源要纠错开）。引擎各来源
/// 一并算，按来源取舍由协调器做。
fn pinyin_hint_on(
    f: &wind_config::PinyinFuzzy,
    corr: &wind_config::CorrectionConfig,
    correct_pinyin_tpl: bool,
) -> bool {
    let h = &corr.hint;
    (h.enabled || correct_pinyin_tpl)
        && ((f.enabled && h.fuzzy) || h.misread || (corr.enabled && h.correction))
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
