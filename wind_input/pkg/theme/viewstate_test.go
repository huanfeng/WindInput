package theme

import (
	"image/color"
	"testing"
)

// TestResolveState_GradientLayersKept 守护状态态补齐：只配渐变/层的状态 patch 不再被
// "有无覆盖"判定当空 patch 丢弃（渐变/层已支持）；只配几何的仍丢弃（state_geometry unsupported）。
func TestResolveState_GradientLayersKept(t *testing.T) {
	noColor := func(ColorRef) color.Color { return nil } // 颜色全 nil，隔离出渐变/层/几何的判定

	// 只配渐变 → 保留（非 nil）。
	gradOnly := &ViewNode{Background: ViewFill{Gradient: &ViewGradient{Stops: []ViewGradientStop{{}, {}}}}}
	if resolveState(gradOnly, nil, nil, noColor) == nil {
		t.Error("只配渐变的状态 patch 不应被丢弃")
	}

	// 只配覆盖层 → 保留。
	layerOnly := &ViewNode{Layers: []ViewImage{{Ref: "x"}}}
	rv := resolveState(layerOnly, nil, nil, noColor)
	if rv == nil {
		t.Fatal("只配 layers 的状态 patch 不应被丢弃")
	}
	if len(rv.Layers) != 1 {
		t.Errorf("状态 patch 应解析出 Layers, got %d", len(rv.Layers))
	}

	// 只配几何（padding）→ 仍丢弃（几何不随状态变，避免跳动）。
	geomOnly := &ViewNode{Padding: ViewEdges{Top: dimp(4)}}
	if resolveState(geomOnly, nil, nil, noColor) != nil {
		t.Error("只配几何的状态 patch 应被丢弃（state_geometry unsupported）")
	}
}
