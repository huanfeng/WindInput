<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# 文档目录 (docs/)

## 用途

项目设计文档、功能规划、测试指南和需求分析。AI agents 在实现功能前应先阅读相关文档。

## 主要文件

| 文件 | 描述 |
|------|------|
| `architecture.md` | 系统架构设计：C++ TSF 层 + Go 服务层 + Named Pipe 通信 |
| `FEATURES_TODO.md` | 功能规划：词库格式、码表支持、通用架构设计 |
| `wubi_requirements.md` | 五笔需求分级（P0 必须支持、P1 建议支持、P2 可选、P3 暂不需要） |
| `TESTING_GUIDE.md` | 完整测试指南：19 类测试场景，覆盖基础输入、快捷键、边界情况 |
| `codetable_filter_design.md` | 码表过滤设计：候选词排序、权重策略 |
| `memory-optimization.md` | 内存优化分析：词库加载、缓存策略 |
| `pinyin_data_analysis.md` | 拼音数据分析：拼音分布、冲突分析 |
| `settings_checklist.md` | 配置检查清单 |
| `test-helper.html` | 测试辅助工具 (HTML 页面) |

## 子目录

无

## 工作指南

### 阅读文档的优先级

1. **架构相关变更** → 先读 `architecture.md`
2. **拼音引擎** → 先读 `pinyin_data_analysis.md`、`codetable_filter_design.md`
3. **五笔引擎** → 先读 `wubi_requirements.md`
4. **新功能开发** → 先读 `FEATURES_TODO.md`
5. **测试验收** → 参考 `TESTING_GUIDE.md` 的对应章节
6. **性能问题** → 参考 `memory-optimization.md`

### 修改文档

- 功能完成后更新 `FEATURES_TODO.md` 的状态
- 新功能的测试用例补充到 `TESTING_GUIDE.md`
- 架构变更必须同时更新 `architecture.md`

### 测试执行

```bash
# 参考 TESTING_GUIDE.md 的测试场景列表
# 1. 基础输入测试 (第 1-6 章)
# 2. 快捷键系统 (第 13 章)
# 3. 多显示器与高 DPI (第 18 章)
# 4. 边界情况 (第 19 章)
```

## 依赖关系

### 内部

- `../wind_input/` - 实现的拼音/五笔引擎（需遵循 `FEATURES_TODO.md`）
- `../wind_setting/` - 配置界面（需遵循 `settings_checklist.md`）
- `../dict/` - 词库数据（格式需符合 `codetable_filter_design.md`）

### 外部

- 无

<!-- MANUAL: -->
