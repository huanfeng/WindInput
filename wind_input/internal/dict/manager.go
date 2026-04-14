package dict

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
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

	// ── 文件后端（useStore=false 时使用）──
	shadowLayers map[string]*ShadowLayer // schemaID -> ShadowLayer
	userDicts    map[string]*UserDict    // schemaID -> UserDict
	tempDicts    map[string]*TempDict    // schemaID -> TempDict

	// 当前活跃方案（文件后端）
	activeSchemaID string
	activeShadow   *ShadowLayer
	activeUserDict *UserDict
	activeTempDict *TempDict

	// ── Store 后端（useStore=true 时使用）──
	store             *store.Store
	useStore          bool
	storeUserLayers   map[string]*StoreUserLayer   // schemaID -> StoreUserLayer
	storeTempLayers   map[string]*StoreTempLayer   // schemaID -> StoreTempLayer
	storeShadowLayers map[string]*StoreShadowLayer // schemaID -> StoreShadowLayer
	freqScorers       map[string]*StoreFreqScorer  // schemaID -> StoreFreqScorer

	// 当前活跃方案（Store 后端）
	activeStoreUser   *StoreUserLayer
	activeStoreTemp   *StoreTempLayer
	activeStoreShadow *StoreShadowLayer

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
		shadowLayers:      make(map[string]*ShadowLayer),
		userDicts:         make(map[string]*UserDict),
		tempDicts:         make(map[string]*TempDict),
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
	dm.useStore = true
	dm.logger.Info("Store 后端已启用", "path", dbPath)
	return nil
}

// UseStore 返回是否使用 Store 后端
func (dm *DictManager) UseStore() bool {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.useStore
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
	userPhrasePath := filepath.Join(dm.dataDir, "user.phrases.yaml")
	dm.phraseLayer = NewPhraseLayerEx("phrases", systemPhrasePath, systemPhraseUserPath, userPhrasePath)
	if err := dm.phraseLayer.Load(); err != nil {
		dm.logger.Warn("加载短语配置失败", "error", err)
	} else {
		dm.logger.Info("短语层加载成功", "phrases", dm.phraseLayer.GetPhraseCount(), "commands", dm.phraseLayer.GetCommandCount())
	}
	dm.compositeDict.AddLayer(dm.phraseLayer)

	return nil
}

// SwitchSchema 切换活跃方案的用户数据层
// schemaID: 方案标识
// shadowFile: Shadow 文件名（相对于 dataDir）
// userDictFile: 用户词库文件名（相对于 dataDir）
// 可选参数通过 SwitchSchemaFull 提供 tempDictFile
func (dm *DictManager) SwitchSchema(schemaID, shadowFile, userDictFile string) {
	dm.SwitchSchemaFull(schemaID, shadowFile, userDictFile, "", 5000, 5)
}

// SwitchSchemaFull 切换活跃方案（包含临时词库）
func (dm *DictManager) SwitchSchemaFull(schemaID, shadowFile, userDictFile, tempDictFile string, tempMaxEntries, tempPromoteCount int) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if schemaID == dm.activeSchemaID {
		return
	}

	if dm.useStore {
		dm.switchSchemaStore(schemaID, tempMaxEntries, tempPromoteCount)
	} else {
		dm.switchSchemaFile(schemaID, shadowFile, userDictFile, tempDictFile, tempMaxEntries, tempPromoteCount)
	}

	dm.activeSchemaID = schemaID
	dm.logger.Info("切换到方案", "schemaID", schemaID)
}

// switchSchemaStore Store 后端的方案切换
func (dm *DictManager) switchSchemaStore(schemaID string, tempMaxEntries, tempPromoteCount int) {
	// 1. 移除旧的 Store 用户词库层
	if dm.activeStoreUser != nil {
		dm.compositeDict.RemoveLayer(dm.activeStoreUser.Name())
	}

	// 2. 懒加载 StoreShadowLayer
	shadowLayer, ok := dm.storeShadowLayers[schemaID]
	if !ok {
		shadowLayer = NewStoreShadowLayer(dm.store, schemaID)
		dm.storeShadowLayers[schemaID] = shadowLayer
		dm.logger.Info("Store Shadow 层已创建", "schemaID", schemaID)
	}
	dm.compositeDict.SetShadowProvider(shadowLayer)
	dm.activeStoreShadow = shadowLayer

	// 3. 懒加载 StoreUserLayer
	userLayer, ok := dm.storeUserLayers[schemaID]
	if !ok {
		userLayer = NewStoreUserLayer(dm.store, schemaID)
		dm.storeUserLayers[schemaID] = userLayer
		dm.logger.Info("Store 用户词库层已创建", "schemaID", schemaID, "entries", userLayer.EntryCount())
	}
	dm.compositeDict.AddLayer(userLayer)
	dm.activeStoreUser = userLayer

	// 4. 设置词频评分器
	scorer, ok := dm.freqScorers[schemaID]
	if !ok {
		scorer = NewStoreFreqScorer(dm.store, schemaID)
		dm.freqScorers[schemaID] = scorer
	}
	dm.compositeDict.SetFreqScorer(scorer)

	// 5. 懒加载 StoreTempLayer
	if dm.activeStoreTemp != nil {
		dm.compositeDict.RemoveLayer(dm.activeStoreTemp.Name())
	}
	tempLayer, ok := dm.storeTempLayers[schemaID]
	if !ok {
		tempLayer = NewStoreTempLayer(dm.store, schemaID)
		tempLayer.SetLimits(tempMaxEntries, tempPromoteCount)
		dm.storeTempLayers[schemaID] = tempLayer
		dm.logger.Info("Store 临时词库层已创建", "schemaID", schemaID)
	}
	dm.compositeDict.AddLayer(tempLayer)
	dm.activeStoreTemp = tempLayer
}

// switchSchemaFile 文件后端的方案切换（原有逻辑）
func (dm *DictManager) switchSchemaFile(schemaID, shadowFile, userDictFile, tempDictFile string, tempMaxEntries, tempPromoteCount int) {
	// 1. 从 CompositeDict 移除旧的 UserDict 层
	if dm.activeUserDict != nil {
		dm.compositeDict.RemoveLayer(dm.activeUserDict.Name())
	}

	// 2. 懒加载目标方案的 ShadowLayer
	shadow, ok := dm.shadowLayers[schemaID]
	if !ok {
		shadowPath := dm.resolveDataPath(shadowFile, schemaID+".shadow.yaml")
		shadow = NewShadowLayer("shadow_"+schemaID, shadowPath)
		if err := shadow.Load(); err != nil {
			dm.logger.Warn("加载 Shadow 规则失败", "schemaID", schemaID, "error", err)
		} else {
			dm.logger.Info("Shadow 层加载成功", "schemaID", schemaID, "rules", shadow.GetRuleCount())
		}
		dm.shadowLayers[schemaID] = shadow
	}
	dm.compositeDict.SetShadowProvider(shadow)
	dm.activeShadow = shadow

	// 3. 懒加载目标方案的 UserDict
	userDict, ok := dm.userDicts[schemaID]
	if !ok {
		userDictPath := dm.resolveDataPath(userDictFile, schemaID+".userwords.txt")
		userDict = NewUserDict("user_"+schemaID, userDictPath)
		if err := userDict.Load(); err != nil {
			dm.logger.Warn("加载用户词库失败", "schemaID", schemaID, "error", err)
		} else {
			dm.logger.Info("用户词库加载成功", "schemaID", schemaID, "entries", userDict.EntryCount())
		}
		dm.userDicts[schemaID] = userDict
	}
	dm.compositeDict.AddLayer(userDict)
	dm.activeUserDict = userDict

	// 4. 懒加载目标方案的 TempDict
	if dm.activeTempDict != nil {
		dm.compositeDict.RemoveLayer(dm.activeTempDict.Name())
	}
	if tempDictFile != "" {
		tempDict, ok := dm.tempDicts[schemaID]
		if !ok {
			tempDictPath := filepath.Join(dm.dataDir, tempDictFile)
			tempDict = NewTempDict("temp_"+schemaID, tempDictPath, tempMaxEntries, tempPromoteCount, dm.logger)
			if err := tempDict.Load(); err != nil {
				dm.logger.Warn("加载临时词库失败", "schemaID", schemaID, "error", err)
			} else {
				dm.logger.Info("临时词库加载成功", "schemaID", schemaID, "entries", tempDict.GetWordCount())
			}
			dm.tempDicts[schemaID] = tempDict
		}
		tempDict.SetTargetDict(userDict)
		dm.compositeDict.AddLayer(tempDict)
		dm.activeTempDict = tempDict
	} else {
		dm.activeTempDict = nil
	}
}

// resolveDataPath 解析用户数据文件路径，防止文件名为空或路径指向目录
func (dm *DictManager) resolveDataPath(fileName, fallback string) string {
	if fileName == "" {
		dm.logger.Warn("用户数据文件名为空，使用默认值", "fallback", fallback)
		fileName = fallback
	}
	p := filepath.Join(dm.dataDir, fileName)
	if info, err := os.Stat(p); err == nil && info.IsDir() {
		dm.logger.Warn("用户数据路径是目录而非文件，使用默认值", "path", p, "fallback", fallback)
		p = filepath.Join(dm.dataDir, fallback)
	}
	return p
}

// RegisterSystemLayer 注册系统词库层
func (dm *DictManager) RegisterSystemLayer(name string, layer DictLayer) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	dm.systemLayers[name] = layer
	dm.compositeDict.AddLayer(layer)
	dm.logger.Info("注册系统词库", "name", name)
}

// UnregisterSystemLayer 取消注册系统词库层
func (dm *DictManager) UnregisterSystemLayer(name string) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if _, ok := dm.systemLayers[name]; ok {
		delete(dm.systemLayers, name)
		dm.compositeDict.RemoveLayer(name)
		dm.logger.Info("取消注册系统词库", "name", name)
	}
}

// GetCompositeDict 获取聚合词库
func (dm *DictManager) GetCompositeDict() *CompositeDict {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.compositeDict
}

// SetSortMode 设置候选排序模式
func (dm *DictManager) SetSortMode(mode candidate.CandidateSortMode) {
	dm.mu.Lock()
	defer dm.mu.Unlock()
	dm.compositeDict.SetSortMode(mode)
}

// GetUserDict 获取当前活跃的用户词库
func (dm *DictManager) GetUserDict() *UserDict {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeUserDict
}

// GetShadowLayer 获取当前活跃的 Shadow 层
func (dm *DictManager) GetShadowLayer() *ShadowLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeShadow
}

// GetTempDict 获取当前活跃的临时词库
func (dm *DictManager) GetTempDict() *TempDict {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeTempDict
}

// GetPhraseLayer 获取短语层
func (dm *DictManager) GetPhraseLayer() *PhraseLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.phraseLayer
}

// GetActiveSchemaID 获取当前活跃方案 ID
func (dm *DictManager) GetActiveSchemaID() string {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeSchemaID
}

// AddUserWord 添加用户词
func (dm *DictManager) AddUserWord(code, text string, weight int) error {
	if dm.useStore {
		if dm.activeStoreUser == nil {
			return fmt.Errorf("Store 用户词库层未初始化")
		}
		return dm.activeStoreUser.Add(code, text, weight)
	}
	if dm.activeUserDict == nil {
		return fmt.Errorf("用户词库未初始化")
	}
	return dm.activeUserDict.Add(code, text, weight)
}

// PinWord 固定词到指定位置（置顶 = position 0）
func (dm *DictManager) PinWord(code, word string, position int) {
	if dm.useStore {
		if dm.activeStoreShadow != nil {
			dm.activeStoreShadow.Pin(code, word, position)
		}
		return
	}
	if dm.activeShadow != nil {
		dm.activeShadow.Pin(code, word, position)
	}
}

// DeleteWord 删除（隐藏）词条
func (dm *DictManager) DeleteWord(code, word string) {
	if dm.useStore {
		if dm.activeStoreShadow != nil {
			dm.activeStoreShadow.Delete(code, word)
		}
		return
	}
	if dm.activeShadow != nil {
		dm.activeShadow.Delete(code, word)
	}
}

// RemoveShadowRule 移除词的所有 Shadow 规则
func (dm *DictManager) RemoveShadowRule(code, word string) {
	if dm.useStore {
		if dm.activeStoreShadow != nil {
			dm.activeStoreShadow.RemoveRule(code, word)
		}
		return
	}
	if dm.activeShadow != nil {
		dm.activeShadow.RemoveRule(code, word)
	}
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

// Save 保存所有可写层
func (dm *DictManager) Save() error {
	dm.mu.RLock()
	defer dm.mu.RUnlock()

	// Store 后端无需手动保存（bbolt 自动持久化）
	if dm.useStore {
		return nil
	}

	var errs []error

	for id, ud := range dm.userDicts {
		if err := ud.Save(); err != nil {
			errs = append(errs, fmt.Errorf("保存用户词库失败 (%s): %w", id, err))
		}
	}

	for id, td := range dm.tempDicts {
		if err := td.Save(); err != nil {
			errs = append(errs, fmt.Errorf("保存临时词库失败 (%s): %w", id, err))
		}
	}

	for id, sl := range dm.shadowLayers {
		if sl.IsDirty() {
			if err := sl.Save(); err != nil {
				errs = append(errs, fmt.Errorf("保存 Shadow 规则失败 (%s): %w", id, err))
			}
		}
	}

	if len(errs) > 0 {
		return fmt.Errorf("保存词库时发生错误: %v", errs)
	}

	return nil
}

// Close 关闭词库管理器
func (dm *DictManager) Close() error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	// 关闭文件后端
	for _, ud := range dm.userDicts {
		ud.Close()
	}
	for _, td := range dm.tempDicts {
		td.Close()
	}
	for _, sl := range dm.shadowLayers {
		if sl.IsDirty() {
			sl.Save()
		}
	}

	// 关闭 Store 后端
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

	if dm.phraseLayer != nil {
		return dm.phraseLayer.Load()
	}
	return nil
}

// ReloadShadow 重新加载当前方案的 Shadow 规则
func (dm *DictManager) ReloadShadow() error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if dm.activeShadow != nil {
		return dm.activeShadow.Load()
	}
	return nil
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

	if dm.useStore {
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
	} else {
		if dm.activeShadow != nil {
			stats["shadow_rules"] = dm.activeShadow.GetRuleCount()
		}
		if dm.activeUserDict != nil {
			stats["user_words"] = dm.activeUserDict.EntryCount()
		}
		if dm.activeTempDict != nil {
			stats["temp_words"] = dm.activeTempDict.GetWordCount()
		}
		stats["schema_count"] = len(dm.shadowLayers)
	}

	stats["total_layers"] = len(dm.compositeDict.GetLayers())

	return stats
}
