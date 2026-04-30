<!-- Updated: 2026-05-01 -->

# AGENTS.md 写作模板

> 本模板是新增/重构模块时 AGENTS.md 的统一骨架。复制本文件内容到目标目录，按需删改即可。
>
> 强制约定：
>
> - 文件首行必须是 `<!-- Updated: YYYY-MM-DD -->`；如有父级文档，第二行加 `<!-- Parent: ../AGENTS.md -->`。
> - 修改目录的对外接口、导出常量或文件结构时，**必须同步更新该目录的 AGENTS.md**（与 CLAUDE.md 中的硬约束一致）。
> - 全局规则（如"枚举与魔法字符串"）只在 [`/docs/design/enum-constraint.md`](design/enum-constraint.md) 维护，本目录文档以一句话引用，**禁止复述完整规则**。

---

## 模板正文（从 `# <模块名>` 起复制）

```markdown
<!-- Parent: ../AGENTS.md -->
<!-- Updated: 2026-05-01 -->

# <模块名>

## Purpose
一两句话说清楚本目录是做什么的、它在系统中的位置、与上下游的关系。

## Key Files
| File | Description |
|------|-------------|
| `xxx.go` / `xxx.ts` / `xxx.cpp` | 一句话职责 |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `sub/` | 一句话职责（see `sub/AGENTS.md`） |

## For AI Agents

### Working In This Directory
- 3-5 条操作要点（构建命令、入口文件、跨包依赖、需注意的副作用等）

### Testing Requirements
- 1-2 条测试要求（运行命令、覆盖范围、必须人工 QA 的部分）

### Common Patterns
- 1-2 条本目录的典型用法或代码片段；只放有别于通用做法的东西，不写"如何用 Go map"这种通识

## Dependencies

### Internal
- 列出依赖的本仓库其他包/模块

### External
- 列出第三方库/外部 SDK

## 全局约束

- 枚举与魔法字符串约束：见 [`/docs/design/enum-constraint.md`](../../docs/design/enum-constraint.md)。
- （如涉及）日志隐私：INFO 级别不得记录用户敏感信息，详见根 `CLAUDE.md`。

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
```

---

## 字段说明

| 字段 | 是否必填 | 说明 |
|------|---------|------|
| `<!-- Updated: YYYY-MM-DD -->` | 必填 | 统一格式，便于一眼判断新鲜度 |
| `<!-- Parent: ../AGENTS.md -->` | 仅有父级时 | 帮助 AI 上溯模块层级 |
| Purpose | 必填 | 不写"是 xxx 的目录"这种废话；要说清楚职责与位置 |
| Key Files | 推荐 | 仅列**对外有意义**的文件；utility 类零碎文件可省略 |
| Subdirectories | 仅有子目录时 | 一句话说明 + 链接到子级 AGENTS.md |
| For AI Agents | 必填 | 这是 AGENTS.md 价值的核心部分；prefer 操作性而非描述性 |
| Dependencies | 推荐 | Internal/External 分组列出 |
| 全局约束引用段 | 推荐 | 当本目录涉及枚举、协议字段、日志输出时必加 |

## 反模式（禁止）

- ❌ 重复写顶层约束的完整规则（应改为一句话引用）。
- ❌ 复述代码已能自描述的内容（如"这是一个 struct，包含 3 个字段"）。
- ❌ 大量空泛形容词（"高效""灵活""强大"），无可操作信息。
- ❌ 过期描述（提及已删除/已重命名的文件或函数）。
- ❌ 链接悬空（建议提交前跑 `scripts/lint_agents_md.ps1` 校验）。

## 相关工具

- [`scripts/lint_agents_md.ps1`](../scripts/lint_agents_md.ps1)：扫描 AGENTS.md 中的引用路径有效性，输出悬空引用列表。
