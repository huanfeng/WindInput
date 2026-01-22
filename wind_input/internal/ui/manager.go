package ui

import (
	"log/slog"
	"runtime"
	"sync"
	"time"

	"golang.org/x/sys/windows"
)

// UICommand represents a command to the UI thread
type UICommand struct {
	Type       string // "show", "hide", "mode"
	Candidates []Candidate
	Input      string
	X, Y       int
	Page       int
	TotalPages int
	ModeText   string
}

// Manager manages the candidate window UI
type Manager struct {
	window   *CandidateWindow
	renderer *Renderer
	logger   *slog.Logger

	mu         sync.Mutex
	candidates []Candidate
	input      string
	page       int
	totalPages int
	caretX     int
	caretY     int

	ready    bool
	readyCh  chan struct{}

	// Command channel for async UI updates
	cmdCh chan UICommand

	// Event to wake up the message loop when commands are available
	cmdEvent windows.Handle
}

// NewManager creates a new UI manager
func NewManager(logger *slog.Logger) *Manager {
	// Create event for waking up message loop
	event, err := CreateEvent()
	if err != nil {
		logger.Error("Failed to create event", "error", err)
	}

	return &Manager{
		window:   NewCandidateWindow(logger),
		renderer: NewRenderer(DefaultRenderConfig()),
		logger:   logger,
		readyCh:  make(chan struct{}),
		cmdCh:    make(chan UICommand, 100), // Buffered channel to avoid blocking IPC
		cmdEvent: event,
	}
}

// Start starts the UI manager (creates window and runs message loop)
// This should be called from a dedicated goroutine
func (m *Manager) Start() error {
	// Lock this goroutine to its OS thread for Windows GUI operations
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()

	m.logger.Info("Starting UI Manager...")

	// Create window
	if err := m.window.Create(); err != nil {
		return err
	}

	m.mu.Lock()
	m.ready = true
	m.mu.Unlock()
	close(m.readyCh)

	m.logger.Info("UI Manager ready")

	// Run combined message loop that handles both Windows messages and UI commands
	// This ensures all UI operations happen on the same thread that created the window
	m.runCombinedLoop()

	return nil
}

// runCombinedLoop runs a combined message loop that handles both Windows messages and UI commands
func (m *Manager) runCombinedLoop() {
	m.logger.Info("Starting combined message loop...")

	var msg MSG
	for {
		// Wait for either a Windows message or the command event
		ret := MsgWaitForMultipleObjects(m.cmdEvent, 50) // 50ms timeout for responsiveness

		switch {
		case ret == WAIT_OBJECT_0:
			// Command event signaled - process pending commands
			ResetEvent(m.cmdEvent)
			m.processPendingCommands()

		case ret == WAIT_OBJECT_0+1:
			// Windows message available - process all pending messages
			for PeekMessage(&msg) {
				if msg.Message == 0x0012 { // WM_QUIT
					m.logger.Info("Received WM_QUIT, exiting loop")
					return
				}
				ProcessMessage(&msg)
			}

		case ret == WAIT_TIMEOUT:
			// Timeout - check for any pending commands (in case event was missed)
			m.processPendingCommands()

		default:
			// Error or other return value
			m.logger.Debug("MsgWaitForMultipleObjects returned", "ret", ret)
		}
	}
}

// processPendingCommands processes all pending commands from the channel
func (m *Manager) processPendingCommands() {
	for {
		select {
		case cmd := <-m.cmdCh:
			m.processOneCommand(cmd)
		default:
			return // No more commands
		}
	}
}

// processOneCommand processes a single UI command
func (m *Manager) processOneCommand(cmd UICommand) {
	// Recover from any panics to keep the loop alive
	defer func() {
		if r := recover(); r != nil {
			m.logger.Error("Panic in UI command processing", "panic", r, "type", cmd.Type)
		}
	}()

	switch cmd.Type {
	case "show":
		m.doShowCandidates(cmd.Candidates, cmd.Input, cmd.X, cmd.Y, cmd.Page, cmd.TotalPages)
	case "hide":
		m.doHide()
	case "mode":
		m.doShowModeIndicator(cmd.ModeText, cmd.X, cmd.Y)
	}
}


// WaitReady waits until the UI manager is ready
func (m *Manager) WaitReady() {
	<-m.readyCh
}

// IsReady returns whether the UI manager is ready
func (m *Manager) IsReady() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.ready
}

// ShowCandidates shows candidates at the given position (async, non-blocking)
func (m *Manager) ShowCandidates(candidates []Candidate, input string, x, y, page, totalPages int) error {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return nil
	}
	m.candidates = candidates
	m.input = input
	m.page = page
	m.totalPages = totalPages
	m.caretX = x
	m.caretY = y
	m.mu.Unlock()

	m.logger.Debug("Queuing ShowCandidates", "input", input, "count", len(candidates), "x", x, "y", y)

	// Send command to UI thread (non-blocking due to buffered channel)
	select {
	case m.cmdCh <- UICommand{
		Type:       "show",
		Candidates: candidates,
		Input:      input,
		X:          x,
		Y:          y,
		Page:       page,
		TotalPages: totalPages,
	}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping show command")
	}

	return nil
}

// doShowCandidates actually shows candidates (called from UI thread)
func (m *Manager) doShowCandidates(candidates []Candidate, input string, x, y, page, totalPages int) {
	m.logger.Debug("doShowCandidates start", "input", input, "count", len(candidates), "x", x, "y", y)

	// Render
	m.logger.Debug("Rendering candidates...")
	img := m.renderer.RenderCandidates(candidates, input, page, totalPages)
	m.logger.Debug("Render complete", "width", img.Bounds().Dx(), "height", img.Bounds().Dy())

	// Update window
	m.logger.Debug("Updating window content...")
	if err := m.window.UpdateContent(img, x, y); err != nil {
		m.logger.Error("UpdateContent failed", "error", err)
		return
	}
	m.logger.Debug("Window content updated")

	// Show window
	m.logger.Debug("Showing window...")
	m.window.Show()
	m.logger.Debug("doShowCandidates complete")
}

// Hide hides the candidate window (async, non-blocking)
func (m *Manager) Hide() {
	// Skip if window is already hidden (avoid flooding channel with hide commands)
	if !m.window.IsVisible() {
		return
	}

	// Send command to UI thread (non-blocking)
	select {
	case m.cmdCh <- UICommand{Type: "hide"}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		// Channel full, but hide is not critical - window will be hidden eventually
		m.logger.Debug("UI command channel full, skipping redundant hide")
	}
}

// doHide actually hides the window (called from UI thread)
func (m *Manager) doHide() {
	m.window.Hide()
}

// UpdatePosition updates the window position
func (m *Manager) UpdatePosition(x, y int) {
	m.mu.Lock()
	m.caretX = x
	m.caretY = y
	m.mu.Unlock()

	m.window.SetPosition(x, y)
}

// Destroy destroys the UI manager
func (m *Manager) Destroy() {
	m.window.Destroy()
	if m.cmdEvent != 0 {
		CloseEvent(m.cmdEvent)
		m.cmdEvent = 0
	}
}

// IsVisible returns whether the window is visible
func (m *Manager) IsVisible() bool {
	return m.window.IsVisible()
}

// ShowModeIndicator shows a brief mode indicator (中/En) (async, non-blocking)
func (m *Manager) ShowModeIndicator(mode string, x, y int) {
	m.mu.Lock()
	if !m.ready {
		m.mu.Unlock()
		return
	}
	m.mu.Unlock()

	m.logger.Debug("Queuing ShowModeIndicator", "mode", mode)

	// Send command to UI thread (non-blocking)
	select {
	case m.cmdCh <- UICommand{
		Type:     "mode",
		ModeText: mode,
		X:        x,
		Y:        y,
	}:
		// Signal the event to wake up the message loop
		if m.cmdEvent != 0 {
			SetEvent(m.cmdEvent)
		}
	default:
		m.logger.Warn("UI command channel full, dropping mode command")
	}
}

// doShowModeIndicator actually shows the mode indicator (called from UI thread)
func (m *Manager) doShowModeIndicator(mode string, x, y int) {
	// Render mode indicator
	img := m.renderer.RenderModeIndicator(mode)

	// Update window
	if err := m.window.UpdateContent(img, x, y); err != nil {
		m.logger.Error("UpdateContent failed", "error", err)
		return
	}

	// Show window briefly
	m.window.Show()

	// Hide after a short delay (send through channel for thread safety)
	go func() {
		time.Sleep(800 * time.Millisecond)
		m.Hide() // Use public method which goes through channel
	}()
}
