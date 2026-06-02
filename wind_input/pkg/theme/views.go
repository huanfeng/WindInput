package theme

// views.go — 盒模型 View 主题 schema（v2.6 P2）。
//
// 两类形态：
//   - YAML schema（Views/ViewNode，*int 显式语义）：主题文件 views: 块的解析目标，
//     沿用 v2.5「nil=未写回退基线，非 nil（含 0）=显式值」。
//   - 渲染消费形态（ResolvedViews/RVNode，plain 值 + 已解析颜色）：候选窗 build 直接读取。
//
// 本切片（切片-0）YAML 可配字段 = margin/padding/border/background.color/font/color/labels/states；
// 其余尺寸（item height、window gap、shadow、accent 宽、派生公式输入）由渲染器从运行时配置/
// 内置默认提供，ResolvedViews 以额外字段承载。Image/layers/gradient 留后续切片。

import "image/color"

// ViewEdges 四向距离。*Dimension：nil=未写（回退基线），非 nil（含 0）=显式值。
// 值支持 px/dp 单位（裸数字=dp，"Npx"=设备像素不缩放），见 Dimension。
type ViewEdges struct {
	Top    *Dimension `yaml:"top,omitempty"`
	Right  *Dimension `yaml:"right,omitempty"`
	Bottom *Dimension `yaml:"bottom,omitempty"`
	Left   *Dimension `yaml:"left,omitempty"`
}

// ViewImagePoint 覆盖图偏移（逻辑像素）。
type ViewImagePoint struct {
	X int `yaml:"x,omitempty"`
	Y int `yaml:"y,omitempty"`
}

// ViewImageSize 覆盖图尺寸（逻辑像素）；0=原图尺寸。
type ViewImageSize struct {
	W int `yaml:"w,omitempty"`
	H int `yaml:"h,omitempty"`
}

// ViewImage 通用图片对象（P0 D5）。背景填充图与 layers[] 覆盖图共用此唯一类型。
// ref 优先查顶层 resources[ref]，否则按字面 path / data: URI 解析；
// 引擎当前支持 mode: nine_slice | stretch | tile | center（其它值回退 stretch）。
// z/anchor/offset/size 仅 layers[] 覆盖图消费；背景填充用 ViewFill.Image 时忽略它们。
type ViewImage struct {
	Ref     string         `yaml:"ref,omitempty"`     // resources 键，或字面 path / data: URI
	Mode    string         `yaml:"mode,omitempty"`    // nine_slice | stretch | tile | center；空=stretch
	Slice   ViewEdges      `yaml:"slice,omitempty"`   // 仅 nine_slice：源图四边切片像素
	Opacity *float64       `yaml:"opacity,omitempty"` // nil=1.0
	Z       int            `yaml:"z,omitempty"`       // 仅 layers[]：内容基准 0，<0 在内容下、>0 在上
	Anchor  string         `yaml:"anchor,omitempty"`  // 仅覆盖图：top-left|top|...|center|...|bottom-right
	Offset  ViewImagePoint `yaml:"offset,omitempty"`  // 仅覆盖图
	Size    ViewImageSize  `yaml:"size,omitempty"`    // 仅覆盖图：0=原尺寸
}

// ViewFill 背景填充。Color 底色 + 可选 Image（画在底色之上、裁剪到圆角内）。
// Gradient 为 P7-E 预留字段（schema 冻结，渲染 later）；与 Color 概念互斥（同时存在时 render later 决定优先级）。
type ViewFill struct {
	Color    string        `yaml:"color,omitempty"`    // ColorToken: "#RRGGBB[AA]" | "${semantic}" | "transparent"
	Shape    string        `yaml:"shape,omitempty"`    // 背景形状: "circle" | "none"（空=none）。当前仅 views.index 消费（序号项圆形/无背景）
	Image    *ViewImage    `yaml:"image,omitempty"`    // 背景填充图（P7-C，D5）；nil=无图
	Gradient *ViewGradient `yaml:"gradient,omitempty"` // 渐变填充（P7-E 预留：schema 冻结，渲染 later）；nil=无渐变
}

// ViewGradient 渐变填充（P7-E 预留字段形状）。当前仅定义 schema、不参与渲染（RVNode 不消费）。
// 设计为 CSS 风格：linear（默认）按 Angle 方向，多色停 Stops 任意位置。
type ViewGradient struct {
	Type  string             `yaml:"type,omitempty"`  // "linear"（默认）| "radial"（预留）
	Angle float64            `yaml:"angle,omitempty"` // linear 角度（度）：0=左→右、90=上→下
	Stops []ViewGradientStop `yaml:"stops,omitempty"` // 色停列表（≥2）
}

// ViewGradientStop 渐变色停。
type ViewGradientStop struct {
	Color string  `yaml:"color"`         // ColorToken
	Pos   float64 `yaml:"pos,omitempty"` // 0..1（沿渐变轴的位置）
}

// ViewBorder 边框。
type ViewBorder struct {
	Width  *Dimension `yaml:"width,omitempty"` // 支持 px/dp：边框常用 "1px" 发丝线（不随 DPI 加粗）
	Color  string     `yaml:"color,omitempty"`
	Radius *Dimension `yaml:"radius,omitempty"`
}

// ViewNode 一个具名 View 的外观属性（盒模型 + Text 属性）。
type ViewNode struct {
	Margin     ViewEdges   `yaml:"margin,omitempty"`
	Padding    ViewEdges   `yaml:"padding,omitempty"`
	Background ViewFill    `yaml:"background,omitempty"`
	Border     ViewBorder  `yaml:"border,omitempty"`
	FontFamily string      `yaml:"font_family,omitempty"`
	FontSize   *int        `yaml:"font_size,omitempty"` // 相对主候选字体的有符号偏移(逻辑px)：-4=base-4、+2=base+2；nil/0=同主字体。随用户主字号同步缩放，零魔法数字
	FontWeight *int        `yaml:"font_weight,omitempty"`
	Color      string      `yaml:"color,omitempty"`  // 文本色 token
	Labels     []string    `yaml:"labels,omitempty"` // 仅 index：序号槽位字符（≤10）
	Layers     []ViewImage `yaml:"layers,omitempty"` // z 层级覆盖图（P7-C，D4）：z<0 在内容下、z>0 在上
	Selected   *ViewNode   `yaml:"selected,omitempty"`
	Hover      *ViewNode   `yaml:"hover,omitempty"`
	Disabled   *ViewNode   `yaml:"disabled,omitempty"` // P7-D：禁用态 patch（候选项暂无运行时触发器，schema 预留）
}

// Views 具名 View 集合（固定骨架，设计文档 D3）。
type Views struct {
	Window        ViewNode      `yaml:"window,omitempty"`
	PreeditBar    ViewNode      `yaml:"preedit_bar,omitempty"`
	CandidateList ViewNode      `yaml:"candidate_list,omitempty"`
	Item          ViewNode      `yaml:"item,omitempty"`
	Index         ViewNode      `yaml:"index,omitempty"`
	Text          ViewNode      `yaml:"text,omitempty"`
	Comment       ViewNode      `yaml:"comment,omitempty"`
	AccentBar     ViewNode      `yaml:"accent_bar,omitempty"`
	FooterBar     ViewNode      `yaml:"footer_bar,omitempty"`
	ModeLabel     ViewNode      `yaml:"mode_label,omitempty"` // 临时拼音等模式徽标（预编辑栏内）：font_size 相对偏移 + color
	Status        *ViewNode     `yaml:"status,omitempty"`     // P4-A 状态泡（独立窗口，单节点）
	Tooltip       *ViewNode     `yaml:"tooltip,omitempty"`    // P4-B Tooltip（独立窗口，单节点）
	Toolbar       *ToolbarViews `yaml:"toolbar,omitempty"`    // P4-C 工具栏
	Menu          *MenuViews    `yaml:"menu,omitempty"`       // P4-D 弹出菜单
	Metrics       *ViewMetrics  `yaml:"metrics,omitempty"`    // P6 候选窗列表级几何
}

// ViewMetrics 候选窗"列表级"几何（P6）：不便归入单个 ViewNode 的尺寸。全部可空指针，nil=走 defaultViews 基线。
type ViewMetrics struct {
	ItemSpacing  *Dimension        `yaml:"item_spacing,omitempty"`  // 横排候选框间距基数（旧 hardcode 12/16）
	BandGap      *Dimension        `yaml:"band_gap,omitempty"`      // band 间距（旧 WindowGap）
	ShadowOffset *Dimension        `yaml:"shadow_offset,omitempty"` // 窗口投影偏移（标量，legacy；新主题用 shadow）
	Shadow       *ViewShadowSpec   `yaml:"shadow,omitempty"`        // 结构化投影（P7-E）：offset_x/y/color 已实现，blur/spread 预留
	AccentBar    *AccentBarMetrics `yaml:"accent_bar,omitempty"`    // 强调条尺寸
}

// ViewShadowSpec 结构化窗口投影（P7-E）。offset_x/offset_y/color 已实现；blur/spread 为预留字段（渲染 later）。
// 优先级：Shadow 非 nil 时其 offset/color 覆盖 legacy shadow_offset + palette.Shadow；未给的子字段回退。
type ViewShadowSpec struct {
	OffsetX *Dimension `yaml:"offset_x,omitempty"` // 水平偏移
	OffsetY *Dimension `yaml:"offset_y,omitempty"` // 垂直偏移
	Blur    *Dimension `yaml:"blur,omitempty"`     // 模糊半径（P7-E 预留，渲染 later）
	Spread  *Dimension `yaml:"spread,omitempty"`   // 扩散（P7-E 预留，渲染 later）
	Color   string     `yaml:"color,omitempty"`    // ColorToken；空=palette.Shadow
}

// AccentBarMetrics 强调条尺寸（P6）+ 开关（P7-5：HasAccentBar 归口此处，原 layout.accent_bar.enabled 退役）。
type AccentBarMetrics struct {
	Enabled     *bool      `yaml:"enabled,omitempty"`      // 是否绘制选中候选左侧强调条；nil/false=不绘制
	Width       *Dimension `yaml:"width,omitempty"`        // 条宽
	Offset      *Dimension `yaml:"offset,omitempty"`       // 左缘偏移
	HeightRatio *float64   `yaml:"height_ratio,omitempty"` // 条高 = ItemHeight × 此比例
}

// RVImage 渲染消费形态的图片 spec（plain 值；不含解码后的位图——位图由 ui 侧按 Ref 一次性解码缓存）。
// P7-C：ResolveCandidateViews 每帧从 ViewImage 廉价转换填入；ui build 时 Ref→缓存位图→Fill/ImageLayer。
type RVImage struct {
	Ref     string  // resources 键或字面 path/data URI
	Mode    string  // nine_slice|stretch|tile|center；空=stretch
	Slice   Padding // 仅 nine_slice
	Opacity float64 // 已解析（nil→1.0）
	Z       int     // 仅 layers：内容基准 0
	Anchor  string  // 仅覆盖图
	OffsetX int     // 仅覆盖图
	OffsetY int     // 仅覆盖图
	W       int     // 仅覆盖图：0=原尺寸
	H       int     // 仅覆盖图：0=原尺寸
}

// RVState 渲染消费形态的状态 patch（P7-D）：selected/hover/disabled 对基态的覆盖。
// 各字段零值/nil = 该属性不覆盖、沿用基态。BgImage 非空 = 该态铺高亮位图（搜狗/极点选中态核心）。
// 文字色/字重作用于整行（候选文字 + 序号 + 注释）。
type RVState struct {
	BgColor     color.Color // nil=沿用基态底色
	BgImage     *RVImage    // 非空=该态铺高亮位图（优先于 BgColor）
	TextColor   color.Color // nil=沿用基态文字色（整行统一）
	BorderColor color.Color // nil=沿用基态边框色
	BorderWidth *Dimension  // nil=沿用基态边框宽（含显式 0）；支持 px/dp
	FontWeight  int         // 0=沿用基态字重
}

// RVNode 渲染消费形态的单个 View 外观（plain 逻辑像素 + 颜色）。
// 各字段为该 View 实际用到的子集；零值表示「用渲染器内置默认」。
// 状态 patch（Selected/Hover/Disabled，P7-D）仅 Item 节点填充；nil=该态无覆盖。
type RVNode struct {
	MarginTop, MarginRight, MarginBottom, MarginLeft Dimension
	PadTop, PadRight, PadBottom, PadLeft             Dimension
	BorderRadius                                     Dimension
	BorderWidth                                      Dimension
	BorderColor                                      color.Color
	BgColor                                          color.Color
	FontSize                                         float64
	FontWeight                                       int
	FontFamily                                       string // 平台字体族名（空=继承全局）；未知名由平台文本引擎回退
	TextColor                                        color.Color
	Selected                                         *RVState  // P7-D：选中态 patch（仅 Item）
	Hover                                            *RVState  // P7-D：悬停态 patch（仅 Item）
	Disabled                                         *RVState  // P7-D：禁用态 patch（schema 预留，暂无渲染触发器）
	BgImage                                          *RVImage  // 背景填充图（P7-C）；nil=无
	Layers                                           []RVImage // z 层级覆盖图（P7-C）
}

// ResolvedViews 候选窗各具名 View 的解析后外观（plain 逻辑像素，渲染器直接读）。
// 派生布局公式（index 圆直径、comment/pager 字号、inputH 等）留在渲染器，其输入从这里取。
// 几何杂项（Window/Item 等不便归入单 View margin/padding 的尺寸）以顶层字段承载。
type ResolvedViews struct {
	Window        RVNode
	PreeditBar    RVNode
	CandidateList RVNode
	Item          RVNode
	Index         RVNode
	Text          RVNode
	Comment       RVNode
	AccentBar     RVNode
	FooterBar     RVNode
	ModeLabel     RVNode // 临时拼音等模式徽标（预编辑栏内）

	// 几何杂项（Dimension 带 px/dp 单位 / 倍率）
	WindowGap        Dimension   // window 列间距（band 之间）
	ShadowOffset     Dimension   // 窗口投影偏移（标量，legacy；= ShadowOffsetX/Y 的同值兜底）
	ShadowOffsetX    Dimension   // 窗口投影水平偏移（P7-E：来自 metrics.shadow.offset_x，未配=ShadowOffset）
	ShadowOffsetY    Dimension   // 窗口投影垂直偏移（P7-E：来自 metrics.shadow.offset_y，未配=ShadowOffset）
	ItemHeight       float64     // 行高（rowH = round(ItemHeight)）
	ItemSpacing      Dimension   // 横排候选框间距基数（已按 isTextIndex 选定 12/16）
	AccentBarWidth   Dimension   // 强调条宽
	AccentBarOffset  Dimension   // 强调条左缘偏移
	AccentBarHRatio  float64     // 强调条高 = ItemHeight * 此比例
	VerticalMaxWidth float64     // 竖排最大宽（逻辑像素）
	ShadowColor      color.Color // 窗口投影颜色（P2 切片-1）
}

// ResolvedStatusViews 状态泡解析后外观（P4-A）。仅颜色——几何/字号由运行时 StatusWindowConfig 提供。
type ResolvedStatusViews struct {
	BgColor   color.Color
	TextColor color.Color
}

// ResolvedTooltipViews Tooltip 解析后外观（P4-B）。仅颜色——几何由 render 内置默认（hardcode）。
type ResolvedTooltipViews struct {
	BgColor   color.Color
	TextColor color.Color
}

// ToolbarViews 工具栏 YAML schema（P4-C）。button base + mode 状态覆盖 + settings 齿轮色。
type ToolbarViews struct {
	Background ViewFill            `yaml:"background,omitempty"`
	Border     ViewBorder          `yaml:"border,omitempty"`
	Grip       ViewNode            `yaml:"grip,omitempty"`
	Button     ToolbarButtonNode   `yaml:"button,omitempty"`
	Settings   ToolbarSettingsNode `yaml:"settings,omitempty"`
}

// ToolbarButtonNode 按钮通用 base（background/color）+ mode 状态覆盖。
type ToolbarButtonNode struct {
	Background ViewFill           `yaml:"background,omitempty"`
	Color      string             `yaml:"color,omitempty"`
	Border     ViewBorder         `yaml:"border,omitempty"`
	Mode       *ToolbarModeStates `yaml:"mode,omitempty"`
}

// ToolbarModeStates 模式按钮中/英两态覆盖（仅 background）。
type ToolbarModeStates struct {
	Chinese ViewNode `yaml:"chinese,omitempty"`
	English ViewNode `yaml:"english,omitempty"`
}

// ToolbarSettingsNode 设置按钮：background（继承 button base 若空）+ 齿轮 icon/hole 色。
type ToolbarSettingsNode struct {
	Background ViewFill `yaml:"background,omitempty"`
	Icon       ViewFill `yaml:"icon,omitempty"`
	Hole       ViewFill `yaml:"hole,omitempty"`
}

// ResolvedToolbarViews 工具栏解析后扁平颜色集（P4-C）。几何 hardcode；mode 中/英 build 按 state 选。
type ResolvedToolbarViews struct {
	BarBg, BarBorder, Grip                 color.Color
	ButtonBg, ButtonText                   color.Color // base（width/punct/settings 共用）
	ModeChineseBg, ModeEnglishBg, ModeText color.Color
	SettingsBg, SettingsIcon, SettingsHole color.Color
}

// MenuViews 弹出菜单 YAML schema（P4-D）。7 色：背景/边框/文本/分隔/禁用 + hover 状态。
type MenuViews struct {
	Background ViewFill       `yaml:"background,omitempty"`
	Border     ViewBorder     `yaml:"border,omitempty"`
	Color      string         `yaml:"color,omitempty"`     // 普通文本
	Separator  ViewFill       `yaml:"separator,omitempty"` // 分隔线色（用 .Color）
	Disabled   string         `yaml:"disabled,omitempty"`  // 禁用文本
	Hover      MenuHoverState `yaml:"hover,omitempty"`
}

// MenuHoverState 菜单项 hover 覆盖：背景 + 文本。
type MenuHoverState struct {
	Background ViewFill `yaml:"background,omitempty"`
	Color      string   `yaml:"color,omitempty"`
}

// ResolvedMenuViews 菜单解析后扁平 7 色集（P4-D）。
type ResolvedMenuViews struct {
	BgColor, BorderColor, TextColor             color.Color
	DisabledColor, HoverBgColor, HoverTextColor color.Color
	SeparatorColor                              color.Color
}

func intp(v int) *int { return &v }

func f64p(v float64) *float64 { return &v }

// dimp 返回指向 dp 尺寸的指针（基线/默认用；px 单位由主题 YAML 显式写 "Npx"）。
func dimp(v int) *Dimension { d := Dimension{Value: v}; return &d }

// edgeOr 返回指针值或回退默认（保留显式 0）。用于 *int 字段（字号/字重）。
func edgeOr(p *int, def int) int {
	if p != nil {
		return *p
	}
	return def
}

// dimOr 返回 Dimension 指针值或回退默认（保留显式 0）。用于几何字段（带 px/dp 单位）。
func dimOr(p *Dimension, def Dimension) Dimension {
	if p != nil {
		return *p
	}
	return def
}

// defaultViews 返回与现 viewbox_build magic number 等价的 YAML 基线（density=compact 量级，
// 逻辑像素，未乘 DPI scale）。仅覆盖本切片 YAML 可配字段；颜色/字号由 palette 与运行时提供，
// 故基线不含颜色。主题 views 块以此为基线覆盖。
func defaultViews() Views {
	return Views{
		Window:     ViewNode{Padding: ViewEdges{Top: dimp(8), Right: dimp(8), Bottom: dimp(8), Left: dimp(8)}, Border: ViewBorder{Width: dimp(1), Radius: dimp(8)}},
		PreeditBar: ViewNode{Padding: ViewEdges{Right: dimp(8), Left: dimp(8)}, Border: ViewBorder{Radius: dimp(4)}},
		Item:       ViewNode{Padding: ViewEdges{Right: dimp(8), Left: dimp(8)}, Border: ViewBorder{Radius: dimp(4)}},
		Index:      ViewNode{FontSize: intp(-4), Labels: []string{"1", "2", "3", "4", "5", "6", "7", "8", "9", "0"}}, // 序号字号默认 base-4（小号）；主题可覆盖 labels / background.shape / font_size
		Text:       ViewNode{Margin: ViewEdges{Left: dimp(4)}},                                                       // index→text 间距（text 字号=base，偏移 0）
		Comment:    ViewNode{FontSize: intp(-4), Margin: ViewEdges{Left: dimp(8)}},                                   // 注释字号默认 base-4；text→comment 间距
		AccentBar:  ViewNode{},
		FooterBar:  ViewNode{FontSize: intp(-4)},                                                    // 翻页/页码字号默认 base-4
		ModeLabel:  ViewNode{FontSize: intp(-4), Padding: ViewEdges{Left: dimp(8), Right: dimp(8)}}, // 模式徽标字号默认 base-4、左右 padding 8（与输入编码分隔）；颜色默认 ${comment}
		Metrics: &ViewMetrics{
			ItemSpacing:  dimp(12),
			BandGap:      dimp(2),
			ShadowOffset: dimp(2),
			AccentBar:    &AccentBarMetrics{Width: dimp(3), Offset: dimp(1), HeightRatio: f64p(0.6)},
		},
	}
}

// mergeEdges 用 ov 的非 nil 字段覆盖 base（保留显式 0）。
func mergeEdges(base, ov ViewEdges) ViewEdges {
	out := base
	if ov.Top != nil {
		out.Top = ov.Top
	}
	if ov.Right != nil {
		out.Right = ov.Right
	}
	if ov.Bottom != nil {
		out.Bottom = ov.Bottom
	}
	if ov.Left != nil {
		out.Left = ov.Left
	}
	return out
}

// mergeViewNode 用 ov 覆盖 base：指针字段非 nil、string 非空、slice 非 nil 时覆盖；
// Selected/Hover 子 patch 递归同规则。
func mergeViewNode(base, ov ViewNode) ViewNode {
	out := base
	out.Margin = mergeEdges(base.Margin, ov.Margin)
	out.Padding = mergeEdges(base.Padding, ov.Padding)
	if ov.Background.Color != "" {
		out.Background.Color = ov.Background.Color
	}
	if ov.Background.Shape != "" {
		out.Background.Shape = ov.Background.Shape
	}
	if ov.Background.Image != nil {
		out.Background.Image = ov.Background.Image
	}
	if ov.Background.Gradient != nil {
		out.Background.Gradient = ov.Background.Gradient // 整体替换（P7-E 预留）
	}
	if ov.Border.Width != nil {
		out.Border.Width = ov.Border.Width
	}
	if ov.Border.Color != "" {
		out.Border.Color = ov.Border.Color
	}
	if ov.Border.Radius != nil {
		out.Border.Radius = ov.Border.Radius
	}
	if ov.FontFamily != "" {
		out.FontFamily = ov.FontFamily
	}
	if ov.FontSize != nil {
		out.FontSize = ov.FontSize
	}
	if ov.FontWeight != nil {
		out.FontWeight = ov.FontWeight
	}
	if ov.Color != "" {
		out.Color = ov.Color
	}
	if ov.Labels != nil {
		out.Labels = ov.Labels
	}
	if ov.Layers != nil {
		out.Layers = ov.Layers // 整组替换，不做逐层 deep-merge
	}
	if ov.Selected != nil {
		var baseSel ViewNode
		if base.Selected != nil {
			baseSel = *base.Selected
		}
		merged := mergeViewNode(baseSel, *ov.Selected)
		out.Selected = &merged
	}
	if ov.Hover != nil {
		var baseHover ViewNode
		if base.Hover != nil {
			baseHover = *base.Hover
		}
		merged := mergeViewNode(baseHover, *ov.Hover)
		out.Hover = &merged
	}
	if ov.Disabled != nil {
		var baseDis ViewNode
		if base.Disabled != nil {
			baseDis = *base.Disabled
		}
		merged := mergeViewNode(baseDis, *ov.Disabled)
		out.Disabled = &merged
	}
	return out
}

// mergeViews 把 override（通常来自主题 YAML）逐具名 View 合并到 base（通常是 defaultViews 基线）。
func mergeViews(base, ov Views) Views {
	out := Views{
		Window:        mergeViewNode(base.Window, ov.Window),
		PreeditBar:    mergeViewNode(base.PreeditBar, ov.PreeditBar),
		CandidateList: mergeViewNode(base.CandidateList, ov.CandidateList),
		Item:          mergeViewNode(base.Item, ov.Item),
		Index:         mergeViewNode(base.Index, ov.Index),
		Text:          mergeViewNode(base.Text, ov.Text),
		Comment:       mergeViewNode(base.Comment, ov.Comment),
		AccentBar:     mergeViewNode(base.AccentBar, ov.AccentBar),
		FooterBar:     mergeViewNode(base.FooterBar, ov.FooterBar),
		ModeLabel:     mergeViewNode(base.ModeLabel, ov.ModeLabel),
		Metrics:       mergeMetrics(base.Metrics, ov.Metrics),
	}
	// 独立窗口 views（Status/Tooltip/Toolbar/Menu）整体透传：ov 非 nil 取 ov，否则 base 兜底。
	// 这 4 个是独立窗口的完整外观定义（P4），不做深度 merge——与现状 rv.Views 整体透传语义一致。
	out.Status = base.Status
	if ov.Status != nil {
		out.Status = ov.Status
	}
	out.Tooltip = base.Tooltip
	if ov.Tooltip != nil {
		out.Tooltip = ov.Tooltip
	}
	out.Toolbar = base.Toolbar
	if ov.Toolbar != nil {
		out.Toolbar = ov.Toolbar
	}
	out.Menu = base.Menu
	if ov.Menu != nil {
		out.Menu = ov.Menu
	}
	return out
}

// mergeMetrics 用 ov 的非 nil 字段覆盖 base（任一为 nil 取另一方；均非 nil 逐字段覆盖）。
func mergeMetrics(base, ov *ViewMetrics) *ViewMetrics {
	if ov == nil {
		return base
	}
	if base == nil {
		return ov
	}
	out := *base
	if ov.ItemSpacing != nil {
		out.ItemSpacing = ov.ItemSpacing
	}
	if ov.BandGap != nil {
		out.BandGap = ov.BandGap
	}
	if ov.ShadowOffset != nil {
		out.ShadowOffset = ov.ShadowOffset
	}
	if ov.Shadow != nil {
		out.Shadow = mergeShadowSpec(out.Shadow, ov.Shadow)
	}
	if ov.AccentBar != nil {
		out.AccentBar = mergeAccentBarMetrics(out.AccentBar, ov.AccentBar)
	}
	return &out
}

// mergeAccentBarMetrics 同 mergeMetrics 的逐字段覆盖语义。
func mergeAccentBarMetrics(base, ov *AccentBarMetrics) *AccentBarMetrics {
	if ov == nil {
		return base
	}
	if base == nil {
		return ov
	}
	out := *base
	if ov.Enabled != nil {
		out.Enabled = ov.Enabled
	}
	if ov.Width != nil {
		out.Width = ov.Width
	}
	if ov.Offset != nil {
		out.Offset = ov.Offset
	}
	if ov.HeightRatio != nil {
		out.HeightRatio = ov.HeightRatio
	}
	return &out
}

// mergeShadowSpec 逐字段覆盖（P7-E）：指针非 nil / string 非空 才覆盖。
func mergeShadowSpec(base, ov *ViewShadowSpec) *ViewShadowSpec {
	if ov == nil {
		return base
	}
	if base == nil {
		return ov
	}
	out := *base
	if ov.OffsetX != nil {
		out.OffsetX = ov.OffsetX
	}
	if ov.OffsetY != nil {
		out.OffsetY = ov.OffsetY
	}
	if ov.Blur != nil {
		out.Blur = ov.Blur
	}
	if ov.Spread != nil {
		out.Spread = ov.Spread
	}
	if ov.Color != "" {
		out.Color = ov.Color
	}
	return &out
}
