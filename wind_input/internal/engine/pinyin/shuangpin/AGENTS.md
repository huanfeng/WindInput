<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-01 | Updated: 2026-04-01 -->

# internal/engine/pinyin/shuangpin

## Purpose
双拼方案定义与键码转换。将双拼键序列（每个汉字固定 2 键：声母键+韵母键）转换为全拼字符串，交由全拼引擎复用现有词库和算法完成候选词查询。

不实现独立引擎，而是作为全拼引擎的前置转换层：`pinyin.Engine` 在配置了双拼方案时调用 `Converter.Convert` 将输入先转为全拼再查询。

## Key Files
| File | Description |
|------|-------------|
| `scheme.go` | `Scheme`：双拼方案结构体，含 `InitialMap`（键→声母）、`FinalMap`（键→韵母列表）、`ZeroInitialKeys`（零声母特殊映射）；全局方案注册表（`Register`/`Get`/`List`/`ListIDs`）；`NewCustomScheme` 支持自定义方案 |
| `converter.go` | `Converter`：双拼→全拼转换器；`Convert(input)` 输出 `ConvertResult`（`FullPinyin`/`Syllables`/`PartialInitial`/`PreeditDisplay`/`PositionMap`）；`ConvertResult.MapConsumedLength` 将全拼 ConsumedLength 回映射到双拼偏移（用于 ConsumedLength 传回 coordinator 控制输入缓冲区消耗量）；零声母三种处理：重复键（aa→a）、伪声母、跨键匹配 |
| `schemes_builtin.go` | 6 个内置方案（`init()` 自动注册）：小鹤（`xiaohe`）、自然码（`ziranma`）、微软拼音（`mspy`）、搜狗（`sogou`）、ABC（`abc`）、紫光（`ziguang`）；约 410 个合法拼音音节列表（`validPinyinSyllables`） |

## For AI Agents

### Working In This Directory
- `FinalMap` 一个键可映射多个韵母（如小鹤 `k` → `["uai","ing"]`），转换时通过声母+韵母是否构成合法拼音来消歧
- `normalizePinyin` 处理 ü 相关规则：j/q/x/y + v → u，l/n + v → nv/lv 保留
- `Convert` 奇数键（末尾单键）作为 partial：写入 `PartialInitial` 和 `HasPartial=true`，全拼前缀用于引擎前缀匹配
- `PositionMap[i]` 表示全拼第 i 字节对应的双拼原始字节偏移，供 `MapConsumedLength` 精确回映射
- `PreeditDisplay` 格式：音节间加 `'` 分隔符（如 `"ni'hao"`），与全拼引擎的 CompositionState.PreeditDisplay 格式一致
- 新增方案时调用 `Register(&Scheme{...})`，在 `schemes_builtin.go` 的 `init()` 中注册

### Testing Requirements
- `go test ./internal/engine/pinyin/shuangpin/`
- `converter_test.go` 包含各方案的键序列转换测试

### Common Patterns
- `Get("xiaohe")` 获取小鹤方案，`List()` 枚举所有注册方案
- `Converter.SetScheme(scheme)` 支持运行时切换方案（配置热更新时使用）

## Dependencies
### Internal
- 无（被 `internal/engine/pinyin` 引用）

### External
- 无

<!-- MANUAL: -->
