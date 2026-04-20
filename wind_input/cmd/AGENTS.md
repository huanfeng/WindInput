<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-20 -->

# cmd

## Purpose
可执行程序入口点集合。包含主服务程序和多个离线工具命令，每个子目录对应一个独立的 `main` 包。

## Key Files
| File | Description |
|------|-------------|
| （无顶层文件） | 各子目录各自独立 |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `service/` | 主服务进程（输入法后端） (see `service/AGENTS.md`) |
| `gen_bindict/` | 从 Rime 词库生成 `pinyin.wdb` 和 `unigram.wdb` (see `gen_bindict/AGENTS.md`) |
| `gen_codetable_wdb/` | 将五笔 Rime `.dict.yaml` 码表转换为 `codetable.wdb` 二进制格式 (see `gen_codetable_wdb/AGENTS.md`) |
| `gen_unigram/` | 从 Rime `.dict.yaml` 提取词频，输出 `unigram.txt` (see `gen_unigram/AGENTS.md`) |
| `test_codetable/` | 码表调试工具，测试码表查询和顶码行为 (see `test_codetable/AGENTS.md`) |

## For AI Agents

### Working In This Directory
- 每个子目录独立编译：`go build ./cmd/service/`
- 工具命令（gen_*、test_*）仅用于开发/构建阶段，不随服务部署
- 添加新命令时在此目录新建子目录，遵循现有模式

### Testing Requirements
- 工具命令通过手动执行验证；主服务通过集成测试验证
- `gen_bindict` 输出文件可用 `test_codetable` 验证

### Common Patterns
- 所有命令使用标准库 `flag` 解析命令行参数
- 工具命令将输出路径通过 `-out` 参数控制

## Dependencies
### Internal
- `internal/dict/binformat` — 二进制词库读写（gen_bindict）
- `internal/dict/dictcache` — 码表缓存转换（gen_wubi_wdb）
- `internal/engine/wubi` — 五笔引擎（test_codetable）

### External
- 无额外外部依赖

<!-- MANUAL: -->
