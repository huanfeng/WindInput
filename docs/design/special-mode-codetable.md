# 引导键特殊模式：自定义码表（快符 / 生僻字）

## 背景与需求

输入法已有一套「由触发键激活的模式」框架（快捷输入 / 临时拼音 / 临时英文），见
`docs/design/mode-trigger-priority-chain.md`。该文档的 `triggerModes()` 注释里已预留
「★ 未来模式（生僻字 / 符号码表）插入此处」的位置。

本设计落地这个预留方向：新增一种**由引导键进入、配置文件驱动**的特殊模式。它加载一份
**自定义码表**（`编码 → 候选`），主要服务两类用户诉求：

- **快符输入**：用户按自己的映射方案快速输入符号。编码一般很短、通常一码一候选，且
  「唯一候选且无后续」时**自动上屏**（不论编码长度）。
- **生僻字模式**：把生僻字单独成表，编码可能是完整长度（如 4 码），是否自动上屏可配。

两者只是同一框架的**两份不同配置**，框架本身不写死用途，未来可再加新用途（再配一份即可）。
候选不仅支持纯文本，也**完整支持命令直通车（`$CC`）与变量（`$X`）、字符/字符串数组
（`$AA`/`$SS`）**，达到完全自定义。

## 已确认的需求边界

| 决策点 | 结论 |
| --- | --- |
| 架构形态 | **通用框架 + 多实例**：一套「自定义码表模式」，可配 N 个实例，每个实例 = 引导键 + 码表 + 行为配置。快符 / 生僻字只是两份配置 |
| 自动上屏策略（每实例选一种） | `prefix_free`（唯一候选且无更长前缀）/ `fixed_length`（达固定码长且唯一候选）/ `manual`（永远手动选） |
| 上屏后去向 | **一次性**：上屏（自动或手动）后立即退出模式、回到正常输入（与现有临时模式一致） |
| 编码字符集 | MVP **仅 a-z**，数字键 1-9 选候选；**预留**自定义字符集（部分方案用符号入码） |
| 配置归属 | MVP **全局**（任何方案可用、无引擎门禁）；**预留** per-scheme 绑定与引擎门禁字段 |
| 码表格式 | **直接用 Rime `.dict.yaml`**，复用五笔同款加载链（文本源 + 自动 wdb 缓存），默认列序 **code 在前、text 在后** |
| 命令 / 变量 / 数组 | `$CC`/`$X`/`$AA`/`$SS` 全支持；把数组展开从 phrase 抽成**共享设施**，phrase 与 special-table 统一对接 |
| 设置页 UI | **不在本 spec**，MVP 手写 yaml 跑通；UI 作为独立后续阶段 |

## 架构与组件

四个职责单一的组件，最大化复用现有设施：

### 1. 后端存储：复用 `*dict.CodeTable` + Rime 加载链

特殊码表的底层存储**直接用现有 `dict.CodeTable`**，它已同时支持文本源与 wdb 二进制，并提供
`Lookup(code)` / `LookupPrefix(prefix, limit)` / `HasLongerCode(input)` / `GetMaxCodeLength()`。

加载走**五笔方案同款 Rime 管线**（`internal/dict/dictcache/convert.go`）：

- `ConvertRimeCodetableToWdb(mainDictPath, wdbPath, ...)`：把 Rime `.dict.yaml` 转 wdb，
  带 mtime 缓存失效、`import_tables` 递归。
- `CodeTable.LoadBinary(wdbPath)`：mmap 加载 wdb。

特殊码表实例**独立加载，不注册进主 `CompositeDict`**（隔离，不污染五笔 / 拼音查询）。
因为走 Rime 管线本就「文本源 + wdb 缓存」二象性齐备，无需区分 yaml/wdb 两期。

> `HasLongerCode` 在 wdb（`LookupPrefixExcludeExact`）与内存两种模式都已实现，prefix-free
> 自动上屏判定零新增逻辑。

### 2. 统一候选 value 展开设施（把 `$AA`/`$SS` 抽共享）

现状：`internal/dict/value_expand.go` 的 `ValueExpander` 处理 `$CC`/`$X`；`$AA`/`$SS`
数组展开则耦合在 `phrase.go` 的 `SearchCommand` 里（`cmdbarArrayHook` + `aa_marker.go` /
`ss_marker.go`）。

改动：把数组展开抽到 `dict` 层一个**共享函数**，与 `ValueExpander` 合并成「一个 raw value
→ 一条或多条候选（含 `Actions`）」的单一入口。

- `phrase.go` 的 `SearchCommand` 改调共享入口（**等价重构，行为不变**）。
- special-table 候选展开也调它。

这是 DRY 关键，也是**改动面最大、风险最高**的一块：phrase 数组行为必须用回归测试锁住，
单独成任务，最后实施。

### 3. `specialMode` 状态机（仿 `quickInputMode`）

新文件 `internal/coordinator/handle_special_mode.go`，仿 `handle_quick_input.go`：

- 独立 buffer（`specialBuffer`），**不进主引擎**。
- 当前激活实例 ID（`specialActiveID`）+ 该实例已加载的 `*CodeTable` 与行为配置引用。
- 进入 / 输入 / 选择 / 退出 / 一次性退出；纯判定逻辑尽量抽纯函数便于单测。

### 4. 实例注册表 `special_mode_registry.go`

启动 / 配置热重载时，按全局 `Input.SpecialModes` 列表装配每个实例的
`(引导键 → 行为配置 + 懒加载的 CodeTable)`：

- 码表**懒加载 + 缓存**：生僻字可能上万条，首次激活才 `ConvertRimeCodetableToWdb` +
  `LoadBinary`，之后缓存；配置变更或文件 mtime 变化时失效重载。
- 校验失败的实例**跳过 + 记 WARN 元数据**（不阻断其它实例 / 不崩输入法）。
- 同一引导键被多个实例抢占 → 配置靠前者命中，记一条 WARN。

### 接入现有触发键框架

- `triggerModes()` 改为**动态构建**：在「临时拼音之后、临时英文之前」按配置插入 N 个
  special-mode 实例项，每项提供 `matchSpecialTrigger(id)`（纯键匹配 + enabled）+
  `setupSpecialMode(id)`（状态设置）。
- 完全复用现有 `enterModeCommitting`：正在输入时按引导键 → 顶码上屏当前高亮候选 + 原子
  进模式（触发符不上屏，嵌入 / 非嵌入编码均已处理）。
- 复用上一轮「嵌入编码下模式徽标提示条」：special-mode 设 `ModeLabel` 即自动生效。

## 配置结构

全局 `Input` 下新增 `special_modes` 列表（`pkg/config/config.go`）：

```yaml
input:
  special_modes:
    - id: "symbols"              # 实例唯一标识（内部 key / 日志元数据）
      name: "快符"               # 模式徽标显示名（SetModeLabel）
      trigger_keys: ["grave"]    # 引导键，复用现有 trigger key 体系
      table: "special/symbols.dict.yaml"   # 码表文件，相对 schemas 目录
      auto_commit: "prefix_free"      # prefix_free | fixed_length | manual
      fixed_length: 4                 # 仅 auto_commit=fixed_length 时生效
      force_vertical: false           # 强制竖排候选
      accent_color: "#3C78AF"         # 模式发光边框色，空=内置默认
      # —— 以下为预留字段，MVP 不实现，先占位 ——
      code_charset: ""                # 空=默认 a-z；预留符号入码
      schemes: []                     # 空=全方案；预留 per-scheme 绑定
      engines: []                     # 空=无引擎门禁；预留引擎门禁
```

- `auto_commit` 三档对应三种策略。`fixed_length` 档 MVP 要求「达 `fixed_length` 码 **且**
  唯一候选」才自动上屏（更长可继续）。
- 校验：`id` 必填且唯一、`trigger_keys` 非空、`table` 文件存在；无效实例跳过 + WARN。
- 全新字段，无旧配置迁移负担。

## 行为与数据流

### 进入

- buffer 空时按引导键 → 直接进 special-mode（空 buffer）。
- 正在输入（有 buffer / 候选）时按引导键 → `enterModeCommitting` 顶码上屏当前高亮候选 +
  原子进模式（触发符不上屏）。
- 嵌入编码下刚进入、候选空 → 复用模式徽标提示条。

### 输入与候选计算（每次按键后）

1. a-z 追加到 `specialBuffer`；查 `CodeTable.Lookup(buffer)` 得直接候选，
   `HasLongerCode(buffer)` 判后续。
2. 候选 value 走**统一展开**（`$CC`/`$X`/`$AA`/`$SS`）→ 最终候选列表（含 `Actions`）。
3. **自动上屏判定**（按实例 `auto_commit`）：
   - `prefix_free`：`len(候选)==1 && !HasLongerCode(buffer)` → 立即上屏并退出。
   - `fixed_length`：`len(buffer) >= fixed_length && len(候选)==1` → 上屏退出。
   - `manual`：从不自动，永远等手动选。
4. 未触发自动上屏 → 显示候选窗，等手动选。

### 选择 / 控制键（MVP，a-z 入码）

| 键 | 行为 |
| --- | --- |
| 数字 1-9 | 选当前页对应候选 → 上屏退出 |
| 空格 | 选当前高亮候选；空 buffer 时上屏引导符字面量（与快捷输入一致） |
| 回车 | 上屏 buffer 原文（应急直出） |
| 退格 | 删 buffer 末字符；空 buffer 再退格 → 退出模式 |
| Esc | 退出模式（不上屏） |
| 方向键 / 翻页键 | 沿用现有高亮 / 翻页逻辑 |

### 上屏 / 命令候选

- 普通文本候选 → `InsertText` 上屏。
- 命令候选（`Actions` 非空）→ 走现有 cmdbar 动作执行通路（`ResponseTypeClearComposition`
  + goroutine 顺序执行），与短语命令候选完全一致。
- 上屏即**退出模式**（一次性），记入 `inputHistory` / `recordCommit`（新增 source 标识
  `SourceSpecialMode`）。

### 退出清理

仿 `exitQuickInputMode`：清模式标志、`ModeLabel`、accent color、恢复 `force_vertical` 前
的布局、`hideUI`。

### 引擎门禁 / 方案

MVP 全局可用、无引擎门禁（任何方案下按引导键都能进）；`schemes` / `engines` 字段预留二期。

## 码表格式：Rime `.dict.yaml`

特殊码表是标准 Rime `.dict.yaml`，默认列序 **code 在前、text 在后**：

```yaml
# schemas/special/symbols.dict.yaml
---
name: symbols
version: "1.0"
sort: by_weight        # 或 by_order（按文件顺序）
columns:
  - code               # 编码（a-z）
  - text               # 符号 / 文本（可含 $CC/$X/$AA/$SS）
  - weight             # 可选
...
jt	→	100
jt	←	90
xh	①
xh	②
sj	$X(...)
bd	$CC("打开百度", open("https://baidu.com"))
ar	$AA("箭头", "←↑→↓")
```

- 一码多候选 = 多行同 code（如 `jt` → →/←）。
- `#` 注释、`weight` 列、`import_tables` 递归全部复用 Rime 管线。
- text 列含 `$` 标记 → 统一展开（命令 / 变量 / 数组）。
- `columns` 指令本就支持声明列序；special-table 默认模板用 code-first。

## 实施分期

### MVP（本 spec）

1. **Rime 码表独立加载**：实例注册表 + 懒加载 + wdb 缓存（复用 `ConvertRimeCodetableToWdb`
   + `LoadBinary`）。
2. **配置结构**：`Input.SpecialModes` + 校验。
3. **`specialMode` 状态机**：`handle_special_mode.go`，进入 / 输入 / 选择 / 退出。
4. **triggerModes 接入**：动态构建 + `matchSpecialTrigger` / `setupSpecialMode`。
5. **三档自动上屏**：`prefix_free` / `fixed_length` / `manual` 纯判定函数。
6. **统一展开**：抽 `$AA`/`$SS` 共享设施 + phrase.go 等价重构 + 回归（**最后实施，单独验证**）。
7. **a-z 入码 / 数字选候选 / 控制键映射**。

### 后续（不在本 spec）

- 设置页 UI（实例增删改 + 码表编辑）。
- per-scheme 绑定与引擎门禁（`schemes` / `engines` 字段）。
- 符号入码字符集（`code_charset`）。

## 测试策略

- **单元**：Rime 码表独立加载往返；三档自动上屏判定纯函数；统一展开共享函数
  （`$CC`/`$X`/`$AA`/`$SS`）。
- **回归（高风险）**：phrase.go 改调统一展开后，`$AA`/`$SS` / `$CC` 行为不变（重点覆盖）。
- **状态机**：special-mode 进入 / 输入 / 选择 / 退出 / 一次性退出；control 键映射。
- **集成**：`triggerModes()` 动态构建与顺序；优先级链（二三候选键 vs special 引导键）不
  回归；现有三模式（临时拼音 / 英文 / 快捷输入）不受影响。
- **边界**：高亮为命令 / 组候选时按引导键 → 回落标点（沿用 `enterModeCommitting` 既有
  护栏）；无效码表实例跳过不崩。

## 复用清单（不重造）

| 复用对象 | 来源 | 用途 |
| --- | --- | --- |
| `CodeTable` + `Lookup`/`HasLongerCode`/`LoadBinary` | `internal/dict/codetable.go` | 特殊码表存储与查询 |
| `ConvertRimeCodetableToWdb` + Rime 加载链 | `internal/dict/dictcache/convert.go` | Rime `.dict.yaml` → wdb 缓存 |
| `ValueExpander`（扩展含数组） | `internal/dict/value_expand.go` | 命令 / 变量 / 数组统一展开 |
| `triggerModes()` / `enterModeCommitting` / `matchTriggerKeyInList` | `internal/coordinator/mode_trigger.go` | 触发键优先级链、顶码上屏进模式 |
| 模式徽标提示条 | `internal/ui/viewbox_build*.go` | 嵌入编码下刚进入的模式提示 |
| `exitQuickInputMode` 退出清理范式 | `internal/coordinator/handle_quick_input.go` | 退出清理参考 |
