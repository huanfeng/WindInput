//go:build windows

// Package bridge handles IPC communication with C++ TSF Bridge
package bridge

import (
	"errors"
	"fmt"
	"image"
	"io"
	"log/slog"
	"net"
	"sync"
	"time"
	"unsafe"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/internal/ipc"
	"golang.org/x/sys/windows"
)

// isPipeClosed 判断 err 是否为对端正常关闭命名管道时的预期错误。
// 这些错误在 TSF 宿主（Chrome/WPS/Excel 等）退出或切换 IME 时频繁出现，
// 不应记为 ERROR 级别——会污染日志、淹没真正的异常。
func isPipeClosed(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) {
		return true
	}
	return errors.Is(err, windows.ERROR_BROKEN_PIPE) ||
		errors.Is(err, windows.ERROR_NO_DATA) ||
		errors.Is(err, windows.ERROR_PIPE_NOT_CONNECTED)
}

var (
	kernel32                        = windows.NewLazySystemDLL("kernel32.dll")
	procGetNamedPipeClientProcessId = kernel32.NewProc("GetNamedPipeClientProcessId")
)

// getNamedPipeClientProcessId returns the process ID of the client connected to the named pipe
func getNamedPipeClientProcessId(handle windows.Handle) (uint32, error) {
	var processID uint32
	ret, _, err := procGetNamedPipeClientProcessId.Call(
		uintptr(handle),
		uintptr(unsafe.Pointer(&processID)),
	)
	if ret == 0 {
		return 0, err
	}
	return processID, nil
}

// BridgePipeName / PushPipeName 已迁至 endpoint_windows.go (Win) /
// endpoint_darwin.go (darwin), 让两个平台用同一变量名指向不同形态的端点。

const (
	// Buffer size for named pipe (64KB like Weasel)
	PipeBufferSize = 64 * 1024

	// Timeout for processing a single request.
	// 慢路径（菜单、模式切换等）在 CPU 高负载时调度延迟可达数百毫秒，
	// 1000ms 既能覆盖正常抖动，又能在真实死锁时快速暴露。
	RequestProcessTimeout = 1000 * time.Millisecond
)

// Server handles IPC communication with C++ TSF Bridge
type Server struct {
	logger  *slog.Logger
	handler MessageHandler
	codec   *ipc.BinaryCodec

	mu          sync.RWMutex
	clientCount int
	// activeConns 跟踪当前活跃的 bridge pipe 连接（请求-响应通道）。
	// 仅作为"集合 + 计数"使用——RestartService 时遍历 Close。
	activeConns map[net.Conn]struct{}

	// Push pipe clients (for proactive state push)
	pushMu           sync.RWMutex
	pushClientCount  int
	pushClients      map[windows.Handle]*pushClient
	pushClientsByPID map[uint32]windows.Handle // PID → 最新 push handle（同 PID 多实例时的兜底）
	pushHandleToPID  map[windows.Handle]uint32 // 反向映射：handle → PID

	// Push pipe client token tracking (per-instance precise targeting)
	// C++ 每个 CIPCClient 实例在连接 push pipe 时写入一个进程内唯一 token，
	// 同时在 CMD_IME_ACTIVATED / CMD_FOCUS_GAINED 中携带该 token。
	// 通过 token 可精确定位多实例宿主（如 explorer）中持有活跃 composition 的那个实例。
	// Token 采用 64 位避免 Windows PID 超过 16 位时与 instance counter 编码冲突。
	tokenToPushHandle map[uint64]windows.Handle // client token → push handle
	pushHandleToToken map[windows.Handle]uint64 // push handle → client token

	// Active client tracking (for secure, targeted push)
	activeMu        sync.RWMutex
	activeProcessID uint32 // Process ID of the client that has focus
	activeToken     uint64 // Per-instance token of the active TextService (0 if unknown)

	// focusedClients 记录"当下持有可编辑 TSF 焦点"的客户端（bridge clientID → PID）。
	// 由 CmdFocusGained / CmdIMEActivated 写入，由 CmdFocusLost / CmdIMEDeactivated
	// / bridge 连接整体断开删除。
	//
	// 为什么是 per-clientID 而不是 per-PID：同一进程可能持有多个 TextService 实例
	// （典型如 Notepad11 多 tab、Explorer 多 XamlIsland），每个实例对应独立的 bridge
	// 连接 & clientID。如果只按 PID 维护集合，关闭其中一个实例时的 IME_DEACTIVATED
	// 会把整个 PID 从集合摘掉，连带把仍在前台的另一个实例的"有焦点"状态也擦掉，
	// 导致工具栏在 tab 关闭后误隐藏。
	//
	// PID 是否"有焦点"的判定 → 遍历 map 查是否有任意 value 等于该 PID。
	// 该集合**仅供前台 hook 门槛使用**，不参与 push 路由 / activeProcessID 等流程。
	focusMu        sync.RWMutex
	focusedClients map[int]uint32

	// Host render manager (for Band window proxy rendering)
	hostRender *HostRenderManager
}

// NewServer creates a new Bridge IPC server
func NewServer(handler MessageHandler, logger *slog.Logger) *Server {
	return &Server{
		handler:           handler,
		logger:            logger,
		codec:             ipc.NewBinaryCodec(),
		activeConns:       make(map[net.Conn]struct{}),
		pushClients:       make(map[windows.Handle]*pushClient),
		pushClientsByPID:  make(map[uint32]windows.Handle),
		pushHandleToPID:   make(map[windows.Handle]uint32),
		tokenToPushHandle: make(map[uint64]windows.Handle),
		pushHandleToToken: make(map[windows.Handle]uint64),
		focusedClients:    make(map[int]uint32),
	}
}

// SetHostRenderManager sets the host render manager for Band window proxy rendering.
func (s *Server) SetHostRenderManager(hrm *HostRenderManager) {
	s.hostRender = hrm
}

// GetHostRenderManager returns the host render manager.
func (s *Server) GetHostRenderManager() *HostRenderManager {
	return s.hostRender
}

// IsActivelyFocusedPID 返回该 PID 当下是否有任意 TextService 实例持有可编辑的
// TSF 焦点（即对应的 bridge clientID 已发 CmdFocusGained 或 CmdIMEActivated 且
// 尚未对应地丢失）。比"是不是连接过的 push 客户端"严格：explorer.exe 因为承载
// 开始菜单搜索框等控件，全天候在 pushClientsByPID 里，但只有用户真正点进搜索
// 框时才有 clientID 在 focusedClients 里指向它。
//
// 用途：工具栏前台 hook 的激活门槛，避免用户点任务栏 / 桌面 / 通知区时
// 误以为 explorer 是"输入焦点"而显示工具栏。
func (s *Server) IsActivelyFocusedPID(pid uint32) bool {
	if pid == 0 {
		return false
	}
	s.focusMu.RLock()
	defer s.focusMu.RUnlock()
	for _, p := range s.focusedClients {
		if p == pid {
			return true
		}
	}
	return false
}

// markFocused 把 clientID→PID 写入 focusedClients。幂等：重复添加覆盖即可。
// 由 server_handler 在 CmdFocusGained / CmdIMEActivated 成功路径上调用，
// 必须传 clientID 才能精确撤销（同 PID 多实例时只摘掉本实例那一条）。
func (s *Server) markFocused(clientID int, pid uint32) {
	if pid == 0 {
		return
	}
	s.focusMu.Lock()
	s.focusedClients[clientID] = pid
	s.focusMu.Unlock()
}

// markUnfocused 仅摘除指定 clientID 的焦点记录，不会影响同 PID 其它 clientID。
// 由 server_handler 在 CmdFocusLost / CmdIMEDeactivated 路径上调用，以及
// handleClient 退出时（bridge 连接整体断开）调用兜底。
func (s *Server) markUnfocused(clientID int) {
	s.focusMu.Lock()
	delete(s.focusedClients, clientID)
	s.focusMu.Unlock()
}

// GetActiveHostRender returns write/hide functions if the active process has host rendering.
// Returns nil functions if host rendering is not active.
func (s *Server) GetActiveHostRender() (writeFrame func(img *image.RGBA, x, y int) error, hideFunc func()) {
	if s.hostRender == nil {
		return nil, nil
	}

	s.activeMu.RLock()
	pid := s.activeProcessID
	s.activeMu.RUnlock()

	if pid == 0 {
		return nil, nil
	}

	state := s.hostRender.GetActiveState(pid)
	if state == nil || state.SHM == nil {
		return nil, nil
	}

	shm := state.SHM
	return shm.WriteFrame, shm.WriteHide
}

// Start begins listening for connections from C++ Bridge.
//
// Bridge pipe（请求-响应 RPC 通道）也迁移到 go-winio overlapped I/O，统一架构。
// 与 push pipe 同样用 winio.ListenPipe + listener.Accept，conn 是 net.Conn，
// 读写走 codec.ReadHeader / WriteMessage（已经是 io.Reader/Writer 接口）。
func (s *Server) Start() error {
	s.logger.Info("Starting Bridge IPC server (binary protocol)", "pipe", BridgePipeName)

	// Start the push pipe listener in a separate goroutine
	go s.startPushPipeListener()

	// Allow desktop clients plus AppContainer/modern hosts (e.g. Start menu search).
	// S:(ML;;NW;;;LW) = Mandatory Label: Low integrity — required for UWP/AppContainer
	//   processes (Microsoft Store, Start Menu) which run at low integrity level.
	pipeConfig := &winio.PipeConfig{
		SecurityDescriptor: "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)S:(ML;;NW;;;LW)",
		MessageMode:        true,
		InputBufferSize:    int32(PipeBufferSize),
		OutputBufferSize:   int32(PipeBufferSize),
	}
	listener, err := winio.ListenPipe(BridgePipeName, pipeConfig)
	if err != nil {
		return fmt.Errorf("failed to listen bridge pipe: %w", err)
	}

	for {
		conn, err := listener.Accept()
		if err != nil {
			if errors.Is(err, net.ErrClosed) {
				s.logger.Info("Bridge pipe listener closed")
				return nil
			}
			s.logger.Error("Bridge pipe accept error", "error", err)
			time.Sleep(200 * time.Millisecond)
			continue
		}

		s.mu.Lock()
		s.clientCount++
		clientID := s.clientCount
		s.activeConns[conn] = struct{}{}
		s.mu.Unlock()

		s.logger.Info("C++ Bridge connected", "clientID", clientID)

		go func(c net.Conn, id int) {
			pid := s.handleClient(c, id)

			// Bridge 连接整体断开 → 摘掉该 clientID 在 focusedClients 的记录。
			// 兜底处理：万一 DLL 进程崩溃没来得及发 FOCUS_LOST / IME_DEACTIVATED，
			// 这里保证 focusedClients 不会留下死掉的 clientID。同 PID 其它存活
			// clientID 不受影响。
			s.markUnfocused(id)

			// Capture the current setup sequence BEFORE acquiring the main lock.
			// 防止旧连接的 cleanup goroutine 销毁同 PID 新连接的 SharedMemory。
			var setupSeq uint64
			if s.hostRender != nil && pid != 0 {
				setupSeq = s.hostRender.GetSetupSeq(pid)
			}

			s.mu.Lock()
			delete(s.activeConns, c)
			activeCount := len(s.activeConns)
			s.mu.Unlock()

			if s.hostRender != nil && pid != 0 && setupSeq != 0 {
				s.hostRender.CleanupClient(pid, setupSeq)
			}

			s.handler.HandleClientDisconnected(activeCount)
		}(conn, clientID)
	}
}

func (s *Server) handleClient(conn net.Conn, clientID int) uint32 {
	defer conn.Close()

	// Get the client's process ID for tracking active client.
	// winio 的 net.Conn 底层 win32File 暴露 Fd()——取出 handle 调用 GetNamedPipeClientProcessId。
	var processID uint32
	if g, ok := conn.(fdGetter); ok {
		var err error
		processID, err = getNamedPipeClientProcessId(windows.Handle(g.Fd()))
		if err != nil {
			s.logger.Warn("Failed to get client process ID", "clientID", clientID, "error", err)
			processID = 0
		} else {
			s.logger.Debug("Handling client", "clientID", clientID, "processID", processID)
		}
	}

	for {
		// Read header (winio 在 MessageMode 下 conn.Read 自带消息边界 + ERROR_MORE_DATA 处理)
		header, err := s.codec.ReadHeader(conn)
		if err != nil {
			if isPipeClosed(err) {
				s.logger.Debug("Bridge pipe closed by peer", "clientID", clientID, "error", err)
			} else {
				s.logger.Error("Failed to read header from Bridge", "clientID", clientID, "error", err)
			}
			break
		}

		// Read payload
		payload, err := s.codec.ReadPayload(conn, header.Length)
		if err != nil {
			if isPipeClosed(err) {
				s.logger.Debug("Bridge pipe closed by peer during payload read", "clientID", clientID, "error", err)
			} else {
				s.logger.Error("Failed to read payload from Bridge", "clientID", clientID, "error", err)
			}
			break
		}

		// Check if this is an async request (no response expected)
		isAsync := s.codec.IsAsyncRequest(header)

		// Handle batch events
		if header.Command == ipc.CmdBatchEvents {
			s.handleBatchEvents(header, payload, conn, clientID, processID)
			continue
		}

		// Activation commands (IMEActivated / FocusGained) 走两段式异步化：
		//
		// 1. processRequest 立即返回（仅做 active 状态字段更新，无任何 handler 调用、
		//    无任何跨进程 shell 调用）。若 C++ 端走 sync 形态，这一步会把 Ack 写回去；
		//    若走 async 形态（当前 wind_tsf 已切换到此），跳过写回。
		//    无论哪种形态，C++ 端的同步 ReceiveResponse 都不会再阻塞「等 Go 完成 handler」，
		//    explorer.exe 等宿主 UI 线程的环形等待被切断。
		//
		// 2. 第二段：在本 goroutine 内同步调用 HandleIMEActivated / HandleFocusGained，
		//    完成后通过 push pipe 推送 CmdActivationStatusPush（C++ 端 AsyncReader 收到
		//    后 Post 到 TSF 线程做 _SyncStateFromResponse + _EnsureHostRenderSetup）。
		//
		// 为什么 isActivation 不再根据 isAsync 过滤：activation 命令的 handler 调用是
		// 协议契约的一部分（工具栏显示、LangBar 状态同步都在 handler 内完成）。无论 C++
		// 端选 sync 还是 async 发送，handler 必须执行；只有「是否回 Ack」会随 isAsync 变化。
		//
		// 为什么保持在同一 handleClient goroutine 而不是 spawn 新 goroutine：
		//   - 单 client 的命令在 bridge pipe 上天然串行；同 goroutine 内联执行可天然
		//     保证 IMEActivated → IMEDeactivated 顺序正确，无需额外锁。
		//   - 本 goroutine 此时已经把 Ack 写出（或直接放行），C++ 端可继续派发新命令；
		//     这些新命令会在 handleClient 下一轮 ReadHeader 时排队读取。
		isActivation := header.Command == ipc.CmdIMEActivated || header.Command == ipc.CmdFocusGained

		// Process request with timeout
		response := s.processRequestWithTimeout(header, payload, clientID, processID)

		// Write response unless this is an async request.
		if !isAsync {
			if err := s.codec.WriteMessage(conn, response); err != nil {
				if isPipeClosed(err) {
					s.logger.Debug("Bridge pipe closed by peer during response write", "clientID", clientID, "error", err)
				} else {
					s.logger.Error("Failed to write response to Bridge", "clientID", clientID, "error", err)
				}
				break
			}
		} else {
			s.logger.Debug("Async request processed, no response sent", "clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command))
		}

		// Activation 两段式第二段：handler 调用 + push, 不受 isAsync 影响。
		if isActivation {
			s.runActivationHandlerAndPush(header, payload, clientID, processID)
		}
	}

	s.logger.Info("C++ Bridge disconnected", "clientID", clientID)
	return processID
}

// pushOutboundBufferSize: per-client push 广播队列容量。
// 状态/配置推送 idempotent，队列满则 drop 最新（下次 push 自带最新 value）。
const pushOutboundBufferSize = 16

// pushWriteTimeout: push pipe 单次写出的超时上限。
// 对端进程活着但 AsyncReader 不消费（半开连接）时，内核接收缓冲填满后
// overlapped Write 会无限期 park writer goroutine；超时让写返回错误，
// writer loop 据此 shutdown 该连接自愈。与 main pipe 的 30s 写超时对称。
const pushWriteTimeout = 30 * time.Second

// pushClient wraps a winio-backed net.Conn for push pipe (Go→C++ broadcasts).
//
// 关键设计：
//   - 底层 conn 是 winio 的 overlapped I/O 包装，Read/Write 不互相串行化
//     （这是从旧 windows.Handle sync I/O 迁移过来的根本动力——旧设计中
//     同 handle 上 sync Read park 会阻塞 sync Write，导致 push 永远卡住）。
//   - outbound 提供 per-client 非阻塞入队；writer goroutine 单独消费。
//   - mu 串行化"writer goroutine 的 drain"与"PushCommitText 等同步直写"，
//     保证 message 顺序在同 client 上一致。
//   - handle 缓存 conn.Fd()——用作所有 push 路径上的 stable identifier
//     （PID/token 反向映射的 key），避免每次都做 type assertion。
//   - closeOnce 保护 conn.Close() / outbound channel 关闭幂等。
type pushClient struct {
	conn      net.Conn
	handle    windows.Handle
	mu        sync.Mutex
	outbound  chan []byte
	closeOnce sync.Once
}

// fdGetter 是 winio 内部 win32File 暴露的 Fd 接口（未导出但通过 interface
// 断言可访问）。conn 走的是 net.Conn 标准接口，但 underlying 类型是
// winio 的 win32MessageBytePipe → win32Pipe → *win32File（具备 Fd()）。
type fdGetter interface {
	Fd() uintptr
}

// newPushClient 从一个新 Accept 的 winio.PipeConn 构造 pushClient。
// 提取底层 handle 用作 key；不持有也不修改 handle 生命周期（conn.Close
// 负责释放）。
func newPushClient(conn net.Conn) (*pushClient, error) {
	g, ok := conn.(fdGetter)
	if !ok {
		return nil, fmt.Errorf("push pipe conn does not expose Fd()")
	}
	return &pushClient{
		conn:     conn,
		handle:   windows.Handle(g.Fd()),
		outbound: make(chan []byte, pushOutboundBufferSize),
	}, nil
}

// Write 通过 mu 串行化写入；底层 net.Conn.Write 走 winio overlapped。
// 每次写前重置 pushWriteTimeout 写超时：覆盖「对端活着但不读」的半开连接，
// 超时错误会让 pushWriterLoop shutdown 本连接自愈。
func (c *pushClient) Write(p []byte) (int, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	_ = c.conn.SetWriteDeadline(time.Now().Add(pushWriteTimeout))
	return c.conn.Write(p)
}

// enqueueBroadcast 非阻塞地把消息塞进 outbound；满则返回 false。
func (c *pushClient) enqueueBroadcast(msg []byte) bool {
	if c == nil || c.outbound == nil {
		return false
	}
	select {
	case c.outbound <- msg:
		return true
	default:
		return false
	}
}

// shutdown 关闭 outbound 让 writer goroutine 在 drain 后退出；
// 同时主动 Disconnect + Close conn 让 C++ 端立即感知 broken pipe。
// 多次调用安全（closeOnce）。
func (c *pushClient) shutdown() {
	if c == nil {
		return
	}
	c.closeOnce.Do(func() {
		if c.outbound != nil {
			close(c.outbound)
		}
		// PipeConn.Disconnect() 调用 DisconnectNamedPipe 强制 client 端
		// 收到 broken pipe；Close() 再释放 server handle。
		// 单独 Close 在 client 持有 handle 时不会通知 client（内核引用计数）。
		if pc, ok := c.conn.(interface{ Disconnect() error }); ok {
			_ = pc.Disconnect()
		}
		_ = c.conn.Close()
	})
}
