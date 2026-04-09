<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-13 | Updated: 2026-04-08 -->

# cmd/test_codetable

## Purpose
码表调试工具。用于测试各类码表（五笔、郑码等）的查询和顶码行为。可加载码表文件，显示码表元信息，并对指定编码执行精确查询和引擎转换，验证候选词排序和顶码（TopCode）功能。

## Key Files
| File | Description |
|------|-------------|
| `main.go` | 命令行程序，加载码表并执行查询测试 |

## For AI Agents

### Working In This Directory
- 命令行用法：
  - `test_codetable <码表路径> [测试编码...]`
  - 示例：`test_codetable dict/wubi/wubi86.wdb a aa aaa aaaa aaaaa`
- 显示内容：
  1. 码表元信息：名称、版本、作者、编码方案、最大码长、是否五笔、是否拼音、词条数量
  2. 编码测试（对每个编码执行两种查询）：
     - 精确匹配：`ct.Lookup(code)` 直接码表查询
     - 引擎转换：`engine.ConvertEx(code, 10)` 通过码表引擎转换（含前缀匹配和排序）
  3. 顶码测试：`engine.HandleTopCode(code)` 测试第 5 码时自动上屏行为
- 若不指定测试编码，使用默认编码列表：
  - 五笔：`a aa aaa aaaa aaaaa`
  - 其他：`a ai wo ni nihao`
- 路径支持相对和绝对两种格式（相对路径相对于 exe 目录）
- 引擎配置：`MaxCodeLength`、`AutoCommitAt4`、`ClearOnEmptyAt4`、`TopCodeCommit`、`PunctCommit` 等

### Testing Requirements
- 手动执行验证码表加载和查询功能
- 用于验证 `gen_bindict` 和 `gen_wubi_wdb` 生成的二进制文件

### Common Patterns
- 调试码表查询和顶码功能时运行此工具
- 验证新码表文件的有效性和性能（加载时间、查询结果数量）
- 对比不同码表的候选词排序和权重差异
- 测试场景：
  1. 空码测试（无候选词的编码）
  2. 多候选测试（单码编码如 `a`）
  3. 顶码测试（4 码以上的编码）
  4. 相似编码对比（如 `aa` vs `aaa`）

## Dependencies
### Internal
- `internal/dict` — LoadCodeTable、CodeTable
- `internal/engine/wubi` — 五笔引擎及 Config
- `internal/candidate` — Candidate 类型

### External
- 无

<!-- MANUAL: -->
