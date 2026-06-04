<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-06-04 -->

# design/ - 活跃设计与技术方案文档

## 用途

正在进行或最近完成的设计方案文档。这些文档记录功能设计、问题分析、技术选型等活跃工作，供开发团队参考和决策。

## 关键文件

| 文件 | 描述 |
|------|------|
| `startmenu-zorder-solution.md` | Win11 开始菜单候选框 z-order 问题的完整分析与解决方案（HostWindow 机制、Band 等级、DLL 代理窗口） |
| `pinyin-data-analysis.md` | 拼音数据来源、候选排序算法、词频影响分析 |
| `pinyin-candidate-quality.md` | 拼音候选质量评估：当前排序算法与改进方向 |
| `quick-input-design.md` | 快速输入（快捷词、自动短语展开）设计方案 |
| `smart-punct-after-digit.md` | 数字后智能标点设计：自动配对、规则学习 |
| `2026-04-08-auto-punctuation-pairing-design.md` | 自动配对标点设计方案（括号、引号等） |
| `2026-04-08-english-auto-pair-design.md` | 英文自动配对设计（括号、引号、代码块） |
| `2026-04-11-status-indicator-window-design.md` | 输入法状态指示窗口设计（UI、样式、显示策略） |
| `command-bar-design.md` | 命令直通车（Command Bar）：在快捷短语之上的小型表达式语言，支持 `$CC(display, action...)` 组合时间/打开/按键/剪贴板/加词等动作 |
| `schema-layers.md` | 输入方案配置三层叠加架构（L1 内置 / L2 用户方案 / L3 schema_overrides）：写入位置选型、`dictionaries` 按 id 合并的规则、新增配置项必读的黄金原则 |
| `macos-port.md` | macOS 移植设计 (IMKit + Go 服务双进程模型, UDS bridge 协议, uicmd 命令模型, 与 Win 端 wind_tsf 的对应关系, PR-A 工作清单) |
| `macos-imkit-plan.md` | **PR-A 实战手册**: macOS IMKit `.app` 工程目录结构 / Swift 类骨架 / Info.plist 模板 / 6 个开发里程碑 / 验证步骤 / 风险清单 |
| `enum-constraint.md` | 枚举与魔法字符串约束 SSOT (跨模块共享) |
| `theme-schema-v3.md` | **主题 Schema v3（当前权威设计语义）**：结构归一（删 layout 并入 views / 删 candidate_window 颜色组并入 colors 扁平 token）+ 亮暗统一（`LightDark<T>` 贯穿颜色与图片）+ base 单链继承 + Fill(色·渐变·图)/font 分组；含 v2.6→v3 迁移映射与编辑器原型（`WindInputThemeEditor/src/lib/theme3`）作参考实现 |
| `theme-vector-images.md` | 主题图片接入矢量图（SVG，oksvg 现场栅格化）+ 单色染色（tint，把图当 alpha mask 用主题色填充）；首个消费=翻页箭头 `views.footer_bar.prev_image/next_image`。栅格化收口 `theme.RasterizeSVG` |
| `theme-toolbar-geometry.md` | 工具栏几何重构：L1 几何单一真相源（HitTest/GetButtonBounds/GetToolbarSize 统一查 Layout 派生的 Rect，删线性公式）+ L2 盒模型化（几何进 schema、measure 生效、补 ResolveToolbarViews）规划 |
| `theme-v3-freeze-report.md` | **主题 v3 冻结契约（当前权威）**：冻结字段面 + 求值语义快照 + 不兼容旧主题（不合法整体回退 default）+ golden 逐字节验收证据。历史演进/阶段文档（architecture/P2/P4-P8、v26-freeze）已移入 `archive/` |
| `settings-incremental-save.md` | 设置端全局保存重构：快照 diff + 按 key 最小化提交（`Config.Set`）。根治「全局保存整份覆盖、formData 不含 stats 导致 null 冲掉 track_english」的 bug；独立段（stats/dict）天然隔离 |

## 文件分类

### 关键问题分析
- `startmenu-zorder-solution.md` - Win11 系统层级问题分析与解决方案
- `pinyin-data-analysis.md` - 数据质量与算法改进分析

### 候选排序与质量
- `pinyin-candidate-quality.md` - 候选质量评估与优化
- `smart-punct-after-digit.md` - 特定场景的智能排序规则

### 功能设计
- `quick-input-design.md` - 快速输入功能设计
- `2026-04-08-auto-punctuation-pairing-design.md` - 标点自动配对
- `2026-04-08-english-auto-pair-design.md` - 英文自动配对
- `2026-04-11-status-indicator-window-design.md` - UI 状态指示

## 工作指南

### 阅读方向

1. **解决 z-order 问题** → `startmenu-zorder-solution.md`（含诊断工具）
2. **改进拼音候选** → `pinyin-data-analysis.md` + `pinyin-candidate-quality.md`
3. **实现快速输入** → `quick-input-design.md`
4. **标点符号相关** → `smart-punct-after-digit.md` + `2026-04-08-auto-punctuation-pairing-design.md`
5. **英文输入相关** → `2026-04-08-english-auto-pair-design.md`
6. **UI 优化** → `2026-04-11-status-indicator-window-design.md`

### 新增设计文档

新设计方案应：
1. 使用描述性标题（**不要日期前缀**，长期演变中文件名带日期会变乱）
2. 包含背景、问题分析、建议方案、实现步骤
3. 完成后可移至 `archive/`
4. 在本文件中更新文件列表

### 完成后的处理

当设计方案完成实现并验证时：
1. 移动文件到 `archive/`
2. 更新 `archive/AGENTS.md` 的文件列表
3. 保留原文件引用便于历史追溯

## 依赖关系

### 内部
- `../AGENTS.md` - 文档目录索引
- `../ARCHITECTURE.md` - 系统架构（某些设计涉及架构变更需同步）
- `../archive/` - 已完成的设计方案历史
- `../testing/` - 测试指南（新功能的测试应参考这些设计）

### 外部
- 项目 CLAUDE.md 中的约束
- Windows API 文档
- 拼音词库源（白霜拼音 rime-frost）

<!-- MANUAL: -->
