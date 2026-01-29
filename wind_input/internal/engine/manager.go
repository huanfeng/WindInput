package engine

import (
	"fmt"
	"log"
	"sync"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict"
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

	// 如果是扩展引擎，使用扩展功能
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
	Type           EngineType
	DictPath       string
	WubiDictPath   string         // 五笔词库路径（用于拼音反查五笔）
	PinyinConfig   *pinyin.Config // 拼音引擎配置
	WubiConfig     *wubi.Config   // 五笔引擎配置
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
	// 加载词库
	d := dict.NewSimpleDict()
	if dictPath != "" {
		if err := d.Load(dictPath); err != nil {
			return fmt.Errorf("加载拼音词库失败: %w", err)
		}
	}

	// 创建引擎
	engine := pinyin.NewEngineWithConfig(d, config)

	// 如果启用五笔反查，加载五笔码表
	if config != nil && config.ShowWubiHint && wubiDictPath != "" {
		if err := engine.LoadWubiTable(wubiDictPath); err != nil {
			log.Printf("[EngineManager] 加载五笔反查码表失败: %v", err)
			// 不返回错误，反查是可选功能
		} else {
			log.Printf("[EngineManager] 五笔反查码表加载成功")
		}
	}

	m.RegisterEngine(EngineTypePinyin, engine)
	return m.SetCurrentEngine(EngineTypePinyin)
}

// initWubiEngine 初始化五笔引擎
func (m *Manager) initWubiEngine(dictPath string, config *wubi.Config) error {
	// 创建五笔引擎
	engine := wubi.NewEngine(config)

	// 加载主码表
	if dictPath != "" {
		if err := engine.LoadCodeTable(dictPath); err != nil {
			return fmt.Errorf("加载五笔码表失败: %w", err)
		}
		log.Printf("[EngineManager] 五笔码表加载成功，词条数: %d", engine.GetEntryCount())
	}

	// 注册并设置为当前引擎
	m.RegisterEngine(EngineTypeWubi, engine)
	return m.SetCurrentEngine(EngineTypeWubi)
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
		dictPath = "dict/pinyin/pinyin.txt"
	}

	fullPath := dictPath
	if m.exeDir != "" && !isAbsPath(dictPath) {
		fullPath = m.exeDir + "/" + dictPath
	}

	// 加载拼音词库
	d := dict.NewSimpleDict()
	if err := d.Load(fullPath); err != nil {
		return fmt.Errorf("加载拼音词库失败: %w", err)
	}

	// 创建引擎
	config := m.pinyinConfig
	if config == nil {
		config = &pinyin.Config{ShowWubiHint: true, FilterMode: "smart"}
	}
	engine := pinyin.NewEngineWithConfig(d, config)

	// 如果启用五笔反查，加载五笔码表
	if config.ShowWubiHint && m.wubiDictPath != "" {
		wubiFullPath := m.wubiDictPath
		if m.exeDir != "" && !isAbsPath(m.wubiDictPath) {
			wubiFullPath = m.exeDir + "/" + m.wubiDictPath
		}
		if err := engine.LoadWubiTable(wubiFullPath); err != nil {
			log.Printf("[EngineManager] 加载五笔反查码表失败: %v", err)
		} else {
			log.Printf("[EngineManager] 五笔反查码表加载成功")
		}
	}

	m.engines[EngineTypePinyin] = engine
	log.Printf("[EngineManager] 拼音引擎加载成功，词条数: %d", d.EntryCount())
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
	if err := engine.LoadCodeTable(fullPath); err != nil {
		return fmt.Errorf("加载五笔码表失败: %w", err)
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
func (m *Manager) UpdateWubiOptions(autoCommit wubi.AutoCommitMode, emptyCode wubi.EmptyCodeMode, topCodeCommit, punctCommit bool) {
	m.mu.Lock()
	defer m.mu.Unlock()

	// 更新保存的配置
	if m.wubiConfig != nil {
		m.wubiConfig.AutoCommit = autoCommit
		m.wubiConfig.EmptyCode = emptyCode
		m.wubiConfig.TopCodeCommit = topCodeCommit
		m.wubiConfig.PunctCommit = punctCommit
	}

	// 更新所有已注册的五笔引擎的配置
	for _, engine := range m.engines {
		if wubiEngine, ok := engine.(*wubi.Engine); ok {
			if cfg := wubiEngine.GetConfig(); cfg != nil {
				cfg.AutoCommit = autoCommit
				cfg.EmptyCode = emptyCode
				cfg.TopCodeCommit = topCodeCommit
				cfg.PunctCommit = punctCommit
			}
		}
	}

	log.Printf("[EngineManager] 更新五笔选项: autoCommit=%d, emptyCode=%d, topCodeCommit=%v, punctCommit=%v",
		autoCommit, emptyCode, topCodeCommit, punctCommit)
}
