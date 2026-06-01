package funcs

import (
	"fmt"
	"net/url"
	"strings"

	"github.com/huanfeng/wind_input/internal/cmdbar"
)

// §3.4 action functions. They are registered separately from the pure
// §3.1-§3.3 functions because they require runtime Services injection
// (see cmdbar.Services). Use cmdbar.RegisterActions to install them
// onto a Registry; the funcs package's init() does NOT register them
// implicitly.

// actionFuncs returns the FuncSpec list for the action MVP. Each
// Eval handler resolves its required service from ctx.Services() at
// invocation time. If the service is nil it returns
// cmdbar.ErrServiceUnavailable so callers can degrade.
//
// 注意: `type` 不在此列表内。它由 eval.Evaluate 在解析 CommandPhrase.Actions
// 时显式拦截构造为 ActionText (P5), 由 coordinator 走 TSF InsertText 通路
// 落字, 不再经过 registry.Lookup; registry 中的 type stub 仅供 arity 元信息,
// 不会真正被 Evaluate 调用。
//
// 2026-05-16 (PR-3) 命名宪法 (docs/design/command-bar-followup.md §1):
//   - run → proc.run, shell → proc.shell, search → web.search 改 namespace
//   - open / key.* / clip.* 保留 (符合规范或例外)
//   - 旧名通过 aliasOf 注册为 Deprecated, Eval 仍指向同一实现, 保证用户
//     已有 yaml 短语继续工作。
func actionFuncs() []cmdbar.FuncSpec {
	openSpec := cmdbar.FuncSpec{
		Name: "open", Category: cmdbar.CategoryAction,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "打开 URL / 程序 / 文件 (通用 ShellExecute 语义)",
		ExampleSrc:  `open("https://baidu.com")`,
		Eval:        fnOpen,
	}
	procRun := cmdbar.FuncSpec{
		Name: "proc.run", Category: cmdbar.CategoryProc,
		MinArgs: 1, MaxArgs: -1, Pure: false,
		Description: "启动外部程序, 可带参数 (第一参为可执行文件, 余下参数为命令行参数)",
		ExampleSrc:  `proc.run("notepad.exe")`,
		Eval:        fnRun,
	}
	procShell := cmdbar.FuncSpec{
		Name: "proc.shell", Category: cmdbar.CategoryProc,
		MinArgs: 1, MaxArgs: 2, Pure: false,
		Description: "通过 cmd /c 执行命令行; 第二参可选 flag (term/pwsh)",
		ExampleSrc:  `proc.shell("echo hi")`,
		Eval:        fnShell,
	}
	keyTap := cmdbar.FuncSpec{
		Name: "key.tap", Category: cmdbar.CategoryKey,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "模拟单次按键组合, 形如 Ctrl+C / Shift+End / Enter / vk:0x5D",
		ExampleSrc:  `key.tap("Ctrl+C")`,
		Eval:        fnKeyTap,
	}
	keySeq := cmdbar.FuncSpec{
		Name: "key.seq", Category: cmdbar.CategoryKey,
		MinArgs: 1, MaxArgs: -1, Pure: false,
		Description: "顺序模拟多个按键组合",
		ExampleSrc:  `key.seq("Home", "Shift+End", "Delete")`,
		Eval:        fnKeySeq,
	}
	keyHold := cmdbar.FuncSpec{
		Name: "key.hold", Category: cmdbar.CategoryKey,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "按下并保持按键组合（不抬起）, 需与 key.release 成对使用",
		ExampleSrc:  `key.hold("Shift")`,
		Eval:        fnKeyHold,
	}
	keyRelease := cmdbar.FuncSpec{
		Name: "key.release", Category: cmdbar.CategoryKey,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "抬起之前由 key.hold 按下的按键组合",
		ExampleSrc:  `key.release("Shift")`,
		Eval:        fnKeyRelease,
	}
	keyType := cmdbar.FuncSpec{
		Name: "key.type", Category: cmdbar.CategoryKey,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "以 Unicode 扫描码方式直接输入文本, 不依赖键盘布局",
		ExampleSrc:  `key.type("hello")`,
		Eval:        fnKeyType,
	}
	clipCopy := cmdbar.FuncSpec{
		Name: "clip.copy", Category: cmdbar.CategoryClip,
		MinArgs: 1, MaxArgs: 1, Pure: false,
		Description: "把文本写入系统剪贴板",
		ExampleSrc:  `clip.copy(last())`,
		Eval:        fnClipCopy,
	}
	clipPaste := cmdbar.FuncSpec{
		Name: "clip.paste", Category: cmdbar.CategoryClip,
		MinArgs: 0, MaxArgs: 0, Pure: false,
		Description: "模拟 Ctrl+V 粘贴剪贴板内容",
		ExampleSrc:  `clip.paste()`,
		Eval:        fnClipPaste,
	}
	webSearch := cmdbar.FuncSpec{
		Name: "web.search", Category: cmdbar.CategoryWeb,
		MinArgs: 2, MaxArgs: 2, Pure: false,
		Description: "用搜索引擎搜索 (engine ∈ baidu/bing/google/zdic)",
		ExampleSrc:  `web.search("baidu", last())`,
		Eval:        fnSearch,
	}
	return []cmdbar.FuncSpec{
		openSpec, procRun, procShell,
		keyTap, keySeq, keyHold, keyRelease, keyType,
		clipCopy, clipPaste, webSearch,
	}
}

// svcs fetches the Services bundle from ctx, or returns
// ErrServiceUnavailable if none is attached.
func svcs(ctx cmdbar.EvalContext) (*cmdbar.Services, error) {
	s := ctx.Services()
	if s == nil {
		return nil, cmdbar.ErrServiceUnavailable
	}
	return s, nil
}

func fnOpen(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Open == nil {
		return "", fmt.Errorf("open: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Open.Open(args[0]); err != nil {
		return "", fmt.Errorf("open: %w", err)
	}
	return "", nil
}

func fnRun(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Proc == nil {
		return "", fmt.Errorf("run: %w", cmdbar.ErrServiceUnavailable)
	}
	cmd := args[0]
	var rest []string
	if len(args) > 1 {
		rest = args[1:]
	}
	if err := s.Proc.Run(cmd, rest...); err != nil {
		return "", fmt.Errorf("run: %w", err)
	}
	return "", nil
}

func fnShell(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Proc == nil {
		return "", fmt.Errorf("shell: %w", cmdbar.ErrServiceUnavailable)
	}
	// 1 参形式: 保留旧 Shell() 调用 (兼容现有 mock 与 cmdbar-repl 行为)。
	if len(args) == 1 {
		if err := s.Proc.Shell(args[0]); err != nil {
			return "", fmt.Errorf("shell: %w", err)
		}
		return "", nil
	}
	// 2 参形式: 第二参为 flag 字符串, 逗号拆分 (空段被 ShellEx 内部跳过)。
	var flags []string
	for _, part := range strings.Split(args[1], ",") {
		if t := strings.TrimSpace(part); t != "" {
			flags = append(flags, t)
		}
	}
	if err := s.Proc.ShellEx(args[0], flags); err != nil {
		return "", fmt.Errorf("shell: %w", err)
	}
	return "", nil
}

func fnKeyTap(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Keys == nil {
		return "", fmt.Errorf("key.tap: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Keys.Tap(args[0]); err != nil {
		return "", fmt.Errorf("key.tap: %w", err)
	}
	return "", nil
}

func fnKeySeq(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Keys == nil {
		return "", fmt.Errorf("key.seq: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Keys.Sequence(args...); err != nil {
		return "", fmt.Errorf("key.seq: %w", err)
	}
	return "", nil
}

func fnKeyHold(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Keys == nil {
		return "", fmt.Errorf("key.hold: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Keys.Hold(args[0]); err != nil {
		return "", fmt.Errorf("key.hold: %w", err)
	}
	return "", nil
}

func fnKeyRelease(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Keys == nil {
		return "", fmt.Errorf("key.release: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Keys.Release(args[0]); err != nil {
		return "", fmt.Errorf("key.release: %w", err)
	}
	return "", nil
}

func fnKeyType(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Keys == nil {
		return "", fmt.Errorf("key.type: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Keys.TypeText(args[0]); err != nil {
		return "", fmt.Errorf("key.type: %w", err)
	}
	return "", nil
}

func fnClipCopy(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Clip == nil {
		return "", fmt.Errorf("clip.copy: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Clip.SetText(args[0]); err != nil {
		return "", fmt.Errorf("clip.copy: %w", err)
	}
	return "", nil
}

func fnClipPaste(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	if s.Clip == nil {
		return "", fmt.Errorf("clip.paste: %w", cmdbar.ErrServiceUnavailable)
	}
	if err := s.Clip.Paste(); err != nil {
		return "", fmt.Errorf("clip.paste: %w", err)
	}
	return "", nil
}

// searchEngineURLs maps engine ids from §3.4 to their query-URL prefix.
// %s is replaced with the URL-encoded query.
var searchEngineURLs = map[string]string{
	"baidu":  "https://www.baidu.com/s?wd=",
	"bing":   "https://www.bing.com/search?q=",
	"google": "https://www.google.com/search?q=",
	"zdic":   "https://www.zdic.net/hans/",
}

func fnSearch(ctx cmdbar.EvalContext, args []string) (string, error) {
	s, err := svcs(ctx)
	if err != nil {
		return "", err
	}
	engine := strings.ToLower(strings.TrimSpace(args[0]))
	q := args[1]
	// If host provides a custom SearchEngine, prefer it.
	if s.Search != nil {
		if err := s.Search.Search(engine, q); err != nil {
			return "", fmt.Errorf("search: %w", err)
		}
		return "", nil
	}
	// Default: compose URL and forward to Open.
	prefix, ok := searchEngineURLs[engine]
	if !ok {
		return "", fmt.Errorf("search: unknown engine %q", engine)
	}
	if s.Open == nil {
		return "", fmt.Errorf("search: %w", cmdbar.ErrServiceUnavailable)
	}
	target := prefix + url.QueryEscape(q)
	if err := s.Open.Open(target); err != nil {
		return "", fmt.Errorf("search: %w", err)
	}
	return "", nil
}
