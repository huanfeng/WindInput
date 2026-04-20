<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# archive/ - 已完成的历史设计文档

## 用途

已实现完成的设计文档存档。这些方案曾在 `design/` 目录中活跃开发，完成后移至此目录以保留历史记录和参考价值。

## 存档文件

| 文件 | 完成时间 | 描述 |
|------|---------|------|
| `refactoring-schema.md` | 2026-03-16 | Schema 方案系统重构：从硬编码迁移至 YAML 方案驱动架构，支持多方案切换 |
| `memory-optimization.md` | 2026-03-23 | 内存优化分析：堆内存管理、Go 与 C++ 互操作性、词库缓存策略改进 |
| `codetable-filter-design.md` | 2026-02-03 | 码表过滤设计：拼音码表频率排序、字符集约束、搜索优化 |
| `debug-build-variant.md` | 2026-04-02 | Debug 编译变体设计：多编译目标管理、输出分离、版本号处理 |

## 文件摘要

### refactoring-schema.md (2026-03-16)

**背景**：原系统使用硬编码配置，不支持方案切换。

**方案**：引入 Schema 驱动架构
- 每个方案对应一个 `*.schema.yaml` 文件
- EngineFactory 根据 schema 动态创建对应引擎
- 支持拼音、双拼、五笔、混输等方案

**成果**：
- `data/schemas/` 目录结构
- `wind_input/internal/schema` 包
- 方案注册、加载、验证流程

**相关代码**：
- `wind_input/internal/schema/loader.go`
- `wind_input/internal/engine/factory.go`
- `data/schemas/*.schema.yaml`

---

### memory-optimization.md (2026-03-23)

**背景**：早期版本内存占用较高，特别是在长时间运行时。

**分析**：
- 词库缓存策略：优先级缓存 vs 全量加载
- Go 与 C++ 互操作：字符串编码转换开销
- Windows API 对象生命周期

**优化措施**：
- 实施分层缓存（热词 + 常用词）
- 减少不必要的字符串复制
- 及时释放临时对象

**相关代码**：
- `wind_input/internal/dict/cache.go`
- `wind_input/internal/engine/*/cache.go`

---

### codetable-filter-design.md (2026-02-03)

**背景**：拼音码表候选过多，需要高效过滤。

**方案**：
- 码表频率排序：使用词频统计优化排序
- 字符集约束：根据输入内容过滤不相关候选
- 搜索优化：前缀匹配、Trie 结构加速

**成果**：
- `wind_input/internal/dict/filter.go`
- 候选排序性能提升 30%+

**相关配置**：
- `data/schemas/*.schema.yaml` 中的 `filter_mode` 字段

---

### debug-build-variant.md (2026-04-02)

**背景**：需要同时支持 Release 和 Debug 编译，多个目标平台（32/64 位）。

**方案**：
- 编译时注入 buildvariant 标识
- 文件名后缀区分（`wind_input_debug.exe` vs `wind_input.exe`）
- 独立的输出目录和配置

**成果**：
- `pkg/buildvariant` 包
- CMakeLists.txt / go.mod 编译脚本支持

---

## 使用档案的指南

### 学习历史背景

当处理相关功能时，参考对应的存档文档了解：
- 初始问题分析
- 当时的决策和权衡
- 实现的关键细节
- 可能的未来改进方向

### 引用存档文档

在代码注释或新文档中可以引用：
```markdown
详见 docs/archive/refactoring-schema.md（2026-03-16）
```

### 恢复已弃用的方案

如果需要重新考虑已弃用的方案（例如，性能问题或新需求），可从存档中：
1. 回顾当初的分析
2. 评估当前情况是否改变
3. 考虑其他变更带来的影响
4. 做出新的决策

## 依赖关系

### 内部
- `../AGENTS.md` - 文档目录索引
- `../design/` - 当前活跃设计（可能演变而来）
- `../ARCHITECTURE.md` - 系统架构（包含当时的实现状态）
- 具体实现文件（见各文件摘要中的"相关代码"）

### 外部
- Git 历史（使用 `git log --all -- <file>` 查看修改历史）

<!-- MANUAL: -->
