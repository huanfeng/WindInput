//go:build windows

package main

import (
	"testing"

	"golang.org/x/sys/windows/registry"
)

func TestRegisterProtocolAt(t *testing.T) {
	const testKey = `Software\Classes\windinput_unittest`
	exe := `C:\Test\wind_setting.exe`
	t.Cleanup(func() { _ = unregisterProtocolAt(registry.CURRENT_USER, testKey) })

	if err := registerProtocolAt(registry.CURRENT_USER, testKey, exe); err != nil {
		t.Fatalf("register: %v", err)
	}
	ok, cmd := protocolStatusAt(registry.CURRENT_USER, testKey)
	if !ok {
		t.Fatal("status should be registered")
	}
	want := `"` + exe + `" "%1"`
	if cmd != want {
		t.Errorf("cmd = %q, want %q", cmd, want)
	}
	if err := unregisterProtocolAt(registry.CURRENT_USER, testKey); err != nil {
		t.Fatalf("unregister: %v", err)
	}
	if ok, _ := protocolStatusAt(registry.CURRENT_USER, testKey); ok {
		t.Error("should be unregistered after unregister")
	}
}
