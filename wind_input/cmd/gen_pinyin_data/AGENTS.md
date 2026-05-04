<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-05-04 -->

# cmd/gen_pinyin_data

## Purpose
从 [mozillazg/pinyin-data](https://github.com/mozillazg/pinyin-data) 原始数据生成 `internal/tooltip/pinyin_data_generated.go`。
默认从 GitHub 下载所需文件；使用 `-src` 指定本地克隆可离线运行。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 唯一入口；包含下载、解析、生成逻辑 |

## Usage

```bash
# 在线（默认）：从 GitHub 下载数据
go run ./cmd/gen_pinyin_data -out internal/tooltip/pinyin_data_generated.go

# 离线：使用本地 pinyin-data 克隆
go run ./cmd/gen_pinyin_data -src /path/to/pinyin-data -out internal/tooltip/pinyin_data_generated.go

# 指定 GitHub 分支/标签
go run ./cmd/gen_pinyin_data -ref v1.0.0 -out internal/tooltip/pinyin_data_generated.go
```

或通过开发脚本：
```powershell
.\dev.ps1 gen
```

## Data Sources & Priority
| 优先级 | 文件 | 说明 |
|--------|------|------|
| 1 | `overwrite.txt` | 手工纠正（本地 -src 模式下可选） |
| 2 | `kXHC1983.txt` | 现代新华字典多音字（最常用音在前） |
| 3 | `kTGHZ2013.txt` | 通用规范汉字多音字（补充 XHC 遗漏） |
| 4 | `kMandarin_8105.txt` | 8105 标准汉字首音（fallback） |

刻意排除 `kHanyuPinyin.txt`（含大量古音、方言音）。

## For AI Agents

### Working In This Directory
- 此工具**不参与日常构建**，仅在需要更新拼音数据时手动运行
- 生成文件头带 `// Code generated ... DO NOT EDIT.`，禁止手工编辑
- `overwrite.txt` 不存在时静默跳过（非错误）

### Testing Requirements
- 运行后检查生成文件中 `杜` 的条目：应仅含 `"dù"`，不含古音 `"dǔ"` 或 `"tú"`
- 运行 `go build ./...` 确认生成文件可编译

## Dependencies
### External
- `net/http` — 在线模式下载
- `github.com/mozillazg/pinyin-data` — 上游数据源（GitHub raw 文件）

<!-- MANUAL: -->
