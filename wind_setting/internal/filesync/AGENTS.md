<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# filesync

## Purpose
文件变化监控包。`FileWatcher` 维护一组被监控文件的 `fileutil.FileState` 快照，支持轮询检测外部程序对文件的修改。当前实现中，`App` 使用轮询模式（前端主动调用 `CheckAllFilesModified`），`FileWatcher` 的 `Start`/`watchLoop` 提供可选的后台自动检测能力（目前未在 `app.go` 中启动）。

## Key Files
| 文件 | 说明 |
|------|------|
| `watcher.go` | `FileWatcher` 全部实现：Watch、Unwatch、UpdateState、CheckChanged、CheckAllChanged、Start/Stop 后台循环 |

## For AI Agents
### Working In This Directory
- `Watch(path)` 记录文件初始快照；`UpdateState(path)` 在应用自身写入后刷新快照（防误报）
- `Unwatch(path)` 用于切换引擎时移除旧词库文件的监控
- `CheckAllChanged()` 返回所有发生变化的文件路径 map，由 `App.CheckAllFilesModified()` 调用
- 后台 `Start(interval)` 目前未在生产代码中调用，可按需启用
- 所有方法均线程安全（`sync.RWMutex`）

### Testing Requirements
- `go build ./internal/filesync/...`
- `go fmt ./internal/filesync/...`

### Common Patterns
```go
watcher := filesync.NewFileWatcher()
watcher.Watch(filePath)          // 开始监控
watcher.UpdateState(filePath)    // 自身写入后更新快照
changed, _ := watcher.CheckChanged(filePath)  // 检查是否被外部修改
watcher.Stop()                   // 关闭（在 app.shutdown 中调用）
```

## Dependencies
### Internal
- `github.com/huanfeng/wind_input/pkg/fileutil` — `FileState`、`GetFileState()`

### External
- 标准库：`sync`、`time`

<!-- MANUAL: -->
