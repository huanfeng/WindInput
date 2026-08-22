<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-08-20 -->

# wind-coordinator

## Purpose
输入法服务的"大脑"。实现 `wind_bridge::MessageHandler`，接收 C++ TSF 桥接层的全部事件（按键/焦点/IME 激活/光标/菜单），编排引擎、候选、UI、词库与持久化，维护完整输入状态机。上游是 TSF 桥接（`wind-bridge`），下游扇出到 `wind-engine`/`wind-candidate`/`wind-store`/`wind-ui` 等十余个 crate。

## Key Files
| File | Description |
|------|-------------|
| `src/lib.rs` | 模块导出（`Coordinator`/重启信号/设置 URL 提供者）；`is_foreground_fullscreen()` 全屏检测（供工具栏全屏隐藏） |
| `src/coordinator.rs` | 核心：`State`（全部输入态）/`Coordinator` 定义、`build`（83 字段装配点，私有字段以本模块为界不外迁）、会话键统一分发 `apply_session_action`、配置热重载 `reload_user_config`。平移出去的项经 `pub(crate) use` 保真，handle_* 仍从 `crate::coordinator::` 引用 |
| `src/coordinator/message_handler.rs` | **子模块**：`impl MessageHandler`（TSF 全部事件入口，含**按键主入口 `handle_key_event`（优先级链）**）+ 失焦归属校验 `is_stale_focus_event` + ext 信封解码。子模块可见父私有字段——重度碰私有态的切片进 `src/coordinator/`，不进平级模块 |
| `src/coordinator/first_show.rs` | **子模块**：候选窗首显闸门（延迟首显判定/释放 + `FirstShowTimer` 共享兜底 timer） |
| `src/coordinator/push_config.rs` | **子模块**：push 通道推送（activation status / 各配置帧 / `push_state_update`） |
| `src/coordinator/langbar_icon.rs` | **子模块**：语言栏图标 SHM 发布（`ICON_PUBLISHER` 进程级单例 + 状态角标） |
| `src/construct.rs` | 构造器族：生产构造 `new`（desktop-ui）+ headless 家族（`new_headless*`）+ `open_user_store`；装配核心 `build` 留在 coordinator.rs |
| `src/ui_sender.rs` | `UiSender`：`ui_tx` 的类型。把「投递 `UiCommand` + 唤醒 UI 线程」绑成一次 `send`——UI 线程是事件驱动的（`wind_ui::wake`），只投递不唤醒 = 那条命令躺到下一个计时器到期才被看见。50+ 处发送点靠类型守门，不靠纪律 |
| `src/config_bundle.rs` | `ConfigBundle`（配置 + 轻量派生缓存快照，热重载整体原子替换）+ `parse_pairs`/`parse_jump_out_*` 配置解析 |
| `src/key_convert.rs` | 键位换算纯函数：`punct_char`/`printable_char`/`numpad_*`/`full_width_source_char`/`en_case_variants`/`wind_mods_to_win32` |
| `src/candidate_nav.rs` | 候选视图导航：分页/高亮移动/悬停清除/末页检索范围临时放宽（`try_relax_scope_on_page_end`） |
| `src/debug_support.rs` | `debug_*` 测试/诊断支撑方法（生产路径不调用；生产 tooltip 用的 `DebugSchemaCtx` 族名字带 debug 但**不在**此文件） |
| `src/pipeline.rs` | `ModeKind`（单一活跃独占模式枚举）+ `Rewind`（夺取回退登记）；含与 Go 决策器的**刻意差异说明**（见下） |
| `src/handle_candidate.rs` | 候选生成/过滤/shadow/词频重排/分页/选词上屏/右键操作 |
| `src/handle_temp.rs` | 临时拼音 + 临时英文模式（触发判定/进出/候选刷新/上屏） |
| `src/handle_url.rs` | 网址模式（夺取缓冲 + 边界退格回退） |
| `src/handle_special.rs` | 引导键特殊模式（自带码表 + 全码上屏策略） |
| `src/handle_mode.rs` | 中英 / 简繁 / 方案 / 主题 / mix 融合模式切换 |
| `src/handle_punct.rs` | 标点编排 + 智能符号同键连按替换状态机（武装/触发/解除） |
| `src/handle_addword.rs` | 快捷加词 / 选词后自动造词 / `dict.add` |
| `src/handle_cmdbar.rs` | 命令直通车（cmdbar）集成：`init_cmdbar` + `EvalContext` 适配 + ime/dict 控制器 |
| `src/handle_menu.rs` | 主菜单 / 候选右键菜单分派、工具栏点击/刷新/位置持久化 |
| `src/handle_lifecycle.rs` | 配置重载、服务重启、独占模式进入/复位（IME 激活/焦点/composition 终止仍在 coordinator.rs 的 `impl MessageHandler`） |
| `src/handle_config.rs` | 配置更新处理（引擎/热键/UI/工具栏） |
| `src/handle_tooltip.rs` | 候选悬停提示（编码/拆字/拼音反查） |
| `src/handle_aux_code.rs` | 辅助码模式：进入/退出/筛选/导航/共享键（翻页+辅助码同一键）/自动退出。`AuxCodeTrigger` 枚举统一两种进入路径（`Direct`/`FromPage`），`init_aux_overlay` 不发 UI 通知（由调用方统一发送） |
| `src/hotkey_match.rs` | key_down 热键匹配 |
| `src/web_host.rs` | `WebDataHost` trait（16 方法）+ 转发 impl：设置页数据 RPC（**独立 crate `wind-webdata`**）消费宿主能力的窄面。依赖方向 webdata→coordinator，本 crate 因此不依赖 wind-transfer/fontdb（Android 闭包免 C 依赖、check-android 免 NDK 的关键，Cargo.toml 有⚠注释）。★新增 RPC 需要新宿主能力时**必须加在本 trait 上** |
| `src/freq_learn_tests.rs` | 词频路由/选词记账/自动造词/加词的 crate 内行为测试（白盒零 RPC；原住 webdata 契约测试，按「是否用 web_data_rpc」分拣回归） |
| `src/host_services.rs` | `HostServices` trait（剪贴板等平台能力注入面）+ 桌面/headless 实现；收录判据见模块文档 |
| `src/stats.rs` | 输入统计采集 |
| `src/watchdog.rs` | 看门狗 |

> `src/handle_key.rs` 仅为模块占位（文档注释），实际按键路由在 `coordinator.rs::handle_key_event`。**注意：`keymap` 不在本 crate**，在 `wind-keys`（`use wind_keys::keymap`）；根 AGENTS.md 旧引用的 `wind-coordinator/src/keymap.rs` 已失效。

## For AI Agents

### Working In This Directory
- **Coordinator 字段（81 个）的锁形态/访问分布/合并禁区**清点在 `docs/design/coordinator-state-inventory.md`——**改动或新增字段前先读**，其 §3「勿动清单」每条都是修过 bug 的结论，§5 是新增字段的归属判据。
- **按键唯一主入口 `handle_key_event`（coordinator/message_handler.rs）**，优先级链顺序即正确性契约，改动前务必理解：key_up toggle 键切换 → 菜单转发 → key_down 热键 → 候选操作热键（Ctrl+数字）→ 加词模式 → 英文透传 → **夺取回退**（`VK_BACK` + `can_rewind`）→ **`state.active` 单点 match 分派** → 空缓冲模式激活 `try_activate_mode` → Ctrl/Alt 组合清空 → URL 夺取激活 → 以词定字 → `apply_nav_key` 统一导航 → 小键盘 → Esc/Back/Space/Enter/字母数字标点（engine_default）。新增逻辑须想清插在链的哪一环。
- **独占模式单点真相源**：临时拼音/临英/URL/特殊/mix 收敛为单字段 `State.active: Option<ModeKind>`（pipeline.rs），结构上保证「同一时刻至多一个独占模式」。新增模式 = 加一个 `ModeKind` 变体 + 一条 match 臂 + 一个 `handle_*_key`，**不要**再引入并行 bool。
- **不移植 Go 决策器**：Rust 各模式按 schema id 独立查引擎（`EngineManager::convert_with`），无被多模式改写的共享引擎，故 pipeline.rs 刻意不引入 Capability/Processor trait 抽象。读 Go 同名模块时勿照搬其 `decider`/`applyEngineDiff` 机制——此处不存在。
- **导航键走统一入口 `apply_nav_key`**（配置驱动 `keymap::NavKeys`，来自 wind-keys）：普通模式与所有候选模式共用；`include_printable` 区分码表型（`-`/`=` 作翻页）与文本/表达式型（临英/快捷，`-`/`=` 作输入）。禁止在各模式里各写一套翻页/高亮。
- **辅助码与翻页共键**：辅助码触发只住 `session_actions`（动词 `aux_code` 单触发 / `page_next_aux_code` 共键），不再有 `key_actions` 那条路径。共键（`page_next_aux_code`）由 `coordinator.rs::apply_session_action` 的 `PageNextAuxCode` 臂在【进入侧】一并处理：先翻页、尚未进入辅助码态则 `enter_aux_code(state, AuxCodeTrigger::FromPage)`（保留刚翻到的页码）；已在辅助码态内则只翻页。`aux_trigger_kind(key_code, shift) -> Option<AuxTriggerKind>` 现在只回答「这个键是不是**专用**辅助码触发键（只绑 `aux_code`、不带翻页）」，供辅助码态内静默消费（`Dedicated`）；字母恒排除，复用 `session_action_for` 同一查表。`enter_aux_code` 统一两种进入路径（`AuxCodeTrigger::Direct` / `FromPage`），有防重入守卫（`state.active == Some(ModeKind::AuxCode)` 时返回 None）。`init_aux_overlay` 不发送 UI 通知（`notify_ui_update` 由调用方统一发送），`AuxCodeTrigger::FromPage` 在 `init_aux_overlay` 前后保存/恢复 `current_page`。所有辅助码逻辑内聚在 `handle_aux_code.rs` 内，其他模块只提供信息 pathway。
- **夺取回退（`pipeline::Rewind`）**：URL 抢前缀、z 抢前导拼音等「夺取式」模式登记快照后，退到前缀边界再退格 → 撤销夺取、把 `snapshot` 回放回正常码表输入流。URL 与 z 共用此机制，勿各写各的回退。
- **拼音逐步转换不变量**：`committed_text`/`committed_segs` 存「选中汉字累积、留组合区不上屏，全转完才整体上屏」；码表（五笔）选词消费整串、绝不进入此态。`preedit` 仅含输入码/拼音，**绝不含候选列表**。
- **配置热重载**：读配置统一经 `self.rt()`（`RwLock<Arc<ConfigBundle>>` 原子快照）；`reload_user_config` 整体替换 bundle，轻量项（标点/热键/候选数/导航键/配对）即时生效，重型项（引擎/方案/词典/字体）仍需重启。
- **锁与线程**：`State` 由单个 `Mutex` 保护，另有多个细粒度 `Mutex`/`Atomic`（pending_first_show、stat_recorded、fullscreen_cached 等）。cmdbar 动作经独立线程异步执行（`self_weak`），故控制器回调自锁的 coordinator 方法是安全的——切勿在持 `state` 锁时调用会再次取锁的方法。
- **工具栏显隐**对齐 Go 公式 `ime_active && toolbar_visible`（两者正交，见 `State` 注释），隐藏经 UI 层 50ms 防抖；全屏经 `fullscreen_cached` 后台异步刷新，勿在 bridge handler 线程同步调 `is_foreground_fullscreen`。

### Feature: desktop-ui（headless/Android 形态）
- `desktop-ui`（默认开）门控桌面渲染路径：生产构造器 `new`、`UiManager`、剪贴板直通、macOS `select_self`。`--no-default-features` 即 headless/Android 形态——入口是 `new_headless_with_ui`（返回 `Receiver<UiCommand>`）+ `inject_ui_event`（反向事件）+ `set_host_services`（剪贴板注入，须在首次使用前）。
- 编译门（与 CI 同一份命令，alias 见 `wind_input/.cargo/config.toml`）：`cargo check-headless`（host）与 `cargo check-android`（aarch64-linux-android；⚠ zstd-sys 等 C 依赖的 build script 需 NDK clang，本机无 NDK 时由 CI 承接）。数据类型一律从 `wind-ui-types` 引；**headless 路径不得新增对 wind-ui 的引用**（CI 有 `cargo tree` 断言）。

### Testing Requirements
- **host 可直接 `cargo test -p wind-coordinator`**（Windows/macOS host 全量；`windows` crate 是 `cfg(windows)` 依赖，非对应平台不参与编译）。历史说法「传递依赖 windows 不能 host 测」已随 wind-ui 解耦作废。
- ⚠️ 集成测试依赖 `build_dev/data`，缺失时**静默跳过且计数照绿**——以 `--test input_flow` 耗时 ≥1s 为数据在位判据（见根 AGENTS.md）。
- 纯逻辑函数（`en_case_variants`/`parse_pairs`/`punct_char` 等）逻辑独立，经无头构造器（`new_headless` 族）覆盖。

## Dependencies

### Internal
- `wind-ipc`（协议常量/键 hash）、`wind-bridge`（MessageHandler/KeyEventData/Push）、`wind-config`、`wind-store`（redb 持久化）、`wind-dict`、`wind-engine`、`wind-candidate`、`wind-transform`、`wind-theme`、`wind-ui-types`（UiCommand/UiEvent 等表现层协议）、`wind-ui`（**optional，desktop-ui feature**：UiManager/剪贴板/macOS forwarder）、`wind-cmdbar`、`wind-phrase`、`wind-keys`（keymap/VK/NavKeys）、`wind-quick-input`、`wind-reverse`、`wind-aux-code`（辅助码表懒加载 `ensure_aux_code_table`/`ModeKind::AuxCode` 筛选态）、`wind-punct`

### External
- `tracing`、`anyhow`、`serde`/`serde_json`、`toml`、`chrono`、`fontdb`；`windows`（仅 `cfg(windows)`）。无 tokio（2026-07 移除，全 workspace 同步线程模型）

## 全局约束
按需引用根 `AGENTS.md`：VK 用 `keymap::VK_*`（来自 wind-keys，禁裸十六进制）；候选导航键走统一入口（`apply_nav_key`/`NavKeys`）；提交只用显式路径（禁 `git add -A`）；改完在 `wind_input/` 跑 `cargo fmt` 并与逻辑改动分开提交；日志 INFO 级不得含用户输入/候选/词库内容。

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->
