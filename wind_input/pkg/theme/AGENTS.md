<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# pkg/theme

## Purpose
主题系统。定义候选窗口、工具栏、弹出菜单、Tooltip、模式指示器的颜色结构体及样式配置，提供从 YAML 文件动态加载主题和颜色解析工具。

## Key Files
| File | Description |
|------|-------------|
| `theme.go` | `Theme` 顶层结构体（含 Meta、CandidateWindow、Style、Toolbar、PopupMenu、Tooltip、ModeIndicator）；`Resolve()` 方法将字符串颜色解析为 `color.Color` |
| `colors.go` | `ParseColor`/`MustParseHexColor`：解析 `#RRGGBB` 或 `#RRGGBBAA` 格式的十六进制颜色 |
| `colors_test.go` | 颜色解析单元测试 |
| `manager.go` | `Manager`：多路径搜索加载主题、列出可用主题（`ListAvailableThemes`/`ListAvailableThemeInfos`）、返回解析后主题（`GetResolvedTheme`） |
| `default_themes.go` | `emptyTheme()`：空主题（所有颜色字段为空，`Resolve()` 时使用硬编码默认值作为回退） |

## For AI Agents

### Working In This Directory
- 颜色格式：`#RRGGBB`（不透明）或 `#RRGGBBAA`（含 Alpha），均支持 6 位和 8 位十六进制
- **主题搜索路径（优先级从高到低）**：
  1. `%APPDATA%\WindInput\themes\<name>\theme.yaml`（用户自定义主题）
  2. `<exeDir>\themes\<name>\theme.yaml`（随程序分发的主题）
  3. 也支持单文件形式：`<dir>/<name>.yaml`
- `Theme` 顶层字段：`meta`、`candidate_window`、`style`、`toolbar`、`popup_menu`、`tooltip`、`mode_indicator`
- `CandidateWindowColors` 含 11 个颜色字段；`ToolbarColors` 含 17 个；`PopupMenuColors` 含 7 个；`TooltipColors` 含 2 个；`ModeIndicatorColors` 含 2 个
- `CandidateWindowStyle` 含样式字段：`index_style`（`"circle"`/`"text"`）、`accent_bar_color`、间距/圆角/行高等布局参数
- 添加新颜色字段时需同时更新：结构体定义、`Resolve()` 中的默认值、所有 theme.yaml 文件（default/dark/msime）
- 渲染器使用 `GetResolvedTheme()` 获取已解析的 `*ResolvedTheme`（包含 `color.Color` 值），避免重复解析
- `ThemeDisplayInfo` 提供 ID（目录名）和 DisplayName（`meta.name + meta.version`）供 UI 菜单展示

### Testing Requirements
- `go test ./pkg/theme/`（`colors_test.go` 测试颜色解析）
- 主题加载测试需要文件系统访问

### Common Patterns
- 渲染器在绘制前调用 `theme.Manager.GetResolvedTheme()` 获取当前主题的解析后颜色值
- 颜色字段存储为 `string`（YAML 中的十六进制），渲染时通过 `MustParseHexColor` 转为 `color.Color`（含 fallback 默认值）
- 空字段 `""` 在 `Resolve()` 时自动回退到硬编码默认颜色，因此 theme.yaml 中可省略不需要自定义的字段

## Dependencies
### Internal
- 无

### External
- `gopkg.in/yaml.v3` — 主题文件解析
- `image/color`（标准库）— `color.Color` 类型

<!-- MANUAL: -->
