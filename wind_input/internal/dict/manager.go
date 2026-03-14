package dict

import (
	"fmt"
	"log"
	"path/filepath"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// DictManager 词库管理器
// 统一管理所有词库层的加载、保存和生命周期
type DictManager struct {
	mu sync.RWMutex

	// 数据目录（%APPDATA%\WindInput）
	dataDir string

	// 各层词库
	phraseLayer    *PhraseLayer // Lv1: 特殊短语
	shadowLayer    *ShadowLayer // Lv2: 用户修正
	pinyinUserDict *UserDict    // Lv3: 拼音用户造词
	wubiUserDict   *UserDict    // Lv3: 五笔用户造词
	activeUserDict *UserDict    // 指向当前活跃的用户词库
	activeEngine   string       // 当前活跃引擎类型: "pinyin" or "wubi"

	// 聚合词库
	compositeDict *CompositeDict

	// 系统词库适配器（由引擎加载后注册）
	systemLayers map[string]DictLayer
}

// NewDictManager 创建词库管理器
func NewDictManager(dataDir string) *DictManager {
	dm := &DictManager{
		dataDir:       dataDir,
		systemLayers:  make(map[string]DictLayer),
		compositeDict: NewCompositeDict(),
	}

	return dm
}

// Initialize 初始化所有词库层
func (dm *DictManager) Initialize(pinyinUserDictPath, wubiUserDictPath string) error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	// 1. 初始化特殊短语层 (Lv1)
	phrasePath := filepath.Join(dm.dataDir, "phrases.yaml")
	dm.phraseLayer = NewPhraseLayer("phrases", phrasePath)
	if err := dm.phraseLayer.Load(); err != nil {
		log.Printf("[DictManager] 加载短语配置失败: %v", err)
		// 不返回错误，短语是可选的
	} else {
		log.Printf("[DictManager] 短语层加载成功: %d 短语, %d 命令",
			dm.phraseLayer.GetPhraseCount(), dm.phraseLayer.GetCommandCount())
	}
	dm.compositeDict.AddLayer(dm.phraseLayer)

	// 2. 初始化 Shadow 层 (Lv2)
	shadowPath := filepath.Join(dm.dataDir, "shadow.yaml")
	dm.shadowLayer = NewShadowLayer("shadow", shadowPath)
	if err := dm.shadowLayer.Load(); err != nil {
		log.Printf("[DictManager] 加载 Shadow 规则失败: %v", err)
	} else {
		log.Printf("[DictManager] Shadow 层加载成功: %d 规则", dm.shadowLayer.GetRuleCount())
	}
	// Shadow 层作为规则提供者，不作为搜索层
	dm.compositeDict.SetShadowProvider(dm.shadowLayer)

	// 3. 初始化拼音用户词库层 (Lv3)
	dm.pinyinUserDict = NewUserDict("pinyin_user", pinyinUserDictPath)
	if err := dm.pinyinUserDict.Load(); err != nil {
		log.Printf("[DictManager] 加载拼音用户词库失败: %v", err)
	} else {
		log.Printf("[DictManager] 拼音用户词库加载成功: %d 词条", dm.pinyinUserDict.EntryCount())
	}

	// 4. 初始化五笔用户词库层 (Lv3)
	dm.wubiUserDict = NewUserDict("wubi_user", wubiUserDictPath)
	if err := dm.wubiUserDict.Load(); err != nil {
		log.Printf("[DictManager] 加载五笔用户词库失败: %v", err)
	} else {
		log.Printf("[DictManager] 五笔用户词库加载成功: %d 词条", dm.wubiUserDict.EntryCount())
	}

	// 默认激活五笔用户词库（将在 SetActiveEngine 中正确设置）
	dm.activeUserDict = dm.wubiUserDict
	dm.activeEngine = "wubi"
	dm.compositeDict.AddLayer(dm.activeUserDict)

	return nil
}

// RegisterSystemLayer 注册系统词库层
// 由引擎加载码表后调用
func (dm *DictManager) RegisterSystemLayer(name string, layer DictLayer) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	dm.systemLayers[name] = layer
	dm.compositeDict.AddLayer(layer)
	log.Printf("[DictManager] 注册系统词库: %s", name)
}

// UnregisterSystemLayer 取消注册系统词库层
func (dm *DictManager) UnregisterSystemLayer(name string) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if _, ok := dm.systemLayers[name]; ok {
		delete(dm.systemLayers, name)
		dm.compositeDict.RemoveLayer(name)
		log.Printf("[DictManager] 取消注册系统词库: %s", name)
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

// GetUserDict 获取当前活跃的用户词库（用于添加用户词）
func (dm *DictManager) GetUserDict() *UserDict {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeUserDict
}

// SetActiveEngine 切换活跃引擎，同时切换对应的用户词库
func (dm *DictManager) SetActiveEngine(engineType string) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if engineType == dm.activeEngine {
		return
	}

	var newUserDict *UserDict
	switch engineType {
	case "pinyin":
		newUserDict = dm.pinyinUserDict
	case "wubi":
		newUserDict = dm.wubiUserDict
	default:
		log.Printf("[DictManager] 未知引擎类型: %s, 保持当前词库", engineType)
		return
	}

	if newUserDict == nil {
		log.Printf("[DictManager] 目标用户词库未初始化: %s", engineType)
		return
	}

	// 从 CompositeDict 中替换用户词库层
	if dm.activeUserDict != nil {
		dm.compositeDict.RemoveLayer(dm.activeUserDict.Name())
	}
	dm.compositeDict.AddLayer(newUserDict)
	dm.activeUserDict = newUserDict
	dm.activeEngine = engineType

	log.Printf("[DictManager] 切换活跃引擎词库: %s, 词条数: %d", engineType, newUserDict.EntryCount())
}

// GetShadowLayer 获取 Shadow 层（用于置顶/删除操作）
func (dm *DictManager) GetShadowLayer() *ShadowLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.shadowLayer
}

// GetPhraseLayer 获取短语层
func (dm *DictManager) GetPhraseLayer() *PhraseLayer {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.phraseLayer
}

// AddUserWord 添加用户词
func (dm *DictManager) AddUserWord(code, text string, weight int) error {
	if dm.activeUserDict == nil {
		return fmt.Errorf("用户词库未初始化")
	}
	return dm.activeUserDict.Add(code, text, weight)
}

// TopWord 置顶词条
func (dm *DictManager) TopWord(code, word string) {
	if dm.shadowLayer != nil {
		dm.shadowLayer.Top(code, word)
	}
}

// DeleteWord 删除（隐藏）词条
func (dm *DictManager) DeleteWord(code, word string) {
	if dm.shadowLayer != nil {
		dm.shadowLayer.Delete(code, word)
	}
}

// Save 保存所有可写层
func (dm *DictManager) Save() error {
	dm.mu.RLock()
	defer dm.mu.RUnlock()

	var errs []error

	if dm.pinyinUserDict != nil {
		if err := dm.pinyinUserDict.Save(); err != nil {
			errs = append(errs, fmt.Errorf("保存拼音用户词库失败: %w", err))
		}
	}

	if dm.wubiUserDict != nil {
		if err := dm.wubiUserDict.Save(); err != nil {
			errs = append(errs, fmt.Errorf("保存五笔用户词库失败: %w", err))
		}
	}

	if dm.shadowLayer != nil && dm.shadowLayer.IsDirty() {
		if err := dm.shadowLayer.Save(); err != nil {
			errs = append(errs, fmt.Errorf("保存 Shadow 规则失败: %w", err))
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

	// 保存所有数据
	if dm.pinyinUserDict != nil {
		dm.pinyinUserDict.Close()
	}

	if dm.wubiUserDict != nil {
		dm.wubiUserDict.Close()
	}

	if dm.shadowLayer != nil && dm.shadowLayer.IsDirty() {
		dm.shadowLayer.Save()
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

// ReloadShadow 重新加载 Shadow 规则
func (dm *DictManager) ReloadShadow() error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if dm.shadowLayer != nil {
		return dm.shadowLayer.Load()
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

	if dm.shadowLayer != nil {
		stats["shadow_rules"] = dm.shadowLayer.GetRuleCount()
	}

	if dm.pinyinUserDict != nil {
		stats["pinyin_user_words"] = dm.pinyinUserDict.EntryCount()
	}

	if dm.wubiUserDict != nil {
		stats["wubi_user_words"] = dm.wubiUserDict.EntryCount()
	}

	if dm.activeUserDict != nil {
		stats["user_words"] = dm.activeUserDict.EntryCount()
	}

	stats["total_layers"] = len(dm.compositeDict.GetLayers())

	return stats
}
