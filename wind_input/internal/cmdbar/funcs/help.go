package funcs

// help.go 内建函数 `help(name)`: 返回指定函数的 Description 字符串。
// 主要给 cmdbar-repl 与 wind_setting UI 浏览函数手册使用; 短语里偶尔
// 用 `help("...")` 当占位也合理。
//
// 设计 docs/design/command-bar-followup.md §1.5。

import (
	"fmt"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

func helpFuncs() []cmdbar.FuncSpec {
	return []cmdbar.FuncSpec{
		{
			Name:          "help",
			Category:      cmdbar.CategoryMeta,
			MinArgs:       1,
			MaxArgs:       1,
			Pure:          true,
			Deterministic: true,
			Description:   "返回指定函数的简介 (查不到时返回空字符串)",
			ExampleSrc:    `help("open")`,
			Eval:          fnHelp,
		},
	}
}

func fnHelp(_ cmdbar.EvalContext, args []string) (string, error) {
	name := args[0]
	spec, ok := cmdbar.DefaultRegistry.Lookup(name)
	if !ok {
		return "", nil
	}
	if spec.Deprecated && spec.AliasOf != "" {
		// alias: 引导到新名, 但仍展示原 Description 给到上下文。
		return fmt.Sprintf("%s — %s", spec.Description, spec.AliasOf), nil
	}
	return spec.Description, nil
}
