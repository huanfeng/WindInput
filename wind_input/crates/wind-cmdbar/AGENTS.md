<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-06-29 -->

# wind-cmdbar

## Purpose

命令直通车的核心库：把短语文本通过词法/语法解析生成 AST，再对 `EvalContext` 求值，产出 display 文本与可执行动作链。纯 Rust 库，不含平台代码，所有副作用能力（按键注入、剪贴板、进程、IME 等）由宿主通过 `Services` trait 注入；宿主（wind-coordinator）在短语 hook 处调用 `evaluate_phrase()`，用户选中候选后调用 `run_actions()`。

## Key Files

| File | Description |
|------|-------------|
| `src/lib.rs` | 统一 re-export 全部公共类型；含端到端集成测试 |
| `src/phrase.rs` | 宿主集成入口：`evaluate_phrase` / `run_actions` / `is_cmdbar_grammar`；高层 `PhraseEval` 把单候选与 `$SS` 数组分流 |
| `src/ast.rs` | AST 节点：`Phrase`（Literal/Template/Command/Array）、`Expr`、`CommandPhrase`、`ArrayPhrase`、`Modifiers`（有序 Vec，last-write-wins）、`ModValue`（parse 期静态投影） |
| `src/eval.rs` | `evaluate` / `expand_array`；`assert_pure_display` 守卫 display 不得含副作用函数 |
| `src/action.rs` | `ActionKind`（Text/Effect）与 `ResolvedAction`；持有 `Expr` 在 `run()` 时延迟求值 |
| `src/context.rs` | `EvalContext` trait、`History`（固定容量环形缓冲，Mutex 互斥）、`MemoryContext`（测试用内存实现） |
| `src/services.rs` | 8 个副作用 trait（`ClipboardService` / `KeyInjector` / `UrlOpener` / `ProcessRunner` / `DictService` / `ImeController` / `ConfigService` / `SearchEngine`）及 `Services` 聚合结构 |
| `src/registry.rs` | `FuncSpec` 元信息、`Registry`（`with_builtins` / `full` / `default_registry`）、`Category` 枚举；`named_params` 白名单 + `eval_named` 入口**必须成对声明**（有自检测试兜底） |
| `src/parser/` | 手写词法（`lexer.rs`）+ 语法（`parser.rs`）；`is_cmdbar_grammar` 检测顶层 `{` 或 marker；`parse_arg_list` 产出位置参数 + 具名参数（`k=expr`，仅函数调用可用，marker 短语的选项仍是末参 `{k: v}`） |
| `src/funcs/` | 内置函数：`value`（code/tail/last/clip/sel/app/title/date/time/now/env）、`text`（len/upper/lower/trim/sub/replace/regex/split/concat/reverse/url/html/json/base64/default）、`calc`（calc/num）、`action`（open/proc.run/proc.any/proc.shell/key.tap/key.seq/clip.copy/clip.paste/web.search/…）、`dict_ime`（dict.add/ime.toggle/ime.schema/ime.theme_cycle/setting.open/setting.web）、`config`（config.get/config.set/config.toggle）、`help` |

## For AI Agents

### Working In This Directory

- **宿主集成入口只有三个**：`is_cmdbar_grammar(text)` 判路由，`evaluate_phrase(text, ctx, reg)` 解析求值，`run_actions(actions, ctx, reg)` 选中执行。不要直接拼接 `parse` + `evaluate`，phrase.rs 已封装并处理了 `$SS`/`$AA` 分流。
- **`type(arg)` 是 eval 拦截的特例**，不经 registry 查找，直接产出 `ActionKind::Text`；其余动作函数产出 `ActionKind::Effect`。`run_actions` 先执行所有 Effect（副作用先于上屏），再拼接 Text，这个顺序不能颠倒。
- **新增内置函数**必须在 `funcs/` 下的对应文件里用 `func_specs!` 宏声明，并在 `registry.rs` 的 `with_builtins()` 或 `full()` 中注册。`pure` 字段关系到 `assert_pure_display`——副作用函数写错 `pure=true` 会让它出现在 display 表达式里，产生候选渲染阶段的副作用。
- **`ArrayElement.src` 是元素级命令源**：`$SS` 里嵌入的 `$CC` 成员由 AST `Display` 渲染成独立的 `$CC(...)` 文本，宿主把每个成员做成单独的命令候选，选中时按这份源执行（整组源不能用）。`Display` 与解析必须往返等价（`embedded_command_src_round_trips` 钉住）——改 `Display`/转义规则时先跑它。
- **`$AA` marker** 是 Rust 版新增（Go 版无）：把一个字符串按 Unicode codepoint 拆分为逐字符候选，parser 展开为 `ArrayPhrase`，`expand_array` 处理。不要混淆 `$SS`（显式列举元素）与 `$AA`（自动按字符拆分）。
- **`${NAME}` 是内部目录变量，不是插值**：字符串里的 `${APP_DIR}` / `${USER_DATA}` / `${LOCAL_DATA}` 在**词法期**展开为绝对目录（`lexer::scan_dir_var` → `wind_config::dir_var`），未知的 `${X}` 原样保留字面。这条豁免必须在**两条**扫描路径上同时生效——`lexer::scan_string`（带引号字符串）与 `parser::scan_template_parts`（无引号模板体）；只改一条，另一条就会把 `{NAME}` 当 `{expr}` 插值 → `UnknownFunc` → 动作静默失败或整条候选被丢弃。想在 `$` 后面紧跟真插值须写 `\${expr}`（`\$` 因此进了转义白名单）。
- **`Services` 字段均可 `None`**；动作函数缺失服务时返回 `CmdbarError::ServiceUnavailable`，`run_actions` 收集首个错误但不中断后续动作——宿主不需要填满所有字段，但要处理返回的 `Option<CmdbarError>`。
- **`Modifiers` 语义是 last-write-wins**（`Modifiers::get` 反向查找）；`Modifiers::merge(defaults, explicit)` 把 explicit 追加在后，自然实现 parser 的 sugar defaults 被显式声明覆盖。修改 modifier 处理逻辑时不要改成 HashMap——有序保留是有意的设计。
- **`ResolvedAction` 不持有求值结果，只持有 `Expr`**，在 `run()` 时按当前 ctx 重新求值（对齐 Go 的闭包延迟语义）。这使 `type(last())` 每次执行都拿到最新 history，宿主在 commit 后调用 `History::push()` 即可。
- **`Registry::full()` 一次装齐所有函数**，不需要像 Go 版分两阶段（stub + RegisterActions）。测试纯模板逻辑用 `Registry::with_builtins()`，不需要副作用函数时不要用 `full()`（避免引入不必要依赖）。

### Testing Requirements

- 无 Windows 平台依赖，可直接在 host 运行：`cargo test -p wind-cmdbar`。
- 新增内置函数在 `funcs/` 对应文件的 `#[cfg(test)]` 里用表驱动加典型 + 边界 + 错误用例；`phrase.rs` 的集成测试验证端到端通路（包括缺服务降级）。
- 测试用上下文用 `MemoryContext::new().with_input(...).with_services(...)`；固定时间输出设置 `ctx.clock = Some(...)`。

## Dependencies

### Internal

无（`Cargo.toml` 中无 `wind-*` 依赖；通过 `Services` trait 与宿主解耦）。

### External

- `anyhow` / `thiserror`：错误传播与 `CmdbarError` 定义
- `chrono`：`date` / `time` / `now` 函数的时间计算
- `regex`：`funcs/text.rs` 的 `regex()` 函数
- `base64`：`funcs/text.rs` 的 `base64()` 函数
- `tracing`：（保留，当前函数实现内未大量使用）

## 全局约束

- 日志：该 crate 目前几乎不打日志；后续接入时 INFO 级禁止记录候选文本/输入码/词库内容，仅记录函数名、arity、错误类型，见根 `AGENTS.md`。
- 提交前跑 `cargo fmt`，见根 `AGENTS.md`。

<!-- MANUAL: 此行以下为人工补充区，重新生成时保留 -->
