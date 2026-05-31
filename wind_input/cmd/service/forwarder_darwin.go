//go:build darwin

package main

import (
	"fmt"
	"image/color"
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

// forwarder_darwin.go — 把 ui.Manager 的 uicmd 命令转成 SHM bitmap +
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
	cfgTheme      string                       // 最近一次从 config 读到的主题名
	cfgStyle      config.ThemeStyle            // 最近一次从 config 读到的 theme_style
	cfgSchema     string                       // 最近一次从 config 读到的 schema.active (右键禁用判定用)
	cfgStatus     config.StatusIndicatorConfig // 状态提示气泡配置 (开关/内容/时长/配色)
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
				f.cfgTheme, f.cfgStyle, f.cfgSchema = cfg.UI.Theme, cfg.UI.ThemeStyle, cfg.Schema.Active
				f.cfgStatus = cfg.UI.StatusIndicator
			}
		}
	}
	if f.cfgTheme == "" {
		if cfg, err := config.Load(); err == nil {
			f.cfgTheme, f.cfgStyle, f.cfgSchema = cfg.UI.Theme, cfg.UI.ThemeStyle, cfg.Schema.Active
			f.cfgStatus = cfg.UI.StatusIndicator
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

	// 启动时按 config 播种 renderer 的 preedit 相关配置。Manager 会在 applyConfig
	// 时投递一次 CmdCandidatesConfig (cmdCh 缓冲 100, 通常不丢), 但订阅前若被消费时序
	// 错过, renderer 会停留在 DefaultRenderConfig 的 HidePreedit=false。提前 seed 一次,
	// 确保首屏候选框就遵循 InlinePreedit (嵌入编码时不画独立编码栏)。
	if cfg, err := config.Load(); err == nil {
		f.renderer.SetHidePreedit(cfg.UI.InlinePreedit)
		f.renderer.SetPreeditMode(cfg.UI.PreeditMode)
	}

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
	case uicmd.CmdCandidatesConfig:
		if p, ok := cmd.Payload.(uicmd.CandidatesConfigPayload); ok {
			f.applyCandidatesConfig(p)
		}
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
	case uicmd.CmdSettingsOpen:
		// 统一菜单的 设置/词库管理/关于 → 让 .app 在 GUI 会话用 NSWorkspace 打开设置应用。
		page := ""
		if p, ok := cmd.Payload.(uicmd.SettingsOpenPayload); ok {
			page = p.Page
		}
		f.srv.BroadcastFrame(f.codec.EncodeOpenSettings(page))
	case uicmd.CmdTooltipShow:
		if p, ok := cmd.Payload.(uicmd.TooltipShowPayload); ok {
			bg, fg := f.tooltipColors()
			f.srv.BroadcastFrame(f.codec.EncodeTooltipShow(p.Text, bg, fg, p.FontPath))
		}
	case uicmd.CmdTooltipHide:
		f.srv.BroadcastFrame(f.codec.EncodeTooltipHide())
	case uicmd.CmdStatusShow:
		if p, ok := cmd.Payload.(uicmd.StatusShowPayload); ok {
			f.showStatusBubble(p)
		}
	case uicmd.CmdStatusHide:
		f.srv.BroadcastFrame(f.codec.EncodeStatusHide())
	case uicmd.CmdToastShow:
		if p, ok := cmd.Payload.(uicmd.ToastShowPayload); ok {
			f.showToast(p)
		}
	case uicmd.CmdToastHide:
		f.srv.BroadcastFrame(f.codec.EncodeToastHide())
	default:
		// 其它命令 (Menu 等) 后续 PR 接入
	}
}

// tooltipColors 取当前已解析主题的 tooltip 配色 (#RRGGBBAA), 供 .app 应用。
// 主题缺省时退到 nil → 用 .app 内置深色默认。
func (f *darwinForwarder) tooltipColors() (bg, fg string) {
	if f.themeMgr == nil {
		return "", ""
	}
	rt := f.themeMgr.GetResolvedTheme()
	if rt == nil {
		return "", ""
	}
	return theme.ColorToHex(rt.Tooltip.BackgroundColor), theme.ColorToHex(rt.Tooltip.TextColor)
}

// showStatusBubble 按 config 把 ShowStatusIndicator 投递的状态合成最终气泡帧推给 .app。
// 关停 (Enabled=false) 或文本为空时不推; temp 模式带 Duration 让 .app 到点自动隐藏。
func (f *darwinForwarder) showStatusBubble(p uicmd.StatusShowPayload) {
	f.refreshThemeIfNeeded() // 取最新 config (开关/内容/时长/配色) 与主题
	cfg := f.cfgStatus
	if !cfg.Enabled {
		return
	}
	text := buildStatusText(p.State, cfg.ShowMode, cfg.ShowPunct, cfg.ShowFullWidth)
	if text == "" {
		return
	}
	bg, fg := f.statusColors()
	duration := int32(0) // always 模式常驻 (duration=0)
	if cfg.DisplayMode != "always" {
		duration = int32(cfg.Duration)
	}
	// 锚到 caret 底部下方 (与候选窗口同位置: CaretY+CaretHeight+4), 随字体大小自适应,
	// 不遮挡文字。OffsetX/OffsetY 为 config 微调。
	x := int32(p.X + cfg.OffsetX)
	y := int32(p.Y + p.Height + 4 + cfg.OffsetY)
	f.srv.BroadcastFrame(f.codec.EncodeStatusShow(text, bg, fg, x, y, duration))
}

// buildStatusText 合并模式/标点/全半角标签 (空格分隔), 镜像 Win ui.BuildStatusText。
func buildStatusText(s uicmd.StatusState, showMode, showPunct, showFullWidth bool) string {
	var parts []string
	if showMode && s.ModeLabel != "" {
		parts = append(parts, s.ModeLabel)
	}
	if showPunct && s.PunctLabel != "" {
		parts = append(parts, s.PunctLabel)
	}
	if showFullWidth && s.WidthLabel != "" {
		parts = append(parts, s.WidthLabel)
	}
	return strings.Join(parts, " ")
}

// statusColors 取状态气泡配色 (#RRGGBBAA): config 自定义 > 主题 ModeIndicator > 内置默认;
// bg 叠加 config.Opacity 到 alpha。镜像 Win StatusRenderer.getColors + applyOpacity。
func (f *darwinForwarder) statusColors() (bg, fg string) {
	var bgC color.Color = color.RGBA{60, 60, 60, 240}
	var fgC color.Color = color.RGBA{255, 255, 255, 255}
	if f.themeMgr != nil {
		if rt := f.themeMgr.GetResolvedTheme(); rt != nil {
			bgC = rt.ModeIndicator.BackgroundColor
			fgC = rt.ModeIndicator.TextColor
		}
	}
	if c, err := theme.ParseHexColor(f.cfgStatus.BackgroundColor); err == nil {
		bgC = c
	}
	if c, err := theme.ParseHexColor(f.cfgStatus.TextColor); err == nil {
		fgC = c
	}
	bgC = applyStatusOpacity(bgC, f.cfgStatus.Opacity)
	return theme.ColorToHex(bgC), theme.ColorToHex(fgC)
}

// applyStatusOpacity 把 opacity (0..1) 叠加到颜色 alpha 通道。
func applyStatusOpacity(c color.Color, opacity float64) color.Color {
	if opacity <= 0 {
		opacity = 0
	} else if opacity > 1 {
		opacity = 1
	}
	r, g, b, a := c.RGBA()
	return color.RGBA{uint8(r >> 8), uint8(g >> 8), uint8(b >> 8), uint8(float64(a>>8) * opacity)}
}

// showToast 把 ui.Manager 投递的 Toast 请求合成最终帧推给 .app (CmdToastShow)。
// bg/fg 取主题 Tooltip 配色 (强制不透明, 与 Win toast 一致); accent 按级别取
// ui.ToastAccentColor; position/duration/maxWidth 透传, 由 .app 落位与计时。
func (f *darwinForwarder) showToast(p uicmd.ToastShowPayload) {
	if p.Title == "" && p.Message == "" {
		return
	}
	f.refreshThemeIfNeeded() // 取最新主题配色
	bg, fg := f.toastColors()
	accent := theme.ColorToHex(ui.ToastAccentColor(wireToToastLevel(p.Level)))
	pos := string(p.Position)
	if pos == "" {
		pos = string(uicmd.ToastBottomRight)
	}
	f.srv.BroadcastFrame(f.codec.EncodeToastShow(p.Title, p.Message, bg, fg, accent, pos, p.Duration, p.MaxWidth))
}

// toastColors 取 Toast 背景/正文配色 (#RRGGBBAA): 沿用主题 Tooltip 调色板 (暗背景+浅文本),
// 背景强制不透明 (与系统通知一致, 避免重要信息透出底层窗口)。无主题时退到内置深色默认。
func (f *darwinForwarder) toastColors() (bg, fg string) {
	var bgC color.Color = color.RGBA{0x2B, 0x2B, 0x2B, 0xFF}
	var fgC color.Color = color.RGBA{0xFF, 0xFF, 0xFF, 0xFF}
	if f.themeMgr != nil {
		if rt := f.themeMgr.GetResolvedTheme(); rt != nil {
			bgC = forceOpaqueColor(rt.Tooltip.BackgroundColor)
			fgC = rt.Tooltip.TextColor
		}
	}
	return theme.ColorToHex(bgC), theme.ColorToHex(fgC)
}

// forceOpaqueColor 把颜色 alpha 强制为 0xFF (镜像 Win toast_renderer.forceAlphaOpaque)。
func forceOpaqueColor(c color.Color) color.Color {
	r, g, b, _ := c.RGBA()
	return color.RGBA{uint8(r >> 8), uint8(g >> 8), uint8(b >> 8), 0xFF}
}

// wireToToastLevel 把线上字符串级别 (uicmd.ToastLevel) 映射回 ui.ToastLevel, 用于取 accent 配色。
func wireToToastLevel(l uicmd.ToastLevel) ui.ToastLevel {
	switch l {
	case uicmd.ToastSuccess:
		return ui.ToastSuccess
	case uicmd.ToastWarn:
		return ui.ToastWarn
	case uicmd.ToastError:
		return ui.ToastError
	default:
		return ui.ToastInfo
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

// computeMenuFlags 算当前页每个候选的右键菜单禁用位 (镜像 Win window_mouse.go 规则)。
// page 为 1-based 页码, perPage 每页数, total 总候选数; 用全局位置判定首/末/单。
func (f *darwinForwarder) computeMenuFlags(candidates []ui.Candidate, page, perPage, total int) []byte {
	if len(candidates) == 0 {
		return nil
	}
	if perPage <= 0 {
		perPage = len(candidates)
	}
	if page < 1 {
		page = 1
	}
	pinyinSchema := f.cfgSchema == "pinyin"
	out := make([]byte, len(candidates))
	for i := range candidates {
		c := candidates[i]
		global := (page-1)*perPage + i
		isFirst := global == 0
		isLast := total > 0 && global == total-1
		single := total <= 1
		isGroupMember := c.IsGroupMember
		isSingleChar := len([]rune(c.Text)) <= 1
		// 拼音引擎的普通候选无稳定 ID, 不能移动 (Win: isPinyin && !isCommand);
		// 纯拼音方案按 schema 判, 混输按候选 Source 判。
		isPinyin := pinyinSchema || string(c.Source) == "pinyin"
		moveBlocked := isPinyin && !c.IsCommand

		var b uint8
		if isFirst || single || moveBlocked || isGroupMember {
			b |= ipc.MenuFlagDisableMoveUp | ipc.MenuFlagDisableMoveTop
		}
		if isLast || single || moveBlocked || isGroupMember {
			b |= ipc.MenuFlagDisableMoveDown
		}
		if (isSingleChar && !c.IsCommand) || isGroupMember {
			b |= ipc.MenuFlagDisableDelete
		}
		if !c.HasShadow || isGroupMember {
			b |= ipc.MenuFlagDisableReset
		}
		out[i] = b
	}
	return out
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

	// 右键菜单禁用位 (每候选 1 字节), 供 .app NSMenu 按候选状态禁用项。
	if flags := f.computeMenuFlags(candidates, p.Page, p.CandidatesPerPage, p.TotalCandidateCount); len(flags) > 0 {
		f.srv.BroadcastFrame(f.codec.EncodeCandidateMenuFlags(flags))
	}

	f.logger.Debug("darwin forwarder pushed frame",
		"seq", seq, "n", len(candidates), "hover", hover,
		"size", fmt.Sprintf("%dx%d", img.Bounds().Dx(), img.Bounds().Dy()),
		"at", fmt.Sprintf("(%d,%d)", x, y))
}

// applyCandidatesConfig 把 Go 端 ui.Manager 下发的候选框配置 (CmdCandidatesConfig)
// 应用到本地 renderer。最关键的是 HidePreedit：InlinePreedit=true 时编码已嵌入宿主
// 应用, 候选框不应再画独立编码行。在此之前 forwarder 不处理该命令, renderer 的
// HidePreedit 恒为 DefaultRenderConfig 的 false, 导致编码栏无视配置始终显示。
// PreeditMode (top/embedded) 同理影响编码行的画法。
func (f *darwinForwarder) applyCandidatesConfig(p uicmd.CandidatesConfigPayload) {
	f.renderer.SetHidePreedit(p.HidePreedit)
	f.renderer.SetPreeditMode(config.PreeditMode(p.PreeditMode))
	f.renderer.SetLayout(config.CandidateLayout(p.Layout))
	f.renderer.SetCmdbarPrefix(p.CmdbarPrefix)
	// 配置可能在候选框已显示时变更 (设置界面实时改), 立即重渲染当前帧。
	f.mu.Lock()
	visible := f.visible
	f.mu.Unlock()
	if visible {
		f.renderAndPush()
	}
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
