//go:build darwin

package bridge

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"image"
	"io"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"

	"github.com/huanfeng/wind_input/internal/ipc"
)

// server_darwin.go 提供 darwin 上 bridge IPC 的完整实现, 走 Unix Domain Socket。
//
// 设计:
//   - 主 socket (BridgePipeName / bridge.sock): 请求-响应通道, 每个 IMKit 端的
//     IMKInputController 实例打开一条独立连接 (与 PR-1 设计文档的方案 A 一致)
//   - 推送 socket (PushPipeName / bridge_push.sock): 服务端主动写, 客户端只读
//   - 协议: 完全复用 internal/ipc 二进制帧, 与 Windows 端的 wire 完全一致,
//     IMKit `.app` 端写一次解码器即可服务两个平台
//   - client identity: 用 accept 顺序自增 connID (uint32) 替代 Windows PID。
//     macOS 上"客户端"是 IMKInputController 实例, 不映射到 OS PID;
//     真正的"前台应用 bundleID"由 IMKit 在 attach 帧自报, 不在这层处理。
//
// 重要简化:
//   - 不实现 focus token / multi-instance 同 PID 区分逻辑 (Windows 多 TextService
//     场景在 macOS 上不存在 — 同一个 IMKit `.app` 进程内 controller 通过独立连接区分)
//   - 不实现 host render (见 host_render_darwin.go 的 no-op 注释)
//
// 退出策略:
//   - Start() 返回前会 MkdirAll 端点目录, 并清理已存在的 stale socket 文件
//   - RestartService() 关闭所有 conn, 调用方负责 Listener.Close

// 常量与 Win 共享 (在 server.go 中定义); 这里仅 darwin 独有的:
const (
	// darwinSocketBuf 主请求帧最大长度; 与 Win 的 PipeBufferSize 对齐, 让 IMKit 端
	// 不需要按平台选不同的 buffer 大小。
	darwinSocketBuf = 64 * 1024

	// darwinRequestProcessTimeout 单个请求处理时长警戒线, 同 Win 版 RequestProcessTimeout。
	darwinRequestProcessTimeout = 1000 * time.Millisecond
)

// RequestProcessTimeout 复刻 Windows 端同名常量, 供 server_handler.go 等
// 平台无关代码引用 (虽然 server_handler.go 已在 PR-6 中 Win-only, 但保留以备未来
// 把 handler 业务层提到平台无关时不漏定义)。
const RequestProcessTimeout = darwinRequestProcessTimeout

// PipeBufferSize 与 Win 端同名常量对齐。
const PipeBufferSize = darwinSocketBuf

// connID 给 accept 到的每个连接分配一个进程内唯一 ID, 替代 Windows PID。
type connID uint32

// pushClient 表示一个已连接的 push socket 客户端。
type pushClient struct {
	conn   net.Conn
	connID connID
	mu     sync.Mutex // 序列化写入避免帧交错
}

// Server darwin 上的 bridge IPC 服务端。
//
// 与 Win 版同名但字段独立 (无 windows.Handle / winio 引用)。
type Server struct {
	logger  *slog.Logger
	handler MessageHandler
	codec   *ipc.BinaryCodec

	// 主请求-响应 listener (bridge.sock)
	listener net.Listener

	// 推送 listener (bridge_push.sock)
	pushListener net.Listener

	mu          sync.RWMutex
	clientCount int
	nextConnID  uint32 // atomic counter
	activeConns map[net.Conn]connID

	// push 客户端列表; 服务端主动 push 时 fanout 写所有 client。
	// macOS 上 push 路由用 connID, 暂不区分 "active client" (单 IMKit `.app` 场景
	// 下所有 client 都属同一前端进程)。
	pushMu          sync.RWMutex
	pushClients     map[connID]*pushClient
	pushClientCount int

	// activeProcessID 名义保留 (Win 端有焦点 PID 概念), darwin 上始终为 0。
	activeMu        sync.RWMutex
	activeProcessID uint32

	// focusedClients 简化为 connID → 是否聚焦, 用于 IsActivelyFocusedPID 接口兼容。
	focusMu        sync.RWMutex
	focusedClients map[connID]bool

	// hostRender 在 darwin 上是 no-op 占位 (见 host_render_darwin.go)。
	hostRender *HostRenderManager

	// shutdown 协调优雅退出。
	shutdownOnce sync.Once
	shutdownCh   chan struct{}
}

// NewServer 创建 darwin bridge Server。
func NewServer(handler MessageHandler, logger *slog.Logger) *Server {
	return &Server{
		handler:        handler,
		logger:         logger,
		codec:          ipc.NewBinaryCodec(),
		activeConns:    make(map[net.Conn]connID),
		pushClients:    make(map[connID]*pushClient),
		focusedClients: make(map[connID]bool),
		shutdownCh:     make(chan struct{}),
	}
}

// SetHostRenderManager 兼容 Win 接口, 但 darwin 上 manager 是 no-op。
func (s *Server) SetHostRenderManager(hrm *HostRenderManager) { s.hostRender = hrm }

// GetHostRenderManager 返回当前 manager (可能为 nil)。
func (s *Server) GetHostRenderManager() *HostRenderManager { return s.hostRender }

// IsActivelyFocusedPID darwin 上 PID 概念不适用; 始终返回 false。
// 调用方 (coordinator 工具栏前台 hook) 在 darwin 上应改用 IMKit 自报的 bundleID,
// 但本次 PR-6 范围未触及那层, 故此 stub 保留以让代码可编译。
func (s *Server) IsActivelyFocusedPID(pid uint32) bool { return false }

// GetActiveHostRender darwin 上始终返回 nil (无 host render)。
func (s *Server) GetActiveHostRender() (writeFrame func(img *image.RGBA, x, y int) error, hideFunc func()) {
	return nil, nil
}

// Start 启动 bridge 监听。先建立运行时目录, 再 listen 两个 socket。
// 阻塞直到 listener 异常或 RestartService 调用。
func (s *Server) Start() error {
	dir := filepath.Dir(BridgePipeName)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("bridge: mkdir runtime dir %s: %w", dir, err)
	}

	// 清理 stale socket 文件 (上次进程未优雅退出残留)
	_ = os.Remove(BridgePipeName)
	_ = os.Remove(PushPipeName)

	ln, err := net.Listen("unix", BridgePipeName)
	if err != nil {
		return fmt.Errorf("bridge: listen unix %s: %w", BridgePipeName, err)
	}
	s.listener = ln
	s.logger.Info("Starting Bridge IPC server (darwin UDS)", "socket", BridgePipeName)

	pushLn, err := net.Listen("unix", PushPipeName)
	if err != nil {
		_ = ln.Close()
		return fmt.Errorf("bridge: listen push unix %s: %w", PushPipeName, err)
	}
	s.pushListener = pushLn
	s.logger.Info("Starting Push pipe listener (darwin UDS)", "socket", PushPipeName)

	go s.acceptPushLoop()

	// 主请求 accept loop, 阻塞直到 listener.Close
	s.acceptLoop()

	return nil
}

// acceptLoop 接受主请求-响应连接。
func (s *Server) acceptLoop() {
	for {
		conn, err := s.listener.Accept()
		if err != nil {
			if errors.Is(err, net.ErrClosed) {
				s.logger.Info("Bridge listener closed, accept loop exit")
				return
			}
			s.logger.Error("bridge accept error", "err", err)
			// 临时错误继续; 致命错误退出。net.Listener.Accept 返回的 OpError
			// 通常是临时网络错误, 简单重试。
			time.Sleep(50 * time.Millisecond)
			continue
		}
		id := connID(atomic.AddUint32(&s.nextConnID, 1))

		s.mu.Lock()
		s.activeConns[conn] = id
		s.clientCount++
		s.mu.Unlock()

		go s.handleClient(conn, id)
	}
}

// acceptPushLoop 接受 push 通道连接, 把 conn 登记到 pushClients。
func (s *Server) acceptPushLoop() {
	for {
		conn, err := s.pushListener.Accept()
		if err != nil {
			if errors.Is(err, net.ErrClosed) {
				return
			}
			s.logger.Error("push accept error", "err", err)
			time.Sleep(50 * time.Millisecond)
			continue
		}
		id := connID(atomic.AddUint32(&s.nextConnID, 1))
		pc := &pushClient{conn: conn, connID: id}
		s.pushMu.Lock()
		s.pushClients[id] = pc
		s.pushClientCount++
		s.pushMu.Unlock()
		s.logger.Info("push client connected", "connID", id, "total", s.pushClientCount)

		// 服务端不读 push channel, 但起一个 goroutine 监听对端关闭。
		go s.watchPushClient(pc)
	}
}

func (s *Server) watchPushClient(pc *pushClient) {
	// 读 1 字节即可探测对端关闭; UDS Read 在 EOF 返回 (0, io.EOF)。
	buf := make([]byte, 1)
	for {
		n, err := pc.conn.Read(buf)
		if err != nil || n == 0 {
			s.pushMu.Lock()
			delete(s.pushClients, pc.connID)
			s.pushClientCount--
			s.pushMu.Unlock()
			_ = pc.conn.Close()
			s.logger.Info("push client disconnected", "connID", pc.connID)
			return
		}
		// darwin 上 push 通道理论不会收到客户端数据, 丢弃。
	}
}

// handleClient 单个请求-响应连接的循环。读一帧, 处理, 写响应。
// 退出时清理状态。
func (s *Server) handleClient(conn net.Conn, id connID) {
	defer func() {
		_ = conn.Close()
		s.mu.Lock()
		delete(s.activeConns, conn)
		s.clientCount--
		s.mu.Unlock()
		s.focusMu.Lock()
		delete(s.focusedClients, id)
		s.focusMu.Unlock()
		s.handler.HandleClientDisconnected(s.GetActiveClientCount())
		s.logger.Info("bridge client disconnected", "connID", id)
	}()
	s.logger.Info("bridge client connected", "connID", id)

	for {
		header, err := s.codec.ReadHeader(conn)
		if err != nil {
			if !isUDSClosed(err) {
				s.logger.Debug("bridge read header error", "err", err, "connID", id)
			}
			return
		}
		payload, err := s.codec.ReadPayload(conn, header.Length)
		if err != nil {
			s.logger.Debug("bridge read payload error", "err", err, "connID", id)
			return
		}
		s.dispatchFrame(conn, id, header, payload)
	}
}

// dispatchFrame 把单帧路由到 MessageHandler。
// 这是 darwin 上的极简 dispatch (Win 端 server_handler.go 一千行业务路径
// 涵盖 token / focus 多客户端等场景, darwin 上 macOS forwarder 接入时按需重写)。
//
// PR-6 范围: 只覆盖 KeyEvent / FocusGained / FocusLost / Caret / IMEActivated /
// IMEDeactivated / ToggleMode 等核心帧, 其他帧暂时打日志后忽略 (返回 Ack)。
func (s *Server) dispatchFrame(conn net.Conn, id connID, header *ipc.IpcHeader, payload []byte) {
	ctx, cancel := context.WithTimeout(context.Background(), darwinRequestProcessTimeout)
	defer cancel()
	_ = ctx

	switch header.Command {
	case ipc.CmdKeyEvent:
		p, err := s.codec.DecodeKeyPayload(payload)
		if err != nil {
			s.logger.Warn("decode KeyEvent failed", "err", err)
			s.writeAck(conn)
			return
		}
		event := "down"
		if p.EventType == 1 {
			event = "up"
		}
		res := s.handler.HandleKeyEvent(KeyEventData{
			KeyCode:   int(p.KeyCode),
			Modifiers: int(p.Modifiers),
			Event:     event,
			Toggles:   p.Toggles,
			PrevChar:  rune(p.PrevChar),
		})
		s.writeKeyResult(conn, res)
	case ipc.CmdCaretUpdate:
		p, err := s.codec.DecodeCaretPayload(payload)
		if err == nil {
			_ = s.handler.HandleCaretUpdate(CaretData{
				X: int(p.X), Y: int(p.Y), Height: int(p.Height),
				CompositionStartX: int(p.CompositionStartX),
				CompositionStartY: int(p.CompositionStartY),
			})
		}
		s.writeAck(conn)
	case ipc.CmdCaretPending:
		s.handler.HandleCaretPending()
		s.writeAck(conn)
	case ipc.CmdFocusLost:
		s.handler.HandleFocusLost()
		s.focusMu.Lock()
		delete(s.focusedClients, id)
		s.focusMu.Unlock()
		s.writeAck(conn)
	case ipc.CmdFocusGained:
		// payload[0..4] 是 PID (Win 端约定); darwin 上 PID 概念不适用, 取 0。
		var pid uint32
		if len(payload) >= 4 {
			pid = binary.LittleEndian.Uint32(payload[0:4])
		}
		_ = s.handler.HandleFocusGained(pid)
		s.focusMu.Lock()
		s.focusedClients[id] = true
		s.focusMu.Unlock()
		s.writeAck(conn)
	case ipc.CmdIMEDeactivated:
		s.handler.HandleIMEDeactivated()
		s.writeAck(conn)
	case ipc.CmdIMEActivated:
		var pid uint32
		if len(payload) >= 4 {
			pid = binary.LittleEndian.Uint32(payload[0:4])
		}
		_ = s.handler.HandleIMEActivated(pid)
		s.focusMu.Lock()
		s.focusedClients[id] = true
		s.focusMu.Unlock()
		s.writeAck(conn)
	case ipc.CmdToggleMode:
		_, _ = s.handler.HandleToggleMode()
		s.writeAck(conn)
	default:
		// 未覆盖的帧 (HostRender setup / token 等 Win-only 概念) 直接 Ack,
		// macOS forwarder 在自己的 PR 内按需扩展 dispatch。
		s.writeAck(conn)
	}
}

func (s *Server) writeAck(conn net.Conn) {
	_, _ = conn.Write(s.codec.EncodeAck())
}

// writeKeyResult 把 KeyEventResult 序列化写回客户端。
// 简化版: 仅覆盖最常见的响应类型, 其他 (commit/composition update) 走 ack。
// 后续 PR 在 macOS forwarder 端补完整响应。
func (s *Server) writeKeyResult(conn net.Conn, r *KeyEventResult) {
	if r == nil {
		_, _ = conn.Write(s.codec.EncodeConsumed())
		return
	}
	switch r.Type {
	case ResponseTypePassThrough:
		_, _ = conn.Write(s.codec.EncodePassThrough())
	case ResponseTypeConsumed:
		_, _ = conn.Write(s.codec.EncodeConsumed())
	case ResponseTypeClearComposition:
		_, _ = conn.Write(s.codec.EncodeClearComposition())
	default:
		_, _ = conn.Write(s.codec.EncodeAck())
	}
}

// ============================================================================
// Push API (Win 接口对齐)
// ============================================================================

// PushStateToActiveClient 推 status update 到所有 push 客户端 (darwin 上单 IMKit
// `.app` 进程视为唯一 active client, 简化为 fanout 给所有 push 连接)。
func (s *Server) PushStateToActiveClient(status *StatusUpdateData) {
	if status == nil {
		return
	}
	// 简化: 不区分 keyDown/keyUp hotkeys, 也不 carry iconLabel; 详细 wire 由后续
	// PR 在 macOS forwarder 实装时按需对齐。
	frame := s.codec.EncodeStatePush(
		status.ChineseMode, status.FullWidth, status.ChinesePunctuation,
		status.ToolbarVisible, status.CapsLock, status.IconLabel,
	)
	s.broadcastPush(frame)
}

// PushCommitTextToActiveClient darwin 上同样 fanout (single-client 场景下行为等价)。
func (s *Server) PushCommitTextToActiveClient(text string) {
	frame := s.codec.EncodeCommitText(text, "", false, false, false)
	s.broadcastPush(frame)
}

// PushClearCompositionToActiveClient ...
func (s *Server) PushClearCompositionToActiveClient() {
	s.broadcastPush(s.codec.EncodeClearComposition())
}

// PushUpdateCompositionToActiveClient ...
func (s *Server) PushUpdateCompositionToActiveClient(text string, caretPos int) {
	s.broadcastPush(s.codec.EncodeUpdateComposition(text, caretPos))
}

// PushEnglishPairConfigToActiveClient ...
func (s *Server) PushEnglishPairConfigToActiveClient(enabled bool, pairs []string) {
	value := ipc.EncodeEnglishPairsValue(enabled, pairs)
	s.broadcastPush(s.codec.EncodeSyncConfig(ipc.ConfigKeyEnglishPairs, value))
}

// PushStatsConfigToActiveClient ...
// darwin 上简化: 仅打 debug 日志, 不发送 (Win 端的 stats 走专用帧, macOS 暂不需要)。
func (s *Server) PushStatsConfigToActiveClient(enabled bool, trackEnglish bool) {
	s.logger.Debug("PushStatsConfigToActiveClient (darwin no-op)",
		"enabled", enabled, "trackEnglish", trackEnglish)
}

// broadcastPush 写帧到所有 push client。任何写失败标记 client 待清理。
func (s *Server) broadcastPush(frame []byte) {
	s.pushMu.RLock()
	clients := make([]*pushClient, 0, len(s.pushClients))
	for _, c := range s.pushClients {
		clients = append(clients, c)
	}
	s.pushMu.RUnlock()

	for _, c := range clients {
		c.mu.Lock()
		_, err := c.conn.Write(frame)
		c.mu.Unlock()
		if err != nil {
			s.logger.Debug("push write error, closing", "connID", c.connID, "err", err)
			_ = c.conn.Close()
		}
	}
}

// GetActiveClientCount 返回当前主连接数。
func (s *Server) GetActiveClientCount() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.clientCount
}

// RestartService 关闭所有连接 + listeners; cmd/service/main.go 的重启路径在 Win
// 上会 process.Exit(), darwin 上同样由调用方决定。
func (s *Server) RestartService() {
	s.shutdownOnce.Do(func() {
		close(s.shutdownCh)
		if s.listener != nil {
			_ = s.listener.Close()
		}
		if s.pushListener != nil {
			_ = s.pushListener.Close()
		}
		s.mu.Lock()
		for conn := range s.activeConns {
			_ = conn.Close()
		}
		s.mu.Unlock()
		// 清理 stale socket 文件
		_ = os.Remove(BridgePipeName)
		_ = os.Remove(PushPipeName)
	})
}

// isUDSClosed 判定 err 是否为对端正常关闭 UDS 的预期错误。
func isUDSClosed(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) || errors.Is(err, net.ErrClosed) {
		return true
	}
	return false
}
