package theme

import (
	"image/color"
	"path/filepath"
	"runtime"
	"testing"
)

// bitmapTestThemeDirs 返回加载位图测试主题 jidian-classic 所需的 themeDirs：
// jidian-classic 是**纯测试主题**（不随发布打包），位于 pkg/theme/testdata/themes/；
// 其继承的基础主题 _base 仍在源 themes/ 下（base 单链解析跨 themeDirs 兜底）。
func bitmapTestThemeDirs(t *testing.T) []string {
	t.Helper()
	_, file, _, _ := runtime.Caller(0)
	base := filepath.Dir(file) // .../wind_input/pkg/theme
	testThemes := filepath.Join(base, "testdata", "themes")
	srcThemes := filepath.Join(base, "..", "..", "themes") // _layouts / _palettes
	return []string{testThemes, srcThemes}
}

// TestBitmapSkinTheme_JidianClassic 端到端验收 P7-C/D/E 位图能力（冻结判据④的雏形）：
// 加载测试位图皮肤 jidian-classic，验证 resources（含 {light,dark}）解析、window.background.image
// 解析进 RVNode.BgImage、背景图能被实际解码、选中高亮位图与暗色变体。
func TestBitmapSkinTheme_JidianClassic(t *testing.T) {
	m := &Manager{themeDirs: bitmapTestThemeDirs(t)}
	if err := m.LoadTheme("jidian-classic"); err != nil {
		t.Fatalf("LoadTheme jidian-classic: %v", err)
	}
	rv := m.GetResolvedV25()
	if rv == nil || rv.Views == nil {
		t.Fatal("resolved/views nil")
	}

	// resources：panel 应解析为绝对路径（相对 theme.yaml）。
	p, ok := rv.Resources["panel"]
	if !ok || p == "" {
		t.Fatalf("resources[panel] 缺失或为空: %v", rv.Resources)
	}

	// window.background.image 应解析进 RVNode.BgImage（spec）。
	cv := ResolveCandidateViews(*rv.Views, rv.Palette)
	bg := cv.Window.BgImage
	if bg == nil {
		t.Fatal("window.background.image 未解析进 RVNode.BgImage")
	}
	if bg.Ref != "panel" || bg.Mode != "nine_slice" {
		t.Errorf("bg spec ref/mode 错: %+v", bg)
	}
	if bg.Slice.Top != 8 || bg.Slice.Left != 8 {
		t.Errorf("bg slice 应为 8: %+v", bg.Slice)
	}

	// 背景图能被实际解码（panel.png 真实存在于主题目录）。
	img, err := LoadBackgroundImage(p)
	if err != nil || img == nil {
		t.Fatalf("背景图解码失败: path=%s err=%v", p, err)
	}

	// P7-C2：window.layers 水印层解析进 RVNode.Layers，且 ref 能解析+解码。
	if mp, ok := rv.Resources["mark"]; !ok || mp == "" {
		t.Fatalf("resources[mark] 缺失: %v", rv.Resources)
	}
	if len(cv.Window.Layers) != 1 {
		t.Fatalf("window 应有 1 个水印层, got %d", len(cv.Window.Layers))
	}
	if l := cv.Window.Layers[0]; l.Ref != "mark" || l.Z != 1 || l.Anchor != "bottom-right" {
		t.Errorf("水印层 spec 错: %+v", l)
	}
	if mimg, merr := LoadBackgroundImage(rv.Resources["mark"]); merr != nil || mimg == nil {
		t.Fatalf("水印图解码失败: %v", merr)
	}

	// P7-D：选中态高亮位图——item.selected 解析出 BgImage(ref=sel) + 白色加粗文字。
	if sel := cv.Item.Selected; sel == nil || sel.BgImage == nil || sel.BgImage.Ref != "sel" {
		t.Fatalf("item.selected 应带高亮位图 sel: %+v", cv.Item.Selected)
	}
	if cv.Item.Selected.TextColor == nil || cv.Item.Selected.FontWeight != 600 {
		t.Errorf("选中态应白字加粗: color=%v weight=%d", cv.Item.Selected.TextColor, cv.Item.Selected.FontWeight)
	}
	if sp, ok := rv.Resources["sel"]; !ok || sp == "" {
		t.Fatalf("resources[sel] 缺失: %v", rv.Resources)
	}
	if simg, serr := LoadBackgroundImage(rv.Resources["sel"]); serr != nil || simg == nil {
		t.Fatalf("选中高亮图解码失败: %v", serr)
	}

	// P8 切片6：其它窗口（status/tooltip/menu/toast）也吃背景图 + layers。
	// 验证字段名命中（yaml.v3 静默忽略不认的字段，故须断言确实解析进 RVNode 而非被丢弃）。
	sv := ResolveStatusViews(rv.Views.Status, rv.Palette)
	if sv.BgImage == nil || sv.BgImage.Ref != "panel" {
		t.Errorf("status.background.image 未解析进 RVNode.BgImage: %+v", sv.BgImage)
	}
	if len(sv.Layers) != 1 || sv.Layers[0].Ref != "mark" {
		t.Errorf("status.layers 未解析: %+v", sv.Layers)
	}
	tv := ResolveTooltipViews(rv.Views.Tooltip, rv.Palette)
	if tv.BgImage == nil || tv.BgImage.Ref != "panel" {
		t.Errorf("tooltip.background.image 未解析: %+v", tv.BgImage)
	}
	if len(tv.Layers) != 1 || tv.Layers[0].Ref != "mark" {
		t.Errorf("tooltip.layers 未解析: %+v", tv.Layers)
	}
	tov := ResolveToastViews(rv.Views.Toast, rv.Palette)
	if tov.BgImage == nil || tov.BgImage.Ref != "panel" {
		t.Errorf("toast.background.image 未解析: %+v", tov.BgImage)
	}
	if len(tov.Layers) != 1 || tov.Layers[0].Ref != "mark" {
		t.Errorf("toast.layers 未解析: %+v", tov.Layers)
	}
	mv := ResolveMenuViews(rv.Views.Menu, rv.Palette)
	if mv.Root.BgImage == nil || mv.Root.BgImage.Ref != "panel" {
		t.Errorf("menu.root.background.image 未解析: %+v", mv.Root.BgImage)
	}
	if len(mv.Root.Layers) != 1 || mv.Root.Layers[0].Ref != "mark" {
		t.Errorf("menu.root.layers 未解析: %+v", mv.Root.Layers)
	}
	// menu 新 schema（root/item/separator）：item.hover patch 应命中（旧扁平写法早已失效）。
	if mv.Item.Hover == nil || mv.Item.Hover.BgColor == nil {
		t.Errorf("menu.item.hover 未解析进 RVState: %+v", mv.Item.Hover)
	}

	// P8 切片6 圆角深色毛边修复守护：配了浅色背景图的窗口，底色必须不透明（α=255），
	// 否则深色半透明底色会在圆角抗锯齿边缘透出形成深色毛边（详见 ui/viewbox_corner_test.go）。
	opaque := func(name string, c color.Color) {
		if c == nil {
			t.Errorf("%s 底色为 nil", name)
			return
		}
		if _, _, _, a := c.RGBA(); a>>8 != 0xFF {
			t.Errorf("%s 底色应不透明(α=255)以避免圆角深色毛边，got α=%d", name, a>>8)
		}
	}
	opaque("status.background", sv.BgColor)
	opaque("tooltip.background", tv.BgColor)
	opaque("toast.background", tov.BgColor)

	// 同时验收 P7-A/B：accent bar 启用、序号无圆背景（极点风格）。V3-D：accent_bar 几何归位到节点。
	if ab := rv.Views.AccentBar; ab.Enabled == nil || !*ab.Enabled {
		t.Error("jidian-classic 应启用 accent_bar")
	}
	if rv.Views.Index.Background.Shape == "circle" {
		t.Error("jidian-classic 序号应为 none（纯数字）")
	}

	// P7-E：结构化阴影 offset_x/y 解析（blur 预留不消费）。V3-D：shadow 归位到 window 节点。
	if rv.Views.Window.Shadow == nil || rv.Views.Window.Shadow.OffsetX == nil {
		t.Error("jidian-classic 应配结构化 shadow")
	}

	// P7-E：暗色位图变体——切暗色后 panel/sel 应指向 *-dark.png（mark 单图不变）。
	lightPanel := rv.Resources["panel"]
	m.SetDarkMode(true)
	dk := m.GetResolvedV25()
	if dk.Resources["panel"] == lightPanel {
		t.Errorf("切暗色后 panel 应换 dark 变体, 仍为 %s", dk.Resources["panel"])
	}
	if filepath.Base(dk.Resources["panel"]) != "panel-dark.png" {
		t.Errorf("暗色 panel 应为 panel-dark.png, got %s", dk.Resources["panel"])
	}
	if filepath.Base(dk.Resources["mark"]) != "mark.png" {
		t.Errorf("单图 mark 暗色应不变, got %s", dk.Resources["mark"])
	}
}
