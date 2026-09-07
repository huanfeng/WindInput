# 重设计差分：config / schema（配置与方案系统）

> 阶段 A 产物（最后一份）。Go 侧 3 个只读 agent 提取、关键论断 grep 抽验 file:line 属实；Rust 侧本人通读。
> 体量：Go `pkg/config` ≈ 3647 行 + `internal/schema` ≈ 3034 行；Rust `wind-config` ≈ 1260 行。
> **schema 是 engine/dict/store 所有质量特性的配置面**——它们的 spec 字段是那些特性的开关，本差分把配置面锁定。

---

## 1. 核心现状

### Schema：两套表示 + 简陋（已 grep 确认死代码）
- `wind-config/schema.rs` 的 `Schema`（70 行）**仅在 lib.rs 导出，无任何跨 crate 使用 = 死脚手架**。`RuntimeState`/`AppCompat` 同样无人用。
- 实际驱动引擎的是 `wind-engine/manager.rs` 的 `SchemaFile`（我在 engine 差分读过），字段极简：engine.type / codetable.{max_code_length,temp_pinyin} / pinyin.scheme / mixed.{primary,secondary,min_pinyin,boost} / dictionaries.{path,type,default,default_enabled} / learning.unigram_path。
- Go 有专门 `internal/schema`：丰富 Spec（CodeTableSpec ~22 字段 / PinyinSpec+Fuzzy 12 标志+Shuangpin / MixedSpec 10 字段 / DictSpec+WeightSpec+Role / EncoderSpec / LearningSpec(AutoLearn/AutoPhrase/Freq)）+ `factory.go`(1694, CreateEngineFromSchema) + SchemaManager + loader(三层合并+override) + learning。

### Config：合并不完整（现状 bug）
- `config.rs` 结构完整（general/schema/hotkeys/input/ui/features/compat/debug），但 `merge_from_file` 是**手写逐字段合并**且漏字段：schema 段漏 primary_codetable/primary_pinyin；input 段只合 6/~15；**features/compat/debug 三段完全不合并** → 用户这些配置静默失效。
- Go 用 yaml.v3 部分覆盖语义（TOML→map→YAML→struct 双序列化），**自动合并所有字段** + 版本迁移 + 保存只写 diff（yamldiff）。

### compat / runtime_state / migration：缺
- Rust app_compat.rs(22)/runtime_state.rs(19) 是 stub；无 migration。Go 有 per-process 兼容规则、运行时状态存取、schema_overrides、版本迁移链。

### 路径：Rust 较好
- `config.rs` 已有 app_dir_name（debug 变体隔离）/user_config_dir(漫游)/local_dir(本机)/cache_dir/log_dir/data_dir——对齐近期 commit 113385f。Go 另有便携模式、datadir.conf 自定义路径、macOS 例外、路径校验。

---

## 2. 决策：统一为一套丰富 Schema 驱动引擎

1. **删死脚手架** `wind-config/schema.rs` 现 Schema；建**一套丰富 Schema**（放 `wind-config` 或新 `wind-schema` crate），字段对齐 Go 的 Spec 体系。
2. `wind-engine` 的 `SchemaFile` 与 `build_engine` 改为消费这套 Schema——**schema 定义与引擎构建解耦**（Go 的 factory 在 schema 包，引擎是被构建对象）。
3. tri-state 字段一律 `Option<bool>`（修 Go 的 plain bool / *bool 混用坏设计）。
4. EngineBundle 的 `interface{}` → Rust **enum**（Pinyin/CodeTable/Mixed）。
5. **合理精简字段**：不照搬 Go 全部 spec 字段——剔除仅为**临时功能兼容**的遗留（如 `auto_commit_unique` 已被 `auto_commit_at_full` 取代）与可能过度设计的架构字段，**只支持实际有意义的**。每个保留字段须对应一个真实特性。

---

## 3. Schema 富 Spec 清单（= engine/dict/store 质量特性的配置面）

> 这些字段就是前几份差分里"缺失能力"的开关。Schema 扩展是阶段 B 质量特性的**前置**：没有 spec 字段就无法配置 auto_commit / fuzzy / freq 参数。

- **CodeTableSpec**（对齐 engine.md §2）：max_code_length / auto_commit_at_full:Option<bool> / auto_commit_min_len / auto_commit_block_on_pinyin / clear_on_empty_max / top_code_commit / punct_commit / show_code_hint / single_code_input / single_code_complete / dedup_candidates / skip_single_char_freq / temp_pinyin / z_key_repeat / **排序两层（见 [frequency.md](./frequency.md)）: base_sort(weight|natural) + user_frequency:bool + freq_strategy(top|step，step 预留)** / **weight_mode / prefix_mode / bucket_limit / short_code_first / charset_preference** / **input_chars（码元字符集，见 §3b）**。
- **PinyinSpec**（对齐 engine.md §1）：scheme(full/shuangpin) / **shuangpin（自定义映射引用，见 §3b——非硬编码内置方案）** / show_code_hint / use_smart_compose / candidate_order / **fuzzy(12 标志: zh_z/ch_c/sh_s/n_l/f_h/r_l/an_ang/en_eng/in_ing/ian_iang/uan_uang + enabled)**。
- **MixedSpec**（对齐 engine.md §3）：primary_schema / secondary_schema / min_pinyin_length(默认2) / codetable_weight_boost(默认1e7) / show_source_hint / enable_abbrev_match / pinyin_only_overflow(默认true) / enable_english / top_code_override_pinyin。
- **DictSpec**（对齐 dict.md）：id/label/path/type(codetable/rime_codetable/rime_pinyin) / default / default_enabled:Option<bool> / enabled:Option<bool> / role(system) / **weight_spec(median/max/min/mode:linear|log/target，归一化上限 10000)** / weight_as_order。
- **EncoderSpec**（造词/编码提示）：rules[{length_equal | length_in_range:[min,max], formula:"AaAbBaBb"}] / max_word_length / exclude_patterns。
- **LearningSpec**（对齐 store.md §3 + [frequency.md](./frequency.md)）：auto_learn{count_threshold:2, min_word_length:2, weight_delta:40, add_weight:800} / auto_phrase{min/max_phrase_len:2/5, idle_timeout_ms:5000, ...} / **freq{enabled, 拼音衰减参数: half_life:72h, base_scale, recency_peak}**（**词频已重构，去掉 boost_max/streak_scale/streak_cap 等 boost-to-weight 旧字段**）/ 码表排序见 CodeTableSpec 的 base_sort+user_frequency / unigram_path / temp_max_entries:5000 / temp_promote_count:5。

> ⚠️ **词频系统已完全重构**，以 [frequency.md](./frequency.md) 为准：词频与权重解耦、只存 {count,last_used}、作排序独立维度（码表 used-first 可选模式 / 拼音衰减分）。FreqSpec 仅保留拼音衰减参数，**单一真值源**（store 默认 + schema 覆盖）——消除旧"两套默认源不一致"。

---

## 3b. 输入方案字符定义（新特性：码元字符集 / 双拼自定义 / 优先级）

> 当前 Rust 把"哪些键是输入码""双拼映射与 `;` 符号"**硬编码**在 coordinator/engine 里。本次改为**方案可配置**。

### 码元字符集 `input_chars`（方案级）
方案声明哪些字符构成"输入码"——决定一次按键是**进输入缓冲**还是作标点/上屏/透传。
- 例：五笔标准 `a-x`；某库还含 `/test` 这类词条 → 配 `a-x/`；虎码等 26 键方案 → `a-z`。
- **取代** coordinator 对 `A-Z`(0x41–0x5A) 的硬编码（见 coordinator.md：按键是否累积进 buffer 改为查方案 `input_chars`）。
- 格式：范围 + 字面集（如 `"a-x/"`、`"a-z"`）。码表词条的合法码元也据此校验（dict 层）。

### 双拼自定义映射（取代硬编码内置方案）
- 现状：Rust `shuangpin.rs` 是 3 行桩；Go 把 ziranma/xiaohe/sogou/mspy **硬编码为内置方案**，映射中的 `;` 等符号也硬编码。
- 目标：双拼方案 = **自定义映射数据**（键位→声母/韵母表 + 该布局使用的符号，如某布局用 `;` 作某韵母），由配置/预置文件提供；**引擎只消费通用映射，不内置具体方案**。
  - 常见布局作为**预置数据文件**随程序发布（默认值），而非代码 enum——用户可改、可加新布局。
  - 布局所用符号是映射的一部分，一并自定义（不再内部硬编码 `;`）。

### 优先级
字符定义（`input_chars` / 双拼映射 / 使用符号）的优先级：**方案配置 > 全局配置 > 内置默认**。原则上**以输入方案配置为主**。

---

## 4. SchemaManager + 工厂 + 三层加载

Go（已核实）：
- `SchemaManager`：load（内置 `exeDir/schemas/` + 用户 `dataDir/schemas/` 同 ID 深拷贝叠加 + `schema_overrides.toml` 全局覆盖按 dict id patch）/ get / list / active；文件 `<id>.schema.toml` 优先 `.yaml`。
- `CreateEngineFromSchema`(factory.go:122) → 按 type 分 createCodeTable/Pinyin/Mixed，返回 `EngineBundle{SchemaID, Engine, SystemLayer, ExtraLayers}`。混输递归构建主码表+次拼音子引擎（spec 优先自身回退 primary/secondary）。

Rust 目标：`SchemaManager`（扫描/合并/激活）+ `build_engine` 消费富 Schema 产出 enum Engine + 层。三层合并（内置/用户/override）。混输递归构建已在 Rust manager.rs 有雏形，保留。

---

## 5. Config 合并 / 保存 / 迁移决策

1. **合并改 deep-merge `toml::Value`**：把默认/系统/用户三层的 `toml::Value` 表深合并，再 `try_into()` **一次性反序列化**——自动覆盖所有字段，**消除手写逐字段合并与漏字段 bug**，无维护负担。（对齐 Go 的"部分覆盖"效果，但 serde 原生、无 TOML→YAML 桥接。）
2. **保存只写 diff**：对比 (默认+系统) base 与当前，仅写差异（对齐 Go yamldiff）。
3. **版本 + 迁移框架从一开始就有**：顶层 `version`，迁移链按版本执行（即使当前无迁移步骤，预留框架）。
4. Go 用户配置可直接读：Go 现在也是 TOML、段结构相近，Rust 大体可直接加载 Go 的 config.toml（schema 文件需适配）——降低老用户迁移成本（与 store bbolt→redb 数据导入是两回事）。

---

## 6. compat / runtime_state / schema_overrides（阶段 C，配合 coordinator）
- **AppCompat**：per-process 规则，按进程名匹配——对应 coordinator.md 的敏感字段/光标定位/应用级行为。**字段级合并**（修 Go 整体替换坏设计）。
  - 已实现：`caret_use_top`、`first_show_mode`、`initial_mode`、`initial_punct`（后两项即「应用独立的初始中英/标点」，语义为初始值而非锁定，字段文档见 `data/compat.toml`）、`host_render`（原 `config.toml` 的 `compat.host_render_processes` 进程名列表已并入本表；白名单现算于 `AppCompat::host_render_processes()`，消费点按事件源 PID 直查 `HostRenderManager::is_process_whitelisted`，**不经** `ActiveCompat` 全局焦点槽缓存，理由见 `host-render-windows-port.md` §11.2/§11.7）。
  - 待接线：`skip_caret_pending`。
  - 已删除：`pin_candidate_position`（长期无消费点；该能力由 `ui.candidate.position_mode = "fixed"` 一套实现）。
    ⚠️ 其配套存储位 `RuntimeState::candidate_pin_positions` 当时被漏下，2026-09-07 一并删除——
    开关删了而存储位留着，会让人误以为「候选窗有按显示器分屏的位置记忆」（实际没有）。
- **RuntimeState**：last 中英文/全半角/标点、引擎类型、工具栏/软键盘锚点（按显示器 key 分桶）；与 `remember_last_state` 的关系（位置类始终持久化）。候选窗固定位**不在此**，走配置 `ui.candidate.custom_x/custom_y`（全局单坐标，不分显示器）。
- **schema_overrides**：每方案覆盖全局配置项——Rust 用**类型化**覆盖（修 Go 的 `map[string]any` 无校验 + 合并散落调用方）。

---

## 7. 路径（保留 Rust，补两项）
- 保留 Rust 现有漫游/本机/缓存/日志分离 + debug 变体隔离（近期成果）。
- 补：**便携模式**（exe 旁 marker → userdata）、**自定义数据目录**（datadir.conf）、路径合法性校验（设置界面用）——阶段 D。
- Rust 不需要 Go 的 legacy `.yaml` 双读回退（Rust 全程 TOML）。

---

## 8. Go 坏设计（不照搬）
1. TOML→map→YAML→struct 双序列化桥接 → Rust deep-merge toml::Value 一次反序列化。
2. plain bool 与 *bool 混用（无法区分未设置/false）→ 一律 Option<bool>。
3. EngineBundle.Engine `interface{}` 需类型断言 → enum。
4. createMixedEngine 380 行 + 重复"自身 spec 否则回退 primary" → 抽 helper/merge。
5. deepCopySchema 一律走 YAML → Rust `#[derive(Clone)]` 原生。
6. schema_overrides `map[string]any` 无类型 + 合并在调用方 → 类型化覆盖。
7. compat 同进程整体替换（丢 base 字段）→ 字段级合并。
8. 词频默认值两套源不一致 → 唯一真值源。
9. PagerBarDisplay 用 "" 作合法枚举（未设置/空三态不分）→ Option 或显式 variant。
10. keypaths 生成的 ident 不做 initialism（UiThemeName）→ Rust N/A。

## 9. Rust 现状要保留的优点
- 路径方案（漫游/本机/缓存/日志 + debug 变体）——近期扎实成果。
- 全程 TOML（无 Go 的 legacy YAML 双读包袱）。
- hotkey.rs 编译（CompiledHotkeys/Compiler/parse_hotkey/select_key_vks 较完整）。

---

## 10. 落地顺序 + 跨文档依赖
1. **Config 合并修复**（§5.1）：deep-merge toml::Value——**快、高价值**（修用户配置静默失效），尽早做。
2. **统一富 Schema + 删死脚手架**（§2/§3）：建丰富 Schema 类型 + 引擎消费它。**这是阶段 B 质量特性的前置**——每落地一个 engine/dict/store 特性，同步加它的 spec 字段（auto_commit/fuzzy/weight_mode/freq 参数…）。
3. **版本+迁移框架**（§5.3）：随 1 一起。
4. **compat/runtime_state/schema_overrides**（§6）：阶段 C，配合 coordinator pipeline。
5. **便携/datadir/校验**（§7）：阶段 D。

> 与 engine/dict/store/coordinator 四份差分共同构成阶段 A 全图：schema 是配置面、engine/dict/store 是质量核心、coordinator 是交互统一。每步 `wind_input/scripts/dev.sh ci` 把关。
