<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# pkg/config

## Purpose
应用配置的完整定义、加载/保存逻辑、路径管理和运行时状态持久化。配置文件为 YAML 格式，存储在 `%APPDATA%\WindInput\config.yaml`。支持三层配置合并机制。

## Key Files
| File | Description |
|------|-------------|
| `config.go` | `Config` 结构体（含所有子配置）、`Load()`/`LoadFrom()`/`Save()`/`SaveTo()`/`DefaultConfig()`，三层加载逻辑，YAML 序列化标签 |
| `paths.go` | 路径常量（`AppName`、`DataSubDir`、`ConfigFileName` 等）和辅助函数（`GetConfigDir`、`GetDataDir`、`GetSystemConfigPath`、`EnsureConfigDir` 等） |
| `config_hotkey.go` | `HotkeyConfig`：热键字符串配置（`ToggleModeKeys`、`SwitchEngine`、`DeleteCandidate`、`PinCandidate`、`ToggleToolbar`、`OpenSettings`、`AddWord` 等） |
| `state.go` | `RuntimeState`：运行时状态持久化（中英文模式、全角、标点），`LoadRuntimeState`/`SaveRuntimeState` |

## For AI Agents

### Working In This Directory
- `Config` 顶层字段：`Startup`、`Schema`、`Hotkeys`、`UI`、`Toolbar`、`Input`、`Advanced`
- **三层配置加载**（`Load()` / `LoadFrom()`）：
  1. 代码默认值（`DefaultConfig()`）
  2. 系统预置配置（`data/config.yaml`，通过 `GetSystemConfigPath()` 定位）覆盖
  3. 用户配置（`%APPDATA%\WindInput\config.yaml`）覆盖
- **Schema 方案系统**：`SchemaConfig`（`Active` + `Available` 字段），用于多方案切换（`wubi86`/`pinyin`）
- **新增 HotkeyConfig 字段**：`DeleteCandidate`（删除候选）、`PinCandidate`（置顶候选）、`ToggleToolbar`（切换工具栏）、`OpenSettings`（打开设置）、`AddWord`（快捷加词，默认 `ctrl+shift+equal`）
- **新增 UIConfig 字段**：`TextRenderMode`（`directwrite`/`gdi`/`freetype`）、`GDIFontWeight`、`GDIFontScale`、`MenuFontWeight`、`MenuFontSize`
- **新增 AdvancedConfig 字段**：`HostRenderProcesses`（Band 窗口宿主进程白名单，默认 `["SearchHost.exe"]`）
- 新增配置项时：在对应子结构体添加字段，设置 YAML 标签，在 `DefaultConfig()` 中提供默认值，在 `applyConfigFallbacks()` 中处理兜底
- `RuntimeState` 与 `Config` 分开存储（`state.yaml`），避免用户编辑配置时覆盖运行时状态
- 数据根目录通过 `GetDataDir(exeDir)` 获取（`exeDir/data`），词库和 Schema 文件均位于此目录下
- 配置热重载通过 `control` 管道触发，`coordinator.UpdateHotkeyConfig` 等方法应用变更

### Testing Requirements
- YAML 序列化/反序列化可做单元测试
- 路径函数在 Windows 环境测试（依赖 `os.UserConfigDir()`）

### Common Patterns
- 路径函数返回 `(string, error)`，调用方在错误时回退到 exeDir
- `GetDataDir()` 直接返回 `string`（无错误，相对于 exeDir 的绝对路径）
- `FuzzyPinyinConfig` 包含 11 个独立开关（含 `IanIang`、`UanUang`），都可独立启用
- `applyConfigFallbacks()` 处理旧格式迁移（如 `theme:"dark"` 迁移到 `theme_style:"dark"`）

## Dependencies
### Internal
- 无

### External
- `gopkg.in/yaml.v3` — YAML 解析/序列化

<!-- MANUAL: -->
