// Package state provides unified state management for the input method
package state

import (
	"log/slog"
	"sync"
)

// IMEState represents the current state of the input method
type IMEState struct {
	ChineseMode        bool // true = Chinese, false = English
	FullWidth          bool // true = full-width, false = half-width
	ChinesePunctuation bool // true = Chinese punctuation, false = English punctuation
	CapsLock           bool // true = CapsLock is ON
	ToolbarVisible     bool // true = toolbar is visible
	IMEActivated       bool // true = IME has focus
}

// StateChangeType indicates which state changed
type StateChangeType int

const (
	StateChangeMode StateChangeType = 1 << iota
	StateChangeFullWidth
	StateChangePunctuation
	StateChangeCapsLock
	StateChangeToolbar
	StateChangeActivation
	StateChangeAll = StateChangeMode | StateChangeFullWidth | StateChangePunctuation | StateChangeCapsLock | StateChangeToolbar | StateChangeActivation
)

// StateListener is called when state changes
// changeType indicates which states changed (can be combined with OR)
type StateListener func(newState IMEState, changeType StateChangeType)

// Manager manages IME state and notifies listeners of changes
type Manager struct {
	mu        sync.RWMutex
	state     IMEState
	listeners []StateListener
	logger    *slog.Logger
}

// NewManager creates a new state manager with default state
func NewManager(logger *slog.Logger) *Manager {
	return &Manager{
		state: IMEState{
			ChineseMode:        true,
			FullWidth:          false,
			ChinesePunctuation: true,
			CapsLock:           false,
			ToolbarVisible:     false,
			IMEActivated:       false,
		},
		listeners: make([]StateListener, 0),
		logger:    logger,
	}
}

// AddListener adds a state change listener
func (m *Manager) AddListener(listener StateListener) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.listeners = append(m.listeners, listener)
}

// GetState returns a copy of the current state
func (m *Manager) GetState() IMEState {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.state
}

// SetState sets the entire state and notifies listeners
func (m *Manager) SetState(newState IMEState) {
	m.mu.Lock()
	oldState := m.state
	m.state = newState

	// Calculate what changed
	var changeType StateChangeType
	if oldState.ChineseMode != newState.ChineseMode {
		changeType |= StateChangeMode
	}
	if oldState.FullWidth != newState.FullWidth {
		changeType |= StateChangeFullWidth
	}
	if oldState.ChinesePunctuation != newState.ChinesePunctuation {
		changeType |= StateChangePunctuation
	}
	if oldState.CapsLock != newState.CapsLock {
		changeType |= StateChangeCapsLock
	}
	if oldState.ToolbarVisible != newState.ToolbarVisible {
		changeType |= StateChangeToolbar
	}
	if oldState.IMEActivated != newState.IMEActivated {
		changeType |= StateChangeActivation
	}

	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	// Notify listeners if anything changed
	if changeType != 0 {
		m.logger.Debug("State changed",
			"changeType", changeType,
			"chineseMode", newState.ChineseMode,
			"fullWidth", newState.FullWidth,
			"chinesePunct", newState.ChinesePunctuation,
			"capsLock", newState.CapsLock,
			"toolbarVisible", newState.ToolbarVisible,
			"imeActivated", newState.IMEActivated,
		)
		for _, listener := range listeners {
			listener(newState, changeType)
		}
	}
}

// SetChineseMode sets the Chinese/English mode
func (m *Manager) SetChineseMode(chineseMode bool) {
	m.mu.Lock()
	if m.state.ChineseMode == chineseMode {
		m.mu.Unlock()
		return
	}
	m.state.ChineseMode = chineseMode
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("Chinese mode changed", "chineseMode", chineseMode)
	for _, listener := range listeners {
		listener(state, StateChangeMode)
	}
}

// SetFullWidth sets the full-width mode
func (m *Manager) SetFullWidth(fullWidth bool) {
	m.mu.Lock()
	if m.state.FullWidth == fullWidth {
		m.mu.Unlock()
		return
	}
	m.state.FullWidth = fullWidth
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("Full-width mode changed", "fullWidth", fullWidth)
	for _, listener := range listeners {
		listener(state, StateChangeFullWidth)
	}
}

// SetChinesePunctuation sets the Chinese punctuation mode
func (m *Manager) SetChinesePunctuation(chinesePunct bool) {
	m.mu.Lock()
	if m.state.ChinesePunctuation == chinesePunct {
		m.mu.Unlock()
		return
	}
	m.state.ChinesePunctuation = chinesePunct
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("Chinese punctuation changed", "chinesePunct", chinesePunct)
	for _, listener := range listeners {
		listener(state, StateChangePunctuation)
	}
}

// SetCapsLock sets the CapsLock state
func (m *Manager) SetCapsLock(capsLock bool) {
	m.mu.Lock()
	if m.state.CapsLock == capsLock {
		m.mu.Unlock()
		return
	}
	m.state.CapsLock = capsLock
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("CapsLock state changed", "capsLock", capsLock)
	for _, listener := range listeners {
		listener(state, StateChangeCapsLock)
	}
}

// SetToolbarVisible sets the toolbar visibility
func (m *Manager) SetToolbarVisible(visible bool) {
	m.mu.Lock()
	if m.state.ToolbarVisible == visible {
		m.mu.Unlock()
		return
	}
	m.state.ToolbarVisible = visible
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("Toolbar visibility changed", "visible", visible)
	for _, listener := range listeners {
		listener(state, StateChangeToolbar)
	}
}

// SetIMEActivated sets the IME activation state
func (m *Manager) SetIMEActivated(activated bool) {
	m.mu.Lock()
	if m.state.IMEActivated == activated {
		m.mu.Unlock()
		return
	}
	m.state.IMEActivated = activated
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("IME activation changed", "activated", activated)
	for _, listener := range listeners {
		listener(state, StateChangeActivation)
	}
}

// ToggleChineseMode toggles the Chinese/English mode and returns the new state
func (m *Manager) ToggleChineseMode() bool {
	m.mu.Lock()
	m.state.ChineseMode = !m.state.ChineseMode
	newMode := m.state.ChineseMode
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("Chinese mode toggled", "chineseMode", newMode)
	for _, listener := range listeners {
		listener(state, StateChangeMode)
	}
	return newMode
}

// ToggleFullWidth toggles the full-width mode and returns the new state
func (m *Manager) ToggleFullWidth() bool {
	m.mu.Lock()
	m.state.FullWidth = !m.state.FullWidth
	newMode := m.state.FullWidth
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("Full-width mode toggled", "fullWidth", newMode)
	for _, listener := range listeners {
		listener(state, StateChangeFullWidth)
	}
	return newMode
}

// ToggleChinesePunctuation toggles the Chinese punctuation mode and returns the new state
func (m *Manager) ToggleChinesePunctuation() bool {
	m.mu.Lock()
	m.state.ChinesePunctuation = !m.state.ChinesePunctuation
	newMode := m.state.ChinesePunctuation
	state := m.state
	listeners := make([]StateListener, len(m.listeners))
	copy(listeners, m.listeners)
	m.mu.Unlock()

	m.logger.Debug("Chinese punctuation toggled", "chinesePunct", newMode)
	for _, listener := range listeners {
		listener(state, StateChangePunctuation)
	}
	return newMode
}

// IsChineseMode returns the current Chinese mode state
func (m *Manager) IsChineseMode() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.state.ChineseMode
}

// IsFullWidth returns the current full-width state
func (m *Manager) IsFullWidth() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.state.FullWidth
}

// IsChinesePunctuation returns the current Chinese punctuation state
func (m *Manager) IsChinesePunctuation() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.state.ChinesePunctuation
}

// IsCapsLock returns the current CapsLock state
func (m *Manager) IsCapsLock() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.state.CapsLock
}

// IsToolbarVisible returns the current toolbar visibility
func (m *Manager) IsToolbarVisible() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.state.ToolbarVisible
}

// IsIMEActivated returns the current IME activation state
func (m *Manager) IsIMEActivated() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.state.IMEActivated
}
