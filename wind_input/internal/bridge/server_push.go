package bridge

import (
	"encoding/binary"
	"time"
	"unsafe"

	"github.com/huanfeng/wind_input/internal/ipc"
	"golang.org/x/sys/windows"
)

// startPushPipeListener starts the push pipe listener for state push
func (s *Server) startPushPipeListener() {
	s.logger.Info("Starting Push pipe listener", "pipe", PushPipeName)

	// Allow desktop clients plus AppContainer/modern hosts (e.g. Start menu search).
	// S:(ML;;NW;;;LW) = Mandatory Label: Low integrity — required for UWP/AppContainer
	sddl := "D:P(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)S:(ML;;NW;;;LW)"
	sd, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		s.logger.Error("Failed to create security descriptor for push pipe", "error", err)
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
		pipePath, err := windows.UTF16PtrFromString(PushPipeName)
		if err != nil {
			s.logger.Error("Failed to convert push pipe path", "error", err)
			time.Sleep(1 * time.Second)
			continue
		}

		handle, err := windows.CreateNamedPipe(
			pipePath,
			windows.PIPE_ACCESS_DUPLEX, // 双向：服务端写状态推送，客户端写 token 握手
			windows.PIPE_TYPE_MESSAGE|windows.PIPE_WAIT,
			windows.PIPE_UNLIMITED_INSTANCES,
			PipeBufferSize,
			16, // 输入缓冲：仅用于接收 4 字节 token 握手
			0,
			sa,
		)

		if err != nil {
			s.logger.Error("Failed to create push pipe", "error", err)
			time.Sleep(1 * time.Second)
			continue
		}

		s.logger.Debug("Waiting for push pipe connection...")

		err = windows.ConnectNamedPipe(handle, nil)
		if err != nil && err != windows.ERROR_PIPE_CONNECTED {
			windows.CloseHandle(handle)
			continue
		}

		writer := &pipeWriter{handle: handle}

		// Get the client's process ID for targeted push
		pushProcessID, err := getNamedPipeClientProcessId(handle)
		if err != nil {
			s.logger.Warn("Failed to get push pipe client process ID", "error", err)
			pushProcessID = 0
		}

		// 立即注册客户端并写 CMD_SERVICE_READY，不等待 token 握手。
		// token 在独立 goroutine 中异步读取，完成后再更新 tokenToPushHandle。
		// 这样主循环可以立刻回到 CreateNamedPipe 等待下一个客户端，
		// 避免 500ms 阻塞导致 EverEdit/Notepad 等应用在此窗口内连接失败。
		s.pushMu.Lock()
		s.pushClientCount++
		clientID := s.pushClientCount
		s.pushClients[handle] = writer
		if pushProcessID != 0 {
			s.pushClientsByPID[pushProcessID] = handle
			s.pushHandleToPID[handle] = pushProcessID
		}
		s.pushMu.Unlock()

		s.logger.Info("Push pipe client connected", "clientID", clientID, "processID", pushProcessID)

		// Notify the newly-connected TSF client that the service is ready.
		encoded := s.codec.EncodeServiceReady()
		if err := s.codec.WriteMessage(writer, encoded); err != nil {
			s.logger.Warn("Failed to send CMD_SERVICE_READY to new push client",
				"clientID", clientID, "error", err)
		} else {
			s.logger.Debug("CMD_SERVICE_READY sent to new push client", "clientID", clientID)
		}

		// 异步读取 token 握手（8 字节），不阻塞主循环。
		// 内层 goroutine 做实际 ReadFile（可能永久阻塞于旧版客户端），
		// 外层 goroutine 持有 500ms 超时并在超时后退出（内层 goroutine 在 handle 关闭时自然退出）。
		go func(h windows.Handle, pid uint32, cid int) {
			tokenCh := make(chan uint64, 1)
			go func() {
				var buf [8]byte
				var n uint32
				if err := windows.ReadFile(h, buf[:], &n, nil); err == nil && n == 8 {
					tokenCh <- binary.LittleEndian.Uint64(buf[:])
				}
			}()
			select {
			case token := <-tokenCh:
				if token == 0 {
					return
				}
				s.pushMu.Lock()
				if _, exists := s.pushClients[h]; exists {
					s.tokenToPushHandle[token] = h
					s.pushHandleToToken[h] = token
				}
				s.pushMu.Unlock()
				s.logger.Debug("Push pipe: token registered", "clientID", cid, "processID", pid, "token", token)
			case <-time.After(500 * time.Millisecond):
				s.logger.Debug("Push pipe: token handshake timed out (old client?)", "clientID", cid, "processID", pid)
			}
		}(handle, pushProcessID, clientID)

		// Note: We don't actively monitor disconnection here.
		// Client disconnection is detected when write fails in PushCommitTextToActiveClient
		// or PushStateToAllClients. This avoids false positives from GetNamedPipeHandleState
		// which can return "Access is denied" on valid pipes.
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
	if _, exists := s.pushClients[handle]; !exists {
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
	return true
}

// PushStateToAllClients broadcasts state update to all connected TSF clients
// This is used for proactive state push (e.g., when mode changes via toolbar click)
func (s *Server) PushStateToAllClients(status *StatusUpdateData) {
	if status == nil {
		return
	}

	// Encode the state push message using CMD_STATE_PUSH
	encoded := s.encodeStatePush(status)

	// Get all push clients with their process IDs
	s.pushMu.RLock()
	type clientInfo struct {
		handle    windows.Handle
		writer    *pipeWriter
		processID uint32
	}
	clients := make([]clientInfo, 0, len(s.pushClients))
	for h, writer := range s.pushClients {
		// 使用反向映射 O(1) 查找 PID
		pid := s.pushHandleToPID[h]
		clients = append(clients, clientInfo{handle: h, writer: writer, processID: pid})
	}
	clientCount := len(clients)
	s.pushMu.RUnlock()

	if clientCount == 0 {
		s.logger.Debug("No push pipe clients to send state to")
		return
	}

	s.logger.Debug("Pushing state to TSF clients via push pipe",
		"count", clientCount,
		"chineseMode", status.ChineseMode,
		"fullWidth", status.FullWidth,
		"capsLock", status.CapsLock)

	// 每个客户端独立 goroutine 写入，避免某个 client 的 pipe buffer 满/阻塞
	// 导致后续 client（如 Notepad 第二个 CLangBar 实例）永远收不到推送。
	// Go map 随机迭代顺序会使阻塞点前后的 client 每次不同，造成状态同步时好时坏。
	for _, client := range clients {
		c := client
		go func() {
			if err := s.codec.WriteMessage(c.writer, encoded); err != nil {
				s.logger.Warn("Failed to push state to client", "processID", c.processID, "error", err)
				s.pushMu.Lock()
				removed := s.cleanupPushHandle(c.handle)
				s.pushMu.Unlock()
				if removed {
					windows.CloseHandle(c.handle)
				}
			}
		}()
	}
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

// PushCommitTextToActiveClient sends a commit text command to the active TSF client only
// This is used for proactive text insertion (e.g., when user clicks a candidate with mouse)
// For security, we only send to the client that currently has focus, not to all clients
func (s *Server) PushCommitTextToActiveClient(text string) {
	if text == "" {
		s.logger.Debug("PushCommitText: empty text, skipping")
		return
	}

	// Get the active process ID
	s.activeMu.RLock()
	activeProcessID := s.activeProcessID
	s.activeMu.RUnlock()

	if activeProcessID == 0 {
		s.logger.Warn("PushCommitText: no active client recorded, cannot send")
		return
	}

	// 对于 CommitText，必须精确定位持有活跃 composition 的 TextService 实例：
	// 1. 优先用 activeToken（C++ 在 CMD_IME_ACTIVATED/CMD_FOCUS_GAINED 中携带）
	// 2. 回退到 pushClientsByPID（最新连接的 handle，适用于单实例进程）
	// 不能广播给同 PID 所有 handle，否则多实例宿主（如 explorer）会重复上屏。
	s.activeMu.RLock()
	activeToken := s.activeToken
	s.activeMu.RUnlock()

	s.pushMu.RLock()
	var handle windows.Handle
	var writer *pipeWriter
	// Primary: token-based exact targeting
	if activeToken != 0 {
		if h, ok := s.tokenToPushHandle[activeToken]; ok {
			if w := s.pushClients[h]; w != nil {
				handle, writer = h, w
			}
		}
	}
	// Fallback: PID-based (token not yet registered or handle already cleaned)
	if writer == nil && activeProcessID != 0 {
		if h, ok := s.pushClientsByPID[activeProcessID]; ok {
			if w := s.pushClients[h]; w != nil {
				handle, writer = h, w
			}
		}
	}
	s.pushMu.RUnlock()

	// Encode the commit text message using CMD_COMMIT_TEXT
	encoded := s.codec.EncodeCommitText(text, "", false, false, false)

	if writer != nil {
		s.logger.Debug("Pushing commit text to active TSF client via push pipe",
			"processID", activeProcessID, "token", activeToken)

		if err := s.codec.WriteMessage(writer, encoded); err != nil {
			s.logger.Warn("Failed to push commit text to active client",
				"processID", activeProcessID, "error", err)
			s.pushMu.Lock()
			removed := s.cleanupPushHandle(handle)
			s.pushMu.Unlock()
			if removed {
				windows.CloseHandle(handle)
			}
			return
		}

		s.logger.Info("Commit text push completed to active client", "processID", activeProcessID)
		return
	}

	// Fallback: active process has no push pipe connection.
	// Try to find a single push pipe client as fallback (safe when only one client is connected).
	// Do NOT broadcast to all clients — that causes duplicate text insertion.
	s.pushMu.RLock()
	clientCount := len(s.pushClients)
	var fallbackHandle windows.Handle
	var fallbackWriter *pipeWriter
	if clientCount == 1 {
		for h, w := range s.pushClients {
			fallbackHandle = h
			fallbackWriter = w
		}
	}
	s.pushMu.RUnlock()

	if clientCount == 1 && fallbackWriter != nil {
		s.logger.Warn("PushCommitText: no push pipe for active process, using single-client fallback",
			"activeProcessID", activeProcessID)
		if err := s.codec.WriteMessage(fallbackWriter, encoded); err != nil {
			s.logger.Warn("Failed to push commit text via fallback", "error", err)
			s.pushMu.Lock()
			removed := s.cleanupPushHandle(fallbackHandle)
			s.pushMu.Unlock()
			if removed {
				windows.CloseHandle(fallbackHandle)
			}
		} else {
			s.logger.Info("Commit text push completed via single-client fallback")
		}
		return
	}

	s.logger.Warn("PushCommitText: no push pipe for active process, skipping to avoid duplicate insertion",
		"activeProcessID", activeProcessID, "pushClientCount", clientCount)
}

// PushClearCompositionToActiveClient sends a clear composition command to the active TSF client
// This is used when mode is toggled via menu/toolbar while there's an active composition
func (s *Server) PushClearCompositionToActiveClient() {
	// Get the active process ID
	s.activeMu.RLock()
	activeProcessID := s.activeProcessID
	s.activeMu.RUnlock()

	if activeProcessID == 0 {
		s.logger.Debug("PushClearComposition: no active client recorded, skipping")
		return
	}

	// Find the push pipe handle for the active process
	s.pushMu.RLock()
	handle, exists := s.pushClientsByPID[activeProcessID]
	var writer *pipeWriter
	if exists {
		writer = s.pushClients[handle]
	}
	s.pushMu.RUnlock()

	if !exists || writer == nil {
		s.logger.Debug("PushClearComposition: no push pipe for active process",
			"activeProcessID", activeProcessID)
		return
	}

	// Encode the clear composition message
	encoded := s.codec.EncodeClearComposition()

	s.logger.Debug("Pushing clear composition to active TSF client via push pipe",
		"processID", activeProcessID)

	// Send to the active client only
	if err := s.codec.WriteMessage(writer, encoded); err != nil {
		s.logger.Warn("Failed to push clear composition to active client",
			"processID", activeProcessID, "error", err)
		s.pushMu.Lock()
		removed := s.cleanupPushHandle(handle)
		s.pushMu.Unlock()
		if removed {
			windows.CloseHandle(handle)
		}
		return
	}

	s.logger.Debug("Clear composition push completed to active client", "processID", activeProcessID)
}

// PushUpdateCompositionToActiveClient sends an update composition command to the active TSF client
// This is used for mouse click partial confirm in pinyin mode
func (s *Server) PushUpdateCompositionToActiveClient(text string, caretPos int) {
	// Get the active process ID
	s.activeMu.RLock()
	activeProcessID := s.activeProcessID
	s.activeMu.RUnlock()

	if activeProcessID == 0 {
		s.logger.Debug("PushUpdateComposition: no active client recorded, skipping")
		return
	}

	// Find the push pipe handle for the active process
	s.pushMu.RLock()
	handle, exists := s.pushClientsByPID[activeProcessID]
	var writer *pipeWriter
	if exists {
		writer = s.pushClients[handle]
	}
	s.pushMu.RUnlock()

	if !exists || writer == nil {
		s.logger.Debug("PushUpdateComposition: no push pipe for active process",
			"activeProcessID", activeProcessID)
		return
	}

	// Encode the update composition message
	encoded := s.codec.EncodeUpdateComposition(text, caretPos)

	s.logger.Debug("Pushing update composition to active TSF client via push pipe",
		"processID", activeProcessID)

	if err := s.codec.WriteMessage(writer, encoded); err != nil {
		s.logger.Warn("Failed to push update composition to active client",
			"processID", activeProcessID, "error", err)
		s.pushMu.Lock()
		removed := s.cleanupPushHandle(handle)
		s.pushMu.Unlock()
		if removed {
			windows.CloseHandle(handle)
		}
		return
	}

	s.logger.Debug("Update composition push completed to active client", "processID", activeProcessID)
}

func (s *Server) pushSyncConfigToAllClients(key string, value []byte, logName string) {
	encoded := s.codec.EncodeSyncConfig(key, value)
	s.pushMu.RLock()
	type clientInfo struct {
		handle windows.Handle
		writer *pipeWriter
	}
	clients := make([]clientInfo, 0, len(s.pushClients))
	for h, w := range s.pushClients {
		clients = append(clients, clientInfo{handle: h, writer: w})
	}
	s.pushMu.RUnlock()

	if len(clients) == 0 {
		s.logger.Debug("No push pipe clients to send config to", "config", logName)
		return
	}

	for _, client := range clients {
		c := client
		go func() {
			if err := s.codec.WriteMessage(c.writer, encoded); err != nil {
				s.logger.Debug("Failed to push config", "config", logName, "error", err)
				s.pushMu.Lock()
				removed := s.cleanupPushHandle(c.handle)
				s.pushMu.Unlock()
				if removed {
					windows.CloseHandle(c.handle)
				}
			}
		}()
	}
}

// PushEnglishPairConfigToAllClients pushes English auto-pair config to all TSF clients
func (s *Server) PushEnglishPairConfigToAllClients(enabled bool, pairs []string) {
	value := ipc.EncodeEnglishPairsValue(enabled, pairs)
	s.pushSyncConfigToAllClients(ipc.ConfigKeyEnglishPairs, value, "English pair config")
}

// PushStatsConfigToAllClients pushes input stats config to all TSF clients.
func (s *Server) PushStatsConfigToAllClients(enabled bool, trackEnglish bool) {
	value := []byte{0, 0}
	if enabled {
		value[0] = 1
	}
	if trackEnglish {
		value[1] = 1
	}
	s.pushSyncConfigToAllClients(ipc.ConfigKeyStats, value, "stats config")
}

// GetActiveClientCount returns the number of active TSF clients
func (s *Server) GetActiveClientCount() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.activeHandles)
}

// RestartService disconnects all clients to force reconnection
// This can be used when the input method is in an abnormal state
func (s *Server) RestartService() {
	s.logger.Info("RestartService: Disconnecting all clients to force reconnection")

	// Close all push pipe clients and clear all mappings
	s.pushMu.Lock()
	pushClientCount := len(s.pushClients)
	for h := range s.pushClients {
		windows.CloseHandle(h)
	}
	// 重置所有 map（比逐条 delete 更高效）
	s.pushClients = make(map[windows.Handle]*pipeWriter)
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
	reqClientCount := len(s.activeHandles)
	for h := range s.activeHandles {
		windows.CloseHandle(h)
		delete(s.activeHandles, h)
	}
	s.mu.Unlock()

	s.logger.Info("RestartService: All clients disconnected",
		"pushClients", pushClientCount,
		"requestClients", reqClientCount)
}
