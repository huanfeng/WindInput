// action.go — P5 引入的 ResolvedAction 模型, 用于把 cmdbar 的副作用区分为
// "纯效果" (effect, 调一下就完事) 与 "文本插入" (text, 需要走 TSF CommitText
// 通路落字)。设计见 docs/design/command-bar-design.md §3.4 / §5。
//
// 由 eval.Evaluate 在解析 CommandPhrase.Actions 时构造, 由 coordinator
// 在 doSelectCandidate 阶段消费: 把所有 ActionText 的字符串拼成一段
// InsertText 走 IME 输出通路, 同时按时序执行 ActionEffect (text 之前同步,
// text 之后异步 30ms 等待落字)。
package cmdbar

// ActionKind 标记 ResolvedAction 是纯副作用还是文本上屏。
type ActionKind int

const (
	// ActionEffect 仅产生副作用, Run 返回的字符串恒为空。
	ActionEffect ActionKind = iota
	// ActionText 需要把 Run 返回的字符串通过 TSF InsertText 上屏;
	// 副作用部分由调用方负责后续合并。
	ActionText
)

// ResolvedAction 是 eval 输出给宿主的统一执行单元。
//
//   - Kind = ActionEffect: Run() 调用时同步执行副作用, 返回 ("", err)。
//   - Kind = ActionText:   Run() 返回上屏文本 (text, nil) 或 ("", err)。
//
// Run 保持闭包形态以延迟参数表达式求值: 例如 `type(last())` 必须每次触发
// 时重新取 last(), 与原 ActionThunk 模型对齐。
type ResolvedAction struct {
	Kind ActionKind
	Run  func() (string, error)
}
