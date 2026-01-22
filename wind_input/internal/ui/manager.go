package ui

import (
	"log/slog"
	"sync"
	"time"
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
}

// NewManager creates a new UI manager
func NewManager(logger *slog.Logger) *Manager {
	return &Manager{
		window:   NewCandidateWindow(logger),
		renderer: NewRenderer(DefaultRenderConfig()),
		logger:   logger,
		readyCh:  make(chan struct{}),
		cmdCh:    make(chan UICommand, 100), // Buffered channel to avoid blocking IPC
	}
}

// Start starts the UI manager (creates window and runs message loop)
// This should be called from a dedicated goroutine
func (m *Manager) Start() error {
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

	// Start command processor in a separate goroutine
	go m.processCommands()

	// Run message loop (blocking)
	m.window.Run()

	return nil
}

// processCommands processes UI commands from the channel
func (m *Manager) processCommands() {
	m.logger.Debug("UI command processor started")

	for cmd := range m.cmdCh {
		m.logger.Debug("Processing UI command", "type", cmd.Type)

		// Recover from any panics to keep the goroutine alive
		func() {
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
		}()
	}

	m.logger.Info("UI command processor stopped")
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
	default:
		// Channel full - try to drain old hide commands and retry
		m.drainHideCommands()
		select {
		case m.cmdCh <- UICommand{Type: "hide"}:
		default:
			m.logger.Warn("UI command channel full, dropping hide command")
		}
	}
}

// doHide actually hides the window (called from UI thread)
func (m *Manager) doHide() {
	m.window.Hide()
}

// drainHideCommands removes redundant hide commands from channel to make room
func (m *Manager) drainHideCommands() {
	for {
		select {
		case cmd := <-m.cmdCh:
			// Put back non-hide commands
			if cmd.Type != "hide" {
				select {
				case m.cmdCh <- cmd:
				default:
					// Channel still full, give up
					return
				}
			}
			// Hide commands are discarded (we'll add a new one)
		default:
			// Channel empty or no more commands to drain
			return
		}
	}
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
