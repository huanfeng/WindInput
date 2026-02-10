package engine

import (
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/dictcache"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
)

// Manager 引擎管理器
type Manager struct {
	mu            sync.RWMutex
	engines       map[EngineType]Engine
	currentType   EngineType
	currentEngine Engine

	// 词库路径（用于动态切换时加载）
	pinyinDictPath string
	wubiDictPath   string

	// 引擎配置
	pinyinConfig *pinyin.Config
	wubiConfig   *wubi.Config

	// 可执行文件目录（用于相对路径）
	exeDir string

	// 词库管理器
	dictManager *dict.DictManager
}

// NewManager 创建引擎管理器
func NewManager() *Manager {
	return &Manager{
		engines: make(map[EngineType]Engine),
	}
}

// RegisterEngine 注册引擎
func (m *Manager) RegisterEngine(engineType EngineType, engine Engine) {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.engines[engineType] = engine
	log.Printf("[EngineManager] 注册引擎: %s", engineType)
}

// SetCurrentEngine 设置当前引擎
func (m *Manager) SetCurrentEngine(engineType EngineType) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	engine, ok := m.engines[engineType]
	if !ok {
		return fmt.Errorf("引擎未注册: %s", engineType)
	}

	m.currentType = engineType
	m.currentEngine = engine
	log.Printf("[EngineManager] 切换引擎: %s", engineType)
	return nil
}

// GetCurrentEngine 获取当前引擎
func (m *Manager) GetCurrentEngine() Engine {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentEngine
}

// GetCurrentType 获取当前引擎类型
func (m *Manager) GetCurrentType() EngineType {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentType
}

// Convert 使用当前引擎转换输入
func (m *Manager) Convert(input string, maxCandidates int) ([]candidate.Candidate, error) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return nil, fmt.Errorf("未设置当前引擎")
	}
	return engine.Convert(input, maxCandidates)
}

// ConvertRaw 使用当前引擎转换输入（不应用过滤，用于测试）
func (m *Manager) ConvertRaw(input string, maxCandidates int) ([]candidate.Candidate, error) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return nil, fmt.Errorf("未设置当前引擎")
	}

	// 检查引擎是否支持 ConvertRaw
	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		return pinyinEngine.ConvertRaw(input, maxCandidates)
	}
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		return wubiEngine.ConvertRaw(input, maxCandidates)
	}

	// 回退到普通 Convert
	return engine.Convert(input, maxCandidates)
}

// ConvertEx 扩展转换，返回更多信息
func (m *Manager) ConvertEx(input string, maxCandidates int) *ConvertResult {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return &ConvertResult{}
	}

	// 五笔引擎：使用五笔扩展功能
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		wubiResult := wubiEngine.ConvertEx(input, maxCandidates)
		return &ConvertResult{
			Candidates:   wubiResult.Candidates,
			ShouldCommit: wubiResult.ShouldCommit,
			CommitText:   wubiResult.CommitText,
			IsEmpty:      wubiResult.IsEmpty,
			ShouldClear:  wubiResult.ShouldClear,
			ToEnglish:    wubiResult.ToEnglish,
		}
	}

	// 拼音引擎：使用新的 ConvertEx 方法
	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		pinyinResult := pinyinEngine.ConvertEx(input, maxCandidates)
		result := &ConvertResult{
			Candidates:     pinyinResult.Candidates,
			IsEmpty:        pinyinResult.IsEmpty,
			PreeditDisplay: pinyinResult.PreeditDisplay,
		}
		// 填充组合态信息
		if pinyinResult.Composition != nil {
			result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
			result.PartialSyllable = pinyinResult.Composition.PartialSyllable
			result.HasPartial = pinyinResult.Composition.HasPartial()
		}
		return result
	}

	// 普通引擎
	candidates, err := engine.Convert(input, maxCandidates)
	if err != nil {
		log.Printf("[EngineManager] 转换错误: %v", err)
	}

	return &ConvertResult{
		Candidates: candidates,
		IsEmpty:    len(candidates) == 0,
	}
}

// Reset 重置当前引擎
func (m *Manager) Reset() {
	engine := m.GetCurrentEngine()
	if engine != nil {
		engine.Reset()
	}
}

// GetMaxCodeLength 获取当前引擎的最大码长
func (m *Manager) GetMaxCodeLength() int {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return 0
	}

	// 五笔引擎
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		return wubiEngine.GetConfig().MaxCodeLength
	}

	// 拼音默认不限制
	return 100
}

// HandleTopCode 处理顶码
func (m *Manager) HandleTopCode(input string) (commitText string, newInput string, shouldCommit bool) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return "", input, false
	}

	// 五笔引擎支持顶码
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		return wubiEngine.HandleTopCode(input)
	}

	return "", input, false
}

// EngineConfig 引擎配置
type EngineConfig struct {
	Type         EngineType
	DictPath     string
	WubiDictPath string         // 五笔词库路径（用于拼音反查五笔）
	PinyinConfig *pinyin.Config // 拼音引擎配置
	WubiConfig   *wubi.Config   // 五笔引擎配置
}

// InitializeFromConfig 从配置初始化引擎
func (m *Manager) InitializeFromConfig(config *EngineConfig) error {
	switch config.Type {
	case EngineTypePinyin:
		return m.initPinyinEngine(config.DictPath, config.WubiDictPath, config.PinyinConfig)
	case EngineTypeWubi:
		return m.initWubiEngine(config.DictPath, config.WubiConfig)
	default:
		return fmt.Errorf("未知引擎类型: %s", config.Type)
	}
}

// initPinyinEngine 初始化拼音引擎
func (m *Manager) initPinyinEngine(dictPath, wubiDictPath string, config *pinyin.Config) error {
	// 确定使用哪个词库
	var d dict.Dict

	// 加载拼音词库：统一走 wdb 路径
	pinyinDict := dict.NewPinyinDict()
	if dictPath != "" {
		if err := loadPinyinDict(pinyinDict, dictPath); err != nil {
			return fmt.Errorf("加载拼音词库失败: %w", err)
		}
	}

	if m.dictManager != nil {
		// 注册系统词库到 DictManager
		systemLayer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
		m.dictManager.RegisterSystemLayer("pinyin-system", systemLayer)

		// 使用 CompositeDict
		d = m.dictManager.GetCompositeDict()
		log.Printf("[EngineManager] 拼音引擎使用 CompositeDict")
	} else {
		// 回退：直接使用 PinyinDict
		d = pinyinDict
	}

	// 创建引擎
	engine := pinyin.NewEngineWithConfig(d, config)

	// 加载 Unigram 语言模型（失败不阻断，仅影响智能组句）
	unigramTxtPath := "dict/pinyin/unigram.txt"
	if m.exeDir != "" {
		unigramTxtPath = m.exeDir + "/" + unigramTxtPath
	}
	if err := loadUnigramModel(engine, unigramTxtPath); err != nil {
		log.Printf("[EngineManager] %v", err)
	}

	// 始终加载五笔码表（临时拼音模式也需要五笔反查）
	// 优先使用 wdb 二进制（mmap），避免文本模式占用大量堆内存
	if wubiDictPath != "" {
		if err := loadWubiTableForPinyin(engine, wubiDictPath); err != nil {
			log.Printf("[EngineManager] 加载五笔反查码表失败: %v", err)
		} else {
			log.Printf("[EngineManager] 五笔反查码表加载成功")
		}
	}

	// 设置 DictManager（用于用户词频学习）
	if m.dictManager != nil {
		engine.SetDictManager(m.dictManager)
	}

	// 加载用户词频
	userFreqPath := "dict/pinyin/user_freq.txt"
	if m.exeDir != "" {
		userFreqPath = m.exeDir + "/" + userFreqPath
	}
	loadPinyinUserFreqs(engine, userFreqPath)

	m.RegisterEngine(EngineTypePinyin, engine)
	return m.SetCurrentEngine(EngineTypePinyin)
}

// loadPinyinDict 加载拼音词库，统一走 wdb 路径
func loadPinyinDict(pinyinDict *dict.PinyinDict, dictPath string) error {
	// 优先检查 exe 目录下已有的预编译 wdb
	wdbInDir := filepath.Join(dictPath, "pinyin.wdb")
	srcPaths := []string{
		filepath.Join(dictPath, "8105.dict.yaml"),
		filepath.Join(dictPath, "base.dict.yaml"),
	}

	// 如果 exe 目录下已有 wdb 且比源文件新，直接使用
	if !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if err := pinyinDict.LoadBinary(wdbInDir); err == nil {
			log.Printf("[EngineManager] 拼音词库(预编译 wdb)加载成功，编码数: %d", pinyinDict.EntryCount())
			return nil
		}
		log.Printf("[EngineManager] 预编译 wdb 加载失败，尝试缓存路径")
	}

	// 尝试缓存目录
	wdbCachePath := dictcache.CachePath("pinyin")
	if dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		log.Printf("[EngineManager] 拼音词库缓存需要重新生成")
		if err := dictcache.ConvertPinyinToWdb(dictPath, wdbCachePath); err != nil {
			log.Printf("[EngineManager] 转换拼音词库到 wdb 失败: %v", err)
			// 最终 fallback: 直接用 exe 目录的 wdb（即使它可能过时）
			if _, statErr := os.Stat(wdbInDir); statErr == nil {
				if err := pinyinDict.LoadBinary(wdbInDir); err == nil {
					log.Printf("[EngineManager] 拼音词库(旧 wdb)加载成功，编码数: %d", pinyinDict.EntryCount())
					return nil
				}
			}
			return fmt.Errorf("无法加载拼音词库: %w", err)
		}
	}

	if err := pinyinDict.LoadBinary(wdbCachePath); err != nil {
		return fmt.Errorf("加载缓存拼音词库失败: %w", err)
	}
	log.Printf("[EngineManager] 拼音词库(缓存 wdb)加载成功，编码数: %d", pinyinDict.EntryCount())
	return nil
}

// loadUnigramModel 加载 Unigram 语言模型（仅 wdb 二进制），返回错误供调用方决定是否阻断
func loadUnigramModel(engine *pinyin.Engine, txtPath string) error {
	wdbPath := strings.TrimSuffix(txtPath, ".txt") + ".wdb"

	// 优先检查 exe 目录下已有的预编译 wdb
	if _, err := os.Stat(wdbPath); err == nil {
		if !dictcache.NeedsRegenerate([]string{txtPath}, wdbPath) {
			bm, err := pinyin.NewBinaryUnigramModel(wdbPath)
			if err == nil {
				engine.SetUnigram(bm)
				log.Printf("[EngineManager] Unigram 模型(预编译 wdb)加载成功: %d 词条", bm.Size())
				return nil
			}
			log.Printf("[EngineManager] 预编译 Unigram wdb 加载失败: %v", err)
		}
	}

	// 尝试缓存目录
	wdbCachePath := dictcache.CachePath("unigram")
	if dictcache.NeedsRegenerate([]string{txtPath}, wdbCachePath) {
		if _, err := os.Stat(txtPath); err == nil {
			if err := dictcache.ConvertUnigramToWdb(txtPath, wdbCachePath); err != nil {
				log.Printf("[EngineManager] 转换 Unigram 到 wdb 失败: %v", err)
			}
		}
	}

	// 尝试加载缓存 wdb
	if _, err := os.Stat(wdbCachePath); err == nil {
		bm, err := pinyin.NewBinaryUnigramModel(wdbCachePath)
		if err == nil {
			engine.SetUnigram(bm)
			log.Printf("[EngineManager] Unigram 模型(缓存 wdb)加载成功: %d 词条", bm.Size())
			return nil
		}
		log.Printf("[EngineManager] 加载缓存 Unigram 失败: %v", err)
	}

	return fmt.Errorf("Unigram 模型 wdb 不可用，智能组句功能将不可用")
}

// initWubiEngine 初始化五笔引擎
func (m *Manager) initWubiEngine(dictPath string, config *wubi.Config) error {
	// 创建五笔引擎
	engine := wubi.NewEngine(config)

	// 加载主码表：优先使用 wdb 缓存
	if dictPath != "" {
		if err := loadWubiCodeTable(engine, dictPath); err != nil {
			return fmt.Errorf("加载五笔码表失败: %w", err)
		}
		log.Printf("[EngineManager] 五笔码表加载成功，词条数: %d", engine.GetEntryCount())
	}

	// 设置 DictManager（用于查询用户词和短语）
	if m.dictManager != nil {
		engine.SetDictManager(m.dictManager)
	}

	// 注册并设置为当前引擎
	m.RegisterEngine(EngineTypeWubi, engine)
	if err := m.SetCurrentEngine(EngineTypeWubi); err != nil {
		return err
	}

	// 后台预生成拼音 wdb，避免首次临时拼音切换卡顿
	go m.preGeneratePinyinWdb()

	return nil
}

// loadWubiCodeTable 加载五笔码表，优先使用预编译 wdb
func loadWubiCodeTable(engine *wubi.Engine, srcPath string) error {
	// 优先检查 exe 目录下预编译的 wdb（和 wubi86.txt 同目录）
	srcDir := filepath.Dir(srcPath)
	wdbInDir := filepath.Join(srcDir, "wubi.wdb")
	if !dictcache.NeedsRegenerate([]string{srcPath}, wdbInDir) {
		if err := loadWubiFromWdb(engine, wdbInDir); err == nil {
			log.Printf("[EngineManager] 五笔码表(预编译 wdb)加载成功")
			return nil
		}
		log.Printf("[EngineManager] 预编译 wubi.wdb 加载失败，尝试缓存路径")
	}

	// 尝试缓存目录
	wdbCachePath := dictcache.CachePath("wubi")
	if dictcache.NeedsRegenerate([]string{srcPath}, wdbCachePath) {
		log.Printf("[EngineManager] 五笔码表缓存需要重新生成")
		if err := dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath); err != nil {
			return fmt.Errorf("转换五笔码表到 wdb 失败: %w", err)
		}
	}

	if err := loadWubiFromWdb(engine, wdbCachePath); err != nil {
		return fmt.Errorf("加载缓存 wubi.wdb 失败: %w", err)
	}

	log.Printf("[EngineManager] 五笔码表(缓存 wdb)加载成功")
	return nil
}

// loadWubiFromWdb 从 wdb 文件加载五笔码表并恢复 Header
func loadWubiFromWdb(engine *wubi.Engine, wdbPath string) error {
	if err := engine.LoadCodeTableBinary(wdbPath); err != nil {
		return err
	}

	// 从 meta.json 恢复 Header 信息
	meta, err := dictcache.LoadCodeTableMeta(wdbPath)
	if err != nil {
		log.Printf("[EngineManager] 加载码表 meta 失败: %v", err)
	} else {
		engine.RestoreCodeTableHeader(dict.CodeTableHeader{
			Name:          meta.Name,
			Version:       meta.Version,
			Author:        meta.Author,
			CodeScheme:    meta.CodeScheme,
			CodeLength:    meta.CodeLength,
			BWCodeLength:  meta.BWCodeLength,
			SpecialPrefix: meta.SpecialPrefix,
			PhraseRule:    meta.PhraseRule,
		})
	}
	return nil
}

// loadWubiTableForPinyin 为拼音引擎加载五笔反查码表（仅 wdb 二进制）
func loadWubiTableForPinyin(engine *pinyin.Engine, srcPath string) error {
	// 优先：与 wubi86.txt 同目录的预编译 wubi.wdb
	srcDir := filepath.Dir(srcPath)
	wdbInDir := filepath.Join(srcDir, "wubi.wdb")
	if !dictcache.NeedsRegenerate([]string{srcPath}, wdbInDir) {
		if err := engine.LoadWubiTableBinary(wdbInDir); err == nil {
			log.Printf("[EngineManager] 五笔反查码表(预编译 wdb, mmap)加载成功")
			return nil
		}
	}

	// 其次：缓存目录的 wubi.wdb（不存在则按需生成）
	wdbCachePath := dictcache.CachePath("wubi")
	if dictcache.NeedsRegenerate([]string{srcPath}, wdbCachePath) {
		log.Printf("[EngineManager] 五笔反查码表缓存不存在，按需生成")
		if err := dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath); err != nil {
			return fmt.Errorf("生成五笔反查码表缓存失败: %w", err)
		}
	}

	if err := engine.LoadWubiTableBinary(wdbCachePath); err == nil {
		log.Printf("[EngineManager] 五笔反查码表(缓存 wdb, mmap)加载成功")
		return nil
	}

	return fmt.Errorf("五笔反查码表 wdb 不可用，仅影响编码提示功能")
}

// GetEngineInfo 获取当前引擎信息
func (m *Manager) GetEngineInfo() string {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return "未加载引擎"
	}

	engineType := m.GetCurrentType()

	// 五笔引擎
	if wubiEngine, ok := engine.(*wubi.Engine); ok {
		info := wubiEngine.GetCodeTableInfo()
		if info != nil {
			return fmt.Sprintf("%s: %s (%d词条)", engineType, info.Name, wubiEngine.GetEntryCount())
		}
	}

	return string(engineType)
}

// SetExeDir 设置可执行文件目录
func (m *Manager) SetExeDir(dir string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.exeDir = dir
}

// SetDictManager 设置词库管理器
func (m *Manager) SetDictManager(dm *dict.DictManager) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.dictManager = dm
}

// GetDictManager 获取词库管理器
func (m *Manager) GetDictManager() *dict.DictManager {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.dictManager
}

// SetDictPaths 设置词库路径
func (m *Manager) SetDictPaths(pinyinPath, wubiPath string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.pinyinDictPath = pinyinPath
	m.wubiDictPath = wubiPath
}

// SetPinyinConfig 设置拼音配置
func (m *Manager) SetPinyinConfig(config *pinyin.Config) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.pinyinConfig = config
}

// SetWubiConfig 设置五笔配置
func (m *Manager) SetWubiConfig(config *wubi.Config) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.wubiConfig = config
}

// SwitchEngine 切换到指定引擎
func (m *Manager) SwitchEngine(targetType EngineType) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 如果已经是目标引擎，不做任何操作
	if m.currentType == targetType {
		return nil
	}

	// 检查是否已注册
	if engine, ok := m.engines[targetType]; ok {
		m.currentType = targetType
		m.currentEngine = engine
		log.Printf("[EngineManager] 切换到已注册引擎: %s", targetType)
		return nil
	}

	// 需要动态加载引擎
	var err error
	switch targetType {
	case EngineTypePinyin:
		err = m.loadPinyinEngineLocked()
	case EngineTypeWubi:
		err = m.loadWubiEngineLocked()
	default:
		return fmt.Errorf("未知引擎类型: %s", targetType)
	}

	if err != nil {
		return err
	}

	m.currentType = targetType
	m.currentEngine = m.engines[targetType]
	log.Printf("[EngineManager] 动态加载并切换引擎: %s", targetType)
	return nil
}

// ToggleEngine 在拼音和五笔之间切换
func (m *Manager) ToggleEngine() (EngineType, error) {
	currentType := m.GetCurrentType()
	var targetType EngineType

	switch currentType {
	case EngineTypePinyin:
		targetType = EngineTypeWubi
	case EngineTypeWubi:
		targetType = EngineTypePinyin
	default:
		targetType = EngineTypePinyin
	}

	if err := m.SwitchEngine(targetType); err != nil {
		return currentType, err
	}

	return targetType, nil
}

// loadPinyinEngineLocked 加载拼音引擎（需要持有锁）
func (m *Manager) loadPinyinEngineLocked() error {
	dictPath := m.pinyinDictPath
	if dictPath == "" {
		dictPath = "dict/pinyin"
	}

	fullPath := dictPath
	if m.exeDir != "" && !isAbsPath(dictPath) {
		fullPath = m.exeDir + "/" + dictPath
	}

	// 确定使用哪个词库
	var d dict.Dict

	// 加载拼音词库：统一走 wdb 路径
	pinyinDict := dict.NewPinyinDict()
	if err := loadPinyinDict(pinyinDict, fullPath); err != nil {
		return fmt.Errorf("加载拼音词库失败: %w", err)
	}

	if m.dictManager != nil {
		// 注册系统词库
		systemLayer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
		m.dictManager.RegisterSystemLayer("pinyin-system", systemLayer)
		d = m.dictManager.GetCompositeDict()
		log.Printf("[EngineManager] 拼音引擎使用 CompositeDict")
	} else {
		d = pinyinDict
	}

	// 创建引擎
	config := m.pinyinConfig
	if config == nil {
		config = &pinyin.Config{ShowWubiHint: true, FilterMode: "smart"}
	}
	engine := pinyin.NewEngineWithConfig(d, config)

	// 加载 Unigram 语言模型（失败不阻断，仅影响智能组句）
	unigramTxtPath := "dict/pinyin/unigram.txt"
	if m.exeDir != "" {
		unigramTxtPath = m.exeDir + "/" + unigramTxtPath
	}
	if err := loadUnigramModel(engine, unigramTxtPath); err != nil {
		log.Printf("[EngineManager] %v", err)
	}

	// 始终加载五笔码表（临时拼音模式也需要五笔反查）
	// 优先使用 wdb 二进制（mmap），避免文本模式占用大量堆内存
	if m.wubiDictPath != "" {
		wubiFullPath := m.wubiDictPath
		if m.exeDir != "" && !isAbsPath(m.wubiDictPath) {
			wubiFullPath = m.exeDir + "/" + m.wubiDictPath
		}
		if err := loadWubiTableForPinyin(engine, wubiFullPath); err != nil {
			log.Printf("[EngineManager] 加载五笔反查码表失败: %v", err)
		} else {
			log.Printf("[EngineManager] 五笔反查码表加载成功")
		}
	}

	// 设置 DictManager（用于用户词频学习）
	if m.dictManager != nil {
		engine.SetDictManager(m.dictManager)
	}

	// 加载用户词频
	userFreqPath := "dict/pinyin/user_freq.txt"
	if m.exeDir != "" {
		userFreqPath = m.exeDir + "/" + userFreqPath
	}
	loadPinyinUserFreqs(engine, userFreqPath)

	m.engines[EngineTypePinyin] = engine
	return nil
}

// loadWubiEngineLocked 加载五笔引擎（需要持有锁）
func (m *Manager) loadWubiEngineLocked() error {
	dictPath := m.wubiDictPath
	if dictPath == "" {
		dictPath = "dict/wubi/wubi86.txt"
	}

	fullPath := dictPath
	if m.exeDir != "" && !isAbsPath(dictPath) {
		fullPath = m.exeDir + "/" + dictPath
	}

	config := m.wubiConfig
	if config == nil {
		config = wubi.DefaultConfig()
	}

	engine := wubi.NewEngine(config)
	if err := loadWubiCodeTable(engine, fullPath); err != nil {
		return fmt.Errorf("加载五笔码表失败: %w", err)
	}

	// 设置 DictManager（用于查询用户词和短语，不含系统词库）
	if m.dictManager != nil {
		engine.SetDictManager(m.dictManager)
	}

	m.engines[EngineTypeWubi] = engine
	log.Printf("[EngineManager] 五笔引擎加载成功，词条数: %d", engine.GetEntryCount())
	return nil
}

// isAbsPath 判断是否为绝对路径
func isAbsPath(path string) bool {
	if len(path) == 0 {
		return false
	}
	// Windows 绝对路径: C:\ 或 \\
	if len(path) >= 2 && path[1] == ':' {
		return true
	}
	if len(path) >= 2 && path[0] == '\\' && path[1] == '\\' {
		return true
	}
	// Unix 绝对路径
	return path[0] == '/'
}

// OnCandidateSelected 选词回调，通知引擎用户选择了某个候选词
// code 为输入的编码（如拼音字符串），text 为选中的文字
func (m *Manager) OnCandidateSelected(code, text string) {
	engine := m.GetCurrentEngine()
	if engine == nil {
		return
	}

	// 拼音引擎：记录选词到用户词典
	if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
		pinyinEngine.OnCandidateSelected(code, text)
	}
}

// SaveUserFreqs 保存拼音引擎的用户词频到文件
func (m *Manager) SaveUserFreqs() {
	m.mu.RLock()
	defer m.mu.RUnlock()

	for _, eng := range m.engines {
		if pinyinEngine, ok := eng.(*pinyin.Engine); ok {
			userFreqPath := "dict/pinyin/user_freq.txt"
			if m.exeDir != "" {
				userFreqPath = m.exeDir + "/" + userFreqPath
			}
			savePinyinUserFreqs(pinyinEngine, userFreqPath)
		}
	}
}

// loadPinyinUserFreqs 加载拼音引擎的用户词频（兼容内存模式和二进制模式）
func loadPinyinUserFreqs(engine *pinyin.Engine, path string) {
	if engine.GetUnigram() == nil {
		return
	}
	if m := engine.GetUnigramModel(); m != nil {
		if err := m.LoadUserFreqs(path); err != nil {
			log.Printf("[EngineManager] 加载用户词频失败: %v", err)
		} else {
			log.Printf("[EngineManager] 用户词频加载成功")
		}
		return
	}
	if bm := engine.GetBinaryUnigramModel(); bm != nil {
		if err := bm.LoadUserFreqs(path); err != nil {
			log.Printf("[EngineManager] 加载用户词频失败: %v", err)
		} else {
			log.Printf("[EngineManager] 用户词频加载成功")
		}
	}
}

// savePinyinUserFreqs 保存拼音引擎的用户词频
func savePinyinUserFreqs(engine *pinyin.Engine, path string) {
	if engine.GetUnigram() == nil {
		return
	}
	if m := engine.GetUnigramModel(); m != nil {
		if err := m.SaveUserFreqs(path); err != nil {
			log.Printf("[EngineManager] 保存用户词频失败: %v", err)
		}
		return
	}
	if bm := engine.GetBinaryUnigramModel(); bm != nil {
		if err := bm.SaveUserFreqs(path); err != nil {
			log.Printf("[EngineManager] 保存用户词频失败: %v", err)
		}
	}
}

// GetEngineDisplayName 获取引擎显示名称
func (m *Manager) GetEngineDisplayName() string {
	switch m.GetCurrentType() {
	case EngineTypePinyin:
		return "拼"
	case EngineTypeWubi:
		return "五"
	default:
		return "?"
	}
}

// UpdateFilterMode 更新当前引擎的过滤模式
func (m *Manager) UpdateFilterMode(mode string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 更新保存的配置
	if m.pinyinConfig != nil {
		m.pinyinConfig.FilterMode = mode
	}
	if m.wubiConfig != nil {
		m.wubiConfig.FilterMode = mode
	}

	// 更新当前运行的引擎配置
	if m.currentEngine != nil {
		switch e := m.currentEngine.(type) {
		case *pinyin.Engine:
			if cfg := e.GetConfig(); cfg != nil {
				cfg.FilterMode = mode
			}
		case *wubi.Engine:
			if cfg := e.GetConfig(); cfg != nil {
				cfg.FilterMode = mode
			}
		}
	}

	// 更新所有已注册引擎的配置
	for _, engine := range m.engines {
		switch e := engine.(type) {
		case *pinyin.Engine:
			if cfg := e.GetConfig(); cfg != nil {
				cfg.FilterMode = mode
			}
		case *wubi.Engine:
			if cfg := e.GetConfig(); cfg != nil {
				cfg.FilterMode = mode
			}
		}
	}

	log.Printf("[EngineManager] 更新过滤模式: %s", mode)
}

// UpdateWubiOptions 更新五笔引擎的选项（热更新）
func (m *Manager) UpdateWubiOptions(autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput bool) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 更新保存的配置
	if m.wubiConfig != nil {
		m.wubiConfig.AutoCommitAt4 = autoCommitAt4
		m.wubiConfig.ClearOnEmptyAt4 = clearOnEmptyAt4
		m.wubiConfig.TopCodeCommit = topCodeCommit
		m.wubiConfig.PunctCommit = punctCommit
		m.wubiConfig.ShowCodeHint = showCodeHint
		m.wubiConfig.SingleCodeInput = singleCodeInput
	}

	// 更新所有已注册的五笔引擎的配置
	for _, engine := range m.engines {
		if wubiEngine, ok := engine.(*wubi.Engine); ok {
			if cfg := wubiEngine.GetConfig(); cfg != nil {
				cfg.AutoCommitAt4 = autoCommitAt4
				cfg.ClearOnEmptyAt4 = clearOnEmptyAt4
				cfg.TopCodeCommit = topCodeCommit
				cfg.PunctCommit = punctCommit
				cfg.ShowCodeHint = showCodeHint
				cfg.SingleCodeInput = singleCodeInput
			}
		}
	}

	log.Printf("[EngineManager] 更新五笔选项: autoCommitAt4=%v, clearOnEmptyAt4=%v, topCodeCommit=%v, punctCommit=%v, showCodeHint=%v, singleCodeInput=%v",
		autoCommitAt4, clearOnEmptyAt4, topCodeCommit, punctCommit, showCodeHint, singleCodeInput)
}

// EnsurePinyinLoaded 确保拼音引擎已加载（不切换当前引擎）
// 用于临时拼音模式：在五笔模式下按需加载拼音引擎
func (m *Manager) EnsurePinyinLoaded() error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if _, ok := m.engines[EngineTypePinyin]; ok {
		return nil // 已加载
	}

	log.Printf("[EngineManager] 临时拼音：加载拼音引擎...")
	return m.loadPinyinEngineLocked()
}

// ConvertWithPinyin 使用拼音引擎转换输入（不切换当前引擎）
// 强制添加五笔编码提示，用于临时拼音模式
func (m *Manager) ConvertWithPinyin(input string, maxCandidates int) *ConvertResult {
	m.mu.RLock()
	pinyinEngine, ok := m.engines[EngineTypePinyin]
	m.mu.RUnlock()

	if !ok {
		return &ConvertResult{IsEmpty: true}
	}

	pe, ok := pinyinEngine.(*pinyin.Engine)
	if !ok {
		return &ConvertResult{IsEmpty: true}
	}

	pinyinResult := pe.ConvertEx(input, maxCandidates)

	// 强制添加五笔编码提示
	pe.AddWubiHintsForced(pinyinResult.Candidates)

	result := &ConvertResult{
		Candidates:     pinyinResult.Candidates,
		IsEmpty:        pinyinResult.IsEmpty,
		PreeditDisplay: pinyinResult.PreeditDisplay,
	}
	if pinyinResult.Composition != nil {
		result.CompletedSyllables = pinyinResult.Composition.CompletedSyllables
		result.PartialSyllable = pinyinResult.Composition.PartialSyllable
		result.HasPartial = pinyinResult.Composition.HasPartial()
	}
	return result
}

// preGeneratePinyinWdb 后台预生成拼音相关的二进制词库
// 在五笔引擎初始化后调用，避免首次临时拼音切换卡顿
// 包括：拼音词库 wdb 和 Unigram 模型 wdb
func (m *Manager) preGeneratePinyinWdb() {
	m.mu.RLock()
	dictPath := m.pinyinDictPath
	exeDir := m.exeDir
	m.mu.RUnlock()

	if dictPath == "" {
		dictPath = "dict/pinyin"
	}
	fullPath := dictPath
	if exeDir != "" && !isAbsPath(dictPath) {
		fullPath = exeDir + "/" + dictPath
	}

	// 1. 预生成拼音词库 wdb
	srcPaths := []string{
		filepath.Join(fullPath, "8105.dict.yaml"),
		filepath.Join(fullPath, "base.dict.yaml"),
	}
	wdbCachePath := dictcache.CachePath("pinyin")
	if dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		log.Printf("[EngineManager] 后台预生成拼音 wdb...")
		if err := dictcache.ConvertPinyinToWdb(fullPath, wdbCachePath); err != nil {
			log.Printf("[EngineManager] 后台预生成拼音 wdb 失败: %v", err)
		} else {
			log.Printf("[EngineManager] 后台预生成拼音 wdb 完成")
		}
	}

	// 2. 预生成 Unigram 模型 wdb
	unigramTxtPath := "dict/pinyin/unigram.txt"
	if exeDir != "" {
		unigramTxtPath = exeDir + "/" + unigramTxtPath
	}
	unigramWdbPath := strings.TrimSuffix(unigramTxtPath, ".txt") + ".wdb"
	unigramCachePath := dictcache.CachePath("unigram")

	// 如果 exe 目录下预编译 wdb 已存在且最新，无需生成缓存
	if _, err := os.Stat(unigramWdbPath); err == nil {
		if !dictcache.NeedsRegenerate([]string{unigramTxtPath}, unigramWdbPath) {
			return
		}
	}
	// 检查缓存是否需要重新生成
	if dictcache.NeedsRegenerate([]string{unigramTxtPath}, unigramCachePath) {
		if _, err := os.Stat(unigramTxtPath); err == nil {
			log.Printf("[EngineManager] 后台预生成 Unigram wdb...")
			if err := dictcache.ConvertUnigramToWdb(unigramTxtPath, unigramCachePath); err != nil {
				log.Printf("[EngineManager] 后台预生成 Unigram wdb 失败: %v", err)
			} else {
				log.Printf("[EngineManager] 后台预生成 Unigram wdb 完成")
			}
		}
	}
}

// UpdatePinyinOptions 更新拼音引擎的选项（热更新）
func (m *Manager) UpdatePinyinOptions(showWubiHint bool) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 更新保存的配置
	if m.pinyinConfig != nil {
		m.pinyinConfig.ShowWubiHint = showWubiHint
	}

	// 更新所有已注册的拼音引擎的配置
	for _, engine := range m.engines {
		if pinyinEngine, ok := engine.(*pinyin.Engine); ok {
			if cfg := pinyinEngine.GetConfig(); cfg != nil {
				oldShowWubiHint := cfg.ShowWubiHint
				cfg.ShowWubiHint = showWubiHint

				// 从 true→false：释放反向索引
				if oldShowWubiHint && !showWubiHint {
					pinyinEngine.ReleaseWubiHint()
				}
			}
			// 如果开启反查但五笔码表未加载，则加载（仅 wdb 二进制）
			if showWubiHint && m.wubiDictPath != "" {
				wubiFullPath := m.wubiDictPath
				if m.exeDir != "" && !isAbsPath(m.wubiDictPath) {
					wubiFullPath = m.exeDir + "/" + m.wubiDictPath
				}
				if err := loadWubiTableForPinyin(pinyinEngine, wubiFullPath); err != nil {
					log.Printf("[EngineManager] 加载五笔反查码表失败: %v", err)
				} else {
					log.Printf("[EngineManager] 五笔反查码表加载成功")
				}
			}
		}
	}

	log.Printf("[EngineManager] 更新拼音选项: showWubiHint=%v", showWubiHint)
}
