//go:build windows

package bridge

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"time"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/internal/ipc"
	"golang.org/x/sys/windows"
)

// startPushPipeListener 用 go-winio 起 push pipe listener。
//
// 关键架构变更（从手工 CreateNamedPipe+sync I/O 迁移到 winio overlapped I/O）：
//   - 同 conn 上的 Read/Write 不再被内核串行化（旧设计中 phase-2 reader 的
//     sync ReadFile park 会阻塞 writer 的 sync WriteFile，导致 push 永远卡住）。
//   - Phase-2 死链监听**重新启用**——overlapped read 可以与 write 并发。
//   - Disconnect+Close 由 winio PipeConn 接口提供（conn.Disconnect()+conn.Close()），
//     不再需要手工 DisconnectNamedPipe+CancelIoEx+CloseHandle 三联。
//   - 不再需要 watchdog——overlapped Write 不会被 sync read park 卡住，
//     真死的 client 会让 conn.Write 返回 broken pipe，自然走 cleanup；
//     「活着但不读」的半开连接由 pushClient.Write 的 pushWriteTimeout 写超时覆盖。
//   - 所有出站消息（含 CommitText/ClearComposition/UpdateComposition）统一经
//     outbound 队列由 pushWriterLoop 单点写出，保证同 client 消息顺序、
//     且任何调用方都不会被慢连接阻塞；禁止绕过队列直写 conn。
func (s *Server) startPushPipeListener() {
	s.logger.Info("Starting Push pipe listener", "pipe", PushPipeName)

	// Allow desktop clients plus AppContainer/modern hosts (e.g. Start menu search).
	// S:(ML;;NW;;;LW) = Mandatory Label: Low integrity — required for UWP/AppContainer
	// 处于低完整性（如 UWP/AppContainer）的客户端需要 AC + LW 才能连接。
	pipeConfig := &winio.PipeConfig{
		SecurityDescriptor: "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)S:(ML;;NW;;;LW)",
		MessageMode:        true,
		InputBufferSize:    16, // 仅用于接收 8 字节 token 握手
		OutputBufferSize:   int32(PipeBufferSize),
	}
	listener, err := winio.ListenPipe(PushPipeName, pipeConfig)
	if err != nil {
		s.logger.Error("Failed to listen push pipe", "error", err)
		return
	}

	for {
		conn, err := listener.Accept()
		if err != nil {
			// listener.Close 会让 Accept 返回 net.ErrClosed；当作正常退出。
			if errors.Is(err, net.ErrClosed) {
				s.logger.Info("Push pipe listener closed")
				return
			}
			s.logger.Error("Push pipe accept error", "error", err)
			time.Sleep(200 * time.Millisecond)
			continue
		}
		s.acceptPushClient(conn)
	}
}

// acceptPushClient 处理新接入的 push pipe 连接。
//
// 流程（vs 旧版的差异）：
//  1. 从 net.Conn 提取 windows.Handle 作为各 map 的稳定 key；
//  2. 调 GetNamedPipeClientProcessId 获取 client PID（与旧版一致）；
//  3. 同步写出 CMD_SERVICE_READY；
//  4. 启动 writer goroutine 消费 outbound；
//  5. 启动 reader goroutine 做 Phase-1 token + Phase-2 dead-link
//     —— Phase-2 现在安全了（winio overlapped 不串行化 read/write）。
func (s *Server) acceptPushClient(conn net.Conn) {
	client, err := newPushClient(conn)
	if err != nil {
		s.logger.Error("Failed to wrap push pipe connection", "error", err)
		_ = conn.Close()
		return
	}

	pushProcessID, err := getNamedPipeClientProcessId(client.handle)
	if err != nil {
		s.logger.Warn("Failed to get push pipe client process ID", "error", err)
		pushProcessID = 0
	}

	s.pushMu.Lock()
	s.pushClientCount++
	clientID := s.pushClientCount
	s.pushClients[client.handle] = client
	if pushProcessID != 0 {
		s.pushClientsByPID[pushProcessID] = client.handle
		s.pushHandleToPID[client.handle] = pushProcessID
	}
	s.pushMu.Unlock()

	s.logger.Info("Push pipe client connected", "clientID", clientID, "processID", pushProcessID)

	// 同步发送 SERVICE_READY，确保它是 client 收到的第一条消息。
	encoded := s.codec.EncodeServiceReady()
	if err := s.codec.WriteMessage(client, encoded); err != nil {
		s.logger.Warn("Failed to send CMD_SERVICE_READY to new push client",
			"clientID", clientID, "error", err)
	} else {
		s.logger.Debug("CMD_SERVICE_READY sent to new push client", "clientID", clientID)
	}

	// Writer：串行消费 outbound 队列。
	go s.pushWriterLoop(client, clientID, pushProcessID)
	// Reader：phase-1 token 握手 + phase-2 死链监听（overlapped 安全并发）。
	go s.pushReaderLoop(client, clientID, pushProcessID)

	// 历史：曾在此处给 coordinator 投递 HandlePushClientConnected 做"启动期
	// 补一拍"，但因 hook gate 已收紧为 IsActivelyFocusedPID（需要 FOCUS_GAINED/
	// IME_ACTIVATED 才进集合），新连接瞬间 focusedPIDs 必为空，kick 已无效。
	// DLL 端的 _DoFullStateSync(WM_SERVICE_READY) → CMD_IME_ACTIVATED 才是
	// 让宿主重获焦点的正路。删除冗余 kick 减少混淆。
}

// pushReaderLoop 处理 token 握手并持续监听 client 端断开。
//
// Phase-1：阻塞读 8 字节 token，注册到 token→handle 映射。
// Phase-2：继续阻塞 Read 等 client 关闭；任何错误/EOF 即触发 cleanup。
//
//	winio 的 overlapped Read 不会阻塞同 conn 上的 Write。
func (s *Server) pushReaderLoop(client *pushClient, cid int, pid uint32) {
	defer s.cleanupPushClient(client)

	// Phase 1: token 握手
	var buf [8]byte
	if _, err := io.ReadFull(client.conn, buf[:]); err != nil {
		s.logger.Info("Push pipe disconnected before token handshake",
			"clientID", cid, "processID", pid, "error", err)
		return
	}
	token := binary.LittleEndian.Uint64(buf[:])
	if token != 0 {
		s.pushMu.Lock()
		// 同 token 重连：旧 handle 必失效，主动清旧。按 token 不按 PID——同
		// PID 可能有多个合法实例（explorer.exe 的多个 CLangBar 宿主）。
		if oldH, ok := s.tokenToPushHandle[token]; ok && oldH != client.handle {
			if oldC, exists := s.pushClients[oldH]; exists {
				_ = s.cleanupPushHandle(oldH) // 已持锁，仅维护 map
				oldC.shutdown()
				s.logger.Info("Push pipe: stale handle replaced by token reconnect",
					"clientID", cid, "processID", pid, "token", token)
			}
		}
		if _, exists := s.pushClients[client.handle]; exists {
			s.tokenToPushHandle[token] = client.handle
			s.pushHandleToToken[client.handle] = token
		}
		s.pushMu.Unlock()
		s.logger.Debug("Push pipe: token registered",
			"clientID", cid, "processID", pid, "token", token)
	}

	// Phase 2: 死链监听——任何 Read 错误（包括 io.EOF / broken pipe）即对端断开。
	// 协议规定 token 后客户端不再写任何数据；此 Read 会一直 park 在内核 wait，
	// 但**不会**阻塞 writer goroutine（winio overlapped 设计）。
	var probe [16]byte
	for {
		if _, err := client.conn.Read(probe[:]); err != nil {
			s.logger.Info("Push pipe client disconnected",
				"clientID", cid, "processID", pid, "token", token, "error", err)
			return
		}
		// 协议不允许 token 后还有数据，丢弃并继续监听。
	}
}

// cleanupPushClient 是 reader/writer goroutine 退出时统一调用的清理。
// 从所有 map 中移除 handle 并 shutdown client（关 outbound + Disconnect + Close）。
func (s *Server) cleanupPushClient(client *pushClient) {
	s.pushMu.Lock()
	_ = s.cleanupPushHandle(client.handle)
	s.pushMu.Unlock()
	client.shutdown()
}

// pushWriterLoop 是 per-client 广播 worker——单 goroutine 串行消费 outbound。
// 写失败时退出；cleanup 由 reader goroutine 的 defer 完成（统一入口），避免
// 重复 cleanup。range over outbound 在 client.shutdown() 关闭后自然退出。
func (s *Server) pushWriterLoop(client *pushClient, cid int, pid uint32) {
	for msg := range client.outbound {
		var cmd uint16
		if len(msg) >= 4 {
			cmd = uint16(msg[2]) | (uint16(msg[3]) << 8)
		}
		writeStart := time.Now()
		err := s.codec.WriteMessage(client, msg)
		writeDuration := time.Since(writeStart)
		if err == nil {
			s.logger.Debug("Push pipe write completed",
				"clientID", cid, "processID", pid,
				"cmd", fmt.Sprintf("0x%04X", cmd), "size", len(msg),
				"duration", writeDuration.String())
			continue
		}
		if isPipeClosed(err) {
			s.logger.Debug("Push pipe writer exiting on peer close",
				"clientID", cid, "processID", pid, "error", err,
				"cmd", fmt.Sprintf("0x%04X", cmd), "duration", writeDuration.String())
		} else {
			s.logger.Warn("Push pipe writer aborting on write error",
				"clientID", cid, "processID", pid, "error", err,
				"cmd", fmt.Sprintf("0x%04X", cmd), "duration", writeDuration.String())
		}
		// shutdown 关 outbound + Disconnect+Close conn → reader goroutine 也会
		// 在下次 Read 时返回 error 并走 defer cleanup。
		client.shutdown()
		return
	}
}

// removePushHandleFromPIDIndex 在写失败清理时维护 pushClientsByPID 的一致性。
// 当被移除的 handle 恰好是该 PID 的最新记录时，尝试从 pushHandleToPID 中为同 PID
// 找另一个存活 handle 作替代；若无其他 handle 则删除该条目。
// 调用时必须持有 pushMu 写锁。
func (s *Server) removePushHandleFromPIDIndex(pid uint32, removedHandle windows.Handle) {
	if pid == 0 || s.pushClientsByPID[pid] != removedHandle {
		return
	}
	for h, p := range s.pushHandleToPID {
		if p == pid && h != removedHandle {
			s.pushClientsByPID[pid] = h
			return
		}
	}
	delete(s.pushClientsByPID, pid)
}

// cleanupPushHandle 从所有内部映射中移除一个 push handle。
// 调用时必须持有 pushMu 写锁，且必须在 windows.CloseHandle(handle) 之前调用。
// 返回 true 表示 handle 确实在 map 中并被移除；返回 false 表示已被其他
// goroutine 先行清理，调用方不应再调用 windows.CloseHandle(handle)。
// （removePushHandleFromPIDIndex 需要先读 pushHandleToPID 找替代 handle，
// 因此 pushHandleToPID 的实际删除放在最后。）
func (s *Server) cleanupPushHandle(handle windows.Handle) bool {
	w, exists := s.pushClients[handle]
	if !exists {
		return false
	}
	pid := s.pushHandleToPID[handle]
	s.removePushHandleFromPIDIndex(pid, handle) // 必须在 delete(pushHandleToPID) 之前
	delete(s.pushClients, handle)
	delete(s.pushHandleToPID, handle)
	if token := s.pushHandleToToken[handle]; token != 0 {
		delete(s.tokenToPushHandle, token)
		delete(s.pushHandleToToken, handle)
	}
	// 关闭 outbound 让 writer goroutine 退出；CloseHandle 在调用方完成。
	// shutdown() 多次调用安全（closeOnce）。
	if w != nil {
		w.shutdown()
	}
	// 注：focusedClients 是按 bridge clientID 索引的，与 push pipe 解耦。
	// 客户端整体断连时的 focus 清理放在 handleClient 的 defer（按 clientID 删），
	// 这里不再触碰，避免双管道生命周期错位导致的清理错乱。
	return true
}

// pushToActiveClient 是所有状态/配置 push 的统一入口：解析当前 active client
// 并把消息丢到该 client 的 outbound 队列。
//
// 为什么不广播给所有 push client？
//   - C++ TSF DLL 注入到每个 TSF 宿主，宿主进程不退出就一直占用 push 连接；
//     长期运行下连接数可达 20+。
//   - 但 English pair / Stats 配置以及状态（中英模式、全半角等）只在该 TSF
//     实例**处理用户按键**时才用到，背景实例缓存的值用不到。
//   - HandleFocusGained / HandleIMEActivated 会在焦点切到背景实例时主动调
//     push（此时它正好成为 active），刚好赶在第一次按键之前——天然的
//     "焦点切换补推"语义。
//
// 定位策略与 PushCommitTextToActiveClient 一致：优先 token（多实例宿主精确
// 区分），回退 PID（旧 token 还没注册时兜底）。
func (s *Server) pushToActiveClient(encoded []byte, kind string) {
	writer, activeProcessID, activeToken := s.resolveActivePushClient()
	if activeProcessID == 0 && activeToken == 0 {
		s.logger.Debug("Push skipped: no active client", "kind", kind)
		return
	}
	if writer == nil {
		s.logger.Debug("Push skipped: active client has no push pipe",
			"kind", kind, "processID", activeProcessID, "token", activeToken)
		return
	}

	if !writer.enqueueBroadcast(encoded) {
		s.logger.Warn("Push dropped: active client queue full",
			"kind", kind, "processID", activeProcessID)
		return
	}
	// 诊断：状态/配置推送是否真的进了队列。配合 pushWriterLoop 的写入日志可以
	// 看到完整的 enqueue → WriteFile → C++ 收到 的链路在哪一环掉链子。
	s.logger.Debug("Push enqueued",
		"kind", kind, "processID", activeProcessID, "size", len(encoded), "queueLen", len(writer.outbound))
}

// resolveActivePushClient 解析当前 active client 对应的 push 连接，
// 返回 (连接, activeProcessID, activeToken)；找不到时连接为 nil。
//
// 三段策略（所有按 active client 定位的 push 路径共用，含 CommitText/
// ClearComposition/UpdateComposition——多实例宿主下必须 token 精确区分，
// 否则可能误投到同进程的其他 DLL 实例造成重复上屏）：
//
//	Phase 1: token 精确匹配且 PID 一致 — 单进程多 DLL 实例场景下走这里。
//	  注意要校验 PID, 否则 activeToken 可能是上一次 IMEActivated 残留的
//	  (FOCUS_GAINED 不更新 token 是为兼容 EverEdit 双进程, 见 server_handler.go)。
//	Phase 2: PID 查表 — 正常单进程场景, 覆盖 activeToken 卡死的情况。
//	Phase 3: token 兜底 — EverEdit 双进程场景, A 进程 (activeProcessID) 没
//	  push pipe, B 进程持 push pipe 并经 IMEActivated 注册 token, 走 token 才能找到 B。
func (s *Server) resolveActivePushClient() (*pushClient, uint32, uint64) {
	s.activeMu.RLock()
	activeProcessID := s.activeProcessID
	activeToken := s.activeToken
	s.activeMu.RUnlock()

	if activeProcessID == 0 && activeToken == 0 {
		return nil, 0, 0
	}

	s.pushMu.RLock()
	defer s.pushMu.RUnlock()
	var writer *pushClient
	if activeToken != 0 {
		if h, ok := s.tokenToPushHandle[activeToken]; ok {
			if s.pushHandleToPID[h] == activeProcessID {
				writer = s.pushClients[h]
			}
		}
	}
	if writer == nil && activeProcessID != 0 {
		if h, ok := s.pushClientsByPID[activeProcessID]; ok {
			writer = s.pushClients[h]
		}
	}
	if writer == nil && activeToken != 0 {
		if h, ok := s.tokenToPushHandle[activeToken]; ok {
			writer = s.pushClients[h]
		}
	}
	return writer, activeProcessID, activeToken
}

// PushStateToActiveClient sends a state update to the currently active TSF client.
// 用于焦点不变时的状态变化（如点击工具栏切换中英模式）。背景 client 不需要——
// 它们处理不到按键，状态不会被使用；下次焦点切到它们时 HandleFocusGained
// 的响应自带最新 status，C++ 侧从那里同步。
func (s *Server) PushStateToActiveClient(status *StatusUpdateData) {
	if status == nil {
		return
	}
	s.pushToActiveClient(s.encodeStatePush(status), "state")
}

// encodeStatePush encodes a state push message (CMD_STATE_PUSH)
func (s *Server) encodeStatePush(status *StatusUpdateData) []byte {
	return s.codec.EncodeStatePush(
		status.ChineseMode,
		status.FullWidth,
		status.ChinesePunctuation,
		status.ToolbarVisible,
		status.CapsLock,
		status.IconLabel,
	)
}

// PushActivationStatusToActiveClient pushes a full activation status to the active TSF client.
//
// IMEActivated / FocusGained 异步化后的状态回包通道。bridge handler 在收到原同步命令时
// 立即 EncodeAck() 返回，HandleIMEActivated / HandleFocusGained 在 goroutine 中执行，
// 完成后调用本方法把完整状态（含 hotkeys 与 hostRenderAvail）推到 active client；
// C++ 端 AsyncReader 看到 CmdActivationStatusPush 后 Post 到 TSF 线程完成 mirror 同步。
//
// 与 PushStateToActiveClient 的区别：本函数走 CmdActivationStatusPush 命令、载荷
// 含 hotkeys + hostRenderAvail，是 activation 握手的等价物；后者走 CmdStatePush，
// 用于"焦点不变、仅状态变化"场景（hotkeys 不变所以不带）。
//
// processID 必须传入：用于查 hostRender 白名单决定 hostRenderAvail flag。
func (s *Server) PushActivationStatusToActiveClient(status *StatusUpdateData, processID uint32) {
	if status == nil {
		return
	}
	hostRenderAvail := false
	if s.hostRender != nil && processID != 0 {
		hostRenderAvail = s.hostRender.IsProcessWhitelisted(processID)
	}
	encoded := s.codec.EncodeActivationStatusPush(
		status.ChineseMode,
		status.FullWidth,
		status.ChinesePunctuation,
		status.ToolbarVisible,
		status.CapsLock,
		hostRenderAvail,
		status.KeyDownHotkeys,
		status.KeyUpHotkeys,
		status.IconLabel,
	)
	s.pushToActiveClient(encoded, "activation")
}

// PushCommitTextToActiveClient sends a commit text command to the active TSF client only
// This is used for proactive text insertion (e.g., when user clicks a candidate with mouse)
// For security, we only send to the client that currently has focus, not to all clients
//
// 经 outbound 队列发出（不直写）：与状态推送共用单 writer goroutine，保证同
// client 上的消息顺序一致，且调用方（coordinator 鼠标选词路径）不会被慢连接
// 阻塞。写失败的连接清理由 pushWriterLoop → reader cleanup 统一完成。
func (s *Server) PushCommitTextToActiveClient(text string) {
	if text == "" {
		s.logger.Debug("PushCommitText: empty text, skipping")
		return
	}

	// 必须精确定位持有活跃 composition 的 TextService 实例（token 优先，详见
	// resolveActivePushClient）；不能广播给同 PID 所有 handle，否则多实例宿主
	// （如 explorer）会重复上屏。
	writer, activeProcessID, activeToken := s.resolveActivePushClient()
	if activeProcessID == 0 && activeToken == 0 {
		s.logger.Warn("PushCommitText: no active client recorded, cannot send")
		return
	}

	// Encode the commit text message using CMD_COMMIT_TEXT
	encoded := s.codec.EncodeCommitText(text, "", false, false, false)

	if writer != nil {
		if !writer.enqueueBroadcast(encoded) {
			s.logger.Warn("PushCommitText dropped: active client queue full",
				"processID", activeProcessID)
			return
		}
		s.logger.Debug("Commit text enqueued to active TSF client",
			"processID", activeProcessID, "token", activeToken)
		return
	}

	// Fallback: active process has no push pipe connection.
	// Try to find a single push pipe client as fallback (safe when only one client is connected).
	// Do NOT broadcast to all clients — that causes duplicate text insertion.
	s.pushMu.RLock()
	clientCount := len(s.pushClients)
	var fallbackWriter *pushClient
	if clientCount == 1 {
		for _, w := range s.pushClients {
			fallbackWriter = w
		}
	}
	s.pushMu.RUnlock()

	if clientCount == 1 && fallbackWriter != nil {
		s.logger.Warn("PushCommitText: no push pipe for active process, using single-client fallback",
			"activeProcessID", activeProcessID)
		if !fallbackWriter.enqueueBroadcast(encoded) {
			s.logger.Warn("PushCommitText dropped: fallback client queue full")
		}
		return
	}

	s.logger.Warn("PushCommitText: no push pipe for active process, skipping to avoid duplicate insertion",
		"activeProcessID", activeProcessID, "pushClientCount", clientCount)
}

// PushClearCompositionToActiveClient sends a clear composition command to the active TSF client
// This is used when mode is toggled via menu/toolbar while there's an active composition
//
// 经 outbound 队列发出（不直写），与状态推送保序；定位策略与 CommitText 统一
// 走 resolveActivePushClient（token 优先，多实例宿主精确区分）。
func (s *Server) PushClearCompositionToActiveClient() {
	writer, activeProcessID, activeToken := s.resolveActivePushClient()
	if activeProcessID == 0 && activeToken == 0 {
		s.logger.Debug("PushClearComposition: no active client recorded, skipping")
		return
	}
	if writer == nil {
		s.logger.Debug("PushClearComposition: no push pipe for active process",
			"activeProcessID", activeProcessID)
		return
	}

	if !writer.enqueueBroadcast(s.codec.EncodeClearComposition()) {
		s.logger.Warn("PushClearComposition dropped: active client queue full",
			"processID", activeProcessID)
		return
	}
	s.logger.Debug("Clear composition enqueued to active client", "processID", activeProcessID)
}

// PushUpdateCompositionToActiveClient sends an update composition command to the active TSF client
// This is used for mouse click partial confirm in pinyin mode
//
// 经 outbound 队列发出（不直写），与状态推送保序；定位策略与 CommitText 统一。
func (s *Server) PushUpdateCompositionToActiveClient(text string, caretPos int) {
	writer, activeProcessID, activeToken := s.resolveActivePushClient()
	if activeProcessID == 0 && activeToken == 0 {
		s.logger.Debug("PushUpdateComposition: no active client recorded, skipping")
		return
	}
	if writer == nil {
		s.logger.Debug("PushUpdateComposition: no push pipe for active process",
			"activeProcessID", activeProcessID)
		return
	}

	if !writer.enqueueBroadcast(s.codec.EncodeUpdateComposition(text, caretPos)) {
		s.logger.Warn("PushUpdateComposition dropped: active client queue full",
			"processID", activeProcessID)
		return
	}
	s.logger.Debug("Update composition enqueued to active client", "processID", activeProcessID)
}

// pushSyncConfigToActiveClient pushes a SyncConfig message to the active TSF client only.
// C++ 侧每个 CKeyEventSink 本地缓存配置，背景实例缓存值用不到；焦点切到背景
// 实例时 HandleFocusGained 会再补推一次（参见 [[pushToActiveClient]] 注释）。
func (s *Server) pushSyncConfigToActiveClient(key string, value []byte, logName string) {
	s.pushToActiveClient(s.codec.EncodeSyncConfig(key, value), logName)
}

// PushEnglishPairConfigToActiveClient pushes English auto-pair config to the active TSF client.
func (s *Server) PushEnglishPairConfigToActiveClient(enabled bool, pairs []string) {
	value := ipc.EncodeEnglishPairsValue(enabled, pairs)
	s.pushSyncConfigToActiveClient(ipc.ConfigKeyEnglishPairs, value, "English pair config")
}

// PushStatsConfigToActiveClient pushes input stats config to the active TSF client.
func (s *Server) PushStatsConfigToActiveClient(enabled bool, trackEnglish bool) {
	value := []byte{0, 0}
	if enabled {
		value[0] = 1
	}
	if trackEnglish {
		value[1] = 1
	}
	s.pushSyncConfigToActiveClient(ipc.ConfigKeyStats, value, "stats config")
}

// GetActiveClientCount returns the number of active TSF clients
func (s *Server) GetActiveClientCount() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.activeConns)
}

// RestartService disconnects all clients to force reconnection
// This can be used when the input method is in an abnormal state
func (s *Server) RestartService() {
	s.logger.Info("RestartService: Disconnecting all clients to force reconnection")

	// Close all push pipe clients and clear all mappings
	s.pushMu.Lock()
	pushClientCount := len(s.pushClients)
	for _, w := range s.pushClients {
		if w != nil {
			w.shutdown() // shutdown 内含 Disconnect + Close + 关 outbound
		}
	}
	// 重置所有 map（比逐条 delete 更高效）
	s.pushClients = make(map[windows.Handle]*pushClient)
	s.pushHandleToPID = make(map[windows.Handle]uint32)
	s.pushClientsByPID = make(map[uint32]windows.Handle)
	s.tokenToPushHandle = make(map[uint64]windows.Handle)
	s.pushHandleToToken = make(map[windows.Handle]uint64)
	s.pushMu.Unlock()

	// Clear active process ID
	s.activeMu.Lock()
	s.activeProcessID = 0
	s.activeMu.Unlock()

	// Close all request-response clients
	s.mu.Lock()
	reqClientCount := len(s.activeConns)
	for c := range s.activeConns {
		_ = c.Close()
		delete(s.activeConns, c)
	}
	s.mu.Unlock()

	s.logger.Info("RestartService: All clients disconnected",
		"pushClients", pushClientCount,
		"requestClients", reqClientCount)
}
