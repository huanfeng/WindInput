package bridge

import (
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
			windows.PIPE_ACCESS_OUTBOUND, // Write-only for push
			windows.PIPE_TYPE_MESSAGE|windows.PIPE_WAIT,
			windows.PIPE_UNLIMITED_INSTANCES,
			PipeBufferSize,
			0, // No input buffer needed
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

		s.pushMu.Lock()
		s.pushClientCount++
		clientID := s.pushClientCount
		s.pushClients[handle] = writer
		// Store mapping from process ID to push pipe handle
		if pushProcessID != 0 {
			s.pushClientsByPID[pushProcessID] = handle
			s.pushHandleToPID[handle] = pushProcessID
		}
		s.pushMu.Unlock()

		s.logger.Info("Push pipe client connected", "clientID", clientID, "processID", pushProcessID)

		// Notify the newly-connected TSF client that the service is ready.
		// This triggers _DoFullStateSync() on the TSF thread so the toolbar
		// appears immediately after service restart, without waiting for a
		// focus-change or key event.
		encoded := s.codec.EncodeServiceReady()
		if err := s.codec.WriteMessage(writer, encoded); err != nil {
			s.logger.Warn("Failed to send CMD_SERVICE_READY to new push client",
				"clientID", clientID, "error", err)
		} else {
			s.logger.Debug("CMD_SERVICE_READY sent to new push client", "clientID", clientID)
		}

		// Note: We don't actively monitor disconnection here.
		// Client disconnection is detected when write fails in PushCommitTextToActiveClient
		// or PushStateToAllClients. This avoids false positives from GetNamedPipeHandleState
		// which can return "Access is denied" on valid pipes.
	}
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

	// Send to all clients
	var failedClients []clientInfo
	successCount := 0
	for _, client := range clients {
		if err := s.codec.WriteMessage(client.writer, encoded); err != nil {
			s.logger.Warn("Failed to push state to client", "processID", client.processID, "error", err)
			failedClients = append(failedClients, client)
		} else {
			successCount++
		}
	}

	// Remove failed clients
	if len(failedClients) > 0 {
		s.pushMu.Lock()
		for _, client := range failedClients {
			delete(s.pushClients, client.handle)
			delete(s.pushHandleToPID, client.handle)
			if client.processID != 0 {
				delete(s.pushClientsByPID, client.processID)
			}
			windows.CloseHandle(client.handle)
		}
		s.pushMu.Unlock()
	}

	s.logger.Debug("State push completed", "success", successCount, "total", clientCount)
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

	// Find the push pipe handle for the active process
	s.pushMu.RLock()
	handle, exists := s.pushClientsByPID[activeProcessID]
	var writer *pipeWriter
	if exists {
		writer = s.pushClients[handle]
	}
	s.pushMu.RUnlock()

	// Encode the commit text message using CMD_COMMIT_TEXT
	encoded := s.codec.EncodeCommitText(text, "", false, false, false)

	if exists && writer != nil {
		// Best case: send to the specific active client
		s.logger.Debug("Pushing commit text to active TSF client via push pipe",
			"processID", activeProcessID)

		if err := s.codec.WriteMessage(writer, encoded); err != nil {
			s.logger.Warn("Failed to push commit text to active client",
				"processID", activeProcessID, "error", err)

			// Remove the failed client
			s.pushMu.Lock()
			delete(s.pushClients, handle)
			delete(s.pushHandleToPID, handle)
			delete(s.pushClientsByPID, activeProcessID)
			s.pushMu.Unlock()
			windows.CloseHandle(handle)
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
			pid := s.pushHandleToPID[fallbackHandle]
			delete(s.pushClients, fallbackHandle)
			delete(s.pushHandleToPID, fallbackHandle)
			if pid != 0 {
				delete(s.pushClientsByPID, pid)
			}
			s.pushMu.Unlock()
			windows.CloseHandle(fallbackHandle)
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

		// Remove the failed client
		s.pushMu.Lock()
		delete(s.pushClients, handle)
		delete(s.pushHandleToPID, handle)
		delete(s.pushClientsByPID, activeProcessID)
		s.pushMu.Unlock()
		windows.CloseHandle(handle)
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

		// Remove the failed client
		s.pushMu.Lock()
		delete(s.pushClients, handle)
		delete(s.pushHandleToPID, handle)
		delete(s.pushClientsByPID, activeProcessID)
		s.pushMu.Unlock()
		windows.CloseHandle(handle)
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

	var failedHandles []windows.Handle
	for _, client := range clients {
		if err := s.codec.WriteMessage(client.writer, encoded); err != nil {
			s.logger.Debug("Failed to push config", "config", logName, "error", err)
			failedHandles = append(failedHandles, client.handle)
		}
	}

	if len(failedHandles) > 0 {
		s.pushMu.Lock()
		for _, h := range failedHandles {
			pid := s.pushHandleToPID[h]
			delete(s.pushClients, h)
			delete(s.pushHandleToPID, h)
			if pid != 0 {
				delete(s.pushClientsByPID, pid)
			}
			windows.CloseHandle(h)
		}
		s.pushMu.Unlock()
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

	// Close all push pipe clients and clear process ID mappings
	s.pushMu.Lock()
	pushClientCount := len(s.pushClients)
	for h := range s.pushClients {
		windows.CloseHandle(h)
		delete(s.pushClients, h)
		delete(s.pushHandleToPID, h)
	}
	// Clear all process ID mappings
	for pid := range s.pushClientsByPID {
		delete(s.pushClientsByPID, pid)
	}
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
