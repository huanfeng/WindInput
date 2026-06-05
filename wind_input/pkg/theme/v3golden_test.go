package theme

import (
	"fmt"
	"image/color"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
)

// v3golden_test.go — v3 重构的**回归基准脚手架**（V3-0）。
//
// 判据（贯穿 V3-0~V3-4）：每个切片后，v3 主题渲染产物（解析后的渲染消费形态）必须逐项
// 等于既有基线。本测试对内置主题 default/msime × light/dark 产出确定性快照，存为
// testdata/golden_v3/*.txt（首次运行落盘 baseline）。后续 v3 改造跑同一快照逐字节对比。
//
// 落盘/更新基线：设环境变量 UPDATE_GOLDEN=1 重跑（结构调整后用，须人工核对 diff 中的「值」未变）。
//
// 设计意图：snapshot 读 ResolveCandidateViews / ResolveXxxViews 等**公开解析 API 的产物**，
// 而非内部字段——v3 各切片会改这些 API 的内部实现（token 来源、palette 结构、几何归位），
// 但其产出的颜色/几何**值**必须不变；快照能精确抓住任何漂移。

func colorHex(c color.Color) string {
	if c == nil {
		return "nil"
	}
	r, g, b, a := c.RGBA()
	return fmt.Sprintf("#%02X%02X%02X%02X", r>>8, g>>8, b>>8, a>>8)
}

func dimStr(d Dimension) string {
	unit := "dp"
	if d.Px {
		unit = "px"
	}
	return fmt.Sprintf("%d%s", d.Value, unit)
}

func dumpRVImage(b *strings.Builder, indent, label string, im *RVImage) {
	if im == nil {
		return
	}
	fmt.Fprintf(b, "%s%s: ref=%s mode=%s slice=%v op=%.2f z=%d anchor=%s off=(%d,%d) wh=(%d,%d)\n",
		indent, label, im.Ref, im.Mode, im.Slice, im.Opacity, im.Z, im.Anchor, im.OffsetX, im.OffsetY, im.W, im.H)
}

// dumpRVState 输出状态 patch（V3-D 改为递归 *RVNode）。输出格式与旧 *RVState 完全一致以守 golden：
// BorderWidth 从旧 *Dimension（nil 表示未设）改为 Dimension（零值表示未设），故零值时仍输出 "nil"。
func dumpRVState(b *strings.Builder, indent, label string, st *RVNode) {
	if st == nil {
		return
	}
	bw := "nil"
	if st.BorderWidth != (Dimension{}) {
		bw = dimStr(st.BorderWidth)
	}
	fmt.Fprintf(b, "%s%s: bg=%s text=%s border=%s bw=%s fw=%d\n",
		indent, label, colorHex(st.BgColor), colorHex(st.TextColor), colorHex(st.BorderColor), bw, st.FontWeight)
	dumpRVImage(b, indent+"  ", label+".bgImage", st.BgImage)
}

func dumpRVNode(b *strings.Builder, name string, n RVNode) {
	fmt.Fprintf(b, "[%s]\n", name)
	fmt.Fprintf(b, "  margin: %s %s %s %s\n", dimStr(n.MarginTop), dimStr(n.MarginRight), dimStr(n.MarginBottom), dimStr(n.MarginLeft))
	fmt.Fprintf(b, "  padding: %s %s %s %s\n", dimStr(n.PadTop), dimStr(n.PadRight), dimStr(n.PadBottom), dimStr(n.PadLeft))
	fmt.Fprintf(b, "  border: radius=%s width=%s color=%s\n", dimStr(n.BorderRadius), dimStr(n.BorderWidth), colorHex(n.BorderColor))
	fmt.Fprintf(b, "  bg=%s text=%s\n", colorHex(n.BgColor), colorHex(n.TextColor))
	fmt.Fprintf(b, "  font: size=%.1f weight=%d family=%q\n", n.FontSize, n.FontWeight, n.FontFamily)
	dumpRVImage(b, "  ", "bgImage", n.BgImage)
	for i := range n.Layers {
		dumpRVImage(b, "  ", fmt.Sprintf("layer[%d]", i), &n.Layers[i])
	}
	dumpRVState(b, "  ", "selected", n.Selected)
	dumpRVState(b, "  ", "hover", n.Hover)
	dumpRVState(b, "  ", "disabled", n.Disabled)
}

func snapshotResolvedV3(rv *ResolvedV3) string {
	var b strings.Builder

	b.WriteString("=== palette (顶层语义色，token 源) ===\n")
	p := rv.Palette
	fmt.Fprintf(&b, "isDark=%v primary=%s\n", p.IsDark, colorHex(p.Primary))
	fmt.Fprintf(&b, "bg=%s surface=%s border=%s text=%s textDim=%s textHint=%s accent=%s onAccent=%s shadow=%s\n",
		colorHex(p.Bg), colorHex(p.Surface), colorHex(p.Border), colorHex(p.Text),
		colorHex(p.TextDim), colorHex(p.TextHint), colorHex(p.Accent), colorHex(p.OnAccent), colorHex(p.Shadow))

	if rv.Views != nil {
		b.WriteString("\n=== candidate views ===\n")
		cv := ResolveCandidateViews(*rv.Views, rv.Palette)
		dumpRVNode(&b, "window", cv.Window)
		dumpRVNode(&b, "preedit_bar", cv.PreeditBar)
		dumpRVNode(&b, "candidate_list", cv.CandidateList)
		dumpRVNode(&b, "item", cv.Item)
		dumpRVNode(&b, "index", cv.Index)
		dumpRVNode(&b, "text", cv.Text)
		dumpRVNode(&b, "comment", cv.Comment)
		dumpRVNode(&b, "accent_bar", cv.AccentBar)
		dumpRVNode(&b, "footer_bar", cv.FooterBar)
		dumpRVNode(&b, "mode_label", cv.ModeLabel)
		fmt.Fprintf(&b, "[geom] windowGap=%s shadowOffset=%s shadowXY=(%s,%s) itemSpacing=%s rowGap=%s accentBar=(w=%s off=%s hr=%.2f) shadowColor=%s\n",
			dimStr(cv.WindowGap), dimStr(cv.ShadowOffset), dimStr(cv.ShadowOffsetX), dimStr(cv.ShadowOffsetY),
			dimStr(cv.ItemSpacing), dimStr(cv.RowGap), dimStr(cv.AccentBarWidth), dimStr(cv.AccentBarOffset), cv.AccentBarHRatio, colorHex(cv.ShadowColor))

		b.WriteString("\n=== other windows ===\n")
		dumpRVNode(&b, "status", ResolveStatusViews(rv.Views.Status, rv.Palette))
		dumpRVNode(&b, "tooltip", ResolveTooltipViews(rv.Views.Tooltip, rv.Palette))
		dumpRVNode(&b, "toast", ResolveToastViews(rv.Views.Toast, rv.Palette))
		mv := ResolveMenuViews(rv.Views.Menu, rv.Palette)
		dumpRVNode(&b, "menu.root", mv.Root)
		dumpRVNode(&b, "menu.item", mv.Item)
		dumpRVNode(&b, "menu.separator", mv.Separator)
	}

	b.WriteString("\n=== behavior ===\n")
	fmt.Fprintf(&b, "%+v\n", rv.Behavior)

	b.WriteString("\n=== resources ===\n")
	keys := make([]string, 0, len(rv.Resources))
	for k := range rv.Resources {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		// 仅记录键 + 是否为 data URI / 文件名（绝对路径含机器差异，取 base）。
		v := rv.Resources[k]
		if strings.HasPrefix(v, "data:") {
			fmt.Fprintf(&b, "%s = <data-uri>\n", k)
		} else {
			fmt.Fprintf(&b, "%s = %s\n", k, filepath.Base(v))
		}
	}

	return b.String()
}

func compareOrWriteGolden(t *testing.T, goldenPath, got string) {
	t.Helper()
	update := os.Getenv("UPDATE_GOLDEN") == "1"
	want, err := os.ReadFile(goldenPath)
	if os.IsNotExist(err) || update {
		if mkErr := os.MkdirAll(filepath.Dir(goldenPath), 0o755); mkErr != nil {
			t.Fatalf("创建 golden 目录: %v", mkErr)
		}
		if wErr := os.WriteFile(goldenPath, []byte(got), 0o644); wErr != nil {
			t.Fatalf("写 golden: %v", wErr)
		}
		t.Logf("已落盘基线 %s（%d 字节）", goldenPath, len(got))
		return
	}
	if err != nil {
		t.Fatalf("读 golden %s: %v", goldenPath, err)
	}
	if string(want) != got {
		t.Errorf("快照与基线不符 %s\n--- 提示：若为 v3 结构调整且值未变，核对后用 UPDATE_GOLDEN=1 重落 ---\n%s",
			goldenPath, firstDiff(string(want), got))
	}
}

// firstDiff 返回首个不同行的上下文（便于定位漂移）。
func firstDiff(want, got string) string {
	wl, gl := strings.Split(want, "\n"), strings.Split(got, "\n")
	n := min(len(gl), len(wl))
	for i := range n {
		if wl[i] != gl[i] {
			return fmt.Sprintf("行 %d:\n  want: %s\n  got:  %s", i+1, wl[i], gl[i])
		}
	}
	if len(wl) != len(gl) {
		return fmt.Sprintf("行数不同: want %d, got %d", len(wl), len(gl))
	}
	return "(无逐行差异，可能尾部空白)"
}

// TestV3GoldenSnapshot 落盘/校验 v3 渲染消费形态基线（回归基准）。
func TestV3GoldenSnapshot(t *testing.T) {
	dirs := bitmapTestThemeDirs(t)
	for _, name := range []string{"default", "msime"} {
		for _, dark := range []bool{false, true} {
			mode := "light"
			if dark {
				mode = "dark"
			}
			t.Run(name+"_"+mode, func(t *testing.T) {
				m := &Manager{themeDirs: dirs}
				if err := m.LoadTheme(name); err != nil {
					t.Fatalf("LoadTheme %s: %v", name, err)
				}
				m.SetDarkMode(dark)
				rv := m.GetResolvedV3()
				if rv == nil {
					t.Fatalf("%s: resolvedV3 为 nil", name)
				}
				snap := snapshotResolvedV3(rv)
				golden := filepath.Join("testdata", "golden_v3", fmt.Sprintf("%s_%s.txt", name, mode))
				compareOrWriteGolden(t, golden, snap)
			})
		}
	}
}
