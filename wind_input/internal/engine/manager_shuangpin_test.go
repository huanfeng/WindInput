// manager_shuangpin_test.go — UpdateShuangpinLayout 隔离性回归测试。
//
// 历史 BUG: UpdateShuangpinLayout 旧实现遍历 m.engines 给所有 *pinyin.Engine
// 通杀 SetShuangpinConverter，导致：
//   - 任意全拼方案 reload 把缓存中双拼方案的 spConverter 清空；
//   - 任意双拼方案 reload 把全拼方案的 engine 错套上 converter。
//
// 现象在用户侧表现为"双拼下出现全拼分词"或"全拼下 zhishi 被截成 zh is"。
//
// 这里的断言聚焦于 isolation：UpdateShuangpinLayout(schemaID, ...) 只能
// 影响 schemaID 对应的 engine，其余 engine 状态必须保持不变。
package engine

import (
	"log/slog"
	"testing"

	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/pinyin/shuangpin"
)

func TestUpdateShuangpinLayout_OnlyAffectsTargetSchema(t *testing.T) {
	logger := slog.New(slog.DiscardHandler)
	m := NewManager(logger)

	pinyinEng := pinyin.NewEngine(nil, logger)
	shuangpinEng := pinyin.NewEngine(nil, logger)
	xiaohe := shuangpin.Get("xiaohe")
	if xiaohe == nil {
		t.Fatal("xiaohe scheme not registered")
	}
	shuangpinEng.SetShuangpinConverter(shuangpin.NewConverter(xiaohe))

	m.engines["pinyin"] = pinyinEng
	m.engines["shuangpin"] = shuangpinEng

	// 全拼方案 reload (layoutID="")：旧 BUG 会清空 shuangpinEng 的 spConverter。
	// 修复后只影响 m.engines["pinyin"]，不应触及 m.engines["shuangpin"]。
	m.UpdateShuangpinLayout("pinyin", "")

	if pinyinEng.GetShuangpinConverter() != nil {
		t.Errorf("pinyin engine: spConverter 应为 nil")
	}
	if shuangpinEng.GetShuangpinConverter() == nil {
		t.Errorf("shuangpin engine: spConverter 不应被全拼方案的 reload 清空（隔离性 BUG 回归）")
	}

	// 双拼方案 reload (layoutID="ziranma")：旧 BUG 会给 pinyinEng 错误套上 converter。
	// 修复后只影响 m.engines["shuangpin"]。
	m.UpdateShuangpinLayout("shuangpin", "ziranma")

	if pinyinEng.GetShuangpinConverter() != nil {
		t.Errorf("pinyin engine: 不应被双拼方案的 reload 错套 converter（隔离性 BUG 回归）")
	}
	gotConv := shuangpinEng.GetShuangpinConverter()
	if gotConv == nil {
		t.Fatalf("shuangpin engine: spConverter 应为 ziranma")
	}
	if gotConv.GetScheme().ID != "ziranma" {
		t.Errorf("shuangpin engine: layout 应切到 ziranma, got %q", gotConv.GetScheme().ID)
	}
}

func TestUpdateShuangpinLayout_UnknownSchemaIDIsNoop(t *testing.T) {
	logger := slog.New(slog.DiscardHandler)
	m := NewManager(logger)

	pinyinEng := pinyin.NewEngine(nil, logger)
	m.engines["pinyin"] = pinyinEng

	// 调用一个不存在的 schemaID：必须不 panic、不影响其它 engine。
	m.UpdateShuangpinLayout("nonexistent", "xiaohe")

	if pinyinEng.GetShuangpinConverter() != nil {
		t.Errorf("pinyin engine 不应被未知 schemaID 的调用影响")
	}
}
