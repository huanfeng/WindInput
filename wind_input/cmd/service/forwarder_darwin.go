//go:build darwin

package main

import (
	"fmt"
	"image"
	"log/slog"
	"strings"
	"sync"

	"github.com/gogpu/gg"
	ggtext "github.com/gogpu/gg/text"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/systemfont"
)

// forwarder_darwin.go — PR-A.5 Phase 2: 把 ui.Manager 的 uicmd 命令转成 SHM bitmap
// + bridge push CmdHostRenderFrame 帧, 让 macOS IMKit `.app` 端 CandidatePanelHost
// 收到并贴出 NSPanel。
//
// 简化策略 (M3 第一版):
//   - 仅处理 CmdCandidatesShow / CmdCandidatesHide / CmdCandidatesPosition
//   - 不复用 wind_input/internal/ui/renderer.go (那套需要 RenderConfig + theme 完整
//     接入, M3 范围太大), 改用与 cmd/shmwriter 同源的最小 gg 渲染 (圆角白底 +
//     蓝色序号圈 + PingFang 文本)
//   - 后续 PR (M4) 把 ui.Renderer 真接入 → 主题/字体配置/动画统一
//
// Toolbar / Toast / Tooltip / Menu / Mode 等 uicmd 暂时 ignore, 走后续阶段。

type darwinForwarder struct {
	mu         sync.Mutex
	logger     *slog.Logger
	srv        *bridge.Server
	hrm        *bridge.HostRenderManager
	codec      *ipc.BinaryCodec
	fontSource *ggtext.FontSource
	lastSeq    uint32
}

// startCandidateForwarder 启动 darwin 渲染转发 goroutine。
// 调用时机: ui.Manager.WaitReady() 之后, 此时 cmdCh 已可订阅。
func startCandidateForwarder(srv *bridge.Server, mgr *ui.Manager,
	hrm *bridge.HostRenderManager, codec *ipc.BinaryCodec,
	logger *slog.Logger) {

	// 解析字体一次, 失败也不致命 (forwarder 仍 run, 收到 CandidatesShow 时再试)
	fontPath := systemfont.ResolveFile("PingFang SC", false)
	if fontPath == "" {
		fontPath = systemfont.ResolveFile("Helvetica", false)
	}
	var src *ggtext.FontSource
	if fontPath != "" {
		s, err := ggtext.NewFontSourceFromFile(fontPath)
		if err == nil {
			src = s
			logger.Info("darwin forwarder font loaded", "path", fontPath)
		} else {
			logger.Warn("darwin forwarder font load failed", "path", fontPath, "err", err)
		}
	} else {
		logger.Warn("darwin forwarder no system font found")
	}

	// 提前 setup SHM, 让 push notify 时 client mmap 已 ready
	if _, err := hrm.SetupHostRender(0); err != nil {
		logger.Error("darwin forwarder SHM setup failed", "err", err)
	}

	f := &darwinForwarder{
		logger:     logger,
		srv:        srv,
		hrm:        hrm,
		codec:      codec,
		fontSource: src,
	}

	mgr.SubscribeCommands(func(cmd uicmd.Command) {
		f.handle(cmd)
	})
	logger.Info("darwin candidate forwarder started")
}

func (f *darwinForwarder) handle(cmd uicmd.Command) {
	switch cmd.Type {
	case uicmd.CmdCandidatesShow:
		p, ok := cmd.Payload.(uicmd.CandidatesShowPayload)
		if !ok {
			return
		}
		f.showCandidates(p)
	case uicmd.CmdCandidatesHide:
		f.hideCandidates()
	default:
		// 其它命令 (Toolbar / Toast / Mode 等) 后续 PR 接入
	}
}

func (f *darwinForwarder) showCandidates(p uicmd.CandidatesShowPayload) {
	if f.fontSource == nil {
		f.logger.Debug("darwin forwarder font not ready, skip render")
		return
	}
	state := f.hrm.GetActiveState(0)
	if state == nil || state.SHM == nil {
		f.logger.Debug("darwin forwarder SHM not ready, skip")
		return
	}
	img := f.renderCandidates(p)
	if img == nil {
		return
	}
	seq, err := state.SHM.WriteFrame(img, int(p.CaretX), int(p.CaretY)+int(p.CaretHeight)+4)
	if err != nil {
		f.logger.Warn("darwin forwarder WriteFrame", "err", err)
		return
	}
	f.mu.Lock()
	f.lastSeq = seq
	f.mu.Unlock()

	payload := ipc.HostRenderFramePayload{
		Seq:    seq,
		X:      int32(p.CaretX),
		Y:      int32(p.CaretY) + int32(p.CaretHeight) + 4,
		Width:  uint32(img.Bounds().Dx()),
		Height: uint32(img.Bounds().Dy()),
		Flags:  0x3, // Visible | ContentReady
	}
	frame := f.codec.EncodeHostRenderFrame(payload)
	f.srv.BroadcastFrame(frame)
	f.logger.Debug("darwin forwarder pushed frame",
		"seq", seq,
		"size", fmt.Sprintf("%dx%d", img.Bounds().Dx(), img.Bounds().Dy()),
		"at", fmt.Sprintf("(%d,%d)", payload.X, payload.Y))
}

func (f *darwinForwarder) hideCandidates() {
	state := f.hrm.GetActiveState(0)
	if state == nil || state.SHM == nil {
		return
	}
	seq := state.SHM.WriteHide()
	payload := ipc.HostRenderFramePayload{
		Seq:   seq,
		Flags: 0, // not visible
	}
	frame := f.codec.EncodeHostRenderFrame(payload)
	f.srv.BroadcastFrame(frame)
}

// renderCandidates 与 cmd/shmwriter/main.go 同源 (M4 后会替换为 ui.Renderer)。
func (f *darwinForwarder) renderCandidates(p uicmd.CandidatesShowPayload) *image.RGBA {
	const (
		padX      = 12.0
		padY      = 10.0
		itemH     = 32.0
		gapItems  = 8.0
		indexBoxW = 18.0
		indexGap  = 6.0
		corner    = 8.0
		fontSize  = 18.0
		idxSize   = 12.0
	)

	probe := gg.NewContext(1, 1)
	face := f.fontSource.Face(fontSize)
	probe.SetFont(face)

	type sized struct {
		text string
		w    float64
	}
	var items []sized
	totalW := padX
	for _, c := range p.Candidates {
		t := strings.TrimSpace(c.Text)
		if t == "" {
			continue
		}
		w, _ := probe.MeasureString(t)
		itemW := indexBoxW + indexGap + w + 12
		items = append(items, sized{text: t, w: itemW})
		totalW += itemW + gapItems
	}
	if len(items) == 0 {
		return nil
	}
	totalW -= gapItems
	totalW += padX
	totalH := padY*2 + itemH

	dc := gg.NewContext(int(totalW), int(totalH))
	dc.SetHexColor("#FFFFFF")
	dc.DrawRoundedRectangle(0, 0, totalW, totalH, corner)
	dc.Fill()
	dc.SetHexColor("#DCDCDC")
	dc.SetLineWidth(1)
	dc.DrawRoundedRectangle(0.5, 0.5, totalW-1, totalH-1, corner)
	dc.Stroke()

	face = f.fontSource.Face(fontSize)
	idxFace := f.fontSource.Face(idxSize)

	x := padX
	y := padY + itemH/2
	for i, it := range items {
		cx := x + indexBoxW/2
		cy := y
		dc.SetHexColor("#4285F4")
		dc.DrawCircle(cx, cy, indexBoxW/2)
		dc.Fill()
		dc.SetFont(idxFace)
		dc.SetHexColor("#FFFFFF")
		dc.DrawStringAnchored(fmt.Sprintf("%d", i+1), cx, cy, 0.5, 0.35)
		dc.SetFont(face)
		dc.SetHexColor("#1E1E1E")
		dc.DrawStringAnchored(it.text, x+indexBoxW+indexGap, y, 0, 0.35)
		x += it.w + gapItems
	}
	return dc.Image().(*image.RGBA)
}

// startPlatformForwarder 是 cmd/service/main.go 的平台 hook (darwin: 启 candidate
// forwarder; windows: no-op, 见 forwarder_windows.go)。
func startPlatformForwarder(srv *bridge.Server, mgr *ui.Manager,
	hrm *bridge.HostRenderManager, logger *slog.Logger) {
	startCandidateForwarder(srv, mgr, hrm, ipc.NewBinaryCodec(), logger)
}
