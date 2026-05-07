<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-20 -->

# data

## Purpose
输入法的数据资源目录，包含 Schema 方案定义、词库源数据、默认配置文件和系统短语配置。这些文件在构建时被复制到 `build/data/` 目录，运行时由 `wind_input` 服务加载。

## Subdirectories

| Directory | Purpose |
|-----------|---------|
| `schemas/` | 输入方案定义文件（`*.schema.yaml`），驱动引擎创建和词库配置 (see `schemas/AGENTS.md`) |
| `dict/` | 词库源数据（拼音 unigram、常用字表等）(see `dict/AGENTS.md`) |
| `examples/` | 用户数据示例文件（短语、Shadow 规则） |

## Key Files

### 根目录文件
| File | Description |
|------|-------------|
| `config.yaml` | 系统预置默认配置文件；加载优先级：代码默认值 → 本文件 → 用户配置（`%APPDATA%\WindInput\config.yaml`）；包含 startup、schema、hotkeys、ui、toolbar、input、advanced 等所有配置项 |
| `system.phrases.yaml` | 系统内置短语配置，随安装包分发 |

### schemas/
| File | Description |
|------|-------------|
| `pinyin.schema.yaml` | 全拼输入方案定义（引擎类型、词库路径、学习策略） |
| `shuangpin.schema.yaml` | 双拼输入方案定义 |
| `wubi86.schema.yaml` | 五笔86输入方案定义（码表配置、自动上屏规则） |
| `wubi86_pinyin.schema.yaml` | 五笔拼音混输方案定义 |

### examples/
| File | Description |
|------|-------------|
| `phrases.example.yaml` | 用户短语示例（自定义短语格式参考） |
| `shadow.example.yaml` | Shadow 规则示例（pin/delete 操作格式参考） |

## For AI Agents

### Working In This Directory
- Schema 文件是方案驱动架构的核心配置，修改后需确保 `internal/schema` 包能正确解析
- 词库源数据较大（unigram.txt ~25MB），不要在 AI 上下文中完整读取
- `config.yaml` 是随安装包分发的系统默认配置，修改会影响所有新安装用户的默认行为；不要在此文件中写入用户个人配置
- `advanced.host_render_processes` 列表控制哪些宿主进程激活 HostWindow 机制（当前默认为 `SearchHost.exe`）
- examples/ 文件供用户参考，修改时保持格式清晰易懂

### Testing Requirements
- 修改 Schema 文件后运行 `cd wind_input && go test ./internal/schema/...`
- 词库数据变更后需重新生成二进制词库（`cmd/gen_bindict`、`cmd/gen_wubi_wdb`）

## Dependencies

### Internal
- `wind_input/internal/schema` — Schema 加载和解析
- `wind_input/internal/dict` — 词库加载
- `wind_input/cmd/gen_*` — 词库生成工具

### External
- 拼音词库源: [白霜拼音 rime-frost](https://github.com/gaboolic/rime-frost)
- 五笔词库源: Rime 生态五笔86词库

<!-- MANUAL: -->
