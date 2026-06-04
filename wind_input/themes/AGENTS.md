<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

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
| `_base/` | **唯一隐藏基础主题**（`_` 前缀 → 不出现在主题列表）：颜色=清风蓝；toolbar=跟随语义（`${accent}`/`${surface}`/`${text_dim}`）；候选窗几何基线；序号纯数字无圆圈。default/msime/jidian 等通过 `base: _base` 继承 |
| `default/` | 默认主题（浅色，蓝色调，白色背景）；`base: _base`，仅 override 序号为圆圈数字 |
| `msime/` | 微软 IME 风格主题；`base: _base`，仅微调主色（微软蓝）+ 紧凑几何 + 文本序号 + 强调条 |

> V3-C 起，旧的 `_layouts/` `_palettes/` 共享零件目录已删除，改用 `base:` 单链继承。
> 主题架构简化后，旧的两套 base（`windy-blue` / `msime-base`）已合并为单一隐藏 `_base`。

## For AI Agents

### Working In This Directory
- 每个主题目录必须包含 `theme.yaml`，文件结构由 `pkg/theme.Theme`（v3）定义
- `theme.yaml` 的 v3 顶层字段：`meta`、`base?`（单链继承 base 主题 ID）、`colors`（扁平语义 token，值为 LightDark）、`layout`（其它窗口几何）、`views`（盒模型树）、`behavior`（用户可覆盖默认）、`resources`（图片）
- 颜色格式：`#RRGGBB` / `#RRGGBBAA`，或 `${token}` 引用，或 `{light, dark}` 亮暗分设
- 添加新主题：创建新子目录和 `theme.yaml`，主题名为目录名，程序重启后自动识别；可 `base: _base` 复用既有颜色/几何，只写需覆盖的块
- 修改现有主题时参考 `default/theme.yaml`（含 views/behavior 的薄 override）与 `_base/theme.yaml`（colors + 候选窗几何基线 + 其它窗口）的注释
- 用户自定义主题也可放在 `%APPDATA%\WindInput\themes\` 目录，优先级高于此处

### Testing Requirements
- 颜色值格式可通过 `pkg/theme.ParseColor` 验证
- 视觉效果需在 Windows 环境手动验证

### Common Patterns
- 主题切换通过右键菜单（UI 的主题子菜单）触发，配置保存到 `cfg.UI.Theme`
- 基础主题 `_base` 因 `_` 前缀被 `ListAvailableThemes` 过滤，不出现在主题列表（仅作 `base:` 继承源）
- 当 theme.yaml 中某 token/字段缺失时，`pkg/theme` 求值用 derive 派生或引擎基线兜底

## Dependencies
### Internal
- `pkg/theme` — 主题结构体定义和加载逻辑

### External
- 无

<!-- MANUAL: -->
