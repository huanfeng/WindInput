//go:build darwin

package main

import (
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"strings"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/internal/ipc"
	"github.com/huanfeng/wind_input/internal/ui"
	"github.com/huanfeng/wind_input/internal/uicmd"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/systemfont"
	"github.com/huanfeng/wind_input/pkg/theme"
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

	// 缓存当前候选, 供悬停重渲染 (hover 仅改高亮, 候选数据不变)。
	lastPayload    uicmd.CandidatesShowPayload
	lastCandidates []ui.Candidate
	hoverIndex     int
	visible        bool

	// 主题: forwarder 在服务进程内自持 theme.Manager (exeDir/data/themes 可解析),
	// 按 config 文件 mtime 检测 ui.theme / theme_style 变化并 renderer.SetTheme 重应用;
	// theme_style=system 时跟随 macOS 外观 (检测 AppleInterfaceStyle)。
	themeMgr      *theme.Manager
	lastTheme     string
	cfgTheme      string            // 最近一次从 config 读到的主题名
	cfgStyle      config.ThemeStyle // 最近一次从 config 读到的 theme_style
	cfgPath       string
	lastCfgMod    time.Time
	lastDark      bool
	lastDarkCheck time.Time
}

// detectDarkMode 检测 macOS 系统是否暗色外观 (读 AppleInterfaceStyle, 2s TTL 缓存,
// 避免每帧 fork 子进程)。暗色时全局域有 AppleInterfaceStyle="Dark", 亮色时该键缺失。
func (f *darwinForwarder) detectDarkMode() bool {
	now := time.Now()
	if !f.lastDarkCheck.IsZero() && now.Sub(f.lastDarkCheck) < 2*time.Second {
		return f.lastDark
	}
	f.lastDarkCheck = now
	out, _ := exec.Command("defaults", "read", "-g", "AppleInterfaceStyle").Output()
	f.lastDark = strings.Contains(string(out), "Dark")
	return f.lastDark
}

// refreshThemeIfNeeded 按需重应用主题: config 文件变化时重读 ui.theme / theme_style
// (mtime 门控避免每帧 Load); theme_style=system 时每帧按 TTL 检测系统暗色。
// 主题名或暗色状态变化时才 renderer.SetTheme。设置界面改主题/风格 → 存 config.yaml
// → 下次渲染检测到并换肤; 系统切暗色 → system 风格下自动跟随。
func (f *darwinForwarder) refreshThemeIfNeeded() {
	if f.themeMgr == nil {
		return
	}
	// 1. config 变化时重读主题名 + 风格
	if f.cfgPath != "" {
		if st, err := os.Stat(f.cfgPath); err == nil && st.ModTime().After(f.lastCfgMod) {
			f.lastCfgMod = st.ModTime()
			if cfg, err := config.Load(); err == nil {
				f.cfgTheme, f.cfgStyle = cfg.UI.Theme, cfg.UI.ThemeStyle
			}
		}
	}
	if f.cfgTheme == "" {
		if cfg, err := config.Load(); err == nil {
			f.cfgTheme, f.cfgStyle = cfg.UI.Theme, cfg.UI.ThemeStyle
		}
		if f.cfgTheme == "" {
			return
		}
	}
	// 2. 判定暗色: dark/light 强制, system 跟随 OS
	var dark bool
	switch f.cfgStyle {
	case config.ThemeStyleDark:
		dark = true
	case config.ThemeStyleLight:
		dark = false
	default:
		dark = f.detectDarkMode()
	}
	// 3. 主题名或暗色变化 → 重新加载/解析并应用 (SetDarkMode 内部重 resolve)
	nameChanged := f.cfgTheme != f.lastTheme
	if nameChanged {
		if err := f.themeMgr.LoadTheme(f.cfgTheme); err != nil {
			f.logger.Warn("darwin forwarder 加载主题失败", "theme", f.cfgTheme, "err", err)
			return
		}
		f.lastTheme = f.cfgTheme
	}
	darkChanged := f.themeMgr.SetDarkMode(dark)
	if nameChanged || darkChanged {
		f.renderer.SetTheme(f.themeMgr.GetResolvedTheme())
		f.logger.Info("darwin forwarder 主题已应用", "theme", f.lastTheme, "dark", dark)
	}
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

	// 主力中文字体候选链: 取第一个本机能解析到的含 CJK 字形的字体。
	// 绝不退到纯拉丁字体 (Helvetica), 否则汉字会渲染成方框 □。
	//   - PingFang SC: 全功能 macOS 默认 (Tahoe 在 AssetsV2, Sequoia 在 /System/Library/Fonts)
	//   - Hiragino Sans GB / STHeiti / Songti: 精简 VM 镜像 (tart 等) 常缺 PingFang 时的备选
	fontFamily := ""
	for _, cand := range []string{"PingFang SC", "Hiragino Sans GB", "STHeiti", "Songti SC", "Songti"} {
		if systemfont.ResolveFile(cand, false) != "" {
			fontFamily = cand
			break
		}
	}
	if fontFamily == "" {
		fontFamily = "Helvetica" // 实在没有 CJK 字体的兜底 (汉字仍会方框, 但 ASCII 可见)
		logger.Warn("darwin forwarder 未找到任何 CJK 字体, 汉字将渲染为方框", "fallback", fontFamily)
	}
	renderer.UpdateFont(18, fontFamily)
	logger.Info("darwin forwarder renderer ready", "font", fontFamily)

	// 提前 setup SHM, 让 push notify 时 client mmap 已 ready
	if _, err := hrm.SetupHostRender(0); err != nil {
		logger.Error("darwin forwarder SHM setup failed", "err", err)
	}

	f := &darwinForwarder{
		logger:     logger,
		srv:        srv,
		hrm:        hrm,
		codec:      codec,
		renderer:   renderer,
		hoverIndex: -1,
		themeMgr:   theme.NewManager(logger),
	}
	if p, err := config.GetConfigPath(); err == nil {
		f.cfgPath = p
	}
	f.refreshThemeIfNeeded() // 启动时应用 config 里的 active 主题

	mgr.SubscribeCommands(func(cmd uicmd.Command, candidates []ui.Candidate) {
		f.handle(cmd, candidates)
	})
	srv.SetCandidateHoverHandler(func(idx int) { f.onHover(idx) })
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
	case uicmd.CmdToolbarShow:
		if p, ok := cmd.Payload.(uicmd.ToolbarShowPayload); ok {
			f.pushModeStatus(p.State, true)
		}
	case uicmd.CmdToolbarUpdate:
		if p, ok := cmd.Payload.(uicmd.ToolbarUpdatePayload); ok {
			f.pushModeStatus(p.State, true)
		}
	case uicmd.CmdToolbarHide:
		f.pushModeStatus(uicmd.ToolbarState{}, false)
	default:
		// 其它命令 (Toast / Menu 等) 后续 PR 接入
	}
}

// pushModeStatus 把输入模式状态经 push 通道发给 .app 菜单栏指示器 (CmdModeStatus)。
// visible=false 时通知客户端隐藏指示器 (IME 失活/失焦)。
func (f *darwinForwarder) pushModeStatus(st uicmd.ToolbarState, visible bool) {
	var flags uint32
	if st.ChineseMode {
		flags |= ipc.StatusChineseMode
	}
	if st.FullWidth {
		flags |= ipc.StatusFullWidth
	}
	if st.ChinesePunct {
		flags |= ipc.StatusChinesePunct
	}
	if st.CapsLock {
		flags |= ipc.StatusCapsLock
	}
	if visible {
		flags |= ipc.StatusToolbarVisible
	}
	f.srv.BroadcastFrame(f.codec.EncodeModeStatus(flags, uint32(st.EffectiveMode), st.ModeLabel))
}

func (f *darwinForwarder) showCandidates(p uicmd.CandidatesShowPayload, candidates []ui.Candidate) {
	if len(candidates) == 0 {
		f.hideCandidates()
		return
	}
	f.mu.Lock()
	f.lastPayload = p
	f.lastCandidates = candidates
	f.hoverIndex = -1 // 新一轮候选, 清除上次悬停
	f.visible = true
	f.mu.Unlock()
	f.renderAndPush()
}

// onHover 鼠标悬停某候选, 仅改高亮重渲染 (候选数据不变)。idx<0 = 无悬停。
func (f *darwinForwarder) onHover(idx int) {
	f.mu.Lock()
	if !f.visible || idx == f.hoverIndex {
		f.mu.Unlock()
		return
	}
	f.hoverIndex = idx
	f.mu.Unlock()
	f.renderAndPush()
}

// renderAndPush 用当前缓存的候选 + hoverIndex 渲染并推帧 (含命中矩形)。
func (f *darwinForwarder) renderAndPush() {
	f.refreshThemeIfNeeded() // 渲染前检测主题变化 (config mtime 变了才重载)
	f.mu.Lock()
	p := f.lastPayload
	candidates := f.lastCandidates
	hover := f.hoverIndex
	f.mu.Unlock()
	if len(candidates) == 0 {
		return
	}
	state := f.hrm.GetActiveState(0)
	if state == nil || state.SHM == nil {
		f.logger.Debug("darwin forwarder SHM not ready, skip")
		return
	}

	hoverBtn := "" // 翻页按钮悬停后续可加
	img, renderResult := f.renderer.RenderCandidates(
		candidates, p.Input, p.CursorPos,
		p.Page, p.TotalPages,
		hover, hoverBtn, p.SelectedIndex)
	if img == nil {
		return
	}

	x := p.CaretX
	y := p.CaretY + p.CaretHeight + 4
	seq, err := state.SHM.WriteFrame(img, x, y)
	if err != nil {
		f.logger.Warn("darwin forwarder WriteFrame", "err", err)
		return
	}

	f.srv.BroadcastFrame(f.codec.EncodeHostRenderFrame(ipc.HostRenderFramePayload{
		Seq: seq, X: int32(x), Y: int32(y),
		Width: uint32(img.Bounds().Dx()), Height: uint32(img.Bounds().Dy()),
		Flags: 0x3, Scale: darwinRenderScale,
	}))

	// 候选命中矩形 + 翻页按钮矩形 (pageUp=index -1, pageDown=index -2), 供 .app hit-test。
	if renderResult != nil {
		rects := make([]ipc.CandidateHitRect, 0, len(renderResult.Rects)+2)
		for _, rc := range renderResult.Rects {
			rects = append(rects, ipc.CandidateHitRect{
				Index: int32(rc.Index), X: int32(rc.X), Y: int32(rc.Y), W: int32(rc.W), H: int32(rc.H),
			})
		}
		if r := renderResult.PageUpRect; r != nil {
			rects = append(rects, ipc.CandidateHitRect{Index: -1, X: int32(r.X), Y: int32(r.Y), W: int32(r.W), H: int32(r.H)})
		}
		if r := renderResult.PageDownRect; r != nil {
			rects = append(rects, ipc.CandidateHitRect{Index: -2, X: int32(r.X), Y: int32(r.Y), W: int32(r.W), H: int32(r.H)})
		}
		if len(rects) > 0 {
			f.srv.BroadcastFrame(f.codec.EncodeCandidateRects(rects))
		}
	}

	f.logger.Debug("darwin forwarder pushed frame",
		"seq", seq, "n", len(candidates), "hover", hover,
		"size", fmt.Sprintf("%dx%d", img.Bounds().Dx(), img.Bounds().Dy()),
		"at", fmt.Sprintf("(%d,%d)", x, y))
}

func (f *darwinForwarder) hideCandidates() {
	f.mu.Lock()
	f.visible = false
	f.lastCandidates = nil
	f.hoverIndex = -1
	f.mu.Unlock()
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
