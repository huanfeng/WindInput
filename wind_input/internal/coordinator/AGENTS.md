<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-06-09 -->

# internal/coordinator

## Purpose
核心协调器，是整个输入法服务的"大脑"。实现 `bridge.MessageHandler` 接口，接收 C++ TSF 桥接层的所有事件，协调引擎、UI、词库的交互，维护完整的输入状态机。

## Key Files
| File | Description |
|------|-------------|
| `coordinator.go` | `Coordinator` 结构体定义、构造函数、状态广播、信号通道（退出/重启）；`NotifySchemaActivated(displayName)` 供外部异步资源就绪后调用，触发 toolbar/TSF 状态同步并显示"<方案>已就绪"指示器 |
| `handle_key_event.go` | 按键事件主入口，根据模式分发处理；正在输入（buffer 非空 / 有候选）时按触发键先走统一优先级回落链 `routeBufferedTriggerKey`（见 `mode_trigger.go`），buffer 空场景仍走旧 `getXxxTriggerKey` |
| `mode_trigger.go` | 触发键激活模式的统一优先级回落链：`decideBufferedTrigger`（纯决策，无副作用）+ `routeBufferedTriggerKey`（执行）+ `enterModeCommitting`（顶码上屏当前高亮候选后原子进入模式，复用 `doSelectCandidate` + `HasNewComposition`）+ `triggerModes()` **动态**有序模式表（快捷输入 > 临时拼音 > **引导键特殊模式 N 个实例**（`special:<id>`，按 `Input.SpecialModes` 配置顺序）> 临时英文）+ `matchTriggerKeyInList` 公共键匹配。详见 docs/design/mode-trigger-priority-chain.md 与 docs/design/special-mode-codetable.md |
| `handle_key_action.go` | 具体按键动作处理（退格、确认、翻页、数字选词等） |
| `handle_candidate_action.go` | 候选词快捷键操作：`matchCandidateActionKey` 匹配 Ctrl+数字/Ctrl+Shift+数字热键；`handleDeleteCandidateByKey` 删除指定候选词（走 `dm.DeleteWord(code, text, cand.ID)`）；`handlePinCandidateByKey` 置顶指定候选词（走 `dm.PinWord(code, text, cand.ID, 0)`）；**R2**: 短语候选统一走 Shadow（不再走 PhraseLayer.MoveToTop） |
| `handle_candidates.go` | 候选词请求引擎计算、分页管理、UI 更新；候选数分档：`refreshEffectivePerPage` 在每条分页源头把生效每页数物化到 `c.candidatesPerPage`（基础档 `candidatesPerPageBase` / 扩展档 `candidatesPerPageExtended`），`shouldUseExtendedCandidates` 是新增"扩展档"场景的唯一对接点 |
| `handle_config.go` | 配置更新处理（引擎切换、热键、UI、工具栏等） |
| `handle_config_menu.go` | 右键菜单命令处理 |
| `handle_config_state.go` | 状态查询方法（`GetChineseMode`、`GetCurrentEngineName` 等） |
| `handle_lifecycle.go` | 焦点获得/失去、IME 激活/停用、客户端断连；含 `HandleCommitRequest`（barrier 机制，见下方说明） |
| `toolbar_visibility.go` | 工具栏位置计算 (`computeToolbarPositionLocked`) + ShellHook 全屏事件转发 (`OnShellFullscreenChange`)；显隐决策已迁出，本文件仅做位置算子与事件适配 |
| `toolbar_reducer.go` | **工具栏显隐单点决策器**：`toolbarReducer` goroutine 接收 7 类事件（IME activate/deactivate、AllClientsDisconnected、user preference、fullscreen、config、caret、content refresh），50ms debounce 合并 burst，按公式 `imeActivated && userWantsVisible && !(fullscreen && hideInFullscreen)` 决策；`sendCritical` (阻塞 100ms) / `sendNonBlocking` (drop) 两种投递；状态机字段仅 reducer goroutine 访问；`snapshotToolbarShowParams` 在 Coordinator 上短临锁取位置 + ToolbarState |
| `handle_mode.go` | 中英文模式切换、CapsLock 状态处理 |
| `handle_punctuation.go` | 中英文标点转换处理；自动配对 (`getAutoPairTracker` 中文模式 / `handleEnglishModeAutoPair` IME 英文模式) ——智能跳过 + 插入配对回退光标，英文模式配对受 `englishModeAutoPairInGo` 平台常量 gate |
| `english_pair_darwin.go` / `english_pair_other.go` | 平台常量 `englishModeAutoPairInGo`：darwin=true（macOS 无 C++ TSF 层，IME 英文模式成对标点由 Go 的 `handleEnglishModeAutoPair` 接管），非 darwin=false（Windows 英文配对由 C++ 处理，Go 透传不重复）。配对的光标移动在 macOS 经 `MoveCursorRight`/`InsertTextWithCursor.CursorOffset` 下发，IMKit `.app` 用 CGEvent 合成方向键（需辅助功能授权） |
| `handle_temp_english.go` | 临时英文模式：五笔输入态下按特定键（如 Z）触发，输入英文后恢复；维护临时英文缓冲区和上屏逻辑 |
| `handle_temp_pinyin.go` | 临时拼音模式：五笔方案下临时切换到拼音输入；通过 `engine.Manager.ActivateTempPinyin`/`DeactivateTempPinyin` 管理拼音词库层的注入与退出 |
| `handle_special_mode.go` | 引导键特殊模式（自定义码表，2026-06-09）：仿 `handle_quick_input.go` 的一次性模式状态机，查实例 `*dict.CodeTable` 而非主引擎。`setupSpecialMode`/`handleSpecialModeKey`/`updateSpecialCandidates`(三档自动上屏:`decideSpecialAutoCommit`)/`buildSpecialUICandidates`(走 `ValueExpander.ExpandToCandidates` 展开 `$CC`/`$X`/`$AA`/`$SS`)/`selectSpecialCandidate`/`commitSpecialCommand`(复用 `commitCmdbarCandidate`)/`exitSpecialMode`/`showSpecialUI`。`triggerKeyToChar` 从 `quickInputPrefix` 抽出供两模式共用。`schemasDirs()` = [用户配置目录/schemas, dataRoot/schemas] 两处解析（与 DiscoverSchemas 同源）。**动态分级加载**（对标主流程 `expandCandidates`）：初始 `specialInitialCandidateLimit=100`，翻页到末 2 页且 `specialHasMore` 时 `expandSpecialCandidates` 翻倍重查（`LookupPrefix`/`AllCandidates`，封顶 `specialMaxCandidateLimit=5000`），`showSpecialUI` 用负 `totalPages` 告知渲染层还有更多。模式内遵循标准输入流程：可打印符号/引导符 → `specialPunctCommit` 顶屏高亮+输出符号；2/3 候选键选词。失焦重放（`replayBufferLen`/`getPendingBufferText`/焦点重建）与热重载（`UpdateInputConfig` 重建 registry）均已对齐临时模式 |
| `special_mode_registry.go` | 特殊模式实例注册表 `specialModeRegistry`：按全局 `Input.SpecialModes` 配置顺序持有实例，`match`(触发键命中)/`get`(按 id)/`ensureLoaded`(Rime 码表懒加载,复用 `dictcache.ConvertRimeCodetableToWdb`+`CodeTable.LoadBinary`,带 wdb 缓存)；无效实例跳过+WARN，触发键去重靠前者优先 |
| `special_mode_decide.go` | `decideSpecialAutoCommit(strategy,fixedLength,bufLen,candCount,hasLonger)` 纯函数：`prefix_free`(唯一候选且无更长前缀)/`fixed_length`(达码长且唯一)/`manual`(从不) |
| `handle_ui_callbacks.go` | UI 回调（工具栏按钮点击、候选窗口鼠标事件）；**R2**: `handleCandidateMove(index, delta, top)` 统一前移/后移/置顶，Pin/Delete/Reset 均传 `cand.ID`；旧短语辅助方法 `handlePhraseMoveUp/Down/ToTop/Reset` 已删除 |
| `handle_addword.go` | 快捷加词功能：`enterAddWordMode`/`exitAddWordMode` 管理加词模式进出；`handleAddWordKey` 在加词模式下处理 ↑↓/Enter/Esc/Ctrl+Enter；`confirmAddWord` 将词条写入 UserDict；`openAddWordDialog` 打开设置页加词对话框；`calcWordCodeForCurrentSchema` 根据编码规则和反向索引自动计算词的编码 |
| `input_history.go` | `InputHistory`：按客户端 ID 隔离的上屏记录器；`Record` 追加记录并裁剪至 maxChars；`GetRecentChars` 提取最近 N 个字符（正序），用于加词推荐；`GetRecentRecords` 返回最近记录（最新在前）；仅内存存储，不持久化（有测试文件 `input_history_test.go`） |
| `cmdbar_services.go` | 命令直通车 (cmdbar) 的 Services 适配层：把 `internal/clipboard` / `internal/proc` / `engineMgr.GetDictManager().AddUserWord` / `uiManager.OpenSettingsWithPage` 封装成 cmdbar 期望的接口；`buildCmdbarServices` 在 `NewCoordinator` 中装配并写入 `c.cmdbarServices`; `cmdbarProcService.ShellEx` 透传 `proc.ShellEx` 支持 shell flag (`term`/`pwsh`)。`cmdbarKeysService`/`cmdbarClipService` 持有 `*Coordinator`, 其按键注入 (Tap/Sequence/Hold/Release/TypeText) 与 `clip.paste` 的 `Paste` 实现按平台拆到 `cmdbar_inject_darwin.go` / `cmdbar_inject_other.go` |
| `cmdbar_inject_darwin.go` | (`//go:build darwin`) cmdbar 按键注入的 macOS 实现: 用 `keyinject.Parse` 规范化键名/修饰键后, 调 `uiManager.SendKeyTap/SendKeySeq/SendKeyHold/SendKeyRelease/SendKeyType` 下发 push 命令交 IMKit `.app` 用 CGEvent (tap/seq/hold/release) 或 `insertText` (type) 执行; `Paste` 读 `clipboard.GetText` 经 `SendKeyType` 上屏 (免辅助功能授权, 剪贴板空时静默返回) |
| `cmdbar_inject_other.go` | (`//go:build !darwin`) 同名方法的 Win/默认实现: keys 走 `internal/keyinject` 本地真实合成, `Paste` 走 `keyinject` Tap(`Ctrl+V`) |
| `cmdbar_postprocess_test.go` | `applyValueExpansion` 单测: 验证 `$CC` 命令、`$X` 模板、普通候选三类分流, 以及 PhraseLayer 来源候选 (PhraseTemplate != "") 跳过避免双重展开 |
| `collapse_groups_test.go` | `collapseGroupMembersIfMixed` 单测: 唯一 group 保展开 / 混合候选 collapse / 多 group 各 collapse / `expandedGroupTemplate` 仅保留命中 group / 空切片与边界 |
| `nav_expand_test.go` | nav 二级展开状态机集成测试: `doSelectCandidate` 对 collapsed nav (cand.GroupCode == inputBuffer) 与 prefix nav 分别置 `expandedGroupTemplate`; `clearState` / `handleAlphaKey` 清零状态机字段 |
| `cmdbar_context.go` | `cmdbarEvalContext` 实现 `cmdbar.EvalContext`，给求值器提供 input/history/clip/env/services 取值；Sel/App/Title 暂为占位空串（P5 接 Win32）；Clip(n>1) 暂返回空（P5 接剪贴板栈） |
| `reload_handler.go` | `ReloadHandler` 接口实现，供 `internal/control` 调用配置热重载 |
| `confirmed_segments_test.go` | 已确认分段逻辑测试 |

## For AI Agents

### Working In This Directory
- `Coordinator` 用单个 `sync.Mutex`（`c.mu`）保护所有状态，所有公开方法都加锁
- 状态广播（`broadcastState`）：仅刷新工具栏内容（mode/punct/fullwidth 等）+ Push 到所有 TSF 客户端，**不**参与显隐决策
- **工具栏显隐**统一走 `c.toolbarReducer`：任何想 Show/Hide 工具栏的入口（IME activate/deactivate、AllClientsDisconnected、user toggle、fullscreen、config reload）都只更新自己的状态字段 + 投递事件（`sendCritical` 关键事件 / `sendNonBlocking` 高频事件），**禁止**直接调 `uiManager.SetToolbarVisible` / `ShowToolbarWithState`。这是 2026-05-26 把"决策公式被复制 4 份"重构为单点决策的核心契约
- 有效模式（`EffectiveMode`）：CapsLock 开启时无论中英文模式均为英文大写
- 退出/重启通过包级 channel 信号（`ExitRequested()`/`RestartRequested()`），`main.go` 监听
- 热键编译结果缓存（`cachedKeyDownHotkeys`），配置变更时置 `hotkeysDirty=true` 触发重新编译
- 运行时状态（中英文、全角、中文标点）在 `startup.remember_last_state=true` 时从 `config.RuntimeState` 恢复
- 临时拼音模式（`handle_temp_pinyin.go`）通过 `engine.Manager.ActivateTempPinyin` 向 `CompositeDict` 注入拼音词库层，退出时 `DeactivateTempPinyin` 卸载，防止拼音词库污染五笔查询
- **模式激活优先级回落链**（`mode_trigger.go`，2026-06-09）：正在输入（buffer 非空 / 有候选）时按触发键，`handle_key_event.go` 入口先调 `routeBufferedTriggerKey`，按 `decideBufferedTrigger`（纯函数，可单测）走优先级链：① 双拼韵母键 → 送引擎 ② 二候选键(候选≥2) ③ 三候选键(候选≥3) ④ **模式激活键 → 顶码上屏当前高亮候选 + 进模式** ⑤ overflow ⑥ 标点。同一键身兼多职时按此优先级裁决（如 `;` 候选足够选候选、不足则回落进模式）。模式间顺序由 `triggerModes()` 定义：**快捷输入 > 临时拼音 > 引导键特殊模式（自定义码表，N 个实例）> 临时英文**；特殊模式已实现（见 `handle_special_mode.go` / docs/design/special-mode-codetable.md），由 `Input.SpecialModes` 配置驱动、动态插在临时拼音之后、临时英文之前。各模式拆为 `matchXxxTrigger`（纯匹配 + enabled，**临时拼音排除 z**）+ `setupXxxMode`（状态设置）。**buffer 空场景仍走旧 `getXxxTriggerKey`**（保留 z 键首次触发临时拼音逻辑，`zHybridFallback` 只管已以 z 开头的回退）。改动这条链或新增模式前必读 docs/design/mode-trigger-priority-chain.md
- 命令直通车 (cmdbar) 接入：`NewCoordinator` 末尾构造 `cmdbarHistory`/`cmdbarServices` 并通过 `installCmdbarPhraseHook` 把解析+求值闭包注入到当前 schema 的 `PhraseLayer.SetCmdbarHook`, 并构造 `c.cmdbarValueExpander` 用于候选后处理。`recordCommit` 同步 `cmdbarHistory.Push(text)` 让 `last()` 可见上一次上屏；`doSelectCandidate` 检测候选 `Actions` 非空时返回 `ResponseTypeClearComposition` 并在 goroutine 内顺序执行动作（错误只记 WARN 元数据，不带内容）。`handlePinCandidateByKey` 拒绝对 cmdbar 动作候选置顶。短语 value **不含** cmdbar marker (`$CC(`/`$CC1(`) 时仍走旧 `templateEngine.Expand` 路径，零行为变更。
- 命令前缀可见性: `$CC(...)` 短语仅在精确编码匹配时出现; `$CC1(...)` 同时参与前缀匹配。该语义由 `dict.IsCmdbarExactOnly` + 各 `SearchPrefix` 尾部 `filterCmdbarExactOnly` 共同实现, coordinator 侧无需配置/热更新入口 (旧 `Phrase.CmdbarPrefixNav` 已彻底删除)。
- 候选后处理: `updateCandidatesEx` 在把 engine 返回的候选转 UI 候选之前, 调 `applyValueExpansion(*candidate)` 用同一个 `ValueExpander` 统一展开 `$CC(`/`$CC1(`/`$X`; PhraseLayer 出口候选 (`PhraseTemplate != ""`) 跳过, 普通候选用 `strings.IndexByte(text, '$') < 0` 早跳。
- **`$AA`/`$SS` 数组候选 collapse 状态机** (2026-05-17 引入, 2026-05-18 升级为 `expandedGroupTemplate` 字段)：当字符组成员候选与其它来源候选 (码表/拼音/普通短语) 混合, **或同 code 多 group** 时, `collapseGroupMembersIfMixed` 把每个 `GroupTemplate` 的全部 member 替换为 1 条 nav 候选 (`IsGroup=true`, Text=GroupName, Comment="(N 项)"), 避免数组成员 weight 难处理 + UI 混排噪音。分组 key 用 `GroupTemplate` (= group 原 PhraseRecord.Text), 让同 code 多 $AA/$SS 也能各自 collapse 为独立 nav。状态机三条 invariant: (1) 用户选中 nav 时, `doSelectCandidate` 置位 `expandedGroupTemplate = cand.GroupTemplate`; (2) 任何 buffer 变化入口 (handleAlphaKey / handleBackspace / handleDelete / popConfirmedSegment / 拼音分隔符 / 拼音分步确认 / 剪贴板编码替换 / lifecycle 重置) 清零 `expandedGroupTemplate`; (3) `clearState` 也清零。下一次 `updateCandidatesEx` 在 collapse 决策时检查 `expandedGroupTemplate` 命中即仅保留该 group 的 member, 让字符组保持展开为成员形态。
- **collapse 后 ApplyShadowPins 二次应用** (2026-05-18 引入, bug 2 修复): `updateCandidatesEx` 在 `collapseGroupMembersIfMixed` 之后**追加一次** `dict.ApplyShadowPins(c.inputBuffer, ...)` 调用。原因: nav (E 类型) 是 coordinator 在引擎 Phase 6 之后 collapse 出来的, 引擎层看不到 nav, 用户对 nav 的 pin 在 Phase 6 阶段无候选可匹配。collapse 后 nav 已带稳定 ID (`PhraseCandidateID(groupCode, GroupTemplate)`, 由 first member 继承), 二次 apply 能命中并放置。`ApplyShadowPins` 幂等 (规则一致 → 位置一致), 引擎层已 pin 的非 nav 候选不被破坏。详见 docs/design/candidate-actions.md §3.2
- **候选调整目标矩阵** 见 docs/design/candidate-actions.md (无日期文件名, 6 类候选 × 4 操作 + Shadow 时序 + 实现取舍)。修改 `handle_candidate_action.go` / `handle_ui_callbacks.go` / `window_mouse.go` 候选调整逻辑前必读
- **F (cmdbar Actions) 跟 C (普通短语) 一致** (2026-05-18): 旧 `handle_candidate_action.go` 用 `len(cand.Actions) > 0` disable pin/delete 已删, 走 phrase id 路径; 右键菜单同样允许。详见 candidate-actions.md §6.4
- **HasShadow 字段语义收缩** (2026-05-18): `cand.HasShadow` 现在仅反映 Pinned 状态 (`HasShadowPin` 查询), 不再含 Deleted。跳过条件改为 `!cand.IsGroupMember` (从旧的 `!cand.IsGroup` 改, 让 nav 参与查询, bug 1 修复)
- 临时英文模式（`handle_temp_english.go`）独立维护一个缓冲区，不影响五笔输入缓冲区
- **加词模式**（`handle_addword.go`）：激活时设置占位 `inputBuffer = "\x00"` 让 C++ 侧进入 composition 状态以转发后续按键，加词完成/取消后调用 `exitAddWordMode` 清理
- **候选词操作**（`handle_candidate_action.go`）：`handleDeleteCandidateByKey`/`handlePinCandidateByKey` 内部会 `c.mu.Unlock()` 后执行词库 IO，再 `c.mu.Lock()` 重新获取锁；调用方须在持有锁时调用
- `inputHistory` 字段（`*InputHistory`）在每次上屏时通过 `inputHistory.Record` 更新，焦点切换时通过 `inputHistory.ClearClient` 清理

### Testing Requirements
- 协调器依赖 Windows UI 和 Named Pipe，集成测试需 Windows 环境
- `input_history_test.go` 可独立运行（无平台依赖）
- 状态机逻辑（模式切换、按键处理）可通过 mock `BridgeServer` 和 `engine.Manager` 做单元测试

### Common Patterns
- 所有 `handle_*.go` 文件中的方法属于 `Coordinator`，按功能拆分文件
- `clearState()` 清空输入缓冲区和所有临时状态，焦点丢失/模式切换时调用
- UI 更新通过 `uiManager` 方法调用（同步，但 UI 内部使用 channel 异步处理）

## Dependencies
### Internal
- `internal/bridge` — BridgeServer 接口、StatusUpdateData、KeyEventData 等类型
- `internal/engine` — 引擎管理器（含 Schema 驱动的方案切换）
- `internal/hotkey` — 热键编译器
- `internal/ipc` — VK_* 虚拟键码常量
- `internal/schema` — 方案信息查询（引擎类型判断）
- `internal/transform` — 标点转换
- `internal/ui` — UI 管理器
- `pkg/config` — 配置类型、RuntimeState
- `pkg/encoding` — `CalcWordCode`（加词编码计算）

### External
- 无

<!-- MANUAL: -->

## Barrier 机制（`HandleCommitRequest`）

**当前状态：Go 侧已实现，C++ 侧尚未接入，代码路径不可达。**

`handle_lifecycle.go` 中的 `HandleCommitRequest` 及其内部辅助函数（`handleSpaceInternal`、`handleEnterInternal`、`handleNumberKeyInternal`）是为 TSF barrier 机制预留的专用路径，设计用于在 WPS、Excel 等特殊宿主应用中以原子方式处理 Space/Enter/数字键上屏。

C++ 侧（`KeyEventSink.cpp`）已实现完整基础设施：
- `_SendCommitRequest()` — 构造并发送 `CmdCommitRequest`（0x0104）命令
- `_HandleCommitResult()` — 接收并处理 Go 返回的上屏结果
- `_CheckBarrierTimeout()` — 500ms 超时兜底（每次 `OnKeyDown` 检查）

但 `_SendCommitRequest` **从未被调用**，因此 Go 侧的 `HandleCommitRequest` 永远不会收到消息。

**接入时注意事项：**
- `handleSpaceInternal` 已使用 `c.selectedIndex` 正确处理高亮候选（2026-05-15 修复，修复前固定选当前页第一个候选）
- `handleNumberKeyInternal` 使用 `(c.currentPage-1)*c.candidatesPerPage + (num-1)` 计算正确页偏移
- 接入后需覆盖"翻页后按 Space/数字"的回归测试
