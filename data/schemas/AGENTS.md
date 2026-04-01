<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-01 | Updated: 2026-04-01 -->

# schemas/ - 输入方案定义文件

## Purpose

输入方案（Schema）定义文件目录，每个 `*.schema.yaml` 文件描述一种完整的输入法方案。Schema 是方案驱动架构的核心配置，由 `wind_input/internal/schema` 包解析，驱动 EngineFactory 创建对应引擎和加载词库。

## Key Files

| File | Description |
|------|-------------|
| `pinyin.schema.yaml` | 全拼输入方案（引擎类型 `pinyin`，fuzzy 音、智能组句、显示五笔提示） |
| `shuangpin.schema.yaml` | 双拼输入方案（引擎类型 `pinyin`，scheme: shuangpin） |
| `wubi86.schema.yaml` | 五笔86输入方案（引擎类型 `wubi`，码表配置、自动上屏规则） |
| `wubi86_pinyin.schema.yaml` | 五笔拼音混输方案（引擎类型 `wubi`，启用拼音混输） |

## Schema 文件格式

每个 schema 文件包含以下顶层字段：

```yaml
schema:
  id: <唯一标识符，如 "pinyin">
  name: <显示名称，如 "全拼">
  icon_label: <语言栏图标文字，如 "拼">
  version: "1.0"
  author: "内置"
  description: <方案描述>

engine:
  type: <pinyin | wubi>
  pinyin:             # 仅 pinyin 引擎
    scheme: <full | shuangpin>
    show_wubi_hint: <bool>
    use_smart_compose: <bool>
    candidate_order: <smart | frequency>
    fuzzy:
      enabled: <bool>
  filter_mode: <smart | strict>

dictionaries:
  - <词库配置列表>

learning:
  enabled: <bool>
  <学习策略配置>
```

## For AI Agents

### Working In This Directory

- Schema 文件是方案驱动架构的核心，修改后需运行 `cd wind_input && go test ./internal/schema/...` 验证解析
- `id` 字段必须唯一，与 `config.yaml` 中 `schema.available` 列表对应
- 新增方案后需同时在 `config.yaml` 的 `schema.available` 中注册
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
- `data/config.yaml` — `schema.available` 列表决定哪些方案可用

<!-- MANUAL: -->
