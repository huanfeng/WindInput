package cmdbar

import (
	"errors"
	"fmt"
	"sync"
)

// ErrNotImplemented is returned by stub side-effect functions whose
// real implementation lives in later phases (P3+). Resolving a stub
// during evaluation surfaces this error so callers can degrade to the
// raw phrase.
var ErrNotImplemented = errors.New("function not implemented in this phase")

// EvalFunc is the signature of a registered command-bar function.
type EvalFunc func(ctx EvalContext, args []string) (string, error)

// FuncCategory 函数语义分组, 用于 wind_setting UI 函数浏览面板分类显示。
//
// 设计 (docs/design/command-bar-followup.md §1.3 / §1.4):
//   - value:   取值, 无 namespace (code/last/clip/sel/app/...)
//   - text:    文本处理 (len/upper/sub/replace/...)
//   - calc:    计算 (calc/num)
//   - action:  保留例外的裸名动作 (type/open)
//   - clip:    clip.* namespace 副作用
//   - key:     key.* namespace 副作用
//   - proc:    proc.* namespace 副作用
//   - dict:    dict.* namespace 副作用
//   - ime:     ime.* namespace 状态切换
//   - setting: setting.* namespace, 打开 wind_setting UI
//   - web:     web.* namespace, 搜索引擎
//   - meta:    内省 (help/list)
type FuncCategory string

const (
	CategoryValue   FuncCategory = "value"
	CategoryText    FuncCategory = "text"
	CategoryCalc    FuncCategory = "calc"
	CategoryAction  FuncCategory = "action"
	CategoryClip    FuncCategory = "clip"
	CategoryKey     FuncCategory = "key"
	CategoryProc    FuncCategory = "proc"
	CategoryDict    FuncCategory = "dict"
	CategoryIME     FuncCategory = "ime"
	CategorySetting FuncCategory = "setting"
	CategoryConfig  FuncCategory = "config" // config.get / config.set / config.toggle
	CategoryWeb     FuncCategory = "web"
	CategoryMeta    FuncCategory = "meta"
)

// FuncSpec is the metadata + entry point for a single registered
// function. MinArgs and MaxArgs are arity bounds (inclusive). MaxArgs
// may be -1 for variadic functions. Pure marks side-effect-free
// functions; only pure functions are permitted inside `$CC` display
// expressions (§5).
//
// 2026-05-16 (PR-3) 扩展元信息:
//   - Category:      语义分组, UI 渲染按此分组
//   - Deterministic: 同输入同输出 (Pure 函数中 code/last/clip/now/date/time 等
//     依赖外部状态, Deterministic=false; len/upper/sub/calc 等
//     无外部状态, Deterministic=true)。预留给将来求值缓存使用。
//   - Description:   一行说明, wind_setting 渲染手册
//   - ExampleSrc:    一行示例, 可在 UI 中"插入示例"按钮使用
//   - Deprecated:    标记 alias / 已弃用; AliasOf 指向新名
//
// alias 注册模式 (见 funcs/register.go::registerAliases): 用 Deprecated=true +
// AliasOf=<新名> 注册旧名, Eval 字段直接复用新函数, 保证行为完全等价。
// wind_setting 渲染时跳过 Deprecated 函数, 但 parser/evaluator 仍接受旧名以
// 兼容已存的用户数据。
type FuncSpec struct {
	Name          string
	Category      FuncCategory
	MinArgs       int
	MaxArgs       int // -1 for variadic
	Pure          bool
	Deterministic bool
	Deprecated    bool
	AliasOf       string
	Description   string
	ExampleSrc    string
	Eval          EvalFunc
}

// Accepts reports whether n is within the spec's arity bounds.
func (f FuncSpec) Accepts(n int) bool {
	if n < f.MinArgs {
		return false
	}
	if f.MaxArgs >= 0 && n > f.MaxArgs {
		return false
	}
	return true
}

// Registry is a thread-safe map of function specs keyed by name. The
// default registry is populated at package init with all §3.1-§3.3
// functions plus stub entries for §3.4-§3.5 (Pure=false, Eval returns
// ErrNotImplemented) so phrase parsing's arity check passes.
type Registry struct {
	mu    sync.RWMutex
	specs map[string]FuncSpec
}

// NewRegistry returns an empty Registry.
func NewRegistry() *Registry {
	return &Registry{specs: make(map[string]FuncSpec)}
}

// Register inserts spec, overwriting any prior entry with the same
// name.
func (r *Registry) Register(spec FuncSpec) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.specs[spec.Name] = spec
}

// Lookup returns the spec for name and whether it was found.
func (r *Registry) Lookup(name string) (FuncSpec, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	s, ok := r.specs[name]
	return s, ok
}

// Names returns a snapshot of all registered function names. Mainly
// useful for diagnostics.
func (r *Registry) Names() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]string, 0, len(r.specs))
	for k := range r.specs {
		out = append(out, k)
	}
	return out
}

// ListFuncs 返回所有已注册函数的完整元信息快照, 供 wind_setting UI 渲染
// 函数手册使用 (按 Category 分组, 跳过 Deprecated)。
//
// 注意: 顺序无定 (map 迭代), 调用方需要稳定顺序时应按 Category + Name 排序。
func (r *Registry) ListFuncs() []FuncSpec {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]FuncSpec, 0, len(r.specs))
	for _, s := range r.specs {
		out = append(out, s)
	}
	return out
}

// DefaultRegistry holds the package's default function set. The funcs
// subpackage populates it during init.
var DefaultRegistry = NewRegistry()

// stub returns an EvalFunc that always reports ErrNotImplemented. It is
// used to register side-effect placeholders that real P3+ wiring will
// later override.
func stub(name string) EvalFunc {
	return func(ctx EvalContext, args []string) (string, error) {
		return "", fmt.Errorf("%w: %s", ErrNotImplemented, name)
	}
}

// registerSideEffectStubs registers P3+ side-effecting functions as
// Pure=false stubs so the display-name purity check and the parse-time
// arity check work today.
//
// 2026-05-16 (PR-3): stubs 注册命名宪法的新名 (主) 与旧名 (alias). 真实
// Eval 由 funcs.RegisterActions 注入; alias 的 Eval 也被覆盖为指向同一
// 实现, 共享 services 调用路径。
func registerSideEffectStubs(r *Registry) {
	stubs := []FuncSpec{
		// 保留例外裸名 (跨 namespace 通用动作)
		{Name: "type", Category: CategoryAction, MinArgs: 1, MaxArgs: 1, Pure: false,
			Description: "把字符串作为上屏文本输出 (走 IME InsertText 通路)",
			ExampleSrc:  `type("hello")`,
			Eval:        stub("type")},
		{Name: "open", Category: CategoryAction, MinArgs: 1, MaxArgs: 1, Pure: false,
			Description: "打开 URL / 程序 / 文件 (通用 ShellExecute 语义)",
			ExampleSrc:  `open("https://baidu.com")`,
			Eval:        stub("open")},

		// key.* 按键模拟
		{Name: "key.tap", Category: CategoryKey, MinArgs: 1, MaxArgs: 1, Pure: false,
			Description: "模拟单次按键组合",
			ExampleSrc:  `key.tap("Ctrl+C")`,
			Eval:        stub("key.tap")},
		{Name: "key.seq", Category: CategoryKey, MinArgs: 1, MaxArgs: -1, Pure: false,
			Description: "顺序模拟多个按键组合",
			ExampleSrc:  `key.seq("Home", "Shift+End", "Delete")`,
			Eval:        stub("key.seq")},

		// clip.* 剪贴板
		{Name: "clip.copy", Category: CategoryClip, MinArgs: 1, MaxArgs: 1, Pure: false,
			Description: "把文本写入系统剪贴板",
			ExampleSrc:  `clip.copy(last())`,
			Eval:        stub("clip.copy")},
		{Name: "clip.paste", Category: CategoryClip, MinArgs: 0, MaxArgs: 0, Pure: false,
			Description: "模拟 Ctrl+V 粘贴",
			ExampleSrc:  `clip.paste()`,
			Eval:        stub("clip.paste")},

		// proc.* 进程 (新名)
		{Name: "proc.run", Category: CategoryProc, MinArgs: 1, MaxArgs: -1, Pure: false,
			Description: "启动外部程序, 可带参数",
			ExampleSrc:  `proc.run("notepad.exe")`,
			Eval:        stub("proc.run")},
		{Name: "proc.shell", Category: CategoryProc, MinArgs: 1, MaxArgs: 2, Pure: false,
			Description: "通过 cmd /c 执行命令行; 第二参可选 flag (term/pwsh)",
			ExampleSrc:  `proc.shell("echo hi")`,
			Eval:        stub("proc.shell")},

		// dict.* 词库 (新名)
		{Name: "dict.add", Category: CategoryDict, MinArgs: 1, MaxArgs: 2, Pure: false,
			Description: "把文本加入用户词库 (code 可选, 不传时按当前方案规则推导)",
			ExampleSrc:  `dict.add(clip())`,
			Eval:        stub("dict.add")},

		// ime.* IME 状态切换
		{Name: "ime.toggle", Category: CategoryIME, MinArgs: 1, MaxArgs: 1, Pure: false,
			Description: "切换 IME 状态 (cn-en / fullshape / layout / candwin)",
			ExampleSrc:  `ime.toggle("cn-en")`,
			Eval:        stub("ime.toggle")},

		// setting.* 设置 UI (新名, 独立 namespace)
		{Name: "setting.open", Category: CategorySetting, MinArgs: 1, MaxArgs: 1, Pure: false,
			Description: "打开 wind_setting 指定页面",
			ExampleSrc:  `setting.open("dict")`,
			Eval:        stub("setting.open")},

		// web.* 搜索引擎 (新名)
		{Name: "web.search", Category: CategoryWeb, MinArgs: 2, MaxArgs: 2, Pure: false,
			Description: "使用搜索引擎搜索 (baidu / bing / google / zdic)",
			ExampleSrc:  `web.search("baidu", last())`,
			Eval:        stub("web.search")},

		// 交互 (P5, 未实现)
		{Name: "ask", Category: CategoryAction, MinArgs: 1, MaxArgs: 1, Pure: false,
			Description: "弹小输入框, 阻塞返回用户输入 (未实现)",
			Eval:        stub("ask")},
		{Name: "pick", Category: CategoryAction, MinArgs: 1, MaxArgs: -1, Pure: false,
			Description: "弹下拉列表选择 (未实现)",
			Eval:        stub("pick")},

		// 函数命名宪法 (command-bar-followup.md §1) 已切换到 canonical 新名,
		// 旧名 alias (run / shell / dict.addword / ime.setting / search) 已删除。
		// FuncSpec.Deprecated / AliasOf 字段保留, 供未来潜在 alias 使用。
	}
	for _, s := range stubs {
		r.Register(s)
	}
}

func init() {
	registerSideEffectStubs(DefaultRegistry)
}
