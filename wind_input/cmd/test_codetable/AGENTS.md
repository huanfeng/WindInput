<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-01 -->

# cmd/test_codetable

## Purpose
码表调试工具。用于交互式测试五笔码表的查询和顶码行为。可加载五笔码表文件，显示码表信息，并按指定编码查询候选词，验证顶码（TopCode）功能。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行程序，加载码表并执行查询测试 |

## For AI Agents

### Working In This Directory
- 命令行用法：
  - `test_codetable <码表路径> [测试编码...]`
  - 示例：`test_codetable dict/wubi/wubi86.txt a aa aaaa`
- 显示内容：
  - 码表元信息（名称、版本、作者、编码方案、码长、词条数等）
  - 对每个编码进行查询，显示候选词
  - 支持顶码测试（第五码输入时的行为）
- 若不指定测试编码，使用默认编码列表测试
- 路径支持相对和绝对两种格式（相对路径相对于 exe 目录）

### Testing Requirements
- 手动执行验证码表加载和查询功能
- 用于验证 `gen_bindict` 和 `gen_wubi_wdb` 生成的二进制文件

### Common Patterns
- 调试五笔顶码功能时运行此工具
- 验证新码表文件的有效性

## Dependencies
### Internal
- `internal/dict` — LoadCodeTable、CodeTable
- `internal/engine/wubi` — 五笔引擎及 Config
- `internal/candidate` — Candidate 类型

### External
- 无

<!-- MANUAL: -->
