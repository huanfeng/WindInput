//go:build windows

package main

import (
	"log/slog"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ui"
)

// startPlatformForwarder windows no-op: Win 端候选框走 LayeredWindow 直绘,
// 不通过 bridge push + SHM。
func startPlatformForwarder(srv *bridge.Server, mgr *ui.Manager,
	hrm *bridge.HostRenderManager, logger *slog.Logger) {
}
