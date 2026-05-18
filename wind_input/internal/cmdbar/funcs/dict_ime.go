// dict_ime.go — P4 阶段补充的 dict.addword / ime.toggle / ime.setting
// 动作实现, 通过 cmdbar.Services 的 DictService / IMEController 接口取
// 真实后端 (由 coordinator 在 NewCoordinator 期间装配)。
//
// 这些函数与 action.go 的 P3 MVP 9 个动作共用同一份 Services 取值约定
// (见 svcs); 缺失对应 service 时返回 cmdbar.ErrServiceUnavailable 以便
// 调用方降级。设计参考 docs/design/command-bar-design.md §3.4。
package funcs

import (
	"fmt"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

// dictIMEActionFuncs 返回 P4 新增动作的 FuncSpec 列表。
// 与 actionFuncs() 一同被 RegisterActions 写入 Registry, 覆盖原本的
// stub 实现 (registerSideEffectStubs 注册的 ErrNotImplemented)。
//
// 2026-05-16 (PR-3) 命名宪法:
//   - dict.addword → dict.add (verb 用 "add", namespace 已含 dict 语义)
//   - ime.setting → setting.open (setting 独立 namespace, verb 统一 "open")
//   - ime.toggle 保留 (符合 namespace.verb 规范)
//
// 旧名通过 aliasOf 注册为 Deprecated, Eval 复用同一实现。
func dictIMEActionFuncs() []cmdbar.FuncSpec {
	dictAdd := cmdbar.FuncSpec{
		Name: "dict.add", Category: cmdbar.CategoryDict,
		MinArgs: 1, MaxArgs: 2, Pure: false,
		Description: "把文本加入用户词库; code 可选, 不传时按当前方案规则自动推导",
		ExampleSrc:  `dict.add(clip())`,
		Eval:        fnDictAddword,
	}
	imeToggle := cmdbar.FuncSpec{
		Name: "ime.toggle", Category: cmdbar.CategoryIME,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "切换 IME 状态 (cn-en / fullshape / layout / candwin)",
		ExampleSrc:  `ime.toggle("cn-en")`,
		Eval:        fnIMEToggle,
	}
	settingOpen := cmdbar.FuncSpec{
		Name: "setting.open", Category: cmdbar.CategorySetting,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "打开 wind_setting 设置窗口的指定页面",
		ExampleSrc:  `setting.open("dict")`,
		Eval:        fnIMESetting,
	}
	return []cmdbar.FuncSpec{
		dictAdd, imeToggle, settingOpen,
	}
}

// fnDictAddword 实现 §3.4 `dict.addword(s)` / `dict.addword(s, code)`。
// code 为空时由 Services 端用当前方案的编码规则自动推导, 与"快捷加词"
// 路径保持一致。失败时把错误包裹返回, 由 coordinator 的 thunk runner
// 统一记 WARN (不带 text 内容)。
func fnDictAddword(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Dict == nil {
		return "", fmt.Errorf("dict.addword: %w", cmdbar.ErrServiceUnavailable)
	}
	text := args[0]
	code := ""
	if len(args) >= 2 {
		code = args[1]
	}
	if err := s.Dict.AddWord(text, code); err != nil {
		return "", fmt.Errorf("dict.addword: %w", err)
	}
	return "", nil
}

// fnIMEToggle 实现 §3.4 `ime.toggle(target)`。target 取值见设计文档
// (candwin / mute / fullshape / cn-en); 当前只接通 candwin, 其它由
// Services 端 (coordinator) 记 WARN 并返回 nil 不中断。
func fnIMEToggle(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.IME == nil {
		return "", fmt.Errorf("ime.toggle: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.IME.Toggle(args[0]); err != nil {
		return "", fmt.Errorf("ime.toggle: %w", err)
	}
	return "", nil
}

// fnIMESetting 实现 §3.4 `ime.setting(page)`。page 直接透传给 IMEController,
// 由宿主 (coordinator) 转给 uiManager.OpenSettingsWithPage。
func fnIMESetting(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.IME == nil {
		return "", fmt.Errorf("ime.setting: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.IME.OpenSetting(args[0]); err != nil {
		return "", fmt.Errorf("ime.setting: %w", err)
	}
	return "", nil
}
