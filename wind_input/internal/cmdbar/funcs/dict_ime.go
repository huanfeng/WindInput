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

// dictIMEActionFuncs 返回 P4/P5 新增动作的 FuncSpec 列表。
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
		Description: "切换 IME 状态 (cn-en / fullshape / layout / candwin / s2t / preedit / toolbar)",
		ExampleSrc:  `ime.toggle("cn-en")`,
		Eval:        fnIMEToggle,
	}
	imeSchema := cmdbar.FuncSpec{
		Name: "ime.schema", Category: cmdbar.CategoryIME,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "切换输入方案并持久化；id 为方案标识符，如 wubi86 / pinyin",
		ExampleSrc:  `ime.schema("pinyin")`,
		Eval:        fnIMESchema,
	}
	imeTheme := cmdbar.FuncSpec{
		Name: "ime.theme", Category: cmdbar.CategoryIME,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "切换主题并持久化；name 为主题名称，如 default / msime 或自定义主题名",
		ExampleSrc:  `ime.theme("msime")`,
		Eval:        fnIMETheme,
	}
	imeThemeCycle := cmdbar.FuncSpec{
		Name: "ime.theme_cycle", Category: cmdbar.CategoryIME,
		MinArgs: 0, MaxArgs: 1, Pure: false,
		Description: `循环切换主题（内置 + 用户安装）并持久化；dir 可选，"next"（默认）向后，"prev" 向前`,
		ExampleSrc:  `ime.theme_cycle()`,
		Eval:        fnIMEThemeCycle,
	}
	settingOpen := cmdbar.FuncSpec{
		Name: "setting.open", Category: cmdbar.CategorySetting,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "打开 wind_setting 设置窗口的指定页面",
		ExampleSrc:  `setting.open("dict")`,
		Eval:        fnIMESetting,
	}
	settingWeb := cmdbar.FuncSpec{
		Name: "setting.web", Category: cmdbar.CategorySetting,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "以 --web 参数启动 wind_setting，直接打开 Web 版设置界面",
		ExampleSrc:  `setting.web("")`,
		Eval:        fnSettingWeb,
	}
	return []cmdbar.FuncSpec{
		dictAdd, imeToggle, imeSchema, imeTheme, imeThemeCycle, settingOpen, settingWeb,
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

// fnIMESchema 实现 `ime.schema(id)`。id 透传给 IMEController.SetSchema，
// 由 coordinator 调用 engineMgr 完成引擎切换并持久化。
func fnIMESchema(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.IME == nil {
		return "", fmt.Errorf("ime.schema: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.IME.SetSchema(args[0]); err != nil {
		return "", fmt.Errorf("ime.schema: %w", err)
	}
	return "", nil
}

// fnSettingWeb 实现 `setting.web(page)`。以 --web 参数启动 wind_setting，
// 直接打开 Web 版设置界面；page 可为空字符串表示默认页。
func fnSettingWeb(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.IME == nil {
		return "", fmt.Errorf("setting.web: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.IME.OpenSettingWeb(args[0]); err != nil {
		return "", fmt.Errorf("setting.web: %w", err)
	}
	return "", nil
}

// fnIMEThemeCycle 实现 `ime.theme_cycle([dir])`。dir 为 "next"（默认）或 "prev"，
// 按已安装主题列表（内置 + 用户安装）循环切换并持久化。
func fnIMEThemeCycle(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.IME == nil {
		return "", fmt.Errorf("ime.theme_cycle: %w", cmdbar.ErrServiceUnavailable)
	}
	dir := ""
	if len(args) >= 1 {
		dir = args[0]
	}
	next, err := s.IME.ThemeCycle(dir)
	if err != nil {
		return "", err
	}
	return next, nil
}

// fnIMETheme 实现 `ime.theme(name)`。通过 ConfigService.Set("ui.theme.name", name)
// 完成主题切换+热更新+持久化，与 config.set("ui.theme.name", name) 等价。
func fnIMETheme(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Config == nil {
		return "", fmt.Errorf("ime.theme: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Config.Set("ui.theme.name", args[0]); err != nil {
		return "", fmt.Errorf("ime.theme: %w", err)
	}
	return "", nil
}
