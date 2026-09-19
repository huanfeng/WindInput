# 网址 / 邮箱模式：补全候选 + 自动学习

分支 `feat/email-url-mode`，worktree `wt-mail/`（core 与 wind-setting 两仓并列，保住
wind-setting 写死的 `../WindInput` 相对依赖）。

设计依据：`docs/design/prefix-hijack-modes.md` §3（新增模式的 15 个接线点）、§5.1（网址加
候选）、§5.2（邮箱模式走「后缀触发」路 1，两模式共用一个补全候选源）。

## 已定的产品决策（2026-09-19 与用户确认）

| 决策点 | 结论 |
|---|---|
| 空格键语义 | 有候选 → 上屏高亮候选；无候选 → 上屏缓冲原文。**网址模式一并改**（§5.1 留的那道题） |
| 空缓冲按 `@` | **不进**邮箱模式，`@` 照常走标点流水线 |
| 设置分组 | 合并为一个「网址/邮箱输入」分区 |
| 网址历史 | **单独开关，默认关**（邮箱后缀学习则跟随邮箱模式开关：它只重排预置列表，不记录输入内容） |

## Stage 1: 存储层（wind-store）
**Goal**: 一张全局 `completion` 表承载两类学习数据，附全套原语。
**Success Criteria**: `cargo test -p wind-store` 绿；表按 `{kind}\0{text}` 编码，两类互不干扰。
**Tests**: 记录/累加、按前缀列举与排序、逐条删、按 kind 清空、prune 上限、jsonl 往返。
**Status**: Complete（10 用例）

## Stage 2: 配置层（wind-config）
**Goal**: `input.email.*` 新增 + `input.url.history_*` 新增，进注册表与 `data/config.toml`。
**Success Criteria**: `cargo test -p wind-config` 绿（含 `registry_covers_every_config_key`
与 `data_config_toml_covers_registry` 两道双向守门）。
**Tests**: 默认值断言、预置后缀条数、tolerant 反序列化。
**Status**: Complete（5 用例，wind-config 429 全绿）

## Stage 3: 邮箱模式骨架（wind-coordinator）
**Goal**: `ModeKind::Email` 走通「`@` 后缀触发 → 累积 → 空格上屏 → 退格回退」，尚无候选。
**Success Criteria**: 15 个接线点全部接上；`cargo test -p wind-coordinator` 绿。
**Tests**: 进入/累积/上屏/Esc/退格回退到正常输入/空缓冲按 `@` 不触发/模式关闭时零影响。
**Status**: Complete（与 Stage 4 合并实现——空格语义要改两遍才能拆开，不值得）

## Stage 4: 补全候选源 + 学习写入
**Goal**: 网址与邮箱共用一个候选源；空格语义改为「有候选选候选」；上屏时写学习数据。
**Success Criteria**: `cargo test -p wind-coordinator` 绿，含网址模式既有用例不回归。
**Tests**: 邮箱后缀按频次重排、自定义后缀可学、网址历史前缀补全、history 开关关闭时不落盘、
空格在有/无候选两种情形的分叉。
**Status**: Complete（20 用例，clippy 零警告）

## Stage 5: RPC / 备份 / 设置端 / 检入产物
**Goal**: 查看与清空入口、备份包登记、设置界面合并分区。
**Success Criteria**: `cargo test -p wind-rpc --test wind_setting_assets` 绿；
邻仓 `dev.sh st` 绿；`dev.sh sg` 重生成的两份检入产物与 core 对账一致。
**Tests**: RPC list/clear 往返、备份 create→restore 带上新数据。
**Status**: Not Started
