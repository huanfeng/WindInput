// toolbar_reducer.go — 工具栏可见性单点决策器。
//
// 设计动机：在引入 reducer 前，"是否显示工具栏" 的决策公式
//
//	shouldShow = imeActivated && userWantsVisible && !(fullscreen && hideInFullscreen)
//
// 被复制在 7 处事件入口（SetIMEActivated true/false、HandleIMEDeactivated、
// HandleClientDisconnected、HandleMenuCommand("toggle_toolbar")、
// OnShellFullscreenChange、handleToolbarContextMenu），每处都自行算位置 + 直接调 UI。
// 任一处行为变化都要同步改 6 个 sibling，多次回归都源自此。
//
// reducer 把决策集中到独立 goroutine：所有原入口降级为投递事件（"发生了什么"），
// 由 reducer 维护内部状态机、做 debounce 合并、单点产出 Show / Hide 命令。
//
// 不参与 reducer 的部分：候选窗 Hide / StatusIndicator / 全局热键注册 —— 这些
// 与工具栏可见性正交，保留在原 handler 中即可。
//
// 死锁防护：reducer 不在持锁回调中等待 coordinator；所有快照通过 snapshotToolbarShowParams
// 一次性取出（短临 c.mu）。事件投递走 send（关键事件最多阻塞 100ms，非关键事件 drop）。
package coordinator

import (
	"time"

	"github.com/huanfeng/wind_input/internal/ui"
)

// toolbarEventKind 枚举 reducer 接受的所有事件种类。
type toolbarEventKind uint8

const (
	tevIMEActivated toolbarEventKind = iota
	tevIMEDeactivated
	tevAllClientsDisconnected
	tevUserPreferenceChanged
	tevFullscreenChanged
	tevConfigChanged
	tevCaretChanged
	tevToolbarContentRefresh
)

// toolbarEvent 由 coordinator 各事件入口投递到 reducer。
//
// payload 字段按事件类型语义复用：
//   - tevUserPreferenceChanged   : visible
//   - tevFullscreenChanged       : visible (= enter fullscreen)
//   - tevConfigChanged           : visible (= hideInFullscreen)
//   - tevCaretChanged            : x, y, valid
type toolbarEvent struct {
	kind    toolbarEventKind
	visible bool
	x, y    int
	valid   bool
}

// toolbarReducerQueueSize 选 16：单次 burst 内常见入口数远小于此（IME activate
// burst 通常 ≤4 个事件），16 留出 4× 余量；同时小到不会掩盖真实积压。
const toolbarReducerQueueSize = 16

// toolbarDebounceWindow IME 激活/焦点切换的 burst 合并窗口。实测 FocusGained →
// IME_Activated → 首个 caret update 间隔在 30-50ms，50ms 既能合并 burst 避免可见
// 闪烁，又不至于让用户感知到工具栏延迟出现。
const toolbarDebounceWindow = 50 * time.Millisecond

// toolbarCriticalSendTimeout 关键事件（IME_Deactivated / AllClientsDisconnected）
// 投递超时：避免 reducer 卡死时无限阻塞调用者。100ms 足够覆盖任何正常 reconcile
// 周期，超时仍未送达说明 reducer 异常，此时记 warn 但不阻塞 IME 事件路径。
const toolbarCriticalSendTimeout = 100 * time.Millisecond

// toolbarReducer 工具栏可见性状态机。单 goroutine 串行处理所有事件，无需内部锁。
type toolbarReducer struct {
	coord *Coordinator
	in    chan toolbarEvent

	// 状态机字段（仅 reducer goroutine 访问）
	imeActivated     bool
	userWantsVisible bool
	fullscreen       bool
	hideInFullscreen bool
	caretX, caretY   int
	caretValid       bool

	// lastShown 上次实际下发到 UI 的可见状态，用于幂等去重。
	lastShown bool
}

// newToolbarReducer 构造并启动 reducer goroutine。
// 初始状态来自 coordinator 当前快照，避免首次事件到达前 reducer 与现实不一致。
func newToolbarReducer(c *Coordinator) *toolbarReducer {
	r := &toolbarReducer{
		coord: c,
		in:    make(chan toolbarEvent, toolbarReducerQueueSize),
	}

	// 初始状态快照（NewCoordinator 末尾调用，此时 c.mu 未被外部持有，
	// 但仍按惯例加锁，避免未来调用点迁移引入竞态）。
	c.mu.Lock()
	r.userWantsVisible = c.toolbarVisible
	if c.config != nil {
		r.hideInFullscreen = c.config.Toolbar.IsHideInFullscreen()
	}
	r.caretX, r.caretY, r.caretValid = c.caretX, c.caretY, c.caretValid
	c.mu.Unlock()

	go r.run()
	return r
}

// sendNonBlocking 用于非关键事件（caret / content refresh / fullscreen 等）：
// 队列满则 drop，记 debug 日志便于排查。
//
// 语义保证：所有 dropable 事件本质都是 idempotent —— caret 后续会再来、
// content refresh 由下次状态变化触发、fullscreen 进/出会成对补齐。drop 不会
// 让状态机永久错位。
func (r *toolbarReducer) sendNonBlocking(e toolbarEvent) {
	select {
	case r.in <- e:
	default:
		r.coord.logger.Debug("toolbar reducer queue full, dropping event", "kind", e.kind)
	}
}

// sendCritical 用于关键事件（IME activate/deactivate / disconnect / user toggle）：
// 最多阻塞 toolbarCriticalSendTimeout，超时记 warn 仍丢弃。超时通常意味着 reducer
// goroutine 异常卡死，此时阻塞 IME 路径无济于事。
func (r *toolbarReducer) sendCritical(e toolbarEvent) {
	select {
	case r.in <- e:
	case <-time.After(toolbarCriticalSendTimeout):
		r.coord.logger.Warn("toolbar reducer send timed out, dropping critical event", "kind", e.kind)
	}
}

// run 是 reducer 主循环。事件到达后启动 debounce 窗口，把窗口内的后续事件
// 合并进同一次 reconcile，避免 burst 期间反复 Show/Hide。
func (r *toolbarReducer) run() {
	for {
		e, ok := <-r.in
		if !ok {
			return
		}
		r.apply(e)

		// debounce 窗口：合并 burst。
		timer := time.NewTimer(toolbarDebounceWindow)
	drain:
		for {
			select {
			case e2, ok := <-r.in:
				if !ok {
					if !timer.Stop() {
						<-timer.C
					}
					r.reconcile()
					return
				}
				r.apply(e2)
			case <-timer.C:
				break drain
			}
		}

		r.reconcile()
	}
}

// apply 把单个事件折叠到内部状态。**不**触发 reconcile —— 由 run() 在 debounce
// 窗口结束时统一 reconcile，确保 burst 期间只下发一次 UI 命令。
//
// 注意：tevCaretChanged 与 tevToolbarContentRefresh 即便 apply 后 reconcile 也
// 不会改变可见性（仅刷位置 / 内容），但保留它们经过主循环有两个意义：
//  1. 让 caret 缓存对下一次真正的 Show 立即可用；
//  2. content refresh 在可见 → 不可见 → 再可见的场景能正确触发重画。
func (r *toolbarReducer) apply(e toolbarEvent) {
	switch e.kind {
	case tevIMEActivated:
		r.imeActivated = true
	case tevIMEDeactivated, tevAllClientsDisconnected:
		r.imeActivated = false
	case tevUserPreferenceChanged:
		r.userWantsVisible = e.visible
	case tevFullscreenChanged:
		r.fullscreen = e.visible
	case tevConfigChanged:
		r.hideInFullscreen = e.visible
	case tevCaretChanged:
		r.caretX, r.caretY, r.caretValid = e.x, e.y, e.valid
	case tevToolbarContentRefresh:
		// 仅触发 reconcile 重画，无状态变更
	}
}

// reconcile 单点决策 + 下发。幂等：lastShown 与 want 相同时跳过 UI 调用。
//
// shouldShow 公式严格反映"原 7 处入口"的合取语义；任何业务规则变化只需改这里。
func (r *toolbarReducer) reconcile() {
	want := r.imeActivated && r.userWantsVisible && !(r.fullscreen && r.hideInFullscreen)

	if r.coord.uiManager == nil {
		return
	}

	if !want {
		if r.lastShown {
			r.coord.uiManager.SetToolbarVisible(false)
			r.lastShown = false
			r.coord.logger.Debug("toolbar reducer: hide",
				"ime", r.imeActivated, "userWants", r.userWantsVisible,
				"fullscreen", r.fullscreen, "hideInFS", r.hideInFullscreen)
		}
		return
	}

	// want=true：取快照（位置 + ToolbarState），下发 ShowToolbarWithState。
	// 不论 lastShown 是否为 true 都要刷一次：覆盖 content refresh / caret 变化 /
	// 重新进入焦点等可见性未翻转但内容/位置需更新的场景。
	posX, posY, state := r.coord.snapshotToolbarShowParams(r.caretX, r.caretY, r.caretValid)
	r.coord.uiManager.ShowToolbarWithState(posX, posY, state)
	if !r.lastShown {
		r.coord.logger.Debug("toolbar reducer: show",
			"x", posX, "y", posY,
			"ime", r.imeActivated, "userWants", r.userWantsVisible,
			"fullscreen", r.fullscreen, "hideInFS", r.hideInFullscreen)
	}
	r.lastShown = true
}

// snapshotToolbarShowParams 在持有 c.mu 的前提下，返回工具栏 Show 所需的位置 +
// ToolbarState。preferCaretX/Y/Valid 来自 reducer 缓存：若 reducer 已经从
// tevCaretChanged 收到比 c.caretState 更新的值，则优先采用，避免 reducer 与
// coordinator 内部状态偶发漂移导致工具栏定位回退。
//
// 函数体短临锁，不调用任何会回投事件的 UI 方法 —— 调用方（reducer）持锁期间
// 不会触发 reducer 自身回环。
func (c *Coordinator) snapshotToolbarShowParams(preferCaretX, preferCaretY int, preferCaretValid bool) (int, int, ui.ToolbarState) {
	c.mu.Lock()
	defer c.mu.Unlock()

	// reducer 缓存的 caret 比 coordinator 字段更新时，临时覆盖再调
	// computeToolbarPositionLocked，避免位置算到旧坐标。
	savedX, savedY, savedValid := c.caretX, c.caretY, c.caretValid
	if preferCaretValid {
		c.caretX, c.caretY, c.caretValid = preferCaretX, preferCaretY, true
	}
	posX, posY := c.computeToolbarPositionLocked()
	c.caretX, c.caretY, c.caretValid = savedX, savedY, savedValid

	return posX, posY, c.buildToolbarState()
}
