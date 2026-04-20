<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# pkg/dictio

## Purpose
字典 IO 工具库，支持多种词库格式的导入/导出。包括 Rime YAML、纯文本列表、TSV、ZIP 压缩包等格式，以及词库合并、转换、验证功能。

## Key Files
| File | Description |
|------|-------------|
| `header.go` | 头信息管理：`Header` 结构体（版本、描述、作者等）；YAML 序列化/反序列化 |
| `columns.go` | 列定义管理：`ColumnDef` 结构体；CSV/TSV 列排序和映射 |
| `format.go` | 通用格式识别和转换：`Format` 枚举（Rime、Winddict、Text、TSV）；`DictEntry` 数据结构 |
| `escape.go` | 字段转义/反转义：`EscapeField`/`UnescapeField`；支持 `\n`、`\t`、`\\` 转义 |
| `import_rime.go` | Rime YAML 词库导入（`.dict.yaml` 格式解析）；支持注释行跳过、BOM 移除 |
| `import_textlist.go` | 纯文本列表导入（一行一个词，可含权重/词频） |
| `import_tsv.go` | TSV 格式导入（Tab 分隔）；可配置列映射 |
| `import_winddict.go` | WindInput 自有词库格式导入（带头信息、列定义）；ZIP 包装支持 |
| `import_zip.go` | ZIP 压缩包解析：自动检测内容格式（YAML/TSV/WindDict）并导入 |
| `import_phrase_yaml.go` | 短语 YAML 格式导入 |
| `export_winddict.go` | 导出为 WindInput 自有二进制格式或文本格式 |
| `export_zip.go` | 导出为 ZIP 压缩包（包含头信息、列定义、词库内容） |
| `import_test.go`/`export_test.go`/`dictio_test.go` | 导入/导出单元测试 |

## For AI Agents

### Working In This Directory
- **导入流程**：检测格式 → 解析头信息和列定义 → 逐行读取条目 → 返回 `[]DictEntry`
- **导出流程**：构建头信息 → 序列化列定义 → 逐行写入条目 → 格式化输出（YAML/TSV/Binary）
- **Rime 词库**：格式为 YAML（头信息 + `---` 分隔符 + 词条列表），词条格式 `word\t编码[\tweight]`
- **转义规则**：`\n`→换行、`\t`→制表、`\\`→反斜杠；尾部孤立反斜杠保留
- **ZIP 支持**：自动检测内容格式，可嵌套不同类型的词库文件

### Testing Requirements
- 运行：`go test ./pkg/dictio`
- 测试覆盖各格式导入/导出、往返一致性、边界情况（空文件、BOM、转义）

### Common Patterns
- 错误处理：格式不符时返回有意义的错误信息（如 "invalid header"）
- 编码转换：所有文本内部统一为 UTF-8；Windows 文本可能带 UTF-16 BOM，需自动移除

## Dependencies
### Internal
- 无

### External
- `archive/zip` — ZIP 格式处理
- `encoding/csv` — CSV 解析
- `gopkg.in/yaml.v3` — YAML 解析/序列化

<!-- MANUAL: -->
