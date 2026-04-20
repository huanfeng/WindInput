<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# editor

## Purpose
各类数据文件的编辑器包。提供统一的编辑器接口（`Editor`）及基础实现（`BaseEditor`），支持文件状态跟踪、并发保护和脏数据标记。目前实现 `ConfigEditor`，可扩展用于词库编辑器。

## Key Files
| 文件 | 说明 |
|------|------|
| `base.go` | 编辑器基础类型：`Editor` 接口定义、`BaseEditor` 结构体实现、文件状态跟踪和脏数据管理 |
| `config.go` | 配置文件编辑器：`ConfigEditor` 实现，包装 `wind_input/pkg/config.Config`，提供 Load/Save/Reload/GetConfig/SetConfig/UpdateConfig 方法 |

## For AI Agents
### Working In This Directory
- 所有编辑器均实现 `Editor` 接口，包含 Load/Save/HasChanged/Reload/IsDirty 五个方法
- `BaseEditor` 使用 `sync.RWMutex` 保护并发访问，调用所有方法时自动加锁
- `UpdateFileState()` 必须在 Load/Save 后手动调用，更新文件状态快照以供后续变化检测
- `MarkDirty()` 标记未保存修改，`ClearDirty()` 清除标记（通常由 Save 方法调用）
- `IsDirty()` 检查是否有本地修改（不涉及外部文件变化）
- `HasChanged()` 检查文件是否被外部程序修改（需比较文件状态快照）

### 编辑器工作流
```go
// 初始化
editor := NewConfigEditor()

// 加载数据
err := editor.Load()  // 加载配置，更新文件状态快照

// 修改数据
editor.UpdateConfig(func(cfg *config.Config) {
    cfg.Engine.Type = "pinyin"
})

// 检查修改状态
if editor.IsDirty() {
    // 有未保存修改
}

// 检查外部修改
changed, err := editor.HasChanged()
if changed {
    // 外部程序修改了文件，需要处理冲突
}

// 保存数据
err := editor.Save()  // 保存配置，清除 dirty 标记，更新文件状态快照
```

### 新增编辑器
若需支持新的文件类型（如短语、用户词库等），遵循以下步骤：
1. 创建 `xxxeditor.go` 文件
2. 定义 `XxxEditor struct { *BaseEditor; data *XxxType }` 嵌入 `BaseEditor`
3. 实现 `Editor` 接口的五个方法
4. 在 Load/Save 后调用 `e.UpdateFileState()` 更新快照

### Testing Requirements
- `go build ./internal/editor/...`
- `go fmt ./internal/editor/...`
- 测试 Load/Save/HasChanged/Reload/IsDirty 等方法的并发安全性

### Common Patterns
```go
// ConfigEditor 使用示例
editor, _ := editor.NewConfigEditor()
_ = editor.Load()

// 读取配置
cfg := editor.GetConfig()

// 修改配置（标记为 dirty）
editor.UpdateConfig(func(cfg *config.Config) {
    cfg.Engine.Type = "pinyin"
})

// 保存配置
_ = editor.Save()

// 检查是否被外部修改
changed, _ := editor.HasChanged()
if changed {
    _ = editor.Reload()  // 重新加载，丢弃本地修改
}
```

## Dependencies
### Internal
- `github.com/huanfeng/wind_input/pkg/config` — Config 结构体和加载/保存函数
- `github.com/huanfeng/wind_input/pkg/fileutil` — FileState 和 GetFileState()

### External
- 标准库：`sync`、`time`

<!-- MANUAL: -->
