<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# editor

## Purpose
提供配置文件和词库文件的内存编辑器。每种数据文件对应一个编辑器类型，所有编辑器均嵌入 `BaseEditor` 获得统一的文件状态跟踪、脏标记和并发保护。编辑器负责加载、修改、保存和外部变化检测，不直接处理 HTTP 或 IPC 通信。

## Key Files
| 文件 | 说明 |
|------|------|
| `base.go` | `Editor` 接口定义；`BaseEditor` 实现（文件路径、`sync.RWMutex`、`fileutil.FileState` 快照、dirty 标记、`loadTime`） |
| `config.go` | `ConfigEditor`：读写 `config.Config`，路径由 `config.GetConfigPath()` 确定 |
| `phrase.go` | `PhraseEditor`：读写 `dictfile.PhrasesConfig`，支持增删短语和计数 |
| `shadow.go` | `ShadowEditor`：读写 `dictfile.ShadowConfig`，支持 pin（固定位置）和 delete（隐藏）操作；按活跃方案 ID 确定文件名（`shadow_<schemaID>.yaml`） |
| `userdict.go` | `UserDictEditor`：读写用户词库，按方案 ID（schemaID）选择不同文件路径；支持搜索、导入、导出 |

## For AI Agents
### Working In This Directory
- 所有编辑器构造函数命名为 `New<Type>Editor()`，返回 `(*<Type>Editor, error)`
- `HasChanged()` 通过对比当前文件 mtime/size 快照与上次加载时的快照判断外部修改
- 保存后须调用 `UpdateFileState()` 更新快照，否则下次 `HasChanged()` 会误报
- `UserDictEditor` 支持运行时切换方案：`NewUserDictEditorForEngine(engineType string)`，方案 ID 如 `"wubi86"`、`"pinyin"`
- **Shadow 架构（2026-03-13 起）**：`ShadowEditor` 使用 pin + delete 两种操作，彻底移除了旧的 `TopWord`/`ReweightWord` 接口
  - `PinWord(code, word, position)` — 固定词条到指定位置（0 = 首位）
  - `DeleteWord(code, word)` — 隐藏词条（标记为 delete 规则）
  - `RemoveRule(code, word)` — 彻底移除 Shadow 规则
  - `GetRulesByCode(code)` — 查询指定编码的规则
  - `GetRuleCount()` — 获取规则总数
- `ShadowEditor` 构造函数：`NewShadowEditor()`（读当前活跃方案）或 `NewShadowEditorForSchema(schemaID)`（按方案 ID）

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

// Shadow 编辑器操作
shadowEditor, _ := editor.NewShadowEditor()
shadowEditor.Load()
shadowEditor.PinWord("sf", "村", 0)      // 固定"村"到首位
shadowEditor.DeleteWord("sf", "什")      // 隐藏"什"
shadowEditor.RemoveRule("sf", "什")      // 彻底移除规则
shadowEditor.Save()
```

## Dependencies
### Internal
- `github.com/huanfeng/wind_input/pkg/config` — 路径解析、Config 类型、加载/保存函数
- `github.com/huanfeng/wind_input/pkg/dictfile` — PhrasesConfig、ShadowConfig、UserDictData 类型及操作函数（含 `PinWord`、`DeleteWord`、`RemoveShadowRule`）
- `github.com/huanfeng/wind_input/pkg/fileutil` — `FileState`（文件快照）、`GetFileState()`

### External
- 标准库：`sync`、`time`

<!-- MANUAL: -->
