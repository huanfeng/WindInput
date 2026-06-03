//go:build windows

package ui

import (
	"image"
	"unsafe"
)

// render renders the menu to an image（盒模型 View 引擎）。
// root LayoutColumn + 每项 LayoutRow（check/text/arrow 文本叶子）；分隔线后处理。
func (m *PopupMenu) render() *image.RGBA {
	m.mu.Lock()
	items := m.items
	hoverIdx := m.hoverIndex
	submenuIdx := m.submenuIndex
	width := m.width
	height := m.height
	hasChecked := m.hasChecked
	hasChildren := m.hasChildren
	rmv := m.resolveMenuViews()
	td := m.textDrawer
	baseFontSize := m.getMenuFontSize()
	var resources map[string]string
	if m.resolvedV25 != nil {
		resources = m.resolvedV25.Resources
	}
	m.mu.Unlock()

	scale := m.dpiScale()
	itemHeightLogical := m.getMenuItemHeight()

	mt := buildMenuTree(items, hoverIdx, submenuIdx, hasChecked, hasChildren, rmv, width, height, baseFontSize, itemHeightLogical, scale, &m.imgRes, resources)
	Layout(mt.root, 0, 0, td)
	dc, img := newSharedDrawContext(width, height)

	// 内圆角 clip：把 root 背景 + hover 满宽高亮裁剪到「边框内侧的圆角」，使首/末项 hover
	// 不溢出圆角、也不覆盖圆角边框（保持满宽方块风格，不改 UI 自定义部分）。
	// 代价：root 边框会被 clip 裁掉外半，故下方在 clip 之外重画完整圆角边框。
	radius := rmv.Root.BorderRadius.Scaled(scale)
	if radius == 0 {
		radius = int(float64(menuCornerRadius) * scale)
	}
	bw := rmv.Root.BorderWidth.Scaled(scale)
	if bw == 0 {
		bw = 1
	}
	innerR := radius - bw
	if innerR < 0 {
		innerR = 0
	}
	dc.DrawRoundedRectangle(float64(bw), float64(bw), float64(width-2*bw), float64(height-2*bw), float64(innerR))
	dc.Clip()

	PaintTree(mt.root, dc, img, td)

	// 后处理分隔线（矢量，定位用分隔项 Rect()；在 clip 内）
	for _, sep := range mt.separators {
		r := sep.Rect()
		sepY := float64(r.Min.Y) + float64(r.Dy())/2
		dc.SetColor(rmv.Separator.BgColor)
		dc.DrawLine(4*scale, sepY, float64(width)-4*scale, sepY)
		dc.Stroke()
	}

	// 完整 root 圆角边框：在内圆角 clip 之外绘制，画在最上，圆角轮廓完整（与旧 paintShapes 边框等价）。
	dc.ResetClip()
	if bc := rmv.Root.BorderColor; bc != nil {
		half := float64(bw) / 2
		dc.SetColor(bc)
		dc.SetLineWidth(float64(bw))
		dc.DrawRoundedRectangle(half, half, float64(width)-2*half, float64(height)-2*half, float64(radius))
		dc.Stroke()
	}

	DrawDebugBanner(img)
	return img
}

// updateWindow updates the layered window with the rendered image
func (m *PopupMenu) updateWindow() {
	img := m.render()

	m.mu.Lock()
	x, y := m.x, m.y
	m.mu.Unlock()

	UpdateLayeredWindowFromImage(m.hwnd, img, x, y)
}

// trackMouseLeave enables mouse leave tracking
func (m *PopupMenu) trackMouseLeave() {
	tme := TRACKMOUSEEVENT{
		CbSize:    uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})),
		DwFlags:   TME_LEAVE,
		HwndTrack: uintptr(m.hwnd),
	}
	procTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
}
