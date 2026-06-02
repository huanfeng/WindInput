package theme

import (
	"testing"

	"gopkg.in/yaml.v3"
)

func TestDefaultViews_Metrics(t *testing.T) {
	v := defaultViews()
	if v.Metrics == nil {
		t.Fatal("defaultViews().Metrics 不应为 nil")
	}
	if v.Metrics.ItemSpacing == nil || v.Metrics.ItemSpacing.Value != 12 {
		t.Errorf("item_spacing 基线应为 12, got %v", v.Metrics.ItemSpacing)
	}
	if v.Metrics.BandGap == nil || v.Metrics.BandGap.Value != 2 {
		t.Errorf("band_gap 基线应为 2, got %v", v.Metrics.BandGap)
	}
	if v.Metrics.ShadowOffset == nil || v.Metrics.ShadowOffset.Value != 2 {
		t.Errorf("shadow_offset 基线应为 2, got %v", v.Metrics.ShadowOffset)
	}
	if v.Metrics.AccentBar == nil || v.Metrics.AccentBar.Width == nil || v.Metrics.AccentBar.Width.Value != 3 {
		t.Errorf("accent_bar.width 基线应为 3, got %+v", v.Metrics.AccentBar)
	}
}

func TestMergeViews_MetricsOverride(t *testing.T) {
	base := defaultViews()
	ov := Views{Metrics: &ViewMetrics{ItemSpacing: dimp(16)}}
	merged := mergeViews(base, ov)
	if merged.Metrics == nil || merged.Metrics.ItemSpacing == nil || merged.Metrics.ItemSpacing.Value != 16 {
		t.Errorf("item_spacing 应被覆盖为 16, got %+v", merged.Metrics)
	}
	if merged.Metrics.BandGap == nil || merged.Metrics.BandGap.Value != 2 {
		t.Errorf("band_gap 应保持基线 2, got %v", merged.Metrics.BandGap)
	}
}

func TestViewMetrics_YAMLParse(t *testing.T) {
	src := []byte("metrics:\n  item_spacing: 14\n  accent_bar:\n    width: 5\n")
	var v Views
	if err := yaml.Unmarshal(src, &v); err != nil {
		t.Fatalf("yaml 解析失败: %v", err)
	}
	if v.Metrics == nil || v.Metrics.ItemSpacing == nil || v.Metrics.ItemSpacing.Value != 14 {
		t.Errorf("item_spacing 应解析为 14, got %+v", v.Metrics)
	}
	if v.Metrics.AccentBar == nil || v.Metrics.AccentBar.Width == nil || v.Metrics.AccentBar.Width.Value != 5 {
		t.Errorf("accent_bar.width 应解析为 5, got %+v", v.Metrics)
	}
}

func TestMergeViews_PreservesIndependentWindows(t *testing.T) {
	base := defaultViews() // 不含 Status/Tooltip/Toolbar/Menu
	ov := Views{
		Status:  &ViewNode{Color: "${text}"},
		Tooltip: &ViewNode{Color: "${text}"},
		Toolbar: &ToolbarViews{},
		Menu:    &MenuViews{},
	}
	merged := mergeViews(base, ov)
	if merged.Status == nil || merged.Tooltip == nil || merged.Toolbar == nil || merged.Menu == nil {
		t.Errorf("mergeViews 应透传 4 个独立窗口字段, got Status=%v Tooltip=%v Toolbar=%v Menu=%v",
			merged.Status, merged.Tooltip, merged.Toolbar, merged.Menu)
	}
	merged2 := mergeViews(base, Views{})
	if merged2.Status != nil || merged2.Tooltip != nil || merged2.Toolbar != nil || merged2.Menu != nil {
		t.Errorf("base/ov 均无独立窗口字段时结果应为 nil, got %+v", merged2)
	}
	if merged.Metrics == nil {
		t.Error("mergeViews 仍应保留候选窗 Metrics")
	}
}
