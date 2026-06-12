<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-06-12 -->

# schemas/ - 输入方案定义文件

## Purpose

输入方案（Schema）定义文件目录，每个 `*.schema.toml` 文件描述一种完整的输入法方案。Schema 是方案驱动架构的核心配置，由 `wind_input/internal/schema` 包解析，驱动 EngineFactory 创建对应引擎和加载词库。

内置方案文件为 **TOML 格式**（`.schema.toml`）。加载器同时支持 `.schema.yaml`（同 stem 并存时 toml 优先、yaml 回退），仅读取、不写出——用户可放置任一格式覆盖文件。

## Key Files

| File | Description |
|------|-------------|
| `pinyin.schema.toml` | 全拼输入方案（引擎类型 `pinyin`，fuzzy 音、智能组句、显示五笔提示） |
| `shuangpin.schema.toml` | 双拼输入方案（引擎类型 `pinyin`，scheme: shuangpin） |
| `wubi86.schema.toml` | 五笔86输入方案（引擎类型 `codetable`，码表配置、自动上屏规则；词库为 rime `.dict.yaml`） |
| `wubi86_pinyin.schema.toml` | 五笔拼音混输方案（引擎类型 `mixed`，引用 wubi86+pinyin） |

## Schema 文件格式

每个 schema 文件包含以下顶层字段（TOML）：

```toml
[schema]
id = "pinyin"          # 唯一标识符
name = "全拼"          # 显示名称
icon_label = "拼"      # 语言栏图标文字
version = "1.0"
author = "内置"
description = "..."

[engine]
type = "pinyin"        # pinyin | codetable | mixed
filter_mode = "smart"  # smart | strict

[engine.pinyin]        # 仅 pinyin 引擎
scheme = "full"        # full | shuangpin
show_code_hint = true
use_smart_compose = true
candidate_order = "smart"   # smart | frequency

[[dictionaries]]       # 词库配置列表（数组表）
# id/path/type/...

[learning]
# 学习策略配置（auto_learn / freq / ...）
```

字段命名与含义和旧 YAML 一致（Schema struct 带双 yaml+toml tag）；新增字段须同步加 `toml` tag。词库文件**默认 rime `.dict.yaml`**；词库 `path` 按各自扩展名解析格式，也隐含支持 split `.dict.toml`+`.dict.tsv`（非默认生成，可经 `cmd/dicttool split` 转换）。

## For AI Agents

### Working In This Directory

- Schema 文件是方案驱动架构的核心，修改后需运行 `cd wind_input && go test ./internal/schema/...` 验证解析
- `id` 字段必须唯一，与 `config.toml` 中 `schema.available` 列表对应
- 新增方案后需同时在 `config.toml` 的 `schema.available` 中注册
- 不要修改 `id` 字段（会影响用户数据目录路径和配置持久化）

### Testing Requirements

```bash
# 验证 Schema 解析
cd wind_input && go test ./internal/schema/...

# 验证方案加载（集成）
cd wind_input && go test ./internal/engine/...
```

## Dependencies

### Internal
- `wind_input/internal/schema` — Schema 文件解析和验证
- `wind_input/internal/engine` — EngineFactory 根据 schema.engine.type 创建引擎
- `data/dict/` — 词库源数据（schema 中 dictionaries 字段引用）
- `data/config.toml` — `schema.available` 列表决定哪些方案可用

<!-- MANUAL: -->
