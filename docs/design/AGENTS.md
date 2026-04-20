<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

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
1. 使用日期前缀（`YYYY-MM-DD-name.md`）或描述性标题
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
- 拼音词库源（雾凇拼音）

<!-- MANUAL: -->
