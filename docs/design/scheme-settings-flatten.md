# 方案设置扁平化：取消类型级全局配置

状态：阶段 1–4 完成，阶段 5 部分完成（移动端门面已接别名）。分支 `feat/scheme-settings`。
取代：`docs/redesign/schema-config-layering.md`（三层结构）中的「全局公共」一层。

## 1 问题

码表 / 拼音 / 混输 / 英文各有一段「类型级全局配置」（`config.toml` 的 `schema.codetable` /
`schema.pinyin` / `schema.mix` / `schema.english`），供同类方案共享；方案再用三态字段逐项覆盖。
一个码表行为值的取值链因此是六层：

```
代码默认 → data/config.toml → data_custom → 用户 config.toml → 方案文件 → schema_overrides/<id>.toml
```

为维持它付出的代价：

- 方案侧整套 `Option` 三态字段与手写 `resolved()`（`CodeTableSpec` 及调频、英文混入子段、`AuxCodeSpec`）；
- 两张靠人工对齐的表（`SCHEMA_OVERRIDES`、`schema_overridden_keys`）；
- 三份快照（`effectiveCodetable` / `followedCodetable` / `codetableOverride`）与 `schema.overrideSummary`；
- 设置端的三态勾选行、「已被 N 个方案覆盖」角标、卡顶「此为全局」提示；
- 为绕开共享打的补丁：拼音 `aux_code` / `separator` 的破例覆盖、`english_merge.exact_position = 0`
  的「自动（全拼 1、双拼 2）」哨兵、两条「内置方案不得声明某项」的守门测试、overlay 方案的
  `special_schema_baseline()`。

全拼与双拼共用 `schema.pinyin` 是最常见的混乱来源：两者键位预算不同，却只能设一份。

## 2 目标模型

```
类型内置默认（代码，不可配） → 方案文件（方案默认） → schema_overrides/<id>.toml（用户的方案设置）
```

- 用户可见两层：**方案默认**与**用户配置**。
- 类型内置默认只为「方案文件没写的字段」兜底。它就是只读出厂文件 `data/config.toml` 里的
  `[schema.codetable]` / `[schema.pinyin]` / `[schema.mix]` / `[schema.english]` 四段：这四段
  **只从出厂层（L1 ⊕ L2）读**，定制层与用户层写了也不生效。取值不变，第三方方案升级后行为
  不变；注册表、`data/config.toml` 列全门禁、结构体零值（大量测试靠它）都不用动。
- overlay 方案今天已经是这个模型（`special_schema_baseline`），本次把它推广到全部方案。

### 2.1 不变的部分

- 混输的码表行为取主成员方案的设置、拼音行为取次成员方案的设置。这是**引用**，不是层级。
- 方案对 `ui.candidate.*` / `input.punct.*` / 注释模板的覆盖：它们跟随的是真全局，不是类型层，
  「跟随全局（值）」的呈现（R8）继续适用。
- 词库启停（`[[dictionaries]] enabled`）仍按方案各存各的。
- 全拼 / 双拼 / 临时拼音共用 `pinyin` 数据桶。

## 3 字段去向

### 3.1 码表

`schema.codetable.*` 全部下沉到方案的 `[engine.codetable]`（含 `frequency` / `english_merge`
两个已有子段）；`auto_phrase` 子段补进方案（此前没有方案级形态）。

### 3.2 拼音

全拼、双拼各自独立，`schema.pinyin.*` 下沉到方案的 `[engine.pinyin]`：

| 块 | 字段 |
|---|---|
| 整句 | `use_smart_compose` `sentence_uses_user_words` `sentence_count` `sentence_max_count` `sentence_cycle_key` |
| 基本 | `separator` `single_char` `abbrev.enabled` |
| 补全 | `completion.*` |
| 英文混入 | `english_merge.*` |
| 调频 | `frequency.*` |
| 自动造词 | `auto_learn.*` |
| 语法模型 | `grammar.*` |
| 双拼专有 | `shuangpin.allow_full_pinyin`（只对双拼方案有意义） |

辅助码已在 `[engine.aux_code]`，只是基线从全局改为内置默认。

调频 / 自动造词 / 语法模型按方案分离，便于并排测试多套模型参数。数据桶仍共用：读侧参数
（衰减、语法权重）按「正在用哪个方案」取，写侧开关（是否造词）按「上屏时的方案」取。

**拼音族共用的只剩模糊音**，挪到 `input.fuzzy.*`，与 `input.correction` 并列（真全局键）。

分离后 `english_merge.exact_position = 0` 的自动哨兵取消，双拼方案文件直接写 `2`；
`factory_schemas_do_not_declare_single_char`、`builtin_pinyin_schemas_do_not_declare_separator`
两条守门测试删除。

### 3.3 混输、英文

- `schema.mix.*` → 混输方案的 `[engine.mixed]`。`schema.mix_modes`（临时融合模式的实例集合）不动。
- `schema.english.*` → 英文方案新增的 `[engine.english]`。临时英文取 `english` 方案的设置。

### 3.4 保持全局的 `schema.*`

`active` `available` `primary_codetable` `primary_pinyin` `keep_all_loaded` `dict_cache_policy`
`mix_modes` `quick_input.*` `frequency.exclude_blocks`。

## 4 旧键：别名

用户 `config.toml` 不再存这四段（`Config` 里的同名字段只装出厂层的值，充当类型内置默认）。
旧键在注册表里保留，只在**按键寻址**的读写口转址：

| 入口 | 位置 |
|---|---|
| 命令栏 `config.get` / `config.set` / `config.toggle` | `wind-coordinator/src/handle_cmdbar.rs` |
| RPC `config.getItem` / `config.setItems` | `wind-rpc/src/dispatch.rs` |
| 移动端（阶段 5 接入） | `wind-mobile/src/config_api.rs` |

RPC 一侧经内部方法 `schema.aliasGet` / `schema.aliasSet` 转到宿主（`wind-webdata`），映射在
`config_schema::scheme_alias`，目标方案在 `EngineManager::scheme_alias_target`。

整份配置的读取不含这些段——避免「方案值被当成用户全局值写回」。

### 4.1 别名指向谁

> 当前活跃方案属于该类型就指它；否则指该类型的主方案。

| 旧键前缀 | 活跃方案是… | 目标 |
|---|---|---|
| `schema.codetable.` | 码表 | 活跃方案 |
| | 混输 | 它的主成员（码表）方案 |
| | 其它 | `primary_codetable` 解析结果 |
| `schema.pinyin.` | 拼音 / 双拼 | 活跃方案 |
| | 混输 | 它的次成员（拼音）方案 |
| | 其它 | `primary_pinyin`（空 = `pinyin`） |
| `schema.mix.` | 混输 | 活跃方案 |
| | 其它 | `available` 里第一个混输方案 |
| `schema.english.` | 英文类方案 | 活跃方案 |
| | 其它 | `english` |
| `schema.pinyin.fuzzy.` | 任意 | `input.fuzzy.`（静态改名，不看方案） |

找不到目标方案时，写返回错误、读返回类型内置默认。

写入落到目标方案的 `schema_overrides/<id>.toml`，走与 `schema.saveConfig` 相同的持久化与
失效路径。

### 4.2 一次性迁移

`Config::load` 把用户层里的四段摘下来（模糊音留在原处），放进 `Config::legacy_type_settings`，
不参与合并。`EngineManager` 在构造与重载时见它非空就搬（`migrate_legacy_type_settings`）：

1. 对每个已安装、非 overlay 的同类方案，把用户写过的每个键写进它的 `schema_overrides`；
   该方案自己已有值的键跳过（方案文件里写的和用户设置里写的都算）；
2. 搬完从用户 `config.toml` 删掉这四段（`Config::clear_legacy_type_settings`）。

用户层只存与出厂不同的值，所以「用户写过的键」就是他改过的项。搬完后每个方案的生效值与搬
之前逐项相同；之后新装的方案不再继承这些值。幂等：删除失败时下次启动重搬，各项都已有值。

`data_custom/config.toml` 里的这四段**不迁移、不读取**，加载时告警一句：定制者把值写进
`data_custom` 下的方案文件。文档站的定制说明同步更新。

模糊音改名 `input.fuzzy.*`：各层加载时把 `[schema.pinyin.fuzzy]` 搬过去（`migrate_fuzzy_value`，
本层已写新键的项不覆盖）；按键寻址的入口先过 `config_schema::canonical_key`，旧键名照常可用。
引擎仍从 `PinyinGlobalConfig::fuzzy` 取，它不进配置文件，由 `Config::pinyin_effective` 填入。

## 5 方案间复制

方案设置按块组织（§3.2 的表即拼音的分块；码表按 上屏 / 匹配 / 通配 / 整句 / 调频 / 自动造词 /
英文混入）。每块提供「复制到其它方案」：一次性把本块的生效值写进所选同类方案的用户层。
不建立任何跟随关系。

## 6 设置端

实施要点（阶段 4）：方案对话框「方案设置」页四类方案通用，经 `schema.getSettings` /
`schema.setSettings` 读写；「复制到…」按块（按键路径划分）排进目标方案的待提交改动，随保存提交；
向导的方案级几项在切换默认方案之后按旧键（别名）写；引擎卡「恢复默认」叠加方案默认值。

- 清单驱动的引擎卡渲染（`scheme_config_section`）改为绑定到一个方案 id，读写走
  `schema.getConfig` / `schema.saveConfig`。拼音、混输、英文由此直接获得方案级编辑。
- 方案页的码表 / 拼音 / 双拼 / 混输 / 英文卡保留，标题写明方案（「码表 · 五笔86」）；同类
  方案多于一个时带切换下拉，默认选主方案。
- 方案对话框的「方案覆盖」改名「方案设置」，内嵌同一个组件。
- 三态勾选行换成普通控件；改过的项带标记，可逐项或整块「恢复方案默认」。
- 删除类型层角标、卡顶「此为全局」提示、`schema.overrideSummary` 中的类型层内容。
- 向导改为写目标方案。

## 7 实施阶段

先加后减：前两步只增加「按方案取值」的能力，类型段原样作基线，现有行为与测试不变；第三步
才把基线从配置文件换成内置默认。

| 阶段 | 内容 | 验收 |
|---|---|---|
| 1 | 拼音：`[engine.pinyin]` 收设置项，通用折叠（`scheme_fold`），管理器与协调器按方案取值 | 全量测试绿；全拼、双拼可各设一份 |
| 2 | 混输 `[engine.mixed]`、英文 `[engine.english]`、码表 `auto_phrase` 同法下沉 | 同上 |
| 3 | 翻转：别名转址；四段不再从用户层与定制层读、一次性迁移；模糊音改 `input.fuzzy`；删类型层登记表与 `followed*` / `overrideSummary` 的类型层内容；码表的三态字段与手写 `resolved()` 换成通用折叠 | 迁移单测：单方案 / 多方案 / 已有覆盖 / overlay 各一例；旧键读写可用；内置方案生效值与改造前逐项相同 |
| 4 | 设置端 | 渲染与写回测试；截图核对 |
| 5 | 移动端、备份与方案包、同步、文档站、规则文档 | 各自门禁 |

出厂行为不应变化。若某阶段导致候选有无、先后或上屏时机改变，在
`docs/architecture/candidate-behavior-changelog.md` 登记。

## 8 发版说明要点

破坏性变更：类型级全局配置取消，设置按方案保存；旧键作为别名继续可用，含义是「当前方案
（或主方案）的对应设置」；`data_custom/config.toml` 里的这四段不再读取；
`schema.pinyin.fuzzy.*` 改名 `input.fuzzy.*`。
