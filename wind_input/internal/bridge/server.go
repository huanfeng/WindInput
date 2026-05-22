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

	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/pkg/buildvariant"
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

// readBufPool 复用 64KB 管道读取缓冲区，避免每次消息读取都 make([]byte, 64KB)。
var readBufPool = sync.Pool{
	New: func() any {
		buf := make([]byte, PipeBufferSize)
		return &buf
	},
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

var (
	BridgePipeName = `\\.\pipe\wind_input` + buildvariant.Suffix()
	PushPipeName   = `\\.\pipe\wind_input` + buildvariant.Suffix() + `_push`
)

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

	mu            sync.RWMutex
	clientCount   int
	activeHandles map[windows.Handle]*pipeWriter // Map handle to writer for broadcasting

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

	// Host render manager (for Band window proxy rendering)
	hostRender *HostRenderManager
}

// NewServer creates a new Bridge IPC server
func NewServer(handler MessageHandler, logger *slog.Logger) *Server {
	return &Server{
		handler:           handler,
		logger:            logger,
		codec:             ipc.NewBinaryCodec(),
		activeHandles:     make(map[windows.Handle]*pipeWriter),
		pushClients:       make(map[windows.Handle]*pushClient),
		pushClientsByPID:  make(map[uint32]windows.Handle),
		pushHandleToPID:   make(map[windows.Handle]uint32),
		tokenToPushHandle: make(map[uint64]windows.Handle),
		pushHandleToToken: make(map[windows.Handle]uint64),
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

// Start begins listening for connections from C++ Bridge
func (s *Server) Start() error {
	s.logger.Info("Starting Bridge IPC server (binary protocol)", "pipe", BridgePipeName)

	// Start the push pipe listener in a separate goroutine
	go s.startPushPipeListener()

	// Allow desktop clients plus AppContainer/modern hosts (e.g. Start menu search).
	// S:(ML;;NW;;;LW) = Mandatory Label: Low integrity — required for UWP/AppContainer
	//   processes (Microsoft Store, Start Menu) which run at low integrity level.
	//   Without this, the mandatory integrity check blocks access before DACL evaluation.
	// D: = DACL: WD=Everyone, SY=SYSTEM, BA=Administrators, AC=ALL APPLICATION PACKAGES
	sddl := "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)S:(ML;;NW;;;LW)"
	sd, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		s.logger.Error("Failed to create security descriptor", "error", err)
		sd = nil
	}

	var sa *windows.SecurityAttributes
	if sd != nil {
		sa = &windows.SecurityAttributes{
			Length:             uint32(unsafe.Sizeof(windows.SecurityAttributes{})),
			SecurityDescriptor: sd,
		}
	}

	for {
		pipePath, err := windows.UTF16PtrFromString(BridgePipeName)
		if err != nil {
			return fmt.Errorf("failed to convert pipe path: %w", err)
		}

		handle, err := windows.CreateNamedPipe(
			pipePath,
			windows.PIPE_ACCESS_DUPLEX,
			// Use MESSAGE mode like Weasel for more reliable message boundaries
			windows.PIPE_TYPE_MESSAGE|windows.PIPE_READMODE_MESSAGE|windows.PIPE_WAIT,
			windows.PIPE_UNLIMITED_INSTANCES,
			PipeBufferSize, // 64KB like Weasel
			PipeBufferSize,
			0,
			sa,
		)

		if err != nil {
			return fmt.Errorf("failed to create named pipe: %w", err)
		}

		s.logger.Debug("Waiting for C++ Bridge connection...")

		err = windows.ConnectNamedPipe(handle, nil)
		if err != nil && err != windows.ERROR_PIPE_CONNECTED {
			windows.CloseHandle(handle)
			continue
		}

		// Create pipe writer for this client
		writer := &pipeWriter{handle: handle}

		s.mu.Lock()
		s.clientCount++
		clientID := s.clientCount
		s.activeHandles[handle] = writer
		s.mu.Unlock()

		s.logger.Info("C++ Bridge connected", "clientID", clientID)

		// Handle client in a separate goroutine to allow concurrent connections
		go func(h windows.Handle, id int) {
			pid := s.handleClient(h, id)

			// Capture the current setup sequence BEFORE acquiring the main lock.
			// This prevents a race where the old connection's cleanup goroutine
			// destroys a newer connection's SharedMemory for the same PID.
			var setupSeq uint64
			if s.hostRender != nil && pid != 0 {
				setupSeq = s.hostRender.GetSetupSeq(pid)
			}

			s.mu.Lock()
			delete(s.activeHandles, h)
			activeCount := len(s.activeHandles)
			s.mu.Unlock()

			// Clean up host render resources only if the generation matches
			if s.hostRender != nil && pid != 0 && setupSeq != 0 {
				s.hostRender.CleanupClient(pid, setupSeq)
			}

			// Notify handler that a client disconnected
			s.handler.HandleClientDisconnected(activeCount)
		}(handle, clientID)
	}
}

func (s *Server) handleClient(handle windows.Handle, clientID int) uint32 {
	defer windows.CloseHandle(handle)

	// Get the client's process ID for tracking active client
	processID, err := getNamedPipeClientProcessId(handle)
	if err != nil {
		s.logger.Warn("Failed to get client process ID", "clientID", clientID, "error", err)
		processID = 0 // Continue without process ID tracking
	} else {
		s.logger.Debug("Handling client", "clientID", clientID, "processID", processID)
	}

	// Create a pipe reader wrapper
	reader := &pipeReader{handle: handle}
	defer reader.release()
	writer := &pipeWriter{handle: handle}

	for {
		// Read header
		header, err := s.codec.ReadHeader(reader)
		if err != nil {
			if isPipeClosed(err) {
				s.logger.Debug("Bridge pipe closed by peer", "clientID", clientID, "error", err)
			} else {
				s.logger.Error("Failed to read header from Bridge", "clientID", clientID, "error", err)
			}
			break
		}

		// Read payload
		payload, err := s.codec.ReadPayload(reader, header.Length)
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
			s.handleBatchEvents(header, payload, writer, clientID, processID)
			continue
		}

		// Process request with timeout
		response := s.processRequestWithTimeout(header, payload, clientID, processID)

		// Skip response for async requests
		if isAsync {
			s.logger.Debug("Async request processed, no response sent", "clientID", clientID, "command", fmt.Sprintf("0x%04X", header.Command))
			continue
		}

		// Write response
		if err := s.codec.WriteMessage(writer, response); err != nil {
			if isPipeClosed(err) {
				s.logger.Debug("Bridge pipe closed by peer during response write", "clientID", clientID, "error", err)
			} else {
				s.logger.Error("Failed to write response to Bridge", "clientID", clientID, "error", err)
			}
			break
		}
	}

	s.logger.Info("C++ Bridge disconnected", "clientID", clientID)
	return processID
}

// pipeReader wraps windows.Handle for io.Reader
// In MESSAGE mode, each ReadFile returns a complete message
type pipeReader struct {
	handle    windows.Handle
	msgBuffer []byte  // Buffer for current message (slice of poolBuf or heap)
	msgOffset int     // Current read offset in msgBuffer
	poolBuf   *[]byte // Pool buffer held until current message is fully consumed
}

func (r *pipeReader) Read(p []byte) (int, error) {
	// If we have buffered data from a previous message read, return that first
	if r.msgOffset < len(r.msgBuffer) {
		n := copy(p, r.msgBuffer[r.msgOffset:])
		r.msgOffset += n
		return n, nil
	}

	// Current message fully consumed; return pool buffer before acquiring a new one
	if r.poolBuf != nil {
		readBufPool.Put(r.poolBuf)
		r.poolBuf = nil
		r.msgBuffer = nil
	}

	// Acquire a reusable 64KB buffer from the pool
	bufPtr := readBufPool.Get().(*[]byte)
	readBuf := *bufPtr
	var bytesRead uint32

	err := windows.ReadFile(r.handle, readBuf, &bytesRead, nil)
	if err != nil {
		// Handle ERROR_MORE_DATA - message is larger than 64KB (should not happen in practice)
		if err == windows.ERROR_MORE_DATA {
			// Copy partial data out BEFORE returning pool buffer to avoid race with other goroutines.
			accum := make([]byte, bytesRead)
			copy(accum, readBuf[:bytesRead])
			readBufPool.Put(bufPtr)
			for {
				tmpPtr := readBufPool.Get().(*[]byte)
				tmp := *tmpPtr
				err = windows.ReadFile(r.handle, tmp, &bytesRead, nil)
				accum = append(accum, tmp[:bytesRead]...)
				readBufPool.Put(tmpPtr)
				if err == nil {
					break
				}
				if err != windows.ERROR_MORE_DATA {
					return 0, err
				}
			}
			r.msgBuffer = accum
			r.msgOffset = 0
			n := copy(p, r.msgBuffer)
			r.msgOffset = n
			return n, nil
		}
		readBufPool.Put(bufPtr)
		return 0, err
	}

	if bytesRead == 0 {
		readBufPool.Put(bufPtr)
		return 0, io.EOF
	}

	// Hold the pool buffer until this entire message is consumed
	r.poolBuf = bufPtr
	r.msgBuffer = readBuf[:bytesRead]
	r.msgOffset = 0

	n := copy(p, r.msgBuffer)
	r.msgOffset = n
	return n, nil
}

// release returns any held pool buffer back to the pool. Must be called when the reader is done.
func (r *pipeReader) release() {
	if r.poolBuf != nil {
		readBufPool.Put(r.poolBuf)
		r.poolBuf = nil
	}
	r.msgBuffer = nil
}

// pipeWriter is the synchronous bridge-pipe writer (request-response RPC).
// 仅用于 bridge pipe；push pipe 已迁移到 net.Conn 基于 winio 的 pushClient。
// mu 串行化并发 WriteFile（Windows 命名管道未保证 thread-safe）。
type pipeWriter struct {
	handle windows.Handle
	mu     sync.Mutex
}

func (w *pipeWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	var bytesWritten uint32
	err := windows.WriteFile(w.handle, p, &bytesWritten, nil)
	if err != nil {
		return 0, err
	}
	return int(bytesWritten), nil
}

// pushOutboundBufferSize: per-client push 广播队列容量。
// 状态/配置推送 idempotent，队列满则 drop 最新（下次 push 自带最新 value）。
const pushOutboundBufferSize = 16

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
func (c *pushClient) Write(p []byte) (int, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
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
