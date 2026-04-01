<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# internal

## Purpose
`wind_setting` 的 Go 内部包，不对外暴露。包含两个子包：`editor`（配置和词库的文件编辑器）和 `filesync`（文件变化监控）。这些包被 `app*.go` 中的 `App` 结构体直接使用。

## Key Files
无顶层文件，全部实现在子目录中。

## Subdirectories
| 目录 | 说明 |
|------|------|
| `editor/` | 各类数据文件的读写编辑器（配置、短语、Shadow、用户词库） |
| `filesync/` | 文件状态快照与变化检测，用于感知外部修改 |

## For AI Agents
### Working In This Directory
- 所有包均为 `wind_setting` 模块私有（`internal/`），不能被外部模块引用
- 新增子包时遵循现有结构：接口定义在 `base.go`，具体实现各自独立文件

### Testing Requirements
- 运行 `go build ./internal/...` 验证编译
- 运行 `go fmt ./internal/...` 格式化

### Common Patterns
- 编辑器均嵌入 `BaseEditor`，获得统一的文件状态跟踪和并发保护
- 所有公开方法通过 `sync.RWMutex` 保护并发访问

## Dependencies
### Internal
- `github.com/huanfeng/wind_input/pkg/config`
- `github.com/huanfeng/wind_input/pkg/dictfile`
- `github.com/huanfeng/wind_input/pkg/fileutil`

### External
- 标准库：`sync`、`time`

<!-- MANUAL: -->
