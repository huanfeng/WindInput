// shmwriter — M3-3 调试 CLI: 用 gg 画一个候选框 mockup 写到 /WindInput_SHM,
// 让 wind-input-demo 可以贴出来。
//
// 用法:
//
//	go run ./cmd/shmwriter -text "你好 / 倪好 / 拟好" -x 400 -y 600 [-loop]
//
// -loop 模式每秒切一次候选 (用于验证 demo polling 闭环和 NSPanel 重定位)。
//
// 后续 (PR-A 后) 这部分逻辑会内嵌进 wind_input 服务的 darwin forwarder, 监听
// ui.Manager.cmdCh, 收 CandidatesShow 时同样调 render + WriteFrame + broadcastPush。
package main

import (
	"flag"
	"fmt"
	"image"
	"log/slog"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/gogpu/gg"
	ggtext "github.com/gogpu/gg/text"

	"github.com/huanfeng/wind_input/internal/bridge"
	"github.com/huanfeng/wind_input/pkg/systemfont"
)

func main() {
	textFlag := flag.String("text", "你好 / 倪好 / 拟好 / 妮好 / 你浩", "候选词列表 (空格分隔)")
	xFlag := flag.Int("x", 400, "屏幕 X (wire top-left)")
	yFlag := flag.Int("y", 600, "屏幕 Y (wire top-left)")
	loopFlag := flag.Bool("loop", false, "每秒切换候选")
	flag.Parse()

	logger := slog.New(slog.NewTextHandler(os.Stderr, nil))
	mgr := bridge.NewHostRenderManager(logger, nil)
	setup, err := mgr.SetupHostRender(0)
	if err != nil {
		fmt.Println("setup SHM failed:", err)
		os.Exit(1)
	}
	fmt.Printf("SHM ready: name=%s size=%d\n", setup.ShmName, setup.MaxBufferSize)
	defer mgr.CleanupAll()

	fontPath := systemfont.ResolveFile("PingFang SC", false)
	if fontPath == "" {
		fontPath = systemfont.ResolveFile("Helvetica", false)
	}
	if fontPath == "" {
		fmt.Println("no system font found")
		os.Exit(1)
	}
	fmt.Println("font:", fontPath)

	src, err := ggtext.NewFontSourceFromFile(fontPath)
	if err != nil {
		fmt.Println("font open:", err)
		os.Exit(1)
	}

	writeOne := func(text string) {
		img := renderCandidates(src, text)
		state := mgr.GetActiveState(0)
		seq, err := state.SHM.WriteFrame(img, *xFlag, *yFlag)
		if err != nil {
			fmt.Println("WriteFrame:", err)
			return
		}
		fmt.Printf("wrote seq=%d size=%dx%d at (%d,%d) text=%q\n",
			seq, img.Bounds().Dx(), img.Bounds().Dy(), *xFlag, *yFlag, text)
	}

	if !*loopFlag {
		writeOne(*textFlag)
		fmt.Println("Demo: 启动 wind-input-demo 应该能看到候选框。Ctrl-C 退出 (会清理 SHM)。")
		// 等 SIGINT/SIGTERM 优雅退出 (走 defer mgr.CleanupAll → shm_unlink)。
		// 不能用 `select {}` — Go runtime 会判为永久阻塞 deadlock。
		sig := make(chan os.Signal, 1)
		signal.Notify(sig, os.Interrupt, syscall.SIGTERM)
		<-sig
		return
	}

	candidates := []string{
		"你好 / 倪好 / 拟好",
		"明天 / 鸣天 / 名天",
		"输入法 / 输入发",
		"清风 / 轻风 / 倾峰",
		"GitHub / git / give",
	}
	for i := 0; ; i++ {
		writeOne(candidates[i%len(candidates)])
		time.Sleep(time.Second)
	}
}

// renderCandidates 用 gg 画一个简化候选框: 圆角白底 + 蓝序号圈 + 候选文本。
// 同 wind_input/internal/ui/renderer.go 的简化版, 仅用于 demo / shmwriter。
func renderCandidates(src *ggtext.FontSource, text string) *image.RGBA {
	parts := strings.Split(text, "/")
	for i, p := range parts {
		parts[i] = strings.TrimSpace(p)
	}

	// 测量每个候选项宽度
	const (
		padX       = 12.0
		padY       = 10.0
		itemH      = 32.0
		gapItems   = 8.0
		indexBoxW  = 18.0
		indexGap   = 6.0
		corner     = 8.0
		fontSize   = 18.0
		idxSize    = 12.0
		bgColor    = 0xFFFFFFFF // ABGR-like? gg.SetHexColor 接受 #RRGGBB
		txtColor   = 0x1E1E1E
		idxBgColor = 0x4285F4
		idxFg      = 0xFFFFFF
	)
	_ = bgColor

	// 用临时 dc 测量字宽
	probe := gg.NewContext(1, 1)
	face := src.Face(fontSize)
	probe.SetFont(face)

	type sized struct {
		text string
		w    float64
	}
	var items []sized
	totalW := padX
	for _, p := range parts {
		if p == "" {
			continue
		}
		w, _ := probe.MeasureString(p)
		itemW := indexBoxW + indexGap + w + 12 // 序号圈 + gap + 文本 + 右 padding
		items = append(items, sized{text: p, w: itemW})
		totalW += itemW + gapItems
	}
	totalW -= gapItems
	totalW += padX
	totalH := padY*2 + itemH

	dc := gg.NewContext(int(totalW), int(totalH))
	// 圆角白底
	dc.SetHexColor("#FFFFFF")
	dc.DrawRoundedRectangle(0, 0, totalW, totalH, corner)
	dc.Fill()
	// 浅灰描边
	dc.SetHexColor("#DCDCDC")
	dc.SetLineWidth(1)
	dc.DrawRoundedRectangle(0.5, 0.5, totalW-1, totalH-1, corner)
	dc.Stroke()

	face = src.Face(fontSize)
	idxFace := src.Face(idxSize)

	x := padX
	y := padY + itemH/2 // baseline 中线
	for i, it := range items {
		// 序号圈
		cx := x + indexBoxW/2
		cy := y
		dc.SetHexColor("#4285F4")
		dc.DrawCircle(cx, cy, indexBoxW/2)
		dc.Fill()
		// 序号数字
		dc.SetFont(idxFace)
		dc.SetHexColor("#FFFFFF")
		dc.DrawStringAnchored(fmt.Sprintf("%d", i+1), cx, cy, 0.5, 0.35)
		// 候选文本
		dc.SetFont(face)
		dc.SetHexColor("#1E1E1E")
		dc.DrawStringAnchored(it.text, x+indexBoxW+indexGap, y, 0, 0.35)

		x += it.w + gapItems
	}

	return dc.Image().(*image.RGBA)
}
