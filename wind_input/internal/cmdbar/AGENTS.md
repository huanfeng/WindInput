<!-- Parent: ../AGENTS.md -->
<!-- Updated: 2026-05-12 -->

# cmdbar

## Purpose
命令直通车 (Command Bar) 的核心库实现, 给 wind_input 的快捷短语扩展一个轻量表达式语言: 字面短语 / `{expr}` 模板 / `$CC(display, action...)` 命令三种形态共用一套解析器与求值器。完整规约见 `docs/design/command-bar-design.md`。

本目录是**纯 Go 库**, 不持有任何全局状态, 也不直接产生副作用。动作函数 (§3.4) 通过 `Services` 接口集获得依赖, 由宿主侧 (P4 起 coordinator) 注入到 `EvalContext.Services()`; P3 完成 9 个动作 (`type / open / run / shell / key.tap / key.seq / clip.copy / clip.paste / search`) 的纯库实现, 真正连线在 P4。

## Key Files
| File | Description |
|------|-------------|
| `context.go` | `EvalContext` 接口 (含 `Services()`) 与 `MemoryContext` 测试实现; 含环形 `History` (容量自定, 默认 16) |
| `services.go` | 动作函数所需依赖接口集: `ClipboardService` / `KeyInjector` / `URLOpener` / `ProcessRunner` (含 `Shell` + `ShellEx(cmd, flags)`) / `DictService` / `IMEController` / `SearchEngine` 与 `Services` 聚合; `ErrServiceUnavailable` 用于缺失服务降级 |
| `registry.go` | `FuncSpec` 元信息 + 线程安全 `Registry`; Category / Deterministic / Deprecated / AliasOf / Description / ExampleSrc 字段; `ListFuncs()` 返回完整 spec 列表供 wind_setting 渲染函数手册; 默认注册命名宪法新名 (proc.run / proc.shell / dict.add / setting.open / web.search) 为 Pure=false stub, 等 `funcs.RegisterActions` 调用后被真实实现覆盖。2026-05-18: 旧名 alias (run/shell/dict.addword/ime.setting/search) 已彻底删除 (发布前清理, 不留迁移负担); `Deprecated` / `AliasOf` 字段保留供未来潜在 alias 使用 |
| `ast/ast.go` | `Expr` / `Phrase` 节点定义 (`StringLit`/`NumberLit`/`Ident`/`Call`/`ObjectLit` + `LiteralPhrase`/`TemplatePhrase`/`CommandPhrase`/`ArrayPhrase`); `CommandPhrase` 同时实现 Expr (用于嵌入 `$SS` 元素位置) 与 Phrase 接口, 带 `Modifiers map[string]any` 字段 (2026-05-16 引入, 详见 follow-up §3.2); `ObjectLit` 是 trailing options bag 字面量; `ArrayPhrase` 是 `$SS(name, elem...)` 字符串数组短语, Elements 类型为 `[]Expr` (StringLit 或 CommandPhrase) |
| `parser/lexer.go` | 手写词法; 字符串内 `{...}` 切出 interp 段, 支持 `\" \\ \{ \} \( \) \n \t \r` 转义; 表达式位置 (字符串外) `{` `}` `:` 作为 ObjectLit 标点 token |
| `parser/parser.go` | 入口 `Parse(src) (Phrase, error)`; 顶层 `findTopLevelMarker` 识别 `$CC(` / `$CC1(` / `$SS(` 三种 marker (与 `{` interpolation 互斥), 分流到 `parseCommandPhrase` 或 `parseArrayPhrase`; marker syntax sugar (`$CC1` ≡ `$CC + {prefix:true}`, `$SS` 隐含 `{prefix:true, expand:"exact", nav:true}`) 在 `markerDefaults` 表里, parser 自动合并显式 options; `parseArrayPhrase` 用 `splitArrayArgs` 按顶层 `,` 切元素, 每个 span 自识别 `$CC(` 走 embedded CommandPhrase 路径, 嵌套深度上限 1 (内层 `$CC` 禁用 prefix modifier) |
| `eval/eval.go` | `Evaluate(phrase, ctx, reg)` → display + actions (支持 LiteralPhrase / TemplatePhrase / CommandPhrase, 显式拒绝 ArrayPhrase 引导调用方走 ExpandArray); `ExpandArray(ArrayPhrase, ctx, reg)` 把 $SS 展开为 N 个 `ArrayElement` (Display + Actions + ElementModifiers), string lit 元素 Actions=nil, 嵌入 CommandPhrase 走完整 Evaluate |
| `action.go` | P5 引入的 `ResolvedAction` 模型 (`Kind ActionEffect/ActionText` + `Run func() (string, error)`), 把动作区分为纯副作用与文本上屏 |
| `funcs/value.go` | §3.1 取值函数 (`code/tail/last/clip/sel/app/title/date/time/now/env`); `code` 返回触发候选时的 inputBuffer 快照, 旧名 `input` 已迁移。**注意**: `code()` / `tail(code, n)` 在 cmdbar 短语场景实际不可用 — 短语精确码触发, 输入码偏移即脱离命中, 不存在 "prefix 命中后追加输入"。这两个函数与 `calc(tail(...))` 组合在短语 yaml 中不应出现, 仅在外部脚本上下文有意义。详见 docs/design/command-bar-design.md §3.1 caveat 节 |
| `funcs/text.go` | §3.2 文本处理 (`len/upper/lower/trim/sub/replace/regex/split/concat/reverse/url/html/json/base64/default`); `t2s/s2t/pinyin` 为占位 stub |
| `funcs/calc.go` | §3.3 `calc` (递归下降算术求值, 支持 `+ - * / % ( )`, 空输入静默返回 `""` 无错) 与 `num` (2/8/10/16 进制互转) |
| `funcs/action.go` | §3.4 动作主名: `open / proc.run / proc.shell / key.tap / key.seq / clip.copy / clip.paste / web.search` (PR-3 命名宪法; 旧名 alias 已删, 见 registry.go 注释)。`type` 由 eval 拦截为 `ActionText` 不走 registry。每个函数从 `ctx.Services()` 取依赖, 缺失返回 `ErrServiceUnavailable`。`proc.shell(cmd[, flags])` 第二参可选 flag (term/pwsh) 走 `Proc.ShellEx` |
| `funcs/dict_ime.go` | §3.4 主名: `dict.add / ime.toggle / setting.open` (命名宪法; 旧名 alias 已删)。fn 实现 (fnDictAddword/fnIMESetting) 内部命名保留旧称, 不影响外部调用语义 |
| `funcs/help.go` | `help(name)` 内建函数: 查 DefaultRegistry 返回该函数 Description 字符串; alias 名返回时附带"-> 新名"提示 |
| `funcs/register.go` | `init` 把 §3.1-§3.3 + help 注册到 `DefaultRegistry`; `RegisterActions(reg)` 用真实 §3.4 实现覆盖 stub。alias 注册 (用于命名宪法迁移期) 已在 2026-05-18 发布前清理移除, 函数命名采用 canonical 形式 |

## Subdirectories
| Directory | Purpose |
|-----------|---------|
| `ast/` | 表达式/短语 AST 节点 |
| `parser/` | 词法 + 语法, 生成 AST |
| `eval/` | AST 求值器 + 显示名纯函数校验 |
| `funcs/` | 内建函数实现 (§3.1-§3.4 已落地, §3.5 ime/dict 等待 P4) |

## For AI Agents

### Working In This Directory
- 入口接口只有两个: `parser.Parse` 和 `eval.Evaluate`; 其余类型通过这两个入口暴露。
- 给注册表增加函数时, 同时填好 `Pure` 字段, 否则会被 display 校验拒绝或反过来污染显示名。
- 副作用函数在 `registry.go::registerSideEffectStubs` 默认占位; P3 的 9 个动作在 `funcs.RegisterActions(reg)` 中覆盖 stub, 保留 `Name/MinArgs/MaxArgs/Pure` 不变。剩余 stub (`dict.addword / ime.toggle / ime.setting / ask / pick`) 留到 P4-P5。
- 给宿主接线时, 实现 `Services` 各字段, 注入到 `EvalContext.Services()` 返回值即可; 缺失字段的动作会返回 `ErrServiceUnavailable`, 调用方据此降级。
- action 函数返回的 display 字符串恒为空, 求值器只把它们当 thunk; 不要把 action 调用放到 display 表达式里 (`assertPureDisplay` 会拦截)。
- action 参数表达式**延迟求值**: thunk 触发时再走 `evalExpr`, 这样 `type(last())` 等动作每次都拿到最新 history (`coordinator` 在 commit 后 Push, 下一次触发即反映)。
- 字符串字面量内的 `{expr}` 在词法层只做花括号匹配, **不**做转义解码; interp 段是 raw substring, 后由 parser 再次 lex/parse。这意味着写在 `{}` 里的子串与外层字符串脱钩, 内部嵌套字符串可以照常用 `"..."` 或 `'...'`。
- `History.Push` 互斥写, `EvalContext.Last` 同走互斥; 后续接 coordinator 时直接调用即可。

### Testing Requirements
- `cd wind_input && go test ./internal/cmdbar/...` 必须全绿。
- 新增内建函数请同步在 `funcs/*_test.go` 表驱动加用例 (典型 + 边界 + 错误)。
- 端到端覆盖通过 `eval/eval_test.go::TestEvaluate_DesignExamples` 跑设计文档 §3.6 全部条目。

### Common Patterns
- 用 `cmdbar.NewMemoryContext()` 构造测试 ctx; 设置 `Clock` 即可固定 `date/time/now` 输出。
- 注册函数典型形态:
  ```go
  {Name: "foo", MinArgs: 1, MaxArgs: 2, Pure: true, Eval: func(ctx, args) (string, error) { ... }}
  ```
- 1-based 索引参数通过 `funcs/text.go::resolve1Based` 处理, 支持负数倒数, 越界返回 ok=false。

## Dependencies

### Internal
- 仅依赖标准库 + `internal/cmdbar/ast`; 通过 `Services` 接口约束 (而非直接导入) 与 `internal/clipboard` / `internal/keyinject` / `internal/proc` / `internal/engine` / `internal/ui` 解耦。
- P4 已接通: `internal/coordinator/cmdbar_services.go` 装配 `Services` 字段, `cmdbar_context.go` 实现 `EvalContext`, `NewCoordinator` 调用 `installCmdbarPhraseHook` 把 (parser.Parse + eval.Evaluate) 注入到 `dict.PhraseLayer.SetCmdbarHook`; `recordCommit` 同步 `History.Push`, 让 `last()` 反映上一次 commit 文本。

### External
- 无第三方依赖。

## 全局约束

- 枚举与魔法字符串约束: 见 [`/docs/design/enum-constraint.md`](../../../docs/design/enum-constraint.md)。
- 日志隐私: 该目录目前不打日志; 后续接入时 INFO 级别禁止记录候选/输入文本, 仅记录元数据 (函数名、arity、错误码), 详见根 `CLAUDE.md`。

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
