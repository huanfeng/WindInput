<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-05-01 | Updated: 2026-05-01 -->

# internal/perf

## Purpose
按键链路性能采样的内存环形缓冲。每次 `handleAlphaKey` 完成后由 coordinator 写入一条 `Sample`，包含 total / updateCandidatesEx / engine 各 phase / showUI 等阶段耗时。可通过 RPC `System.DumpPerf` 主动导出为 JSON Lines 文件，定位首键冷启动、引擎查询、UI 渲染等流畅性瓶颈。

不依赖任何业务包；不涉及落盘自动化（仅在显式调用 `ExportJSONL` 时落盘）。

## Key Files
| File | Description |
|------|-------------|
| `perf.go` | `Sample`/`EngineTiming`/`Stats` 类型；全局 `Record`/`Snapshot`/`Clear`/`SetCapacity`/`Capacity`/`ComputeStats`/`FormatStats`/`ExportJSONL` |
| `perf_test.go` | 环形缓冲覆写、首键/续键统计分位数、JSONL 导出文件结构 |

## Public API
- `Record(Sample)` — 线程安全 O(1) 写入；超过容量丢弃最旧
- `Snapshot()` — 拷贝当前缓冲（最旧→最新）
- `Clear()` / `SetCapacity(n)` / `Capacity()`
- `ComputeStats()` → `Stats{First, Continuation, All}`，每组含 P50/P95/P99/Max/Avg
- `FormatStats(Stats) string` — 单行可读摘要
- `ExportJSONL(path) (count, err)` — 首行 header（含 stats），后续每行一个 Sample

## For AI Agents

### Working In This Directory
- 默认容量 512 条样本，约 50KB；命中率优先于精度（`Sample.Engine` 字段缺省按 0 处理）
- `Sample.Input` 含用户输入编码——这是 DEBUG 用途的取舍；`ExportJSONL` 必须由用户/RPC 主动触发，不要做自动落盘
- 全局单例（`global` 包级变量），不要在测试中并发不同 capacity；测试用例需先 `Clear()` 再 `SetCapacity()`

### Testing Requirements
- 运行：`go test ./internal/perf`

## Dependencies
### Internal
- 无

### External
- 标准库（`encoding/json`、`os`、`sort`、`sync`、`time`）

<!-- MANUAL: -->
