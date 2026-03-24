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
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/engine/pinyin/shuangpin"
	"github.com/huanfeng/wind_input/internal/engine/wubi"
)

// EngineBundle 引擎创建结果（包含引擎实例和相关资源）
type EngineBundle struct {
	SchemaID string
	Engine   interface{} // *pinyin.Engine 或 *wubi.Engine 或 *mixed.Engine
}

// CreateEngineFromSchema 根据 Schema 创建引擎实例并加载词库
func CreateEngineFromSchema(s *Schema, exeDir string, dm *dict.DictManager) (*EngineBundle, error) {
	switch s.Engine.Type {
	case EngineTypeCodeTable:
		return createCodeTableEngine(s, exeDir, dm)
	case EngineTypePinyin:
		return createPinyinEngine(s, exeDir, dm)
	case EngineTypeMixed:
		return createMixedEngine(s, exeDir, dm)
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

	// 五笔学习配置
	if s.Learning.Mode == LearningAuto || s.Learning.Mode == LearningFrequency {
		config.EnableUserFreq = true
		config.ProtectTopN = s.Learning.ProtectTopN
	}

	engine := wubi.NewEngine(config)

	// 加载主码表
	dictSpec := s.GetDefaultDictSpec()
	if dictSpec != nil {
		srcPath := resolvePath(exeDir, dictSpec.Path)
		if err := loadWubiCodeTable(engine, srcPath, dictSpec.Type); err != nil {
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

	// 配置双拼转换器
	if spec.Scheme == "shuangpin" && spec.Shuangpin != nil {
		spScheme := shuangpin.Get(spec.Shuangpin.Layout)
		if spScheme != nil {
			engine.SetShuangpinConverter(shuangpin.NewConverter(spScheme))
			log.Printf("[SchemaFactory] 双拼模式: layout=%s (%s)", spScheme.ID, spScheme.Name)
		} else {
			log.Printf("[SchemaFactory] 未知的双拼方案: %s，回退到全拼", spec.Shuangpin.Layout)
		}
	}

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
		if err := loadWubiTableForPinyin(engine, rdPath, rd.Type); err != nil {
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
	dictDir := filepath.Dir(dictPath)
	srcPaths := dictcache.RimePinyinSourcePaths(dictPath)

	wdbInDir := filepath.Join(dictDir, "pinyin.wdb")
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

func loadWubiCodeTable(engine *wubi.Engine, srcPath, dictType string) error {
	var srcDir string
	var srcPaths []string

	if dictType == "rime_wubi" {
		// srcPath 是主词库 .dict.yaml 文件路径，自动发现关联词库
		srcDir = filepath.Dir(srcPath)
		srcPaths = dictcache.RimeWubiSourcePaths(srcPath)
	} else {
		// 传统单文件码表格式
		srcDir = filepath.Dir(srcPath)
		srcPaths = []string{srcPath}
	}

	wdbInDir := filepath.Join(srcDir, "wubi.wdb")
	if len(srcPaths) > 0 && !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if err := loadWubiFromWdb(engine, wdbInDir); err == nil {
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath("wubi")
	if len(srcPaths) == 0 || dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		var convertErr error
		if dictType == "rime_wubi" {
			convertErr = dictcache.ConvertRimeWubiToWdb(srcPath, wdbCachePath)
		} else {
			convertErr = dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath)
		}
		if convertErr != nil {
			return fmt.Errorf("转换五笔码表到 wdb 失败: %w", convertErr)
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
func LoadWubiTableForPinyinEngine(engine *pinyin.Engine, srcPath, dictType string) error {
	return loadWubiTableForPinyin(engine, srcPath, dictType)
}

func loadWubiTableForPinyin(engine *pinyin.Engine, srcPath, dictType string) error {
	var srcDir string
	var srcPaths []string

	if dictType == "rime_wubi" {
		srcDir = filepath.Dir(srcPath)
		srcPaths = dictcache.RimeWubiSourcePaths(srcPath)
	} else {
		srcDir = filepath.Dir(srcPath)
		srcPaths = []string{srcPath}
	}

	wdbInDir := filepath.Join(srcDir, "wubi.wdb")
	if len(srcPaths) > 0 && !dictcache.NeedsRegenerate(srcPaths, wdbInDir) {
		if err := engine.LoadWubiTableBinary(wdbInDir); err == nil {
			return nil
		}
	}

	wdbCachePath := dictcache.CachePath("wubi")
	if len(srcPaths) == 0 || dictcache.NeedsRegenerate(srcPaths, wdbCachePath) {
		var convertErr error
		if dictType == "rime_wubi" {
			convertErr = dictcache.ConvertRimeWubiToWdb(srcPath, wdbCachePath)
		} else {
			convertErr = dictcache.ConvertCodeTableToWdb(srcPath, wdbCachePath)
		}
		if convertErr != nil {
			return fmt.Errorf("生成五笔反查码表缓存失败: %w", convertErr)
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
		pinyinDictPath = resolvePath(exeDir, "dict/pinyin/rime_ice.dict.yaml")
	}

	srcPaths := dictcache.RimePinyinSourcePaths(pinyinDictPath)
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

// createMixedEngine 创建混输引擎（五笔+拼音并行查询）
// 五笔引擎使用 DictManager 的主 CompositeDict（含 codetable-system 层），
// 拼音引擎使用独立的 CompositeDict（含 pinyin-system 层），避免交叉污染。
func createMixedEngine(s *Schema, exeDir string, dm *dict.DictManager) (*EngineBundle, error) {
	// === 1. 读取混输配置 ===
	mixedSpec := s.Engine.Mixed
	if mixedSpec == nil {
		mixedSpec = &MixedSpec{
			MinPinyinLength: 2,
			WubiWeightBoost: 10000000,
			ShowSourceHint:  true,
		}
	}

	// === 2. 创建五笔引擎 ===
	codeTableSpec := s.Engine.CodeTable
	if codeTableSpec == nil {
		codeTableSpec = &CodeTableSpec{
			MaxCodeLength:     4,
			TopCodeCommit:     true,
			PunctCommit:       true,
			ShowCodeHint:      true,
			CandidateSortMode: "frequency",
		}
	}

	wubiConfig := &wubi.Config{
		MaxCodeLength:     codeTableSpec.MaxCodeLength,
		AutoCommitAt4:     codeTableSpec.AutoCommitUnique,
		ClearOnEmptyAt4:   codeTableSpec.ClearOnEmptyMax,
		TopCodeCommit:     codeTableSpec.TopCodeCommit,
		PunctCommit:       codeTableSpec.PunctCommit,
		ShowCodeHint:      codeTableSpec.ShowCodeHint,
		SingleCodeInput:   codeTableSpec.SingleCodeInput,
		FilterMode:        s.Engine.FilterMode,
		CandidateSortMode: codeTableSpec.CandidateSortMode,
		DedupCandidates:   true,
		SkipShadow:        true, // 混输模式：Shadow 由 MixedEngine 合并后统一应用
	}

	// 五笔学习配置
	if s.Learning.Mode == LearningAuto || s.Learning.Mode == LearningFrequency {
		wubiConfig.EnableUserFreq = true
		wubiConfig.ProtectTopN = s.Learning.ProtectTopN
	}

	wubiEngine := wubi.NewEngine(wubiConfig)

	// 加载五笔码表（从 dictionaries 中查找五笔词库）
	var wubiDictSpec *DictSpec
	for i := range s.Dicts {
		if s.Dicts[i].Default {
			wubiDictSpec = &s.Dicts[i]
			break
		}
	}
	if wubiDictSpec != nil {
		srcPath := resolvePath(exeDir, wubiDictSpec.Path)
		if err := loadWubiCodeTable(wubiEngine, srcPath, wubiDictSpec.Type); err != nil {
			return nil, fmt.Errorf("混输：加载五笔码表失败: %w", err)
		}
		log.Printf("[SchemaFactory] 混输：五笔码表加载成功 (%s), 词条数: %d", s.Schema.ID, wubiEngine.GetEntryCount())
	}

	// 注册五笔码表到 DictManager 的主 CompositeDict
	if dm != nil {
		codeTable := wubiEngine.GetCodeTable()
		if codeTable != nil {
			systemLayer := dict.NewCodeTableLayer("codetable-system", dict.LayerTypeSystem, codeTable)
			dm.RegisterSystemLayer("codetable-system", systemLayer)
		}
		wubiEngine.SetDictManager(dm)
		dm.SetSortMode(candidate.CandidateSortMode(codeTableSpec.CandidateSortMode))
	}

	// === 3. 创建拼音引擎（使用独立的 CompositeDict）===
	pinyinSpec := s.Engine.Pinyin
	if pinyinSpec == nil {
		pinyinSpec = &PinyinSpec{
			Scheme:          "full",
			ShowWubiHint:    true,
			UseSmartCompose: true,
		}
	}

	pinyinConfig := &pinyin.Config{
		ShowWubiHint: pinyinSpec.ShowWubiHint,
		FilterMode:   s.Engine.FilterMode,
		SkipShadow:   true, // 混输模式：Shadow 由 MixedEngine 合并后统一应用
	}

	// 模糊音配置
	if pinyinSpec.Fuzzy != nil && pinyinSpec.Fuzzy.Enabled {
		pinyinConfig.Fuzzy = &pinyin.FuzzyConfig{
			ZhZ:   pinyinSpec.Fuzzy.ZhZ,
			ChC:   pinyinSpec.Fuzzy.ChC,
			ShS:   pinyinSpec.Fuzzy.ShS,
			NL:    pinyinSpec.Fuzzy.NL,
			FH:    pinyinSpec.Fuzzy.FH,
			RL:    pinyinSpec.Fuzzy.RL,
			AnAng: pinyinSpec.Fuzzy.AnAng,
			EnEng: pinyinSpec.Fuzzy.EnEng,
			InIng: pinyinSpec.Fuzzy.InIng,
		}
	}

	// 加载拼音词库
	pinyinDict := dict.NewPinyinDict()
	var pinyinDictSpec *DictSpec
	for i := range s.Dicts {
		if s.Dicts[i].Type == "rime_pinyin" {
			pinyinDictSpec = &s.Dicts[i]
			break
		}
	}
	if pinyinDictSpec != nil {
		dictPath := resolvePath(exeDir, pinyinDictSpec.Path)
		if err := loadPinyinDict(pinyinDict, dictPath); err != nil {
			return nil, fmt.Errorf("混输：加载拼音词库失败: %w", err)
		}
	}

	// 创建独立的 CompositeDict（仅包含拼音系统层，不污染五笔查询）
	pinyinCompositeDict := dict.NewCompositeDict()
	pinyinSystemLayer := dict.NewPinyinDictLayer("pinyin-system", dict.LayerTypeSystem, pinyinDict)
	pinyinCompositeDict.AddLayer(pinyinSystemLayer)

	// 缓存拼音系统层到 engine manager（供临时拼音模式恢复使用）
	if dm != nil {
		dm.RegisterSystemLayer("pinyin-system", pinyinSystemLayer)
		// 立即从主 CompositeDict 移除拼音层，只保留在独立 dict 中
		if mainDict := dm.GetCompositeDict(); mainDict != nil {
			mainDict.RemoveLayer("pinyin-system")
		}
	}

	pinyinEngine := pinyin.NewEngineWithConfig(pinyinCompositeDict, pinyinConfig)

	// 混输模式下的双拼转换器
	if pinyinSpec.Scheme == "shuangpin" && pinyinSpec.Shuangpin != nil {
		spScheme := shuangpin.Get(pinyinSpec.Shuangpin.Layout)
		if spScheme != nil {
			pinyinEngine.SetShuangpinConverter(shuangpin.NewConverter(spScheme))
			log.Printf("[SchemaFactory] 混输双拼模式: layout=%s (%s)", spScheme.ID, spScheme.Name)
		}
	}

	// 加载 Unigram 语言模型
	if s.Learning.UnigramPath != "" {
		unigramTxtPath := resolvePath(exeDir, s.Learning.UnigramPath)
		if err := loadUnigramModel(pinyinEngine, unigramTxtPath); err != nil {
			log.Printf("[SchemaFactory] 混输：%v", err)
		}
	}

	// 加载反查词库（五笔反查）
	reverseDicts := s.GetDictsByRole(DictRoleReverseLookup)
	for _, rd := range reverseDicts {
		rdPath := resolvePath(exeDir, rd.Path)
		if err := loadWubiTableForPinyin(pinyinEngine, rdPath, rd.Type); err != nil {
			log.Printf("[SchemaFactory] 混输：加载反查码表失败: %v", err)
		}
	}

	// 设置拼音引擎的 DictManager（用于用户词频学习）
	if dm != nil {
		pinyinEngine.SetDictManager(dm)
	}

	// 加载拼音用户词频
	if s.Learning.Mode == LearningAuto || s.Learning.Mode == LearningFrequency {
		if s.UserData.UserFreqFile != "" {
			userFreqPath := resolvePath(exeDir, s.UserData.UserFreqFile)
			loadPinyinUserFreqs(pinyinEngine, userFreqPath)
		}
		pinyinConfig.EnableUserFreq = true
	}

	// === 4. 创建混输引擎 ===
	mixedConfig := &mixed.Config{
		MinPinyinLength: mixedSpec.MinPinyinLength,
		WubiWeightBoost: mixedSpec.WubiWeightBoost,
		ShowSourceHint:  mixedSpec.ShowSourceHint,
	}
	if mixedConfig.MinPinyinLength <= 0 {
		mixedConfig.MinPinyinLength = 2
	}
	if mixedConfig.WubiWeightBoost <= 0 {
		mixedConfig.WubiWeightBoost = 10000000
	}

	mixedEngine := mixed.NewEngine(wubiEngine, pinyinEngine, mixedConfig)

	// 设置 DictManager（用于合并后统一应用 Shadow 规则）
	if dm != nil {
		mixedEngine.SetDictManager(dm)
	}

	log.Printf("[SchemaFactory] 混输引擎创建成功 (%s): 五笔=%d词条, 拼音=%d编码",
		s.Schema.ID, wubiEngine.GetEntryCount(), pinyinDict.EntryCount())

	// GC 释放临时内存
	go func() {
		runtime.GC()
		debug.FreeOSMemory()
	}()

	return &EngineBundle{
		SchemaID: s.Schema.ID,
		Engine:   mixedEngine,
	}, nil
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
