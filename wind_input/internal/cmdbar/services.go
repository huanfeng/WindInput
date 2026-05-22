package cmdbar

import "errors"

// ErrServiceUnavailable is returned by action functions when the
// required service has not been injected (nil field on Services) on the
// current EvalContext. Callers (P4 coordinator) can use this to degrade
// gracefully instead of crashing.
var ErrServiceUnavailable = errors.New("cmdbar: service unavailable")

// ClipboardService backs `clip.copy` / `clip.paste`. Production wiring
// will adapt internal/clipboard in P4; tests supply mocks.
type ClipboardService interface {
	SetText(text string) error
	GetText() (string, error)
}

// KeyInjector backs `key.tap` / `key.seq` / `key.hold` / `key.release` /
// `key.type`. Production wiring will adapt internal/keyinject in P4.
type KeyInjector interface {
	Tap(combo string) error
	Sequence(combos ...string) error
	Hold(combo string) error
	Release(combo string) error
	TypeText(text string) error
}

// URLOpener backs `open` (and indirectly `search`). Production wiring
// will adapt internal/proc in P4.
type URLOpener interface {
	Open(target string) error
}

// ProcessRunner backs `run` and `shell`. Production wiring will adapt
// internal/proc in P4.
//
// ShellEx 用于 `shell(cmd, "flagA,flagB")` 的扩展形式; flags 白名单见
// internal/proc.ShellFlag*。实现可在不支持时让 ShellEx 退化为 Shell。
type ProcessRunner interface {
	Run(cmd string, args ...string) error
	Shell(cmdline string) error
	ShellEx(cmdline string, flags []string) error
}

// DictService backs `dict.addword`. P3 keeps this as an interface only;
// the action function itself remains a stub until P4.
type DictService interface {
	// AddWord inserts (text, code) into the user dictionary. Code may
	// be empty so the dictionary derives it.
	AddWord(text, code string) error
}

// IMEController backs `ime.toggle` / `ime.setting`. P3 keeps interface
// only.
type IMEController interface {
	Toggle(target string) error
	OpenSetting(page string) error
}

// SearchEngine is optional. The default action implementation composes
// a URL and forwards to URLOpener; override only when a host app wants
// alternate semantics.
type SearchEngine interface {
	Search(engine, query string) error
}

// Services bundles every injectable side-effect dependency that action
// functions may need. Every field may be nil; action functions check
// the field they need and return ErrServiceUnavailable when missing.
// The active Services struct is exposed via EvalContext.Services().
type Services struct {
	Clip   ClipboardService
	Keys   KeyInjector
	Open   URLOpener
	Proc   ProcessRunner
	Dict   DictService
	IME    IMEController
	Search SearchEngine
}
