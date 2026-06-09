// Package coordinator orchestrates communication between C++ Bridge, Engine, and UI
package coordinator

import (
	"context"
	"encoding/hex"
	"image"
	"image/color"
	"log/slog"
	"strings"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/cmdbar"
	cmdbarast "github.com/huanfeng/wind_input/internal/cmdbar/ast"
	cmdbareval "github.com/huanfeng/wind_input/internal/cmdbar/eval"
	cmdbarfuncs "github.com/huanfeng/wind_input/internal/cmdbar/funcs"
	cmdbarparser "github.com/huanfeng/wind_input/internal/cmdbar/parser"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/hotkey"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/internal/tooltip"
	"github.com/huanfeng/wind_input/internal/transform"
	"github.com/huanfeng/wind_input/internal/transform/s2t"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/theme"
)

// Restart request channel - main should listen to this
var restartRequestCh = make(chan struct{}, 1)

// RequestRestart signals that a restart is requested
func RequestRestart() {
	select {
	case restartRequestCh <- struct{}{}:
	default:
		// Channel already has a request pending
	}
}

// RestartRequested returns a channel that signals when restart is requested
func RestartRequested() <-chan struct{} {
	return restartRequestCh
}

// Exit request channel - main should listen to this
var exitRequestCh = make(chan struct{}, 1)

// RequestExit signals that an application exit is requested
func RequestExit() {
	select {
	case exitRequestCh <- struct{}{}:
	default:
		// Channel already has a request pending
	}
}

// ExitRequested returns a channel that signals when exit is requested
func ExitRequested() <-chan struct{} {
	return exitRequestCh
}

// Modifier key flags (must match C++ side)
const (
	ModShift = 0x01
	ModCtrl  = 0x02
	ModAlt   = 0x04
	ModWin   = 0x08 // Command 键（macOS ⌘，Swift 已映射 .command→0x08）
)

// EffectiveMode represents the effective input mode considering CapsLock
type EffectiveMode int

const (
	ModeChinese      EffectiveMode = iota // 中文模式
	ModeEnglishLower                      // 英文小写模式
	ModeEnglishUpper                      // 英文大写模式 (CapsLock on)
)

// ConfirmedSegment 代表拼音分步确认中一个已确认但未上屏的文本段。
// 用户选词后，如果输入缓冲区未完全消费，候选文字暂存于此而非直接上屏，
// 用户可通过退格键回退到上一个确认段重新选词。
type ConfirmedSegment struct {
	Text         string // 已确认的汉字，如 "我们"
	ConsumedCode string // 消耗的原始拼音编码，如 "women"
}

// caretState 光标位置与自适应检测相关状态
type caretState struct {
	// 当前光标位置（来自 C++ TSF Bridge）
	caretX      int
	caretY      int
	caretHeight int
	caretValid  bool // true if we have received valid caret position (coordinates can be negative in multi-monitor)

	// Composition start position: captured when inputBuffer transitions from empty to non-empty.
	// Used to anchor the candidate window at the start of the composition when inline preedit is enabled,
	// instead of following the current caret position which moves as the user types.
	compositionStartX     int
	compositionStartY     int
	compositionStartValid bool

	// Last known valid window position (for fallback)
	lastValidX int
	lastValidY int

	// 当前活跃进程信息
	activeProcessID   uint32    // 当前活跃进程 ID
	activeProcessName string    // 当前活跃进程名（小写）
	lastKeyTime       time.Time // 最近一次按键处理的时间

	// 首次 show 推迟：当本次按键启动一个新的 composition 时（如 WPS 会触发文本
	// reflow 让光标位置变化），不能用按键前的旧坐标显示候选窗，否则会先在错误
	// 位置出现再跳到正确位置。设置 pendingFirstShow=true 后，handleAlphaKey 不
	// 立即 showUI；待 HandleCaretUpdate 收到 reflow 后的新坐标时再 show。
	// pendingFirstShowToken 用于超时回调比对身份。
	pendingFirstShow      bool
	pendingFirstShowToken uint64

	// Excel/WPS 表格等"打字驱动焦点切换"应用兼容：单元格选中态收到首键时，
	// 应用会立即切换到单元格编辑态的 ITfDocumentMgr，旧 composition 被终止，
	// 此时清空 buffer 会丢失用户首键意图。检测条件（HandleFocusLost 中评估）：
	//   1) pendingFirstShow=true（composition 刚启动，候选还没来得及 show）
	//   2) inputBuffer 非空且短（≤8）
	//   3) 距上次按键 <200ms
	// 满足时设置 pendingReplay=true 并保留 buffer，等同 PID 在 500ms 内
	// HandleFocusGained 重新到达时，向新文档 push update_composition 重放。
	pendingReplay         bool
	pendingReplayPID      uint32
	pendingReplayDeadline time.Time
}

// tempModeState 临时输入模式（临时英文/临时拼音）状态
type tempModeState struct {
	tempEnglishMode       bool                  // 是否处于临时英文模式
	tempEnglishTriggerKey string                // 当前使用的触发键类型（触发键方式进入时）
	tempEnglishBuffer     string                // 临时英文缓冲区
	tempEnglishCursorPos  int                   // 临时英文光标位置
	tempEnglishCandidates []candidate.Candidate // 临时英文模式的英文候选列表
	// 临时英文分级加载状态（对标正常模式的 candidateLimit/candidateInput/hasMoreCandidates）
	tempEnglishCandLimit int    // 当前英文候选加载上限
	tempEnglishCandInput string // 加载时的 tempEnglishBuffer 快照
	tempEnglishHasMore   bool   // 词库里是否还有更多候选未取出
	tempPinyinMode       bool   // 是否处于临时拼音模式
	tempPinyinBuffer     string // 临时拼音输入缓冲区
	tempPinyinCursorPos  int    // 临时拼音光标位置
	tempPinyinCommitted  string // 临时拼音部分上屏累积文本
	tempPinyinTriggerKey string // 临时拼音触发键类型（"backtick"/"semicolon"/"z"）

	// z 键混合模式回退缓存: 仅在从 zHybridFallback 切入临时拼音时记录,
	// 让用户的下一次 backspace 在"什么都还没敲"的状态下能回到正常输入流.
	// 任何新字符插入都会清空这两个字段, 使回退仅对"切入即误判"场景有效.
	tempPinyinRewindBuffer string // 切入前的 inputBuffer (例如 "zzh")
	tempPinyinRewindKey    string // 触发切入的小写字母 (例如 "a")
}

// addWordState 快捷加词模式状态
type addWordState struct {
	addWordActive bool   // 是否处于加词模式
	addWordChars  []rune // 可选字符池
	addWordLen    int    // 当前选取的词长
	addWordCode   string // 自动计算的编码
}

// quickInputState 快捷输入模式状态
type quickInputState struct {
	quickInputMode              bool                   // 是否处于快捷输入模式
	quickInputTriggerKey        string                 // 当前使用的触发键类型（如 "semicolon"）
	quickInputBuffer            string                 // 触发键后的输入缓冲区（不含触发键本身）
	quickInputPinyinMode        bool                   // 是否处于快捷输入的临时拼音子模式
	quickInputPinyinBuffer      string                 // 快捷输入临时拼音缓冲区
	quickInputPinyinCursorPos   int                    // 快捷输入拼音光标位置
	quickInputPinyinCommitted   string                 // 快捷输入拼音部分上屏累积文本
	quickInputPinyinDictSwapped bool                   // 是否已交换词库层（仅码表引擎下为 true）
	savedLayout                 config.CandidateLayout // 进入快捷输入前的布局（用于退出时恢复）
}

// specialModeState 引导键特殊模式（自定义码表）状态
type specialModeState struct {
	specialMode        bool                   // 是否处于特殊模式
	specialActiveID    string                 // 当前激活实例 id
	specialTriggerKey  string                 // 当前触发键
	specialBuffer      string                 // 编码缓冲（不含触发符）
	specialSavedLayout config.CandidateLayout // 进入前布局（force_vertical 时恢复用）
	// 动态分级加载（对标正常模式 candidateLimit/candidateInput/hasMoreCandidates）：
	// 初始只取一小批，翻页到末尾时 expandSpecialCandidates 翻倍重查。
	specialCandidateLimit int    // 当前候选加载上限
	specialCandidateInput string // 加载时的 specialBuffer 快照
	specialHasMore        bool   // 码表中是否还有更多候选未加载
}

// Coordinator orchestrates between C++ Bridge, Engine, and native UI
type Coordinator struct {
	engineMgr    *engine.Manager
	uiManager    *ui.Manager
	logger       *slog.Logger
	config       *config.Config
	bridgeServer BridgeServer // Interface for broadcasting state to TSF clients
	version      string       // App version for display in menu

	mu    sync.Mutex
	cfgMu *sync.RWMutex // 与 rpc.Server 共享，守护 *config 读写（cfgMu → mu 顺序）

	// Input mode state
	chineseMode bool // true = Chinese, false = English
	capsLockOn  bool // CapsLock state (authority source)

	// 敏感字段（密码/隐私）输入抑制：焦点进入 IS_PASSWORD/IS_PRIVATE 等控件时，
	// 按英文半角直通处理输入，但**不改变** chineseMode（图标仍显示当前模式，与主流
	// 输入法一致）。离开该字段时清除标志即可，无需存/恢复模式。
	// 详见 handle_lifecycle.go 中 applyPasswordFieldPolicyNoLock，输入侧消费点在
	// handle_key_event.go（按 sensitiveFieldActive 把 chineseMode/fullWidth 视为关闭）。
	sensitiveFieldActive bool

	// Full-width and punctuation state
	fullWidth          bool // true = full-width, false = half-width
	chinesePunctuation bool // true = Chinese punctuation, false = English punctuation
	punctFollowMode    bool // true = punctuation follows Chinese/English mode
	toolbarVisible     bool // true = 用户偏好显示工具栏（菜单 toggle / config）
	imeActivated       bool // true = IME 已激活（持有焦点），由 SetIMEActivated 维护
	// 历史字段 toolbarSuppressedByFullscreen 已迁入 toolbarReducer.fullscreen，
	// 不再在 coordinator 层维护；ShellHook / IME 入口统一向 reducer 投递事件。

	// toolbarReducer 工具栏可见性单点决策器，详见 toolbar_reducer.go。
	// 在 NewCoordinator 末尾初始化；之后所有显隐变化通过事件投递触达。
	toolbarReducer *toolbarReducer

	// Input state
	inputBuffer        string
	inputCursorPos     int                // 光标在 inputBuffer 中的字节位置（0 = 最左，len(inputBuffer) = 最右）
	preeditDisplay     string             // 带音节分隔符的显示文本（如 "zhong guo"），五笔时为空
	syllableBoundaries []int              // 音节边界在 inputBuffer 中的位置（如 [5] 表示位置 5 处有分隔符）
	confirmedSegments  []ConfirmedSegment // 拼音分步确认：已确认但未上屏的文本段
	candidates         []ui.Candidate
	currentPage        int
	totalPages         int
	// candidatesPerPage 是「当前生效」的每页候选数（物化值），所有分页/选择/切片逻辑读它。
	// 它在每条分页源头由 refreshEffectivePerPage() 写入：普通码表输入用 candidatesPerPageBase，
	// 临时拼音/快捷输入/短语/拼音引擎等场景用 candidatesPerPageExtended（若已配置）。
	candidatesPerPage int
	// candidatesPerPageBase 用户配置的基础档（初始化/热更新写它）；candidatesPerPageExtended
	// 用户配置的扩展档（<=0 表示禁用，始终用基础档）。两者只读，生效值物化到 candidatesPerPage。
	candidatesPerPageBase     int
	candidatesPerPageExtended int
	selectedIndex             int // 当前页内选中的候选索引（0-based），用于上下箭头键选择

	// 分级加载状态
	candidateLimit    int    // 当前加载上限（0=无限制）
	candidateInput    string // 加载时的 inputBuffer 快照
	hasMoreCandidates bool   // 是否还有更多候选未加载
	pendingFirstKey   bool   // 下一次 updateCandidatesEx 视为首键（由 handleAlphaKey 设置）

	// 光标位置与自适应检测
	caretState

	// 应用兼容性规则
	appCompat        *config.AppCompat     // 兼容性规则（从 compat.yaml 加载）
	activeCompatRule *config.AppCompatRule // 当前进程匹配的兼容性规则（nil 表示无特殊处理）

	// 临时输入模式
	tempModeState

	// Punctuation converter with state (for paired punctuation like quotes)
	punctConverter *transform.PunctuationConverter

	// Auto-pair tracker for bracket pairing (push on insert, pop on skip/delete)
	pairTracker    *transform.PairTracker
	pairTrackerEn  *transform.PairTracker // 英文配对追踪器
	pairInsertTime time.Time              // 最近一次自动配对插入的时间，用于抑制 SelectionChanged 清栈

	// lastSelfCommitTime 最近一次 IME 主动向宿主提交文本的时间。
	// 用于在 HandleSelectionChanged 里区分"用户主动操作"和"IME 自家提交导致的回响"：
	// 我们提交候选词后宿主插入文本，会立即触发一次 OnEndEdit → HandleSelectionChanged，
	// 若不加 grace window 会让自动造词把刚 append 的字立刻 flush 掉（bufLen=1 不足 → 清空）。
	lastSelfCommitTime time.Time

	// 简入繁出（S->T）转换管理器（lazy 加载，单例）
	s2tManager *s2t.Manager

	// Hotkey compiler for binary protocol
	hotkeyCompiler *hotkey.Compiler

	// 热键缓存（避免每次焦点变化重新编译）
	cachedKeyDownHotkeys []uint32
	cachedKeyUpHotkeys   []uint32
	hotkeysDirty         bool // 配置变化时置 true

	// Dark mode watcher for system theme changes
	darkModeWatcher *theme.DarkModeWatcher

	// 输入历史：追踪最近上屏文字，用于加词推荐
	inputHistory *InputHistory

	// 数字后智能标点：追踪上一个直通输出是否为数字
	// 用于在中文标点模式下将数字后的 。→. ，→, 自动转换为英文标点
	lastOutputWasDigit bool

	// 快捷加词模式
	addWordState

	// 快捷输入模式
	quickInputState

	// 引导键特殊模式（自定义码表）
	specialModeState
	specialModeReg *specialModeRegistry

	// 输入统计采集器
	statCollector *store.StatCollector
	statRecorded  bool // 当前按键处理中是否已记录统计

	// keyPhaseTimer 在 HandleKeyEvent 单次调用范围内累积各 phase 耗时, defer 时若超阈
	// 输出 breakdown WARN。nil 在 HandleKeyEvent 之外, markKeyPhase 调用静默忽略。
	// 持有 c.mu 期间访问, 不需要额外锁。
	keyPhaseTimer *phaseTimer

	// 事件通知器（旁路 RPC 路径变更时用于广播给设置端订阅者，nil-safe）
	eventNotifier EventNotifier

	// toolbarUserPos 保存用户拖动后的工具栏位置，按显示器分别记录。
	// key = ui.MonitorKeyStr(workRight, workBottom)，value = 工具栏左上角屏幕坐标。
	// 焦点切换时优先使用该值；切换到没有记录的显示器时回退到右下角默认位置。
	toolbarUserPos map[string]image.Point

	// candidatePinPositions 保存启用了「固定候选位置」的应用候选窗拖动后的位置。
	// 外层 key = 小写进程名；内层 key = ui.MonitorKeyStr(workRight, workBottom)，value = [x, y]。
	// 焦点切换 / 菜单 toggle / 拖动落盘后同步推送到 uiManager.SetActiveAppPinState。
	candidatePinPositions map[string]map[string][2]int

	// tooltip 异步查询（独立锁，避免与主锁形成死锁）
	tooltipService  *tooltip.Service
	tooltipCancel   context.CancelFunc
	tooltipHoverIdx int
	tooltipMu       sync.Mutex

	// 命令直通车 (cmdbar) 集成。
	// last() 与 z 键重复上屏共用 inputHistory (上面 c.inputHistory 字段),
	// 不再维护独立的 cmdbarHistory。Services 提供 clipboard/key/process/
	// dict/ime 等动作后端, 由 cmdbar.EvalContext 暴露给求值器使用。
	// cmdbarServices 在 NewCoordinator 中初始化, 之后只读, 不需要额外加锁。
	cmdbarServices *cmdbar.Services
	// cmdbarValueExpander 共享 hook + 全局 templateEngine, 供候选后处理统一展开
	// 含 "$CC(" 或 "$X" 的任意候选 (码表/用户词库/拼音), 不只局限于短语。
	// 不持有锁, Expand 是纯函数 + hook 闭包内不再回环 c.mu。
	cmdbarValueExpander *dict.ValueExpander

	// expandedGroupTemplate 用户主动选中 collapsed group nav 后, 标记"这个 code 的组保持展开"。
	// 由 doSelectCandidate 内 IsGroup 分支在 cand.GroupCode == c.inputBuffer 时设值,
	// 在 inputBuffer 发生任何变化时清零 (handleAlphaKey / handleBackspace / handleDelete /
	// handleCursorXxx / clearState 等入口), 确保用户重新输入后回到默认 collapse 行为。
	// 详见 collapseGroupMembersIfMixed 文档。
	expandedGroupTemplate string
}

// EventNotifier 由外部（rpc.Server 适配器）注入。当 coordinator 旁路 RPC 路径
// 直接修改状态（如热键切换方案、快捷加词）时，通过本接口广播事件，使设置端等
// 订阅者能感知变化。允许为 nil（无副作用）。
type EventNotifier interface {
	NotifyConfigUpdate()               // 配置层变更（schema.active 切换等）
	NotifyUserDictAdd(schemaID string) // 用户词库新增条目
}

// BridgeServer interface for pushing state/config/composition events to TSF clients.
// 所有 push 仅触达 active client——背景 TSF 实例处理不到按键，状态/配置缓存
// 用不到；下次焦点切换到它们时由 HandleFocusGained 响应或随后的 push 补齐。
type BridgeServer interface {
	PushStateToActiveClient(status *bridge.StatusUpdateData)
	PushCommitTextToActiveClient(text string)                      // Mouse-click candidate commit text
	PushClearCompositionToActiveClient()                           // Clear inline composition
	PushUpdateCompositionToActiveClient(text string, caretPos int) // Mouse partial-confirm composition update
	PushEnglishPairConfigToActiveClient(enabled bool, pairs []string)
	PushStatsConfigToActiveClient(enabled bool, trackEnglish bool)
	RestartService()
	// GetActiveHostRender returns write/hide functions if the active process has host rendering.
	// Returns nil functions if host rendering is not active for the current process.
	GetActiveHostRender() (writeFrame func(img *image.RGBA, x, y int) error, hideFunc func())

	// IsActivelyFocusedPID 报告指定 PID 当下是否仍有任一 TSF clientID 持有可编辑焦点。
	// 用于 HandleIMEDeactivated / HandleFocusLost 区分两种场景：
	//   - 单实例销毁 (如 Notepad 关一个 tab): 同 PID 仍有兄弟实例 focused
	//     → 应跳过 IME 失活, 保留工具栏和输入状态
	//   - 真正失焦 / IME 切换: 同 PID 无其它 focused client → 正常 deactivate
	// 调用方传当前 activeProcessID. 0 → false. server.markUnfocused 必须先于
	// 本接口被调用（server_handler 已保证顺序）。
	IsActivelyFocusedPID(pid uint32) bool
}

// muWaitThreshold 是 c.mu 等锁时长的诊断警戒线。
// 超过即写 WARN，便于定位 coordinator 跨 client 锁竞争。
// 5ms 阈值的依据：健康路径下 c.mu 应该几乎无等待 (μs 级)，5ms 已是异常的下限。
const muWaitThreshold = 5 * time.Millisecond

// muLockTraceWait 给 c.mu.Lock() 加 wait 时长仪表化。在热点入口替换裸 c.mu.Lock()
// 即可暴露「谁等谁久」。caller 字符串作为日志关键字, 用于定位等锁的入口方法。
//
// 不测 hold time: coordinator 多数方法都是 Lock() + defer Unlock() 全段持锁,
// 持锁时长可由调用方 (如 bridge 的 slowRequestThreshold WARN) 间接推断;
// wait time 才是判断「锁是否成为瓶颈」的关键信号。
//
// 调用约定 (替代 c.mu.Lock()):
//
//	c.muLockTraceWait("HandleKeyEvent")
//	defer c.mu.Unlock()
func (c *Coordinator) muLockTraceWait(caller string) {
	t0 := time.Now()
	c.mu.Lock()
	if waited := time.Since(t0); waited > muWaitThreshold {
		c.logger.Warn("coordinator.mu wait", "caller", caller, "duration", waited)
	}
}

// SetBridgeServer sets the bridge server for state broadcasting
func (c *Coordinator) SetBridgeServer(server BridgeServer) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.bridgeServer = server
}

// SetVersion sets the app version for display in the menu
func (c *Coordinator) SetVersion(v string) {
	c.version = v
}

// SetEventNotifier 注入事件通知器（典型由 main.go 在 rpc.Server 创建后调用）
func (c *Coordinator) SetEventNotifier(n EventNotifier) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.eventNotifier = n
}

// GetEffectiveMode returns the effective input mode considering CapsLock
// - Chinese mode + CapsLock OFF = Chinese
// - Chinese mode + CapsLock ON = English Upper (temporary English for caps)
// - English mode + CapsLock OFF = English Lower
// - English mode + CapsLock ON = English Upper
func (c *Coordinator) GetEffectiveMode() EffectiveMode {
	if c.capsLockOn {
		return ModeEnglishUpper
	}
	if c.chineseMode {
		return ModeChinese
	}
	return ModeEnglishLower
}

// GetEffectiveModeNoLock returns the effective input mode without acquiring lock
// Caller must hold the lock
func (c *Coordinator) getEffectiveModeNoLock() EffectiveMode {
	if c.capsLockOn {
		return ModeEnglishUpper
	}
	if c.chineseMode {
		return ModeChinese
	}
	return ModeEnglishLower
}

// isEffectiveChinesePunct 返回当前是否应使用中文标点（考虑 CapsLock 等模式影响）
// CapsLock 开启时视为英文模式，不使用中文标点。调用者必须持有锁。
func (c *Coordinator) isEffectiveChinesePunct() bool {
	return c.chinesePunctuation && c.getEffectiveModeNoLock() == ModeChinese
}

// IsCapsLockOn returns the current CapsLock state
func (c *Coordinator) IsCapsLockOn() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.capsLockOn
}

// getIconLabelNoLock computes the taskbar icon label based on current state (caller must hold lock)
// This determines what character is displayed in the Windows taskbar input indicator
// Chinese mode uses the schema's icon_label (e.g., "拼", "五", "双", "混")
func (c *Coordinator) getIconLabelNoLock() string {
	effectiveChinese := c.chineseMode && !c.capsLockOn
	if effectiveChinese {
		if c.engineMgr != nil {
			_, iconLabel := c.engineMgr.GetSchemaDisplayInfo()
			if iconLabel != "" {
				return iconLabel
			}
		}
		return "中"
	}
	if c.capsLockOn {
		return "A"
	}
	return "英"
}

// BuildCurrentStatus returns the current status for external callers (e.g., initial state push).
func (c *Coordinator) BuildCurrentStatus() *bridge.StatusUpdateData {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.buildStatusUpdate()
}

// buildStatusUpdate creates a StatusUpdateData from current state (caller must hold lock)
func (c *Coordinator) buildStatusUpdate() *bridge.StatusUpdateData {
	keyDownHotkeys, keyUpHotkeys := c.getCompiledHotkeys()
	return &bridge.StatusUpdateData{
		ChineseMode:        c.chineseMode,
		FullWidth:          c.fullWidth,
		ChinesePunctuation: c.chinesePunctuation,
		ToolbarVisible:     c.toolbarVisible,
		CapsLock:           c.capsLockOn,
		IconLabel:          c.getIconLabelNoLock(),
		KeyDownHotkeys:     keyDownHotkeys,
		KeyUpHotkeys:       keyUpHotkeys,
	}
}

// NotifySchemaActivated 由外部（如 main 的异步资源就绪回调）在切换/激活方案后调用。
// 它会同步工具栏 + Push 状态到所有 TSF 客户端，并在屏幕右下角弹出一次 toast 通知，
// 让用户在等待 wdat 等异步资源就绪后明确感知"现在可以正常输入了"。
// displayName 为空时只 broadcast、不弹通知。
func (c *Coordinator) NotifySchemaActivated(displayName string) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.broadcastState()

	uiReady := c.uiManager != nil && c.uiManager.IsReady()
	c.logger.Info("NotifySchemaActivated",
		"displayName", displayName,
		"uiReady", uiReady,
	)

	if displayName == "" || !uiReady {
		return
	}
	c.uiManager.ShowToastSuccess(displayName + " 词库加载完成。")
}

// broadcastState broadcasts the current state to toolbar and all TSF clients
// This should be called after any state change. Caller must hold the lock.
func (c *Coordinator) broadcastState() {
	// 1. Update Go toolbar
	c.syncToolbarStateNoLock()

	// 2. Push state to all TSF clients (async to avoid blocking on pipe writes)
	if c.bridgeServer != nil {
		status := c.buildStatusUpdate()
		server := c.bridgeServer
		go server.PushStateToActiveClient(status)
	}
}

// buildToolbarState creates a ToolbarState from current coordinator state (caller must hold lock)
func (c *Coordinator) buildToolbarState() ui.ToolbarState {
	effectiveMode := c.getEffectiveModeNoLock()

	// Get icon_label from current schema for toolbar display
	var modeLabel string
	if c.engineMgr != nil {
		_, modeLabel = c.engineMgr.GetSchemaDisplayInfo()
	}

	return ui.ToolbarState{
		ChineseMode:   effectiveMode == ModeChinese,
		FullWidth:     c.fullWidth,
		ChinesePunct:  c.isEffectiveChinesePunct(),
		CapsLock:      c.capsLockOn,
		EffectiveMode: int(effectiveMode),
		ModeLabel:     modeLabel,
	}
}

// syncToolbarStateNoLock synchronizes the current state to the toolbar (without lock)
func (c *Coordinator) syncToolbarStateNoLock() {
	if c.uiManager == nil {
		return
	}
	c.uiManager.UpdateToolbarState(c.buildToolbarState())
}

// NewCoordinator creates a new Coordinator
func NewCoordinator(engineMgr *engine.Manager, uiManager *ui.Manager, cfg *config.Config, appCompat *config.AppCompat, logger *slog.Logger) *Coordinator {
	candidatesPerPageBase := 9
	if cfg != nil && cfg.UI.CandidatesPerPage > 0 {
		candidatesPerPageBase = cfg.UI.CandidatesPerPage
	}
	candidatesPerPageExtended := 0
	if cfg != nil {
		candidatesPerPageExtended = cfg.UI.CandidatesPerPageExtended
	}

	// 确定初始状态
	startInChineseMode := true
	fullWidth := false
	chinesePunctuation := true
	punctFollowMode := false
	toolbarVisible := false

	// 始终加载 RuntimeState，工具栏位置无条件恢复；
	// 输入模式字段（ChineseMode 等）仅在 remember_last_state=true 时生效。
	var toolbarUserPos map[string]image.Point
	runtimeState, runtimeStateErr := config.LoadRuntimeState()
	if runtimeStateErr != nil {
		logger.Warn("Failed to load runtime state, using defaults", "error", runtimeStateErr)
	} else if len(runtimeState.ToolbarPositions) > 0 {
		toolbarUserPos = make(map[string]image.Point, len(runtimeState.ToolbarPositions))
		for k, pos := range runtimeState.ToolbarPositions {
			toolbarUserPos[k] = image.Point{X: pos[0], Y: pos[1]}
		}
	}

	// 加载「固定候选位置」记忆（与 toolbar 同样无条件持久化）
	var candidatePinPositions map[string]map[string][2]int
	if runtimeStateErr == nil && len(runtimeState.CandidatePinPositions) > 0 {
		candidatePinPositions = make(map[string]map[string][2]int, len(runtimeState.CandidatePinPositions))
		for proc, byMonitor := range runtimeState.CandidatePinPositions {
			inner := make(map[string][2]int, len(byMonitor))
			for k, v := range byMonitor {
				inner[k] = v
			}
			candidatePinPositions[proc] = inner
		}
	}

	if cfg != nil {
		if cfg.Startup.RememberLastState && runtimeStateErr == nil {
			startInChineseMode = runtimeState.ChineseMode
			fullWidth = runtimeState.FullWidth
			chinesePunctuation = runtimeState.ChinesePunct
		} else {
			// 使用默认配置
			startInChineseMode = cfg.Startup.DefaultChineseMode
			fullWidth = cfg.Startup.DefaultFullWidth
			chinesePunctuation = cfg.Startup.DefaultChinesePunct
		}

		punctFollowMode = cfg.Input.PunctFollowMode
		toolbarVisible = cfg.Toolbar.Visible
	}

	c := &Coordinator{
		engineMgr:                 engineMgr,
		uiManager:                 uiManager,
		logger:                    logger,
		config:                    cfg,
		chineseMode:               startInChineseMode,
		fullWidth:                 fullWidth,
		chinesePunctuation:        chinesePunctuation,
		punctFollowMode:           punctFollowMode,
		toolbarVisible:            toolbarVisible,
		inputBuffer:               "",
		candidates:                nil,
		currentPage:               1,
		totalPages:                1,
		candidatesPerPage:         candidatesPerPageBase, // 初始物化为基础档；首次查询时由 refreshEffectivePerPage 重算
		candidatesPerPageBase:     candidatesPerPageBase,
		candidatesPerPageExtended: candidatesPerPageExtended,
		caretState: caretState{
			caretX:      100,
			caretY:      100,
			caretHeight: 20,
		},
		punctConverter:        transform.NewPunctuationConverter(),
		pairTracker:           transform.NewPairTracker(cfg.Input.AutoPair.ChinesePairs),
		pairTrackerEn:         transform.NewPairTracker(cfg.Input.AutoPair.EnglishPairs),
		hotkeyCompiler:        hotkey.NewCompiler(cfg),
		hotkeysDirty:          true, // 首次使用时需要编译
		s2tManager:            newS2TManager(logger),
		inputHistory:          NewInputHistory(20),
		appCompat:             appCompat,
		cfgMu:                 new(sync.RWMutex),
		toolbarUserPos:        toolbarUserPos,
		candidatePinPositions: candidatePinPositions,
	}

	// 根据配对表设置引号配对状态
	c.updatePairedQuotes(cfg.Input.AutoPair.ChinesePairs)

	// 加载自定义标点映射
	c.punctConverter.SetCustomMappings(cfg.Input.PunctCustom.Enabled, cfg.Input.PunctCustom.Mappings)

	// 初始化简入繁出（按需加载，未启用时不读盘）
	c.reconfigureS2T(cfg.S2T)

	// Set up toolbar callbacks
	c.setupToolbarCallbacks()

	// Set up candidate window callbacks for mouse interaction
	c.setupCandidateCallbacks()

	// 设置状态窗口右键菜单回调
	c.setupStatusWindowCallbacks()

	// Set up global hotkey callbacks (RegisterHotKey for combination hotkeys)
	c.setupGlobalHotkeyCallbacks()

	// Initialize UI config (including debug options)
	if c.uiManager != nil && cfg != nil {
		fontSpec := cfg.UI.FontFamily
		if fontSpec == "" {
			fontSpec = cfg.UI.FontPath
		}
		c.uiManager.UpdateConfig(cfg.UI.FontSize, cfg.UI.FontSizeFollowTheme, fontSpec, cfg.UI.HideCandidateWindow)
		// 主题 behavior 用户覆盖层（哲学Y；与 UpdateUIConfig 热更新对称，否则启动后失效直到下次热更新）
		c.uiManager.SetBehaviorOverrides(
			cfg.UI.AlwaysShowPager, cfg.UI.AlwaysShowPagerFollowTheme,
			cfg.UI.ShowPageNumber, cfg.UI.ShowPageNumberFollowTheme,
			cfg.UI.VerticalMaxWidth, cfg.UI.VerticalMaxWidthFollowTheme,
		)
		// Set candidate layout (horizontal/vertical)
		if cfg.UI.CandidateLayout != "" {
			c.uiManager.SetCandidateLayout(cfg.UI.CandidateLayout)
		}
		// Set hide preedit when inline preedit is enabled
		c.uiManager.SetHidePreedit(cfg.UI.InlinePreedit)
		// 用户全局序号标签覆盖
		c.uiManager.SetCandidateIndexLabels(cfg.UI.CandidateIndexLabels)
		// Set preedit display mode
		c.uiManager.SetPreeditMode(cfg.UI.PreeditMode)
		// 候选窗在光标上方时反转 bands（与 UpdateUIConfig 热更新对称；启动时遗漏则需热更新才能生效）
		c.uiManager.SetFlipLayoutWhenAbove(cfg.UI.FlipLayoutWhenAbove)
		// 翻页器显示方式覆盖（与 UpdateUIConfig 热更新对称；此处遗漏会导致用户的
		// never/always 设置在启动后失效，直到下一次配置热更新才生效）
		c.uiManager.SetPagerBarDisplay(cfg.UI.PagerBarDisplay)
		c.uiManager.SetPageNumberDisplay(cfg.UI.PageNumberDisplay)
		// Set status indicator config (旧字段兼容)
		c.uiManager.UpdateStatusIndicatorConfig(
			cfg.UI.StatusIndicatorDuration,
			cfg.UI.StatusIndicatorOffsetX,
			cfg.UI.StatusIndicatorOffsetY,
		)
		// 初始化完整状态提示配置
		siCfg := cfg.UI.StatusIndicator
		c.uiManager.UpdateStatusIndicatorFullConfig(ui.StatusWindowConfig{
			Enabled:         siCfg.Enabled,
			DisplayMode:     ui.StatusDisplayMode(siCfg.DisplayMode),
			Duration:        siCfg.Duration,
			SchemaNameStyle: siCfg.SchemaNameStyle,
			ShowMode:        siCfg.ShowMode,
			ShowPunct:       siCfg.ShowPunct,
			ShowFullWidth:   siCfg.ShowFullWidth,
			PositionMode:    ui.StatusPositionMode(siCfg.PositionMode),
			OffsetX:         siCfg.OffsetX,
			OffsetY:         siCfg.OffsetY,
			CustomX:         siCfg.CustomX,
			CustomY:         siCfg.CustomY,
			FontSize:        siCfg.FontSize,
			Opacity:         siCfg.Opacity,
			BackgroundColor: siCfg.BackgroundColor,
			TextColor:       siCfg.TextColor,
			BorderRadius:    siCfg.BorderRadius,
		})
		// 设置编码提示延迟
		c.uiManager.SetTooltipDelay(cfg.UI.TooltipDelay)
		// 设置文本渲染模式
		if cfg.UI.TextRenderMode != "" {
			c.uiManager.SetTextRenderMode(cfg.UI.TextRenderMode)
		}
		// 设置候选框GDI字体参数
		if cfg.UI.GDIFontWeight > 0 || cfg.UI.GDIFontScale > 0 {
			c.uiManager.SetGDIFontParams(cfg.UI.GDIFontWeight, cfg.UI.GDIFontScale)
		}
		// 设置菜单GDI字体参数（独立于候选框）
		if cfg.UI.MenuFontWeight > 0 {
			c.uiManager.SetMenuFontParams(cfg.UI.MenuFontWeight, cfg.UI.GDIFontScale)
		}
		// 设置菜单字体大小
		if cfg.UI.MenuFontSize > 0 {
			c.uiManager.SetMenuFontSize(cfg.UI.MenuFontSize)
		}
		// 设置候选文本最大显示字符数
		c.uiManager.SetMaxCandidateChars(cfg.UI.MaxCandidateChars)
		// 设置副作用 cmdbar 候选的渲染前缀
		c.uiManager.SetCmdbarCandidatePrefix(cfg.UI.GetCmdbarCandidatePrefix())
		// 初始化主题暗色模式并加载主题
		c.initThemeMode(cfg)
	}

	// 初始化 tooltip service（含拆字数据库路径和字体）
	c.rebuildTooltipServiceLocked()

	c.startGoroutineWatchdog()

	// 初始化命令直通车 (cmdbar): Services 装配 + 动作函数注册 + 短语 hook 注入。
	// last() 复用 c.inputHistory, 不再单独维护 cmdbar 历史缓冲。
	// 见 docs/design/command-bar-design.md §7。
	c.cmdbarServices = c.buildCmdbarServices()
	// RegisterActions 是幂等的: 覆盖 DefaultRegistry 中的 stubs 为真实实现。
	cmdbarfuncs.RegisterActions(cmdbar.DefaultRegistry)
	c.installCmdbarPhraseHook()

	// 工具栏可见性 reducer：所有显隐决策的单一裁判，详见 toolbar_reducer.go。
	// 必须在 setupToolbarCallbacks 之后启动也仍然安全 —— callback 到达时 reducer 已就绪。
	c.toolbarReducer = newToolbarReducer(c)

	// 特殊模式注册表（引导键自定义码表）
	c.specialModeReg = newSpecialModeRegistry(c.config.Input.SpecialModes, c.schemasDirs(), c.logger)

	return c
}

// installCmdbarPhraseHook 把命令直通车 hook 注入到当前 schema 的 PhraseLayer。
// 短语 value 含 "$CC(" 时由该 hook 解析并返回 (display, actions) 给候选构造,
// 不含时仍走旧的 templateEngine 路径 (双路径策略, design §7.2)。
//
// 解析或求值出错时返回 err 让 PhraseLayer 退化为字面量短语并记 WARN, 不阻断输入。
func (c *Coordinator) installCmdbarPhraseHook() {
	if c.engineMgr == nil {
		return
	}
	dm := c.engineMgr.GetDictManager()
	if dm == nil {
		return
	}
	pl := dm.GetPhraseLayer()
	if pl == nil {
		return
	}
	hook := func(value string) (string, []cmdbar.ResolvedAction, map[string]any, bool, error) {
		phrase, err := cmdbarparser.Parse(value)
		if err != nil {
			return "", nil, nil, true, err
		}
		// hook 是从 phrase.SearchCommand 调用的, 而 SearchCommand 在
		// HandleKeyEvent → handleAlphaKey → convert 链路中, c.mu 已被
		// 调用方持有。**禁止**在此处再次 c.mu.Lock(), 否则 sync.Mutex
		// 自死锁, 表现为输入完全卡死 (但 UI 线程因不走此锁仍可拖动)。
		//
		// 关键: 把 inputBuffer 拷贝到 evalCtx.input 作为**快照**, 让
		// action thunks 异步触发时 (此时 c.inputBuffer 已被 clearState
		// 清空) 仍能拿到触发候选时的编码。否则 code()/tail(code,n) 在
		// 异步执行点永远返回空。
		evalCtx := c.newCmdbarEvalContextLocked(c.inputBuffer)
		display, actions, err := cmdbareval.Evaluate(phrase, evalCtx, cmdbar.DefaultRegistry)
		if err != nil {
			return "", nil, nil, true, err
		}
		// 从 AST 取 modifiers (CommandPhrase 才有), 透传给 candidate.Modifiers,
		// 替代 dict 层旧的 IsExactOnly 字符串扫描。LiteralPhrase / TemplatePhrase
		// 没有 modifiers, 返回 nil 即可。
		var modifiers map[string]any
		if cp, ok := phrase.(cmdbarast.CommandPhrase); ok {
			modifiers = cp.Modifiers
		}
		return display, actions, modifiers, true, nil
	}
	pl.SetCmdbarHook(dict.CmdbarPhraseHook(hook))

	// 装配 ValueExpander, 给候选后处理使用。hook 与 PhraseLayer 共享同一闭包。
	// ArrayHook 在下方 arrayHook 构造完成后再赋值。
	c.cmdbarValueExpander = &dict.ValueExpander{
		Hook:           dict.CmdbarPhraseHook(hook),
		TemplateEngine: dict.GetTemplateEngine(),
	}

	// 装配 $SS 数组 hook。与 phrase hook 同样持有 c.mu 已被调用方 (SearchCommand)
	// 持有的假设, 不能再加锁。元素中嵌入的 $CC 也通过同一 evalCtx 求值。
	arrayHook := func(value string) (string, []dict.CmdbarArrayElement, map[string]any, bool, error) {
		phrase, err := cmdbarparser.Parse(value)
		if err != nil {
			return "", nil, nil, true, err
		}
		ap, ok := phrase.(cmdbarast.ArrayPhrase)
		if !ok {
			return "", nil, nil, false, nil
		}
		evalCtx := c.newCmdbarEvalContextLocked(c.inputBuffer)
		name, evalElements, groupModifiers, err := cmdbareval.ExpandArray(ap, evalCtx, cmdbar.DefaultRegistry)
		if err != nil {
			return "", nil, nil, true, err
		}
		out := make([]dict.CmdbarArrayElement, 0, len(evalElements))
		for _, e := range evalElements {
			out = append(out, dict.CmdbarArrayElement{
				Display:          e.Display,
				Actions:          e.Actions,
				ElementModifiers: e.ElementModifiers,
			})
		}
		return name, out, groupModifiers, true, nil
	}
	pl.SetCmdbarArrayHook(dict.CmdbarArrayHook(arrayHook))
	// 同步给 ValueExpander 装配 ArrayHook，使特殊码表候选的 $SS 展开可用。
	if c.cmdbarValueExpander != nil {
		c.cmdbarValueExpander.ArrayHook = dict.CmdbarArrayHook(arrayHook)
	}
}

// initThemeMode initializes the dark mode state and starts the system theme watcher if needed
func (c *Coordinator) initThemeMode(cfg *config.Config) {
	if c.uiManager == nil {
		return
	}

	themeStyle := cfg.UI.ThemeStyle
	if themeStyle == "" {
		themeStyle = config.ThemeStyleSystem
	}

	// Determine initial dark mode state
	isDark := false
	switch themeStyle {
	case config.ThemeStyleDark:
		isDark = true
	case config.ThemeStyleLight:
		isDark = false
	default: // system
		isDark = theme.IsSystemDarkMode()
	}

	// Set dark mode on the theme manager before loading the theme
	c.uiManager.SetDarkMode(isDark)

	// Load the theme
	themeName := cfg.UI.Theme
	if themeName == "" {
		themeName = "default"
	}
	c.uiManager.LoadTheme(themeName)
	c.notifyThemeFallbackIfAny()

	// Start system theme watcher if following system mode
	if themeStyle == config.ThemeStyleSystem {
		c.startDarkModeWatcher()
	}
}

// startDarkModeWatcher starts watching for system dark mode changes
func (c *Coordinator) startDarkModeWatcher() {
	// Stop existing watcher if any
	if c.darkModeWatcher != nil {
		c.darkModeWatcher.Stop()
	}

	c.darkModeWatcher = theme.NewDarkModeWatcher(c.logger, func(isDark bool) {
		// Called on system theme change — re-resolve and apply the theme
		if c.uiManager != nil {
			c.uiManager.SetDarkMode(isDark)
			c.uiManager.ReapplyTheme()
		}
	})
	c.darkModeWatcher.Start()
}

// stopDarkModeWatcher stops the system dark mode watcher
func (c *Coordinator) stopDarkModeWatcher() {
	if c.darkModeWatcher != nil {
		c.darkModeWatcher.Stop()
		c.darkModeWatcher = nil
	}
}

// hasPendingInput 检查是否有任何类型的待处理输入或活跃的临时模式。
// 临时模式激活即意味着有待清理的 UI 状态（即使缓冲区为空）。
func (c *Coordinator) hasPendingInput() bool {
	return len(c.inputBuffer) > 0 || len(c.confirmedSegments) > 0 ||
		c.tempEnglishMode || len(c.tempEnglishBuffer) > 0 ||
		c.tempPinyinMode || len(c.tempPinyinBuffer) > 0 ||
		c.quickInputMode || c.specialMode
}

// getPendingBufferText 获取当前待处理缓冲区的文本（用于 CommitOnSwitch 上屏）
// 优先级：主输入缓冲（含确认段）> 临时英文缓冲 > 临时拼音缓冲
func (c *Coordinator) getPendingBufferText() string {
	// 如果有确认段，拼接确认文本 + 剩余编码
	if len(c.confirmedSegments) > 0 || len(c.inputBuffer) > 0 {
		var text string
		for _, seg := range c.confirmedSegments {
			text += seg.Text
		}
		text += c.inputBuffer
		if c.fullWidth {
			return transform.ToFullWidth(text)
		}
		return text
	}

	var text string
	switch {
	case len(c.tempEnglishBuffer) > 0:
		text = c.tempEnglishBuffer
	case len(c.tempPinyinBuffer) > 0:
		text = c.tempPinyinBuffer
		if c.tempPinyinTriggerKey == "z" && c.config != nil && c.config.Input.TempPinyin.ZIncludeOnCommitEnabled() {
			text = "z" + text
		}
	case c.quickInputMode && len(c.quickInputPinyinBuffer) > 0:
		text = c.quickInputPinyinBuffer
	case c.quickInputMode && len(c.quickInputBuffer) > 0:
		text = c.quickInputBuffer
	case c.specialMode:
		text = c.specialBuffer
	default:
		return ""
	}
	if c.fullWidth {
		return transform.ToFullWidth(text)
	}
	return text
}

func (c *Coordinator) clearState() {
	c.inputBuffer = ""
	c.inputCursorPos = 0
	c.preeditDisplay = ""
	c.syllableBoundaries = nil
	c.confirmedSegments = nil
	// 输入流重置, 用户主动展开 collapse 组的标记必须清零, 否则下次复用旧 group code
	// 时会跳过 collapse, 行为不一致。
	c.expandedGroupTemplate = ""
	c.tempEnglishMode = false
	c.tempEnglishBuffer = ""
	// 清除临时拼音状态时，同步卸载引擎层的拼音词库层，避免污染五笔查询
	if c.tempPinyinMode && c.engineMgr != nil {
		c.engineMgr.DeactivateTempPinyin()
	}
	c.tempPinyinMode = false
	c.tempPinyinBuffer = ""
	c.tempPinyinCommitted = ""
	c.tempPinyinRewindBuffer = ""
	c.tempPinyinRewindKey = ""
	c.candidates = nil
	c.currentPage = 1
	c.totalPages = 1
	c.selectedIndex = 0
	c.compositionStartValid = false
	// 清理加词模式状态
	c.addWordActive = false
	c.addWordChars = nil
	c.addWordLen = 0
	c.addWordCode = ""
	// 清理快捷输入模式状态（恢复布局需在重置标志前执行）
	if c.quickInputMode {
		// 如果处于快捷输入的临时拼音子模式且已交换词库层，先恢复
		if c.quickInputPinyinDictSwapped && c.engineMgr != nil {
			c.engineMgr.DeactivateTempPinyin()
		}
		if c.savedLayout != "" && c.uiManager != nil {
			c.uiManager.SetCandidateLayout(c.savedLayout)
		}
		if c.uiManager != nil {
			c.uiManager.SetQuickInputMode(false)
		}
	}
	if c.uiManager != nil {
		c.uiManager.SetModeLabel("")
		c.uiManager.SetModeAccentColor(nil)
	}
	c.quickInputMode = false
	c.quickInputBuffer = ""
	c.quickInputPinyinMode = false
	c.quickInputPinyinBuffer = ""
	c.quickInputPinyinCommitted = ""
	c.quickInputPinyinDictSwapped = false
	c.savedLayout = ""

	// 清理特殊模式状态（恢复布局需在重置标志前执行）
	if c.specialMode {
		if c.specialSavedLayout != "" && c.uiManager != nil {
			c.uiManager.SetCandidateLayout(c.specialSavedLayout)
		}
		if c.uiManager != nil {
			c.uiManager.SetModeLabel("")
			c.uiManager.SetModeAccentColor(nil)
		}
	}
	c.specialMode = false
	c.specialActiveID = ""
	c.specialTriggerKey = ""
	c.specialBuffer = ""
	c.specialSavedLayout = ""

	// 注意：不清除 activeProcessID，需要跨 composition 持久化

	// 清空配对栈（输入状态重置意味着光标位置不再可预测）
	if c.pairTracker != nil {
		c.pairTracker.Clear()
	}
	if c.pairTrackerEn != nil {
		c.pairTrackerEn.Clear()
	}

	// 清除命令结果缓存，确保 uuid/date/time 等下次生成新值
	c.engineMgr.InvalidateCommandCache()
}

// modeAccentColor 返回指定模式的内发光颜色，优先读配置，空则用内置默认色。
// modeName: "temp_pinyin" | "quick_input"
// 默认关闭；需在配置中将 mode_accent_border 设为 true 才启用。
func (c *Coordinator) modeAccentColor(modeName string) color.Color {
	if c.config == nil || c.config.UI.ModeAccentBorder == nil || !*c.config.UI.ModeAccentBorder {
		return nil
	}
	var hexStr string
	var def color.RGBA
	switch modeName {
	case "temp_pinyin":
		if c.config != nil {
			hexStr = c.config.UI.TempPinyinAccentColor
		}
		def = color.RGBA{66, 165, 245, 255}
	case "quick_input":
		if c.config != nil {
			hexStr = c.config.UI.QuickInputAccentColor
		}
		def = color.RGBA{102, 187, 106, 255}
	default:
		return nil
	}
	if parsed, ok := parseHexColor(hexStr); ok {
		return parsed
	}
	return def
}

// parseHexColor 解析 "#RRGGBB" 或 "#RRGGBBAA" 十六进制颜色字符串。
func parseHexColor(s string) (color.RGBA, bool) {
	s = strings.TrimPrefix(s, "#")
	if len(s) != 6 && len(s) != 8 {
		return color.RGBA{}, false
	}
	b, err := hex.DecodeString(s)
	if err != nil {
		return color.RGBA{}, false
	}
	if len(b) == 3 {
		return color.RGBA{R: b[0], G: b[1], B: b[2], A: 255}, true
	}
	return color.RGBA{R: b[0], G: b[1], B: b[2], A: b[3]}, true
}

// resetCompositionAnchorAfterCommit 在"部分上屏但保留剩余编码"等场景下被调用：
// 上屏会让 C++ 端结束当前 composition 并立即开启新 composition，旧的 compositionStart
// 锁定不应继续生效，否则候选窗会停留在前一段 composition 的位置。
// C++ 端会在新 composition 创建时通过 _compositionJustStarted 推迟首次 IPC 至
// OnLayoutChange，因此 Go 端无需再维护"等待坐标"的状态。
// 调用方必须持有 c.mu 锁。
func (c *Coordinator) resetCompositionAnchorAfterCommit() {
	c.compositionStartValid = false
}
