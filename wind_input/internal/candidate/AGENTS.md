<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-05-17 -->

# internal/candidate

## Purpose
候选词数据结构和排序逻辑。定义 `Candidate` 结构体及 `CandidateList`，供引擎、词库、协调器共用。新增 `CandidateSortMode` 类型，支持多种排序策略（词频优先/自然顺序等）。

## Key Files
| File | Description |
|------|-------------|
| `candidate.go` | `Candidate` 结构体、`CandidateList`（实现 `sort.Interface`）、`Better`/`BetterNatural` 比较函数；`CandidateSortMode` 类型常量 |
| `filter.go` | 候选词过滤逻辑（按通用规范汉字等级过滤） |

## For AI Agents

### Working In This Directory
- `Candidate` 是跨包的核心数据类型，修改字段需检查所有引用方（engine/pinyin、engine/wubi、dict、coordinator、ui）
- 排序规则（`Better`）：权重降序 → 文本升序 → 编码升序 → 消耗长度降序
- `BetterNatural`：自然顺序排序（不调整词频权重，按码表原始顺序为主）
- `CandidateSource` 枚举: `None / Codetable / Pinyin / English / Phrase`; `SourcePhrase` (2026-05-16 引入) 标记 PhraseLayer 候选, 在混输引擎中走 `mixed.PhraseWeightBoost` 独立 tier (1M), 不与码表词 (10M) 抢首位
- `CandidateSortMode` 常量由 Schema 的 `candidate_sort_mode` 字段设置，传递给 `CompositeDict.SetSortMode`
- `IsCommon` 字段由 `dict.InitCommonCharsWithPath` 初始化的通用字符表决定
- `IsCommand` 标识 uuid/date/time 等命令候选，UI 渲染时可能有特殊样式
- `IsGroupMember` (2026-05-17 引入) 标识 `$AA` 字符组、`$SS` 字符串数组**展开后**的子项候选; 右键菜单 pin/delete/前移/后移/置顶/恢复默认 全 disable —— 顺序由源 marker (`$AA(chars)` / `$SS(elem...)`) 完整定义, 走"编辑短语"路径修改 yaml, 不允许 Shadow 双轨漂移。导航候选 (`IsGroup=true`) 本身**不**标 (组入口不展开), 普通短语 / 用户词 / 系统词 / 单 `$CC` 命令亦不标
- `GroupName` (2026-05-17 引入) 字符组/字符串组的显示名, `IsGroupMember=true` 时由 PhraseLayer 在 `Search` / `SearchCommand` / `expandSSGroup` 三条出口统一填充; coordinator 的 `expandAACandidates` 兼容路径也填。配合 `GroupCode` 给 `collapseGroupMembersIfMixed` 在混合候选场景下生成 nav 候选展示用 (Text=GroupName, Comment="N 字")
- `GroupTemplate` (2026-05-18 引入) 字符组/字符串组的**原始 PhraseRecord.Text** (含 `$AA(...)` / `$SS(...)` marker), 在 D (`IsGroupMember=true`) 和 E (collapse 后 nav, `IsGroup=true`) 类型上填充。用作 nav 候选的 stable id 模板:
  - nav.ID = `PhraseCandidateID(GroupCode, GroupTemplate)` = `"phrase:" + GroupCode + ":" + GroupTemplate`
  - nav.PhraseTemplate = GroupTemplate
  让 Shadow pin / DisablePhrase 跨"PhraseLayer 直接 nav"和"coordinator collapseGroupMembersIfMixed 出来的 nav"间稳定命中 (而非按显示名 / 跨语言变化的 Name)。详见 docs/design/candidate-actions.md §5
- **Candidate.ID 命名空间速查** (与 nav 形态对齐): 用户/系统词为空; 静态/动态/`$CC` 短语为 `phrase:<code>:<template>`; `$AA` 字符候选 `phrase:<code>:<char>`; `$SS` 元素 `phrase:<code>:<rawElement>`; **nav (`IsGroup=true`) `phrase:<code>:<GroupTemplate>`** (2026-05-18 起从空升级为稳定 id)
- `ConsumedLength` 用于拼音部分上屏场景（选词后剩余拼音继续输入）
- `DisplayText` / `Actions`（命令直通车）：当短语 value 含 `$CC(...)` 时由 PhraseLayer 通过 coordinator 注入的 hook 生成。`DisplayText` 仅做候选显示（空则回落 `Text`）；`Actions` 是闭包列表，由 `doSelectCandidate` 在选中时按序异步执行，**不**走 InsertText 路径。该类候选不允许被热键 pin，见 `handle_candidate_action.go::handlePinCandidateByKey`。

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
