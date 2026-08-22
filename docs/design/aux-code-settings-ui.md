# 辅助码设置界面计划

状态：计划（2026-08-20）。core 功能已合入 main（PR #68，`529b6d80`），设置界面未做。
本文回答「界面该做成什么样、分几步、每步的判据是什么」，不含实现。

## 1. 为什么不是「接两个配置项」

`capabilities.rs` 的 `UNCOVERED_BY_DESIGN` ㉒ 条把 `schema.pinyin.aux_code.enabled` /
`max_phrase_len` 登记为「待做 GUI」，并存了一份 manifest 草稿。但只接那两项，用户仍然用不了辅助码——
**能不能用取决于四件事，配置项只占其中两件**：

| # | 配置面 | 落点 | 现状 |
|---|---|---|---|
| 1 | 全局基线 `enabled` / `max_phrase_len` | `config.toml` `[schema.pinyin.aux_code]` | 键已登记 REGISTRY，无控件 |
| 2 | 方案级覆盖（tri-state，`None` = 跟随全局） | `.schema.toml` `[engine.aux_code]` / `schema_overrides/{id}.toml` | 无控件 |
| 3 | 码表 `files`（这个方案用哪张字形表） | 同上（方案属性） | 无控件 |
| 4 | **触发键** | **方案文件 `[session_actions]`**（如 `backtick = "aux_code"`） | `session_actions` 动词表 UI 已有，**但动词列表里没有 `aux_code`** |

第 4 项是「开了能不能进去」的决定因素，却最容易被漏——它不在 `[schema.pinyin.aux_code]` 段里，
按配置键去找根本找不到它。

## 2. 三个非做不可的判据

### 2.1 引导键会被音节分隔符吃掉（「配了没反应」的头号来源）

全拼出厂 `separator = "auto"` 且 `'` 作选词键时，**反引号恒为音节分隔符**——
 分隔符臂在按键分发里先于 `session_actions` 裁决，所以即使方案里把反引号绑成 `aux_code` 也进不去。
 core 已在 `handle_aux_code.rs` 打了告警日志，但用户看不见日志。

⇒ 界面必须承担这件事，两条同时做：
- 开关的 hint 直书前提，不能只写「启用辅助码」；
- **绑定冲突要有可见反馈**：当方案 `[session_actions]` 里把某键绑成 `aux_code`、而该键在当前
  `separator` 设置下是分隔符时，在按键表那一行给出警示（这是 UI 能替用户挡住的坑，不该留给日志）。

### 2.2 `files` 非空 ≠ 功能开启

方案里配了码表只表示「这个方案推荐哪张表」，开关另在别处。界面上这两件事必须**分区呈现**，
不能把码表下拉做成「选了就等于开」。

### 2.3 码表文件可能不存在

`data/schemas/aux_code/` 不入版本库（`stroke.txt` 上游是 LGPL-3.0，按 `NOTICE.md` 政策只下载不入库），
由构建时 `gen_aux_code` 生成。用户机器上**可能一张表都没有**（自建、旧版升级等情形）。

⇒ 码表选择控件要能表达「装了哪些」，选到缺失的表要如实报，不能静默失效。

## 3. 分期

### P1 — 最小可用（改动小，价值最大）

1. `session_actions` 的动词列表加 `Verb { value: "aux_code", label: "辅助码筛选" }`
   （现有列表是 `none` / `page_next` / `page_prev` / `cancel` + 动态项；与 `key_actions` 的动词表分开），
   共键场景另提供 `page_next_aux_code`。
2. `settings_manifest.toml` 加两个 `[[items]]`（`group = "schema"`、`section = "候选行为"`、
   `subsection = "辅助码"`），草稿已在 `capabilities.rs` ㉒ 条注释里，含 `type` / `min-max` /
   `enabled_when`。**hint 按 §2.1 重写**，交代引导键前提。
   > `section = "候选行为"` 会让它们落进**拼音方案配置对话框**（`pages/mod.rs` 的
   > `build_scheme_config_button(state, "拼音方案配置", "候选行为", true)`），
   > 与 `shuangpin.allow_full_pinyin` 同一处——这正是「全局基线」该在的地方，不是散在全局设置页。
3. 从 `UNCOVERED_BY_DESIGN` 移除这两个键（`uncovered_capability_keys_match_allowlist`
   的 stale 半边会盯着这一步）。

P1 完成后：用户能在界面上开辅助码、绑触发键。码表仍依赖方案自带或手改。

### P2 — 码表选择与冲突提示

4. 新 RPC 列出可用码表（扫程序 `data/schemas/aux_code/` 与用户目录同名路径），返回
   id / 显示名 / 是否存在。现有三张：`stroke`（笔画）、`flypy_full`（小鹤形码）、
   `ZRM-wanxiang`（自然码万象）。
5. 拼音方案配置对话框的「辅助码」区块加码表选择，按 §2.2 与开关分区、按 §2.3 处理缺失。
6. 按 §2.1 的第二条做绑定冲突警示。

### P3 — 方案级覆盖

7. 照 `dialogs/schema_codetable.rs` 的既定模式做「方案级辅助码配置」：**一个总开关，
   关 = 该方案一个字段都不写、整体跟随全局；开 = 整套自己配**（不是每字段一个 tri-state 控件——
   那个模式本仓已经权衡过并否决，见该文件头部注释）。落 `schema_overrides/{id}.toml`，
   并按既有规矩登记 `SideCommitter`（非 `config.toml` 落点一律登记，禁止另加保存按钮）。

## 4. 不做

- **不在全局设置页放辅助码开关**。它是拼音引擎的全局默认行为，归拼音方案配置对话框；
  全局设置页放它会让「方案级覆盖才是真开关」这件事更难说清。
- **不做码表编辑器**。码表是构建产物，用户要自定义就放同名文件覆盖（走既有的同结构覆盖机制）。
- **不替用户自动改 `separator`**。冲突时提示并给出改法，改不改由用户定——
  静默改动一个影响全局输入的键，比「配了没反应」更糟。

## 5. 相关

- `docs/design/schema-key-actions.md` — 方案级 `[key_actions]` 的动词值域
- `wind-setting/src/dialogs/schema_codetable.rs` 头部注释 — 方案级覆盖 UI 的既定模式与权衡
- `wind-setting/src/capabilities.rs` ㉒ 条 — manifest 候选草稿与出处
