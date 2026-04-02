package ui

import (
	"image"
	"image/color"
	"unsafe"

	"github.com/gogpu/gg"
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
