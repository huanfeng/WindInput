<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-20 -->

# 文档目录 (docs/)

## 用途

项目设计文档、功能规划、测试指南和需求分析。AI agents 在实现功能前应先阅读相关文档。

## 主要文件

| 文件 | 描述 |
|------|------|
| `ARCHITECTURE.md` | 系统架构设计：C++ TSF 层 + Go 服务层 + Named Pipe 通信 + Schema 方案架构 |
| `DEVELOPMENT.md` | 开发指南：环境搭建、构建流程、代码规范 |

## 子目录

| 目录 | 描述 |
|------|------|
| `design/` | 活跃的设计与技术方案文档 |
| `requirements/` | 需求规格与功能规划 |
| `testing/` | 测试指南、测试用例、验证清单 |
| `archive/` | 已完成的历史设计文档（Schema 重构、内存优化、码表过滤） |

### design/

| 文件 | 描述 |
|------|------|
| `startmenu-zorder-solution.md` | Win11 开始菜单候选框 z-order 解决方案 |
| `pinyin-data-analysis.md` | 拼音数据分析：数据来源、候选排序、优化方向 |

### requirements/

| 文件 | 描述 |
|------|------|
| `features-todo.md` | 功能规划：词库格式、码表支持、通用架构设计 |
| `wubi-requirements.md` | 五笔需求分级（P0 必须支持、P1 建议支持、P2 可选、P3 暂不需要） |
| `wubi-requirements-todo.md` | 五笔需求实现进度跟踪 |

### testing/

| 文件 | 描述 |
|------|------|
| `testing-guide.md` | 完整测试指南：19 类测试场景，覆盖基础输入、快捷键、边界情况 |
| `test-word-frequency.md` | 词频功能测试文档 |
| `settings-checklist.md` | 配置检查清单 |
| `test-helper.html` | 测试辅助工具 (HTML 页面) |

### archive/

| 文件 | 描述 |
|------|------|
| `refactoring-schema.md` | Schema 方案系统重构记录（已完成 2026-03-16） |
| `memory-optimization.md` | 内存优化分析记录（已完成） |
| `codetable-filter-design.md` | 码表过滤设计记录（已完成 2026-02-03） |

## 工作指南

### 阅读文档的优先级

1. **架构相关变更** → 先读 `ARCHITECTURE.md`
2. **方案系统** → 先读 `archive/refactoring-schema.md`
3. **拼音引擎** → 先读 `design/pinyin-data-analysis.md`、`archive/codetable-filter-design.md`
4. **五笔引擎** → 先读 `requirements/wubi-requirements.md`、`requirements/wubi-requirements-todo.md`
5. **新功能开发** → 先读 `requirements/features-todo.md`
6. **测试验收** → 参考 `testing/testing-guide.md` 的对应章节
7. **性能问题** → 参考 `archive/memory-optimization.md`
8. **词频相关** → 参考 `testing/test-word-frequency.md`
9. **Win11 开始菜单候选框 z-order / HostWindow 机制** → 先读 `design/startmenu-zorder-solution.md`

### 修改文档

- 功能完成后更新 `requirements/features-todo.md` 的状态
- 新功能的测试用例补充到 `testing/testing-guide.md`
- 架构变更必须同时更新 `ARCHITECTURE.md`

### 测试执行

```bash
# 参考 testing/testing-guide.md 的测试场景列表
# 1. 基础输入测试 (第 1-6 章)
# 2. 快捷键系统 (第 13 章)
# 3. 多显示器与高 DPI (第 18 章)
# 4. 边界情况 (第 19 章)
```

## 依赖关系

### 内部

- `../wind_input/` - 实现的拼音/五笔引擎（需遵循 `requirements/features-todo.md`）
- `../wind_setting/` - 配置界面（需遵循 `testing/settings-checklist.md`）
- `../data/` - 词库源数据和 Schema 方案定义

### 外部

- 无

<!-- MANUAL: -->
