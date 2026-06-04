package theme

import (
	"testing"

	"gopkg.in/yaml.v3"
)

// views_metrics_test.go — V3-D 属性归位后的「列表级几何」基线/覆盖/解析测试。
// 旧 metrics 杂物抽屉删除，几何归位到 candidate_list（gap/band_gap）、window（shadow）、accent_bar 节点。

func TestDefaultViews_GeometryDefaults(t *testing.T) {
	v := defaultViews()
	if v.CandidateList.Gap == nil || v.CandidateList.Gap.Value != 12 {
		t.Errorf("candidate_list.gap 基线应为 12, got %v", v.CandidateList.Gap)
	}
	if v.CandidateList.BandGap == nil || v.CandidateList.BandGap.Value != 2 {
		t.Errorf("candidate_list.band_gap 基线应为 2, got %v", v.CandidateList.BandGap)
	}
	if v.Window.Shadow == nil || v.Window.Shadow.OffsetX == nil || v.Window.Shadow.OffsetX.Value != 2 {
		t.Errorf("window.shadow.offset_x 基线应为 2, got %+v", v.Window.Shadow)
	}
	if v.AccentBar.Width == nil || v.AccentBar.Width.Value != 3 {
		t.Errorf("accent_bar.width 基线应为 3, got %+v", v.AccentBar)
	}
}

func TestMergeViews_GeometryOverride(t *testing.T) {
	base := defaultViews()
	ov := Views{CandidateList: ViewNode{Gap: dimp(16)}}
	merged := mergeViews(base, ov)
	if merged.CandidateList.Gap == nil || merged.CandidateList.Gap.Value != 16 {
		t.Errorf("candidate_list.gap 应被覆盖为 16, got %+v", merged.CandidateList)
	}
	if merged.CandidateList.BandGap == nil || merged.CandidateList.BandGap.Value != 2 {
		t.Errorf("band_gap 应保持基线 2, got %v", merged.CandidateList.BandGap)
	}
}

func TestGeometry_YAMLParse(t *testing.T) {
	src := []byte("candidate_list:\n  gap: 14\naccent_bar:\n  enabled: true\n  width: 5\n")
	var v Views
	if err := yaml.Unmarshal(src, &v); err != nil {
		t.Fatalf("yaml 解析失败: %v", err)
	}
	if v.CandidateList.Gap == nil || v.CandidateList.Gap.Value != 14 {
		t.Errorf("candidate_list.gap 应解析为 14, got %+v", v.CandidateList)
	}
	if v.AccentBar.Width == nil || v.AccentBar.Width.Value != 5 {
		t.Errorf("accent_bar.width 应解析为 5, got %+v", v.AccentBar)
	}
	if v.AccentBar.Enabled == nil || !*v.AccentBar.Enabled {
		t.Errorf("accent_bar.enabled 应解析为 true, got %+v", v.AccentBar.Enabled)
	}
}

func TestMergeViews_PreservesIndependentWindows(t *testing.T) {
	base := defaultViews() // 不含 Status/Tooltip/Toolbar/Menu
	ov := Views{
		Status:  &ViewNode{Color: "${text}"},
		Tooltip: &ViewNode{Color: "${text}"},
		Toast:   &ViewNode{Color: "${text}"},
		Toolbar: &ToolbarViews{},
		Menu:    &MenuViews{},
	}
	merged := mergeViews(base, ov)
	if merged.Status == nil || merged.Tooltip == nil || merged.Toast == nil || merged.Toolbar == nil || merged.Menu == nil {
		t.Errorf("mergeViews 应透传 5 个独立窗口字段, got Status=%v Tooltip=%v Toast=%v Toolbar=%v Menu=%v",
			merged.Status, merged.Tooltip, merged.Toast, merged.Toolbar, merged.Menu)
	}
	merged2 := mergeViews(base, Views{})
	if merged2.Status != nil || merged2.Tooltip != nil || merged2.Toast != nil || merged2.Toolbar != nil || merged2.Menu != nil {
		t.Errorf("base/ov 均无独立窗口字段时结果应为 nil, got %+v", merged2)
	}
	// 候选窗几何归位到节点后仍应保留 candidate_list 默认。
	if merged.CandidateList.Gap == nil {
		t.Error("mergeViews 仍应保留 candidate_list 几何默认")
	}
}
