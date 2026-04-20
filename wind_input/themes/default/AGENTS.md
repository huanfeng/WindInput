<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# themes/default

## Purpose
默认主题定义。为清风输入法提供浅色配色方案（白色背景、蓝色调）。包含候选窗口、工具栏和弹出菜单的颜色配置，是系统的标准主题。

## Key Files
| File | Description |
|------|-------------|
| `theme.yaml` | 默认主题配置文件，定义所有 UI 元素的颜色（`#RRGGBB` 或 `#RRGGBBAA` 格式） |

## For AI Agents

### Working In This Directory
- 主题文件结构（由 `pkg/theme.Theme` 定义）：
  - `meta` — 主题元信息（名称、版本、作者）
  - `candidate_window` — 候选窗口颜色组（背景、文字、边框、高亮等，共 11 个字段）
  - `style` — 候选窗口样式（`index_style`、`accent_bar_color`、间距、圆角等）
  - `toolbar` — 工具栏颜色组（共 17 个字段）
  - `popup_menu` — 弹出菜单颜色组（共 7 个字段）
  - `tooltip` — 编码提示框颜色组
  - `mode_indicator` — 模式指示器颜色组
- 颜色格式：6 位或 8 位十六进制（`#RRGGBB` 或 `#RRGGBBAA`）
- 此主题作为参考示例，包含详细注释说明每个颜色字段的含义
- 未填写的颜色字段由 `pkg/theme.Resolve()` 自动使用硬编码默认值回退，无需全部填写

### Testing Requirements
- 颜色格式可通过 `pkg/theme.ParseColor` 验证
- 视觉效果需在 Windows 环境下手动验证

### Common Patterns
- 主题目录名作为主题标识符（该目录为 `default`，对应主题名 `"default"`）
- `pkg/theme/default_themes.go` 中的 `emptyTheme()` 提供空主题（所有颜色字段为空），`Resolve()` 时使用硬编码默认值
- 用户通过 UI 右键菜单选择主题，配置保存到 `cfg.UI.Theme`
- 启动时由 `pkg/theme.Manager` 加载对应主题，加载失败则使用 `emptyTheme()` 回退

## Dependencies
### Internal
- `pkg/theme` — 主题结构体定义和加载逻辑

### External
- 无

<!-- MANUAL: -->
