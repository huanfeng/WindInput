package ui

import (
	"image"
	"image/color"
	"unsafe"

	"github.com/fogleman/gg"
)

// menuTextItem holds deferred text drawing info for Phase 2
type menuTextItem struct {
	text     string
	x, y     float64
	fontSize float64
	clr      color.Color
}

// render renders the menu to an image
func (m *PopupMenu) render() *image.RGBA {
	m.mu.Lock()
	items := m.items
	hoverIdx := m.hoverIndex
	submenuIdx := m.submenuIndex
	width := m.width
	height := m.height
	hasChecked := m.hasChecked
	hasChildren := m.hasChildren
	colors := m.getPopupMenuColors()
	td := m.textDrawer
	baseFontSize := m.getMenuFontSize()
	m.mu.Unlock()

	scale := GetDPIScale()
	fontSize := baseFontSize * scale
	itemH := int(float64(m.getMenuItemHeight()) * scale)
	sepH := int(float64(menuSeparatorHeight) * scale)
	padX := float64(menuPaddingX) * scale
	padY := int(float64(menuPaddingY) * scale)
	checkW := 0.0
	if hasChecked {
		checkW = float64(menuCheckMarkWidth) * scale
	}
	arrowW := 0.0
	if hasChildren {
		arrowW = float64(menuArrowWidth) * scale
	}

	dc := gg.NewContext(width, height)

	// Calculate corner radius with DPI scaling
	radius := float64(menuCornerRadius) * scale

	// ========== Phase 1: Draw all shapes with gg ==========

	// Fill background with rounded rectangle
	dc.SetRGBA(1, 1, 1, 0) // Transparent background first
	dc.Clear()

	dc.SetColor(colors.BackgroundColor)
	dc.DrawRoundedRectangle(0.5, 0.5, float64(width)-1, float64(height)-1, radius)
	dc.Fill()

	// Set clip to rounded rectangle so hover backgrounds don't overflow
	dc.DrawRoundedRectangle(1, 1, float64(width)-2, float64(height)-2, radius-1)
	dc.Clip()

	// Collect ALL text items (including symbols) for Phase 2
	var textItems []menuTextItem

	// Draw items
	y := padY
	for i, item := range items {
		if item.Separator {
			sepY := float64(y + sepH/2)
			dc.SetColor(colors.SeparatorColor)
			dc.DrawLine(4*scale, sepY, float64(width)-4*scale, sepY)
			dc.Stroke()
			y += sepH
		} else {
			isHovered := (i == hoverIdx && !item.Disabled) || (i == submenuIdx)

			// Draw item background
			if isHovered {
				dc.SetColor(colors.HoverBgColor)
				dc.DrawRectangle(1, float64(y), float64(width-2), float64(itemH))
				dc.Fill()
			}

			// Collect check mark for Phase 2 (unified text rendering)
			if item.Checked {
				var symColor color.Color
				if item.Disabled {
					symColor = colors.DisabledColor
				} else if isHovered {
					symColor = colors.HoverTextColor
				} else {
					symColor = colors.TextColor
				}
				cx := padX/2 + checkW/2
				cy := float64(y) + float64(itemH)/2 + fontSize/3
				sw := td.MeasureString("✓", fontSize)
				textItems = append(textItems, menuTextItem{
					text: "✓", x: cx - sw/2, y: cy, fontSize: fontSize, clr: symColor,
				})
			}

			// Collect menu item text for Phase 2
			var textColor color.Color
			if item.Disabled {
				textColor = colors.DisabledColor
			} else if isHovered {
				textColor = colors.HoverTextColor
			} else {
				textColor = colors.TextColor
			}
			textX := padX + checkW
			textY := float64(y) + float64(itemH)/2 + fontSize/3
			textItems = append(textItems, menuTextItem{
				text: item.Text, x: textX, y: textY, fontSize: fontSize, clr: textColor,
			})

			// Collect submenu arrow for Phase 2 (unified text rendering)
			if len(item.Children) > 0 {
				var arrowColor color.Color
				if item.Disabled {
					arrowColor = colors.DisabledColor
				} else if isHovered {
					arrowColor = colors.HoverTextColor
				} else {
					arrowColor = colors.TextColor
				}
				ax := float64(width) - padX/2 - arrowW/2
				ay := float64(y) + float64(itemH)/2 + fontSize/3
				sw := td.MeasureString("▸", fontSize)
				textItems = append(textItems, menuTextItem{
					text: "▸", x: ax - sw/2, y: ay, fontSize: fontSize, clr: arrowColor,
				})
			}

			y += itemH
		}
	}

	// Reset clip and draw border
	dc.ResetClip()
	dc.SetColor(colors.BorderColor)
	dc.DrawRoundedRectangle(0.5, 0.5, float64(width)-1, float64(height)-1, radius)
	dc.Stroke()

	// ========== Phase 2: Draw ALL text (items + symbols) with TextDrawer ==========
	img := dc.Image().(*image.RGBA)
	td.BeginDraw(img)
	for _, t := range textItems {
		td.DrawString(t.text, t.x, t.y, t.fontSize, t.clr)
	}
	td.EndDraw()

	return img
}

// updateWindow updates the layered window with the rendered image
func (m *PopupMenu) updateWindow() {
	img := m.render()

	m.mu.Lock()
	x, y := m.x, m.y
	width, height := m.width, m.height
	m.mu.Unlock()

	hdcScreen, _, _ := procGetDC.Call(0)
	if hdcScreen == 0 {
		return
	}
	defer procReleaseDC.Call(0, hdcScreen)

	hdcMem, _, _ := procCreateCompatibleDC.Call(hdcScreen)
	if hdcMem == 0 {
		return
	}
	defer procDeleteDC.Call(hdcMem)

	bi := BITMAPINFO{
		BmiHeader: BITMAPINFOHEADER{
			BiSize:        uint32(unsafe.Sizeof(BITMAPINFOHEADER{})),
			BiWidth:       int32(width),
			BiHeight:      -int32(height),
			BiPlanes:      1,
			BiBitCount:    32,
			BiCompression: BI_RGB,
		},
	}

	var bits unsafe.Pointer
	hBitmap, _, _ := procCreateDIBSection.Call(
		hdcMem,
		uintptr(unsafe.Pointer(&bi)),
		DIB_RGB_COLORS,
		uintptr(unsafe.Pointer(&bits)),
		0, 0,
	)
	if hBitmap == 0 {
		return
	}
	defer procDeleteObject.Call(hBitmap)

	procSelectObject.Call(hdcMem, hBitmap)

	// Copy image data (RGBA to BGRA with premultiplied alpha)
	pixelCount := width * height
	dstSlice := unsafe.Slice((*byte)(bits), pixelCount*4)

	for i := 0; i < pixelCount; i++ {
		srcIdx := i * 4
		dstIdx := i * 4

		r := img.Pix[srcIdx+0]
		g := img.Pix[srcIdx+1]
		b := img.Pix[srcIdx+2]
		a := img.Pix[srcIdx+3]

		if a == 255 {
			dstSlice[dstIdx+0] = b
			dstSlice[dstIdx+1] = g
			dstSlice[dstIdx+2] = r
			dstSlice[dstIdx+3] = a
		} else if a == 0 {
			dstSlice[dstIdx+0] = 0
			dstSlice[dstIdx+1] = 0
			dstSlice[dstIdx+2] = 0
			dstSlice[dstIdx+3] = 0
		} else {
			dstSlice[dstIdx+0] = byte(uint16(b) * uint16(a) / 255)
			dstSlice[dstIdx+1] = byte(uint16(g) * uint16(a) / 255)
			dstSlice[dstIdx+2] = byte(uint16(r) * uint16(a) / 255)
			dstSlice[dstIdx+3] = a
		}
	}

	ptSrc := POINT{X: 0, Y: 0}
	ptDst := POINT{X: int32(x), Y: int32(y)}
	size := SIZE{Cx: int32(width), Cy: int32(height)}
	blend := BLENDFUNCTION{
		BlendOp:             AC_SRC_OVER,
		BlendFlags:          0,
		SourceConstantAlpha: 255,
		AlphaFormat:         AC_SRC_ALPHA,
	}

	procUpdateLayeredWindow.Call(
		uintptr(m.hwnd),
		hdcScreen,
		uintptr(unsafe.Pointer(&ptDst)),
		uintptr(unsafe.Pointer(&size)),
		hdcMem,
		uintptr(unsafe.Pointer(&ptSrc)),
		0,
		uintptr(unsafe.Pointer(&blend)),
		ULW_ALPHA,
	)
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
