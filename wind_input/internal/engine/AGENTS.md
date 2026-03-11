<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# internal/engine

## Purpose
引擎管理层。定义 `Engine`/`ExtendedEngine` 接口和 `ConvertResult` 数据结构，通过 `Manager` 统一管理拼音和五笔引擎的注册、切换和调用。支持运行时动态切换引擎（`SwitchEngine`/`ToggleEngine`）。

## Key Files
| File | Description |
|------|-------------|
| `engine.go` | `Engine`、`ExtendedEngine` 接口定义，`ConvertResult` 结构体（含拼音专用字段） |
| `manager.go` | `Manager`：引擎注册表、当前引擎管理、`Convert`/`ConvertEx`/`HandleTopCode` 调度 |
| `manager_init.go` | 从配置初始化引擎（`InitializeFromConfig`）、动态加载拼音/五笔引擎 |
| `manager_config.go` | 配置更新：引擎类型切换、拼音/五笔参数热更新 |
| `manager_userfreq.go` | 用户词频保存（`SaveUserFreqs`）、候选选中回调转发 |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `pinyin/` | 拼音输入引擎（DAG、Viterbi、音节 Trie、模糊拼音等） |
| `wubi/` | 五笔输入引擎（码表查询、顶码） |

## For AI Agents

### Working In This Directory
- `Manager` 使用 `sync.RWMutex` 保护引擎注册表，读操作（Convert）用读锁，切换用写锁
- `ConvertEx` 根据引擎类型做类型断言（`*pinyin.Engine`/`*wubi.Engine`），返回类型特定的扩展字段
- 动态加载引擎时通过 `pinyinDictPath`/`wubiDictPath` 找到词库，这两个路径在服务启动时由 `main.go` 设置
- 引擎切换同时需要切换 `DictManager` 的活跃用户词库（`dictManager.SetActiveEngine`）
- `SaveUserFreqs` 在服务退出前调用，持久化用户词频学习数据

### Testing Requirements
- `go test ./internal/engine/...`（会递归测试 pinyin/ 和 wubi/ 子目录）
- 引擎初始化测试需要词库文件，可 mock 或使用测试数据

### Common Patterns
- `EngineType` 常量：`"pinyin"`、`"wubi"`
- `InitializeFromConfig` 失败时 `main.go` 会回退到拼音引擎
- 引擎接口设计为无状态（拼音引擎确实无状态），`Reset()` 为预留接口

## Dependencies
### Internal
- `internal/candidate` — Candidate 类型
- `internal/dict` — DictManager、Dict 接口
- `internal/engine/pinyin` — 拼音引擎实现
- `internal/engine/wubi` — 五笔引擎实现

### External
- 无

<!-- MANUAL: -->
