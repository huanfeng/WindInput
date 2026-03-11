<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# pkg/config

## Purpose
应用配置的完整定义、加载/保存逻辑、路径管理和运行时状态持久化。配置文件为 YAML 格式，存储在 `%APPDATA%\WindInput\config.yaml`。

## Key Files
| File | Description |
|------|-------------|
| `config.go` | `Config` 结构体（含所有子配置）、`Load()`、`SaveDefault()`，YAML 序列化标签 |
| `paths.go` | 路径常量和辅助函数（`GetConfigDir`、`GetPinyinDictPath`、`GetWubiDictPath` 等） |
| `config_hotkey.go` | `HotkeyConfig`：热键字符串配置（ToggleModeKeys、SwitchEngine 等） |
| `state.go` | `RuntimeState`：运行时状态持久化（中英文模式、全角、标点），`LoadRuntimeState`/`SaveRuntimeState` |

## For AI Agents

### Working In This Directory
- `Config` 顶层字段：`Startup`、`Dictionary`、`Engine`、`Hotkeys`、`UI`、`Toolbar`、`Input`、`Advanced`
- 新增配置项时：在对应子结构体添加字段，设置 YAML 标签，在 `SaveDefault()` 中提供默认值
- `RuntimeState` 与 `Config` 分开存储（`state.yaml`），避免用户编辑配置时覆盖运行时状态
- `GetPinyinDictPath()` 返回相对路径 `"dict/pinyin"`，在 `main.go` 中拼接 exeDir
- `GetWubiDictPath()` 返回相对路径 `"dict/wubi/wubi86.txt"`
- 配置热重载通过 `control` 管道触发，`coordinator.UpdateEngineConfig` 等方法应用变更

### Testing Requirements
- YAML 序列化/反序列化可做单元测试
- 路径函数在 Windows 环境测试（依赖 `os.UserConfigDir()`）

### Common Patterns
- 所有路径函数返回 `(string, error)`，调用方在错误时回退到 exeDir
- `FuzzyPinyinConfig` 包含 11 个独立开关，都可独立启用

## Dependencies
### Internal
- 无

### External
- `gopkg.in/yaml.v3` — YAML 解析/序列化

<!-- MANUAL: -->
