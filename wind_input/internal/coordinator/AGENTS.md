<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# internal/coordinator

## Purpose
核心协调器，是整个输入法服务的"大脑"。实现 `bridge.MessageHandler` 接口，接收 C++ TSF 桥接层的所有事件，协调引擎、UI、词库的交互，维护完整的输入状态机。

## Key Files
| File | Description |
|------|-------------|
| `coordinator.go` | `Coordinator` 结构体定义、构造函数、状态广播、信号通道（退出/重启） |
| `handle_key_event.go` | 按键事件主入口，根据模式分发处理 |
| `handle_key_action.go` | 具体按键动作处理（退格、确认、翻页、数字选词等） |
| `handle_candidate_action.go` | 候选词快捷键操作：`matchCandidateActionKey` 匹配 Ctrl+数字/Ctrl+Shift+数字热键；`handleDeleteCandidateByKey` 删除指定候选词（写入 ShadowLayer.Delete）；`handlePinCandidateByKey` 置顶指定候选词（写入 ShadowLayer.Pin 或 PhraseLayer.MoveToTop） |
| `handle_candidates.go` | 候选词请求引擎计算、分页管理、UI 更新 |
| `handle_config.go` | 配置更新处理（引擎切换、热键、UI、工具栏等） |
| `handle_config_menu.go` | 右键菜单命令处理 |
| `handle_config_state.go` | 状态查询方法（`GetChineseMode`、`GetCurrentEngineName` 等） |
| `handle_lifecycle.go` | 焦点获得/失去、IME 激活/停用、客户端断连 |
| `handle_mode.go` | 中英文模式切换、CapsLock 状态处理 |
| `handle_punctuation.go` | 中英文标点转换处理 |
| `handle_temp_english.go` | 临时英文模式：五笔输入态下按特定键（如 Z）触发，输入英文后恢复；维护临时英文缓冲区和上屏逻辑 |
| `handle_temp_pinyin.go` | 临时拼音模式：五笔方案下临时切换到拼音输入；通过 `engine.Manager.ActivateTempPinyin`/`DeactivateTempPinyin` 管理拼音词库层的注入与退出 |
| `handle_ui_callbacks.go` | UI 回调（工具栏按钮点击、候选窗口鼠标事件） |
| `handle_addword.go` | 快捷加词功能：`enterAddWordMode`/`exitAddWordMode` 管理加词模式进出；`handleAddWordKey` 在加词模式下处理 ↑↓/Enter/Esc/Ctrl+Enter；`confirmAddWord` 将词条写入 UserDict；`openAddWordDialog` 打开设置页加词对话框；`calcWordCodeForCurrentSchema` 根据编码规则和反向索引自动计算词的编码 |
| `input_history.go` | `InputHistory`：按客户端 ID 隔离的上屏记录器；`Record` 追加记录并裁剪至 maxChars；`GetRecentChars` 提取最近 N 个字符（正序），用于加词推荐；`GetRecentRecords` 返回最近记录（最新在前）；仅内存存储，不持久化（有测试文件 `input_history_test.go`） |
| `confirmed_segments_test.go` | 已确认分段逻辑测试 |

## For AI Agents

### Working In This Directory
- `Coordinator` 用单个 `sync.Mutex`（`c.mu`）保护所有状态，所有公开方法都加锁
- 状态广播（`broadcastState`）：先更新工具栏 → 再 Push 到所有 TSF 客户端；广播前释放锁避免死锁
- 有效模式（`EffectiveMode`）：CapsLock 开启时无论中英文模式均为英文大写
- 退出/重启通过包级 channel 信号（`ExitRequested()`/`RestartRequested()`），`main.go` 监听
- 热键编译结果缓存（`cachedKeyDownHotkeys`），配置变更时置 `hotkeysDirty=true` 触发重新编译
- 运行时状态（中英文、全角、中文标点）在 `startup.remember_last_state=true` 时从 `config.RuntimeState` 恢复
- 临时拼音模式（`handle_temp_pinyin.go`）通过 `engine.Manager.ActivateTempPinyin` 向 `CompositeDict` 注入拼音词库层，退出时 `DeactivateTempPinyin` 卸载，防止拼音词库污染五笔查询
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
