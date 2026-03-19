package schema

import (
	"fmt"
	"log"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strings"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/dict/dictcache"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
)

// EngineBundle 引擎创建结果（包含引擎实例和相关资源）
type EngineBundle struct {
	SchemaID string
	Engine   interface{} // *pinyin.Engine 或 *wubi.Engine
}

// CreateEngineFromSchema 根据 Schema 创建引擎实例并加载词库
func CreateEngineFromSchema(s *Schema, exeDir string, dm *dict.DictManager) (*EngineBundle, error) {
	switch s.Engine.Type {
	case EngineTypeCodeTable:
		return createCodeTableEngine(s, exeDir, dm)
	case EngineTypePinyin:
		return createPinyinEngine(s, exeDir, dm)
	default:
		return nil, fmt.Errorf("不支持的引擎类型: %s", s.Engine.Type)
	}
}

// createCodeTableEngine 创建码表引擎（五笔等）
func createCodeTableEngine(s *Schema, exeDir string, dm *dict.DictManager) (*EngineBundle, error) {
	spec := s.Engine.CodeTable
	if spec == nil {
		spec = &CodeTableSpec{
			MaxCodeLength:     4,
			TopCodeCommit:     true,
			PunctCommit:       true,
			ShowCodeHint:      true,
			CandidateSortMode: "natural",
		}
	}

	config := &wubi.Config{
		MaxCodeLength:     spec.MaxCodeLength,
		AutoCommitAt4:     spec.AutoCommitUnique,
		ClearOnEmptyAt4:   spec.ClearOnEmptyMax,
		TopCodeCommit:     spec.TopCodeCommit,
		PunctCommit:       spec.PunctCommit,
		ShowCodeHint:      spec.ShowCodeHint,
		SingleCodeInput:   spec.SingleCodeInput,
		FilterMode:        s.Engine.FilterMode,
		CandidateSortMode: spec.CandidateSortMode,
		DedupCandidates:   true,
	}

	engine := wubi.NewEngine(config)

	// 加载主码表
	dictSpec := s.GetDefaultDictSpec()
	if dictSpec != nil {
		srcPath := resolvePath(exeDir, dictSpec.Path)
		if err := loadWubiCodeTable(engine, srcPath); err != nil {
			return nil, fmt.Errorf("加载码表失败: %w", err)
		}
		log.Printf("[SchemaFactory] 码表加载成功 (%s), 词条数: %d", s.Schema.ID, engine.GetEntryCount())
	}

	// 注册码表为 CompositeDict 的 system layer + 设置 DictManager
	if dm != nil {
		codeTable := engine.GetCodeTable()
		if codeTable != nil {
			systemLayer := dict.NewCodeTableLayer("codetable-system", dict.LayerTypeSystem, codeTable)
			dm.RegisterSystemLayer("codetable-system", systemLayer)
		}
		engine.SetDictManager(dm)
		// 同步排序模式到 CompositeDict，避免启动时使用默认的词频排序
		dm.SetSortMode(candidate.CandidateSortMode(spec.CandidateSortMode))
	}

	// 后台预生成拼音 wdb
	go preGeneratePinyinWdb(s, exeDir)

	// GC 释放临时内存
	go func() {
		runtime.GC()
		debug.FreeOSMemory()
	}()

	return &EngineBundle{
		SchemaID: s.Schema.ID,
		Engine:   engine,
	}, nil
}

// createPinyinEngine 创建拼音引擎
func createPinyinEngine(s *Schema, exeDir string, dm *dict.DictManager) (*EngineBundle, error) {
	spec := s.Engine.Pinyin
	if spec == nil {
		spec = &PinyinSpec{
			Scheme:          "full",
			ShowWubiHint:    true,
			UseSmartCompose: true,
		}
	}

	config := &pinyin.Config{
		ShowWubiHint: spec.ShowWubiHint,
		FilterMode:   s.Engine.FilterMode,
	}

	// 模糊音配置
	if spec.Fuzzy != nil && spec.Fuzzy.Enabled {
		config.Fuzzy = &pinyin.FuzzyConfig{
			ZhZ:   spec.Fuzzy.ZhZ,
			ChC:   spec.Fuzzy.ChC,
			ShS:   spec.Fuzzy.ShS,
			NL:    spec.Fuzzy.NL,
			FH:    spec.Fuzzy.FH,
			RL:    spec.Fuzzy.RL,
			AnAng: spec.Fuzzy.AnAng,
			EnEng: spec.Fuzzy.EnEng,
			InIng: spec.Fuzzy.InIng,
		}
	}

	// 加载拼音词库
	pinyinDict := dict.NewPinyinDict()

	dictSpec := s.GetDefaultDictSpec()
	if dictSpec != nil {
		dictPath := resolvePath(exeDir, dictSpec.Path)
		if err := loadPinyinDict(pinyinDict, dictPath); err != nil {
			return nil, fmt.Errorf("加载拼音词库失败: %w", err)
		}
	}

	// 构建 CompositeDict
	var compositeDict *dict.CompositeDict
	if dm != nil {
		systemLayer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
		dm.RegisterSystemLayer("pinyin-system", systemLayer)
		compositeDict = dm.GetCompositeDict()
		log.Printf("[SchemaFactory] 拼音引擎使用 CompositeDict")
	} else {
		// 无 DictManager 时创建独立 CompositeDict
		compositeDict = dict.NewCompositeDict()
		systemLayer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
		compositeDict.AddLayer(systemLayer)
	}

	engine := pinyin.NewEngineWithConfig(compositeDict, config)

	// 加载 Unigram 语言模型
	if s.Learning.UnigramPath != "" {
		unigramTxtPath := resolvePath(exeDir, s.Learning.UnigramPath)
		if err := loadUnigramModel(engine, unigramTxtPath); err != nil {
			log.Printf("[SchemaFactory] %v", err)
		}
	}

	// 加载反查词库（如五笔反查）
	reverseDicts := s.GetDictsByRole(DictRoleReverseLookup)
	for _, rd := range reverseDicts {
		rdPath := resolvePath(exeDir, rd.Path)
		if err := loadWubiTableForPinyin(engine, rdPath); err != nil {
			log.Printf("[SchemaFactory] 加载反查码表失败: %v", err)
		} else {
			log.Printf("[SchemaFactory] 反查码表加载成功")
		}
	}

	// 设置 DictManager
	if dm != nil {
		engine.SetDictManager(dm)
	}

	// 加载用户词频（仅在 learning.mode=auto 或 frequency 时加载）
	// 路径从 schema.UserData.UserFreqFile 读取
	if s.Learning.Mode == LearningAuto || s.Learning.Mode == LearningFrequency {
		if s.UserData.UserFreqFile != "" {
			userFreqPath := resolvePath(exeDir, s.UserData.UserFreqFile)
			loadPinyinUserFreqs(engine, userFreqPath)
		}
		config.EnableUserFreq = true // 同步到引擎 config，控制 OnCandidateSelected
	}

	return &EngineBundle{
		SchemaID: s.Schema.ID,
		Engine:   engine,
	}, nil
}

// --- 词库加载辅助函数（从 manager_init.go 迁移） ---

func loadPinyinDict(pinyinDict *dict.PinyinDict, dictPath string) error {
	wdbInDir := filepath.Join(dictPath, "pinyin.wdb")
	srcPaths := []string{
		filepath.Join(dictPath, "8105.dict.yaml"),
		filepath.Join(dictPath, "base.dict.yaml"),
	}

	if !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if err := pinyinDict.LoadBinary(wdbInDir); err == nil {
			log.Printf("[SchemaFactory] 拼音词库(预编译 wdb)加载成功, 编码数: %d", pinyinDict.EntryCount())
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath("pinyin")
	if dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		if err := dictcache.ConvertPinyinToWdb(dictPath, wdbCachePath); err != nil {
			if _, statErr := os.Stat(wdbInDir); statErr == nil {
				if err := pinyinDict.LoadBinary(wdbInDir); err == nil {
					return nil
				}
			}
			return fmt.Errorf("无法加载拼音词库: %w", err)
		}
	}

	if err := pinyinDict.LoadBinary(wdbCachePath); err != nil {
		return fmt.Errorf("加载缓存拼音词库失败: %w", err)
	}
	log.Printf("[SchemaFactory] 拼音词库(缓存 wdb)加载成功, 编码数: %d", pinyinDict.EntryCount())
	return nil
}

func loadUnigramModel(engine *pinyin.Engine, txtPath string) error {
	wdbPath := strings.TrimSuffix(txtPath, ".txt") + ".wdb"

	if _, err := os.Stat(wdbPath); err == nil {
		if !dictcache.NeedsRegenerate([]string{txtPath}, wdbPath) {
			bm, err := pinyin.NewBinaryUnigramModel(wdbPath)
			if err == nil {
				engine.SetUnigram(bm)
				log.Printf("[SchemaFactory] Unigram 模型(预编译 wdb)加载成功: %d 词条", bm.Size())
				return nil
			}
		}
	}

	wdbCachePath := dictcache.CachePath("unigram")
	if dictcache.NeedsRegenerate([]string{txtPath}, wdbCachePath) {
		if _, err := os.Stat(txtPath); err == nil {
			if err := dictcache.ConvertUnigramToWdb(txtPath, wdbCachePath); err != nil {
				log.Printf("[SchemaFactory] 转换 Unigram 到 wdb 失败: %v", err)
			}
		}
	}

	if _, err := os.Stat(wdbCachePath); err == nil {
		bm, err := pinyin.NewBinaryUnigramModel(wdbCachePath)
		if err == nil {
			engine.SetUnigram(bm)
			log.Printf("[SchemaFactory] Unigram 模型(缓存 wdb)加载成功: %d 词条", bm.Size())
			return nil
		}
	}

	return fmt.Errorf("Unigram 模型 wdb 不可用，智能组句功能将不可用")
}

func loadWubiCodeTable(engine *wubi.Engine, srcPath string) error {
	srcDir := filepath.Dir(srcPath)
	wdbInDir := filepath.Join(srcDir, "wubi.wdb")
	if !dictcache.NeedsRegenerate([]string{srcPath}, wdbInDir) {
		if err := loadWubiFromWdb(engine, wdbInDir); err == nil {
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath("wubi")
	if dictcache.NeedsRegenerate([]string{srcPath}, wdbCachePath) {
		if err := dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath); err != nil {
			return fmt.Errorf("转换五笔码表到 wdb 失败: %w", err)
		}
	}

	if err := loadWubiFromWdb(engine, wdbCachePath); err != nil {
		return fmt.Errorf("加载缓存 wubi.wdb 失败: %w", err)
	}
	return nil
}

func loadWubiFromWdb(engine *wubi.Engine, wdbPath string) error {
	if err := engine.LoadCodeTableBinary(wdbPath); err != nil {
		return err
	}

	// 从 sidecar meta.json 恢复 Header 信息
	meta, err := dictcache.LoadCodeTableMeta(wdbPath)
	if err != nil {
		log.Printf("[SchemaFactory] 加载码表 meta 失败: %v", err)
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

// LoadWubiTableForPinyinEngine 为拼音引擎加载五笔反查码表（导出供热更新使用）
func LoadWubiTableForPinyinEngine(engine *pinyin.Engine, srcPath string) error {
	return loadWubiTableForPinyin(engine, srcPath)
}

func loadWubiTableForPinyin(engine *pinyin.Engine, srcPath string) error {
	srcDir := filepath.Dir(srcPath)
	wdbInDir := filepath.Join(srcDir, "wubi.wdb")
	if !dictcache.NeedsRegenerate([]string{srcPath}, wdbInDir) {
		if err := engine.LoadWubiTableBinary(wdbInDir); err == nil {
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath("wubi")
	if dictcache.NeedsRegenerate([]string{srcPath}, wdbCachePath) {
		if err := dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath); err != nil {
			return fmt.Errorf("生成五笔反查码表缓存失败: %w", err)
		}
	}

	if err := engine.LoadWubiTableBinary(wdbCachePath); err == nil {
		return nil
	}

	return fmt.Errorf("五笔反查码表 wdb 不可用")
}

// LoadPinyinUserFreqs 加载拼音用户词频
func loadPinyinUserFreqs(engine *pinyin.Engine, path string) {
	if engine.GetUnigram() == nil {
		return
	}
	if m := engine.GetUnigramModel(); m != nil {
		if err := m.LoadUserFreqs(path); err != nil {
			log.Printf("[SchemaFactory] 加载用户词频失败: %v", err)
		} else {
			log.Printf("[SchemaFactory] 用户词频加载成功")
		}
		return
	}
	if bm := engine.GetBinaryUnigramModel(); bm != nil {
		if err := bm.LoadUserFreqs(path); err != nil {
			log.Printf("[SchemaFactory] 加载用户词频失败: %v", err)
		} else {
			log.Printf("[SchemaFactory] 用户词频加载成功")
		}
	}
}

// SavePinyinUserFreqs 保存拼音用户词频
func SavePinyinUserFreqs(engine *pinyin.Engine, path string) {
	if engine.GetUnigram() == nil {
		return
	}
	if m := engine.GetUnigramModel(); m != nil {
		if err := m.SaveUserFreqs(path); err != nil {
			log.Printf("[SchemaFactory] 保存用户词频失败: %v", err)
		}
		return
	}
	if bm := engine.GetBinaryUnigramModel(); bm != nil {
		if err := bm.SaveUserFreqs(path); err != nil {
			log.Printf("[SchemaFactory] 保存用户词频失败: %v", err)
		}
	}
}

func preGeneratePinyinWdb(s *Schema, exeDir string) {
	// 查找拼音词库路径
	var pinyinDictPath string
	for _, d := range s.Dicts {
		if d.Type == "rime_pinyin" {
			pinyinDictPath = resolvePath(exeDir, d.Path)
			break
		}
	}

	// 如果当前方案没有拼音词库，尝试默认路径
	if pinyinDictPath == "" {
		pinyinDictPath = resolvePath(exeDir, "dict/pinyin")
	}

	srcPaths := []string{
		filepath.Join(pinyinDictPath, "8105.dict.yaml"),
		filepath.Join(pinyinDictPath, "base.dict.yaml"),
	}
	wdbCachePath := dictcache.CachePath("pinyin")
	if dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		log.Printf("[SchemaFactory] 后台预生成拼音 wdb...")
		if err := dictcache.ConvertPinyinToWdb(pinyinDictPath, wdbCachePath); err != nil {
			log.Printf("[SchemaFactory] 后台预生成拼音 wdb 失败: %v", err)
		}
	}

	// 预生成 Unigram
	unigramTxtPath := resolvePath(exeDir, "dict/pinyin/unigram.txt")
	unigramWdbPath := strings.TrimSuffix(unigramTxtPath, ".txt") + ".wdb"
	unigramCachePath := dictcache.CachePath("unigram")

	if _, err := os.Stat(unigramWdbPath); err == nil {
		if !dictcache.NeedsRegenerate([]string{unigramTxtPath}, unigramWdbPath) {
			return
		}
	}
	if dictcache.NeedsRegenerate([]string{unigramTxtPath}, unigramCachePath) {
		if _, err := os.Stat(unigramTxtPath); err == nil {
			dictcache.ConvertUnigramToWdb(unigramTxtPath, unigramCachePath)
		}
	}

	runtime.GC()
	debug.FreeOSMemory()
}

// resolvePath 解析相对路径为绝对路径
func resolvePath(exeDir, path string) string {
	if path == "" {
		return ""
	}
	if isAbsPath(path) {
		return path
	}
	if exeDir != "" {
		return filepath.Join(exeDir, path)
	}
	return path
}

func isAbsPath(path string) bool {
	if len(path) == 0 {
		return false
	}
	if len(path) >= 2 && path[1] == ':' {
		return true
	}
	if len(path) >= 2 && path[0] == '\\' && path[1] == '\\' {
		return true
	}
	return path[0] == '/'
}
