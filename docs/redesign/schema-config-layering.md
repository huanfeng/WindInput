# 方案配置分层重构设计

> **2026-07-14 修订（简化）**：码表行为覆盖不再走独立的 `[codetable]` 扁平段 + `SchemeOverride`/
> `CodetableOverride` 结构与 `enabled` 总开关。改为——行为字段回到 `CodeTableSpec`（`[engine.codetable]`）
> 作 tri-state `Option`，与引擎固定参数**同段同构**：方案作者可在 `.schema.toml` 内联行为基线；
> `schema_overrides/{id}.toml` 用**相同的 `[engine.codetable]` 段**（设置页写入）经 `read_schema` 的
> `merge_toml` 深合并覆盖；最终 `CodetableGlobal::resolved(&CodeTableSpec)` 一次折叠到全局基线
> （`Some` 覆盖 / `None` 回落）。收益：override 与方案文件格式一致（可直接抄段改值）、隐藏/内置特殊模式
> 方案（如 `quick_symbols`）行为自包含无需 override 文件、去掉恒为 true 的 `enabled` 仪式。
> 同时 `[[schema.special_modes]]` 瘦身为 `{ schema, trigger_keys }`，`id/name/short_name` 从被引用方案
> 文件派生（`schema_name` / `schema_icon_label` / `effective_id`），消除与方案文件的重复。
>
> 状态：**已实现并通过全量测试**（wind-config/engine/coordinator 改造 + data 清理；build/test/clippy/fmt 绿）。
> 本项目未发布，**不考虑旧配置迁移**。
> 实现补记：`temp_max_entries` 经核查为**完全未接入的死配置**（`evict_temp_words` 无生产调用方），
> 故仅从配置删除，store 侧无需常量化；`single_code_input/complete` 已接入 `wind-engine` 码表引擎。
> 关联：`docs/redesign/config-schema.md`（配置总览）、`docs/redesign/frequency.md`（调频）、
> `wind-config/src/{config,schema,config_schema}.rs`、`wind-engine/src/manager.rs`。

## 1. 目标

把方案（码表 / 拼音 / 混输）的配置项按**性质**分离，并对"用户可配"部分做分层：

1. `.schema.toml` 只保留方案设计者固化的**引擎参数**，不再承载任何用户可配项。
2. 用户可配项的**全局公共默认**收拢到 `config.toml` 的 `schema.codetable` / `schema.pinyin` / `schema.mix`
   （与已存在的 `schema.pinyin` 模式对称）。
3. **方案个性覆盖仅码表支持**，走已有的 `schema_overrides/{id}.toml`，由方案内一个**总开关**控制是否生效。
   拼音与混输**不支持**方案 override，只用全局。
4. 用不同结构体表达不同配置性质（引擎固定 / 全局行为 / 方案覆盖 / 调频造词）。

## 2. 三层物理结构

```
.schema.toml ──────────────── 引擎固定（清空所有用户可配项，且代码不再读取这些键）
config.toml ───────────────── 全局公共  [schema.codetable] / [schema.pinyin] / [schema.mix]
schema_overrides/{id}.toml ── 方案覆盖（仅码表；带开关；设置页 saveConfig 写入；深合并到 base schema）
```

- `schema_overrides/` 机制已存在：`%APPDATA%/WindInput/schema_overrides/{id}.toml`，
  读方案时深合并到基础 `.schema.toml` 之上（见 `wind-engine/src/manager.rs::read_schema`）。
  本次后该目录只承载：**码表行为覆盖**（带开关）、**词库启停**、**双拼布局**。
- 全局层走三层合并（代码默认 L1 → `data/config.toml` L2 → 用户 `config.toml` L3），现状已支持。

## 3. 字段完整分类

### 3.1 码表（codetable）

**全局行为**（`schema.codetable` + 方案 override，tri-state `Option<bool>`）：

| 字段 | 含义 |
|------|------|
| `top_code_commit` | 顶码上屏 |
| `clear_on_empty_max` | 满码空码清空 |
| `auto_commit_at_full` | 满码唯一自动上屏 |
| `auto_commit_min_len` | 自动上屏最短码长（**隐藏参数**，默认 0 = 等于全码长；不在设置 UI 暴露） |
| `punct_commit` | 标点顶码上屏 |
| `show_code_hint` | 显示编码提示 |
| `single_code_input` | 精确匹配模式（不前缀匹配） |
| `single_code_complete` | 精确匹配空码补全 |
| `z_key_repeat` | z 键重复输入 |
| `z_key_action` | z 键功能（进哪个模式）；`String` 而非 `bool`，值域见下 |

> `user_frequency` 已**删除**：调频统一由 `schema.codetable.frequency.enabled` 控制（§3.4）。

> **`z_key_action` 为什么在方案级**：字母天然是编码键，能否借作引导键取决于**这张码表里
> 它是不是死码**（五笔 86 的 z 是，别的码表未必）。这是方案的属性——全局
> `input.temp_pinyin.trigger_keys` 无从表达，配了字母就是所有方案里无条件抢键。故字母引导键
> 已从各处 `trigger_keys` 移除（只认符号），能力收归本项，存量配置在加载期迁移
> （`Config::migrate_letter_trigger_keys`）。
>
> 值域：`""`/`none` / `temp_pinyin` / `temp_english` / `mix:<id>` / `special:<id>`。
> 与 `z_key_repeat` **正交**（可同时开）：repeat 先手，用户继续打字母才轮到本项。

**引擎固定**（留 `.schema.toml`）：

| 字段 | 含义 |
|------|------|
| `max_code_length` | 最大码长 |
| `base_sort` | 基础排序 weight/natural |
| `input_chars` | 码元字符集 |
| `[engine.chaizi]` | 拆字字体/库（当前硬编码忽略，另案处理，本次不动） |
| `[[dictionaries]]` 元信息 | id/label/description/path/type/default/weight_as_order/weight_spec |
| `[encoder]` | 造词编码规则 |

**删除**（legacy / 未实现）：
- `auto_commit_unique`（被 auto_commit_at_full 取代）
- `candidate_sort_mode`（被 base_sort + frequency 取代）
- `user_frequency`（并入 frequency.enabled）
- `short_code_first`（阶段 B 预留但未实现，删除，将来需要再加）
- `weight_mode` / `prefix_mode` / `charset_preference`（阶段 B 预留但未对接，全删；将来接入再加）
- `freq_strategy` 从码表移入 `schema.codetable.frequency.strategy`（§3.4）

### 3.2 拼音（pinyin）

**全局唯一**（`schema.pinyin`，**无方案 override**）：

| 字段 | 含义 |
|------|------|
| `show_code_hint` | 显示编码提示 |
| `use_smart_compose` | 智能组字 |
| `separator` | 拼音分隔策略（**已改为可被方案覆盖**，见下方 override 清单） |
| `fuzzy.*`（11 项） | 模糊音 |

> `candidate_order`：**删除**。当前未接入引擎（`PinyinEngine::Config` 有字段，注释"后续阶段接入"，
> 全工程无消费；排序实际由 unigram + 词频决定）。将来要做智能排序再加回。

**引擎固定**（留 `.schema.toml`）：`scheme`(full/shuangpin)、`weight_spec`、`unigram_path`（置于 `[engine.pinyin]`，属解码/语言模型，非学习）、dicts。

**方案 override（这三项，始终生效、无行为开关）**：
- `shuangpin.layout`（决策 P1-a：单个双拼方案 + 可配布局）
- 词库启停 `[[dictionaries]] enabled`
- `[engine.aux_code]`（辅助码，2026-08-19 补）：`files` 是方案属性（全拼配笔画、双拼配
  小鹤形码），`enabled` / `max_phrase_len` 是 tri-state，折叠全局 `[schema.pinyin.aux_code]`
  （`AuxCodeGlobal::resolved`，与 `CodetableGlobal::resolved` 同构）。
  > **为什么拼音破了「无方案 override」这条**：全拼与双拼在辅助码上不是偏好不同，
  > 而是**键位预算不同**——全拼的反引号出厂已被音节分隔符占用
  > （`separator = "auto"` + `'` 作选词键），且分隔符臂在按键分派里位于 `[key_actions]`
  > 裁决之前，绑了也进不去；双拼则 `pinyin_separator_key` 恒早退、反引号是自由键。
  > 「双拼开、全拼关」由此成为常态需求，全局唯一的开关表达不了。
  > 这与 `shuangpin.layout` 破例的性质相同：**方案的编码规则决定了它，不是用户的偏好**。
- `[engine.pinyin].separator`（手动音节分隔符，2026-08-30 补）：tri-state，`None`/空串
  回落全局 `[schema.pinyin].separator`（`EngineManager::pinyin_separator_mode_of`）。
  出厂方案一律不声明，由 `builtin_pinyin_schemas_do_not_declare_separator` 守门。
  > **它与上一条是同一条论证的另一半**。上面写着「全拼的反引号已被分隔符占用，故辅助码
  > 要能按方案开关」——那时 `separator` 还是全局唯一的，于是只能单向让步：辅助码躲开
  > 分隔符。真正的争用双方是「哪个功能拿到反引号」，而**双方都只能按方案回答**
  > （双拼把韵母塞进字母键、符号键空闲；全拼的音节边界只能靠符号键表达）。
  > 只让一方可配，另一方就永远是那个被夺走键位的：用户把全局 `separator` 设成
  > `backtick`，双拼出厂的 `backtick = "aux_code"` 当场失效，且只剩一条 warn 日志。

### 3.3 混输（mix）

混输是"两引擎组合器"，配置面只含**融合策略**；调频/造词/词库走被引用子方案。

**全局唯一**（`schema.mix`，**无方案 override**——与拼音一致）：

| 字段 | 含义 |
|------|------|
| `show_source_hint` | 显示来源标记 |
| `enable_english` | 启用英文候选（原悬空 tri-state，补全全局回退） |
| `pinyin_only_overflow` | 超码长仅查拼音（原悬空 tri-state） |
| `top_code_override_pinyin` | 顶码偏好（原悬空 tri-state） |
| `auto_commit_block_on_pinyin` | 满码上屏遇拼音否决（决策 F：从码表移入） |
| `min_pinyin_length` | 拼音最小触发长度（决策 M1：归用户配置） |

**引擎固定**（留 `.schema.toml`）：`primary_schema`、`secondary_schema`、`codetable_weight_boost`。

**继承主码表**：码表类行为（top_code_commit、z_key_repeat 等）不在 mix 重复，走主码表 `schema.codetable`
（决策 M2：删掉 mix 自己的 `z_key_repeat`）。混输无方案 override，但其主码表子方案仍各自走自己的 override。

### 3.4 调频 + 自动造词（全局唯一，按引擎分；决策 D / 1）

不做方案级。码表与拼音字段集不同（已由 FreqSpec 现状佐证：protect_top_n 仅码表、half_life/base_scale/recency_peak 仅拼音）：

| 全局键 | 字段 |
|--------|------|
| `schema.codetable.frequency` | `enabled` / `protect_top_n` / `strategy`（top/step，原 freq_strategy 迁入） |
| `schema.codetable.auto_phrase` | `enabled` / `min_phrase_len` / `max_phrase_len` / `add_weight` / `weight_delta` / `count_threshold` / `idle_timeout_ms` / `promote_count`(原 temp_promote_count) |
| `schema.pinyin.frequency` | `enabled` / `half_life` / `base_scale` / `recency_peak` |
| `schema.pinyin.auto_learn` | `enabled` / `count_threshold` / `min_word_length` / `weight_delta` / `add_weight` / `promote_count`(原 temp_promote_count) |

混输无自己的调频/造词。`.schema.toml` 内 `[learning]` 整段删除（`LearningSpec` 移除；`unigram_path` 迁至 `[engine.pinyin]`）。

**临时词层参数**：
- `temp_promote_count`（晋升所需使用次数）→ 用户配置，移入对应引擎的自动造词配置（`auto_phrase.promote_count` / `auto_learn.promote_count`）。
- `temp_max_entries`（临时库容量上限）→ **降为 store 层常量**，移出配置（类比已硬编码的 `TEMP_WORD_MAX_WEIGHT`）。
- `unigram_path`（语言模型路径）是引擎参数，迁至 `[engine.pinyin]`（原 `[learning]`，因属解码非学习）；`LearningSpec` 随之整体删除。

### 3.5 临时拼音（决策 E）

全局唯一 `input.temp_pinyin.*`（现状已有 `trigger_keys`，补 `enabled` / `schema`）。不下放方案。
**删除方案级 `[engine.codetable.temp_pinyin]`**（当前 `manager.rs::temp_pinyin_target()` 读它，
需改为读全局 `input.temp_pinyin.{enabled,schema}`）。

### 3.6 词库启停

`[[dictionaries]] enabled` → 方案 override 始终生效（深合并 `Schema.dictionaries`，不受码表行为开关管）。
`default_enabled` 作为出厂默认留 `.schema.toml`。

## 4. 取值解析链

```
# 码表（活跃方案为码表，如 wubi86）—— 唯一支持方案 override 的引擎
if override[{id}].codetable.enabled == true:
    f = override.codetable.<f>  ??  config.schema.codetable.<f>  ??  代码兜底
else:
    f = config.schema.codetable.<f>  ??  代码兜底

# 拼音（无方案 override）
f = config.schema.pinyin.<f>  ??  代码兜底

# 混输（无方案 override）
f = config.schema.mix.<f>  ??  代码兜底
# 码表类行为继承主码表 config.schema.codetable（主码表自身仍可有 override）

# 调频 / 造词（不看方案）
codetable: config.schema.codetable.{frequency,auto_phrase}
pinyin:    config.schema.pinyin.{frequency,auto_learn}

# 临时拼音（不看方案）
input.temp_pinyin.*
```

开关只管码表的"行为字段"；词库启停、双拼布局这类 override 始终生效。
决策 B 让 `.schema.toml` 无行为默认，故解析塌缩成两层，无"作者默认 vs 用户全局"优先级冲突。

## 5. 结构体设计（Rust，wind-config）

```rust
// ── 全局公共（config.rs，进 Config.schema）──
struct SchemaConfig {
    active: String, available: Vec<String>,
    primary_codetable: String, primary_pinyin: String,
    codetable: CodetableGlobal,   // 新增
    pinyin:    PinyinGlobal,      // 现 PinyinGlobalConfig，去 candidate_order，扩 frequency/auto_learn
    mix:       MixGlobal,         // 新增（全局唯一，无 override）
    quick_input: QuickInputConfig,
    special_modes: Vec<SpecialModeConfig>,
    mix_modes: Vec<MixModeConfig>,
}

struct CodetableGlobal {
    // 行为：全局是基线，用绝对值 bool（非 tri-state）
    top_code_commit: bool, clear_on_empty_max: bool, auto_commit_at_full: bool,
    auto_commit_min_len: usize,   // 隐藏参数，默认 0=全码长
    punct_commit: bool, show_code_hint: bool,
    single_code_input: bool, single_code_complete: bool, z_key_repeat: bool,
    z_key_action: String,            // ""/none/temp_pinyin/temp_english/mix:<id>/special:<id>
    frequency: CodetableFrequency,   // enabled / protect_top_n / strategy
    auto_phrase: AutoPhraseConfig,   // …含 promote_count
}

struct MixGlobal {
    show_source_hint: bool, enable_english: bool, pinyin_only_overflow: bool,
    top_code_override_pinyin: bool, auto_commit_block_on_pinyin: bool,
    min_pinyin_length: usize,
}

struct PinyinGlobal {  // 现 PinyinGlobalConfig 改造
    show_code_hint: bool, use_smart_compose: bool,
    separator: String, fuzzy: PinyinFuzzy,   // 删 candidate_order
    frequency: PinyinFrequency,    // enabled / half_life / base_scale / recency_peak
    auto_learn: AutoLearnConfig,   // …含 promote_count
}

// ── 方案覆盖（新结构，仅码表；从 schema_overrides/{id}.toml 解析，不进 Config/注册表）──
struct SchemeOverride {
    codetable: Option<CodetableOverride>,   // enabled 开关 + 各行为 Option<bool>
    pinyin: Option<PinyinOverride>,         // 仅 shuangpin.layout（始终生效）
    dictionaries: Vec<DictOverride>,        // id + enabled
}

struct CodetableOverride {
    enabled: bool,                  // 总开关：false 则全部忽略，回落全局
    top_code_commit: Option<bool>, /* …其余行为字段全 Option，无 frequency/auto_phrase（那是全局） */
}

// ── 引擎固定（schema.rs，瘦身后的 Schema）──
//   CodeTableSpec 删除全部行为字段 + freq_strategy + short_code_first + temp_pinyin
//     + weight_mode/prefix_mode/charset_preference，仅留 max_code_length/base_sort/input_chars/chaizi(待)。
//   PinyinSpec 删除 engine.filter_mode（顶层 EngineSpec.filter_mode 一并删）。
//   MixedSpec 仅留 primary/secondary/codetable_weight_boost。
//   LearningSpec 整体删除（freq/auto_learn/auto_phrase/temp_* 上移全局；unigram_path 迁入 PinyinSpec）。
```

## 6. 实现影响点

1. **`config_schema.rs` 注册表**：新增约 30+ 个 `schema.codetable.* / schema.mix.* / schema.pinyin.frequency.* /
   schema.pinyin.auto_learn.* / schema.codetable.{frequency,auto_phrase}.*` 键，并同步 `data/config.toml`
   （否则 `registry_covers_every_config_key` / `data_config_toml_has_no_orphan_keys` 两个测试红）。
   方案 override（`SchemeOverride`）不进此注册表，需独立校验。
2. **`schema.rs`**：`CodeTableSpec` / `PinyinSpec` / `EngineSpec`（删 filter_mode）/ `MixedSpec` / `LearningSpec` 删字段；新增 `SchemeOverride` 系列结构。
3. **`wind-engine/src/manager.rs`**：
   - `read_schema` 深合并逻辑：行为字段已不在 `Schema`，改为单独解析码表 `SchemeOverride` 并与全局 `Config.schema.codetable` 合成 effective 配置。
   - `freq_cache`（`schema_id -> FreqSettings`）改为按引擎类型（码表/拼音）取全局，不再 per-schema。
   - `code_commit: Mutex<CodeCommitConfig>` 缓存改为 `schema.codetable` 全局。
   - `temp_pinyin_target()`（约 manager.rs:834）改读全局 `input.temp_pinyin`，不再读 `schema.engine.codetable.temp_pinyin`。
   - pinyin 引擎 `Config` 删 `candidate_order` / `filter_mode`。
4. **`config.rs`**：删 `CodeCommitConfig`（字段并入 `CodetableGlobal`）；`PinyinGlobalConfig` 改造；新增 `CodetableGlobal` / `MixGlobal`。
5. **`data/config.toml`**：写入 `[schema.codetable]` / `[schema.mix]` / 各 frequency/auto_* 段出厂默认
   （含 wubi86 偏好，如 top_code_commit=true——wubi86 是主力码表方案，其偏好即全局默认）。
6. **`data/schemas/*.schema.toml`**：删除所有迁出字段（行为 / learning / engine.filter_mode / freq_strategy 等）。
7. **`wind-store`**：`temp_max_entries` 降为常量。
8. **设置 UI（wind_setting_native）**：码表分"全局公共 + 方案覆盖（带开关）"两区；拼音/混输仅全局区。
9. **legacy 清理**：`auto_commit_unique` / `candidate_sort_mode` / `user_frequency` / `short_code_first` / `candidate_order` 删除。

## 7. 实现期再核对（非阻塞）

无。所有字段归属已敲定，可直接进入 §8 实现。

## 8. 实现顺序建议

1. wind-config 结构体改造（config.rs 新增全局结构 + schema.rs 瘦身 + 新增 SchemeOverride）。
2. config_schema.rs 注册表 + data/config.toml 同步（先让两个对照测试绿）。
3. data/schemas/*.schema.toml 清理迁出字段。
4. wind-engine manager.rs 解析链改造（effective 配置合成 + freq_cache/code_commit 缓存口径）。
5. wind-store temp_max_entries 常量化。
6. 设置 UI 适配（码表全局区+方案覆盖区 / 拼音·混输仅全局区）。
7. 全量 `wind_input/scripts/dev.sh ci` 把关。
