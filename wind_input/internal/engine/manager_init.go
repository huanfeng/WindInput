package engine

import (
	"fmt"
	"log"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strings"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/dictcache"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
)

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

	// 码表加载完成后主动释放转换过程中产生的临时内存
	go func() {
		runtime.GC()
		debug.FreeOSMemory()
		log.Printf("[EngineManager] 五笔引擎初始化后内存已释放")
	}()

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

	// 预生成完成后主动释放转换过程中产生的临时内存
	runtime.GC()
	debug.FreeOSMemory()
	log.Printf("[EngineManager] 拼音 wdb 预生成后内存已释放")
}
