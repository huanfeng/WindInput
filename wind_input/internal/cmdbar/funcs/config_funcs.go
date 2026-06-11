// config_funcs.go — config.get / config.set / config.toggle 动作实现。
// 通过 cmdbar.Services.Config（ConfigService 接口）读写持久化配置项，
// key 为 YAML 键路径（如 "ui.candidate_layout"），由 pkg/config.Fields 注册表校验。
package funcs

import (
	"fmt"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

// configActionFuncs 返回 config.* 动作的 FuncSpec 列表，由 RegisterActions 写入 Registry。
func configActionFuncs() []cmdbar.FuncSpec {
	return []cmdbar.FuncSpec{
		{
			Name: "config.get", Category: cmdbar.CategoryConfig,
			MinArgs: 1, MaxArgs: 1,
			Pure:          true,
			Deterministic: false,
			Description:   "读取配置项当前值；key 为 YAML 路径（如 ui.candidate.layout）",
			ExampleSrc:    `config.get("ui.theme.style")`,
			Eval:          fnConfigGet,
		},
		{
			Name: "config.set", Category: cmdbar.CategoryConfig,
			MinArgs: 2, MaxArgs: 2,
			Pure:        false,
			Description: "设置配置项并持久化；key 为 YAML 路径，value 为字符串值",
			ExampleSrc:  `config.set("ui.theme.style", "dark")`,
			Eval:        fnConfigSet,
		},
		{
			Name: "config.toggle", Category: cmdbar.CategoryConfig,
			MinArgs: 1, MaxArgs: 1,
			Pure:        false,
			Description: "枚举配置项循环切换下一值，bool 配置项翻转；持久化并返回新值",
			ExampleSrc:  `config.toggle("ui.theme.style")`,
			Eval:        fnConfigToggle,
		},
	}
}

func fnConfigGet(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Config == nil {
		return "", fmt.Errorf("config.get: %w", cmdbar.ErrServiceUnavailable)
	}
	return s.Config.Get(args[0])
}

func fnConfigSet(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Config == nil {
		return "", fmt.Errorf("config.set: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Config.Set(args[0], args[1]); err != nil {
		return "", fmt.Errorf("config.set: %w", err)
	}
	return "", nil
}

// fnConfigToggle 实现 config.toggle(key)。
// 返回值非空（新值字符串），可用于 $CC 的 display 侧显示当前状态，
// 例如：$CC(config.toggle("ui.theme.style"))。
func fnConfigToggle(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Config == nil {
		return "", fmt.Errorf("config.toggle: %w", cmdbar.ErrServiceUnavailable)
	}
	next, err := s.Config.Toggle(args[0])
	if err != nil {
		return "", fmt.Errorf("config.toggle: %w", err)
	}
	return next, nil
}
