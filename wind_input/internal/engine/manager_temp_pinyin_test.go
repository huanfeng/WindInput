// manager_temp_pinyin_test.go — ActivateTempPinyin / DeactivateTempPinyin
// 对 CompositeDict 词库层的副作用回归测试.
//
// 这一组测试的目的: 验证一轮 Activate -> Deactivate 后, CompositeDict 的
// 层名集合应当与初始状态一致. 任何"做过临时拼音后丢层"的行为都应当被
// 这里捕获.
package engine

import (
	"log/slog"
	"sort"
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/schema"
)

// stubLayer 是 dict.DictLayer 的最小化测试实现 (engine 包内部).
// 与 coordinator 包里同名的 stub 重复, 暂不抽公共; 后续若需要可下沉到
// internal/dict/testutil.
type stubLayer struct {
	name    string
	layerTy dict.LayerType
}

func (s *stubLayer) Name() string                                       { return s.name }
func (s *stubLayer) Type() dict.LayerType                               { return s.layerTy }
func (s *stubLayer) Search(_ string, _ int) []candidate.Candidate       { return nil }
func (s *stubLayer) SearchPrefix(_ string, _ int) []candidate.Candidate { return nil }

// layerNames 返回 CompositeDict 当前所有层的名称, 已排序便于稳定比较.
func layerNames(cd *dict.CompositeDict) []string {
	layers := cd.GetLayers()
	names := make([]string, 0, len(layers))
	for _, l := range layers {
		names = append(names, l.Name())
	}
	sort.Strings(names)
	return names
}

// 码表方案下完整一轮 Activate -> Deactivate, 应当恢复到初始的层名集合.
// 这是已知能正确工作的对照组.
func TestTempPinyin_Codetable_RoundTripPreservesLayers(t *testing.T) {
	const (
		codetableID = "wubi"
		pinyinID    = "pinyin"
	)
	logger := slog.New(slog.DiscardHandler)
	dm := dict.NewDictManager("", "", logger)

	codetableLayer := &stubLayer{name: "codetable-system", layerTy: dict.LayerTypeSystem}
	pinyinLayer := &stubLayer{name: "pinyin-system", layerTy: dict.LayerTypeSystem}

	// 码表方案初始状态: 只有 codetable-system 在 CompositeDict 里.
	dm.RegisterSystemLayer(codetableLayer.Name(), codetableLayer)

	m := NewManager(logger)
	m.SetDictManager(dm)
	m.SetCurrentIDForTest(codetableID)
	m.SetPrimaryPinyinIDForTest(pinyinID)
	// systemLayers 映射: codetable schema -> codetableLayer, pinyin schema -> pinyinLayer.
	// ActivateTempPinyin 需要后者来注册拼音层; DeactivateTempPinyin 需要前者来恢复码表层.
	m.RegisterSystemLayerForTest(codetableID, codetableLayer)
	m.RegisterSystemLayerForTest(pinyinID, pinyinLayer)

	before := layerNames(dm.GetCompositeDict())

	m.ActivateTempPinyin()
	duringNames := layerNames(dm.GetCompositeDict())
	if got, want := duringNames, []string{"pinyin-system"}; !equalStrings(got, want) {
		t.Errorf("after Activate: layers = %v, want %v", got, want)
	}

	m.DeactivateTempPinyin()
	after := layerNames(dm.GetCompositeDict())
	if !equalStrings(after, before) {
		t.Errorf("after roundtrip: layers = %v, want %v (same as before)", after, before)
	}
}

// 混输方案下 Activate -> Deactivate. 混输的 CompositeDict 同时含
// codetable-system + pinyin-system 两个层. 期望一轮后两个层都还在.
// 当前 ActivateTempPinyin 实现没有针对 Mixed 短路, 本测试若失败即为
// TODO 2 提到的"Mixed 引擎下脏状态"问题被捕获.
func TestTempPinyin_Mixed_RoundTripPreservesLayers(t *testing.T) {
	const (
		mixedID  = "mixed"
		pinyinID = "pinyin"
	)
	logger := slog.New(slog.DiscardHandler)
	dm := dict.NewDictManager("", "", logger)

	codetableLayer := &stubLayer{name: "codetable-system", layerTy: dict.LayerTypeSystem}
	pinyinLayer := &stubLayer{name: "pinyin-system", layerTy: dict.LayerTypeSystem}

	// 混输初始: 两个层都在 CompositeDict 里.
	dm.RegisterSystemLayer(codetableLayer.Name(), codetableLayer)
	dm.RegisterSystemLayer(pinyinLayer.Name(), pinyinLayer)

	// 注入一个 Mixed schema, 让 isMixedEngineLocked 通过 schema 路径识别出
	// 当前是混输引擎 (没有真实 *mixed.Engine 实例时的兜底).
	sm := schema.NewSchemaManager("", "", logger)
	sm.InjectSchemaForTest(mixedID, &schema.Schema{
		Schema: schema.SchemaInfo{ID: mixedID, Name: "Mixed"},
		Engine: schema.EngineSpec{Type: schema.EngineTypeMixed},
	})
	sm.SetActiveForTest(mixedID)

	m := NewManager(logger)
	m.SetDictManager(dm)
	m.SetSchemaManager(sm)
	m.SetCurrentIDForTest(mixedID)
	m.SetPrimaryPinyinIDForTest(pinyinID)
	// 混输方案 systemLayers[mixedID] 的"代表层"通常是 codetable 层
	// (reRegisterSystemLayer 中 last-wins, 但实际两条都通过 RegisterSystemLayer
	// 注册到了 CompositeDict). DeactivateTempPinyin 用 systemLayers[currentID]
	// 来恢复码表层.
	m.RegisterSystemLayerForTest(mixedID, codetableLayer)
	m.RegisterSystemLayerForTest(pinyinID, pinyinLayer)

	before := layerNames(dm.GetCompositeDict())
	wantBefore := []string{"codetable-system", "pinyin-system"}
	if !equalStrings(before, wantBefore) {
		t.Fatalf("setup error: initial layers = %v, want %v", before, wantBefore)
	}

	m.ActivateTempPinyin()
	m.DeactivateTempPinyin()

	after := layerNames(dm.GetCompositeDict())
	if !equalStrings(after, before) {
		t.Errorf("mixed roundtrip lost layers: layers = %v, want %v", after, before)
	}
}

func equalStrings(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
