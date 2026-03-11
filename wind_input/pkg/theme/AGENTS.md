<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# pkg/theme

## Purpose
主题系统。定义候选窗口、工具栏、弹出菜单的颜色结构体，提供主题加载（从 YAML 文件或内置默认主题）和颜色解析工具。

## Key Files
| File | Description |
|------|-------------|
| `theme.go` | `Theme` 顶层结构体（含 Meta、CandidateWindow、Toolbar、PopupMenu 颜色组）；`ThemeMeta` |
| `colors.go` | `ParseColor(hex string) color.RGBA`：解析 `#RRGGBB` 或 `#RRGGBBAA` 格式的十六进制颜色 |
| `colors_test.go` | 颜色解析单元测试 |
| `manager.go` | `Manager`：从文件系统加载主题、列出可用主题、切换当前主题 |
| `default_themes.go` | 内置默认主题数据（作为回退，无需外部文件） |

## For AI Agents

### Working In This Directory
- 颜色格式：`#RRGGBB`（不透明）或 `#RRGGBBAA`（含 Alpha），均支持 6 位和 8 位十六进制
- 主题文件位于 `themes/<name>/theme.yaml`（相对于 exeDir），`Manager` 扫描此目录
- `CandidateWindowColors` 包含 10 个颜色字段；`ToolbarColors` 包含 17 个；`PopupMenuColors` 包含 7 个
- 添加新颜色字段时需同时更新：结构体定义、默认主题数据、两个 theme.yaml 文件（default/dark）
- `ui.Manager` 持有 `theme.Manager` 实例，通过 `LoadTheme(name)` 切换

### Testing Requirements
- `go test ./pkg/theme/`（`colors_test.go` 测试颜色解析）
- 主题加载测试需要文件系统访问

### Common Patterns
- 渲染器在绘制前调用 `theme.Manager.Current()` 获取当前主题的颜色值
- 颜色字段存储为 `string`（YAML 中的十六进制），渲染时通过 `ParseColor` 转为 `color.RGBA`

## Dependencies
### Internal
- 无

### External
- `gopkg.in/yaml.v3` — 主题文件解析
- `image/color`（标准库）— `color.RGBA` 类型

<!-- MANUAL: -->
