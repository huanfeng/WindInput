package dict

import (
	"fmt"
	"log/slog"
	"path/filepath"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/store"
)

// DictManager 词库管理器
// 统一管理所有词库层的加载、保存和生命周期
type DictManager struct {
	mu sync.RWMutex

	logger *slog.Logger

	// 用户数据目录（%APPDATA%\WindInput）
	dataDir string
	// 程序数据目录（exe 所在目录/data，存放 system.phrases.yaml 等）
	systemDir string

	// 全局层
	phraseLayer *PhraseLayer // Lv1: 特殊短语（全局共享）

	// 当前活跃方案
	activeSchemaID string

	// ── Store 后端 ──
	store             *store.Store
	storeUserLayers   map[string]*StoreUserLayer   // schemaID -> StoreUserLayer
	storeTempLayers   map[string]*StoreTempLayer   // schemaID -> StoreTempLayer
	storeShadowLayers map[string]*StoreShadowLayer // schemaID -> StoreShadowLayer
	freqScorers       map[string]*StoreFreqScorer  // schemaID -> StoreFreqScorer
	freqProfile       *store.FreqProfile           // 当前方案的词频评分参数

	// 当前活跃方案（Store 后端）
	activeDataSchemaID string // 数据方案 ID（混输方案映射到主方案）
	activeStoreUser    *StoreUserLayer
	activeStoreTemp    *StoreTempLayer
	activeStoreShadow  *StoreShadowLayer

	// 聚合词库
	compositeDict *CompositeDict

	// 系统词库适配器（由引擎加载后注册）
	systemLayers map[string]DictLayer
}

// NewDictManager 创建词库管理器
// dataDir: 用户数据目录（%APPDATA%\WindInput）
// systemDir: 程序数据目录（exeDir/data，存放 system.phrases.yaml 等）
func NewDictManager(dataDir, systemDir string, logger *slog.Logger) *DictManager {
	if logger == nil {
		logger = slog.Default()
	}
	return &DictManager{
		logger:            logger,
		dataDir:           dataDir,
		systemDir:         systemDir,
		storeUserLayers:   make(map[string]*StoreUserLayer),
		storeTempLayers:   make(map[string]*StoreTempLayer),
		storeShadowLayers: make(map[string]*StoreShadowLayer),
		freqScorers:       make(map[string]*StoreFreqScorer),
		systemLayers:      make(map[string]DictLayer),
		compositeDict:     NewCompositeDict(),
	}
}

// OpenStore 打开 bbolt 数据库并启用 Store 后端
// 应在 Initialize() 之前调用
func (dm *DictManager) OpenStore(dbPath string) error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	s, err := store.Open(dbPath)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}

	dm.store = s
	dm.logger.Info("Store 后端已启用", "path", dbPath)
	return nil
}

// GetStore 获取底层 Store（可用于词频记录等）
func (dm *DictManager) GetStore() *store.Store {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.store
}

// Initialize 初始化全局层（短语层）
func (dm *DictManager) Initialize() error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	// 初始化短语层 (Lv1) — 全局共享
	// 系统短语：优先加载用户目录的同名文件（用户修改后的副本），不存在则加载程序目录的原始文件
	systemPhrasePath := filepath.Join(dm.systemDir, "system.phrases.yaml")
	systemPhraseUserPath := filepath.Join(dm.dataDir, "system.phrases.yaml")
	dm.phraseLayer = NewPhraseLayerEx("phrases", systemPhrasePath, systemPhraseUserPath, dm.store)

	if err := dm.SeedDefaultPhrases(); err != nil {
		dm.logger.Error("种子默认短语失败", "error", err)
	}
	if err := dm.phraseLayer.LoadFromStore(dm.store); err != nil {
		dm.logger.Warn("从 Store 加载短语失败", "error", err)
	} else {
		dm.logger.Info("短语层从 Store 加载成功", "phrases", dm.phraseLayer.GetPhraseCount(), "commands", dm.phraseLayer.GetCommandCount())
	}

	dm.compositeDict.AddLayer(dm.phraseLayer)

	return nil
}

// SeedDefaultPhrases populates the Phrases bucket with system defaults
// if it is currently empty. This is called once on first startup.
func (dm *DictManager) SeedDefaultPhrases() error {
	if dm.store == nil {
		return nil
	}

	count, err := dm.store.PhraseCount()
	if err != nil {
		return err
	}
	if count > 0 {
		return nil // Already seeded
	}

	var records []store.PhraseRecord

	// Load system phrases: prefer user-dir copy, fall back to system-dir original
	systemFile := filepath.Join(dm.systemDir, "system.phrases.yaml")
	systemUserFile := filepath.Join(dm.dataDir, "system.phrases.yaml")

	systemLoaded := false
	if entries, err := ParsePhraseYAMLFile(systemUserFile); err == nil {
		for _, e := range entries {
			if e.Code == "" || (e.Text == "" && e.Texts == "") {
				continue
			}
			rec := store.PhraseRecord{
				Code:     strings.ToLower(e.Code),
				Text:     e.Text,
				Texts:    e.Texts,
				Name:     e.Name,
				Type:     detectPhraseType(e),
				Position: e.Position,
				Enabled:  !e.Disabled,
				IsSystem: true,
			}
			if rec.Position <= 0 {
				rec.Position = 1
			}
			records = append(records, rec)
		}
		systemLoaded = true
	}
	if !systemLoaded {
		if entries, err := ParsePhraseYAMLFile(systemFile); err == nil {
			for _, e := range entries {
				if e.Code == "" || (e.Text == "" && e.Texts == "") {
					continue
				}
				rec := store.PhraseRecord{
					Code:     strings.ToLower(e.Code),
					Text:     e.Text,
					Texts:    e.Texts,
					Name:     e.Name,
					Type:     detectPhraseType(e),
					Position: e.Position,
					Enabled:  !e.Disabled,
					IsSystem: true,
				}
				if rec.Position <= 0 {
					rec.Position = 1
				}
				records = append(records, rec)
			}
		}
	}

	if len(records) > 0 {
		dm.logger.Info("种子默认短语", "count", len(records))
		return dm.store.SeedPhrases(records)
	}
	return nil
}

// SwitchSchemaFull 切换活跃方案（包含临时词库）
func (dm *DictManager) SwitchSchemaFull(schemaID, dataSchemaID string, tempMaxEntries, tempPromoteCount int, opts ...string) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if schemaID == dm.activeSchemaID {
		return
	}

	// opts[0] = freqSchemaID（可选，默认与 dataSchemaID 相同）
	freqSchemaID := dataSchemaID
	if len(opts) > 0 && opts[0] != "" {
		freqSchemaID = opts[0]
	}

	dm.switchSchemaStore(schemaID, dataSchemaID, freqSchemaID, tempMaxEntries, tempPromoteCount)

	dm.activeSchemaID = schemaID
	dm.logger.Info("切换到方案", "schemaID", schemaID)
}

// switchSchemaStore Store 后端的方案切换
// schemaID: 活跃方案 ID（如 wubi86_pinyin）
// dataSchemaID: 数据方案 ID（如 wubi86，用于用户词库/临时词库/Shadow 的 bucket key）
// freqSchemaID: 词频数据方案 ID（如 wubi86_pinyin，用于词频 bucket key；混输方案独立于主方案）
func (dm *DictManager) switchSchemaStore(schemaID, dataSchemaID, freqSchemaID string, tempMaxEntries, tempPromoteCount int) {
	dm.logger.Info("Store 方案切换", "schemaID", schemaID, "dataSchemaID", dataSchemaID, "freqSchemaID", freqSchemaID)
	dm.activeDataSchemaID = dataSchemaID

	// 1. 移除旧的 Store 用户词库层
	if dm.activeStoreUser != nil {
		dm.compositeDict.RemoveLayer(dm.activeStoreUser.Name())
	}

	// 2. 懒加载 StoreShadowLayer（使用 freqSchemaID，混输方案独立于主方案）
	shadowLayer, ok := dm.storeShadowLayers[freqSchemaID]
	if !ok {
		shadowLayer = NewStoreShadowLayer(dm.store, freqSchemaID)
		dm.storeShadowLayers[freqSchemaID] = shadowLayer
		dm.logger.Info("Store Shadow 层已创建", "schemaID", freqSchemaID)
	}
	dm.compositeDict.SetShadowProvider(shadowLayer)
	dm.activeStoreShadow = shadowLayer

	// 3. 懒加载 StoreUserLayer（使用 dataSchemaID 作为 bucket key）
	userLayer, ok := dm.storeUserLayers[dataSchemaID]
	if !ok {
		userLayer = NewStoreUserLayer(dm.store, dataSchemaID)
		dm.storeUserLayers[dataSchemaID] = userLayer
		dm.logger.Info("Store 用户词库层已创建", "dataSchemaID", dataSchemaID, "entries", userLayer.EntryCount())
	}
	dm.compositeDict.AddLayer(userLayer)
	dm.activeStoreUser = userLayer

	// 4. 设置词频评分器（使用 freqSchemaID，混输方案独立于主方案）
	scorer, ok := dm.freqScorers[freqSchemaID]
	if !ok {
		scorer = NewStoreFreqScorer(dm.store, freqSchemaID, dm.freqProfile)
		dm.freqScorers[freqSchemaID] = scorer
	}
	dm.compositeDict.SetFreqScorer(scorer)

	// 5. 懒加载 StoreTempLayer
	if dm.activeStoreTemp != nil {
		dm.compositeDict.RemoveLayer(dm.activeStoreTemp.Name())
	}
	tempLayer, ok := dm.storeTempLayers[dataSchemaID]
	if !ok {
		tempLayer = NewStoreTempLayer(dm.store, dataSchemaID)
		dm.storeTempLayers[dataSchemaID] = tempLayer
		dm.logger.Info("Store 临时词库层已创建", "dataSchemaID", dataSchemaID)
	}
	// 总是应用最新的 limits（GetOrCreate 可能提前创建过 layer 但未设过 limits；
	// 配置热更新后 promoteCount/maxEntries 也需要重新生效）
	tempLayer.SetLimits(tempMaxEntries, tempPromoteCount)
	dm.compositeDict.AddLayer(tempLayer)
	dm.activeStoreTemp = tempLayer
}

// RegisterSystemLayer 注册系统词库层
// 同名旧层会先从 compositeDict 中移除，避免多次注册造成层的累积
// （多个混输/拼音方案预加载时，每个 factory 都会注册同名 codetable-system / pinyin-system）。
func (dm *DictManager) RegisterSystemLayer(name string, layer DictLayer) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	// 先移除 compositeDict 中所有同名层，再添加新层（防止 append 累积）
	for dm.compositeDict.RemoveLayer(name) {
	}
	dm.systemLayers[name] = layer
	dm.compositeDict.AddLayer(layer)
	dm.logger.Debug("注册系统词库", "name", name)
}

// UnregisterSystemLayer 取消注册系统词库层（移除所有同名层，防御性清理）
func (dm *DictManager) UnregisterSystemLayer(name string) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	delete(dm.systemLayers, name)
	removed := false
	for dm.compositeDict.RemoveLayer(name) {
		removed = true
	}
	if removed {
		dm.logger.Debug("取消注册系统词库", "name", name)
	}
}

// GetCompositeDict 获取聚合词库
func (dm *DictManager) GetCompositeDict() *CompositeDict {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.compositeDict
}

// ExistsInSystemDict 检查 code+text 是否已存在于系统词库层。
// 先做精确匹配，若未找到再做前缀匹配，以覆盖前缀输入时词条来自更长编码的情况。
func (dm *DictManager) ExistsInSystemDict(code, text string) bool {
	dm.mu.RLock()
	defer dm.mu.RUnlock()

	for _, layer := range dm.compositeDict.GetLayersByType(LayerTypeSystem) {
		for _, c := range layer.Search(code, 0) {
			if c.Text == text {
				return true
			}
		}
		for _, c := range layer.SearchPrefix(code, 0) {
			if c.Text == text {
				return true
			}
		}
	}
	return false
}

// ClearFreqScorer 清除 CompositeDict 上的词频评分器（调频关闭时调用）
func (dm *DictManager) ClearFreqScorer() {
	dm.mu.Lock()
	defer dm.mu.Unlock()
	dm.compositeDict.SetFreqScorer(nil)
}

// SetFreqProfile 设置词频评分参数（在方案加载时由 factory 调用）
func (dm *DictManager) SetFreqProfile(profile *store.FreqProfile) {
	dm.mu.Lock()
	defer dm.mu.Unlock()
	dm.freqProfile = profile
	// 清空已缓存的 scorer，下次 switchSchemaStore 时使用新 profile 重建
	dm.freqScorers = make(map[string]*StoreFreqScorer)
}

// SetSortMode 设置候选排序模式
func (dm *DictManager) SetSortMode(mode candidate.CandidateSortMode) {
	dm.mu.Lock()
	defer dm.mu.Unlock()
	dm.compositeDict.SetSortMode(mode)
}

// GetShadowProvider 获取当前活跃的 ShadowProvider
func (dm *DictManager) GetShadowProvider() ShadowProvider {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeStoreShadow
}

// GetPhraseLayer 获取短语层
func (dm *DictManager) GetPhraseLayer() *PhraseLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.phraseLayer
}

// GetActiveSchemaID 获取当前活跃方案 ID
// 返回数据方案 ID（如混输方案映射到主方案），确保数据操作使用正确的 bucket
func (dm *DictManager) GetActiveSchemaID() string {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	if dm.activeDataSchemaID != "" {
		return dm.activeDataSchemaID
	}
	return dm.activeSchemaID
}

// AddUserWord 添加用户词
func (dm *DictManager) AddUserWord(code, text string, weight int) error {
	if dm.activeStoreUser == nil {
		return fmt.Errorf("Store 用户词库层未初始化")
	}
	return dm.activeStoreUser.Add(code, text, weight)
}

// PinWord 固定词到指定位置（置顶 = position 0）
func (dm *DictManager) PinWord(code, word string, position int) {
	if dm.activeStoreShadow != nil {
		dm.activeStoreShadow.Pin(code, word, position)
	}
}

// DeleteWord 删除词条。
// 若词条存在于系统词库，则通过 Shadow 隐藏；
// 若词条仅存在于用户/临时词库，则直接删除源记录（不污染 Shadow）。
func (dm *DictManager) DeleteWord(code, word string) {
	// 无论是否存在于系统词库，都先清理用户/临时词库中的同名记录
	if dm.activeStoreUser != nil {
		_ = dm.activeStoreUser.Remove(code, word)
	}
	if dm.activeStoreTemp != nil {
		_ = dm.activeStoreTemp.Remove(code, word)
	}

	if dm.ExistsInSystemDict(code, word) {
		// 系统词库中存在：还需要通过 Shadow 隐藏
		if dm.activeStoreShadow != nil {
			dm.activeStoreShadow.Delete(code, word)
		}
	}
}

// RemoveShadowRule 移除词的所有 Shadow 规则
func (dm *DictManager) RemoveShadowRule(code, word string) {
	if dm.activeStoreShadow != nil {
		dm.activeStoreShadow.RemoveRule(code, word)
	}
}

// HasShadowRule 检查指定编码和词是否有 Shadow 规则
func (dm *DictManager) HasShadowRule(code, word string) bool {
	if dm.activeStoreShadow != nil {
		rules := dm.activeStoreShadow.GetShadowRules(code)
		if rules == nil {
			return false
		}
		for _, p := range rules.Pinned {
			if p.Word == word {
				return true
			}
		}
		for _, d := range rules.Deleted {
			if d == word {
				return true
			}
		}
	}
	return false
}

// SaveShadow 保存 Shadow 规则
func (dm *DictManager) SaveShadow() error {
	return nil // bbolt 自动持久化
}

// ── Store 后端专用访问器 ──

// GetStoreUserLayer 获取当前活跃的 Store 用户词库层（仅 Store 模式下有效）
func (dm *DictManager) GetStoreUserLayer() *StoreUserLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeStoreUser
}

// GetStoreTempLayer 获取当前活跃的 Store 临时词库层（仅 Store 模式下有效）
func (dm *DictManager) GetStoreTempLayer() *StoreTempLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeStoreTemp
}

// GetStoreShadowLayer 获取当前活跃的 Store Shadow 层（仅 Store 模式下有效）
func (dm *DictManager) GetStoreShadowLayer() *StoreShadowLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeStoreShadow
}

// GetOrCreateStoreUserLayer 获取或创建指定 schemaID 的用户词库层（按需 lazy-create）
// 用于混输模式下让拼音子引擎使用独立 bucket，避免污染主码表用户词库
func (dm *DictManager) GetOrCreateStoreUserLayer(schemaID string) *StoreUserLayer {
	if dm.store == nil || schemaID == "" {
		return nil
	}
	dm.mu.Lock()
	defer dm.mu.Unlock()
	userLayer, ok := dm.storeUserLayers[schemaID]
	if !ok {
		userLayer = NewStoreUserLayer(dm.store, schemaID)
		dm.storeUserLayers[schemaID] = userLayer
		dm.logger.Info("Store 用户词库层已创建（按需）", "dataSchemaID", schemaID, "entries", userLayer.EntryCount())
	}
	return userLayer
}

// UpdateActiveTempLimits 更新当前活跃临时词库层的 limits（用于配置热更新）
// 不持有 m.mu 等其它锁，可在外层锁的回调中安全调用。
func (dm *DictManager) UpdateActiveTempLimits(maxEntries, promoteCount int) {
	dm.mu.RLock()
	tempLayer := dm.activeStoreTemp
	dm.mu.RUnlock()
	if tempLayer != nil {
		tempLayer.SetLimits(maxEntries, promoteCount)
	}
}

// GetOrCreateStoreTempLayer 获取或创建指定 schemaID 的临时词库层
func (dm *DictManager) GetOrCreateStoreTempLayer(schemaID string) *StoreTempLayer {
	if dm.store == nil || schemaID == "" {
		return nil
	}
	dm.mu.Lock()
	defer dm.mu.Unlock()
	tempLayer, ok := dm.storeTempLayers[schemaID]
	if !ok {
		tempLayer = NewStoreTempLayer(dm.store, schemaID)
		dm.storeTempLayers[schemaID] = tempLayer
		dm.logger.Info("Store 临时词库层已创建（按需）", "dataSchemaID", schemaID)
	}
	return tempLayer
}

// ActivateEnglishStoreLayers 激活英文词库的 Store 层（用户词 + Shadow）
// 英文词库使用固定 schemaID "english"，跨方案共享
func (dm *DictManager) ActivateEnglishStoreLayers() {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if dm.store == nil {
		return
	}

	const englishSchemaID = "english"

	// 用户词库层
	userLayer, ok := dm.storeUserLayers[englishSchemaID]
	if !ok {
		userLayer = NewStoreUserLayer(dm.store, englishSchemaID)
		dm.storeUserLayers[englishSchemaID] = userLayer
		dm.logger.Info("英文 Store 用户词库层已创建")
	}
	if dm.compositeDict.GetLayerByName(userLayer.Name()) == nil {
		dm.compositeDict.AddLayer(userLayer)
	}

	// Shadow 层
	shadowLayer, ok := dm.storeShadowLayers[englishSchemaID]
	if !ok {
		shadowLayer = NewStoreShadowLayer(dm.store, englishSchemaID)
		dm.storeShadowLayers[englishSchemaID] = shadowLayer
		dm.logger.Info("英文 Store Shadow 层已创建")
	}
	_ = shadowLayer
}

// DeactivateEnglishStoreLayers 停用英文词库的 Store 层
func (dm *DictManager) DeactivateEnglishStoreLayers() {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	const englishSchemaID = "english"

	if userLayer, ok := dm.storeUserLayers[englishSchemaID]; ok {
		dm.compositeDict.RemoveLayer(userLayer.Name())
	}
}

// GetEnglishShadowLayer 获取英文 Shadow 层（供候选置顶/删除操作）
func (dm *DictManager) GetEnglishShadowLayer() *StoreShadowLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.storeShadowLayers["english"]
}

// GetEnglishUserLayer 获取英文用户词库层
func (dm *DictManager) GetEnglishUserLayer() *StoreUserLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.storeUserLayers["english"]
}

// Save 保存所有可写层
func (dm *DictManager) Save() error {
	// bbolt 自动持久化，无需手动保存
	return nil
}

// Close 关闭词库管理器
func (dm *DictManager) Close() error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if dm.store != nil {
		if err := dm.store.Close(); err != nil {
			dm.logger.Error("关闭 Store 失败", "error", err)
			return err
		}
		dm.store = nil
	}

	return nil
}

// Search 搜索候选词（便捷方法）
func (dm *DictManager) Search(code string, limit int) []candidate.Candidate {
	return dm.compositeDict.Search(code, limit)
}

// SearchPrefix 前缀搜索（便捷方法）
func (dm *DictManager) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	return dm.compositeDict.SearchPrefix(prefix, limit)
}

// ReloadPhrases 重新加载短语配置
func (dm *DictManager) ReloadPhrases() error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if dm.phraseLayer == nil {
		return nil
	}
	return dm.phraseLayer.LoadFromStore(dm.store)
}

// GetStats 获取统计信息
func (dm *DictManager) GetStats() map[string]int {
	dm.mu.RLock()
	defer dm.mu.RUnlock()

	stats := make(map[string]int)

	if dm.phraseLayer != nil {
		stats["phrases"] = dm.phraseLayer.GetPhraseCount()
		stats["commands"] = dm.phraseLayer.GetCommandCount()
	}

	if dm.activeStoreShadow != nil {
		stats["shadow_rules"] = dm.activeStoreShadow.GetRuleCount()
	}
	if dm.activeStoreUser != nil {
		stats["user_words"] = dm.activeStoreUser.EntryCount()
	}
	if dm.activeStoreTemp != nil {
		stats["temp_words"] = dm.activeStoreTemp.GetWordCount()
	}
	stats["schema_count"] = len(dm.storeShadowLayers)
	stats["store_enabled"] = 1

	stats["total_layers"] = len(dm.compositeDict.GetLayers())

	return stats
}
