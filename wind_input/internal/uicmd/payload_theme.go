package uicmd

// ThemeColors 主题色板 (解析后的 wire 形态)。
// 字段保留主要语义点, 具体值在 Go 服务侧从 pkg/theme.ResolvedTheme 转换得到。
//
// 注意: 当前定义只覆盖渲染端必需的最小集合,
// 后续按 macOS 接入实际需求扩展; 新增字段不要破坏旧 wire 兼容,
// 改用尾部追加 + 反序列化端判 buffer 是否结束。
type ThemeColors struct {
	Background       Color
	Border           Color
	Text             Color
	TextSelected     Color
	TextComment      Color
	HighlightBg      Color
	IndexNormal      Color
	IndexSelected    Color
	StatusBg         Color
	StatusText       Color
	ToastInfoAccent  Color
	ToastWarnAccent  Color
	ToastErrorAccent Color
}

// ThemeFonts 主题字体配置 (wire 形态)。
type ThemeFonts struct {
	Family        string // 候选词字体
	Size          float64
	CommentFamily string // 注释字体 (可空 → 等同 Family)
	CommentSize   float64
	IndexFamily   string
	IndexSize     float64
	MenuFamily    string
	MenuSize      float64
	StatusFamily  string
	StatusSize    float64
	ToastFamily   string
	ToastSize     float64
}

// ThemeGeometry 主题几何参数。
type ThemeGeometry struct {
	BorderRadius float64
	BorderWidth  float64
	PaddingX     float64
	PaddingY     float64
	ItemSpacing  float64
	ShadowRadius float64
	ShadowOffset float64
	Opacity      float64
}

// WindowsRenderHints Windows 渲染后端专有提示。
// macOS 端忽略此结构, 用 NSPanel + CoreText 自主决定渲染细节。
type WindowsRenderHints struct {
	TextRenderMode string // "directwrite"/"gdi"/"freetype"
	GDIFontWeight  int32
	GDIFontScale   float64
	MenuFontWeight int32
	MenuFontScale  float64
	MenuFontSize   float64
}

// ThemeApplyPayload 应用解析后的主题。
//
// 设计要点:
//   - 主题 YAML 在 Go 服务侧解析 (SSOT), 此 payload 是解析结果的 wire 镜像。
//   - ThemeID 仅用于日志/调试和 IMKit 端可选缓存。
//   - Style 由"用户选择 + 系统当前外观"在 Go 端解算后下发, 渲染端不再判断。
type ThemeApplyPayload struct {
	ThemeID      string
	Style        ThemeStyle
	Colors       ThemeColors
	Fonts        ThemeFonts
	Geometry     ThemeGeometry
	WindowsHints WindowsRenderHints // Win 渲染后端消费, darwin 忽略
}

func (ThemeApplyPayload) isPayload()               {}
func (ThemeApplyPayload) CommandType() CommandType { return CmdThemeApply }

// ConfigUpdatePayload 全局配置增量更新 (字号、字族等)。
//
// 与 CandidatesConfigPayload 的区别:
//   - CandidatesConfigPayload 限定在候选框相关的布局/可见性参数
//   - ConfigUpdatePayload 是跨多窗口共用的全局视觉参数
type ConfigUpdatePayload struct {
	FontSize     float64
	FontFamily   string
	TooltipDelay int32 // 毫秒, 0=使用默认
	DarkMode     bool  // 是否深色模式 (由 Go 端解算)
	WindowsHints WindowsRenderHints
}

func (ConfigUpdatePayload) isPayload()               {}
func (ConfigUpdatePayload) CommandType() CommandType { return CmdConfigUpdate }

// ============================================================================
// marshal / unmarshal
// ============================================================================

func writeThemeColors(w *binWriter, c ThemeColors) {
	w.writeColor(c.Background)
	w.writeColor(c.Border)
	w.writeColor(c.Text)
	w.writeColor(c.TextSelected)
	w.writeColor(c.TextComment)
	w.writeColor(c.HighlightBg)
	w.writeColor(c.IndexNormal)
	w.writeColor(c.IndexSelected)
	w.writeColor(c.StatusBg)
	w.writeColor(c.StatusText)
	w.writeColor(c.ToastInfoAccent)
	w.writeColor(c.ToastWarnAccent)
	w.writeColor(c.ToastErrorAccent)
}

func readThemeColors(r *binReader, c *ThemeColors) error {
	var err error
	for _, dst := range []*Color{
		&c.Background, &c.Border, &c.Text, &c.TextSelected, &c.TextComment,
		&c.HighlightBg, &c.IndexNormal, &c.IndexSelected,
		&c.StatusBg, &c.StatusText,
		&c.ToastInfoAccent, &c.ToastWarnAccent, &c.ToastErrorAccent,
	} {
		if *dst, err = r.readColor(); err != nil {
			return err
		}
	}
	return nil
}

func writeThemeFonts(w *binWriter, f ThemeFonts) error {
	pairs := []struct {
		s string
		v float64
	}{
		{f.Family, f.Size},
		{f.CommentFamily, f.CommentSize},
		{f.IndexFamily, f.IndexSize},
		{f.MenuFamily, f.MenuSize},
		{f.StatusFamily, f.StatusSize},
		{f.ToastFamily, f.ToastSize},
	}
	for _, p := range pairs {
		if err := w.writeString(p.s); err != nil {
			return err
		}
		w.writeF64(p.v)
	}
	return nil
}

func readThemeFonts(r *binReader, f *ThemeFonts) error {
	targets := []struct {
		s *string
		v *float64
	}{
		{&f.Family, &f.Size},
		{&f.CommentFamily, &f.CommentSize},
		{&f.IndexFamily, &f.IndexSize},
		{&f.MenuFamily, &f.MenuSize},
		{&f.StatusFamily, &f.StatusSize},
		{&f.ToastFamily, &f.ToastSize},
	}
	for _, t := range targets {
		s, err := r.readString()
		if err != nil {
			return err
		}
		*t.s = s
		v, err := r.readF64()
		if err != nil {
			return err
		}
		*t.v = v
	}
	return nil
}

func writeThemeGeometry(w *binWriter, g ThemeGeometry) {
	w.writeF64(g.BorderRadius)
	w.writeF64(g.BorderWidth)
	w.writeF64(g.PaddingX)
	w.writeF64(g.PaddingY)
	w.writeF64(g.ItemSpacing)
	w.writeF64(g.ShadowRadius)
	w.writeF64(g.ShadowOffset)
	w.writeF64(g.Opacity)
}

func readThemeGeometry(r *binReader, g *ThemeGeometry) error {
	var err error
	for _, dst := range []*float64{
		&g.BorderRadius, &g.BorderWidth, &g.PaddingX, &g.PaddingY,
		&g.ItemSpacing, &g.ShadowRadius, &g.ShadowOffset, &g.Opacity,
	} {
		if *dst, err = r.readF64(); err != nil {
			return err
		}
	}
	return nil
}

func writeWindowsHints(w *binWriter, h WindowsRenderHints) error {
	if err := w.writeString(h.TextRenderMode); err != nil {
		return err
	}
	w.writeI32(h.GDIFontWeight)
	w.writeF64(h.GDIFontScale)
	w.writeI32(h.MenuFontWeight)
	w.writeF64(h.MenuFontScale)
	w.writeF64(h.MenuFontSize)
	return nil
}

func readWindowsHints(r *binReader, h *WindowsRenderHints) error {
	var err error
	if h.TextRenderMode, err = r.readString(); err != nil {
		return err
	}
	if h.GDIFontWeight, err = r.readI32(); err != nil {
		return err
	}
	if h.GDIFontScale, err = r.readF64(); err != nil {
		return err
	}
	if h.MenuFontWeight, err = r.readI32(); err != nil {
		return err
	}
	if h.MenuFontScale, err = r.readF64(); err != nil {
		return err
	}
	if h.MenuFontSize, err = r.readF64(); err != nil {
		return err
	}
	return nil
}

func (p ThemeApplyPayload) marshal(w *binWriter) error {
	if err := w.writeString(p.ThemeID); err != nil {
		return err
	}
	if err := w.writeString(string(p.Style)); err != nil {
		return err
	}
	writeThemeColors(w, p.Colors)
	if err := writeThemeFonts(w, p.Fonts); err != nil {
		return err
	}
	writeThemeGeometry(w, p.Geometry)
	return writeWindowsHints(w, p.WindowsHints)
}

func (p *ThemeApplyPayload) unmarshal(r *binReader) error {
	var err error
	if p.ThemeID, err = r.readString(); err != nil {
		return err
	}
	s, err := r.readString()
	if err != nil {
		return err
	}
	p.Style = ThemeStyle(s)
	if err := readThemeColors(r, &p.Colors); err != nil {
		return err
	}
	if err := readThemeFonts(r, &p.Fonts); err != nil {
		return err
	}
	if err := readThemeGeometry(r, &p.Geometry); err != nil {
		return err
	}
	return readWindowsHints(r, &p.WindowsHints)
}

func (p ConfigUpdatePayload) marshal(w *binWriter) error {
	w.writeF64(p.FontSize)
	if err := w.writeString(p.FontFamily); err != nil {
		return err
	}
	w.writeI32(p.TooltipDelay)
	w.writeBool(p.DarkMode)
	return writeWindowsHints(w, p.WindowsHints)
}

func (p *ConfigUpdatePayload) unmarshal(r *binReader) error {
	var err error
	if p.FontSize, err = r.readF64(); err != nil {
		return err
	}
	if p.FontFamily, err = r.readString(); err != nil {
		return err
	}
	if p.TooltipDelay, err = r.readI32(); err != nil {
		return err
	}
	if p.DarkMode, err = r.readBool(); err != nil {
		return err
	}
	return readWindowsHints(r, &p.WindowsHints)
}
