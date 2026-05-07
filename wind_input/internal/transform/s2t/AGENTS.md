<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-05-07 | Updated: 2026-05-07 -->

# internal/transform/s2t

## Purpose
简入繁出（Simplified→Traditional）转换实现。基于 OpenCC 词典数据，提供候选词级别的最长正向匹配替换：

- `Manager`：运行时单例，封装启用/变体状态、按需加载词典池、对外暴露 `Convert` / `ApplyToTexts`
- `Converter`：把若干步骤词典串成一条转换链，配合 LRU 缓存
- `Dict`：单个 .octrie 二进制词典加载与查询（二分 + 最长前缀匹配）
- `DictPool`：按需加载/释放词典实例，未启用时零内存占用
- `Chain`：变体 → 词典名清单（s2t / s2tw / s2twp / s2hk）

## Key Files
| File | Description |
|------|-------------|
| `format.go` | `.octrie` 二进制格式常量（魔数 `WIOC`、HeaderSize、EntrySize） |
| `dict.go` | `Dict`：单个词典加载、`Lookup`、`LongestPrefix`（O(maxKey × log N)） |
| `dictpool.go` | `DictPool`：懒加载 + 写锁保护；`ReleaseAll` 触发 GC 回收 |
| `chain.go` | `Chain(variant)` 返回链路；`AllRequiredDicts` 列出全部可能的词典名 |
| `converter.go` | `Converter`：多步串行转换、`applyStep` 最长正向匹配、LRU 缓存（容量 1024） |
| `manager.go` | `Manager`：单例 API（`Reconfigure`、`SetEnabled`、`SetVariant`、`Convert`、`ApplyToTexts`） |

## For AI Agents

### Working In This Directory
- 词典文件由 `cmd/gen_opencc_dict` 工具从 OpenCC 上游 .txt 编译而来，运行时位于 `data/opencc/*.octrie`
- `Manager` 默认变体为 `S2TStandard`；`Reconfigure` 在 enabled 切换或 variant 变更时才触发实际加载/释放
- `Converter` 缓存仅对长度 ≤ 64 字节的字符串生效（避免长句污染缓存）
- 加载失败 fail-safe：返回错误时调用方应回退 `Enabled=false`，并保留原始字符串输出（见 `coordinator/handle_s2t.go`）
- 词典 .octrie 加载后只读，多协程并发安全；唯一写入路径是 `DictPool.Acquire / ReleaseAll`，已加写锁

### Testing Requirements
- `converter_test.go` 提供内存版 `.octrie` 构造工具 `makeTestDict`，避免依赖磁盘
- 修改 trie 结构 / 算法时务必跑 `go test ./internal/transform/s2t/...` 验证最长前缀匹配语义

### Common Patterns
- coordinator 在候选生成后立即调用 `s2tManager.Convert(text)` 把候选 Text 替换为繁体
- 热键 / 右键菜单触发 `handleToggleS2T` / `handleSetS2TVariant`，最终落到 `Manager.SetEnabled` / `Manager.SetVariant`
- 变体切换不重新加载已加载的共享词典（如 STPhrases、STCharacters），仅追加变体专属词典（TWVariants/HKVariants 等）

## Dependencies

### Internal
- `pkg/config` — `S2TVariant` 枚举与 `S2TConfig` 结构

### External
- 无（纯 Go，无 CGO）

<!-- MANUAL: -->
