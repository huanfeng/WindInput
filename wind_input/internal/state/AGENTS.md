<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# internal/state

## Purpose
IME 状态管理器。集中管理输入法的运行时状态（中英文模式、全角、中文标点、CapsLock、工具栏可见、IME 激活），并在状态变更时通知注册的监听器。

注意：当前版本中 `coordinator` 包直接维护状态字段（未使用此包），`state.Manager` 为独立设计，可供未来重构使用。

## Key Files
| File | Description |
|------|-------------|
| `manager.go` | `IMEState`、`StateChangeType` 位掩码、`StateListener` 回调类型、`Manager` 实现 |

## For AI Agents

### Working In This Directory
- `StateChangeType` 使用位掩码，可组合多个变更类型（`StateChangeAll`）
- `Manager.SetState(newState, changeType)` 触发所有已注册监听器
- 若将来 `coordinator` 重构为使用此包，需注意 `Manager` 自带 `sync.RWMutex`

### Testing Requirements
- 纯 Go 逻辑，无平台依赖，可直接单元测试

### Common Patterns
- 监听器模式（Observer），通过 `AddListener` 注册回调

## Dependencies
### Internal
- 无

### External
- 无

<!-- MANUAL: -->
