<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-01 | Updated: 2026-04-20 -->

# pkg/encoding

## Purpose
字符编码工具包。提供码表输入法的词组编码公式解析、规则匹配和编码计算功能，供加词功能（AddWord）在运行时自动计算新词的编码使用。

## Key Files
| File | Description |
|------|-------------|
| `encoder.go` | `Rule`（编码规则）、`FormulaStep`（公式步骤）、`ParseFormula`（解析编码公式字符串）、`MatchRule`（按词长匹配规则）、`CalcWordCode`（根据规则计算词组编码） |
| `encoder_test.go` | `ParseFormula`、`MatchRule`、`CalcWordCode` 的完整单元测试 |

## For AI Agents

### Working In This Directory
- **编码公式格式**：偶数长度字符串，每两个字符一组——大写字母（A-Z）表示字序（A=第1字，B=第2字，…，Z=末字），小写字母（a-z）表示码序（a=第1码，b=第2码，…）
  - 示例：`"AaAbBaBb"` = 第1字第1码 + 第1字第2码 + 第2字第1码 + 第2字第2码
- **Rule 结构体**：`LengthEqual`（精确匹配词长）或 `LengthRange [2]int`（范围匹配 [min, max]），两者互斥；`Formula` 为编码公式字符串
- **MatchRule**：先按 `LengthEqual` 精确匹配，再按 `LengthRange` 范围匹配，返回第一个匹配的规则
- **CalcWordCode**：需提供词语字符串、每个汉字的全码 map（`map[string]string`）和规则列表；词长不足 2 字、无匹配规则、字符缺码、码序越界均返回 error
- 此包为纯逻辑包，无 Windows 依赖，可在任何平台运行和测试

### Testing Requirements
- `go test ./pkg/encoding/`
- 测试覆盖：公式解析、末字（Z）处理、精确/范围规则匹配、2/3/4/5 字词编码计算、缺码和越界错误

### Common Patterns
- `charCodes` map 的 key 为单个汉字字符串，value 为该字的全码（如五笔 `"中" -> "khkg"`）
- 规则列表通常由 Schema 配置文件中的 `encoding_rules` 字段解析而来

## Dependencies
### Internal
- 无

### External
- 无（仅标准库）

<!-- MANUAL: -->
