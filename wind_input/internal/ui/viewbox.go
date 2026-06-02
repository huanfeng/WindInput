package ui

// 盒模型 View 渲染引擎（v2.6 主题架构 P1）。
// 设计见 docs/design/theme-view-architecture.md。
//
// 本文件实现引擎的"布局层"：measure（量算各 View 的边框盒尺寸）与 arrange（分配坐标）。
// 布局层不依赖任何图形/字体后端——文本度量通过 TextMeasurer 接口注入，故可纯断言单测。
// 绘制层（paint）在 viewbox_paint.go，生产构建器（从 RenderConfig 构建 View 树）在 viewbox_build.go。

import (
	"image"
	"image/color"
	"math"

	"github.com/huanfeng/wind_input/pkg/theme"
)

// Edges 复用 theme.Padding（Top/Right/Bottom/Left int），作为 margin/padding/slice 的统一表示。
type Edges = theme.Padding

// LayoutKind 决定一个容器 View 如何排布其子节点。
type LayoutKind int

const (
	// LayoutStack 子节点不流动，各自定位在内容盒左上（叠放）。
	LayoutStack LayoutKind = iota
	// LayoutRow 子节点沿水平主轴依次排布（横排候选列表 / 候选项内部）。
	LayoutRow
	// LayoutColumn 子节点沿垂直主轴依次排布（窗口内 band 堆叠 / 竖排候选列表）。
	LayoutColumn
)

// Align 交叉轴对齐方式。
type Align int

const (
	// AlignStart 交叉轴起点对齐。
	AlignStart Align = iota
	// AlignCenter 交叉轴居中对齐。
	AlignCenter
)

// GlyphKind 矢量字形（翻页箭头等，非文本/图片，由 paint 层用 gg 路径绘制）。
type GlyphKind int

const (
	// GlyphNone 无字形。
	GlyphNone GlyphKind = iota
	// GlyphChevronLeft 左向尖括号 ‹（翻页上一页）。
	GlyphChevronLeft
	// GlyphChevronRight 右向尖括号 ›（翻页下一页）。
	GlyphChevronRight
)

// Fill 背景填充：底色 + 可选背景图（画在底色之上，裁剪到 View 圆角内）。
type Fill struct {
	Color   color.Color
	Image   *image.RGBA // nil = 无背景图
	Mode    string      // nine_slice | stretch | tile | center
	Slice   Edges       // 仅 nine_slice
	Opacity float64     // 0..1
}

// Border 描边：宽度/颜色/圆角。描边沿边框盒边缘绘制，不参与布局尺寸（与旧渲染器一致）。
type Border struct {
	Width  int
	Color  color.Color // nil = 不描边
	Radius int
}

// ViewShadow 投影（偏移 + 颜色；无模糊，与旧渲染器 (2,2) 偏移一致）。
type ViewShadow struct {
	OffsetX int
	OffsetY int
	Color   color.Color
}

// ImageLayer 覆盖图层（z 相对内容基准=0；z<0 在内容下，z>0 在内容上）。
// 可以是图片（Img 非空）或纯色矩形（Color 非空，如选中项左侧 accent 强调条）。
type ImageLayer struct {
	Img     *image.RGBA
	Color   color.Color // 纯色层；Img 为空时生效（圆角由 Radius 控制）
	Radius  int         // 纯色层圆角
	Mode    string
	Slice   Edges
	Opacity float64
	Z       int
	Anchor  string // top-left|top|...|center|...|bottom-right；空=top-left
	OffsetX int
	OffsetY int
	W       int // 0 = 原尺寸（纯色层必须显式给）
	H       int // 0 = 原尺寸（纯色层必须显式给）
}

// TextStyle 文本排版属性。
type TextStyle struct {
	FontSize   float64
	LineHeight float64     // 0 = 等于 FontSize
	Weight     int         // 0 = 继承全局；>=600 视为粗体
	Color      color.Color // nil = 不绘制
	Align      Align       // AlignStart=左对齐(默认), AlignCenter=水平居中(如圆内序号)
}

// View 是最小渲染元素（盒模型节点）。一个 View 要么是文本叶子（Text != ""），
// 要么是容器（Children 非空），两者互斥（容器不直接持文本）。
type View struct {
	// 盒模型
	Margin     Edges
	Padding    Edges
	Background Fill
	Border     Border
	Layers     []ImageLayer
	Shadow     *ViewShadow // nil = 无投影；通常只窗口根设置

	// 内容（互斥）
	Text      string    // 非空 = 文本叶子
	TextStyle TextStyle // 仅文本叶子用
	Children  []*View   // 容器子节点

	// 矢量字形叶子（翻页箭头）：Glyph != GlyphNone 时在本 View 矩形中心绘制
	Glyph          GlyphKind
	GlyphColor     color.Color
	GlyphSize      float64
	GlyphLineWidth float64
	Layout         LayoutKind
	Gap            int   // 主轴方向子节点间距（px）
	CrossAlign     Align // 交叉轴对齐
	Stretch        bool  // 交叉轴撑满父内容（列布局=撑满宽、行布局=撑满高）；优先于 CrossAlign
	Grow           bool  // 弹性占位：吸收父容器主轴方向的剩余空间（右对齐/space-between）

	// 固定尺寸覆盖（>0 时覆盖量算出的边框盒尺寸；候选项常用固定行高/列宽对齐）
	FixedW int
	FixedH int

	// 量算/排布结果（引擎填充）
	measuredW int
	measuredH int
	rect      image.Rectangle // 边框盒（不含 margin），绝对坐标
}

// TextMeasurer 文本度量接口；生产用 TextDrawer（MeasureString 同签名），测试可注入桩。
type TextMeasurer interface {
	MeasureString(text string, fontSize float64) float64
}

// Layout 对根 View 执行量算 + 排布，根的边框盒左上角定位到 (x, y)。
func Layout(root *View, x, y int, m TextMeasurer) {
	root.measure(m)
	root.arrange(x, y)
}

// Rect 返回 View 排布后的边框盒（绝对坐标）。须在 Layout 之后调用。
func (v *View) Rect() image.Rectangle { return v.rect }

// measure 自底向上量算边框盒尺寸（不含 margin；Border 不计入尺寸，仅描边）。
func (v *View) measure(m TextMeasurer) (w, h int) {
	var contentW, contentH int

	if v.Text != "" {
		fs := v.TextStyle.FontSize
		contentW = ceil(m.MeasureString(v.Text, fs))
		lh := v.TextStyle.LineHeight
		if lh == 0 {
			lh = fs
		}
		contentH = ceil(lh)
	} else {
		for i, c := range v.Children {
			cw, ch := c.measure(m)
			mbw := cw + c.Margin.Left + c.Margin.Right
			mbh := ch + c.Margin.Top + c.Margin.Bottom
			switch v.Layout {
			case LayoutRow:
				if i > 0 {
					contentW += v.Gap
				}
				contentW += mbw
				if mbh > contentH {
					contentH = mbh
				}
			case LayoutColumn:
				if i > 0 {
					contentH += v.Gap
				}
				contentH += mbh
				if mbw > contentW {
					contentW = mbw
				}
			default: // LayoutStack
				if mbw > contentW {
					contentW = mbw
				}
				if mbh > contentH {
					contentH = mbh
				}
			}
		}
	}

	w = contentW + v.Padding.Left + v.Padding.Right
	h = contentH + v.Padding.Top + v.Padding.Bottom
	if v.FixedW > 0 {
		w = v.FixedW
	}
	if v.FixedH > 0 {
		h = v.FixedH
	}
	v.measuredW, v.measuredH = w, h
	return w, h
}

// arrange 自顶向下分配坐标；(x, y) 为本 View 边框盒左上角（margin 已由父节点扣除）。
func (v *View) arrange(x, y int) {
	v.rect = image.Rect(x, y, x+v.measuredW, y+v.measuredH)
	if len(v.Children) == 0 {
		return
	}

	cx := x + v.Padding.Left
	cy := y + v.Padding.Top
	contentW := v.measuredW - v.Padding.Left - v.Padding.Right
	contentH := v.measuredH - v.Padding.Top - v.Padding.Bottom

	switch v.Layout {
	case LayoutRow:
		grow := distributeGrow(v.Children, v.Gap, contentW, true)
		cur := cx
		for i, c := range v.Children {
			if i > 0 {
				cur += v.Gap
			}
			cur += c.Margin.Left
			if c.Grow {
				c.measuredW = grow
			}
			childY := cy + c.Margin.Top
			if c.Stretch {
				c.measuredH = contentH - c.Margin.Top - c.Margin.Bottom
			} else if v.CrossAlign == AlignCenter {
				childY = cy + (contentH-(c.measuredH+c.Margin.Top+c.Margin.Bottom))/2 + c.Margin.Top
			}
			c.arrange(cur, childY)
			cur += c.measuredW + c.Margin.Right
		}
	case LayoutColumn:
		grow := distributeGrow(v.Children, v.Gap, contentH, false)
		cur := cy
		for i, c := range v.Children {
			if i > 0 {
				cur += v.Gap
			}
			cur += c.Margin.Top
			if c.Grow {
				c.measuredH = grow
			}
			childX := cx + c.Margin.Left
			if c.Stretch {
				c.measuredW = contentW - c.Margin.Left - c.Margin.Right
			} else if v.CrossAlign == AlignCenter {
				childX = cx + (contentW-(c.measuredW+c.Margin.Left+c.Margin.Right))/2 + c.Margin.Left
			}
			c.arrange(childX, cur)
			cur += c.measuredH + c.Margin.Bottom
		}
	default: // LayoutStack
		for _, c := range v.Children {
			c.arrange(cx+c.Margin.Left, cy+c.Margin.Top)
		}
	}
}

func ceil(f float64) int { return int(math.Ceil(f)) }

// distributeGrow 计算每个 Grow 子节点应分得的主轴尺寸（剩余空间均分）。
// horizontal=true 用宽度，否则高度。无 Grow 子节点或无剩余空间时返回 0。
func distributeGrow(children []*View, gap, content int, horizontal bool) int {
	fixed := 0
	nGrow := 0
	for i, c := range children {
		if i > 0 {
			fixed += gap
		}
		if c.Grow {
			nGrow++
			continue
		}
		if horizontal {
			fixed += c.measuredW + c.Margin.Left + c.Margin.Right
		} else {
			fixed += c.measuredH + c.Margin.Top + c.Margin.Bottom
		}
	}
	if nGrow == 0 || content <= fixed {
		return 0
	}
	return (content - fixed) / nGrow
}
