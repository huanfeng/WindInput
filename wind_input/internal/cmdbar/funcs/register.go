package funcs

import "github.com/huanfeng/wind_input/internal/cmdbar"

// init registers all pure §3.1-§3.3 functions onto
// cmdbar.DefaultRegistry. Side-effect stubs are registered separately
// inside the cmdbar package itself.
func init() {
	for _, spec := range valueFuncs() {
		cmdbar.DefaultRegistry.Register(spec)
	}
	for _, spec := range textFuncs() {
		cmdbar.DefaultRegistry.Register(spec)
	}
	for _, spec := range calcFuncs() {
		cmdbar.DefaultRegistry.Register(spec)
	}
	// help / 内省函数 (Category=meta), 注册到 default registry。
	for _, spec := range helpFuncs() {
		cmdbar.DefaultRegistry.Register(spec)
	}
}

// RegisterActions installs the P3 action MVP (type / open / proc.run /
// proc.shell / key.tap / key.seq / clip.copy / clip.paste / web.search) onto
// reg, overwriting the package's pre-registered stubs (Pure=false,
// Eval returns ErrNotImplemented). After this call, evaluating the
// action expressions will dispatch to the real implementations, which
// resolve their Services from EvalContext.Services() at runtime.
//
// Pass cmdbar.DefaultRegistry for the package-level registry; tests
// can pass an isolated *Registry for hermetic verification.
func RegisterActions(reg *cmdbar.Registry) {
	for _, spec := range actionFuncs() {
		reg.Register(spec)
	}
	// dict.add / ime.toggle / setting.open 真实实现, 覆盖
	// registerSideEffectStubs 的 ErrNotImplemented 占位。
	for _, spec := range dictIMEActionFuncs() {
		reg.Register(spec)
	}
}
