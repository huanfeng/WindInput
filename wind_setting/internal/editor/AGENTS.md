<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# editor

## Purpose
提供配置文件和词库文件的内存编辑器。每种数据文件对应一个编辑器类型，所有编辑器均嵌入 `BaseEditor` 获得统一的文件状态跟踪、脏标记和并发保护。编辑器负责加载、修改、保存和外部变化检测，不直接处理 HTTP 或 IPC 通信。

## Key Files
| 文件 | 说明 |
|------|------|
| `base.go` | `Editor` 接口定义；`BaseEditor` 实现（文件路径、`sync.RWMutex`、`fileutil.FileState` 快照、dirty 标记） |
| `config.go` | `ConfigEditor`：读写 `config.Config`，路径由 `config.GetConfigPath()` 确定 |
| `phrase.go` | `PhraseEditor`：读写 `dictfile.PhrasesConfig`，支持增删短语和计数 |
| `shadow.go` | `ShadowEditor`：读写 `dictfile.ShadowConfig`，支持置顶/删除/调权重规则 |
| `userdict.go` | `UserDictEditor`：读写用户词库，按引擎类型（wubi/pinyin）选择不同文件路径；支持搜索、导入、导出 |

## For AI Agents
### Working In This Directory
- 所有编辑器构造函数命名为 `New<Type>Editor()`，返回 `(*<Type>Editor, error)`
- `HasChanged()` 通过对比当前文件 mtime/size 快照与上次加载时的快照判断外部修改
- 保存后须调用 `UpdateFileState()` 更新快照，否则下次 `HasChanged()` 会误报
- `UserDictEditor` 支持运行时切换引擎：`NewUserDictEditorForEngine(engineType string)`，引擎类型为 `"wubi"` 或 `"pinyin"`
- `ShadowEditor` 便捷方法：`TopWord`（置顶）、`DeleteWord`（隐藏）、`ReweightWord`（调权重）

### Testing Requirements
- `go build ./internal/editor/...`
- `go fmt ./internal/editor/...`
- 功能验证须配合实际词库文件路径（由 `wind_input/pkg/config` 解析）

### Common Patterns
```go
// 标准编辑器使用模式
editor, err := editor.NewConfigEditor()
if err == nil {
    editor.Load()
}
// 修改后保存
editor.SetConfig(cfg)
editor.Save()           // 保存并更新 FileState
editor.HasChanged()     // 检查外部是否修改
```

## Dependencies
### Internal
- `github.com/huanfeng/wind_input/pkg/config` — 路径解析、Config 类型、加载/保存函数
- `github.com/huanfeng/wind_input/pkg/dictfile` — PhrasesConfig、ShadowConfig、UserDictData 类型及操作函数
- `github.com/huanfeng/wind_input/pkg/fileutil` — `FileState`（文件快照）、`GetFileState()`

### External
- 标准库：`sync`、`time`

<!-- MANUAL: -->
