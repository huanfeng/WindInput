// cmdbar_context.go — 命令直通车 EvalContext 的 coordinator 适配实现。
// 把 coordinator 当前的输入缓冲、历史、剪贴板、Services 暴露给 cmdbar
// 求值器使用。
//
// 线程模型: EvalContext 的方法可能在 hook 求值阶段被 doSelectCandidate
// 持锁直接调用 (Input/Last), 也可能在 goroutine 中通过 action thunk 异步调用
// (Clip/Last 等)。Input() 字段是触发时的快照, 异步 action 仍能拿到当时的
// 编码 (此时 c.inputBuffer 已被 clearState 清空); Last() 走 inputHistory
// 自身的锁, 不依赖 c.mu。
package coordinator

import (
	"os"
	"time"

	"github.com/huanfeng/wind_input/internal/clipboard"
	"github.com/huanfeng/wind_input/internal/cmdbar"
	"github.com/huanfeng/wind_input/internal/foreground"
)

// cmdbarEvalContext 实现 cmdbar.EvalContext。
//
// 注意: input/history 字段在 hook 进入时一次性写入并随 evalCtx 闭包逃逸到
// action thunks; 也就是说 action 异步执行时 input 仍是触发候选时的快照,
// last() 走 inputHistory 当时引用 (records 自身的环形容器在 inputHistory
// 内部继续增长, 但 evalCtx 不缓存它们 —— 直接调 GetRecentRecords 取最新)。
type cmdbarEvalContext struct {
	input    string           // 触发候选时的 inputBuffer 快照 (code() 的来源)
	history  *InputHistory    // coordinator 共享的输入历史 (last() 的来源)
	services *cmdbar.Services // coordinator 装配的 Services
}

// newCmdbarEvalContextLocked 构造一个 EvalContext (调用方需持 c.mu)。
// inputSnapshot 由调用方传入 (通常是 c.inputBuffer); 快照后字段值与
// coordinator 状态解耦, 即使 inputBuffer 后续被清空也不影响 action thunks。
func (c *Coordinator) newCmdbarEvalContextLocked(inputSnapshot string) *cmdbarEvalContext {
	return &cmdbarEvalContext{
		input:    inputSnapshot,
		history:  c.inputHistory,
		services: c.cmdbarServices,
	}
}

func (ctx *cmdbarEvalContext) Input() string { return ctx.input }

// Last 返回最近第 n 条上屏文本 (1-based)。统一走 coordinator.inputHistory,
// 与 z 键重复上屏 / 加词推荐使用同一事实源。n 越界返回空。
func (ctx *cmdbarEvalContext) Last(n int) string {
	if ctx.history == nil || n < 1 {
		return ""
	}
	records := ctx.history.GetRecentRecords(n, 0)
	if n > len(records) {
		return ""
	}
	return records[n-1].Text
}

// Clip 仅支持当前剪贴板 (n<=1); 剪贴板栈 (n>1) 留待 P5 实现, 暂返回空。
func (ctx *cmdbarEvalContext) Clip(n int) string {
	if n > 1 {
		// TODO(P5): 接入剪贴板历史栈
		return ""
	}
	text, err := clipboard.GetText()
	if err != nil {
		return ""
	}
	return text
}

// Sel 仍占位返回空 (兼容性问题, 暂不实现); App/Title 走 foreground 包
// 真实读 Win32 前台窗口元数据。见 design §4.3。
func (ctx *cmdbarEvalContext) Sel() string   { return "" }
func (ctx *cmdbarEvalContext) App() string   { return foreground.App() }
func (ctx *cmdbarEvalContext) Title() string { return foreground.Title() }

func (ctx *cmdbarEvalContext) Env(name string) string {
	if name == "" {
		return ""
	}
	return os.Getenv(name)
}

func (ctx *cmdbarEvalContext) Now() time.Time { return time.Now() }

func (ctx *cmdbarEvalContext) Services() *cmdbar.Services { return ctx.services }
