//go:build !windows

package ui

import "errors"

// ActivateIME is a no-op on non-Windows platforms.
func ActivateIME() error {
	return errors.New("activate_ime: not supported on this platform")
}

// ActivateIME is a no-op on non-Windows platforms.
func (m *Manager) ActivateIME() {}
