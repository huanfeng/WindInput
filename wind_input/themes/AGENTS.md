<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# themes

## Purpose
主题数据文件目录。每个子目录对应一个主题，包含 `theme.yaml` 颜色配置文件。主题在运行时由 `pkg/theme.Manager` 从此目录扫描和加载。

## Key Files
| File | Description |
|------|-------------|
| （无顶层文件） | 各子目录各自独立 |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `default/` | 默认主题（浅色，蓝色调，白色背景） |
| `dark/` | 深色主题（深灰背景，适合深色系桌面） |

## For AI Agents

### Working In This Directory
- 每个主题目录必须包含 `theme.yaml`，文件结构由 `pkg/theme.Theme` 定义
- `theme.yaml` 的顶层字段：`meta`、`candidate_window`、`toolbar`、`popup_menu`
- 颜色格式：`#RRGGBB` 或 `#RRGGBBAA`（8 位含 Alpha 通道）
- 添加新主题：创建新子目录和 `theme.yaml`，主题名为目录名，程序重启后自动识别
- 修改现有主题时参考 `default/theme.yaml` 的注释了解各字段含义

### Testing Requirements
- 颜色值格式可通过 `pkg/theme.ParseColor` 验证
- 视觉效果需在 Windows 环境手动验证

### Common Patterns
- 主题切换通过右键菜单（UI 的主题子菜单）触发，配置保存到 `cfg.UI.Theme`
- 内置默认主题作为回退（`pkg/theme/default_themes.go`），即使 themes 目录缺失也可运行

## Dependencies
### Internal
- `pkg/theme` — 主题结构体定义和加载逻辑

### External
- 无

<!-- MANUAL: -->
