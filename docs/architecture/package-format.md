# 分发包与配置片段格式规格

状态：规格草案（2026-08 讨论定稿）。文中「现状」= 已实现行为；「新增」= 已定方向、未实施。
本文件是跨仓契约的权威来源，涉及四个仓库：

| 仓库 | 角色 |
|---|---|
| WindInput（主仓） | 格式实现（`wind-transfer/src/scheme.rs`）、片段校验与应用（wind-config / wind-webdata RPC） |
| wind-setting | 导入 UI、内容侦测分派、`windinput://` 协议与 `.wpkg` 关联 |
| WindInputTools | web 打包 / 导出工具（生成端，须严格按本规格产出） |
| WindInputDocs | cookbook 分发（片段代码块 + 一键导入链接） |

跨仓契约无编译期约束，改动本规格必须同步四仓（参照 `datadir.conf` 的教训：单向契约、静默漂移）。

## 1. 术语与格式家族

- **配置片段（fragment）**：一段 TOML 文本，内容为 config.toml 的键值子集。一等格式，可独立分发（剪贴板 / `.toml` 文件 / 文档站代码块）。
- **方案包（scheme package）**：zip 容器，内容为 schemas 根相对路径的方案文件与其引用资源。现状即 v2 格式。
- **配置包（config package）**：方案包的超集——额外携带 `config_patch.toml`（即一份配置片段）与可选 extras 资源。
- **分发包**：方案包与配置包的统称。后缀 `.wpkg`（新增，用于文件关联）或 `.zip`，导入端永远两者都接受。
- **备份包**：`kind = "backup"` 的自描述 zip（清单 `manifest.toml`，有 `spec_version` 门禁）。**不在本规格范围**，见 `docs/design/import-export-backup-design.md`。个人数据（用户词库/词频/临时词）只进备份包，永不进分发包。

## 2. 配置片段（一等格式）

### 2.1 定义

- 合法的 TOML 文本，键为 config.toml 的点分路径展开形式（表结构或点分键均可，语义等价）。
- 键域 = `wind-config/src/config_schema.rs` 的 `REGISTRY` 登记键 ∪ 「合法但不登记」显式白名单（注释模板等刻意不进 REGISTRY 的 `Option<T>` 键；名单落在 wind-config，须配守门测试防腐烂，与 `ABSENT_FROM_DATA_CONFIG` 同族待遇）。

### 2.2 应用语义（新增）

- 逐键走 `set_user_value` 通路合并进用户层 config.toml：REGISTRY 类型/值域校验、等默认即删（prune）、类型迁移全部自动生效。**禁止整文件覆盖**（备份还原的 config 域整文件覆盖是备份专属语义，分发格式不得复用）。
- **原子性**：预览阶段逐键报告（键 / 当前值 / 新值 / 错误原因）；应用阶段任何一键校验失败则**整片段拒绝**，不做半应用。
- 应用必须经过与 `config.setItems` 相同的代码路径，保证运行时镜像回灌（改了即生效，不许出现「重启后才生效」）。
- **Map 键逐条合并**（2026-08-19 定稿）：REGISTRY 登记为 `Map` 的键（`keys.key_actions` /
  `keys.schema_hotkeys` / `keys.session_actions` / `input.punct.custom_mappings`），片段中其下的表
  **恒为逐条合并**（upsert：并入当前生效表，同名条目覆盖，其余条目保留）。片段**不能**对 Map 键
  整表替换或删除条目——分发包带整表替换会清掉用户既有绑定，这是本语义存在的理由；顺带消灭了
  「空表 = 清空」的脚枪（空表 = 无条目 = no-op）。预览逐条目报告（条目名不并入点分键——
  `custom_mappings` 的条目名可含 `.`，`PatchEntry` 用独立 `map_entry` 字段承载）。
  `StructList` 键（如 `schema.mix_modes`）无此语义，仍为整值替换——**分发包不得携带 StructList 键**
  （同样会覆盖用户数据），生成端与 cookbook 都要遵守。
- **可执行内容提示**（`patch.rs` 的 `RISKY_KEYS`）：片段之所以能开放到「改任意登记键」，前提是
  config.toml 里没有可执行内容（能执行程序的短语属用户数据，只进备份包）。`ui.toolbar.buttons`
  的 `action` 是 cmdbar 表达式，且 StructList 整值覆盖，于是「导入片段 → 多出按钮 → 用户点一下」
  就是一条任意程序执行路径。处置是预览里给 `warning` 提示而非阻断（用户自己写是正当需求）。
- **降级闸**（`mark_degraded_seeds`）：Map 合并的种子取生效配置；段级降级后坏段处是出厂空表，
  合并写回会整表抹掉用户已有条目。故种子不可信的 Map 条目在预览阶段就标成条目级错误，
  应用侧「有错即整体拒绝」自动生效。标量键不拦（落盘的是显式新值，与种子无关）。

### 2.3 说明元信息 `[package]`（保留段，2026-08-20 定稿）

分发内容要能自我介绍——用户看到的不该只是一串键名变更，还要有一句「这是干什么的」。

- 片段可携带**顶层保留段 `[package]`**，字段 `title`（单行标题）与 `description`（多行说明）。
  三种载体（片段 / 分发包 `package.toml` / 文本信封）**同名同义**，一套语法学一次；
  片段被放进包里当 `config_patch.toml` 时原样可用。
- `package` 是片段里**唯一**的保留顶层段：展平时整段跳过，既不产出配置条目、也不报「未知配置键」。
  config.toml 没有 `package` 域，不存在与真实配置键撞名的可能。
- 段内**未知子键一律忽略**（向前兼容：将来加字段，旧客户端不炸）。
- 限额与净化（说明是分发者提供的任意文本，会直接渲染进 UI）：
  `title` ≤ 200 字符且不含换行，`description` ≤ 4000 字符，超限即**报错**（分发前该自测，不静默截断）；
  `\r\n` 归一为 `\n`，除 `\n` `\t` 外的 C0 控制字符（U+0000–U+001F）与 DEL（U+007F）一律拒绝；
  C1（U+0080–U+009F）不拦（现代文本里它们是合法码点的一部分，不是控制意图）。
  UI 侧另有显示行数上限与滚动，core 的限额不替代它。
- 判据落在 trim 的哪一侧是有讲究的：**换行与长度按 trim 之后判**（TOML 多行字符串写 title 时，
  尾随 `\n` 是语法固有产物，不是分发者的错误；「不含换行」的意图是 title 是单行的），
  而**控制字符必须在 trim 之前判**——VT（U+000B）与 FF（U+000C）既是控制字符又算 Rust 的空白，
  挪到 trim 之后会被悄悄吃掉从而放行。
- **版本门禁先于说明校验**：`format_version` 过高的包，其说明字段规则也可能已变，
  用当前规则去判它本身就是错的——先报「版本过高，请升级」。zip 与信封两条路同判据同顺序。
- 展示位置：片段导入对话框在预览列表上方显示；包/信封导入确认在顶部显示。
  包内 `config_patch.toml` 自带的说明属于该片段，显示在「将修改的配置」区块处，
  与包级说明（来自 `package.toml`）各显各的，**不做合并或优先级判定**——两级说的不是同一件事。

### 2.4 RPC 契约（P0-P1 已实现；Map 合并与 written 为 2026-08-19 增补，`info` 为 2026-08-20 增补）

- `config.previewPatch { text }` → `{ entries: [{ key, mapEntry?, current?, next, error? }], ok, info? }`。
  Map 键的每个条目一行：`key` = 父 Map 键，`mapEntry` = 条目名，`current` = 当前表中该条目的值（缺席 = 新增）。
  `info: { title?, description? }` 来自片段的 `[package]` 段（§2.3），两字段都缺省时整个 `info` 不出现。
- `config.applyPatch { text }` → `{ ok, applied, needsRestart, written: [{ key, value }] }` / 整体拒绝（错误列表）。
  `written` 是**落盘后的最终键值**——Map 父键携带合并后的整表。设置端用它回灌配置镜像
  （回声豁免比对的是整键值，Map 合并后客户端无法从 entries 自行拼出整表，必须由 core 回传）。

## 3. 分发包容器

### 3.1 现状（方案包 v2）

- zip，条目名 = schemas 根相对路径（零层级，导入零改写落到 `%APPDATA%\WindInput\schemas\`）。
- `package.toml`（`PACKAGE_META_NAME`）可选；缺失时按「根目录存在 `*.schema.toml`」识别。现有字段：
  - `[package]` app_version / platform / created_at / format_version（§3.2）/ title / description（§2.3）
  - `[schema]` id / version
  - `[refs]` system / missing
- 导出恒为自包含（系统目录资源读源打包）。
- 导入校验：逐条目穿越守卫 `bundle::validate_entry_rel`；含 `manifest.toml`/`manifest.json` → 报「是备份包」；根无 `*.schema.toml` → 报「不是有效的方案包」。写入 = 全部读入内存后逐文件 tmp + rename。

### 3.2 新增：`format_version` 门禁

- `[package]` 增加 `format_version: u32`。当前规格版本 = **2**。
- 门禁规则（落点 `list_payload_entries`，仿 `Manifest::validate`）：
  - `package.toml` 缺失 → 视为 legacy（format_version = 1），按现状宽容导入；
  - `format_version >` 实现支持的版本 → **硬拒绝**并提示升级应用。
- 原则：**宽容只给过去，不给未来**。`read_package_meta` 现状「解析失败静默回落默认值」的行为对 legacy 包保留，但含 `format_version` 字段而解析失败的包应报错而非静默回落。

### 3.3 新增：配置包布局

```
<id>-<version>.wpkg  (zip)
├─ package.toml              # 携带新能力时必须；format_version = 2
│    [contents]              # 声明包内内容类型，导入端照单校验，未声明的顶层目录拒绝
├─ *.schema.toml + 词库/拆字/字体/双拼布局    # 零层级，语义与 v2 相同
├─ config_patch.toml         # 可选；一份配置片段，语义完全按 §2
└─ <extras 顶层目录>/         # 保留位；类型集合未定（首批候选：注释库），暂不实现
```

- `config_patch.toml` 的校验与应用 = §2 的片段管线，包不引入任何额外语义。
- extras 目录解包必须走同一个 `validate_entry_rel` 守卫，不得另写守卫。

config_patch 的实现决策（2026-08-19 定稿）：

- **按名识别，不落盘**：根条目 `config_patch.toml` 由导入端识别为配置片段，**不写进 schemas 目录**
  （它不是方案资源；落盘即死文件）。
- **要求 `format_version ≥ 2`**：legacy（v1 / 无 package.toml）包中出现 `config_patch.toml` → 硬拒绝
  并提示包需重新打包——v1 语义下它会被旧客户端当死文件，生成端必须声明版本。
- **应用编排在设置端，两步走**：确认对话框展示片段逐键 diff（§2.4 previewPatch 结果）→ 用户确认 →
  ① `scheme.import` 写方案文件 → ② `config.applyPatch` 应用片段（继承热重载与镜像回灌）。
  预览有任一错误条目则**整包禁止导入**（分发侧应出厂前测过）。两步之间非原子：文件已装而片段失败时
  如实报错（方案已导入、配置未应用），不回滚文件。core 侧不做跨域原子化——patch 管线的热重载与
  事件广播都住在 RPC 分发层，在文件层复刻它们是第二份真相源。

### 3.4 文本信封（`kind = "schema_text"`，2026-08-19 定稿）

小方案（快符类：方案 + 小词库共 KB 级）的纯文本分发格式——一段 TOML 文本即一个完整分发包，
剪贴板 / 文档站代码块即贴即装。**分发格式，不是存储格式**：导入端拆解落盘后，存储形态与
zip 导入完全一致（方案文件 + 词库文件），引擎与缓存管线零感知。

```toml
[package]
format_version = 2          # 必填。信封无 legacy——缺失即错，高于当前支持即拒绝
kind = "schema_text"        # 必填。显式声明，侦测不做猜测
title = "快符方案"           # 可选，说明元信息（§2.3）
description = """
分号引导的符号表，含常用标点与配对符号。
"""

[schema]                    # 可选冗余，免解析 files 即可显示 id/版本
id = "kf"
version = "1.00.0"

[[files]]
path = "kf.schema.toml"     # schemas 根相对路径，逐条过 validate_entry_rel
content = '''…方案原文…'''   # 逐字内嵌；生成端负责选择不冲突的 TOML 字符串引法

[[files]]
path = "flypy/12_kf.dict.yaml"
content = '''…词库原文…'''
```

- 根（path 无 `/`）必须含至少一个 `*.schema.toml`（与 zip 侦测规则 2 同构）。
- `config_patch.toml` 可作为 files 条目出现，语义同 §3.3（按名识别、不落盘、设置端两步编排）。
- **显式声明才走此路**：`[schema]` 是合法片段键前缀（config.toml 有 `schema.` 段），
  裸方案 TOML 文本**不**自动识别为信封——没有 `kind = "schema_text"` 的文本一律进片段管线，
  由「未知配置键」如实报错（侦测规则「不猜」的延伸）。
- 限额：信封文本 ≤ 2 MB、files ≤ 64 条（超限即拒；大方案走 zip/.wpkg，信封只服务小方案）。
- RPC：`scheme.previewImportText { text }` / `scheme.importText { text }`，返回形状与
  `scheme.previewImport` / `scheme.import` 一致（复用同一确认对话框）。

## 4. 内容侦测规则（统一导入分派）

导入入口收到内容后按序判定（规则顺序即优先级）：

1. zip 且含 `manifest.toml` / `manifest.json` → 备份包 → 转备份还原流程（或提示去备份页）。
2. zip 且根含 `*.schema.toml` → 分发包（方案包/配置包）→ `scheme.previewImport` 流程；若同时含 `config_patch.toml`，预览必须附带 §2.4 的逐键 diff。
3. zip 且仅含 `config_patch.toml`（无方案文件）→ 纯配置包 → 片段流程。
   **实现状态（2026-08-19）**：core 侧暂未承接此形——`scheme.previewImport` 对无根方案的包报
   「不是有效的方案包」，官方生成端也不产出纯配置 zip；纯配置分发请用片段文本或文本信封（§3.4）。
   本条规则保留为格式家族的完备性定义，待有真实需求再接。
4. 非 zip 的合法 TOML 文本，且 `package.kind = "schema_text"` → 文本信封（§3.4）→ `scheme.previewImportText` 流程。
5. 其余合法 TOML 文本（文件或剪贴板）→ 配置片段流程。
6. 均不匹配 → 明确报错，不猜。

生成端（WindInputTools / 手工打包）必须保证产物落进规则 2 或 3，否则视为无效包。

## 5. 安全要求

- **穿越守卫**：所有 zip 条目一律过 `validate_entry_rel`（components 白名单式；拦 `..`、绝对路径、盘符相对 `C:foo`、UNC、段内 `:`；返回 `\`→`/` 归一化后的路径，判根/取 id/落盘一律用归一化值）。
- **既有取舍（2026-08-19 审查记录，接受不改）**：①批量落盘是逐文件 tmp+rename——单文件原子，批内**不**原子，中途失败已写入的前序文件不回滚（与 §3.3「文件与片段两步非原子」同哲学：如实报错优于伪原子）；②同批内大小写不同的重复路径（Windows 不区分大小写）不做归并检测，Merge 策略下第二个按冲突呈现、Replace 策略下后写覆盖先写。
- **限额**（新增，数值待定）：下载体积上限、解压后总量与条目数上限、下载超时。
- **确认对话框最低内容**（不可减配，任何入口一致）：来源（域名或文件路径）、方案 id 与版本、文件数与体积、与现有文件的冲突数、`config_patch` 将改动的键（当前值 → 新值）逐键列出。
- **协议来源策略**：`windinput://` 的 `url` 参数限官方域白名单 + https（现状已限 https）。本地文件 / 剪贴板不限来源——能力越强的内容走越窄的门（片段仅能改登记键 → 最开放；协议一键到达 → 最紧）。

## 6. 版本与兼容

- 已发布旧客户端遇到含 `config_patch.toml` 的包：将其当普通文件落进 schemas 目录（无害但配置不生效）。**无法补救**，分发侧（文档站 / web 工具）必须标注最低应用版本。
- 导出端默认文件名切换到 `.wpkg` 的时机：在 `.wpkg` 文件关联随版本发布**之后**，避免「导出的文件双击没反应」窗口期。
- 备份包保持 `.zip`，不参与 `.wpkg` 关联（防误双击触发整机还原确认）。
