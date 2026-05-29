package ui

import (
	"fmt"
	"image"
	"image/color"
	"image/png"
	"os"
	"path/filepath"
	"time"

	"github.com/huanfeng/wind_input/internal/clipboard"
	"github.com/huanfeng/wind_input/pkg/config"
)

// doTakeScreenshot 在 UI 线程上执行截图：对所有当前可见的 UI 窗口调用各自的
// CaptureToFile，图像保存到用户数据目录下的 screenshots/ 子目录。
func (m *Manager) doTakeScreenshot() {
	dir, err := config.GetScreenshotsDir()
	if err != nil {
		m.logger.Warn("Screenshot: failed to resolve screenshots dir", "error", err)
		m.doShowToast(ToastOptions{
			Message:  "截图失败：无法获取保存路径",
			Level:    ToastWarn,
			Position: ToastBottomRight,
			Duration: 3000,
		})
		return
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		m.logger.Warn("Screenshot: failed to create screenshots dir", "error", err)
		m.doShowToast(ToastOptions{
			Message:  "截图失败：无法创建目录",
			Level:    ToastWarn,
			Position: ToastBottomRight,
			Duration: 3000,
		})
		return
	}

	ts := time.Now().Format("20060102_150405")
	saved := 0
	copiedToClipboard := false

	// 候选窗口：用当前存储的候选数据重新渲染，直接保存，不推送到 HWND
	m.window.mu.Lock()
	candidateVisible := m.window.visible
	m.window.mu.Unlock()
	if candidateVisible {
		img, _ := m.renderer.RenderCandidates(
			m.candidates, m.input, m.cursorPos,
			m.page, m.totalPages,
			-1, "", m.selectedIndex,
		)
		if img != nil {
			path := filepath.Join(dir, "candidate_"+ts+".png")
			if err := savePNG(img, path); err != nil {
				m.logger.Warn("Screenshot: failed to save candidate", "error", err)
			} else {
				saved++
				m.logger.Info("Screenshot saved", "path", path)
			}
			// 同时复制候选窗口截图到剪贴板，便于粘贴到聊天软件
			if err := clipboard.SetImage(img); err != nil {
				m.logger.Warn("Screenshot: failed to copy candidate to clipboard", "error", err)
			} else {
				copiedToClipboard = true
				m.logger.Info("Screenshot copied to clipboard")
			}
		}
	}

	// 工具栏
	if m.toolbar != nil && m.toolbar.IsVisible() {
		path := filepath.Join(dir, "toolbar_"+ts+".png")
		if err := m.toolbar.CaptureToFile(path); err != nil {
			m.logger.Warn("Screenshot: failed to save toolbar", "error", err)
		} else {
			saved++
			m.logger.Info("Screenshot saved", "path", path)
		}
	}

	// 状态提示
	if m.status != nil && m.status.IsVisible() {
		path := filepath.Join(dir, "status_"+ts+".png")
		if err := m.status.CaptureToFile(path); err != nil {
			m.logger.Warn("Screenshot: failed to save status", "error", err)
		} else {
			saved++
			m.logger.Info("Screenshot saved", "path", path)
		}
	}

	// Toast 通知（通常不可见，有则顺带保存）
	if m.toast != nil && m.toast.IsVisible() {
		path := filepath.Join(dir, "toast_"+ts+".png")
		if err := m.toast.CaptureToFile(path); err != nil {
			m.logger.Warn("Screenshot: failed to save toast", "error", err)
		} else {
			saved++
			m.logger.Info("Screenshot saved", "path", path)
		}
	}

	// Tooltip（悬停提示）
	if m.tooltip != nil && m.tooltip.IsVisible() {
		path := filepath.Join(dir, "tooltip_"+ts+".png")
		if err := m.tooltip.CaptureToFile(path); err != nil {
			m.logger.Warn("Screenshot: failed to save tooltip", "error", err)
		} else {
			saved++
			m.logger.Info("Screenshot saved", "path", path)
		}
	}

	// 右键菜单
	if m.unifiedPopupMenu != nil && m.unifiedPopupMenu.IsVisible() {
		path := filepath.Join(dir, "popup_menu_"+ts+".png")
		if err := m.unifiedPopupMenu.CaptureToFile(path); err != nil {
			m.logger.Warn("Screenshot: failed to save popup menu", "error", err)
		} else {
			saved++
			m.logger.Info("Screenshot saved", "path", path)
		}
	}

	m.logger.Info("UI screenshots taken", "count", saved, "dir", dir)

	if saved > 0 {
		msg := fmt.Sprintf("已保存 %d 张截图\n%s", saved, dir)
		if copiedToClipboard {
			msg = fmt.Sprintf("已保存 %d 张截图（候选窗口已复制到剪贴板）\n%s", saved, dir)
		}
		m.doShowToast(ToastOptions{
			Message:  msg,
			Level:    ToastSuccess,
			Position: ToastBottomRight,
			Duration: 4000,
		})
	} else {
		m.doShowToast(ToastOptions{
			Message:  "没有可见的 UI 窗口可截图",
			Level:    ToastInfo,
			Position: ToastBottomRight,
			Duration: 3000,
		})
	}
}

// savePNG 将 image.RGBA（预乘 alpha）转换为 straight alpha 后编码为 PNG，
// 保留透明度。gg 背景与 DWrite 文字像素现均为合法预乘（文字在 copyToImageRGB
// 中已按 alpha 预乘），故 R_straight = R' × 255/A 可正确还原，不会出现回绕。
func savePNG(img *image.RGBA, path string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("mkdir: %w", err)
	}

	bounds := img.Bounds()
	out := image.NewNRGBA(bounds)
	for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
		for x := bounds.Min.X; x < bounds.Max.X; x++ {
			src := img.RGBAAt(x, y)
			if src.A == 0 {
				out.SetNRGBA(x, y, color.NRGBA{})
			} else if src.A == 255 {
				out.SetNRGBA(x, y, color.NRGBA{R: src.R, G: src.G, B: src.B, A: 255})
			} else {
				a := uint32(src.A)
				out.SetNRGBA(x, y, color.NRGBA{
					R: uint8(uint32(src.R) * 255 / a),
					G: uint8(uint32(src.G) * 255 / a),
					B: uint8(uint32(src.B) * 255 / a),
					A: src.A,
				})
			}
		}
	}

	f, err := os.Create(path)
	if err != nil {
		return fmt.Errorf("create: %w", err)
	}
	defer f.Close()
	if err := png.Encode(f, out); err != nil {
		return fmt.Errorf("encode: %w", err)
	}
	return nil
}
