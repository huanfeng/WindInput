<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# pkg/systemfont

## Purpose
Windows 系统字体目录扫描和信息提供。通过 Windows Registry（`HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts`）枚举已安装的字体族，并提供本地化显示名称。仅 Windows 平台（`//go:build windows`）。

## Key Files
| File | Description |
|------|-------------|
| `catalog_windows.go` | `FontInfo` 数据结构；`List()`/`ListFamilies()` 函数；Registry 扫描和缓存机制；字体名称本地化异步解析 |
| `nametable.go` | `NameTable`：TrueType 字体 Name Table（元数据）解析；用于提取本地化的字体显示名称 |
| `catalog_windows_test.go` | Registry 扫描单元测试 |

## For AI Agents

### Working In This Directory
- **Registry 路径**：`HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts`
- **Registry 扫描**：值名称为 `字体族名 [修饰符]`（如 `Consolas Bold`），值数据为文件路径（如 `consolasb.ttf`）
- **字体样式处理**：移除后缀修饰符（`Bold`、`Italic` 等），仅保留字体族名
- **本地化**：从 TTF 文件的 Name Table 提取多语言显示名称；异步后台执行，不阻塞 `List()` 返回
- **缓存**：使用 `sync.Once` 缓存扫描结果，避免重复 Registry 操作

### Testing Requirements
- 依赖 Windows 环境测试
- Registry 操作可 mock（使用 `golang.org/x/sys/windows/registry` 的可测试接口）
- 字体 TTF 解析可用示例字体文件测试

### Common Patterns
- 错误处理：字体名称本地化失败时回退到英文 Registry 名称
- 样式后缀列表：`" ExtraBold"`、`" Bold"`、`" Italic"` 等，按优先级移除

## Dependencies
### Internal
- 无

### External
- `golang.org/x/sys/windows/registry` — Windows Registry 操作
- `os` — 文件操作（TTF 读取）
- `path/filepath` — 文件路径处理

<!-- MANUAL: -->
