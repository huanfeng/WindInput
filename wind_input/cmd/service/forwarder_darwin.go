//go:build darwin

package main

import (
	"fmt"
	"log/slog"
	"sync"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/systemfont"
)

// forwarder_darwin.go — PR-A M4: 把 ui.Manager 的 uicmd 命令转成 SHM bitmap +
// bridge push CmdHostRenderFrame 帧, 让 macOS IMKit `.app` 端 CandidatePanelHost
// 收到并贴出 NSPanel。
//
// M4 用**真** ui.Renderer (跨平台 gg 渲染核心) 替代 M3 的 mockup, 与 Win 端共用
// 同一渲染逻辑 (主题/布局/字体/序号圈/分页/选中高亮), 实现 Win/Mac 视觉一致。
// renderer freetype 后端 + PingFang 字体 (systemfont darwin catalog 定位)。
//
// 处理 CmdCandidatesShow / CmdCandidatesHide; Toolbar/Toast/Mode 等后续 PR 接入。

// darwinRenderScale: 候选框位图 HiDPI 渲染倍率 (2=Retina)。
const darwinRenderScale = 2

type darwinForwarder struct {
	mu       sync.Mutex
	logger   *slog.Logger
	srv      *bridge.Server
	hrm      *bridge.HostRenderManager
	codec    *ipc.BinaryCodec
	renderer *ui.Renderer
}

// startCandidateForwarder 启动 darwin 渲染转发 goroutine。
// 调用时机: ui.Manager.WaitReady() 之后, 此时 cmdCh 已可订阅。
func startCandidateForwarder(srv *bridge.Server, mgr *ui.Manager,
	hrm *bridge.HostRenderManager, codec *ipc.BinaryCodec,
	logger *slog.Logger) {

	// HiDPI: 按 2x 渲染候选框位图 (现代 Mac 多为 Retina), 客户端按 logical 尺寸
	// (像素/scale) 显示 → Retina 上 1 device px : 1 image px, 清晰不糊。
	// 必须在 NewRenderer/DefaultRenderConfig 前注入, 因为它们读 GetDPIScale 算字号/尺寸。
	// TODO: 后续可由客户端上报 backingScaleFactor 动态决定 (非 Retina 屏用 1x)。
	ui.SetDPIScaleProvider(func() float64 { return float64(darwinRenderScale) })

	renderer := ui.NewRenderer(ui.DefaultRenderConfig())
	// darwin 仅 freetype 后端 (text_backend_darwin.go 忽略 mode 恒走 freetype)
	renderer.SetTextRenderMode(ui.TextRenderModeFreetype)

	// 主力中文字体: PingFang SC (systemfont darwin → AssetsV2 PingFang.ttc)
	fontFamily := "PingFang SC"
	if systemfont.ResolveFile(fontFamily, false) == "" {
		fontFamily = "Helvetica"
	}
	renderer.UpdateFont(18, fontFamily)
	logger.Info("darwin forwarder renderer ready", "font", fontFamily)

	// 提前 setup SHM, 让 push notify 时 client mmap 已 ready
	if _, err := hrm.SetupHostRender(0); err != nil {
		logger.Error("darwin forwarder SHM setup failed", "err", err)
	}

	f := &darwinForwarder{
		logger:   logger,
		srv:      srv,
		hrm:      hrm,
		codec:    codec,
		renderer: renderer,
	}

	mgr.SubscribeCommands(func(cmd uicmd.Command, candidates []ui.Candidate) {
		f.handle(cmd, candidates)
	})
	logger.Info("darwin candidate forwarder started")
}

func (f *darwinForwarder) handle(cmd uicmd.Command, candidates []ui.Candidate) {
	switch cmd.Type {
	case uicmd.CmdCandidatesShow:
		p, ok := cmd.Payload.(uicmd.CandidatesShowPayload)
		if !ok {
			return
		}
		f.showCandidates(p, candidates)
	case uicmd.CmdCandidatesHide:
		f.hideCandidates()
	default:
		// 其它命令 (Toolbar / Toast / Mode 等) 后续 PR 接入
	}
}

func (f *darwinForwarder) showCandidates(p uicmd.CandidatesShowPayload, candidates []ui.Candidate) {
	if len(candidates) == 0 {
		f.hideCandidates()
		return
	}
	state := f.hrm.GetActiveState(0)
	if state == nil || state.SHM == nil {
		f.logger.Debug("darwin forwarder SHM not ready, skip")
		return
	}

	// 用真 ui.Renderer 渲染 (与 Win 端同一逻辑)。hoverIndex=-1 (hover 后续 PR),
	// selectedIndex 高亮键盘选中项。renderResult.Rects 为每个候选的 panel-local 命中矩形。
	img, renderResult := f.renderer.RenderCandidates(
		candidates, p.Input, p.CursorPos,
		p.Page, p.TotalPages,
		-1, "", p.SelectedIndex)
	if img == nil {
		return
	}

	// 候选框贴在 caret 下方 (top-left wire 坐标)
	x := p.CaretX
	y := p.CaretY + p.CaretHeight + 4
	seq, err := state.SHM.WriteFrame(img, x, y)
	if err != nil {
		f.logger.Warn("darwin forwarder WriteFrame", "err", err)
		return
	}

	payload := ipc.HostRenderFramePayload{
		Seq:    seq,
		X:      int32(x),
		Y:      int32(y),
		Width:  uint32(img.Bounds().Dx()),
		Height: uint32(img.Bounds().Dy()),
		Flags:  0x3, // Visible | ContentReady
		Scale:  darwinRenderScale,
	}
	f.srv.BroadcastFrame(f.codec.EncodeHostRenderFrame(payload))

	// 推候选命中矩形 (panel-local), 供 .app NSPanel 鼠标点选 hit-test。
	if renderResult != nil && len(renderResult.Rects) > 0 {
		rects := make([]ipc.CandidateHitRect, 0, len(renderResult.Rects))
		for _, rc := range renderResult.Rects {
			rects = append(rects, ipc.CandidateHitRect{
				Index: int32(rc.Index),
				X:     int32(rc.X), Y: int32(rc.Y),
				W: int32(rc.W), H: int32(rc.H),
			})
		}
		f.srv.BroadcastFrame(f.codec.EncodeCandidateRects(rects))
	}

	f.logger.Debug("darwin forwarder pushed frame",
		"seq", seq, "n", len(candidates),
		"size", fmt.Sprintf("%dx%d", img.Bounds().Dx(), img.Bounds().Dy()),
		"at", fmt.Sprintf("(%d,%d)", x, y))
}

func (f *darwinForwarder) hideCandidates() {
	state := f.hrm.GetActiveState(0)
	if state == nil || state.SHM == nil {
		return
	}
	seq := state.SHM.WriteHide()
	f.srv.BroadcastFrame(f.codec.EncodeHostRenderFrame(ipc.HostRenderFramePayload{
		Seq: seq, Flags: 0,
	}))
}

// startPlatformForwarder 是 cmd/service/main.go 的平台 hook (darwin: 启 candidate
// forwarder; windows: no-op, 见 forwarder_windows.go)。
func startPlatformForwarder(srv *bridge.Server, mgr *ui.Manager,
	hrm *bridge.HostRenderManager, logger *slog.Logger) {
	startCandidateForwarder(srv, mgr, hrm, ipc.NewBinaryCodec(), logger)
}
