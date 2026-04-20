<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-20 -->

# pkg/buildvariant

## Purpose
编译变体管理。通过 ldflags 注入构建变量，在编译时区分调试版和发布版，提供运行时检查接口。支持不同的应用名称、显示名称和配置目录。

## Key Files
| File | Description |
|------|-------------|
| `variant.go` | 编译变体定义和查询函数（`IsDebug()`、`Suffix()`、`AppName()`、`DisplayName()`） |

## For AI Agents

### Working In This Directory
- `variant` 变量通过 ldflags 注入：`-X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug`（调试版）或留空（发布版）
- `IsDebug()` — 返回 `true` 当 `variant == "debug"`
- `Suffix()` — 调试版返回 `"_debug"`，发布版返回 `""`
- `AppName()` — 调试版返回 `"WindInputDebug"`，发布版返回 `"WindInput"`
- `DisplayName()` — 调试版返回 `"清风输入法 (Debug)"`，发布版返回 `"清风输入法"`
- 调试版应用在任务管理器和注册表中以 `WindInputDebug` 出现，与发布版并存

### Testing Requirements
- 无特殊测试需求（纯编译时注入，运行时查询简单）

### Common Patterns
- 构建脚本在编译时根据版本类型设置 ldflags
- 其他包通过 `buildvariant.IsDebug()` 或 `buildvariant.Suffix()` 进行条件逻辑

## Dependencies
### Internal
- 无

### External
- 无（仅标准库）

<!-- MANUAL: -->
