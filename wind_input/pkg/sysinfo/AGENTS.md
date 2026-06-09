<!-- Parent: ../AGENTS.md -->
<!-- Updated: 2026-06-09 -->

# pkg/sysinfo

## Purpose
运行时系统资源探测。当前仅提供物理内存查询，用于在低内存机器上让高内存操作（首次安装时生成 wdat/wdb 词库，峰值可超 1GB）切换到省内存路径，避免 8GB 内存机器 OOM。

## Key Files
| File | Description |
|------|-------------|
| `memory.go` | 跨平台逻辑：`AvailablePhysicalMB()`、`LowMemoryMode()`，以及阈值/环境变量决策 |
| `memory_windows.go` | Windows 实现：`GlobalMemoryStatusEx`（自声明 `MEMORYSTATUSEX` 结构）读取可用物理内存 |
| `memory_other.go` | 非 Windows 占位：`availablePhysicalBytes` 返回 0（未知） |

## For AI Agents

### Working In This Directory
- `LowMemoryMode()` 决策优先级：①环境变量 `WINDINPUT_FORCE_LOWMEM`（1/true/on 强开，0/false/off 强关）→ ②可用物理内存 < 阈值（默认 1024MB，`WINDINPUT_LOWMEM_MB` 覆盖）→ ③探测失败（返回 0）时返回 **false**，保持原快速路径，行为不变
- 「未知即快速路径」是核心兜底：非 Windows 平台、或 `GlobalMemoryStatusEx` 调用失败时一律不启用省内存模式，确保不引入回归
- 修改 `MEMORYSTATUSEX` 结构体务必保持字段顺序/布局与 MSDN 一致，并正确设置 `Length = sizeof`，否则读到的内存值无意义
- 调用方目前为 `internal/dict/dictcache`（convert.go 三个转换函数）

### Testing Requirements
- `go test ./pkg/sysinfo/`（当前无单测；逻辑分支可通过设置 `WINDINPUT_FORCE_LOWMEM` 验证）
- 内存阈值行为建议在真实低内存环境或用 `WINDINPUT_LOWMEM_MB` 调高阈值模拟

### Common Patterns
- 高内存操作入口处 `if sysinfo.LowMemoryMode() { /* 启用省内存路径 */ }`

## Dependencies

### Internal
- 无

### External
- `golang.org/x/sys/windows` — `NewLazySystemDLL` 加载 kernel32

## 全局约束
- 枚举与魔法字符串约束：见 [`/docs/design/enum-constraint.md`](../../../docs/design/enum-constraint.md)。

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
