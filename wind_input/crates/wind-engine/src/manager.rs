//! 引擎管理器
//!
//! 与 Go 版本 `wind_input/internal/engine/manager.go` 对齐。
//!
//! 职责：
//! - 预加载所有可用方案的词典与引擎（Pinyin / CodeTable）
//! - 持有当前活跃方案，支持运行时切换 / 循环切换
//! - 将 `convert` 请求分发到当前引擎
//!
//! 词典加载逻辑从原 `wind_service::bridge_impl` 下沉至此，使引擎层自洽。

use crate::codetable::CodeTableEngine;
use crate::encoder;
use crate::engine::{BoundaryResolution, ConvertResult, Engine, EngineType};
use crate::freq_rerank::ProtectPolicy;
use crate::pinyin::{Config as PinyinConfig, PinyinEngine};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, warn};
use wind_candidate::CandidateSource;
use wind_config::Config;
use wind_config::schema::{DictSpec, Schema, merge_toml};
use wind_dict::cached::{CachedDict, ReverseIndex};

// 方案定义已统一到 wind_config::schema::Schema（取代此前的私有 SchemaFile）。
// 引擎只消费该共享类型；构建逻辑（build_engine）保持不变。

/// 拼音族共享数据归属命名空间：所有拼音引擎方案（全拼/双拼）的用户词/临时词/词频
/// 统一落此键空间（P2c）。区别于恰好同名的真实方案 id "pinyin"（如临时拼音默认目标）。
pub const PINYIN_DATA_SCHEMA: &str = "pinyin";

/// 内置英文方案 id（`data/schemas/english.schema.toml`）。
///
/// 它有**两个入口**：作为 active 方案常驻，或被临时英文 overlay 引用——两者打的是同一份
/// 词库，故用户数据（词频 / 候选调整）也归同一个桶，见 `Coordinator::effective_data_schema`。
/// 融合英文候选（混输 / 快捷输入）同样引用它，但那些场景用户在写中文句子，只借词库不共享
/// 上屏行为。
pub const ENGLISH_SCHEMA: &str = "english";

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
    /// **位次减半**：候选每积累一次有效使用，目标位次除以 2，随半衰期回落
    /// （`docs/design/freq-rerank-model.md`）。
    ///
    /// 与 `Top`/`Step` 的本质差别：后两者是**布尔 used-first**——只要用过一次就整体跳到
    /// 档内最前，策略只决定「已用过的那批内部怎么排」；`Position` 则让位次连续表达强弱，
    /// 用得越多爬得越前，没有「用过 / 没用过」这道台阶。
    ///
    /// 适合**前缀匹配为主**的方案（英文尤其——它几乎所有候选都是前缀匹配），那里
    /// 布尔闸门会让任何选过一次的词直接顶到最前，过于粗暴。
    Position,
}

/// 前缀补全参与词频位置提升的范围。判据是**语义单元数**
/// （[`wind_candidate::semantic_units`]：汉字逐字计、西文词整体计 1），不是字符数——
/// 英文候选 `hello` 有 5 个 char，按字符数会被「只提升单个」挡死，而英文所有候选都是
/// 前缀匹配，那等于英文调频全灭。
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
/// 按方案解析后缓存，避免每键读盘（frequency.md §8）。
#[derive(Debug, Clone, Copy)]
pub struct FreqSettings {
    /// 词频维度主开关（全局 schema.{codetable,pinyin}.frequency.enabled）；关则完全不重排。
    pub enabled: bool,
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
    /// ⚠️ 本项**按候选来源生效、不按当前方案**，故 `freq_settings` 的三个分支取的是
    /// 同一个值——混输方案里混进来的英文候选同样要按英文的口径记账。
    pub english_code_by_input: bool,
}

impl Default for FreqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            strategy: FreqStrategy::Step,
            protect: ProtectPolicy::NONE,
            promote_prefix: PromotePrefix::Single,
            english_code_by_input: false,
        }
    }
}

/// 「作为混输辅助的拼音」才有的收敛项。
///
/// 存在的理由：有些拼音行为只在混输语境下需要收紧——混输的击键串**同时是码表码**，
/// 把它整串当拼音解读会抢走码表候选的首位。这类判据不能从全局配置读（纯拼音方案不该
/// 受影响），只能由调用方按语境注入，故设为 `build_engine` 的参数而非配置字段。
///
/// 字段承载具体取值，「是不是混输辅助」由 [`MixedRole`] 表达。此前这里是个裸
/// `Option<bool>`（只有简拼开关），语境判据与取值挤在一个布尔里，加第二个收敛项时无处安放。
#[derive(Clone, Copy)]
struct MixPinyinOpts {
    /// `schema.mix.enable_pinyin_abbrev`：是否产出简拼候选。
    abbrev: bool,
}

/// 码表引擎的整句开关最终取值。
///
/// 作为混输主引擎构建时取**混输方案自己**的声明，否则取本方案的。抽成纯函数是为了让
/// 「混输不继承 primary_schema 的整句声明」这条语义可以直接单测——它埋在
/// `build_engine` 里的话，要跑通整个引擎构建才验得到。
fn resolve_sentence_input(role: Option<MixedRole>, own: bool) -> bool {
    match role {
        Some(MixedRole::Primary { sentence_input }) => sentence_input,
        _ => own,
    }
}

/// 递归构建混输子引擎时，告诉被构建方「你在给谁当零件」。`None` = 独立方案。
///
/// 此前这个位置是裸的 `Option<MixPinyinOpts>`——`None`/`Some` 兼作「是不是混输辅助拼音」
/// 的判据。主引擎那一侧也需要按语境收敛（见 [`Self::Primary`]）之后，那个二值判据就不够用了：
/// 它表达不了「是混输的**主**引擎」这第三种情形。
#[derive(Clone, Copy)]
enum MixedRole {
    /// 混输主（码表）。
    ///
    /// `sentence_input` 由**混输方案自己**的 `[engine.codetable]` 决定，**不继承
    /// primary_schema 的取值**：`wubi86` 开了整句不代表 `wubi86_pinyin` 也该开——
    /// 后者的超码长区间已经归拼音管（`MixedEngine::convert` 超码长直接走
    /// `convert_overflow`，根本不经过主引擎），继承过来只会得到一个「配置开着却不生效」
    /// 的状态，那是最难排查的一种。
    Primary { sentence_input: bool },
    /// 混输次（拼音），携带拼音侧的语境收敛。
    Secondary(MixPinyinOpts),
}

/// 一个待注册的码表词库层（[`EngineManager::load_codetable_layers`] 的产出）。
///
/// 此前是个 5 元组。具名后消费点 `l.default_weight` / `l.base_order` 自证，
/// 不再靠位置对齐。
struct CodetableLayer {
    name: String,
    dict: CachedDict,
    enabled: bool,
    /// `[[dictionaries]].base_order`：库间硬分档。
    base_order: i32,
    /// `[[dictionaries]].default_weight`：抹平整库权重。
    default_weight: Option<i32>,
}

/// 方案级 `[weight_spec]` → [`wind_dict::WeightNorm`]，施加到本方案**全部**词库层。
///
/// 未配置 → `None`（守约方案的常态，不做任何换算）。
///
/// ## ⚠️ 必须是方案级、不可退回按库
///
/// 全方案共用一个映射函数，单调映射天然保序 ⇒ **库间相对关系原样保留**。
/// 按库配过一版，实测有反转：扩展库单独归一化后其条目反超主库，而作者写的
/// `base_order` 救不回来（`better()` 里 weight 在 base_order 之前）。
/// 根因是**两个不同的映射函数之间没有保序保证**。见 `Schema::weight_spec`。
///
/// ⚠️ **配了但参数不自洽时告警而非静默跳过**：那会让方案作者以为「配了就生效了」，
/// 而实际什么也没发生——本仓「配置就位、消费点不可达」那一类静默失效的同款形态。
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
/// **实例身份 = 方案 id**。此前这是 `config.schema.special_modes` 的一个数组条目，
/// 身份靠数组下标；那让「一个 key 一个控件」的设置页模型套不上，也让预置配置文件
/// 写不进去（写出即冻结快照）。见 `docs/redesign/overlay-mode-config.md`。
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
    /// 已解析的码表文件绝对路径。**`enabled == false` 时恒空**（关闭即不解析）。
    pub files: Vec<std::path::PathBuf>,
}

/// 引擎管理器（懒加载：仅在需要时构建对应方案引擎，降低启动内存）
/// 某个码位区间在词库里的命中情况，[`EngineManager::scan_chars_in_range`] 的产出。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RangeScan {
    /// 命中的字符，按码位升序、去重。
    pub chars: Vec<char>,
    /// **受影响的词条数**（去重）：含有区间内任一字符的词条有多少条。
    ///
    /// ★ 与 `chars.len()` 是两个数量级的事，界面必须显示这个：虎码里全角逗号 `，` 只是
    /// 1 个字符，却出现在 326 条词条里（词条内部的标点）。只报「1 个字符」，谁都会
    /// 毫不犹豫地把它设为生僻，那 326 个词跟着一起判非常用。
    ///
    /// ⚠️ 刻意**不是**「各字符命中数之和」：一条词条里同时出现两个注音字符时，那种口径
    /// 会把它数两遍，答出来的就不再是「会影响多少条词」。
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
            // ⛔ 空白与控制字符即便落在区间里也不收（`is_markable`）：`ASCII` 块整块可批量，
            // 而词库里含空格的词条成百上千，不挡就会给空格登记一条覆盖——含空格的候选
            // 从此全判非常用，设置页多出一行「看不出是什么也点不掉」的空白，导出还能写
            // 进去、导入却被拒，往返都对不上。
            //
            // 挡在**扫描**这一层而不是写入前：预览与执行读的是同一份 `chars`，在这里滤掉
            // 才能保证「预览说 N 个」与「实际写 N 个」始终一致。
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

pub struct EngineManager {
    /// schema_id -> 引擎实例（懒加载，Arc 便于无锁 convert）
    engines: Mutex<HashMap<String, Arc<dyn Engine>>>,
    /// 当前活跃方案 ID
    active: Mutex<String>,
    /// 活跃方案的**变更代际**：每次 `active` 真正改变时 +1。
    ///
    /// 供上层判断「自我上次记录以来，活跃方案有没有被动过」——只比对 id 是做不到的，
    /// 「切走又切回来」与「从未变过」在值上完全同形。当前消费者是协调器的方案往返键
    /// （`toggle_schema`）：来源记录必须在期间发生任何切换时作废，否则会把用户送回
    /// 几步之前的方案。
    ///
    /// 递增与 [`crate::active_hook::notify_active_changed`] **绑死在
    /// [`Self::on_active_changed`] 里**，不是各赋值点自己加。理由是漏掉通知会让设置界面
    /// 的方案显示不刷新（看得见、会被报），漏掉计数则只在往返键那个低频路径上出错
    /// （看不见）——把易漏的接线搭在不易漏的接线上。
    schema_generation: std::sync::atomic::AtomicU64,
    /// 可用方案列表（已过滤不支持的方案，用于循环切换）。
    /// Mutex 以支持配置热重载时原地更新（无需重建 EngineManager）。
    available: Mutex<Vec<String>>,
    /// 数据目录（懒加载时按需读取 schema）
    data_dir: Option<std::path::PathBuf>,
    /// redb 持久化存储（用户词/临时词层；None=无持久化，如纯测试/REPL）
    store: Option<Arc<wind_store::Store>>,
    /// 全局码表配置（公共基线；方案经 schema_overrides 的 [codetable] 段逐字段覆盖）。
    /// Mutex 以支持热重载（变更后清空引擎缓存按新策略重建）。
    codetable: Mutex<wind_config::CodetableGlobal>,
    /// 全局混输配置（融合策略；全局唯一，无方案级 override）。Mutex 以支持热重载。
    mix: Mutex<wind_config::MixGlobal>,
    /// 全局英文配置（英文方案的行为与调频；全局唯一）。Mutex 以支持热重载。
    english: Mutex<wind_config::config::EnglishGlobal>,
    /// 全局临时拼音配置（码表方案下临时切拼音反查；全局唯一）。Mutex 以支持热重载。
    temp_pinyin: Mutex<wind_config::config::TempPinyinConfig>,
    /// 词频排序设置缓存（schema_id -> FreqSettings；按需解析、避免每键读盘）
    freq_cache: Mutex<HashMap<String, FreqSettings>>,
    /// 方案引擎类型缓存（`schema_engine_type`）。**按 id 缓存，reload/invalidate 时清**，
    /// 与 `freq_cache`/`name_cache` 同生命周期。
    ///
    /// 存在理由是性能：`read_schema` 每次都要 `fs::read_to_string` + `toml::from_str`，
    /// 而 `apply_freq_rerank` 每次按键就要调 2 次（混输 5+ 次）——五笔单字母下与逐候选的
    /// redb 查询叠加，造成可感知卡顿。缓存 `None` 同样有意义：方案文件不存在时反复尝试
    /// 读盘，还会刷出成片的 `Schema file not found` 警告。
    schema_type_cache: Mutex<HashMap<String, Option<String>>>,
    /// 方案级 `[key_actions]` 表缓存（schema_id → 表）。
    ///
    /// **必须缓存**：消费点 `Coordinator::bound_action_for` 在按键热路径上，且进模式的两条
    /// 通路各调一次。没有它就是每键读两个文件（方案 + override）、解析两份 TOML、再反序列化
    /// 整个 `Schema`——`read_schema` 本身不带任何缓存。
    ///
    /// 与 `schema_type_cache` 等同批失效（`invalidate_schema`）。
    ///
    /// 值用 `Arc` 而非裸表：消费点在按键热路径上，返回 owned 表意味着**命中缓存也要
    /// clone 一遍整张表**（含每个键名与动词的 `String` 分配）。缓存本身是为了省去读盘与
    /// TOML 解析，clone 会把省下来的一部分又还回去。
    key_actions_cache: Mutex<HashMap<String, Arc<std::collections::BTreeMap<String, String>>>>,
    /// 方案级 `[session_actions]` 表缓存（schema_id → 表）。
    ///
    /// 与 [`Self::key_actions_cache`] 同构、**同批失效**：两个失效点（`invalidate_schema`
    /// 按 id 移除、配置热重载整表清空）都要接，漏一处的表现是「设置页改了不生效」。
    session_actions_cache: Mutex<HashMap<String, Arc<std::collections::BTreeMap<String, String>>>>,
    /// 方案级行为覆盖缓存（schema_id → `[punct]` / `[candidate]` / `[phrases]` 三段快照）。
    ///
    /// 三段合成**一个**缓存条目而不是各存各的：它们同源（一次 `read_schema`）、同批失效。
    /// 分三个缓存等于同一个方案读三次盘、加三个失效点——上面两个表已经因为「同批失效
    /// 要接两处」在注释里互相提醒过一次，不要把这个数量再翻倍。
    ///
    /// 与 [`Self::key_actions_cache`] 同批失效：`invalidate_schema` 按 id 移除、
    /// 配置热重载整表清空，两处都要接。
    behavior_cache: Mutex<HashMap<String, Arc<wind_config::SchemaBehavior>>>,
    /// 方案显示名缓存（schema_id -> schema.name；缺则回退 id）。按需读盘一次。
    name_cache: Mutex<HashMap<String, String>>,
    /// overlay 方案注册表缓存（见 [`Self::overlay_modes`]）。
    ///
    /// 与其它缓存不同，这是**整表**缓存而非 per-schema：它是一个集合，任何方案的
    /// 安装/删除/`[overlay]` 段变更都会改变它的内容**与下标**，按 id 局部失效没有意义。
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
    /// 内存护栏:每份索引可达数万词条,最多缓存两份(见 `reverse_index_for`)。
    reverse_index: Mutex<HashMap<String, Arc<ReverseIndex>>>,
    /// 码表**单字全码**表缓存:方案 id → (汉字 → 全码)。供造词按 `[[encoder.rules]]` 组装
    /// 词组编码(见 `encode_word`)。与 `reverse_index` 分开是刻意的——那份按「码长升序」排,
    /// 服务悬停 `[编码]` 的打法列表展示;这份要的是「按权重挑全码」,两种排序需求互斥。
    /// **只缓存一份**:造词恒对活跃方案(混输则其主码表)进行,切方案即弃,无需两份护栏。
    single_char_codes: Mutex<Option<SingleCharCodeCache>>,
    /// 全局拼音配置（fuzzy/show_code_hint/...）。Mutex 以支持热重载。
    pinyin: Mutex<wind_config::config::PinyinGlobalConfig>,
    /// 双拼韵母键集缓存：(已缓存的活跃方案 id, Option<HashSet<u8>>)。
    /// None = 当前活跃方案不是双拼；Some = 双拼布局的 finals 键集合。
    /// 活跃方案 id 变化时按需重建（惰性），避免每键读盘。
    shuangpin_finals_cache: Mutex<(String, Option<std::collections::HashSet<u8>>)>,
    /// 每方案构建锁（single-flight）：同一方案的引擎/缓存构建串行，避免后台预热与首次
    /// 切换并发时重复构建同一份大缓存；不同方案可并行构建（缓存在各自子目录，互不冲突）。
    build_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// 反查索引的每方案构建锁。与 `build_locks` **刻意分开**：索引构建（秒级、上百 MB）
    /// 不该和引擎构建互相阻塞，两者的等待方也不同（前者是后台线程，后者是切换方案的用户）。
    /// 兼作「是否正在建」的判据（`try_lock` 失败即在建），供打字线路决定要不要再 spawn。
    index_build_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

/// 进程级缓存根目录（%LOCALAPPDATA%\WindInput\cache），EngineManager::new 设置一次。
static CACHE_DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

/// 多库合并缓存（`combined.wdat`）的指纹 tag。
const COMBINED_CACHE_TAG: &str = "combined/v1";
/// rime_pinyin 主表+import_tables 合并缓存（`merged.wdat`）的指纹 tag。
const MERGED_CACHE_TAG: &str = "merged/v1";

/// 反查索引「小到可以直接读进内存」的上限（3 MB）。
///
/// 超过它就 mmap——两条路的查询行为完全相同（同一份字节布局、同一套查找代码），
/// 故这个值**只影响性能，不影响正确性**，取错了不会有人拿到错误的反查结果。
///
/// # 为什么是 3 MB
///
/// 真实索引规模是**离散的几档**，3 MB 落在最低两档之间的空档里，两端都不敏感
/// （2026-08-24 实测，`wind-dict` 的 `reverse_index_bench`）：
///
/// | 方案 | 词数 | 索引 | 重开耗时（常驻 / mmap） |
/// |---|---|---|---|
/// | wubi86（出厂） | 89081 | **2.3 MB** | **1.06 ms** / 9.37 ms |
/// | pinyin（出厂） | 640842 | 23.0 MB | 7.65 ms / 9.08 ms |
/// | feihuzj2（用户） | 2519693 | 88.2 MB | 29.3 ms / **9.73 ms** |
///
/// 小索引常驻明显更快——2.3 MB 顺序读只要 1 ms，而 mmap 的建映射开销近乎恒定 ~9 ms，
/// 在这个尺寸上纯属亏本。大索引则相反：常驻要多付 88 MB 私有内存，换来的**点查速度
/// 完全相同**（两者都是 0.25 µs/次，页缓存热了之后 mmap 只是普通内存读）。
///
/// # 为什么不做成配置键
///
/// `docs/architecture/config-design-rules.md` §R1：「差异可由程序判定 → 走自动判定，
/// 不加用户键」。索引大小是程序自己就知道的量，且两侧都正确——这个旋钮**只影响性能**。
/// 后续若要给高级用户开口子，走独立的调试用配置文件，不进 `config.toml`
/// （那会拖上 R6 五道闸门 + 文档站两页，为一个没人调得动的值）。
pub const REVERSE_INDEX_RESIDENT_MAX: usize = 3 * 1024 * 1024;

/// 单字全码表缓存项：`(方案 id, 汉字 → 全码)`。见 `EngineManager::single_char_codes`。
type SingleCharCodeCache = (String, Arc<HashMap<char, String>>);

// merge_toml（schema ⊕ schema_overrides 的深合并，含 dictionaries 按 id 稀疏合并的例外）
// 已上移至 wind_config::schema::merge_toml —— 方案包导出（wind-transfer）折叠 override 时
// 必须与这里的引擎加载视图同源，故合并实现只允许存在一份。

/// 源文件 → 缓存路径：`<cache>/<方案>/<文件名干>.<ext>`。
///
/// 用**每方案子目录**(父目录名=schemas/<方案>/ 即方案名)做命名空间，避免跨方案同名冲突，
/// 并把一个方案的全部缓存(主库/扩展/unigram/merged)归拢一处，便于整方案失效=删一目录。
/// 文件名干剥掉 `.dict.yaml` 的 `.dict` 冗余中缀(`rime_frost.dict` → `rime_frost`)。
/// 未设置缓存根时回退到源旁(保持旧行为，便于测试/无 LOCALAPPDATA 场景)。
fn cache_path(source: &Path, ext: &str) -> std::path::PathBuf {
    if let Some(Some(dir)) = CACHE_DIR.get() {
        let scheme = source
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut stem = source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(s) = stem.strip_suffix(".dict") {
            stem = s.to_string();
        }
        let base = if scheme.is_empty() {
            dir.clone()
        } else {
            dir.join(&scheme)
        };
        return base.join(format!("{stem}.{ext}"));
    }
    source.with_extension(ext)
}

/// 递归删除目录下的缓存产物（wdat 词库 / fp 指纹 / wdb unigram），best-effort：
/// 单个文件删除失败（如仍被 mmap 占用）计入 failed 继续。只认扩展名白名单，
/// 不触碰目录本身与其它文件（缓存根与用户数据同在 %LOCALAPPDATA% 命名空间下）。
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
            .is_some_and(|s| matches!(s, "wdat" | "fp" | "wdb" | "wridx"));
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
        let override_dir = Config::user_config_dir().map(|d| d.join("schema_overrides"));
        Self::with_store_override(config, data_dir, store, override_dir)
    }

    /// 同 [`with_store`]，但显式指定 override 目录（测试用，避免污染真实用户目录）。
    pub fn with_store_override(
        config: &Config,
        data_dir: Option<&Path>,
        store: Option<Arc<wind_store::Store>>,
        override_dir: Option<std::path::PathBuf>,
    ) -> Self {
        // 初始化缓存根（一次）：%LOCALAPPDATA%\WindInput\cache，提前建好目录
        CACHE_DIR.get_or_init(|| {
            let dir = Config::cache_dir();
            if let Some(d) = &dir {
                let _ = std::fs::create_dir_all(d);
            }
            dir
        });

        let active_id = config.active_schema().to_string();
        let mut available = config.schema.available.clone();
        if available.is_empty() {
            available.push(active_id.clone());
        }
        // 过滤不支持的方案（如双拼），但始终保留活跃方案
        let ov = override_dir.as_deref();
        available.retain(|sid| sid == &active_id || Self::schema_supported(sid, data_dir, ov));
        // 主码表方案(拼音反查码源):config 显式 > available 首个 codetable 类型方案。
        let primary_codetable = Self::resolve_primary_codetable(
            &config.schema.primary_codetable,
            &available,
            data_dir,
            ov,
        );

        let mgr = Self {
            engines: Mutex::new(HashMap::new()),
            active: Mutex::new(active_id.clone()),
            schema_generation: std::sync::atomic::AtomicU64::new(0),
            available: Mutex::new(available),
            data_dir: data_dir.map(|d| d.to_path_buf()),
            store,
            codetable: Mutex::new(config.schema.codetable.clone()),
            mix: Mutex::new(config.schema.mix.clone()),
            english: Mutex::new(config.schema.english.clone()),
            temp_pinyin: Mutex::new(config.input.temp_pinyin.clone()),
            freq_cache: Mutex::new(HashMap::new()),
            schema_type_cache: Mutex::new(HashMap::new()),
            key_actions_cache: Mutex::new(HashMap::new()),
            session_actions_cache: Mutex::new(HashMap::new()),
            behavior_cache: Mutex::new(HashMap::new()),
            name_cache: Mutex::new(HashMap::new()),
            overlay_cache: Mutex::new(None),
            override_dir,
            primary_codetable: Mutex::new(primary_codetable),
            primary_pinyin: Mutex::new(config.schema.primary_pinyin.clone()),
            reverse_index: Mutex::new(HashMap::new()),
            single_char_codes: Mutex::new(None),
            pinyin: Mutex::new(config.schema.pinyin.clone()),
            shuangpin_finals_cache: Mutex::new((String::new(), None)),
            build_locks: Mutex::new(HashMap::new()),
            index_build_locks: Mutex::new(HashMap::new()),
        };
        // 仅同步构建活跃方案；其余方案由 Coordinator 启动后台预热（prewarm_schema）提前构建，
        // 避免首次切换时同步重熔大词库卡顿。单飞构建锁保证预热与切换不重复构建。
        mgr.ensure_loaded(&active_id);
        mgr
    }

    /// 当前拼音方案是否显示编码提示(反查)。
    /// Task 1.5：改为直接读全局 [pinyin] 配置，不再读 schema 级 show_code_hint。
    /// (码表类方案的「剩余编码」由码表引擎在 convert 内处理，不走此路径。)
    pub fn pinyin_show_code_hint(&self) -> bool {
        self.pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .show_code_hint
    }

    /// 当前活跃引擎是否开启整句输入（当前只有码表引擎会返回 true）。
    /// 供协调器判定手动分隔符键是否放行，见 `Engine::sentence_input_enabled`。
    pub fn sentence_input_enabled(&self) -> bool {
        self.active_engine()
            .is_some_and(|e| e.sentence_input_enabled())
    }

    /// 拼音分隔符模式（auto/quote/backtick/none）的原始配置值。
    /// 分隔符键的最终判定（含 auto 动态避让候选选择键）在协调器侧完成——
    /// 因「`'` 是否为选择键」需读 `select_key_groups`（协调器配置），引擎无该信息。
    pub fn pinyin_separator_mode(&self) -> String {
        self.pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .separator
            .clone()
    }

    /// 当前活跃拼音方案是否为双拼（`engine.pinyin.scheme == "shuangpin"`）。
    /// 双拼不支持手动音节分隔符（`'` 会进 buffer 但引擎 convert 前剥除，致 buffer 与 preedit
    /// 发散、Backspace 删不可见字符），供协调器 gate。复用韵母键集缓存（Some 即双拼），
    /// 与 `shuangpin_final_key` 同源、方案切换/reload 自动失效。
    pub fn pinyin_is_shuangpin(&self) -> bool {
        let active_id = self.active_schema_id();
        {
            let cache = self
                .shuangpin_finals_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cache.0 == active_id {
                return cache.1.is_some();
            }
        }
        let finals_set = self.build_shuangpin_finals(&active_id);
        let is_sp = finals_set.is_some();
        *self
            .shuangpin_finals_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (active_id, finals_set);
        is_sp
    }

    // 注：曾有 `shuangpin_final_key(key) -> bool`（对齐 Go IsShuangpinFinalKey），供协调器
    // 在选词分支前避让双拼韵母键。**已删**——它只让键跳过选词，下游没人接住，键继续流向
    // 模式引导键与标点流水线，微软/搜狗/紫光的 `;` = ing 照样打不出。现由码元字符集
    // 单点仲裁：`PinyinEngine::input_chars` 从双拼布局推导，协调器的 `try_code_char_gate`
    // 抢在选词/引导键/标点**全部三条**之前接管。本处缓存仍为 `pinyin_is_shuangpin` 服务。

    /// 内部辅助：为指定方案 id 构建双拼韵母键集（非双拼返回 None）。
    fn build_shuangpin_finals(&self, schema_id: &str) -> Option<std::collections::HashSet<u8>> {
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
        let layout_id = if schema.engine.pinyin.shuangpin.layout.is_empty() {
            DEFAULT_SHUANGPIN_LAYOUT.to_string()
        } else {
            schema.engine.pinyin.shuangpin.layout.clone()
        };
        // 用户目录优先（resolve_schema_file）：%APPDATA%/…/schemas/shuangpin/<id>.toml
        // 存在即覆盖安装目录，使用户自带/覆盖的双拼布局生效。
        let lp = Self::resolve_schema_file(&format!("shuangpin/{layout_id}.toml"), data_dir);
        crate::pinyin::shuangpin::Layout::from_toml(&lp)
            .map(|lay| lay.final_key_set())
            .ok()
    }

    /// 指定方案**当前生效**的双拼布局 id（已合并 `schema_overrides/<id>.toml`）。
    ///
    /// 非双拼方案、或方案读不出来时返回空串。方案没写 `layout` 时返回缺省的 `xiaohe`
    /// ——与 [`build_shuangpin_finals`](Self::build_shuangpin_finals) 的兜底同源，
    /// 两处若分叉，设置页会显示一个和实际生效的不是同一个布局。
    pub fn shuangpin_layout_of(&self, schema_id: &str) -> String {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return String::new();
        };
        let Some(schema) =
            Self::read_schema(schema_id, Some(data_dir), self.override_dir.as_deref())
        else {
            return String::new();
        };
        if !schema
            .engine
            .pinyin
            .scheme
            .eq_ignore_ascii_case("shuangpin")
        {
            return String::new();
        }
        if schema.engine.pinyin.shuangpin.layout.is_empty() {
            DEFAULT_SHUANGPIN_LAYOUT.to_string()
        } else {
            schema.engine.pinyin.shuangpin.layout.clone()
        }
    }

    /// 拼音方案编码提示:返回主码表中 `text` 实际对应的编码(多码取最长者=全码,简码可能
    /// 被一级简码等占用),不存在返回空。对齐 Go `manager_convert.go` 的
    /// ApplyCodeHintsToCandidates——用主码表**反向索引**取实际码,而非按字生成码再校验
    /// (后者生成码常与码表实际码不一致,导致全被拒)。
    /// **返回 `None` 表示反查索引尚未就绪**，不是「查不到」——见 [`Self::word_codes_in`]
    /// 的三态说明。展示类调用方 `unwrap_or_default()` 即可。
    pub fn codetable_reverse_hint(&self, text: &str) -> Option<String> {
        let primary = self
            .primary_codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if primary.is_empty() {
            return Some(String::new()); // 没有主码表＝确定没有编码可显示，不是「没就绪」
        }
        Some(
            self.reverse_index_if_ready(&primary)?
                .codes_of(text)
                .and_then(|codes| codes.last().map(str::to_string))
                .unwrap_or_default(),
        )
    }

    /// 用**主拼音方案**为词推断带空格的音节码（`行长` → `hang zhang`），供候选注释的注音
    /// 消歧；方案不可加载或推不出返回空。
    ///
    /// # 为什么必须走 [`Self::generate_word_pinyin`] 而不是逐字取读音
    ///
    /// 逐字取最常用读音在**词组上系统性出错**：「行长」得 `xíng cháng`（两个字都错）、
    /// 「银行」得 `yín xíng`。而 `generate_word_pinyin` 是真消歧——枚举每字读音的笛卡尔积、
    /// 取第一个**能在拼音词典里查回该词**的组合，词典查不回的组合直接被排除。
    ///
    /// # 谁需要它
    ///
    /// **拼音来源候选不需要**（它们自带 `code` + `boundary`，那是词条真值，比推断更可靠）。
    /// 需要它的是**码表/短语等非拼音来源候选**——五笔方案下候选的 `boundary` 恒为 0、
    /// `code` 是形码，此前只能退到逐字首音。而五笔用户恰恰是「注音」这个功能最主要的受众
    /// （打得出但不会读），把消歧漏在这条路径上等于功能对主力场景失效。
    ///
    /// 成本：每次候选刷新 × 当前页 5~9 条，且仅在模板含 `${pinyin}` 时求值；`generate_word_pinyin`
    /// 自带 `MAX_READING_COMBOS`(64) 组合数护栏，超限即放弃推断。
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

    /// 查 `schema_id` 方案词库中 `text` 的**全部**实际编码,按码长升序以 `/` 连接
    /// (如 `a/ab/abc`,供悬停 [编码] 段);方案 id 为空或词不在词库返回空——
    /// 不按取码规则生成,生成码常与词库实际码不一致。
    /// # 三态返回（**别把后两者混为一谈**）
    ///
    /// - `None` —— **反查索引尚未就绪**（正在后台构建，或还没人触发构建）。
    /// - `Some("")` —— 索引已就绪，但这个词不在词库里。
    /// - `Some(codes)` —— 就绪且查到。
    ///
    /// 分开这两者是**正确性要求**，不是洁癖：加词去重（`handle_addword`）拿它判断
    /// 「系统词库里已经有这个码+词了吗」。若把「没就绪」当成「查不到」，去重判据直接失效
    /// ⇒ 往临时层写入一条系统词库已有的重复条目 ⇒ 候选出现重复项，且该条目会计入
    /// 提升计数、可能被**永久固化**进用户词库。这是一次真实的 wrong-action 风险，
    /// 而非「这一屏少显示点东西」。
    ///
    /// 展示类调用方（悬停 [编码] 段、候选注释）`unwrap_or_default()` 即可 —— 这一次
    /// 不显示编码，下一次索引好了自然就有了。
    ///
    /// **本方法绝不阻塞**。需要「等到就绪为止」的调用方用
    /// [`Self::word_codes_in_blocking`]，并且必须清楚自己会等上秒级。
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

    /// 同 [`Self::word_codes_in`]，但**索引没建好就地建**（可能阻塞秒级）。
    ///
    /// 只给「宁可等、也不能拿到错误答案」的调用方用——目前只有加词去重。
    /// ⚠️ **绝不可用在按键处理链路上**：TSF→服务是同步 IPC，那一等就是整机卡顿。
    pub fn word_codes_in_blocking(&self, schema_id: &str, text: &str) -> String {
        if schema_id.is_empty() {
            return String::new();
        }
        self.reverse_index_for(schema_id)
            .codes_of(text)
            .map(|codes| codes.join("/"))
            .unwrap_or_default()
    }

    /// **词语联想的词源方案**：从哪本词库里捞「以上文为前缀的更长的词」。
    ///
    /// # 混输必须解析到成员方案
    ///
    /// 混输方案**自己没有词库**（它引用两个成员），拿它的 id 去建反查索引会得到一张空表
    /// ——词语联想一条也出不来，且**完全静默**。真机实测（2026-08-16）活跃方案
    /// `wubi86_pinyin` 取到 0 条，而它的主码表成员 `wubi86` 取到正常结果。
    ///
    /// 这正是本仓反复出现的那个形状：功能在开发者的测试方案上好好的，在用户实际用的
    /// 方案上是死的。
    ///
    /// # 为什么不复用 `code_source_schema`
    ///
    /// 那个函数回答的是「编码提示该显示哪本码表的码」，其**拼音分支返回全局主码表**
    /// ——对编码提示是对的（拼音候选要显示对应的五笔码），对词语联想却完全错位：
    /// 用户在纯拼音方案下打字，联想词却从五笔码表里捞。两个问题只是碰巧在码表方案上
    /// 答案相同。
    pub fn assoc_word_schema(&self) -> String {
        let active = self.active_schema_id();
        match self.active_engine().map(|e| e.engine_type()) {
            Some(EngineType::Mixed) => self
                .mixed_primary_schema(&active)
                .unwrap_or_else(|| active.clone()),
            _ => active,
        }
    }

    /// **词语联想**取数：`schema_id` 词库里以 `prefix` 开头、且严格更长的词，
    /// 按词库权重降序取前 `limit` 条。返回 (整词, 权重)。
    ///
    /// 复用悬停 [编码] 段那份反查索引（词 → 编码，按词字节序排），前缀扫描是二分 + 顺序走。
    /// 索引本身懒构建、最多缓存两份——首次联想会触发一次全量构建（十万词级约几十毫秒），
    /// 之后常驻。
    pub fn assoc_prefix_words(
        &self,
        schema_id: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<(String, i32)> {
        if schema_id.is_empty() || prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        // 索引没就绪就这次不联想（**不阻塞按键线程**）。联想是锦上添花，且
        // `maybe_enter_assoc` 在无候选时直接返回、不改任何状态，降级完全无副作用。
        let Some(idx) = self.reverse_index_if_ready(schema_id) else {
            return Vec::new();
        };
        idx.texts_with_prefix(prefix, limit)
            .into_iter()
            .map(|(t, w)| (t.to_string(), w))
            .collect()
    }

    /// 已建好的反查索引；**没有就返回 `None`，绝不现建**。
    ///
    /// 供打字链路上的展示类消费方（候选注释 `${code}`、悬停 [编码] 段、词语联想）使用：
    /// 它们宁可这一次不显示编码，也不能让按键处理停下来等一次秒级构建
    /// ——TSF→服务是**同步 IPC**，那一停就是整机卡顿（真机实测 29.5 秒）。
    pub fn reverse_index_if_ready(&self, schema_id: &str) -> Option<Arc<ReverseIndex>> {
        self.reverse_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned()
    }

    /// 取 `schema_id` 的反查索引,缺则全量构建并缓存（**会阻塞秒级**）。
    ///
    /// 内存护栏:最多保留两份——本次请求方 + 全局主码表(悬停查活跃码表、拼音提示查主码表,
    /// 两者常为同一方案;方案切换的残留索引随下次构建清退)。
    ///
    /// ⚠️ **只该在两种场合调用**：后台预热线程，或用户主动发起、本就预期要等的操作
    /// （如加词去重校验——那里返回空表会导致**重复加词**，是正确性问题，不能降级）。
    /// 打字链路一律用 [`Self::reverse_index_if_ready`]。
    fn reverse_index_for(&self, schema_id: &str) -> Arc<ReverseIndex> {
        // primary 在 reverse_index 锁外取,避免嵌套锁。
        let primary = self
            .primary_codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // 快路径：已建好直接返回。
        if let Some(m) = self.reverse_index_if_ready(schema_id) {
            return m;
        }
        // ★ 构建**必须在锁外**：这是一次秒级、且要分配上百 MB 的操作，握着锁做等于
        //   让所有线程（包括查别的方案的）一起排队。此前正是持锁构建。
        //   代价是两个线程可能同时建同一份（少见且无害，各自算完最后一个写入者生效）。
        let m = Arc::new(self.build_reverse_index_for(schema_id));
        let mut guard = self.reverse_index.lock().unwrap_or_else(|e| e.into_inner());
        // 复查：等待期间别的线程可能已经建好并写入，此时沿用它，避免同一份索引在内存里
        // 存在两个副本（各 95MB 量级）。
        if let Some(existing) = guard.get(schema_id) {
            return existing.clone();
        }
        guard.insert(schema_id.to_string(), m.clone());
        guard.retain(|k, _| k == schema_id || k == &primary);
        m
    }

    /// 后台预热反查索引：把「首次使用时才建」提前到预热线程。
    ///
    /// 该索引原本是懒构建——第一次按键触发候选注释/悬停时才建，而它对大词库是秒级操作，
    /// 恰好落在打字的同步链路上。预热线程本就在启动后 1.5 秒跑，把这件事挪进去，
    /// 绝大多数用户就再也碰不到它。
    ///
    /// 幂等；返回是否真的执行了构建（供调用方计时/记日志）。
    ///
    /// 走 `index_build_locks` 单飞：并发调用只有一个真在建，其余等它建完后看到已就绪即返回。
    /// 与 `ensure_loaded` 同一套「取锁 → 复查」形态。
    pub fn prewarm_reverse_index(&self, schema_id: &str) -> bool {
        if schema_id.is_empty() || self.reverse_index_if_ready(schema_id).is_some() {
            return false;
        }
        let lock = self.index_build_lock_for(schema_id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        // 抢到锁后复查：等待期间可能已被另一线程建好。
        if self.reverse_index_if_ready(schema_id).is_some() {
            return false;
        }
        let _ = self.reverse_index_for(schema_id);
        true
    }

    /// 该方案的反查索引是否**正在后台构建**。
    ///
    /// 打字线路据此决定「要不要再起一个后台构建线程」——没有它，索引没建好期间的
    /// 每一次按键都会 spawn 一个新线程去建同一份东西。
    pub fn is_building_reverse_index(&self, schema_id: &str) -> bool {
        !schema_id.is_empty()
            && self.reverse_index_if_ready(schema_id).is_none()
            && self.index_build_lock_for(schema_id).try_lock().is_err()
    }

    /// 单字全码表是否已就绪。它与反查索引同为**惰性全量构建**（同一批词库、同一量级），
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

    /// 反查索引缓存路径：`<cache>/<方案目录>/<方案 id>.wridx`。
    ///
    /// # 为什么键是方案 id 而不是主词库路径
    ///
    /// 索引内容取决于该方案启用的**整组**词库，而「两个方案共用同一个主库、各挂不同扩展」
    /// 是常态。`combined.wdat` 当年正是按主库命名的，其代码注释里就写着这挡不住
    /// 「两个方案指向同一个 combined」——那会让两个方案反复顶掉对方的缓存。
    ///
    /// 方案 id 会成为文件名，故只保留 ASCII 字母/数字/`-`/`_`，其余一律换成 `_`：
    /// 目录部分沿用 `cache_path` 已经算好的方案目录，这里只换文件名，不引入新的路径拼接。
    ///
    /// **无缓存根时返回 `None` = 本次不落盘**。刻意不像 `cache_path` 那样回退到「源文件旁」
    /// ——词库源常在只读的安装目录，而反查索引是个上百 MB 的产物，落错地方比不落更糟。
    fn reverse_index_cache_path(schema_id: &str, first_dict: &Path) -> Option<std::path::PathBuf> {
        CACHE_DIR.get()?.as_ref()?;
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
        Some(cache_path(first_dict, "wridx").with_file_name(format!("{safe}.wridx")))
    }

    /// 各词库缓存产物的摘要列表，用作 `.wridx` 的二级指纹源（顺序即语义）。
    ///
    /// 任一词库处于内存模式（`source_file() == None`，即它的 wdat 写失败了）就返回 `None`
    /// ——那意味着这份词库**没有稳定的磁盘产物**，以它为源的索引不该落盘：下次启动
    /// 无从校验，只会复用一份来历不明的缓存。
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

    /// 按方案全量构建反查索引(汉字/词 → 全部编码,码长升序)。失败返回空表。
    ///
    /// # 为什么要落盘成 `.wridx`
    ///
    /// 索引在大词库上是**长尾灾难**：feihuzj2 方案 251 万词 → 95.4 MB 常驻，且最多缓存两份。
    /// 落盘后由 [`ReverseIndex::open`] 决定常驻还是 mmap（阈值
    /// [`REVERSE_INDEX_RESIDENT_MAX`]），大索引的字节因此完全不进程私有内存。
    ///
    /// **落盘与 mmap 是一件事的两面**：不持久化的话每次启动仍要全量重建（峰值一点没降），
    /// 还平白多写一次上百 MB 磁盘——严格比不做更差。所以这里的指纹校验不是优化，是前提。
    ///
    /// 指纹走「二级」通道（[`wind_dict::cache_fp::cache_digest`]）：源是各词库的 `.wdat`
    /// **摘要**而非 yaml 内容。照一级指纹读满 250 MB yaml 才能回答「缓存还能不能用」，
    /// 会把复用命中这条本该零成本的路径变成每次启动的固定开销。
    /// 扫某方案的全部**启用**词库，收出落在 `[start, end]` 码位区间内的字符。
    ///
    /// 供「按类型批量设常用/生僻」圈定范围。只收**词库里真实出现过**的字符，不按整个
    /// Unicode 块展开——「带圈 CJK 字母及月份」有 256 个码位而虎码一个都没编，全展开就是
    /// 一堆读端永远查不到的死记录，还会把设置页的列表撑长。
    ///
    /// ★ 返回的条目数不是装饰，界面**必须**显示它：全角逗号 `，` 只是 1 个字符，却出现在
    /// 326 条词条里（词条内部的标点）。只显示「1 个字符」，谁都会毫不犹豫地一键设为生僻，
    /// 那 326 个词跟着一起判非常用。字符数与影响面在这里差了两个数量级。
    ///
    /// ⚠️ O(全表)，只在用户手动点菜单时调用，**绝不能进按键链路**（同 `for_each_entry`）。
    /// 走 `load_dicts_individually` 而不是已加载的引擎：与反查索引构建同一条路径，不必为此
    /// 给 `Engine` trait 加一个九成实现都用不上的方法。
    /// ⚠️ 扫的是**全部已启用方案**，不是当前活跃方案：常用字覆盖是全局作用域（键就是一个
    /// 字，不带方案），扫描若绑定活跃方案，用户在五笔下就设不了只出现在虎码里的注音符号
    /// ——而他看到的候选正是从那个方案来的。作用域一致比省这点扫描时间重要。
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
        // 多个方案常共用同一份词库（五笔与五笔拼音），按源文件去重，否则 `entries`
        // 会把同一条词条数好几遍，报给用户的影响面就虚高了。
        //
        // ⚠️ 已知限制：`CachedDict::Memory`（mmap 失败时的回退形态）没有源文件，去重对它
        // 失效，跨方案共用时 `entries` 会偏高。刻意不为此另造一个键——这个数是给用户估
        // 影响面用的，**偏高比偏低安全**，而 Memory 形态本身已是异常路径。
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
        // 耗时留痕：这是全表遍历，是本功能唯一的重活。用户觉得「点了要等一下」时，
        // 这行日志能直接回答「等的是扫描还是别的」，不必再去猜。
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

    fn build_reverse_index_for(&self, schema_id: &str) -> ReverseIndex {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return ReverseIndex::default();
        };
        let schemas = data_dir.join("schemas");
        let Some(schema) =
            Self::read_schema(schema_id, Some(data_dir), self.override_dir.as_deref())
        else {
            return ReverseIndex::default();
        };
        let dicts = Self::load_dicts_individually(&schema, &schemas);
        if dicts.is_empty() {
            return ReverseIndex::default();
        }
        // 顺手清掉旧版留下的合并缓存（本方案已不再需要它）。
        Self::purge_legacy_combined(&schema, &schemas);

        let cache = dicts
            .first()
            .and_then(|d| d.source_file())
            .and_then(|p| Self::reverse_index_cache_path(schema_id, p));
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
                    return idx;
                }
                // 指纹说新鲜但打不开＝文件被截断/损坏。落到下面重建即可，但要留痕：
                // 静默重建会让「每次启动都慢」这类故障失去唯一的外部线索。
                Err(e) => warn!("反查索引缓存 {} 打不开（{e}），重建", c.display()),
            }
        }

        // ② 重建。
        let t0 = std::time::Instant::now();
        let image = wind_dict::cached::serialize_reverse_index_from(&dicts);
        let built = t0.elapsed();

        // ③ 落盘后**从盘上重新打开**——这一步才真正把索引字节移出进程私有内存。
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
                            return idx;
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
        idx
    }

    /// 取 `schema_id` 的单字全码表，缺则构建并缓存（只留一份，见字段注释）。
    fn single_char_full_codes(&self, schema_id: &str) -> Arc<HashMap<char, String>> {
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
        let Some(schema) =
            Self::read_schema(schema_id, Some(data_dir), self.override_dir.as_deref())
        else {
            return HashMap::new();
        };
        let cap = schema.engine.codetable.max_code_length;
        let dicts = Self::load_dicts_individually(&schema, &data_dir.join("schemas"));
        if dicts.is_empty() {
            return HashMap::new();
        }
        let idx = wind_dict::cached::build_single_char_full_codes_from(&dicts, cap);
        info!(
            "Built single-char full-code table: {} ({} chars, cap={}, {} dicts)",
            schema_id,
            idx.len(),
            cap,
            dicts.len()
        );
        idx
    }

    /// 按方案的 `[[encoder.rules]]` 为词计算码表词组编码（造词/加词统一入口）。
    ///
    /// 单字全码取自**该方案的码表词库**——码源与词库同源是刚性要求：造词的唯一目的是
    /// 「造出来的词以后能打出来」，用与词库解耦的静态资源（如拆字表）出码，用户换了词库
    /// 或加了扩展库就可能造出打不出的码。
    ///
    /// 任一字取不到码即整词失败，错误里带上是哪个字（见 [`encoder::EncodeError`]）。
    pub fn encode_word(&self, schema_id: &str, word: &str) -> Result<String, encoder::EncodeError> {
        let spec = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
        .and_then(|s| s.encoder)
        .unwrap_or_default();
        let codes = self.single_char_full_codes(schema_id);
        encoder::calc_word_code(word, &spec, |c| codes.get(&c).cloned())
    }

    /// 批量版 [`Self::encode_word`]：规则完全一致，但**方案只读一次**。
    ///
    /// 分出这个入口不是锦上添花：`read_schema` 没有缓存，每次调用都要读盘、解析 TOML、
    /// 合并 override 层。逐词调 `encode_word` 在万级词表上就退化成万次文件解析，
    /// 而单字全码表（`single_char_full_codes`）本身是带缓存的，真正的热点只有前者。
    ///
    /// 返回与 `words` **同序等长**；取不到码的位置为空串——调用方靠下标把码配回词，
    /// 跳过失败项会让其后所有词错位配到别人的码上。
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
                |w| match encoder::calc_word_code(w, &spec, |c| codes.get(&c).cloned()) {
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

    /// 确保指定方案引擎已加载；返回是否可用
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

    fn ensure_loaded(&self, schema_id: &str) -> bool {
        if self.is_loaded(schema_id) {
            return true;
        }
        // single-flight：取该方案的专用构建锁（不同方案各自一把，可并行构建）。
        let build_lock = {
            let mut locks = self.build_locks.lock().unwrap_or_else(|e| e.into_inner());
            locks.entry(schema_id.to_string()).or_default().clone()
        };
        let _build_guard = build_lock.lock().unwrap_or_else(|e| e.into_inner());
        // 抢到锁后复查：等待期间可能已被另一线程（预热/切换）构建完成。
        if self.is_loaded(schema_id) {
            return true;
        }
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
        match Self::build_engine(
            schema_id,
            self.data_dir.as_deref(),
            self.store.clone(),
            &codetable_cfg,
            &mix_cfg,
            self.override_dir.as_deref(),
            &pinyin_cfg,
            // 顶层入口：方案自身是拼音时不加约束（简拼开）。混输在其内部为 secondary 注入。
            None,
        ) {
            Some(engine) => {
                info!(
                    "Loaded engine: {} (type={:?})",
                    schema_id,
                    engine.engine_type()
                );
                self.engines
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(schema_id.to_string(), Arc::from(engine));
                true
            }
            None => {
                warn!("Failed to build engine for schema: {}", schema_id);
                false
            }
        }
    }

    /// 取当前活跃引擎（必要时懒加载）
    fn active_engine(&self) -> Option<Arc<dyn Engine>> {
        let id = self.active_schema_id();
        self.ensure_loaded(&id);
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
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
        self.active_engine()
            .map(|e| e.engine_type() == EngineType::Pinyin)
            .unwrap_or(false)
    }

    /// 当前活跃引擎是否为**纯码表**类型（混输 `Mixed` 不算——其拼音半边恒前缀匹配，
    /// 精确匹配语义只对纯码表方案自洽；供协调器判定短语是否随「精确匹配模式」抑制前缀枚举）。
    pub fn is_codetable(&self) -> bool {
        self.active_engine()
            .map(|e| e.engine_type() == EngineType::CodeTable)
            .unwrap_or(false)
    }

    /// 活跃引擎的最大编码长度（码表返回其码长；拼音/无意义引擎返回 0）。
    /// 供协调器判定短语前缀补全是否"未满码"（镜像码表引擎 single_code_complete 的 `input < max` 条件）。
    pub fn active_max_code_length(&self) -> usize {
        self.active_engine()
            .map(|e| e.max_code_length())
            .unwrap_or(0)
    }

    /// 活跃引擎的码元字符集（`[engine.codetable].input_chars` / `.leading_chars`）。
    ///
    /// 未加载引擎、或引擎无「码元」概念（拼音等，`Engine::input_chars` 返回 `None`）时，
    /// 回落内置默认 `a-z`——**绝不回落空集**，那会让该方案一个字也打不出来。
    /// 对协调器而言「无码元集概念」与「默认码元集」行为相同，故此处合并为一个返回值。
    ///
    /// 混输取主码表子引擎的集合（见 `MixedEngine::input_chars`）。
    pub fn active_input_chars(&self) -> wind_config::CodeCharSet {
        self.active_engine()
            .and_then(|e| e.input_chars().cloned())
            .unwrap_or_else(wind_config::CodeCharSet::default_alpha)
    }

    /// 该字符在活跃方案下是否为**码元**（可进输入缓冲）。
    ///
    /// 与 [`Self::active_input_chars`] 的区别：本方法不克隆整张位图。它在按键热路径上
    /// 每键调用，而 `active_engine()` 本身已含一次 `String` 分配，不该再叠一次拷贝。
    pub fn active_is_code_char(&self, ch: char) -> bool {
        match self.active_engine().as_ref().and_then(|e| e.input_chars()) {
            Some(cs) => cs.contains(ch),
            None => wind_config::CodeCharSet::default_contains(ch),
        }
    }

    /// 该字符在活跃方案下是否可作**首码**（缓冲为空时起头）。
    ///
    /// 典型用途：数字是码元（打得出 `Win10`）但不是首码——空缓冲下的数字键仍须是
    /// 选词/透传，否则用户既选不了「第 1 个候选」也拿不回原生数字输入。
    pub fn active_is_leading_char(&self, ch: char) -> bool {
        match self.active_engine().as_ref().and_then(|e| e.input_chars()) {
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

    /// **指定方案**的基础排序是否忽略权重（`base_sort = "natural"`）。
    ///
    /// 与 [`Self::active_base_sort_ignores_weight`] 的区别在于取哪个方案的配置：overlay 类模式
    /// （临时拼音、快捷输入等）跑的是**另一个方案**的引擎，而活跃方案往往是码表——拿五笔的
    /// `base_sort` 去排拼音候选是错的。这类路径必须用本方法按目标方案取。
    pub fn base_sort_ignores_weight_of(&self, schema_id: &str) -> bool {
        if !self.ensure_loaded(schema_id) {
            return false;
        }
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .map(|e| e.base_sort_ignores_weight())
            .unwrap_or(false)
    }

    /// 可用方案列表（快照拷贝）。
    pub fn available_schemas(&self) -> Vec<String> {
        self.available
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 所有已安装且受支持的方案列表（目录扫描），**含隐藏方案**。
    ///
    /// 扫描 `data_dir/schemas/*.schema.toml`，对每个文件取去掉 `.schema.toml` 后缀的 id，
    /// 按 `is_supported()` 过滤掉不支持的方案，再并入当前 `available`（保证已启用方案即使
    /// 文件异常也在列），去重后按 id 字典序稳定排序返回。
    ///
    /// `data_dir` 为 None 时（纯测试）回退到 `available_schemas()`。
    ///
    /// **隐藏方案（`[schema].hidden`）也在返回值里**——那是「设置页列表要不要显示」的
    /// 呈现层判断，不属于「装了哪些方案」。此处曾把它过滤掉，于是另外两个消费者静默
    /// 拿到了子集：`rebuild_all_caches` 漏掉隐藏方案的缓存不失效，`delete_package` 的
    /// keep 列表漏掉它们、删方案时可能连带删掉隐藏方案还在引用的共享资源。
    /// 需要按 hidden 过滤的地方自己调 [`Self::schema_is_hidden`]。
    ///
    /// **不影响** `available_schemas()`（循环切换用，只认用户启用列表）。
    pub fn installed_schemas(&self) -> Vec<String> {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return self.available_schemas();
        };
        let mut ids: Vec<String> = self.available_schemas();
        let ov = self.override_dir.as_deref();

        // 合并扫描：安装目录 data/schemas 与用户目录 %APPDATA%/…/schemas，
        // 两处的 *.schema.toml 都算"已安装"——用户目录可新增第三方方案（read_schema
        // 走 resolve_schema_file，本就用户目录优先，故用户方案能被读出并通过过滤）。
        // 注：此处扫描顺序无关紧要（靠 !ids.contains 去重，两目录都贡献 id）；与
        // shuangpin_layouts 的"用户优先覆盖"语义不同——那里靠前目录同名 stem 胜出。
        let mut scan_dirs: Vec<std::path::PathBuf> = vec![data_dir.join("schemas")];
        if let Some(user) = Config::user_config_dir() {
            let ud = user.join("schemas");
            if ud != scan_dirs[0] {
                scan_dirs.push(ud);
            }
        }

        for dir in &scan_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let fname_str = fname.to_string_lossy();
                if let Some(id) = fname_str.strip_suffix(".schema.toml") {
                    let id = id.to_string();
                    // 只按「受支持」过滤；隐藏与否留给调用方（见本函数文档）。
                    if !ids.contains(&id) && Self::schema_supported(&id, Some(data_dir), ov) {
                        ids.push(id);
                    }
                }
            }
        }

        ids.sort();
        ids.dedup();
        ids
    }

    /// **overlay 方案注册表**：所有带 `[overlay]` 段的已安装方案，按 id 字典序。
    ///
    /// 这是「有哪些特殊模式」的**唯一真相源**，取代了原先的 `config.schema.special_modes`
    /// 数组。`ModeKind::Special(u8)` / `State.special_id` 的下标即本表下标。
    ///
    /// ★ **枚举源是 [`Self::installed_schemas`] 而不是 `available`**：overlay 方案
    /// `hidden = true`、不进 `schema.available`（不参与循环切换），只由 overlay 触发懒加载。
    /// 用 `available` 会得到一张恒空的表。⚠️ `all_key_action_keys` 至今仍只遍历
    /// `available`——将来若要收 overlay 方案自己的 `[key_actions]`，那里也得换源。
    ///
    /// ★ 走静态 `read_schema`（含 `schema_overrides/{id}.toml` 的 `merge_toml` 深合并），
    /// 故用户在设置页对 `[overlay]` 的覆盖**自动生效**，无需额外接线；方案也不必被加载。
    ///
    /// ★ **下标稳定性**：按 id 排序意味着安装一个新 overlay 方案会改变其后方案的下标。
    /// 这不比原先的 config 数组序差（热重载删条目同样错位，`layout.rs` 已在处理越界回落），
    /// 且 overlay 是瞬态的（上屏即退出）。整表随 [`Self::invalidate_schema`] 失效。
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
                // 与 `schema_icon_label` 走**同一个函数**而不只是"同口径"：这两处从前各写
                // 了一份 `.chars().next()`，靠注释声明彼此一致，编译器不管——改一处漏一处的
                // 表现是"方案切换显 `Wb`、进特殊模式显 `符`"，且没有测试会发现。
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
    pub fn overlay_index_of(&self, schema_id: &str) -> Option<u8> {
        self.overlay_modes()
            .iter()
            .position(|e| e.schema_id == schema_id)
            .map(|i| i as u8)
    }

    /// 枚举可选的双拼布局：合并扫描 [用户目录, 安装目录] 的
    /// `schemas/shuangpin/*.toml`，用户目录同名（按文件名 stem）覆盖安装目录。
    ///
    /// 返回 `(id, 显示名)`：**id 取文件名 stem**（与加载路径 `{layout}.toml` 一致，
    /// 保证"能选=能加载"），显示名取布局 `[meta].name`；解析失败（如缺 `[finals]`）
    /// 的布局跳过并告警。供设置页"双拼布局"下拉动态取值，取代前端硬编码清单。
    ///
    /// `data_dir` 为 None（纯测试）时返回空。
    pub fn shuangpin_layouts(&self) -> Vec<(String, String)> {
        let Some(data_dir) = self.data_dir.as_deref() else {
            return Vec::new();
        };
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        if let Some(user) = Config::user_config_dir() {
            dirs.push(user.join("schemas").join("shuangpin"));
        }
        dirs.push(data_dir.join("schemas").join("shuangpin"));
        Self::scan_shuangpin_layouts(&dirs)
    }

    /// 纯扫描逻辑（可测）：按 `dirs` 顺序扫描 `*.toml`，靠前目录优先，
    /// 以文件名 stem 去重；输出按 id 字典序稳定排序。
    fn scan_shuangpin_layouts(dirs: &[std::path::PathBuf]) -> Vec<(String, String)> {
        // 值是胜出文件的路径，仅供被遮蔽方打覆盖日志时引用（见下）。
        let mut seen: std::collections::HashMap<String, std::path::PathBuf> =
            std::collections::HashMap::new();
        let mut out: Vec<(String, String)> = Vec::new();
        for dir in dirs {
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
                if let Some(winner) = seen.get(&id) {
                    // 靠前目录（用户）已收录：本条是被遮蔽的安装目录同名布局。打点必须放在
                    // **被遮蔽方**——只有扫到这里才确证「两侧都有同名」，命中那一刻还不知道
                    // 安装目录有没有；但日志里给的路径要是**胜出**的那份，故记住胜出路径。
                    Config::log_user_override(
                        "shuangpin",
                        &format!("shuangpin/{id}.toml"),
                        winner,
                        true,
                    );
                    continue;
                }
                seen.insert(id.clone(), entry.path());
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
        if let Some(n) = self
            .name_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
        {
            return n.clone();
        }
        let name = Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
        .map(|s| s.schema.name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| schema_id.to_string());
        self.name_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(schema_id.to_string(), name.clone());
        name
    }

    /// 指定方案的图标短称（`schema.icon_label` 截断到 [`wind_config::ICON_LABEL_MAX_WIDTH`]）；
    /// 未配置返回空串。用于状态气泡/工具栏 short 模式（对齐 Go GetSchemaDisplayInfo 的 iconLabel）。
    ///
    /// 截断口径见 [`wind_config::icon_label_trunc`]——上限由渲染宽度与 C++ 侧缓冲共同决定，
    /// 与"是不是方案"无关，故与非中文态标签（`[ui.labels]`）共用同一个函数。
    pub fn schema_icon_label(&self, schema_id: &str) -> String {
        Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
        .map(|s| wind_config::icon_label_trunc(&s.schema.icon_label))
        .unwrap_or_default()
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
        Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
        .map(|s| s.engine.mixed.primary_schema)
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
        Self::read_schema(
            schema_id,
            self.data_dir.as_deref(),
            self.override_dir.as_deref(),
        )
    }

    /// 读取某方案 override 层（TOML 值，无则 None）。
    pub fn get_schema_override(&self, schema_id: &str) -> Option<toml::Value> {
        let dir = self.override_dir.as_deref()?;
        Self::read_override_value(schema_id, dir)
    }

    /// 仅写入某方案 override 层（原子 tmp+rename），**不**使引擎缓存失效。
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

    /// 运行时启停某方案的扩展词库：对**已加载引擎**即时翻对应系统层的 enabled 标志
    /// （无需重建/重熔大词库）；未加载的方案此处不做事（下次构建按已持久化的 override 生效）。
    /// 启用集变化会影响反查索引/编码提示（基于启用词库合并），故一并失效之使下次重算。
    /// 返回是否对已加载引擎即时生效。**注意**：调用方须先 [`persist_schema_override`] 持久化，
    /// 否则重启/重建后状态丢失。
    pub fn set_dict_enabled_live(&self, schema_id: &str, dict_id: &str, enabled: bool) -> bool {
        let engine = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned();
        let hit = engine.is_some_and(|e| e.set_dict_enabled(dict_id, enabled));
        // 反查索引依赖「启用词库合并」，启用集变了须失效（懒重建）。
        // 注：编码提示开关已改读全局 config.pinyin.show_code_hint，无方案级缓存需失效。
        self.reverse_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // 单字全码表同源于「启用词库合并」，与反查索引同生命周期，一并失效。
        *self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        hit
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
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(schema_id);
        self.freq_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(schema_id);
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
        // overlay 注册表是**集合**：装/删方案或改 [overlay] 段都会改变它的内容与下标，
        // 按 id 局部失效没有意义，整表置 None 由下次 overlay_modes() 重建。
        *self.overlay_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // 主码表(及其词库/override)可能变更:失效反查索引,下次按新内容重建。
        self.reverse_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // 单字全码表同源于「启用词库合并」，与反查索引同生命周期，一并失效。
        *self
            .single_char_codes
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // 双拼布局可能变更：失效韵母键缓存，下次按新布局重建。
        *self
            .shuangpin_finals_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (String::new(), None);
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

    /// 切换到指定方案；成功返回 true（必要时懒加载）
    /// 活跃方案变更的统一收尾：记一代 + 通知上层。**所有改 `active` 的地方都要调它**。
    ///
    /// 必须在**释放 active 锁之后**调用——回调是上层代码（RPC 广播），不应在持锁期间执行。
    fn on_active_changed(&self, id: &str) {
        self.schema_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::active_hook::notify_active_changed(id);
    }

    /// 活跃方案的变更代际，见 [`Self::schema_generation`] 字段说明。
    ///
    /// 用法是「记下当时的值，之后比对是否仍相等」，**不要**对差值或绝对值做判断。
    pub fn schema_generation(&self) -> u64 {
        self.schema_generation
            .load(std::sync::atomic::Ordering::Relaxed)
    }

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

    /// 按新配置热重载方案集（无需重建 EngineManager）：重算可用方案、更新上屏策略、
    /// 清空引擎/词频/名称缓存使其按新配置/词典重建，并切到新的活跃方案。
    /// 返回活跃方案是否发生变化（供上层决定是否清输入缓冲、刷新 UI）。
    pub fn reload_from_config(&self, config: &Config) -> bool {
        let new_active = config.active_schema().to_string();
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
        *self
            .primary_pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = config.schema.primary_pinyin.clone();
        // 全局拼音配置变更：更新缓存，引擎缓存随下方 clear() 一起失效，下次按新配置重建。
        *self.pinyin.lock().unwrap_or_else(|e| e.into_inner()) = config.schema.pinyin.clone();
        // 丢弃缓存：引擎按新上屏策略/词典重建，名称/词频按新方案重读。
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.freq_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.schema_type_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.name_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // 双拼布局可能变更：失效韵母键缓存，下次按新布局重建。
        *self
            .shuangpin_finals_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (String::new(), None);

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
        // 判据是「有没有别的方案可去」，不是「列表里有几个」。
        //
        // 原写法 `n <= 1 → None` 隐含假设「当前方案一定在 available 里」——那时 n == 1
        // 确实意味着无处可去。直达热键能切到**未启用**方案之后（如英文），n == 1 反而
        // 常常意味着「有一个地方可以回」，照旧返回 None 会让用户卡在英文方案里出不来。
        if !available.iter().any(|s| s != &current) {
            return None;
        }
        // 起点：当前方案的下一个；**当前方案不在 available 时从头开始**。
        //
        // 原写法是 `position(..).unwrap_or(0)` 再从 +1 起步，那样「不在列表」会被当成
        // 「在第 0 个」，于是恰好跳过 available[0]。以前 active 恒在 available 里、这条
        // 兜底几乎不触发；直达热键能切到未启用方案之后（如英文），它就成了常规路径。
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

    /// 转换输入为候选（分发到当前引擎）
    pub fn convert(&self, input: &str, max_candidates: usize) -> ConvertResult {
        match self.active_engine() {
            Some(engine) => engine.convert(input, max_candidates).unwrap_or_else(|e| {
                warn!("convert error: {}", e);
                ConvertResult::default()
            }),
            None => ConvertResult::default(),
        }
    }

    /// 当前活跃引擎类型（必要时懒加载）
    pub fn current_engine_type(&self) -> Option<EngineType> {
        self.active_engine().map(|e| e.engine_type())
    }

    /// 指定方案**已加载引擎**的类型（内存查，无 IO）。未加载返回 None。
    ///
    /// 与 [`schema_engine_type`](Self::schema_engine_type) 的区别是后者每次都
    /// **读文件 + 解析 TOML**，不可放进逐键路径；本函数只查已加载引擎表。
    /// 供 overlay 类模式（临拼等）按目标方案类型分流取数策略。
    pub fn loaded_engine_type(&self, schema_id: &str) -> Option<EngineType> {
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .map(|e| e.engine_type())
    }

    /// 确保指定方案可加载（懒加载）。用于 overlay 模式（特殊模式等）激活前的可用性校验。
    pub fn ensure_schema(&self, schema_id: &str) -> bool {
        self.ensure_loaded(schema_id)
    }

    /// 顶码上屏：超过满码长时取前 N 码首选上屏，返回 (上屏文本, 剩余编码)。
    /// 仅码表/混输引擎按 top_code_commit 实现，其余返回 None。
    pub fn handle_top_code(&self, input: &str) -> Option<(String, String)> {
        self.active_engine()?.handle_top_code(input)
    }

    /// 满码自动上屏「显示态」复评（透传到活跃引擎）：据已过滤/重排/shadow 的显示候选复评，
    /// 引擎按未过滤候选因生僻同码字判不唯一而否决时，智能过滤后剩唯一精确全码则放行上屏。
    pub fn recheck_auto_commit(
        &self,
        input: &str,
        candidates: &[wind_candidate::Candidate],
    ) -> Option<String> {
        self.active_engine()?.recheck_auto_commit(input, candidates)
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
    /// 方案适用范围**仅码表/混输**：拼音方案本身就在打拼音，再叠一层无意义，且会吞掉引导符
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
    /// **所有临时拼音进入点的公共门卫**（引导键 / 字母触发 / 直达热键 / 顶屏进模式 /
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

    /// 活跃方案的**有效**码表行为配置：全局 `schema.codetable` 经该方案 `.schema.toml` 的
    /// `[engine.codetable]`（内联 + `schema_overrides` 合并后）行为字段折叠。供 coordinator 读
    /// punct_commit / z_key_repeat 等行为字段（取代旧的直接读 schema 字段）。
    /// 当前方案的**按键功能表**（`[key_actions]`，方案文件内联 + `schema_overrides` 已合并）。
    ///
    /// 与 [`Self::codetable_settings`] 不同，**混输方案不下钻到 primary_schema**：按键功能是
    /// 「用户在这个方案里按这个键想干什么」，属于方案自身的交互属性，不像码表行为那样是
    /// 「这张码表怎么工作」。混输方案想配就在自己的文件里配。
    ///
    /// 读不到方案（文件缺失/解析失败）时返回空表 = 不覆盖任何键，各键照常走全局链。
    /// 返回 `Arc`：本表在按键热路径上被查，`Arc::clone` 只加一次引用计数，而返回 owned
    /// 表要复制整张表连同每个 `String`。调用方按 `&*` / `.iter()` 用即可。
    pub fn active_key_actions(&self) -> Arc<std::collections::BTreeMap<String, String>> {
        let id = self.active_schema_id();
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

    /// 当前方案的**会话态按键表**（`[session_actions]`，方案文件内联 + `schema_overrides`
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

    /// 当前方案的**行为覆盖三段**（`[punct]` / `[candidate]` / `[phrases]`，方案文件内联 +
    /// `schema_overrides` 已合并）。
    ///
    /// 与 [`Self::active_key_actions`] 的取表规则逐条相同：
    /// - **混输方案不下钻 `primary_schema`**——这三段都是「用户在这个方案里想要什么」，
    ///   属于方案自身的交互属性，不像码表行为那样是「这张码表怎么工作」。混输方案想配
    ///   就在自己文件里配；
    /// - 读不到方案（文件缺失 / 解析失败）时返回 default = 一段都不覆盖；
    /// - 返回 `Arc`：`[phrases]` 含两个 `Vec<String>`，而消费点在候选构建热路径上。
    ///
    /// 见 `docs/design/schema-scoped-behavior.md` §6.5。
    pub fn active_behavior(&self) -> Arc<wind_config::SchemaBehavior> {
        self.behavior_for(&self.active_schema_id())
    }

    /// 指定方案的行为覆盖三段。
    ///
    /// 独立于 [`Self::active_behavior`] 暴露，是因为短语作用域的归属方案**不一定是
    /// active**——临时英文归 `english` 桶而主方案常是五笔/混输（见
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

    /// **所有**可用方案的 `[key_actions]` 里出现过的键名（并集，去重）。
    ///
    /// 用途只有一个：算出「需要让 TSF 转发 keyup 的修饰键」。转发集必须是并集而非
    /// 活跃方案的那一份——`CompiledHotkeys` 在启动时编译一次并随 activation 推给 C++，
    /// 若按活跃方案裁剪，切方案后 C++ 手里就是旧表，修饰键要等下次焦点切换才生效
    /// （表现为「刚切完方案这个键不灵，点一下别的窗口又灵了」）。
    ///
    /// 取并集的代价是方案 A 配的 `rshift` 在方案 B 里也被转发。无害：keydown 侧纯修饰键
    /// 一律放行不吃（`KeyEventSink.cpp::_IsPureModifierKey`），到了服务端
    /// `bound_action_for` 按**活跃**方案查表落空即不动作。
    ///
    /// 指定方案的码元字符集（按 id，非活跃方案也能查）。
    ///
    /// 与引擎构建走**同一个构造器**，不另写一份解析：区间语法（`a-x/`）、`leading_chars`
    /// 为空时等于全集、非法时回落全集——这些规则只该有一处。
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

    /// 不走 `key_actions_cache`：该缓存按活跃方案 id 存单份，而这里要的是跨方案的并集。
    pub fn all_key_action_keys(&self) -> std::collections::BTreeSet<String> {
        self.all_action_keys().0
    }

    /// **所有**可用方案的 `[session_actions]` 里出现过的键名（并集，去重）。
    ///
    /// 与 [`Self::all_key_action_keys`] 同一个用途与同一条理由：转发集必须是并集而非活跃
    /// 方案那一份，否则切方案后 C++ 手里是旧表，要等下次焦点切换才生效。
    ///
    /// ⚠️ **本表的并集代价与 key_actions 那张不同**：那张最终只取纯修饰键
    /// （`schema_bound_modifier_vks` 的 `filter_map`），keyup 侧多转发无害；而本表支持
    /// 减号、方括号、分号等**可打印符号键**，它们带 `FORWARD_ONLY` 进 keydown 表。
    /// 无会话时必须放行给下游按标点处理，否则表现是**丢键**，且只在没绑该键的方案下复现。
    pub fn all_session_action_keys(&self) -> std::collections::BTreeSet<String> {
        self.all_action_keys().1
    }

    /// 两张表的键名并集，一次遍历同时收齐。
    ///
    /// 合并而不是各写一遍：两者都要 `read_schema` 扫全部可用方案（含 override 深合并），
    /// 分开调用就是把同一批文件读两遍。调用点都在配置生效期（构造 / 热重载），不是热路径，
    /// 但没有理由白读一遍。
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
                // ⚠️ 会话态侧**滤掉显式 `none`**，`key_actions` 侧不滤：两者的消费者对
                // 「被禁用的键」敏感度不同。本集合要据此决定装不装 CapsLock 全局钩子——
                // 方案写了 `capslock = "none"` 却照装，就是白担一个全局钩子的风险。
                // 而 key_actions 那份最终只取纯修饰键进 keyup 转发集，多转发一个不动作的
                // 键宿主无感，滤它反而要多解析一遍动词。
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

    pub fn codetable_settings(&self) -> wind_config::CodetableGlobal {
        let id = self.active_schema_id();
        let global = self
            .codetable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // 混输方案自身无独立 codetable 配置，override 从其 primary_schema（主码表方案）读取
        let resolve_id = if matches!(self.current_engine_type(), Some(EngineType::Mixed)) {
            Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref())
                .map(|s| s.engine.mixed.primary_schema)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone())
        } else {
            id
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

    /// **当前方案**的词频衰减参数：英文方案取英文段，其余取码表段。
    ///
    /// 拼音走 [`Self::pinyin_freq_profile`]，不经这里（消费点按引擎类型分派）。
    ///
    /// 存在的理由是消费端只有一个调用点，却要服务两套配置：调用方拿不到「当前是哪类
    /// 方案」以外的信息，把选择留在那里就会变成消费点各判一次、迟早漏一处。
    pub fn active_freq_profile(&self) -> wind_store::freq::FreqProfile {
        self.freq_profile_for(&self.active_schema_id())
    }

    /// **指定方案**的词频衰减参数。与 [`Self::freq_settings_for`] 同一套分流，
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

    /// 码表/混输的词频衰减参数。**与拼音完全独立，不读拼音段任何字段。**
    ///
    /// 曾做成「码表段为 0 时回落拼音段」，已否决：那让两套配置藕断丝连——用户在设置页把
    /// 码表半衰期留在 0，改拼音的却发现码表跟着变，而设置页上那是两个独立控件。**一个控件
    /// 一个值**，回落链只在配置层不可见时才是便利，一旦两端都有 GUI 就变成了陷阱。
    ///
    /// `base_scale`/`recency_peak` 取 store 默认而非拼音配置值：它们是**打分模型**的系数，
    /// 位置提升模型不读（见 `FreqProfile::pinyin_score` 的死链说明），取哪个值都一样，
    /// 取默认才符合「不读拼音段」这条界线。
    ///
    /// ⚠️ 仅 `strategy = "position"` 时被消费；`top`/`step` 直接比 `count`/`last_used`。
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

    /// 某方案**当前生效**的码表配置（基线 + 方案覆盖折叠后的全实值）。
    ///
    /// 存在的理由是设置页需要一个「起点快照」：方案文件里的行为字段是 `Option`，
    /// 未设置时序列化成 `null`，UI 拿到 null 无法显示成开关——它得知道「不设置的话
    /// 实际是什么」。基线本身又分两种（普通方案跟随全局、特殊方案跟随内置默认，见
    /// [`Self::codetable_baseline`]），UI 侧算不出来，只能由此处给。
    ///
    /// 与 `resolve_codetable` 的区别只是取全局镜像的方式：那个是关联函数（供 `build_engine`
    /// 在持有配置副本时调用），这个从 `self` 取当前镜像。
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

    /// 某方案**不含 override 层**的码表配置：全局基线 ⊕ 方案文件内联，折叠后的全实值。
    ///
    /// 这是设置页三态控件的「跟随值」——用户把某一项的覆盖取消掉之后，它会回到这个值。
    /// 与 [`Self::effective_codetable`] 只差一个 `override_dir`：那个含用户覆盖（＝当前
    /// 实际在跑的值），这个不含（＝取消覆盖后会变成的值）。两者都要给设置页，因为
    /// **已覆盖的项**在两份里取值不同，而 UI 要同时显示「现在是什么」和「取消后是什么」。
    ///
    /// ⚠️ 别想着让 UI 拿 `effective` 减去 override 自己算：折叠的基线分普通/特殊两种
    /// （见 [`Self::codetable_baseline`]），UI 侧算不出来——这正是 `effective_codetable`
    /// 当初存在的理由，同一条在这里再次成立。
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

    /// 折叠方案行为时用哪份基线：普通方案用全局 `schema.codetable`，**overlay 方案用内置默认值**。
    ///
    /// overlay 方案（快符、生僻字表这类）与主码表的性质完全不同——它们是几十条的小符号表，
    /// 而全局基线是按五笔这种数万条的全码表调的。继承的后果是用户改五笔的「精确匹配」，
    /// 快符跟着变，且改的人根本意识不到自己动了另一个表。
    ///
    /// ★ **判据是 `[overlay]` 段存在，不是 `[schema].hidden`**。两者正交，回答的是不同问题：
    /// `hidden` 管「列不列进方案切换列表」，与该折叠哪份基线无关；上面那条理由讲的是
    /// 「它被**叠加使用**、是张小符号表」——那正是 `[overlay]` 声明的事。
    ///
    /// ⚠️ 曾用 `hidden`，理由是「是否被 `special_modes` 引用在这一层拿不到」。该理由随
    /// `special_modes` 数组解散而失效：`[overlay]` 就在 `Schema` 里，本函数已持有它。
    /// 当时另一条理由「英文方案虽然也 hidden」更是早已过时——english 自 `8d3351bf`
    /// 「英文改为可切换方案」起就不是 hidden 了，且它在 `build_engine` 里走独立 english
    /// 分支、用 `CommitOptions::default()` 提前 return，根本到不了这里。
    ///
    /// 换判据后，「hidden 但非 overlay 的码表方案」（如只作 mix 成员用的隐藏小码表）
    /// 改为跟随全局——它被当普通候选来源使用，跟随全局本就更合理。
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
    /// **不是 `CodetableGlobal::default()`**——那是结构体零值，大量集成测试以
    /// `Config::default()` 构造并依赖它（顶码/标点上屏都关着），拿它当特殊方案基线会让
    /// 快符默认不顶码、标点不上屏、精确匹配下不补全，而这些恰是用户预期存在的行为。
    /// 两个概念共用一处定义，改哪边都会伤到另一边。
    ///
    /// 取值：与 `data/config.toml` 的出厂值一致，**除 z 的两项**——`z_key_repeat`（重复上屏）
    /// 与 `z_key_action`（借 z 作引导键）对小符号表都没有意义：`z` 在那里多半是个正经编码，
    /// 抢走它就等于让该编码永远打不出来。两项都显式写出而非靠 `..Default::default()` 兜底，
    /// 与本函数「与出厂值不同的项逐条列明」的写法一致。
    ///
    /// 「不继承全局」的意义是**用户改了全局之后特殊方案不跟着变**，而不是让它们的出厂
    /// 表现与普通方案不同。
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
    /// 同源（`code_source_schema`）：码表方案只用**自己的**拆字配置——没配置就没有拆字，
    /// 不回落主码表（看其它方案的拆字对本方案无意义）；拼音回落全局主码表、混输取其
    /// 主码表成员。无配置返回 None。路径相对 `schemas/`（用户目录优先）。
    pub fn chaizi_spec(&self) -> Option<wind_config::schema::ChaiziSpec> {
        let id = self.code_source_schema();
        if id.is_empty() {
            return None;
        }
        let schema =
            Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref())?;
        let c = schema.engine.chaizi;
        c.is_configured().then_some(c)
    }

    /// 活跃方案的辅助码生效设置：全局基线 `[schema.pinyin.aux_code]` 折叠方案段
    /// `[engine.aux_code]`（含 `schema_overrides` 深合并后的结果）。
    ///
    /// **一次 `read_schema` 出全部三个值**。此前 `files` 与 `max_phrase_len` 是两个各自
    /// `read_schema` 的函数，进入辅助码时一次按键要读盘 + 解析 TOML 四遍（base + override
    /// 各两轮），而这发生在按键线程上。
    ///
    /// 与拆字不同：辅助码过滤的对象是**拼音候选**，触发时活跃方案即拼音方案本身，
    /// 故取 `active_schema_id()`（而非 `code_source_schema()`——那是码表字形反查的归属）。
    ///
    /// `files` 逐条解析，`schemas/` 基准、用户目录优先——与 `read_schema` 同源
    /// `self.data_dir`，故方案从哪读、码表也从哪解析；缺失逐条 warn，不中断其余文件。
    pub fn aux_code_settings(&self) -> AuxCodeSettings {
        let global = self
            .pinyin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .aux_code
            .clone();
        let id = self.active_schema_id();
        let spec = (!id.is_empty())
            .then(|| Self::read_schema(&id, self.data_dir.as_deref(), self.override_dir.as_deref()))
            .flatten()
            .map(|s| s.engine.aux_code);
        let resolved = global.resolved(spec.as_ref());
        // 关闭时不解析路径：省掉 N 次目录探测，也避免为一个用不上的功能刷 warn。
        let files = if resolved.enabled {
            spec.map(|c| {
                c.files
                    .iter()
                    .filter_map(|rel| {
                        let p = wind_config::Config::resolve_schema_resource(
                            self.data_dir.as_deref(),
                            rel,
                        );
                        if p.is_none() {
                            tracing::warn!(
                                "辅助码文件不存在（用户/系统 schemas 目录均未找到）: {rel}"
                            );
                        }
                        p
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
            files,
        }
    }

    /// 活跃方案的词频排序设置。等价于 `freq_settings_for(active_schema_id())`。
    pub fn freq_settings(&self) -> FreqSettings {
        self.freq_settings_for(&self.active_schema_id())
    }

    /// **指定方案**的词频排序设置（frequency.md §3/§8）。按引擎类型分：
    /// 英文取 `schema.english.frequency`，拼音取 `schema.pinyin.frequency`，
    /// 其余取**该方案折叠后的**码表调频段（全局基线 + 方案 `[engine.codetable.frequency]`）。
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
                return *s;
            }
        }
        // 英文记账口径**按候选来源生效、不按当前方案**——混输方案里混进来的英文候选
        // 同样读它。故在分流之外先取一次，三个分支共用同一个值；只在英文分支里读的话，
        // 混输下的英文候选会静默回到默认口径。
        let english_code_by_input = {
            let en = self.english.lock().unwrap_or_else(|e| e.into_inner());
            en.frequency.code_scope == "input"
        };
        let engine_type = self.schema_engine_type(&id);
        let settings = match engine_type.as_deref() {
            Some("english") => {
                let en = self.english.lock().unwrap_or_else(|e| e.into_inner());
                FreqSettings {
                    enabled: en.frequency.enabled,
                    strategy: Self::parse_freq_strategy(&en.frequency.strategy),
                    // 英文没有「简码位」：一个 a 后面跟的是几万个词而不是钦定首选，
                    // 套用码表那套按码长分级的首选保护只会锁死前几位不让调频。
                    protect: ProtectPolicy::NONE,
                    promote_prefix: PromotePrefix::parse(&en.frequency.promote_prefix),
                    english_code_by_input,
                }
            }
            Some("pinyin") => {
                let pf = self.pinyin.lock().unwrap_or_else(|e| e.into_inner());
                // 拼音 strategy/protect 字段不参与（仅码表 used-first 排序用），取默认。
                FreqSettings {
                    enabled: pf.frequency.enabled,
                    strategy: FreqStrategy::Step,
                    protect: ProtectPolicy::NONE,
                    promote_prefix: PromotePrefix::parse(&pf.frequency.promote_prefix),
                    english_code_by_input,
                }
            }
            _ => {
                // **按方案折叠后的**调频段，不是全局镜像：基线（普通方案=全局、特殊方案=
                // 内置默认）叠加该方案 `[engine.codetable.frequency]` 的稀疏覆盖。
                // 此前这里直接读 `self.codetable.lock()`，于是方案文件里写了调频也无人读。
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
                    // 之前，无需再按语义单元收窄；且这与 `Top`/`Step` 的历史行为一致（那两者
                    // 对前缀补全从无限制），避免升级后存量用户的调频突然变窄。
                    promote_prefix: PromotePrefix::parse(&ct.frequency.promote_prefix),
                    english_code_by_input,
                    enabled: ct.frequency.enabled,
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
            .insert(id, settings);
        settings
    }

    /// 词频策略字符串 → 枚举（纯映射，便于单测）。
    fn parse_freq_strategy(s: &str) -> FreqStrategy {
        match s {
            "top" => FreqStrategy::Top,
            "position" => FreqStrategy::Position,
            _ => FreqStrategy::Step,
        }
    }

    /// 用指定方案引擎转换（不改变当前活跃方案，必要时懒加载）。
    /// 用于临时拼音：码表模式下临时借用拼音引擎反查。
    pub fn convert_with(
        &self,
        schema_id: &str,
        input: &str,
        max_candidates: usize,
    ) -> ConvertResult {
        if !self.ensure_loaded(schema_id) {
            return ConvertResult::default();
        }
        let engine = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned();
        match engine {
            Some(e) => e.convert(input, max_candidates).unwrap_or_else(|err| {
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
        let engine = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned();
        engine.map(|e| e.enumerate(limit)).unwrap_or_default()
    }

    /// 指定方案「进入即展示」浏览态的**呈现上限**（`Engine::browse_display_limit`）。
    /// 内存查已加载引擎表，无 IO（区别于 `effective_codetable`，那个每次都读 TOML）。
    /// 未加载 / 无浏览语义返回 None = 不限。
    pub fn browse_display_limit_of(&self, schema_id: &str) -> Option<usize> {
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .and_then(|e| e.browse_display_limit())
    }

    /// 反查 `(code, text)` 在该方案词典里的音节边界；方案未加载/非拼音/查不到均返回 0。
    ///
    /// **不做推断**（区别于 `generate_word_pinyin`）：拿现成的码点查取真值。
    /// 供词频列表显示音节格式——词频表本身不带 boundary。
    pub fn syllable_boundary_of(&self, schema_id: &str, code: &str, text: &str) -> u64 {
        if !self.ensure_loaded(schema_id) {
            return 0;
        }
        let engine = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned();
        engine.map_or(0, |e| e.syllable_boundary_of(code, text))
    }

    /// 用指定方案的引擎为词语生成**带空格的全拼音节码**（造词反推、多音字消歧）。
    /// 方案非拼音类、未能加载或无法生成时返回 None（**含非汉字的词必落此路**，逐字兜底
    /// 取不到读音）。调用方可回退 `wind_reverse::ReverseLookup::gen_pinyin`——注意那条路
    /// **同样产出带空格的音节码**（每字一音节），故落库前一样要 `split_spaced_code` 拆成
    /// 扁平 key，不可当扁平码直传（曾因此把 `"ni hao"` 写成 key，词彻底打不出来）。
    pub fn generate_word_pinyin(&self, schema_id: &str, text: &str) -> Option<String> {
        if !self.ensure_loaded(schema_id) {
            return None;
        }
        let engine = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned()?;
        engine.generate_word_pinyin(text)
    }

    /// 批量求解 `(code, text)` 的音节边界，兼作拼音词条合法性判据（导入闸口用）。
    ///
    /// 引擎句柄只取一次——导入动辄上万行，逐行 `ensure_loaded` + 加锁不可接受
    /// （同 [`Self::generate_words_pinyin`] 的理由）。返回与 `pairs` **同序等长**。
    ///
    /// ⚠️ 方案未加载 / 非拼音方案 → 整批 [`BoundaryResolution::NoInfo`]，即
    /// **「合法但无边界」而非「非法」**。码表词库导入正是走这条路，判成非法会全军覆没。
    pub fn resolve_boundaries(
        &self,
        schema_id: &str,
        pairs: &[(&str, &str)],
    ) -> Vec<BoundaryResolution> {
        if !self.ensure_loaded(schema_id) {
            return vec![BoundaryResolution::NoInfo; pairs.len()];
        }
        let engine = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned();
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
    /// 返回与 `texts` **同序等长**；生成不出的位置为 None，由调用方决定回退还是留空。
    pub fn generate_words_pinyin(&self, schema_id: &str, texts: &[&str]) -> Vec<Option<String>> {
        if !self.ensure_loaded(schema_id) {
            return vec![None; texts.len()];
        }
        let engine = self
            .engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(schema_id)
            .cloned();
        let Some(engine) = engine else {
            return vec![None; texts.len()];
        };
        texts
            .iter()
            .map(|t| engine.generate_word_pinyin(t))
            .collect()
    }

    // ───────────────────────── 词典加载 ─────────────────────────

    /// 在 [用户配置/schemas, 安装/schemas] 中解析一个 schemas 相对文件路径，用户目录优先。
    /// 用户目录存在同名文件即覆盖安装目录（schema 用户覆盖；方案/词典/字根表共用）。
    fn resolve_schema_file(rel: &str, data_dir: &Path) -> std::path::PathBuf {
        if let Some(user) = Config::user_config_dir() {
            let p = user.join("schemas").join(rel);
            if p.is_file() {
                Config::log_user_override(
                    "schema",
                    rel,
                    &p,
                    data_dir.join("schemas").join(rel).is_file(),
                );
                return p;
            }
        }
        data_dir.join("schemas").join(rel)
    }

    /// 读取并解析 schema 文件（仅 TOML）。用户目录优先（见 resolve_schema_file）；
    /// 若 `override_dir/{id}.toml` 存在则深合并到基础方案之上（设置页 override 层 L3）。
    fn read_schema(
        schema_id: &str,
        data_dir: Option<&Path>,
        override_dir: Option<&Path>,
    ) -> Option<Schema> {
        let data_dir = data_dir?;
        let toml_path = Self::resolve_schema_file(&format!("{}.schema.toml", schema_id), data_dir);
        if !toml_path.exists() {
            warn!("Schema file not found: {}.schema.toml", schema_id);
            return None;
        }
        let content = std::fs::read_to_string(&toml_path).ok()?;
        let mut base: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                warn!("Parse schema TOML failed {}: {}", toml_path.display(), e);
                return None;
            }
        };
        // 合并 override 层（存在才读；不存在则零影响）。
        if let Some(ov) = override_dir.and_then(|d| Self::read_override_value(schema_id, d)) {
            merge_toml(&mut base, ov);
        }
        match base.try_into() {
            Ok(s) => Some(s),
            Err(e) => {
                warn!("Schema {} override 合并后解析失败: {}", schema_id, e);
                None
            }
        }
    }

    /// 读取某方案的 override TOML 值（无则 None）。
    fn read_override_value(schema_id: &str, override_dir: &Path) -> Option<toml::Value> {
        let path = override_dir.join(format!("{schema_id}.toml"));
        let content = std::fs::read_to_string(&path).ok()?;
        toml::from_str(&content).ok()
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
        mixed_role: Option<MixedRole>,
    ) -> Option<Box<dyn Engine>> {
        let data_dir = data_dir?;
        let schemas = data_dir.join("schemas");
        let schema = Self::read_schema(schema_id, Some(data_dir), override_dir)?;

        // 混输方案：递归构建主（码表）+ 次（拼音）子引擎，包装为 MixedEngine
        if schema.engine.engine_type.to_lowercase() == "mixed" {
            let m = &schema.engine.mixed;
            if m.primary_schema.is_empty() {
                warn!("mixed schema {} 缺少 primary_schema", schema_id);
                return None;
            }
            let primary = Self::build_engine(
                &m.primary_schema,
                Some(data_dir),
                store.clone(),
                codetable_cfg,
                mix_cfg,
                override_dir,
                pinyin_cfg,
                Some(MixedRole::Primary {
                    // 取**混输方案自己**声明的值，不继承 primary_schema 的（见 MixedRole::Primary）。
                    sentence_input: schema.engine.codetable.sentence_input,
                }),
            )?;
            // 「声明了整句、却配着拼音子引擎」是个不会生效的组合：超码长区间归拼音
            // （`MixedEngine::convert` 直接走 `convert_overflow`，不经主引擎），而整句的
            // 门槛正是超码长。明说一句，别让人对着一个没反应的配置项干瞪眼。
            if schema.engine.codetable.sentence_input && !m.secondary_schema.is_empty() {
                warn!(
                    "混输方案 {} 声明了 [engine.codetable] sentence_input，但它配有拼音子引擎                      {}：超码长输入由拼音接管，码表整句不会触发。混输下的整句尚未接线。",
                    schema_id, m.secondary_schema
                );
            }
            // secondary（拼音）是**唯一**注入 [`MixPinyinOpts`] 的地方：这些收敛只约束
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
                    Some(MixedRole::Secondary(MixPinyinOpts {
                        abbrev: mix_cfg.enable_pinyin_abbrev,
                    })),
                )
            };
            // 融合策略走全局 schema.mix（无方案级 override）。
            let min_py = if mix_cfg.min_pinyin_length > 0 {
                mix_cfg.min_pinyin_length
            } else {
                2
            };
            let block_on_pinyin = mix_cfg.auto_commit_block_on_pinyin;
            // 英文候选（schema.mix.enable_english）：开启时懒加载 english 词库引擎混入混输候选。
            // 走 build_engine("english") → EnglishEngine（词库缺失则 None，静默退化为无英文）。
            // 开关热切换经 reload_from_config 的 engines.clear() 重建混输引擎自然生效。
            let english = if mix_cfg.enable_english {
                Self::build_engine(
                    "english",
                    Some(data_dir),
                    store.clone(),
                    codetable_cfg,
                    mix_cfg,
                    override_dir,
                    pinyin_cfg,
                    None,
                )
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
            // 用户词层：英文用户词（专有名词、缩写、项目内部词汇——词库里不会有的那些）。
            // 归属 `schema_id`（即 "english"），与 `data_schema_id` 对英文的返回值一致；
            // 读写不同源的话，加进去的词永远查不出来。
            //
            // **刻意不挂临时词层**：临时词库是「自动造词的暂存区」，条目由连续上屏推导而来、
            // 攒够次数才晋升用户词库。英文没有造词流程，挂上去只会是一张永远空的表，
            // 却让每次查询多走一层。
            //
            // 注册顺序 User → System 与码表分支一致（用户词在前）。
            if let Some(store) = &store {
                dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
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
            return Some(Box::new(crate::english::EnglishEngine::new(
                CodeTableEngine::new(mcl, commit_opts, Arc::new(dm)),
            )));
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
            // （unigram 语言模型的加载已移除：它是 cn_dicts 的一份副本，词图打分改用词条
            // 自身的词典权重，见 `pinyin::lattice::score_node_inner`。配置键与 unigram.txt/
            // wdb 产物已一并清理；老用户机器上残留的 wdb 由缓存目录清理逻辑扫走。）
            //
            // 从全局拼音配置构建引擎配置和模糊音（Task 1.4：修 fuzzy 从未生效 bug）。
            // enabled 作总开关：未启用时所有模糊标志归零（与 Go 行为一致）。
            let pg = pinyin_cfg;
            let fuzzy = crate::pinyin::fuzzy::FuzzyConfig {
                zh_z: pg.fuzzy.enabled && pg.fuzzy.zh_z,
                ch_c: pg.fuzzy.enabled && pg.fuzzy.ch_c,
                sh_s: pg.fuzzy.enabled && pg.fuzzy.sh_s,
                n_l: pg.fuzzy.enabled && pg.fuzzy.n_l,
                f_h: pg.fuzzy.enabled && pg.fuzzy.f_h,
                r_l: pg.fuzzy.enabled && pg.fuzzy.r_l,
                an_ang: pg.fuzzy.enabled && pg.fuzzy.an_ang,
                en_eng: pg.fuzzy.enabled && pg.fuzzy.en_eng,
                in_ing: pg.fuzzy.enabled && pg.fuzzy.in_ing,
                ian_iang: pg.fuzzy.enabled && pg.fuzzy.ian_iang,
                uan_uang: pg.fuzzy.enabled && pg.fuzzy.uan_uang,
            };
            let pcfg = PinyinConfig {
                show_code_hint: pg.show_code_hint,
                use_smart_compose: pg.use_smart_compose,
                // 无覆盖（纯拼音方案）时保持历史行为：简拼开。
                enable_abbrev: mix_secondary.map(|o| o.abbrev).unwrap_or(true),
                // 残码整句只在**非混输**下启用，理由见 `PinyinConfig::enable_partial_final`。
                // ⚠️ 判据是「是不是混输辅助」本身，不是 `abbrev` 的取值——两个开关恰好都
                // 「混输时关掉」，但语义正交，串用会在其中一个被单独调整时静默错配。
                enable_partial_final: mix_secondary.is_none(),
                // ⚠️ 补全这两项**不按 `mix_pinyin` 分流**，与上面三项刻意不同：它们约束的是
                // 「引擎敢预测多少你没打的音节」，这个偏好与「当前是不是混输」无关，是用户
                // 对候选面的统一取舍。分流会让同一个设置在两种方案下表现不一致。
                // 守门测试：`mixed_completion_config`。
                completion_min_syllables: pg.completion.min_syllables,
                completion_max_extra_syllables: pg.completion.max_extra_syllables,
                // 全拼降级输入（双拼下多人共用）。**混输强制关闭**，理由同上面的
                // `enable_partial_final`：混输的击键串同时是码表码，再挂一条全拼流是过度
                // 解读。何况混输接双拼这个组合本身就不成立（`MixedEngine::pinyin_may_continue`
                // 的「前提：混输不接双拼」），此处不必也不该为它留口子。
                //
                // 非双拼方案下本项即便为 true 也不生效——引擎侧判据是它与 `shuangpin.is_some()`
                // 取与（见 `PinyinConfig::allow_full_pinyin`）。
                allow_full_pinyin: pg.shuangpin.allow_full_pinyin && mix_secondary.is_none(),
            };
            let mut engine = PinyinEngine::new(pcfg, dict).with_fuzzy(fuzzy.clone());
            // 上下文语言模型：**两个开关都得显式打开才启用**——
            // `weight != 0` 且 `model` 非空。任一为默认值都**根本不读文件**，
            // 既省内存，也让「没配就等于没这功能」在字节层面成立。
            //
            // ★ 为什么模型名也要参与判据：默认模型名一度是 `zh-hans-bgw.gram`，
            // 于是「只把 weight 调成非 0」就会静默启用它——而该模型实测在 192 条
            // 整句评测上是 **−4**（见设计文档 §8）。现在默认空串，用户必须两个字段
            // 都写过一遍才会生效，不存在「不知道自己开了什么」。
            //
            // 模型数据不随安装包分发，缺失时降级为关闭而非报错（见设计文档 §5）。
            if pinyin_cfg.grammar.weight != 0.0 && !pinyin_cfg.grammar.model.trim().is_empty() {
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
            if schema
                .engine
                .pinyin
                .scheme
                .eq_ignore_ascii_case("shuangpin")
            {
                let layout_id = if schema.engine.pinyin.shuangpin.layout.is_empty() {
                    "xiaohe".to_string()
                } else {
                    schema.engine.pinyin.shuangpin.layout.clone()
                };
                // 用户目录优先（见 resolve_schema_file）：用户自带/覆盖布局生效。
                let lp =
                    Self::resolve_schema_file(&format!("shuangpin/{layout_id}.toml"), data_dir);
                match crate::pinyin::shuangpin::Layout::from_toml(&lp) {
                    Ok(layout) => {
                        let mut conv = crate::pinyin::shuangpin::ShuangpinConverter::new(layout);
                        conv.set_fuzzy(fuzzy.zh_z, fuzzy.ch_c, fuzzy.sh_s);
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
                dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
                    store.clone(),
                    PINYIN_DATA_SCHEMA,
                )));
                dm.register_layer(Box::new(wind_dict::StoreTempLayer::new(
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
            // ⚠️ 与 resolve_codetable 是**两个平行的折叠点**，基线判据必须一致——
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
                // 作为混输主引擎构建时**改取混输方案自己的声明**，理由见 `MixedRole::Primary`。
                sentence_input: resolve_sentence_input(
                    mixed_role,
                    schema.engine.codetable.sentence_input,
                ),
            };
            // 码表引擎经 DictManager(CompositeDict) 查询。系统词库不再合并成单个 combined，
            // 而是主库 + 每个扩展（含禁用）各自一个 System 层，查询期由 composite 合并去重。
            // 开关扩展只需翻该层 enabled 标志即时生效，无需重熔大词库。
            let layers = Self::load_codetable_layers(&schema, &schemas);
            if layers.is_empty() {
                warn!("No usable codetable dictionary for schema {}", schema_id);
                return None;
            }
            // 注入 redb Store 时，注册用户词/临时词层（按 schema 隔离），让用户词进候选合并。
            let dm = wind_dict::DictManager::new();
            if let Some(store) = &store {
                dm.register_layer(Box::new(wind_dict::StoreUserLayer::new(
                    store.clone(),
                    schema_id,
                )));
                dm.register_layer(Box::new(wind_dict::StoreTempLayer::new(
                    store.clone(),
                    schema_id,
                )));
            }
            // 主库优先注册（在 load_codetable_layers 中已置首），扩展库其后。
            // base_order 决定等权/natural 排序的库间档位；default_weight 覆盖无权重库的权重档。
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
            Some(Box::new(Self::attach_sentence_freq(
                CodeTableEngine::new(mcl, commit_opts, Arc::new(dm)).with_charset(charset),
                commit_opts.sentence_input,
                &schemas,
            )))
        }
    }

    /// 码表方案：把主词库 + 每个扩展词库（含**当前禁用**的）各自加载为独立 system 层。
    /// 返回 `(层名, CachedDict, 初始enabled)`：主库 → `codetable-system`(恒启用)；扩展 →
    /// `codetable-extra-<id>`(enabled=is_enabled())。**不再合并 combined.wdat**——查询期由
    /// CompositeDict 合并去重，开关扩展只需翻该层 enabled 标志，无需重熔大词库（对齐 Go 的
    /// 每库独立缓存 + 查询期合并）。主库优先返回（层序最靠前 → 等权重时排前）。
    /// 词库文件路径解析：用户配置/schemas 优先，回退 schemas_dir（与 read_schema 同语义）。
    ///
    /// 同时支持 **wdat-only 词库**——用户可只投放编译好的 `xxx.wdat` 而不带
    /// `xxx.dict.yaml`（对齐 Go 的 wdb-only 分发）。yaml 在两个目录都不存在时，改按同名
    /// wdat 再探一轮**相同顺序**，命中则返回该目录下的 yaml 路径：文件本身不存在，但
    /// `CachedDict::load_at_with` 会据此推导同目录的 wdat 并直接 mmap。
    ///
    /// 为什么必须按 wdat 再探而不能只靠兜底：兜底恒指向安装目录（通常是只读的
    /// Program Files），而用户投放的 wdat 一般在用户目录，只探 yaml 会把路径定位到错误
    /// 的目录上。Go 版为此专门引入了 `wdbOnlyHint` 参数，这里靠单点解析一并解决。
    ///
    /// 本函数取代了此前 `load_codetable_layers` 与 `load_dictionary` 里两份逐字相同的
    /// 闭包——「两处各写一份」正是 R6 那条陈旧路径的成因。
    fn resolve_dict_file(rel: &str, schemas_dir: &Path) -> std::path::PathBuf {
        Self::resolve_dict_file_in(
            rel,
            Config::user_config_dir()
                .map(|u| u.join("schemas"))
                .as_deref(),
            schemas_dir,
        )
    }

    /// [`Self::resolve_dict_file`] 的纯函数内核：用户 schemas 目录显式传入，便于测试
    /// 四级优先级（user yaml → sys yaml → user wdat → sys wdat → 兜底 sys）。
    fn resolve_dict_file_in(
        rel: &str,
        user_schemas: Option<&Path>,
        schemas_dir: &Path,
    ) -> std::path::PathBuf {
        // fn item 而非闭包：闭包会借住 `sys`，与后面几处 `return sys` 的 move 冲突。
        fn has_wdat(p: &Path) -> bool {
            wind_dict::cached::wdat_sibling(p).is_some_and(|w| w.is_file())
        }
        let sys = schemas_dir.join(rel);
        // 1) yaml 按原优先级
        if let Some(u) = user_schemas {
            let p = u.join(rel);
            if p.is_file() {
                // 遮蔽判定含 wdat：安装侧只投放了 wdat（无 yaml）时，用户的 yaml 同样是覆盖。
                Config::log_user_override("dict", rel, &p, sys.is_file() || has_wdat(&sys));
                return p;
            }
        }
        if sys.is_file() {
            return sys;
        }
        // 2) 两处都无 yaml → 按 wdat-only 同序再探
        if let Some(u) = user_schemas {
            let p = u.join(rel);
            if has_wdat(&p) {
                Config::log_user_override("dict", rel, &p, has_wdat(&sys));
                return p;
            }
        }
        if has_wdat(&sys) {
            return sys;
        }
        // 3) 兜底同原行为：返回安装目录路径，由调用方报加载失败
        sys
    }

    fn load_codetable_layers(schema: &Schema, schemas_dir: &Path) -> Vec<CodetableLayer> {
        let resolve =
            |rel: &str| -> std::path::PathBuf { Self::resolve_dict_file(rel, schemas_dir) };
        let is_english =
            |e: &DictSpec| -> bool { !e.dict_type.is_empty() && e.dict_type == "english" };

        let usable: Vec<&DictSpec> = schema
            .dictionaries
            .iter()
            .filter(|d| !d.path.is_empty())
            .collect();
        if usable.is_empty() {
            return Vec::new();
        }
        // 主库 = 首个 default；无 default 则取首个可用库。
        let main_idx = usable.iter().position(|d| d.default).unwrap_or(0);

        let load_one = |e: &DictSpec| -> Option<CachedDict> {
            let full = resolve(&e.path);
            match CachedDict::load_at_with(&full, &cache_path(&full, "wdat"), is_english(e)) {
                Ok(d) => Some(d),
                Err(err) => {
                    warn!("Failed to load codetable dict {}: {}", full.display(), err);
                    None
                }
            }
        };

        let mut out: Vec<CodetableLayer> = Vec::new();
        // 主库优先注册。加载失败 → 无系统层可用，放弃整方案（避免无候选）。
        match load_one(usable[main_idx]) {
            Some(d) => {
                info!(
                    "  codetable main: {} ({} entries)",
                    usable[main_idx].path,
                    d.len()
                );
                out.push(CodetableLayer {
                    name: "codetable-system".to_string(),
                    dict: d,
                    enabled: true,
                    base_order: usable[main_idx].base_order,
                    default_weight: usable[main_idx].default_weight,
                });
            }
            None => return Vec::new(),
        }
        // 扩展库（含禁用的，全部加载常驻，供运行时热插拔）。
        for (i, e) in usable.iter().enumerate() {
            if i == main_idx {
                continue;
            }
            let enabled = e.is_enabled();
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

    /// 方案实际参与构建的词库列表：启用的全部；**一个都没启用时兜底取首个**
    /// （否则整方案无候选，比"少一个扩展库"糟糕得多）。
    ///
    /// 抽成单一函数是刚性要求：[`Self::load_dictionary`]（拼音引擎用）与
    /// [`Self::load_dicts_individually`]（两个索引构建方用）**必须选出同一批库**。
    /// 两处各写一份必然漂移，症状是「候选里有的词，悬停却查不到编码」——错位无声、
    /// 且没有任何测试会失败。
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

    /// 按方案加载**全部启用词库，各自独立**——不合并、不产出中间文件。
    ///
    /// # 为什么两个索引构建方要走这里而不是 [`Self::load_dictionary`]
    ///
    /// 反查索引与单字全码表都只需要「全部条目」，而 `load_dictionary` 的多库分支为此
    /// 先合成一个 `combined.wdat`：feihuzj2 方案上那是 **230MB 文件 + 30 秒构建**，
    /// 而两条路径产出的索引**逐位相同**（等价性论证与测试见
    /// [`wind_dict::cached::build_reverse_index_from`]）。直接逐库读只要 1.85 秒。
    ///
    /// 词库选取规则与 `load_dictionary` 保持一致（启用优先、全禁用时兜底取首个），
    /// **两处必须同源**：不一致会让「候选里有的词，悬停却查不到编码」这类错位出现，
    /// 且极难归因。
    ///
    /// 各库经 [`wind_dict::reader_pool`] 按路径共享 mmap，活跃引擎通常已持有同一批
    /// reader，故这里几乎零成本：不重新解析、不新增映射。
    fn load_dicts_individually(schema: &Schema, schemas_dir: &Path) -> Vec<CachedDict> {
        Self::enabled_dict_specs(schema)
            .iter()
            .filter_map(|e| {
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
                    t => {
                        CachedDict::load_at_with(&full, &cache_path(&full, "wdat"), t == "english")
                            .ok()
                    }
                }
            })
            .collect()
    }

    /// 清理本方案遗留的 `combined.wdat` —— 改用逐库构建之前那个中间产物
    /// （feihuzj2 方案上是 **230MB**，且此后再不会有人读它）。
    ///
    /// 只对**确定不再需要它**的方案动手：非拼音方案的 live 引擎走 `load_codetable_layers`
    /// 的每库独立层，两个索引构建方已改走 [`Self::load_dicts_individually`]，于是
    /// combined 再无消费方。拼音方案的多库分支仍可能需要它（见 [`Self::load_dictionary`]），
    /// 一律不动 —— 判据取 [`Schema::is_pinyin`] 而非自己比字符串，因为它还正确处理了
    /// `engine.type` 缺省、要靠默认词库类型反推的那种方案。
    ///
    /// 删不掉就算了（正被别的进程映射、或无权限）：这只是块浪费的磁盘，不是正确性问题，
    /// 下次再来一遍即可。
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
                let mut fp = combined.clone().into_os_string();
                fp.push(".fp");
                let _ = std::fs::remove_file(std::path::PathBuf::from(fp));
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
    /// ⚠️ **现存唯一的多库消费方是拼音引擎**（`PinyinEngine` 只持有一个 `CachedDict`、
    /// 没有 composite 分层，故多库拼音方案必须拿到合并视图）。出厂 pinyin/shuangpin
    /// 各只声明 1 个词库、走单库快路径，因此**出厂配置下 `combined.wdat` 根本不会产生**；
    /// 只有用户经 `schema_overrides` 给拼音方案加第二个词库才会触发。
    ///
    /// 码表/英文的 live 查询走 `load_codetable_layers` 的每库独立层；两个索引构建方
    /// 已改走 [`Self::load_dicts_individually`]，不再经过这里。
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
                match CachedDict::load_at_with(&full, &cache_path(&full, "wdat"), lowercase) {
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
        // 拿锁后复查
        if Self::combined_cache_fresh(&paths, combined, COMBINED_CACHE_TAG)
            && let Ok(reader) = wind_dict::reader_pool::open_wdat(combined)
        {
            info!(
                "Using combined cache: {} ({} keys)",
                combined.display(),
                reader.key_count()
            );
            return Some(CachedDict::Mmap(reader));
        }

        // 按 code 聚合所有源词库条目（前面的库优先级更高，先加入；同 text 取更高权重）
        let mut agg: HashMap<String, Vec<(String, i32)>> = HashMap::new();
        let mut total = 0usize;
        for (p, dict_type) in sources {
            // rime_pinyin 需经 import_tables 展开，否则只读到主文件元数据
            let loaded = if dict_type == "rime_pinyin" {
                Self::load_rime_pinyin_dict(p)
            } else {
                // 必须用与 load_codetable_layers 相同的缓存路径。此前这里是
                // `CachedDict::load(p)`，它把缓存落在**源文件旁**（yaml.with_extension），
                // 而源在只读的安装目录：写入必然拒绝访问 → 退化成内存模式 → 每次都要重解析
                // 整份 yaml。实测按键触发反查索引时同步卡 452ms（88526 条重新解析），
                // 且这三个库的 wdat 早已在 reader 池里。改用 cache_path 后指纹命中即复用
                // 池中 mmap，零解析。
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
                                slot.1 = weight; // 继承后续库中同词更高权重（对齐 Go composite）
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
            return None;
        }

        let mut writer = wind_dict::datformat::WdatWriter::new();
        for (code, mut entries) in agg {
            entries.sort_by_key(|e| std::cmp::Reverse(e.1));
            writer.add(code, entries);
        }
        match writer.write(combined) {
            Ok(_) => {
                // 写内容指纹(覆盖全部源，与上面 fresh 校验的 paths 一致)
                wind_dict::cache_fp::write_cache_fp(combined, &paths, COMBINED_CACHE_TAG);
                match wind_dict::reader_pool::open_wdat(combined) {
                    Ok(reader) => {
                        info!(
                            "Wrote combined cache: {} ({} keys from {} dicts)",
                            combined.display(),
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

    /// 缓存是否可复用：按源文件**内容指纹**判定（非 mtime）。
    /// scp/部署/版本控制会刷新源 mtime，旧的 mtime 校验会因此恒失效 → 每次重建(300MB)；
    /// 改为内容指纹后，源内容未变即复用，构建后由 write_cache_fp 写指纹 sidecar。
    ///
    /// `paths` 必须覆盖**全部**参与构建的源文件——含 rime_pinyin 的 import_tables 子表，
    /// 否则改子表不会让缓存失效（静默复用陈旧件）。见 [`Self::rime_source_paths`]。
    fn combined_cache_fresh(paths: &[&Path], combined: &Path, tag: &str) -> bool {
        wind_dict::cache_fp::cache_is_fresh(combined, paths, tag)
    }

    /// 某个词库源实际参与构建的**全部**文件：主表本身，外加 `rime_pinyin` 的
    /// `import_tables` 子表（子表不存在则跳过）。非 rime_pinyin 只有主表。
    ///
    /// 抽出来是因为它有两个消费方（merged 与 combined 两层缓存），而此前只有前者展开了
    /// 子表 —— 于是改子表时内层 merged 正确重建、外层 combined 指纹却纹丝不动，
    /// 继续喂陈旧数据给反查索引。**指纹漏掉任一真实输入，就是一条静默陈旧路径。**
    fn rime_source_paths(dict_path: &Path, dict_type: &str) -> Vec<std::path::PathBuf> {
        let mut out = vec![dict_path.to_path_buf()];
        if dict_type != "rime_pinyin" {
            return out;
        }
        // 以下三条降级都会让 import_tables 整批读不到，只剩主表（rime 主表往往只有头部
        // 元数据、正文寥寥）→ 拼音几乎无候选。必须留下痕迹，否则症状是「拼音突然打不出字」
        // 而日志一片干净。
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
        // 已知局限（沿用自改动前）：这里是**朴素子串**搜索，而非 codetable.rs 的
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

    /// 加载 rime_pinyin 词典（合并 import_tables 子词典到 .merged.wdat）
    /// 给开启整句的码表引擎指明**词频来源目录**（见 `codetable::sentence::SentenceFreq`）。
    ///
    /// 词库走约定路径 `pinyin/rime_frost.dict.yaml`（= 内置拼音方案的主词库），而不是去读
    /// 拼音方案的 schema：整句要的只是「一份带真实词频的中文词表」，与用户把拼音方案配成
    /// 什么样无关；读 schema 反而会让码表方案的行为随另一个方案的配置漂移。
    ///
    /// ⚠️ 只交路径、**不在这里同步加载**：构建期加载会让两类用不上它的场景白付代价——
    /// 混输主引擎（整句永远调不到）、以及压根没开整句的方案。加载还可能现场构建
    /// `merged.wdat`（数秒），同步做就直接体现为「切到五笔卡住」。
    ///
    /// 但**懒到按键线程上同样不行**：实测首次解码要 ~660 ms（见 `sentence::LazyTables`），
    /// 恰好落在用户敲下第 5 个码那一刻。故这里在交完路径后立刻 `prewarm_sentence()`，
    /// 把两张表推给后台线程——开了整句才付、且不占按键线程。
    fn attach_sentence_freq(
        engine: CodeTableEngine,
        sentence_on: bool,
        schemas_dir: &Path,
    ) -> CodeTableEngine {
        if !sentence_on {
            return engine;
        }
        let engine = engine.with_sentence_schemas_dir(schemas_dir.to_path_buf());
        engine.prewarm_sentence();
        engine
    }

    /// 加载整句词频用的拼音词库。由 `codetable::sentence` 的懒加载在首次解码时调用。
    pub(crate) fn load_sentence_freq_dict(schemas_dir: &Path) -> Option<CachedDict> {
        let path = schemas_dir.join("pinyin/rime_frost.dict.yaml");
        let d = Self::load_rime_pinyin_dict(&path)?;
        info!("码表整句：已接入拼音词库作为词频来源 {}", path.display());
        Some(d)
    }

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
        // merged.wdat 写到可写缓存目录（与 unigram 一致）。安装目录（如 Program Files）
        // 通常只读，若写在源旁会失败 → 回退仅主词典(rime header 数十条) → 拼音无候选。
        let merged_wdat = cache_path(dict_path, "merged.wdat");
        // 收集全部源（主表 + import_tables 子表）。指纹/缓存校验需覆盖全部源，
        // 故须先于 fresh 判定算出（仅解析头部 yaml，开销极低）。
        // 与 combined 层共用 rime_source_paths——两处各写一份正是 R6 那条陈旧路径的成因。
        // 按缓存文件 single-flight。这里是该缺陷最典型的现场：pinyin / shuangpin /
        // 混输的 secondary 子引擎三者的 schema 都指向 pinyin/rime_frost.dict.yaml，
        // cache_path 又按源文件父目录名做命名空间，最终是同一个 merged.wdat。
        let build_lock = wind_dict::reader_pool::file_lock(&merged_wdat);
        let _build_guard = build_lock.lock().unwrap_or_else(|e| e.into_inner());

        let sub_paths = Self::rime_source_paths(dict_path, "rime_pinyin");
        let src_refs: Vec<&Path> = sub_paths.iter().map(|p| p.as_path()).collect();

        // merged 缓存对**全部源**做内容指纹校验：主表或任一子表内容变化、或源清单增删都
        // 判定失效并重建（避免「子表改了却仍用旧 merged」的静默陈旧）。
        if merged_wdat.exists()
            && Self::combined_cache_fresh(&src_refs, &merged_wdat, MERGED_CACHE_TAG)
        {
            match wind_dict::reader_pool::open_wdat(&merged_wdat) {
                Ok(reader) => {
                    info!(
                        "Using merged mmap cache: {} ({} keys)",
                        merged_wdat.display(),
                        reader.key_count()
                    );
                    return Some(CachedDict::Mmap(reader));
                }
                Err(e) => {
                    warn!("Stale merged cache ({}), regenerating", e);
                    Self::remove_stale_cache(&merged_wdat);
                }
            }
        } else if merged_wdat.exists() {
            info!(
                "merged cache stale (sources changed), regenerating: {}",
                merged_wdat.display()
            );
            Self::remove_stale_cache(&merged_wdat);
        }

        // 并行解析每个源正文（纯 CPU 多线程），直接产出 (code,text,weight)：不再为每个子表
        // 生成中间 .wdat，也绕过 CodetableDict 的 BTreeMap 构建与逐 code 排序（merged 稍后会
        // 统一按权重重排）。WdatWriter 的 add 系列不合并同 code 的多次调用（内部直接 push），
        // 故先用 HashMap 聚合，否则同一 code 会落成多条记录、读取端只命中其一 → 同 code
        // 候选系统性丢失。
        // 全拼按 code 聚合；简拼（声母缩写，如 nh→你好）按简拼码聚合，存进 wdat 独立 AbbrevSection。
        // 全拼条目携带 boundary（wdat v4 音节边界，取自 rime 源数据 `ni hao` 的空格）；
        // 简拼码是各音节首字母的拼接、本身不构成音节序列，无边界语义，故 agg_ab 不带。
        let mut agg: HashMap<String, Vec<(String, i32, u64)>> = HashMap::new();
        // 简拼 → (全拼码 → 该码下的最高权重)。**内层用 map 去重**：同一简拼下多个词
        // 常共用一个全拼码（`nh` 的「你好」「拟好」同为 `nihao`），存词时它们是不同条目，
        // 改存码后就成了重复项，不去重会把 AbbrevSection 撑大且截断时挤掉别的码。
        let mut agg_ab: HashMap<String, HashMap<String, i32>> = HashMap::new();
        let mut total_entries = 0usize;
        for sub_path in &sub_paths {
            // lowercase_code=false：import_tables 子表均为拼音表(非 english)，与改前
            // CachedDict::load_at(默认不小写 code)行为一致。
            match wind_dict::codetable::parse_rime_entries_parallel(sub_path, false) {
                Ok((fulls, abbrevs)) => {
                    let count = fulls.len();
                    info!(
                        "  Loading {} entries ({} abbrev) from {}",
                        count,
                        abbrevs.len(),
                        sub_path.display()
                    );
                    for (code, text, weight, boundary) in fulls {
                        agg.entry(code).or_default().push((text, weight, boundary));
                    }
                    for (ab, code, weight) in abbrevs {
                        let slot = agg_ab
                            .entry(ab)
                            .or_default()
                            .entry(code)
                            .or_insert(i32::MIN);
                        *slot = (*slot).max(weight);
                    }
                    total_entries += count;
                }
                Err(e) => warn!("  Failed to load {}: {}", sub_path.display(), e),
            }
        }

        if total_entries == 0 {
            warn!("No entries loaded from pinyin dictionary");
            return None;
        }

        let mut writer = wind_dict::datformat::WdatWriter::new();

        for (code, mut entries) in agg {
            // 同 code 下按权重降序，保证候选顺序稳定
            entries.sort_by_key(|e| std::cmp::Reverse(e.1));
            // order 取排序后的叶内序号，与改前 `writer.add`（内部 with_local_order）语义一致；
            // 额外携带 boundary（v4）。
            let with_order: Vec<(String, i32, u32, u64)> = entries
                .into_iter()
                .enumerate()
                .map(|(i, (text, weight, boundary))| (text, weight, i as u32, boundary))
                .collect();
            writer.add_with_boundary(code, with_order);
        }
        // 简拼表 → 独立 AbbrevSection（与全拼查询互不污染）。存的是**全拼码**，
        // 查询时据此走主表装配候选（wdat v5，见 codetable::RimeEntries）。
        // 同简拼下按权重降序——该权重只决定截断时保留哪些码。
        let abbrev_count = agg_ab.len();
        for (ab, codes) in agg_ab {
            let mut entries: Vec<(String, i32)> = codes.into_iter().collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            writer.add_abbrev(ab, entries);
        }
        info!(
            "  merged pinyin: {} codes + {} abbrevs",
            writer.key_count(),
            abbrev_count
        );

        info!("Writing merged .wdat cache ({} entries)...", total_entries);
        // 写缓存目录；若仍失败（缓存目录不可写等）退到系统临时目录。绝不退化成仅主词典
        // （rime header 仅数十条），那会让拼音/混输/临时拼音全部无候选。
        let temp_fallback = std::env::temp_dir().join(
            merged_wdat
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("rime.merged.wdat")),
        );
        for target in [&merged_wdat, &temp_fallback] {
            let is_fallback = target.as_path() == temp_fallback.as_path();
            if let Err(e) = writer.write(target) {
                if is_fallback {
                    warn!("Failed to write merged cache {}: {}", target.display(), e);
                } else {
                    // 正式缓存写不进去是**降级的起点**，不能只记 warn 就滑过去。
                    // 注意不要归因于「文件被 mmap 占用」——实测 Windows 上 rename 覆盖正被
                    // 映射的文件是会成功的（见 reader_pool 的回归测试）。真实原因通常是缓存
                    // 目录不可写、磁盘空间不足或路径权限。
                    error!(
                        "写入 merged 缓存失败 {}: {}。常见原因是缓存目录不可写、磁盘空间不足\
                         或权限受限。接下来会退到临时目录副本。",
                        target.display(),
                        e
                    );
                }
                continue;
            }
            // 写内容指纹(覆盖全部源；仅对正式缓存路径，fresh 校验也只看 merged_wdat)
            if target.as_path() == merged_wdat.as_path() {
                wind_dict::cache_fp::write_cache_fp(&merged_wdat, &src_refs, MERGED_CACHE_TAG);
            }
            match wind_dict::reader_pool::open_wdat(target) {
                Ok(reader) => {
                    if is_fallback {
                        // 功能可用，所以此前只是 info/warn——但这是一次静默降级，代价实在：
                        // 副本路径与正式缓存不同，reader 池无从合并（同一份词库映射两次），
                        // 且指纹只写正式路径，下次启动会原样再走一遍。
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

    /// 删除待重建的陈旧缓存。失败不阻断重建流程（后续 write 走 tmp + rename，本就能覆盖），
    /// 但**必须留痕**：此前这里是 `let _ =`，删不掉时日志上完全没有痕迹，而它往往是缓存
    /// 目录权限/占用问题的第一个征兆。
    ///
    /// 注意：文件正被 mmap 持有**不会**让删除以外的重建步骤失败——rename 覆盖照样成功
    /// （见 `reader_pool` 的回归测试），陈旧数据由 reader 池的 stamp 校验负责挡住。
    fn remove_stale_cache(path: &Path) {
        if let Err(e) = std::fs::remove_file(path) {
            if e.kind() == std::io::ErrorKind::NotFound {
                return; // 本就不存在，不是问题
            }
            warn!(
                "无法删除待重建的缓存 {}: {}。重建仍会继续（rename 可覆盖），\
                 但若反复出现，请检查该目录的权限。",
                path.display(),
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 混输主引擎的整句开关**不继承** `primary_schema` 的声明。
    ///
    /// 用户在 `wubi86.schema.toml` 里开了整句，不代表 `wubi86_pinyin` 也该开——后者的
    /// 超码长区间已经归拼音管（`MixedEngine::convert` 直接走 `convert_overflow`，
    /// 不经主引擎），继承过来只会得到「配置开着却不生效」这种最难排查的状态。
    #[test]
    fn mixed_primary_does_not_inherit_sentence_input() {
        // 独立方案：用自己的声明。
        assert!(resolve_sentence_input(None, true));
        assert!(!resolve_sentence_input(None, false));

        // ★ 混输主：`own=true`（primary_schema 开着）也要被压成混输方案自己的取值。
        assert!(!resolve_sentence_input(
            Some(MixedRole::Primary {
                sentence_input: false
            }),
            true
        ));
        // 混输方案自己声明了 ⇒ 开（当前只在没配拼音子引擎的退化混输下真正生效）。
        assert!(resolve_sentence_input(
            Some(MixedRole::Primary {
                sentence_input: true
            }),
            false
        ));

        // 混输次（拼音）走不到码表分支，取值等同独立方案即可。
        let sec = Some(MixedRole::Secondary(MixPinyinOpts { abbrev: true }));
        assert!(resolve_sentence_input(sec, true));
    }

    // ───────────────────────── overlay 方案注册表 ─────────────────────────

    /// 造一个含三个方案的 data_dir：两个带 `[overlay]` 段、一个不带。
    ///
    /// ⚠️ **不能断言 `overlay_modes().len()`**：`installed_schemas` 还会扫描
    /// `Config::user_config_dir()/schemas`（真实用户目录），开发机上那里可能就装着快符方案。
    /// 故所有断言都写成「我造的这几个的相对关系」，不写绝对数量。
    fn make_overlay_data_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wind_overlay_data_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let schemas = dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        // zz_kf / zz_rare：overlay 方案。id 刻意用 `zz_` 前缀——注册表按 id 字典序，
        // 这样它们排在真实用户方案之后，相对顺序断言不受开发机上装了什么影响。
        std::fs::write(
            schemas.join("zz_kf.schema.toml"),
            "[schema]\nid = \"zz_kf\"\nname = \"快符\"\nicon_label = \"符\"\nhidden = true\n\
             [engine]\ntype = \"codetable\"\n\
             [engine.codetable]\nmax_code_length = 1\n\
             [overlay]\nkind = \"special\"\nshow_all_on_enter = true\ncandidate_layout = \"vertical\"\n",
        )
        .unwrap();
        std::fs::write(
            schemas.join("zz_rare.schema.toml"),
            "[schema]\nid = \"zz_rare\"\nname = \"生僻字\"\nhidden = true\n\
             [engine]\ntype = \"codetable\"\n\
             [overlay]\n",
        )
        .unwrap();
        // 普通码表方案：无 [overlay] 段，不该进注册表。
        std::fs::write(
            schemas.join("zz_plain.schema.toml"),
            "[schema]\nid = \"zz_plain\"\nname = \"普通\"\n\
             [engine]\ntype = \"codetable\"\n",
        )
        .unwrap();
        dir
    }

    fn overlay_mgr(tag: &str) -> (EngineManager, std::path::PathBuf) {
        let dir = make_overlay_data_dir(tag);
        let ov = dir.join("overrides");
        std::fs::create_dir_all(&ov).unwrap();
        let cfg = Config::default();
        let mgr = EngineManager::with_store_override(&cfg, Some(&dir), None, Some(ov.clone()));
        (mgr, ov)
    }

    /// 注册表的准入判据是**`[overlay]` 段存在**，不是 `hidden`。
    ///
    /// 这两个属性正交：`hidden` 只说「不列进方案切换列表」，隐藏的码表方案也可能只是
    /// mix 成员、并无 overlay 生命周期。拿 hidden 当判据会把这类方案误收进特殊模式列表。
    #[test]
    fn overlay_modes_admits_by_section_presence() {
        let (mgr, _ov) = overlay_mgr("admit");
        let modes = mgr.overlay_modes();
        let ids: Vec<&str> = modes.iter().map(|e| e.schema_id.as_str()).collect();

        assert!(ids.contains(&"zz_kf"), "带 [overlay] 的方案应进表：{ids:?}");
        assert!(
            ids.contains(&"zz_rare"),
            "空 [overlay] 段同样算声明：{ids:?}"
        );
        assert!(
            !ids.contains(&"zz_plain"),
            "无 [overlay] 段的普通方案不该进表：{ids:?}"
        );
    }

    /// 下标 = 按 id 字典序，且 `overlay_index_of` 与之一致。
    ///
    /// 这条钉住的是 `ModeKind::Special(u8)` 的下标语义来源：它必须由本表给出，
    /// 不再是用户 config 数组的顺序。
    #[test]
    fn overlay_modes_sorted_by_id_and_index_matches() {
        let (mgr, _ov) = overlay_mgr("order");
        let modes = mgr.overlay_modes();
        let kf = modes.iter().position(|e| e.schema_id == "zz_kf").unwrap();
        let rare = modes.iter().position(|e| e.schema_id == "zz_rare").unwrap();
        assert!(kf < rare, "zz_kf 应排在 zz_rare 之前（id 字典序）");

        assert_eq!(mgr.overlay_index_of("zz_kf"), Some(kf as u8));
        assert_eq!(mgr.overlay_index_of("zz_rare"), Some(rare as u8));
        assert_eq!(
            mgr.overlay_index_of("zz_plain"),
            None,
            "不在表里的方案定位不到"
        );
    }

    /// 显示名/短称从方案文件 `[schema]` 派生，短称只取首字符。
    ///
    /// 原先这两个字段在 `special_modes` 条目里重复了一份、缺省时才回落方案文件；
    /// 下沉后条目即方案，重复消失。
    #[test]
    fn overlay_entry_derives_name_and_icon_from_schema() {
        let (mgr, _ov) = overlay_mgr("name");
        let modes = mgr.overlay_modes();
        let kf = modes.iter().find(|e| e.schema_id == "zz_kf").unwrap();
        assert_eq!(kf.name, "快符");
        // 一个汉字宽 2，恰好等于上限（`wind_config::ICON_LABEL_MAX_WIDTH`）、原样保留。
        // ⚠️ 这条断言**测不到截断**——它验证的是"取自方案文件且没被多截"。截断本身由
        // `wind_config::schema` 的单元测试覆盖，别在这里加长标签的 fixture 来重复测。
        assert_eq!(kf.icon_label, "符", "宽度在上限内，不截断");
        assert!(kf.spec.show_all_on_enter);
        assert_eq!(
            kf.spec.candidate_layout,
            wind_config::LayoutIntent::Vertical
        );

        let rare = modes.iter().find(|e| e.schema_id == "zz_rare").unwrap();
        assert_eq!(rare.icon_label, "", "未配 icon_label 时为空，不臆造");
        assert!(!rare.spec.show_all_on_enter, "空 [overlay] 段取字段默认值");
    }

    /// `schema_overrides/{id}.toml` 的 `[overlay]` 覆盖**自动生效**——注册表走的是
    /// 静态 `read_schema`，它内部已做 `merge_toml` 深合并，无需为 overlay 另接一条线。
    ///
    /// 这正是「配置下沉到方案文件」相对「留在 config.toml 数组」的核心收益：
    /// 用户改动的存储、编辑、保存通路全部现成。
    #[test]
    fn overlay_spec_honors_schema_override_layer() {
        let (mgr, ov) = overlay_mgr("override");
        // 覆盖前：方案文件写的是 vertical。
        let before = mgr.overlay_modes();
        let kf = before.iter().find(|e| e.schema_id == "zz_kf").unwrap();
        assert_eq!(
            kf.spec.candidate_layout,
            wind_config::LayoutIntent::Vertical
        );

        // 用户在设置页改成横排 + 关掉进入即展示 → 落 override 层。
        std::fs::write(
            ov.join("zz_kf.toml"),
            "[overlay]\ncandidate_layout = \"horizontal\"\nshow_all_on_enter = false\n",
        )
        .unwrap();
        mgr.invalidate_schema("zz_kf"); // 整表失效（见 overlay_cache 的文档）

        let after = mgr.overlay_modes();
        let kf = after.iter().find(|e| e.schema_id == "zz_kf").unwrap();
        assert_eq!(
            kf.spec.candidate_layout,
            wind_config::LayoutIntent::Horizontal,
            "override 的 [overlay] 未生效"
        );
        assert!(!kf.spec.show_all_on_enter);
        assert_eq!(kf.name, "快符", "override 未提及的字段仍来自方案文件");
    }

    /// 半衰期回落只有**两级**：本段的值 > store 默认（72h）。
    ///
    /// 码表段曾回落到拼音段（三级），已否决——设置页上那是两个独立控件，回落链会让用户
    /// 「把码表的留在 0、改了拼音的、发现码表跟着变」。回落链只在配置层不可见时是便利。
    #[test]
    fn half_life_falls_back_to_store_default_only() {
        let store_default = wind_store::freq::FreqProfile::default().half_life_hours;
        assert_eq!(store_default, 72.0, "store 默认值变了，本测试的基准需同步");

        // ① 本段有值 → 用它
        assert_eq!(EngineManager::resolve_half_life(6.0, store_default), 6.0);
        // ② 本段为 0 → store 默认。**这条是 ① 的反向对照**：若实现写成恒取第一个参数，
        //    ① 仍会绿，只有本条能抓到。
        assert_eq!(
            EngineManager::resolve_half_life(0.0, store_default),
            store_default
        );
        // 负值当未设置：配置是 f64，手改配置文件写成负数不该得出负半衰期（那会让
        // decay_factor 随时间**增长**）。
        assert_eq!(
            EngineManager::resolve_half_life(-1.0, store_default),
            store_default
        );
    }

    /// 码表衰减参数**不读拼音段任何字段**。
    ///
    /// 这条不能只靠读代码保证：`codetable_freq_profile` 曾以 `pinyin_freq_profile()` 为基，
    /// 只覆盖 half_life——那种写法下拼音段的其余字段会静默漏进来，而且看起来毫无问题。
    #[test]
    fn codetable_profile_is_independent_of_pinyin_config() {
        let mut cfg = Config::default();
        // 把拼音段三个字段全设成可辨认的非默认值。
        cfg.schema.pinyin.frequency.half_life = 999.0;
        cfg.schema.pinyin.frequency.base_scale = 888.0;
        cfg.schema.pinyin.frequency.recency_peak = 777.0;
        cfg.schema.codetable.frequency.half_life = 0.0; // 码表未设 → 该走 store 默认
        let mgr = EngineManager::new(&cfg, None);

        let def = wind_store::freq::FreqProfile::default();
        let ct = mgr.codetable_freq_profile();
        assert_eq!(
            ct.half_life_hours, def.half_life_hours,
            "不得回落拼音的 999"
        );
        assert_eq!(ct.base_scale, def.base_scale, "不得取拼音的 888");
        assert_eq!(ct.recency_peak, def.recency_peak, "不得取拼音的 777");

        // 反向对照：拼音那侧确实读到了这些值，证明上面三条不是「配置根本没生效」。
        let py = mgr.pinyin_freq_profile();
        assert_eq!(py.half_life_hours, 999.0);
        assert_eq!(py.base_scale, 888.0);
        assert_eq!(py.recency_peak, 777.0);
    }

    /// 英文衰减参数与码表、拼音**三者互不相干**，各读各的 half_life。
    ///
    /// 方案文件的 `[engine.codetable.frequency]` 能覆盖基线，**且逐字段稀疏**。
    ///
    /// 此前 `freq_settings` 直读全局镜像，方案文件里写了调频也无人读——「每方案独立调频」
    /// 这个能力看起来存在（字段能写进 TOML）、实际完全不生效。
    #[test]
    fn schema_level_frequency_overrides_baseline() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_schema_freq");
        let schemas = base_dir.join("schemas");
        let _ = std::fs::remove_dir_all(&base_dir);
        std::fs::create_dir_all(&schemas).unwrap();
        // 方案只覆盖 enabled 与 strategy，其余（promote_prefix / protect_*）留空跟随基线。
        let mut f = std::fs::File::create(schemas.join("ct_freq.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"ct_freq\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 4\n[engine.codetable.frequency]\nenabled = true\nstrategy = \"position\"\n"
        )
        .unwrap();
        drop(f);

        let mut cfg = Config::default();
        // 全局：关、step、一简保护 3 位——三项都与方案要覆盖的不同。
        cfg.schema.codetable.frequency.enabled = false;
        cfg.schema.codetable.frequency.strategy = "step".to_string();
        cfg.schema.codetable.frequency.protect_top_n_len1 = 3;
        cfg.schema.available = vec!["ct_freq".to_string()];
        cfg.schema.active = "ct_freq".to_string();
        let mgr = EngineManager::new(&cfg, Some(&base_dir));

        let s = mgr.freq_settings_for("ct_freq");
        assert!(s.enabled, "方案写了 enabled = true 应覆盖全局的 false");
        assert_eq!(
            s.strategy,
            FreqStrategy::Position,
            "方案写了 position 应覆盖全局的 step"
        );
        assert_eq!(
            s.protect.by_len[0], 3,
            "方案没写的字段应跟随基线（全局一简保护 3 位）"
        );

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// 英文此前共用码表段，于是「英文调频衰减慢一点」这个愿望只能靠改五笔的半衰期实现。
    #[test]
    fn english_profile_is_independent_of_codetable_and_pinyin() {
        let mut cfg = Config::default();
        cfg.schema.pinyin.frequency.half_life = 999.0;
        cfg.schema.codetable.frequency.half_life = 888.0;
        cfg.schema.english.frequency.half_life = 111.0;
        let mgr = EngineManager::new(&cfg, None);

        assert_eq!(mgr.english_freq_profile().half_life_hours, 111.0);
        // 反向对照：另两侧读到的是各自的值，证明上面不是「三者碰巧都取了默认」。
        assert_eq!(mgr.codetable_freq_profile().half_life_hours, 888.0);
        assert_eq!(mgr.pinyin_freq_profile().half_life_hours, 999.0);

        // 英文段留 0 时走 store 默认，**不回落码表的 888**。
        cfg.schema.english.frequency.half_life = 0.0;
        let mgr2 = EngineManager::new(&cfg, None);
        let def = wind_store::freq::FreqProfile::default();
        assert_eq!(
            mgr2.english_freq_profile().half_life_hours,
            def.half_life_hours,
            "英文段为 0 应取内置默认，不得回落码表段"
        );
    }

    /// 英文记账口径**对所有分支生效**——混输/码表方案下混进来的英文候选同样按它记账。
    ///
    /// 只在「当前是英文方案」时读这个字段的话，混输里的英文候选会静默回到默认口径，
    /// 而读写两端口径不一致的后果是：写进去的词频记录，读的时候永远找不到。
    #[test]
    fn english_code_scope_applies_to_every_schema_kind() {
        let mut cfg = Config::default();
        cfg.schema.english.frequency.code_scope = "input".to_string();
        // active 是码表方案（默认），英文候选仍应按 input 口径记账。
        let mgr = EngineManager::new(&cfg, None);
        assert!(
            mgr.freq_settings().english_code_by_input,
            "码表方案下英文候选也须按 schema.english 的口径"
        );

        // 反向对照：改回 candidate 时确实变 false，证明上面读的就是这个字段。
        cfg.schema.english.frequency.code_scope = "candidate".to_string();
        let mgr2 = EngineManager::new(&cfg, None);
        assert!(!mgr2.freq_settings().english_code_by_input);
    }

    /// 词库路径解析的四级优先级。第三级（用户目录的 wdat 优先于安装目录）是关键：
    /// 用户投放的 wdat-only 词库通常在用户目录，而兜底恒指向安装目录——Go 版正是为此
    /// 才需要额外的 wdbOnlyHint 参数。
    #[test]
    fn resolve_dict_file_priority_order() {
        let base = std::env::temp_dir().join(format!("wind-resolve-{}", std::process::id()));
        let user = base.join("user/schemas");
        let sys = base.join("sys/schemas");
        std::fs::create_dir_all(user.join("s")).unwrap();
        std::fs::create_dir_all(sys.join("s")).unwrap();
        let rel = "s/d.dict.yaml";
        let touch = |p: &std::path::Path| std::fs::write(p, b"x").unwrap();

        // 全空 → 兜底安装目录
        assert_eq!(
            EngineManager::resolve_dict_file_in(rel, Some(&user), &sys),
            sys.join(rel),
            "全都没有时应兜底到安装目录（与改造前行为一致）"
        );

        // 只有安装目录的 wdat → 命中它
        touch(&sys.join("s/d.wdat"));
        assert_eq!(
            EngineManager::resolve_dict_file_in(rel, Some(&user), &sys),
            sys.join(rel)
        );

        // 用户目录也有 wdat → 用户目录优先
        touch(&user.join("s/d.wdat"));
        assert_eq!(
            EngineManager::resolve_dict_file_in(rel, Some(&user), &sys),
            user.join(rel),
            "用户目录的 wdat 必须优先于安装目录"
        );

        // 出现安装目录的 yaml → yaml 整体优先于 wdat
        touch(&sys.join(rel));
        assert_eq!(
            EngineManager::resolve_dict_file_in(rel, Some(&user), &sys),
            sys.join(rel),
            "yaml 在场时不得被任何 wdat 抢走"
        );

        // 用户目录的 yaml → 最高优先级
        touch(&user.join(rel));
        assert_eq!(
            EngineManager::resolve_dict_file_in(rel, Some(&user), &sys),
            user.join(rel)
        );

        // 无用户目录时不应 panic
        assert_eq!(
            EngineManager::resolve_dict_file_in(rel, None, &sys),
            sys.join(rel)
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn freq_strategy_top_parsed() {
        assert_eq!(EngineManager::parse_freq_strategy("top"), FreqStrategy::Top);
    }

    #[test]
    fn freq_strategy_step_and_unknown_fallback() {
        assert_eq!(
            EngineManager::parse_freq_strategy("step"),
            FreqStrategy::Step
        );
        // 未知策略值回退 step（稳健默认）。
        assert_eq!(
            EngineManager::parse_freq_strategy("bogus"),
            FreqStrategy::Step,
            "未知策略应回退 step"
        );
    }

    /// 临时拼音目标方案取自 schema.primary_pinyin（空=全拼）。
    /// 回归防线：该字段一度只被定义/登记而无人读取，导致临时拼音恒为全拼。
    #[test]
    fn temp_pinyin_target_follows_primary_pinyin() {
        let ct = Some(EngineType::CodeTable);
        assert_eq!(
            EngineManager::resolve_temp_pinyin_target(true, ct, "shoudao"),
            Some("shoudao".to_string()),
            "配置的主拼音方案应作为临时拼音目标"
        );
        assert_eq!(
            EngineManager::resolve_temp_pinyin_target(true, ct, ""),
            Some("pinyin".to_string()),
            "primary_pinyin 空应定义为全拼，而非扫描 available"
        );
        assert_eq!(
            EngineManager::resolve_temp_pinyin_target(false, ct, "shoudao"),
            None,
            "总开关关闭时不进临时拼音"
        );
    }

    /// 临时拼音仅适用码表/混输方案。判据必须在 target 这个公共门卫上——
    /// 曾只在引导键分支加判据，热键/顶屏进模式等入口直接漏网。
    #[test]
    fn temp_pinyin_target_scope_limited_to_codetable_and_mixed() {
        for ty in [EngineType::CodeTable, EngineType::Mixed] {
            assert_eq!(
                EngineManager::resolve_temp_pinyin_target(true, Some(ty), "shoudao"),
                Some("shoudao".to_string()),
                "{ty:?} 方案应支持临时拼音"
            );
        }
        // 拼音方案：本身就在打拼音，不再叠一层（引导符须留给标点输出）。
        assert_eq!(
            EngineManager::resolve_temp_pinyin_target(true, Some(EngineType::Pinyin), "shoudao"),
            None,
            "拼音方案不应进入临时拼音"
        );
        assert_eq!(
            EngineManager::resolve_temp_pinyin_target(true, Some(EngineType::English), "shoudao"),
            None,
            "英文引擎不应进入临时拼音"
        );
        assert_eq!(
            EngineManager::resolve_temp_pinyin_target(true, None, "shoudao"),
            None,
            "无活跃引擎时不应进入临时拼音"
        );
    }

    /// primary_pinyin 经 config 一路传到 manager 缓存（构造期接线防线）。
    #[test]
    fn primary_pinyin_wired_from_config() {
        let mut cfg = Config::default();
        cfg.schema.primary_pinyin = "shoudao".to_string();
        let mgr = EngineManager::with_store_override(&cfg, None, None, None);
        assert_eq!(
            *mgr.primary_pinyin.lock().unwrap(),
            "shoudao",
            "config.schema.primary_pinyin 应在构造期进入 manager"
        );
    }

    #[test]
    fn codetable_inline_resolves_over_global() {
        // 全局基线 + 方案 [engine.codetable] 行为逐字段折叠（Some 覆盖 / None 回落）。
        let global = wind_config::CodetableGlobal {
            top_code_commit: false,
            z_key_repeat: false,
            ..Default::default()
        };
        let ov = wind_config::schema::CodeTableSpec {
            top_code_commit: Some(true),
            ..Default::default()
        };
        let eff = global.resolved(Some(&ov));
        assert!(eff.top_code_commit, "Some 字段应覆盖全局");
        assert!(!eff.z_key_repeat, "None 字段应回落全局");
        // 无方案行为（None）：整体回落全局。
        assert!(!global.resolved(None).top_code_commit, "无覆盖时应回落全局");
    }

    /// Fix 1+2 端到端：方案 `.schema.toml` 内联 `[engine.codetable]` 行为无需 override 文件即生效，
    /// 且逐字段折叠到全局基线（内联给的覆盖，未给的回落）。
    #[test]
    fn resolve_codetable_reads_schema_inline_behavior() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_inline_data");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        let mut f = std::fs::File::create(schemas.join("wb_test.schema.toml")).unwrap();
        // 内联：自动上屏开、顶码关；z_key_repeat 不写 → 应回落全局。
        // **不写 hidden**：回落全局是普通方案的语义，特殊方案另有一条
        // （见 special_schema_does_not_inherit_global_codetable）。此处原用 qsym +
        // hidden = true，两条语义撞在同一个用例里，特殊方案改为不继承后才暴露出来。
        write!(
            f,
            "[schema]\nid = \"wb_test\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 8\nauto_commit_at_full = true\ntop_code_commit = false\n"
        )
        .unwrap();
        drop(f);

        // 全局基线：auto_commit_at_full=false / top_code_commit=true / z_key_repeat=true。
        let global = wind_config::CodetableGlobal {
            auto_commit_at_full: false,
            top_code_commit: true,
            z_key_repeat: true,
            ..Default::default()
        };
        // override_dir 指向空目录：证明无 override 文件也能读到内联行为。
        let ov_dir = std::env::temp_dir().join("wind_eng_inline_empty_ov");
        let _ = std::fs::remove_dir_all(&ov_dir);
        let eff =
            EngineManager::resolve_codetable("wb_test", Some(&base_dir), &global, Some(&ov_dir));
        assert!(eff.auto_commit_at_full, "内联 Some(true) 应覆盖全局 false");
        assert!(!eff.top_code_commit, "内联 Some(false) 应覆盖全局 true");
        assert!(eff.z_key_repeat, "内联未给的字段应回落全局 true");

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// overlay 方案（带 `[overlay]` 段）**不继承**全局 `schema.codetable`：没写的字段取内置
    /// 默认，而不是主码表的设置。
    ///
    /// 快符是几十条的小符号表，全局基线是按五笔那种数万条全码表调的。继承的后果是用户改
    /// 五笔的「精确匹配」，快符跟着变，而改的人根本意识不到自己动了另一个表。
    #[test]
    fn overlay_schema_does_not_inherit_global_codetable() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_special_baseline");
        let schemas = base_dir.join("schemas");
        let _ = std::fs::remove_dir_all(&base_dir);
        std::fs::create_dir_all(&schemas).unwrap();
        // 两个方案，除 `[overlay]` 段外**逐字相同**——差别只能来自那一段本身。
        for (id, overlay) in [("sp_overlay", true), ("sp_normal", false)] {
            let mut f = std::fs::File::create(schemas.join(format!("{id}.schema.toml"))).unwrap();
            let ov = if overlay { "[overlay]\n" } else { "" };
            write!(
                f,
                "[schema]\nid = \"{id}\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 8\n{ov}"
            )
            .unwrap();
        }

        // 基准是**特殊方案基线**，不是 CodetableGlobal::default()——后者是结构体零值，
        // 与「特殊方案该长什么样」是两件事（见 special_schema_baseline 的文档）。
        let def = EngineManager::special_schema_baseline();
        // 全局把三个开关都拨到与该基线相反的一侧，这样「有没有继承」一眼可辨。
        let global = wind_config::CodetableGlobal {
            single_code_input: !def.single_code_input,
            z_key_repeat: !def.z_key_repeat,
            top_code_commit: !def.top_code_commit,
            ..Default::default()
        };
        let ov = std::env::temp_dir().join("wind_eng_special_baseline_ov");
        let _ = std::fs::remove_dir_all(&ov);

        let sp =
            EngineManager::resolve_codetable("sp_overlay", Some(&base_dir), &global, Some(&ov));
        assert_eq!(
            sp.single_code_input, def.single_code_input,
            "overlay 方案不该继承全局的精确匹配"
        );
        assert_eq!(sp.z_key_repeat, def.z_key_repeat);
        assert_eq!(sp.top_code_commit, def.top_code_commit);

        // 反向对照：同样的文件去掉 `[overlay]` 就该继承——否则上面三条在「全局基线整个
        // 失效」时也会通过。
        let np = EngineManager::resolve_codetable("sp_normal", Some(&base_dir), &global, Some(&ov));
        assert_eq!(
            np.single_code_input, global.single_code_input,
            "普通方案仍须继承全局"
        );
        assert_eq!(np.z_key_repeat, global.z_key_repeat);
        assert_eq!(np.top_code_commit, global.top_code_commit);

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// ★ 基线判据是 `[overlay]` 段而**不是** `hidden`：钉住两个「反对角」组合。
    ///
    /// 这两例正是旧判据（`hidden`）判错的地方，本测试在换判据前是红的：
    ///
    /// | 方案 | 旧判据（hidden） | 新判据（overlay） |
    /// |---|---|---|
    /// | 有 `[overlay]`、无 `hidden` | 全局 ❌ 会继承五笔的精确匹配 | 内置基线 ✅ |
    /// | 有 `hidden`、无 `[overlay]` | 内置基线 ❌ | 全局 ✅ 它只是不列进切换列表的普通码表 |
    ///
    /// 必须两条一起断言：只测其一的话，判据被改成恒真/恒假时另一半照样通过。
    #[test]
    fn codetable_baseline_keys_on_overlay_section_not_hidden() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_baseline_offdiag");
        let schemas = base_dir.join("schemas");
        let _ = std::fs::remove_dir_all(&base_dir);
        std::fs::create_dir_all(&schemas).unwrap();
        // 两个方案的 hidden 与 overlay 刻意**取反**，两个属性由此可分辨。
        for (id, extra) in [("od_overlay_only", "[overlay]\n"), ("od_hidden_only", "")] {
            let mut f = std::fs::File::create(schemas.join(format!("{id}.schema.toml"))).unwrap();
            let h = if id == "od_hidden_only" {
                "hidden = true\n"
            } else {
                ""
            };
            write!(
                f,
                "[schema]\nid = \"{id}\"\n{h}[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 8\n{extra}"
            )
            .unwrap();
        }

        let def = EngineManager::special_schema_baseline();
        // 全局拨到与内置基线相反的一侧，「取了哪份基线」一眼可辨。
        let global = wind_config::CodetableGlobal {
            single_code_input: !def.single_code_input,
            z_key_repeat: !def.z_key_repeat,
            ..Default::default()
        };
        let ov = std::env::temp_dir().join("wind_eng_baseline_offdiag_ov");
        let _ = std::fs::remove_dir_all(&ov);

        // 有 [overlay] 但没写 hidden：仍取内置基线（旧判据在这里会去继承全局）。
        let a = EngineManager::resolve_codetable(
            "od_overlay_only",
            Some(&base_dir),
            &global,
            Some(&ov),
        );
        assert_eq!(
            a.single_code_input, def.single_code_input,
            "声明了 [overlay] 就该取内置基线，与写没写 hidden 无关"
        );
        assert_eq!(a.z_key_repeat, def.z_key_repeat);

        // hidden 但没有 [overlay]：它只是不进切换列表的普通码表，该跟随全局
        // （旧判据在这里会误给内置基线）。
        let b =
            EngineManager::resolve_codetable("od_hidden_only", Some(&base_dir), &global, Some(&ov));
        assert_eq!(
            b.single_code_input, global.single_code_input,
            "hidden 只是不列进切换列表，没有 [overlay] 就该跟随全局"
        );
        assert_eq!(b.z_key_repeat, global.z_key_repeat);

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// `effective_codetable`（设置页的初值快照来源）必须与 `resolve_codetable`（引擎实际
    /// 构建时用的折叠）**同口径**——它们是第三、第四个平行折叠点，一处改了另一处没改，
    /// 用户会看到「设置页显示精确匹配是关的，实际按开的跑」。
    ///
    /// 顺带钉住基线分流：设置页给 overlay 方案显示的初值必须来自 overlay 基线，否则用户
    /// 一打开「自定义码表配置」就把全局的值写进了本不继承全局的方案。
    #[test]
    fn effective_codetable_matches_resolve_and_splits_by_overlay() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_effective_ct");
        let schemas = base_dir.join("schemas");
        let _ = std::fs::remove_dir_all(&base_dir);
        std::fs::create_dir_all(&schemas).unwrap();
        // 与 overlay_schema_does_not_inherit_global_codetable 同构：除 `[overlay]` 外逐字相同。
        for (id, overlay) in [("eff_overlay", true), ("eff_normal", false)] {
            let mut f = std::fs::File::create(schemas.join(format!("{id}.schema.toml"))).unwrap();
            let ov = if overlay { "[overlay]\n" } else { "" };
            write!(
                f,
                "[schema]\nid = \"{id}\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 4\nsingle_code_complete = false\n{ov}"
            )
            .unwrap();
        }

        let def = EngineManager::special_schema_baseline();
        let mut cfg = Config::default();
        // 全局三项都拨到与特殊基线相反的一侧：继承与否一眼可辨。
        cfg.schema.codetable.single_code_input = !def.single_code_input;
        cfg.schema.codetable.z_key_repeat = !def.z_key_repeat;
        cfg.schema.codetable.punct_commit = !def.punct_commit;
        cfg.schema.available = vec!["eff_normal".to_string()];
        cfg.schema.active = "eff_normal".to_string();
        let mgr = EngineManager::new(&cfg, Some(&base_dir));

        let sp = mgr.effective_codetable("eff_overlay");
        assert_eq!(
            sp.single_code_input, def.single_code_input,
            "overlay 方案的初值快照该来自 overlay 基线，不该继承全局"
        );
        assert_eq!(sp.z_key_repeat, def.z_key_repeat);
        assert_eq!(sp.punct_commit, def.punct_commit);
        assert!(
            !sp.single_code_complete,
            "方案文件显式写了 false，应压过基线的 true"
        );

        // 反向对照：去掉 `[overlay]` 就该继承全局——否则上面三条在「全局整个失效」时也通过。
        let np = mgr.effective_codetable("eff_normal");
        assert_eq!(
            np.single_code_input, cfg.schema.codetable.single_code_input,
            "普通方案的初值快照仍须继承全局"
        );
        assert_eq!(np.z_key_repeat, cfg.schema.codetable.z_key_repeat);

        // 与引擎构建实际用的那条折叠路径逐字段比对（同口径守护）。
        for id in ["eff_overlay", "eff_normal"] {
            let via_resolve = EngineManager::resolve_codetable(
                id,
                Some(&base_dir),
                &cfg.schema.codetable,
                mgr.override_dir.as_deref(),
            );
            assert_eq!(
                mgr.effective_codetable(id),
                via_resolve,
                "{id}: 设置页快照与引擎实际折叠必须一致"
            );
        }

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn merge_toml_table_recurse_and_scalar_replace() {
        let mut base: toml::Value = toml::from_str("a = 1\n[t]\nx = 1\ny = 2\n").unwrap();
        let over: toml::Value = toml::from_str("a = 9\n[t]\ny = 20\nz = 30\n").unwrap();
        merge_toml(&mut base, over);
        assert_eq!(base.get("a").unwrap().as_integer(), Some(9));
        let t = base.get("t").unwrap();
        assert_eq!(t.get("x").unwrap().as_integer(), Some(1), "未覆盖键保留");
        assert_eq!(t.get("y").unwrap().as_integer(), Some(20), "覆盖键替换");
        assert_eq!(t.get("z").unwrap().as_integer(), Some(30), "新增键加入");
    }

    /// **键集合可变的表**（键名是数据而非结构字段，如 `key_actions` 的按键名）同样逐键合并。
    ///
    /// 与上一个用例的区别：那里的 x/y/z 是已知字段，这里的键由用户任意填写。`merge_toml`
    /// 不关心键名语义，故两者行为一致——但这是 `docs/design/schema-key-actions.md` §3
    /// 「逐键合并」覆盖语义能成立的前提，值得单独钉住。
    ///
    /// ★ 顺带钉住一条**能力缺口**：override **无法表达「删除 base 的某个键」**。
    /// 合并只会新增/覆盖，base 里有而 override 里没有的键恒保留。因此「本方案禁用某个
    /// 全局绑定」只能靠显式哨兵值（设计里的 `none`），不能靠"从 override 里删掉这一行"。
    #[test]
    fn merge_toml_merges_tables_with_arbitrary_key_sets() {
        // base = 方案作者内联；over = 用户 override。键名均为按键名，非结构字段。
        let mut base: toml::Value =
            toml::from_str("[key_actions]\nbackslash = \"special:fuhao\"\nz = \"temp_pinyin\"\n")
                .unwrap();
        let over: toml::Value = toml::from_str(
            "[key_actions]\nz = \"temp_english\"\nrshift = \"toggle_schema:english\"\n",
        )
        .unwrap();
        merge_toml(&mut base, over);

        let ka = base.get("key_actions").unwrap();
        assert_eq!(
            ka.get("backslash").unwrap().as_str(),
            Some("special:fuhao"),
            "override 未提及的键保留——这是逐键合并而非整段替换的判据"
        );
        assert_eq!(
            ka.get("z").unwrap().as_str(),
            Some("temp_english"),
            "同名键被 override 覆盖"
        );
        assert_eq!(
            ka.get("rshift").unwrap().as_str(),
            Some("toggle_schema:english"),
            "override 新增的键加入"
        );
        assert_eq!(
            ka.as_table().unwrap().len(),
            3,
            "合并结果 = 两侧键集合的并集"
        );
    }

    /// dictionaries 的 override 是**按 id 的稀疏合并**，不是数组整体替换：
    /// 只有 enabled 会被覆盖，结构定义（顺序/path/label/base_order）恒以方案文件为准。
    #[test]
    fn merge_toml_dictionaries_only_overrides_enabled_by_id() {
        // 方案文件：三个库，其中 ext2 是"方案后续新增"的（override 写入时还不存在）。
        let mut base: toml::Value = toml::from_str(
            "[[dictionaries]]\nid = \"main\"\npath = \"a.yaml\"\ndefault = true\nbase_order = 0\n\
             [[dictionaries]]\nid = \"ext1\"\npath = \"new/b.yaml\"\nlabel = \"新标签\"\nbase_order = 1\n\
             [[dictionaries]]\nid = \"ext2\"\npath = \"c.yaml\"\nbase_order = 2\n",
        )
        .unwrap();
        // override：老格式整表快照（含 path/label 副本）+ 一个方案已删除的库 gone。
        let over: toml::Value = toml::from_str(
            "[[dictionaries]]\nid = \"ext1\"\nenabled = false\npath = \"old/b.yaml\"\nlabel = \"旧标签\"\nbase_order = 99\n\
             [[dictionaries]]\nid = \"gone\"\nenabled = true\npath = \"gone.yaml\"\n",
        )
        .unwrap();
        merge_toml(&mut base, over);

        let arr = base.get("dictionaries").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 3, "方案文件的库集合与顺序不受 override 影响");
        assert_eq!(arr[2].get("id").unwrap().as_str(), Some("ext2"));
        assert!(
            arr[2].get("enabled").is_none(),
            "override 未提及的库保持无显式 enabled（继承 default_enabled）"
        );

        let ext1 = &arr[1];
        assert_eq!(
            ext1.get("enabled").unwrap().as_bool(),
            Some(false),
            "enabled 被覆盖"
        );
        assert_eq!(
            ext1.get("path").unwrap().as_str(),
            Some("new/b.yaml"),
            "path 以方案文件为准——老快照里的副本必须被忽略"
        );
        assert_eq!(ext1.get("label").unwrap().as_str(), Some("新标签"));
        assert_eq!(ext1.get("base_order").unwrap().as_integer(), Some(1));

        assert!(
            !arr.iter()
                .any(|d| d.get("id").and_then(|v| v.as_str()) == Some("gone")),
            "方案已删除的库不因 override 复活"
        );
    }

    /// base 侧没有 dictionaries 时，override 的稀疏项无 path、造不出可用词库，应整键忽略。
    #[test]
    fn merge_toml_dictionaries_not_created_from_override_alone() {
        let mut base: toml::Value = toml::from_str("[schema]\nid = \"x\"\n").unwrap();
        let over: toml::Value =
            toml::from_str("[[dictionaries]]\nid = \"ext1\"\nenabled = true\n").unwrap();
        merge_toml(&mut base, over);
        assert!(base.get("dictionaries").is_none());
    }

    /// 方案级 `[session_actions]`：逐键合并 + 显式 `none` 留在表里 + 并集滤掉 `none`。
    ///
    /// 三条断言各守一个会被「简化」掉的点：
    /// - override 逐键合并（整段替换会让方案作者内联的其余键消失）；
    /// - 显式 `none` **必须留在表里**——过滤掉它，消费点就会以为「方案没提这个键」而回落
    ///   全局，用户写的禁用完全失效；
    /// - 但**并集要滤掉** `none`——并集的消费者据此决定装不装 CapsLock 全局钩子、要不要让
    ///   TSF 转发，被禁用的键两者都不需要。两处方向相反，合并成一套就必错一头。
    #[test]
    fn active_session_actions_merges_per_key_and_union_drops_none() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_sa_data");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        let mut f = std::fs::File::create(schemas.join("sa.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"sa\"\n[engine]\ntype = \"codetable\"\n\
             [session_actions]\nminus = \"page_prev\"\nequal = \"page_next\"\n"
        )
        .unwrap();
        drop(f);

        let ov_dir = std::env::temp_dir().join("wind_eng_sa_overrides");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let mut cfg = Config::default();
        cfg.schema.active = "sa".to_string();
        cfg.schema.available = vec!["sa".to_string()];
        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        let inline = mgr.active_session_actions();
        assert_eq!(inline.get("minus").map(String::as_str), Some("page_prev"));
        assert_eq!(inline.get("equal").map(String::as_str), Some("page_next"));

        // 用户 override：改 minus 的动作、禁用 equal、不提方案作者写的其余键。
        let ov: toml::Value =
            toml::from_str("[session_actions]\nminus = \"page_next\"\nequal = \"none\"\n").unwrap();
        mgr.write_schema_override("sa", &ov).unwrap();

        let merged = mgr.active_session_actions();
        assert_eq!(
            merged.get("minus").map(String::as_str),
            Some("page_next"),
            "override 应逐键覆盖"
        );
        assert_eq!(
            merged.get("equal").map(String::as_str),
            Some("none"),
            "显式 none 必须留在表里——滤掉它，消费点会回落全局，用户的禁用就失效了"
        );

        let union = mgr.all_session_action_keys();
        assert!(union.contains("minus"), "启用的键要进可达性并集");
        assert!(
            !union.contains("equal"),
            "显式 none 的键不该进并集——否则白装一个全局钩子 / 白转发一个不动作的键"
        );

        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    /// 方案级 `[key_actions]`：方案文件内联 + override **逐键合并**，不是整段替换。
    ///
    /// 这是 `docs/design/schema-key-actions.md` §3 覆盖语义的端到端确认——上游
    /// `merge_toml_merges_tables_with_arbitrary_key_sets` 只证明了 toml 层的合并行为，
    /// 这里证明它真的贯通到 `active_key_actions()` 的返回值。
    #[test]
    fn active_key_actions_merges_schema_file_and_override_per_key() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_ka_data");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        let mut f = std::fs::File::create(schemas.join("ka.schema.toml")).unwrap();
        // 方案作者内联：`\` 进快符，z 进临拼。
        write!(
            f,
            "[schema]\nid = \"ka\"\n[engine]\ntype = \"codetable\"\n\
             [key_actions]\nbackslash = \"special:fuhao\"\nz = \"temp_pinyin\"\n"
        )
        .unwrap();
        drop(f);

        let ov_dir = std::env::temp_dir().join("wind_eng_ka_overrides");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let mut cfg = Config::default();
        cfg.schema.active = "ka".to_string();
        cfg.schema.available = vec!["ka".to_string()];
        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        let inline = mgr.active_key_actions();
        assert_eq!(
            inline.get("backslash").map(String::as_str),
            Some("special:fuhao")
        );
        assert_eq!(inline.get("z").map(String::as_str), Some("temp_pinyin"));

        // 用户 override：改 z 的去向、给 grave 加禁用、不提 backslash。
        let ov: toml::Value =
            toml::from_str("[key_actions]\nz = \"temp_english\"\ngrave = \"none\"\n").unwrap();
        mgr.write_schema_override("ka", &ov).unwrap();

        let merged = mgr.active_key_actions();
        assert_eq!(
            merged.get("backslash").map(String::as_str),
            Some("special:fuhao"),
            "override 未提及的键必须保留——整段替换会让它消失"
        );
        assert_eq!(
            merged.get("z").map(String::as_str),
            Some("temp_english"),
            "同名键被 override 覆盖"
        );
        assert_eq!(
            merged.get("grave").map(String::as_str),
            Some("none"),
            "override 新增的禁用项加入"
        );

        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    /// 方案级 `[punct]` / `[candidate]` / `[phrases]`：内联 + override 合并，三段同批失效。
    ///
    /// 与 `active_key_actions_merges_schema_file_and_override_per_key` 同形，
    /// 但这里额外钉住**三段共用一个缓存条目**时的失效正确性：override 只改一段，
    /// 另两段不能被顺手清成默认值（三段合订的实现方式若写成「读一段存一段」就会）。
    #[test]
    fn active_behavior_merges_schema_file_and_override() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_beh_data");
        let schemas = base_dir.join("schemas");
        let _ = std::fs::remove_dir_all(&base_dir);
        std::fs::create_dir_all(&schemas).unwrap();
        let mut f = std::fs::File::create(schemas.join("beh.schema.toml")).unwrap();
        write!(
            f,
            "[schema]
id = \"beh\"
[engine]
type = \"codetable\"
             [punct]
mode = \"english\"
             [candidate]
layout = \"vertical\"
             [phrases]
enabled = false
"
        )
        .unwrap();
        drop(f);

        let ov_dir = std::env::temp_dir().join("wind_eng_beh_overrides");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let mut cfg = Config::default();
        cfg.schema.active = "beh".to_string();
        cfg.schema.available = vec!["beh".to_string()];
        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        let inline = mgr.active_behavior();
        assert_eq!(inline.punct, wind_config::PunctIntent::English);
        assert_eq!(inline.candidate_layout, wind_config::LayoutIntent::Vertical);
        assert!(!inline.phrases.enabled);

        // 用户 override：只改布局，不提另外两段。
        let ov: toml::Value = toml::from_str(
            "[candidate]
layout = \"horizontal\"
",
        )
        .unwrap();
        mgr.write_schema_override("beh", &ov).unwrap();

        let merged = mgr.active_behavior();
        assert_eq!(
            merged.candidate_layout,
            wind_config::LayoutIntent::Horizontal,
            "override 覆盖同段"
        );
        assert_eq!(
            merged.punct,
            wind_config::PunctIntent::English,
            "override 未提及的段必须保留——三段合订若写成「读一段存一段」这里会回落 Follow"
        );
        assert!(!merged.phrases.enabled, "override 未提及的段必须保留");

        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    /// 读不到方案（文件缺失）时三段全部回落默认值 = 一段都不覆盖。
    ///
    /// 这条不是凑数：默认值若不是「不覆盖」，一个装歪的方案包就会静默改掉用户的标点态
    /// 与候选方向，而用户找不到是谁改的。
    #[test]
    fn active_behavior_defaults_to_no_override_when_schema_missing() {
        let mut cfg = Config::default();
        cfg.schema.active = "nonexistent_schema".to_string();
        let dir = std::env::temp_dir().join("wind_eng_beh_missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("schemas")).unwrap();
        let mgr = EngineManager::with_store_override(&cfg, Some(&dir), None, Some(dir.clone()));

        let b = mgr.active_behavior();
        assert_eq!(b.punct, wind_config::PunctIntent::Follow);
        assert_eq!(b.candidate_layout, wind_config::LayoutIntent::Follow);
        assert!(
            b.phrases.enabled,
            "短语默认加载——默认关会让所有方案静默失去短语"
        );
        assert!(b.phrases.categories.is_empty());
        assert!(b.phrases.exclude_categories.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `schema_code_char_set` 必须与引擎构建同规则：`leading_chars` 为空时等于全集。
    ///
    /// 它服务于设置页的「这个键是不是本方案首码」提示。两处规则若不一致，提示就会
    /// 指向一个内核并不认可的冲突——比不提示更糟，用户会按提示去改一个没坏的配置。
    #[test]
    fn schema_code_char_set_follows_engine_rules() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_ccs_data");
        let schemas = base_dir.join("schemas");
        let _ = std::fs::remove_dir_all(&base_dir);
        std::fs::create_dir_all(&schemas).unwrap();
        // input_chars 含 `;`，leading_chars 未写 → 首码集 = 全集，故 `;` 可起头。
        let mut f = std::fs::File::create(schemas.join("ccs.schema.toml")).unwrap();
        write!(
            f,
            "[schema]
id = \"ccs\"
[engine]
type = \"codetable\"
             [engine.codetable]
input_chars = \"a-z;\"
"
        )
        .unwrap();
        drop(f);

        let mut cfg = Config::default();
        cfg.schema.active = "ccs".to_string();
        cfg.schema.available = vec!["ccs".to_string()];
        let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, None);

        let set = mgr.schema_code_char_set("ccs").expect("方案应可读");
        assert!(set.contains_leading(';'), "leading_chars 为空时应等于全集");
        assert!(set.contains_leading('a'));
        assert!(!set.contains_leading('/'), "没配的符号不该在首码集里");

        // 读不到的方案返回 None，而不是默认 a-z——拿默认值去提示等于凭空报冲突。
        assert!(mgr.schema_code_char_set("no_such_schema").is_none());

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// 方案没写 `[key_actions]` 时返回空表——各键照常走全局引导键链。
    #[test]
    fn active_key_actions_empty_when_schema_declares_none() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_ka_none_data");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        let mut f = std::fs::File::create(schemas.join("kan.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"kan\"\n[engine]\ntype = \"codetable\"\n"
        )
        .unwrap();
        drop(f);

        let mut cfg = Config::default();
        cfg.schema.active = "kan".to_string();
        cfg.schema.available = vec!["kan".to_string()];
        let mgr = EngineManager::with_store_override(&cfg, Some(&base_dir), None, None);
        assert!(mgr.active_key_actions().is_empty());

        let _ = std::fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn schema_override_merge_and_delete() {
        use std::io::Write;
        let base_dir = std::env::temp_dir().join("wind_eng_ov_data");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        let mut f = std::fs::File::create(schemas.join("tcfg.schema.toml")).unwrap();
        write!(
            f,
            "[schema]\nid = \"tcfg\"\nname = \"基础名\"\n[engine]\ntype = \"codetable\"\n[engine.codetable]\nmax_code_length = 4\n"
        )
        .unwrap();
        drop(f);

        let ov_dir = std::env::temp_dir().join("wind_eng_ov_overrides");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let cfg = Config::default();
        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        // 基础层
        let base = mgr.schema_base("tcfg").unwrap();
        assert_eq!(base.schema.name, "基础名");
        assert_eq!(base.engine.codetable.max_code_length, 4);

        // 写 override：覆盖 name + max_code_length
        let ov: toml::Value = toml::from_str(
            "[schema]\nname = \"覆盖名\"\n[engine.codetable]\nmax_code_length = 5\n",
        )
        .unwrap();
        mgr.write_schema_override("tcfg", &ov).unwrap();

        let merged = mgr.schema_merged("tcfg").unwrap();
        assert_eq!(merged.schema.name, "覆盖名", "override 覆盖 name");
        assert_eq!(merged.engine.codetable.max_code_length, 5);
        assert_eq!(merged.schema.id, "tcfg", "未覆盖字段保留基础值");
        // base 不受 override 影响
        assert_eq!(mgr.schema_base("tcfg").unwrap().schema.name, "基础名");

        // 删除 override → 回到基础层
        assert!(mgr.delete_schema_override("tcfg").unwrap());
        assert_eq!(mgr.schema_merged("tcfg").unwrap().schema.name, "基础名");

        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    /// Task 4.3：验证 shuangpin 方案在 EngineManager 层 available 过滤中真正被放行。
    ///
    /// 测试设计：
    /// - 用 temp 数据目录，写三个最小 schema TOML：
    ///   * "dummy_ct"：codetable 类型，作为 active（ensure_loaded 无词库会 warn 但不 panic）
    ///   * "sp_test"：pinyin + scheme="shuangpin" → is_supported()=true，应留在 available
    ///   * "sp_unsupported"：pinyin + scheme="ziranma_xxx" → is_supported()=false，应被过滤
    /// - 不触发词库加载（shuangpin 不是 active，schema_supported 只做 TOML 解析）
    /// - Linux 无词库环境可跑。
    #[test]
    fn shuangpin_available_not_filtered_out() {
        use std::io::Write;

        // 建 temp 数据目录
        let base_dir = std::env::temp_dir().join("wind_eng_sp_available_test");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();

        // active schema：最小 codetable，无词库（ensure_loaded 失败 = warn，manager 仍构造成功）
        {
            let mut f = std::fs::File::create(schemas.join("dummy_ct.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"dummy_ct\"\n[engine]\ntype = \"codetable\"\n"
            )
            .unwrap();
        }

        // shuangpin schema：engine.type="pinyin" + scheme="shuangpin" → is_supported()=true
        {
            let mut f = std::fs::File::create(schemas.join("sp_test.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"sp_test\"\n[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"shuangpin\"\n"
            )
            .unwrap();
        }

        // 不支持的双拼变体：engine.type="pinyin" + scheme="ziranma_xxx" → is_supported()=false
        {
            let mut f = std::fs::File::create(schemas.join("sp_unsupported.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"sp_unsupported\"\n[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"ziranma_xxx\"\n"
            )
            .unwrap();
        }

        // 构造 config：active = dummy_ct（首个 available 即为 active）
        let mut cfg = Config::default();
        cfg.schema.active = "dummy_ct".into();
        cfg.schema.available = vec!["dummy_ct".into(), "sp_test".into(), "sp_unsupported".into()];

        let ov_dir = std::env::temp_dir().join("wind_eng_sp_available_ov");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        let available = mgr.available_schemas();

        // shuangpin 方案应通过过滤，进入 available 列表
        assert!(
            available.contains(&"sp_test".to_string()),
            "shuangpin schema 应在 available 中，实际 available={available:?}"
        );

        // 不支持的 scheme 应被过滤掉（过滤仍有效）
        assert!(
            !available.contains(&"sp_unsupported".to_string()),
            "ziranma_xxx schema 应被过滤，实际 available={available:?}"
        );

        // active 方案始终保留（不论 schema_supported 结果如何）
        assert!(
            available.contains(&"dummy_ct".to_string()),
            "active schema 应始终保留，实际 available={available:?}"
        );

        // 清理
        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    /// 双拼布局的非字母键（mspy `;` = ing）必须成为**码元**、但**不可作首码**。
    ///
    /// 这是「`;` 能打 ying」的唯一接线点：协调器只认 `active_is_code_char`，
    /// 布局里写了 `";" = ["ing"]` 而这里为 false，`;` 就会被次选键 / quick_mix 引导键 /
    /// 标点流水线依次拦下（三条都拦，故只堵一条无用）。首码为 false 同样是硬要求——
    /// 一旦 `;` 能起头，空缓冲按 `;` 就归码表，快捷输入再也进不去。
    /// ⚠️ 必须用 `build_dev/data`（带编译好的词典）而不是源码 `data/`：码元集挂在
    /// **活跃引擎**上，引擎建不起来 → `active_engine()` 为 None → 回落 `a-z` → 断言全红。
    /// 旧的 `shuangpin_final_key` 只读布局 TOML 文件，源 `data/` 就够，这条依赖是新增的。
    #[test]
    fn shuangpin_symbol_final_is_code_char_but_not_leading() {
        let data_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !data_dir.join("schemas/shuangpin.schema.toml").exists() {
            eprintln!("跳过：缺 build_dev/data（未构建）");
            return;
        }

        // override 把内置 shuangpin 方案的布局换成 mspy（`;` = ing），不动真实方案文件。
        let ov_dir = std::env::temp_dir().join("wind_eng_sp_charset_ov");
        let _ = std::fs::remove_dir_all(&ov_dir);
        std::fs::create_dir_all(&ov_dir).unwrap();
        std::fs::write(
            ov_dir.join("shuangpin.toml"),
            "[engine.pinyin.shuangpin]\nlayout = \"mspy\"\n",
        )
        .unwrap();

        let mut cfg = Config::default();
        cfg.schema.active = "shuangpin".into();
        cfg.schema.available = vec!["shuangpin".into()];

        let mgr =
            EngineManager::with_store_override(&cfg, Some(&data_dir), None, Some(ov_dir.clone()));

        // mspy `;` = ing → 码元，但只能作第二码
        assert!(mgr.active_is_code_char(';'), "mspy `;` 应是码元（= ing）");
        assert!(
            !mgr.active_is_leading_char(';'),
            "mspy `;` 只作韵母（第二码），不得进首码集——否则夺走 quick_mix 引导键"
        );
        // 字母键不受影响：仍是码元且可起头
        assert!(mgr.active_is_code_char('k'), "字母 k 应是码元");
        assert!(mgr.active_is_leading_char('n'), "声母 n 应可作首码");
        // 布局没用到的符号不得混进来
        assert!(!mgr.active_is_code_char('['), "mspy 未用 `[`，不应是码元");

        // 反向对照：换回全字母布局（小鹤）后必须回落默认集——否则上面的断言可能只是
        // 「某处无条件把 `;` 放进码元集」，而不是真的从布局推导出来的。
        let ov2 = std::env::temp_dir().join("wind_eng_sp_charset_ov_xh");
        let _ = std::fs::remove_dir_all(&ov2);
        std::fs::create_dir_all(&ov2).unwrap();
        std::fs::write(
            ov2.join("shuangpin.toml"),
            "[engine.pinyin.shuangpin]\nlayout = \"xiaohe\"\n",
        )
        .unwrap();
        let mgr2 =
            EngineManager::with_store_override(&cfg, Some(&data_dir), None, Some(ov2.clone()));
        assert!(
            !mgr2.active_is_code_char(';'),
            "小鹤布局无符号键 → `;` 不得是码元"
        );
        assert!(
            mgr2.active_input_chars().is_default_alpha(),
            "全字母布局应回落内置 a-z"
        );

        let _ = std::fs::remove_dir_all(&ov_dir);
        let _ = std::fs::remove_dir_all(&ov2);
    }

    /// 对照：非双拼方案（codetable，未配 `input_chars`）码元集回落内置 `a-z`，
    /// `;` 不得因为「别的方案用它作韵母」而变成码元——码元集是**方案级**的。
    #[test]
    fn non_shuangpin_schema_keeps_default_alpha_charset() {
        use std::io::Write;

        let base_dir = std::env::temp_dir().join("wind_eng_sp_finalkey_ct_test");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        {
            let mut f = std::fs::File::create(schemas.join("wubi.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"wubi\"\n[engine]\ntype = \"codetable\"\n"
            )
            .unwrap();
        }

        let mut cfg = Config::default();
        cfg.schema.active = "wubi".into();
        cfg.schema.available = vec!["wubi".into()];

        let ov_dir = std::env::temp_dir().join("wind_eng_sp_finalkey_ct_ov");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        assert!(
            !mgr.active_is_code_char(';'),
            "codetable 方案未配 input_chars → `;` 不是码元"
        );
        assert!(mgr.active_is_code_char('k'), "字母仍是码元（回落 a-z）");
        assert!(
            mgr.active_input_chars().is_default_alpha(),
            "未配 input_chars 的方案必须恰好回落内置 a-z"
        );

        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    /// installed_schemas 应返回所有已安装且受支持的方案，不受 available 限制。
    ///
    /// 测试方案：
    ///   - "dummy_active": codetable，active 方案（始终在 available）
    ///   - "sp_installed": pinyin + scheme="shuangpin"，已安装但**未**在 config.available
    ///     → installed_schemas 应包含它，available_schemas 不含它
    ///   - "unsupported_installed": pinyin + scheme="ziranma_xxx"，已安装但不受支持
    ///     → installed_schemas 不含它
    ///   - "ct_installed": codetable，已安装但未在 config.available
    ///     → installed_schemas 应包含它
    #[test]
    fn installed_schemas_includes_all_supported_not_just_available() {
        use std::io::Write;

        let base_dir = std::env::temp_dir().join("wind_eng_installed_schemas_test");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();

        // active（codetable）
        {
            let mut f = std::fs::File::create(schemas.join("dummy_active.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"dummy_active\"\n[engine]\ntype = \"codetable\"\n"
            )
            .unwrap();
        }

        // 已安装双拼（shuangpin），未在 available → 应出现在 installed_schemas
        {
            let mut f = std::fs::File::create(schemas.join("sp_installed.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"sp_installed\"\n[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"shuangpin\"\n"
            )
            .unwrap();
        }

        // 已安装但不受支持（scheme="ziranma_xxx"）→ 应被过滤
        {
            let mut f =
                std::fs::File::create(schemas.join("unsupported_installed.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"unsupported_installed\"\n[engine]\ntype = \"pinyin\"\n[engine.pinyin]\nscheme = \"ziranma_xxx\"\n"
            )
            .unwrap();
        }

        // 已安装 codetable，未在 available → 应出现在 installed_schemas
        {
            let mut f = std::fs::File::create(schemas.join("ct_installed.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"ct_installed\"\n[engine]\ntype = \"codetable\"\n"
            )
            .unwrap();
        }

        // config.available 只含 active，其余方案未启用
        let mut cfg = Config::default();
        cfg.schema.active = "dummy_active".into();
        cfg.schema.available = vec!["dummy_active".into()];

        let ov_dir = std::env::temp_dir().join("wind_eng_installed_schemas_ov");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        let available = mgr.available_schemas();
        let installed = mgr.installed_schemas();

        // available 只含 active，未启用方案不在其中
        assert_eq!(available, vec!["dummy_active".to_string()]);

        // installed 含 active
        assert!(
            installed.contains(&"dummy_active".to_string()),
            "active 应在 installed_schemas 中，实际={installed:?}"
        );

        // installed 含未启用的双拼方案
        assert!(
            installed.contains(&"sp_installed".to_string()),
            "已安装 shuangpin 方案应在 installed_schemas 中，实际={installed:?}"
        );

        // installed 含未启用的 codetable 方案
        assert!(
            installed.contains(&"ct_installed".to_string()),
            "已安装 codetable 方案应在 installed_schemas 中，实际={installed:?}"
        );

        // 不受支持的方案被过滤掉
        assert!(
            !installed.contains(&"unsupported_installed".to_string()),
            "不支持的方案不应在 installed_schemas 中，实际={installed:?}"
        );

        // 结果有序（字典序）
        let mut sorted = installed.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            installed, sorted,
            "installed_schemas 应按字典序排序且无重复"
        );

        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    /// scan_shuangpin_layouts：合并扫描多目录、靠前目录（用户）优先、
    /// 跳过解析失败（缺 [finals]）的布局、按 id 字典序排序。
    #[test]
    fn scan_shuangpin_layouts_merges_user_priority() {
        use std::io::Write;

        let base = std::env::temp_dir().join("wind_eng_sp_layouts_test");
        let _ = std::fs::remove_dir_all(&base);
        let install = base.join("install");
        let user = base.join("user");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&user).unwrap();

        let write_layout =
            |dir: &std::path::Path, file: &str, id: &str, name: &str, finals: bool| {
                let mut f = std::fs::File::create(dir.join(file)).unwrap();
                let finals_sec = if finals {
                    "[finals]\na = [\"a\"]\n"
                } else {
                    ""
                };
                write!(f, "[meta]\nid = \"{id}\"\nname = \"{name}\"\n{finals_sec}").unwrap();
            };

        // 安装目录：xiaohe、mspy
        write_layout(&install, "xiaohe.toml", "xiaohe", "小鹤双拼", true);
        write_layout(&install, "mspy.toml", "mspy", "微软双拼", true);
        // 用户目录：新增 shoudao + 同名覆盖 xiaohe（改显示名）
        write_layout(&user, "shoudao.toml", "shoudao", "手道双拼", true);
        write_layout(&user, "xiaohe.toml", "xiaohe", "小鹤(用户版)", true);
        // 用户目录：损坏布局（缺 [finals]）应被跳过
        write_layout(&user, "broken.toml", "broken", "坏的", false);

        // dirs 顺序：用户优先
        let dirs = vec![user.clone(), install.clone()];
        let got = EngineManager::scan_shuangpin_layouts(&dirs);

        assert_eq!(
            got,
            vec![
                ("xiaohe".to_string(), "小鹤(用户版)".to_string()),
                ("mspy".to_string(), "微软双拼".to_string()),
                ("shoudao".to_string(), "手道双拼".to_string()),
            ],
            "布局枚举应合并、用户优先、跳过损坏、内置方案按流行度排序，实际={got:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Task 1：data_schema_id 拼音族折叠 + 未知方案返回自身 id。
    ///
    /// 策略：用 temp 目录写最小 schema TOML（与既有测试同模式）：
    ///   - "py_test"：engine.type="pinyin" → data_schema_id 应返回 "pinyin"
    ///   - "ct_test"：engine.type="codetable" → 返回自身 "ct_test"
    ///   - "nonexistent"：无此 schema 文件 → schema_engine_type=None → 返回自身
    #[test]
    fn data_schema_id_folds_pinyin_and_returns_self() {
        use std::io::Write;

        let base_dir = std::env::temp_dir().join("wind_eng_data_schema_id_test");
        let schemas = base_dir.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();

        // 拼音方案
        {
            let mut f = std::fs::File::create(schemas.join("py_test.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"py_test\"\n[engine]\ntype = \"pinyin\"\n"
            )
            .unwrap();
        }

        // 码表方案
        {
            let mut f = std::fs::File::create(schemas.join("ct_test.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"ct_test\"\n[engine]\ntype = \"codetable\"\n"
            )
            .unwrap();
        }

        let mut cfg = Config::default();
        cfg.schema.active = "ct_test".into();
        cfg.schema.available = vec!["ct_test".into(), "py_test".into()];

        let ov_dir = std::env::temp_dir().join("wind_eng_data_schema_id_ov");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        // 拼音方案折叠为 "pinyin"
        assert_eq!(
            mgr.data_schema_id("py_test"),
            "pinyin",
            "拼音方案 data_schema_id 应返回 pinyin"
        );

        // 码表方案返回自身 id
        assert_eq!(
            mgr.data_schema_id("ct_test"),
            "ct_test",
            "码表方案 data_schema_id 应返回自身 id"
        );

        // 未知方案（schema_engine_type=None）返回自身 id
        assert_eq!(
            mgr.data_schema_id("nonexistent"),
            "nonexistent",
            "未知方案 data_schema_id 应返回自身 id"
        );

        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    /// P2d Task 1：write_data_schema_id 混输按候选来源分流；非混输忽略 source。
    #[test]
    fn write_data_schema_id_routes_mixed_by_source() {
        use std::io::Write;

        let base_dir = std::env::temp_dir().join("wind_eng_write_data_schema_id_test");
        let schemas = base_dir.join("schemas");
        let _ = std::fs::remove_dir_all(&base_dir);
        std::fs::create_dir_all(&schemas).unwrap();

        // 拼音方案
        {
            let mut f = std::fs::File::create(schemas.join("py_test.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"py_test\"\n[engine]\ntype = \"pinyin\"\n"
            )
            .unwrap();
        }
        // 码表方案
        {
            let mut f = std::fs::File::create(schemas.join("ct_test.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"ct_test\"\n[engine]\ntype = \"codetable\"\n"
            )
            .unwrap();
        }
        // 混输方案
        {
            let mut f = std::fs::File::create(schemas.join("mx_test.schema.toml")).unwrap();
            write!(
                f,
                "[schema]\nid = \"mx_test\"\n[engine]\ntype = \"mixed\"\n[engine.mixed]\nprimary_schema = \"ct_test\"\nsecondary_schema = \"py_test\"\n"
            )
            .unwrap();
        }

        let mut cfg = Config::default();
        cfg.schema.active = "mx_test".into();
        cfg.schema.available = vec!["mx_test".into(), "ct_test".into(), "py_test".into()];

        let ov_dir = std::env::temp_dir().join("wind_eng_write_data_schema_id_ov");
        let _ = std::fs::remove_dir_all(&ov_dir);

        let mgr =
            EngineManager::with_store_override(&cfg, Some(&base_dir), None, Some(ov_dir.clone()));

        // 非混输：忽略 source，等价 data_schema_id
        assert_eq!(
            mgr.write_data_schema_id("py_test", CandidateSource::None),
            Some("pinyin".to_string()),
            "拼音方案忽略 source，折叠为 pinyin"
        );
        assert_eq!(
            mgr.write_data_schema_id("ct_test", CandidateSource::Pinyin),
            Some("ct_test".to_string()),
            "码表方案忽略 source，返回自身 id"
        );

        // 混输：按来源分流
        assert_eq!(
            mgr.write_data_schema_id("mx_test", CandidateSource::CodeTable),
            Some("ct_test".to_string()),
            "混输 + CodeTable → 主码表方案 id"
        );
        assert_eq!(
            mgr.write_data_schema_id("mx_test", CandidateSource::Pinyin),
            Some("pinyin".to_string()),
            "混输 + Pinyin → pinyin"
        );
        assert_eq!(
            mgr.write_data_schema_id("mx_test", CandidateSource::None),
            None,
            "混输 + None → 无法归因，跳过"
        );
        assert_eq!(
            mgr.write_data_schema_id("mx_test", CandidateSource::Phrase),
            None,
            "混输 + Phrase → 无法归因，跳过"
        );
        assert_eq!(
            mgr.write_data_schema_id("mx_test", CandidateSource::English),
            None,
            "混输 + English → 无法归因，跳过"
        );

        // mixed_primary_schema
        assert_eq!(
            mgr.mixed_primary_schema("mx_test"),
            Some("ct_test".to_string()),
            "混输方案的主码表方案 id"
        );
        assert_eq!(
            mgr.mixed_primary_schema("ct_test"),
            None,
            "非混输方案 mixed_primary_schema 返回 None"
        );
        assert_eq!(
            mgr.mixed_primary_schema("nonexistent"),
            None,
            "未知方案 mixed_primary_schema 返回 None"
        );

        let _ = std::fs::remove_dir_all(&base_dir);
        let _ = std::fs::remove_dir_all(&ov_dir);
    }

    #[test]
    fn purge_cache_files_only_removes_cache_extensions() {
        let dir = std::env::temp_dir().join(format!("wind_eng_purge-{}", std::process::id()));
        let sub = dir.join("wubi86");
        std::fs::create_dir_all(&sub).unwrap();
        // 缓存产物（应删）：wdat / 多点 combined.wdat / fp 指纹 / wdb
        std::fs::write(sub.join("main.wdat"), b"x").unwrap();
        std::fs::write(sub.join("main.combined.wdat"), b"x").unwrap();
        std::fs::write(sub.join("main.wdat.fp"), b"x").unwrap();
        std::fs::write(dir.join("unigram.wdb"), b"x").unwrap();
        // 非缓存文件（应留）
        std::fs::write(dir.join("note.txt"), b"x").unwrap();
        std::fs::write(sub.join("raw.dict.yaml"), b"x").unwrap();

        let (mut removed, mut failed) = (0usize, 0usize);
        purge_cache_files(&dir, &mut removed, &mut failed);
        assert_eq!((removed, failed), (4, 0));
        assert!(dir.join("note.txt").exists());
        assert!(sub.join("raw.dict.yaml").exists());
        assert!(!sub.join("main.wdat").exists());
        assert!(!dir.join("unigram.wdb").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
    /// 区间累加的两条规则：字符去重升序，词条数**按条去重**。
    ///
    /// 后者是这段逻辑最容易写错的地方——顺手写成「各字符命中数之和」，一条含两个注音的
    /// 词条就会被数两遍，界面上那个「会影响 N 条词」的数字随即失真。虎码里全角逗号只是
    /// 1 个字符却牵连 326 条词，这种失真用户是察觉不到的。
    #[test]
    fn range_scan_counts_entries_once_per_entry() {
        let (start, end) = (0x3100, 0x312F); // 注音符号
        let mut s = super::RangeScan::default();
        s.tally("\u{3105}", start, end); // ㄅ：命中
        s.tally("\u{3105}\u{3106}", start, end); // ㄅㄆ：两个字符，仍只算一条词条
        s.tally("\u{3105}\u{3105}", start, end); // 同字符两次，也只算一条
        s.tally("\u{6211}", start, end); // 我：区间外，不计
        s.tally("", start, end);
        s.finish();

        assert_eq!(
            s.chars,
            vec!['\u{3105}', '\u{3106}'],
            "字符去重且按码位升序"
        );
        assert_eq!(s.entries, 3, "三条词条命中；若按字符数累加会得到 4");
    }

    /// 扫描**不收空白与控制字符**，哪怕它们落在区间里。
    ///
    /// `ASCII` 块（0020–007F）整块允许批量，而词库里含空格的词条成百上千。不挡的话，
    /// 用户对任一 ASCII 行点「整类设为生僻」就会给**空格**登记一条覆盖——从此含空格的
    /// 候选全判非常用，设置页多出一行看不出是什么的空白，导出写得进去、导入却被拒。
    #[test]
    fn range_scan_skips_unmarkable_chars() {
        let (start, end) = (0x0020, 0x007F); // ASCII
        let mut s = super::RangeScan::default();
        s.tally("a b", start, end); // 命中 a、b，**不收**中间的空格
        s.tally("\t", start, end); // 整条只有制表符 ⇒ 一个都不收
        s.finish();

        assert_eq!(s.chars, vec!['a', 'b'], "空白不该进候选清单");
        assert_eq!(s.entries, 1, "只有第一条真正命中了可登记字符");
    }

    /// 空区间（`charblock` 的「其它」用 `start > end` 表示）不该命中任何东西。
    #[test]
    fn range_scan_empty_range_matches_nothing() {
        let mut s = super::RangeScan::default();
        s.tally("\u{3105}\u{6211}", 1, 0);
        s.finish();
        assert!(s.chars.is_empty() && s.entries == 0);
    }
}
