<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/candidate

## Purpose
候选词数据结构和排序逻辑。定义 `Candidate` 结构体及 `CandidateList`，供引擎、词库、协调器共用。

## Key Files
| File | Description |
|------|-------------|
| `candidate.go` | `Candidate` 结构体、`CandidateList`（实现 `sort.Interface`）、`Better` 比较函数 |
| `filter.go` | 候选词过滤逻辑（按通用规范汉字等级过滤） |

## For AI Agents

### Working In This Directory
- `Candidate` 是跨包的核心数据类型，修改字段需检查所有引用方
- 排序规则（`Better`）：权重降序 → 文本升序 → 编码升序 → 消耗长度降序
- `IsCommon` 字段由 `dict.InitCommonCharsWithPath` 初始化的通用字符表决定
- `IsCommand` 标识 uuid/date/time 等命令候选，UI 渲染时可能有特殊样式
- `ConsumedLength` 用于拼音部分上屏场景（选词后剩余拼音继续输入）

### Testing Requirements
- 排序逻辑可通过简单的单元测试覆盖
- 过滤逻辑依赖 `common_chars.txt` 数据文件初始化

### Common Patterns
- 引擎返回 `[]Candidate`，coordinator 转换为 `[]ui.Candidate` 供 UI 使用
- `Hint` 字段用于显示五笔编码提示（反查）或码表编码提示

## Dependencies
### Internal
- 无（被其他包引用，自身无内部依赖）

### External
- 无

<!-- MANUAL: -->
