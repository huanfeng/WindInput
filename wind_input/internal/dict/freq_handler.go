package dict

import (
	"github.com/huanfeng/wind_input/internal/store"
)

// FreqHandler 词频记录处理器
// 独立于造词逻辑，负责记录用户选词频次到 Store
// 查询时通过 StoreFreqScorer 读取并计算加成
type FreqHandler struct {
	store    *store.Store
	schemaID string
}

// NewFreqHandler 创建词频记录处理器
func NewFreqHandler(s *store.Store, schemaID string) *FreqHandler {
	if s == nil {
		return nil
	}
	return &FreqHandler{
		store:    s,
		schemaID: schemaID,
	}
}

// Record 记录一次选词（写入 Store 的词频 bucket）
func (h *FreqHandler) Record(code, text string) {
	if h == nil || h.store == nil {
		return
	}
	h.store.IncrementFreq(h.schemaID, code, text)
}

// GetSchemaID 获取方案 ID
func (h *FreqHandler) GetSchemaID() string {
	if h == nil {
		return ""
	}
	return h.schemaID
}
