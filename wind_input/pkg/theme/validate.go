package theme

import (
	"fmt"
	"image/color"
	"strings"
)

// validate.go — V3-D 加载期校验（lint，已定决策 11）。一刀切只读 v3 后无 legacy 兜底，
// 一个 ${typo} 会静默变 transparent → 透明/黑屏难排查。故加载期 fail fast：
//   - 所有 ${token} 引用可达（token 在已解析 colors 表存在）；
//   - 所有 image.ref 在 resources 中存在，或为合法字面 path/data URI；
//   - 颜色字面值（非 ${}/transparent）可解析为 hex。
//
// 循环引用 / 未知 token 在 colors 表内部已由 resolveColorTokens fail fast（refexpand.go）；
// base 链无环由 loadThemeFileWithDir fail fast（manager.go）。本文件补齐**使用点**（views 各节点）
// 的引用校验。失败返回带节点路径的 error，ResolveV3 据此不进入渲染。

// validateViews 校验合并后的 views 各节点颜色 token 引用 + 图片 ref 可达性。
// tokens = 已解析 colors 表（含 primary + derive/auto_dark 后的全部 token）；resources = 主题资源表。
func validateViews(views *Views, tokens map[string]color.Color, resources map[string]ResourceRef) error {
	if views == nil {
		return nil
	}
	v := &viewsValidator{tokens: tokens, resources: resources}
	// 候选窗具名节点（含 states 递归）。
	v.node("window", &views.Window)
	v.node("preedit_bar", &views.PreeditBar)
	v.node("candidate_list", &views.CandidateList)
	v.node("item", &views.Item)
	v.node("index", &views.Index)
	v.node("text", &views.Text)
	v.node("comment", &views.Comment)
	v.node("accent_bar", &views.AccentBar)
	v.node("footer_bar", &views.FooterBar)
	v.node("mode_label", &views.ModeLabel)
	// 其它独立窗口（指针，未配则跳过）。
	v.node("status", views.Status)
	v.node("tooltip", views.Tooltip)
	v.node("toast", views.Toast)
	if tb := views.Toolbar; tb != nil {
		v.fill("toolbar.background", tb.Background)
		v.border("toolbar.border", tb.Border)
		v.node("toolbar.grip", &tb.Grip)
		v.color("toolbar.button.color", tb.Button.Color)
		v.fill("toolbar.button.background", tb.Button.Background)
		v.border("toolbar.button.border", tb.Button.Border)
		if md := tb.Button.Mode; md != nil {
			v.node("toolbar.button.mode.chinese", &md.Chinese)
			v.node("toolbar.button.mode.english", &md.English)
		}
		v.fill("toolbar.settings.background", tb.Settings.Background)
		v.fill("toolbar.settings.icon", tb.Settings.Icon)
		v.fill("toolbar.settings.hole", tb.Settings.Hole)
	}
	if mn := views.Menu; mn != nil {
		v.node("menu.root", &mn.Root)
		v.node("menu.item", &mn.Item)
		v.node("menu.separator", &mn.Separator)
	}
	return v.err
}

// viewsValidator 累积首个校验错误（fail fast，遇错即记并短路后续上报）。
type viewsValidator struct {
	tokens    map[string]color.Color
	resources map[string]ResourceRef
	err       error
}

// node 校验单个 ViewNode（含 background/border/color/layers + states 递归）。nil 跳过。
func (v *viewsValidator) node(path string, n *ViewNode) {
	if v.err != nil || n == nil {
		return
	}
	v.fill(path+".background", n.Background)
	v.border(path+".border", n.Border)
	v.color(path+".color", n.Color)
	for i := range n.Layers {
		v.imageRef(fmt.Sprintf("%s.layers[%d]", path, i), n.Layers[i].Ref)
	}
	if n.Shadow != nil {
		v.color(path+".shadow.color", n.Shadow.Color)
	}
	v.node(path+".selected", n.Selected)
	v.node(path+".hover", n.Hover)
	v.node(path+".disabled", n.Disabled)
}

// fill 校验背景填充：color token + image.ref + gradient stops。
func (v *viewsValidator) fill(path string, f ViewFill) {
	if v.err != nil {
		return
	}
	v.color(path+".color", f.Color)
	if f.Image != nil {
		v.imageRef(path+".image", f.Image.Ref)
	}
	if f.Gradient != nil {
		for i := range f.Gradient.Stops {
			v.color(fmt.Sprintf("%s.gradient.stops[%d]", path, i), f.Gradient.Stops[i].Color)
		}
	}
}

// border 校验边框色 token。
func (v *viewsValidator) border(path string, b ViewBorder) {
	v.color(path+".color", b.Color)
}

// color 校验单个颜色字段（ColorRef：标量或内联 {light,dark}）：两分支分别校验。
func (v *viewsValidator) color(path string, c ColorRef) {
	v.colorStr(path, c.Light)
	if c.Dark != c.Light {
		v.colorStr(path, c.Dark)
	}
}

// colorStr 校验单个颜色 token 字符串：空跳过；transparent 合法；${token} 须可达；其余须可解析为 hex。
func (v *viewsValidator) colorStr(path, s string) {
	if v.err != nil || s == "" || s == "transparent" {
		return
	}
	if strings.HasPrefix(s, "${") && strings.HasSuffix(s, "}") {
		name := s[2 : len(s)-1]
		if _, ok := v.tokens[name]; !ok {
			v.err = fmt.Errorf("views.%s 引用未知颜色 token: %s", path, s)
		}
		return
	}
	if _, err := ParseHexColor(s); err != nil {
		v.err = fmt.Errorf("views.%s 颜色字面值非法（非 ${token}/transparent/合法 hex）: %q", path, s)
	}
}

// imageRef 校验图片 ref：resources 命中、data: URI、或字面 path（含 / \ 或扩展名）均合法；否则报错。
func (v *viewsValidator) imageRef(path, ref string) {
	if v.err != nil || ref == "" {
		return
	}
	if _, ok := v.resources[ref]; ok {
		return
	}
	if strings.HasPrefix(ref, "data:") {
		return
	}
	if strings.ContainsAny(ref, "/\\") || strings.Contains(ref, ".") {
		return // 字面路径（含目录分隔或扩展名）
	}
	v.err = fmt.Errorf("views.%s 引用未知图片 ref（不在 resources、且非合法 path/data URI）: %q", path, ref)
}
