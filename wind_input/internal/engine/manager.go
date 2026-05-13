package engine

import (
	"errors"
	"fmt"
	"log/slog"
	"sync"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine/codetable"
	"github.com/huanfeng/wind_input/internal/engine/mixed"
	"github.com/huanfeng/wind_input/internal/engine/pinyin"
	"github.com/huanfeng/wind_input/internal/schema"
	"github.com/huanfeng/wind_input/pkg/encoding"
)

// Manager 引擎管理器
type Manager struct {
	mu sync.RWMutex
	// engineBuildMu 串行化引擎创建过程，避免同一方案被并发构建。
	// 与 m.mu 解耦，重 IO 期间不持 m.mu，按键路径不被阻塞。
	engineBuildMu sync.Mutex
	engines       map[string]Engine         // schemaID -> Engine
	systemLayers  map[string]dict.DictLayer // schemaID -> 该方案注册的系统词库层
	currentID     string                    // 当前活跃方案 ID
	currentEngine Engine

	// 临时方案切换
	tempSchemaID  string // 非空 = 临时方案模式
	savedSchemaID string // 临时切换前的方案 ID

	// 方案管理器
	schemaManager *schema.SchemaManager

	// 数据根目录（exeDir/data）
	dataRoot string

	// 词库管理器
	dictManager *dict.DictManager

	// 反向索引缓存（字 → 编码列表）
	// 缓存键由 primaryCodetableID 决定（独立于 currentID），
	// 这样切到拼音方案时反向索引不会失效
	cachedReverseIndex    map[string][]string
	cachedReverseSchemaID string

	// primaryCodetableID / primaryPinyinID 由 main.go / reload 路径写入
	// 拼音/双拼引擎的"编码提示"统一从主码表方案派生；
	// 码表方案的"临时拼音"统一指向主拼音方案。
	primaryCodetableID string
	primaryPinyinID    string

	// 英文词库
	englishDict  *dict.EnglishDict
	englishLayer *dict.EnglishDictLayer

	// 日志
	logger *slog.Logger
}

// NewManager 创建引擎管理器
func NewManager(logger *slog.Logger) *Manager {
	return &Manager{
		engines:      make(map[string]Engine),
		systemLayers: make(map[string]dict.DictLayer),
		logger:       logger,
	}
}

// SetSchemaManager 设置方案管理器
func (m *Manager) SetSchemaManager(sm *schema.SchemaManager) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.schemaManager = sm
}

// SetDataRoot 设置数据根目录（exeDir/data）
func (m *Manager) SetDataRoot(dir string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.dataRoot = dir
}

// SetPrimarySchemas 设置主码表 / 主拼音方案。
//
// - 主码表：拼音/双拼方案的编码提示从此方案的码表派生（运行期反向索引）；
// - 主拼音：码表方案的临时拼音/快捷输入指向此方案。
//
// 正常情况下两个 ID 均来自配置文件的显式设置（设置界面保证始终写入非空值）。
// 仅在首次启动、配置文件尚未写入时才触发兜底推断（码表取第一个，拼音优先全拼）。
// 主码表方案变更会清空 cachedReverseIndex，下次访问时按新方案重建。
func (m *Manager) SetPrimarySchemas(codetableID, pinyinID string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if codetableID == "" {
		codetableID = m.inferPrimaryByTypeLocked(schema.EngineTypeCodeTable)
	}
	if pinyinID == "" {
		pinyinID = m.inferPrimaryByTypeLocked(schema.EngineTypePinyin)
	}
	if codetableID != m.primaryCodetableID {
		m.cachedReverseIndex = nil
		m.cachedReverseSchemaID = ""
	}
	m.primaryCodetableID = codetableID
	m.primaryPinyinID = pinyinID
	m.logger.Info("主方案设置", "primaryCodetable", codetableID, "primaryPinyin", pinyinID)
}

// GetPrimaryCodetableID 返回当前主码表方案 ID
func (m *Manager) GetPrimaryCodetableID() string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.primaryCodetableID
}

// GetPrimaryPinyinID 返回当前主拼音方案 ID
func (m *Manager) GetPrimaryPinyinID() string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.primaryPinyinID
}

// inferPrimaryByTypeLocked 按引擎类型从 SchemaManager 中选合适的主方案。
// 对拼音类型：优先选全拼方案，再选双拼，避免因列表顺序随机选中双拼作为默认。
// 调用方需持有 m.mu。
func (m *Manager) inferPrimaryByTypeLocked(t schema.EngineType) string {
	if m.schemaManager == nil {
		return ""
	}
	var firstMatch string
	for _, info := range m.schemaManager.ListSchemas() {
		s := m.schemaManager.GetSchema(info.ID)
		if s == nil {
			continue
		}
		if s.Engine.Type == t {
			if firstMatch == "" {
				firstMatch = info.ID
			}
			// 拼音类：优先选全拼，避免双拼因排序靠前而成为默认
			if t == schema.EngineTypePinyin && s.Engine.Pinyin != nil &&
				s.Engine.Pinyin.Scheme == schema.PinyinSchemeFull {
				return info.ID
			}
		}
		// 混输方案：包含码表子引擎，可作为主码表回退
		if t == schema.EngineTypeCodeTable && s.Engine.Type == schema.EngineTypeMixed {
			if s.Engine.Mixed != nil && s.Engine.Mixed.PrimarySchema != "" {
				return s.Engine.Mixed.PrimarySchema
			}
		}
	}
	return firstMatch
}

// SetDictManager 设置词库管理器
func (m *Manager) SetDictManager(dm *dict.DictManager) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.dictManager = dm
}

// SetCurrentIDForTest 仅供测试使用: 直接设置 currentID, 不触发引擎构建 /
// 词库层注册. 让 IsTempPinyinEnabled / IsZKeyRepeatEnabled 等纯查询能基于
// 注入的 schema 工作.
// 生产代码请走 SwitchSchema.
func (m *Manager) SetCurrentIDForTest(id string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.currentID = id
}

// GetDictManager 获取词库管理器
func (m *Manager) GetDictManager() *dict.DictManager {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.dictManager
}

// SwitchSchema 切换到指定方案（如引擎未加载则创建）
//
// 锁策略：引擎构建（重 IO）发生在 m.mu 之外，仅在最终提交切换时短暂持写锁，
// 避免按键路径在 GetCurrentEngine 上排队。构建期间旧引擎仍是 currentEngine。
func (m *Manager) SwitchSchema(schemaID string) error {
	// Phase 1: 快路径——已加载则直接切换
	m.mu.Lock()
	if m.currentID == schemaID {
		m.mu.Unlock()
		return nil
	}
	if _, ok := m.engines[schemaID]; ok {
		m.applySwitchLocked(schemaID)
		m.mu.Unlock()
		m.logger.Info("切换到已加载方案", "schemaID", schemaID)
		return nil
	}
	m.mu.Unlock()

	// Phase 2: 慢路径——构建引擎（不持 m.mu）
	if err := m.ensureEngineBuilt(schemaID); err != nil {
		return err
	}

	// Phase 3: 提交切换
	m.mu.Lock()
	defer m.mu.Unlock()
	if _, ok := m.engines[schemaID]; !ok {
		return fmt.Errorf("方案 %q 构建后未注册", schemaID)
	}
	m.applySwitchLocked(schemaID)
	m.logger.Info("加载并切换方案", "schemaID", schemaID)
	return nil
}

// applySwitchLocked 执行系统词库层切换并更新 currentID/currentEngine。
// 调用方必须持有 m.mu 写锁。
func (m *Manager) applySwitchLocked(schemaID string) {
	if m.dictManager != nil {
		m.dictManager.UnregisterSystemLayer("codetable-system")
		m.dictManager.UnregisterSystemLayer("pinyin-system")
	}
	m.currentID = schemaID
	m.currentEngine = m.engines[schemaID]
	m.cachedReverseIndex = nil
	m.cachedReverseSchemaID = ""
	m.reRegisterSystemLayer(schemaID)
}

// ToggleSchemaResult 方案切换结果
type ToggleSchemaResult struct {
	// NewSchemaID 成功切换到的方案 ID；若一圈下来未找到可切换方案，
	// 该字段保持当前方案 ID 不变（调用方据此决定是否要更新 UI / 持久化配置）。
	NewSchemaID string
	// SkippedSchemas 因真正的加载失败而跳过的方案（ID → 错误信息）。
	// 应展示为"<方案>异常"。
	SkippedSchemas map[string]string
	// PendingSchemas 因资源（如拼音 wdat 缓存）尚在后台生成而暂时不可用的方案。
	// 区别于 SkippedSchemas：这些方案预期很快会就绪，UI 应展示"<方案>准备中"
	// 而不是"<方案>异常"，避免误导用户去排查。
	PendingSchemas map[string]string
}

// ToggleSchema 按 available 列表循环切换方案
// available 为配置中启用的方案 ID 列表（顺序决定切换顺序）；
// 若为空则回退到 SchemaManager 中所有已加载方案。
// 当下一个方案加载失败时，会自动跳过并尝试后续方案。
func (m *Manager) ToggleSchema(available []string) (*ToggleSchemaResult, error) {
	m.mu.RLock()
	sm := m.schemaManager
	currentID := m.currentID
	m.mu.RUnlock()

	if sm == nil {
		return nil, fmt.Errorf("SchemaManager 未设置")
	}

	// 使用 available 列表；若为空则回退到所有已加载方案
	var idList []string
	if len(available) > 0 {
		idList = available
	} else {
		schemas := sm.ListSchemas()
		for _, s := range schemas {
			idList = append(idList, s.ID)
		}
	}

	if len(idList) <= 1 {
		return &ToggleSchemaResult{NewSchemaID: currentID}, nil
	}

	// 找当前方案在列表中的位置
	startIdx := 0
	for i, id := range idList {
		if id == currentID {
			startIdx = i
			break
		}
	}

	// 从下一个方案开始，逐个尝试切换，跳过失败/构建中的方案
	var skipped, pending map[string]string
	n := len(idList)
	for offset := 1; offset < n; offset++ {
		candidateID := idList[(startIdx+offset)%n]

		if err := m.SwitchSchema(candidateID); err != nil {
			if errors.Is(err, schema.ErrAssetBuilding) {
				// 资源还在后台生成，不算"加载失败"，避免上层报"方案异常"
				m.logger.Info("方案资源准备中，暂跳过", "schemaID", candidateID)
				if pending == nil {
					pending = make(map[string]string)
				}
				pending[candidateID] = err.Error()
				continue
			}
			m.logger.Warn("方案加载失败，跳过", "schemaID", candidateID, "error", err)
			if skipped == nil {
				skipped = make(map[string]string)
			}
			skipped[candidateID] = err.Error()
			continue
		}

		// 切换成功，同步 DictManager
		m.mu.RLock()
		dm := m.dictManager
		m.mu.RUnlock()
		if dm != nil {
			s := sm.GetSchema(candidateID)
			if s != nil {
				dm.SwitchSchemaFull(candidateID, s.DataSchemaID(),
					s.Learning.TempMaxEntries, s.Learning.TempPromoteCount,
					s.Schema.ID)
			}
		}

		// 更新 SchemaManager 的活跃方案
		sm.SetActive(candidateID)

		return &ToggleSchemaResult{
			NewSchemaID:    candidateID,
			SkippedSchemas: skipped,
			PendingSchemas: pending,
		}, nil
	}

	// 一圈未成功：若全是真失败则返回 error；若仅为"准备中"或混合，
	// 保留当前方案不动并把状态返回给上层做友好提示。
	if len(skipped) > 0 && len(pending) == 0 {
		return nil, fmt.Errorf("所有可用方案均加载失败")
	}
	return &ToggleSchemaResult{
		NewSchemaID:    currentID,
		SkippedSchemas: skipped,
		PendingSchemas: pending,
	}, nil
}

// ActivateTempSchema 临时激活方案（如码表方案下临时用拼音）
func (m *Manager) ActivateTempSchema(schemaID string) error {
	// 预检：避免在 ensureEngineBuilt 之后才发现已在临时模式
	m.mu.RLock()
	if m.tempSchemaID != "" {
		existing := m.tempSchemaID
		m.mu.RUnlock()
		return fmt.Errorf("已在临时方案模式中: %s", existing)
	}
	m.mu.RUnlock()

	// 构建引擎（不持 m.mu）
	if err := m.ensureEngineBuilt(schemaID); err != nil {
		return err
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	if m.tempSchemaID != "" {
		return fmt.Errorf("已在临时方案模式中: %s", m.tempSchemaID)
	}
	if _, ok := m.engines[schemaID]; !ok {
		return fmt.Errorf("方案 %q 构建后未注册", schemaID)
	}

	m.savedSchemaID = m.currentID
	m.tempSchemaID = schemaID
	m.currentID = schemaID
	m.currentEngine = m.engines[schemaID]
	m.logger.Info("临时激活方案", "schemaID", schemaID, "saved", m.savedSchemaID)
	return nil
}

// DeactivateTempSchema 退出临时方案，恢复到之前的方案
func (m *Manager) DeactivateTempSchema() {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.tempSchemaID == "" {
		return
	}

	if eng, ok := m.engines[m.savedSchemaID]; ok {
		m.currentID = m.savedSchemaID
		m.currentEngine = eng
	}

	m.logger.Info("退出临时方案", "tempSchemaID", m.tempSchemaID, "restored", m.savedSchemaID)
	m.tempSchemaID = ""
	m.savedSchemaID = ""
}

// IsTempSchemaActive 是否处于临时方案模式
func (m *Manager) IsTempSchemaActive() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.tempSchemaID != ""
}

// ensureEngineBuilt 构建方案引擎（如未加载），不持 m.mu 写锁。
//
// 锁策略：
//   - engineBuildMu 串行化构建，避免同方案被并发构建。
//   - 重 IO（CreateEngineFromSchema：词典转换、mmap、unigram 加载等）在锁外执行，
//     按键路径的 GetCurrentEngine 在此期间不会被阻塞。
//   - 仅在最后注册引擎/系统词库层时短暂持 m.mu 写锁。
//
// 注意：构建期间，factory 内部的 dm.RegisterSystemLayer 会修改共享的 CompositeDict，
// 旧引擎在此期间的查询可能短暂看到混合层；该窗口仅在 IO 期间存在，影响有限。
func (m *Manager) ensureEngineBuilt(schemaID string, opts ...schema.EngineCreateOptions) error {
	m.engineBuildMu.Lock()
	defer m.engineBuildMu.Unlock()

	// 在 m.mu 内快速取出构建所需的引用（不在 IO 期间持锁）
	m.mu.RLock()
	if _, ok := m.engines[schemaID]; ok {
		m.mu.RUnlock()
		return nil
	}
	sm := m.schemaManager
	dataRoot := m.dataRoot
	dictManager := m.dictManager
	m.mu.RUnlock()

	if sm == nil {
		return fmt.Errorf("SchemaManager 未设置")
	}
	s := sm.GetSchema(schemaID)
	if s == nil {
		return fmt.Errorf("方案 %q 不存在", schemaID)
	}

	resolver := func(id string) *schema.Schema {
		return sm.GetSchema(id)
	}
	dataDir := sm.GetDataDir()

	// 重 IO 在锁外执行
	bundle, err := schema.CreateEngineFromSchema(s, dataRoot, dataDir, dictManager, m.logger, resolver, opts...)
	if err != nil {
		return fmt.Errorf("创建方案 %q 引擎失败: %w", schemaID, err)
	}

	// 仅在注册阶段短暂持写锁
	m.mu.Lock()
	defer m.mu.Unlock()

	switch eng := bundle.Engine.(type) {
	case *pinyin.Engine:
		m.engines[schemaID] = eng
	case *codetable.Engine:
		m.engines[schemaID] = eng
	case *mixed.Engine:
		m.engines[schemaID] = eng
		if encoderSpec := m.resolveEncoder(s); encoderSpec != nil && len(encoderSpec.Rules) > 0 {
			schemaRules := make([]encoding.SchemaEncoderRule, len(encoderSpec.Rules))
			for i, sr := range encoderSpec.Rules {
				schemaRules[i] = encoding.SchemaEncoderRule{LengthEqual: sr.LengthEqual, LengthInRange: sr.LengthInRange, Formula: sr.Formula}
			}
			eng.SetEncoderRules(encoding.ConvertSchemaRules(schemaRules))
		}
		if s.Engine.Mixed != nil && s.Engine.Mixed.EnableEnglish != nil && *s.Engine.Mixed.EnableEnglish {
			if err := m.ensureEnglishLoadedLocked(); err == nil {
				eng.SetEnglishSearch(m.SearchEnglish)
			}
		}
	default:
		return fmt.Errorf("未知引擎类型: %T", bundle.Engine)
	}

	// 系统词库层直接由工厂返回，避免依赖 dm.compositeDict 当前状态
	// （并发切换可能在工厂返回与此处之间替换了 codetable-system / pinyin-system 层）。
	if bundle.SystemLayer != nil {
		m.systemLayers[schemaID] = bundle.SystemLayer
	}

	return nil
}

// reRegisterSystemLayer 为缓存引擎重新注册系统词库层到 CompositeDict
func (m *Manager) reRegisterSystemLayer(schemaID string) {
	if m.dictManager == nil {
		return
	}
	// 从缓存的 systemLayers 中取出该方案的系统词库层并重新注册
	if layer, ok := m.systemLayers[schemaID]; ok && layer != nil {
		m.dictManager.RegisterSystemLayer(layer.Name(), layer)
		m.logger.Debug("重新注册系统词库层", "layer", layer.Name(), "schemaID", schemaID)
	}
}

// --- 查询方法 ---

// GetCurrentEngine 获取当前引擎
func (m *Manager) GetCurrentEngine() Engine {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentEngine
}

// GetCurrentType 获取当前引擎类型（通过 SchemaManager 读取真实的 engine.type）
func (m *Manager) GetCurrentType() EngineType {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.schemaManager != nil {
		if s := m.schemaManager.GetSchema(m.currentID); s != nil {
			return s.Engine.Type
		}
	}
	return EngineType(m.currentID) // fallback
}

// GetCurrentSchemaID 获取当前方案 ID
func (m *Manager) GetCurrentSchemaID() string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.currentID
}

// GetEngineDisplayName 获取引擎显示名称（从 Schema 读取）
func (m *Manager) GetEngineDisplayName() string {
	m.mu.RLock()
	sm := m.schemaManager
	id := m.currentID
	m.mu.RUnlock()

	if sm != nil {
		s := sm.GetSchema(id)
		if s != nil {
			return s.Schema.IconLabel
		}
	}
	return "?"
}

// GetSchemaNameByID 按 ID 获取方案显示名称
func (m *Manager) GetSchemaNameByID(id string) string {
	m.mu.RLock()
	sm := m.schemaManager
	m.mu.RUnlock()

	if sm != nil {
		s := sm.GetSchema(id)
		if s != nil {
			return s.Schema.Name
		}
	}
	return id
}

// SwitchToSchemaByID 切换到指定方案（含 DictManager 同步和 SchemaManager 更新）
func (m *Manager) SwitchToSchemaByID(schemaID string) error {
	m.mu.RLock()
	sm := m.schemaManager
	currentID := m.currentID
	m.mu.RUnlock()

	if sm == nil {
		return fmt.Errorf("SchemaManager 未设置")
	}
	if schemaID == currentID {
		return nil
	}

	if err := m.SwitchSchema(schemaID); err != nil {
		return err
	}

	// 同步 DictManager
	m.mu.RLock()
	dm := m.dictManager
	m.mu.RUnlock()
	if dm != nil {
		s := sm.GetSchema(schemaID)
		if s != nil {
			dm.SwitchSchemaFull(schemaID, s.DataSchemaID(),
				s.Learning.TempMaxEntries, s.Learning.TempPromoteCount,
				s.Schema.ID)
		}
	}

	// 更新 SchemaManager 的活跃方案
	sm.SetActive(schemaID)

	return nil
}

// GetSchemaManager 返回底层的 SchemaManager（用于查询方案元信息）
func (m *Manager) GetSchemaManager() *schema.SchemaManager {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.schemaManager
}

// GetSchemaDisplayInfo 获取方案显示信息（名称 + 图标）
func (m *Manager) GetSchemaDisplayInfo() (name, iconLabel string) {
	m.mu.RLock()
	sm := m.schemaManager
	id := m.currentID
	m.mu.RUnlock()

	if sm != nil {
		s := sm.GetSchema(id)
		if s != nil {
			return s.Schema.Name, s.Schema.IconLabel
		}
	}
	return id, "?"
}

// IsCurrentEngineType 检查当前方案的引擎类型
func (m *Manager) IsCurrentEngineType(engineType schema.EngineType) bool {
	m.mu.RLock()
	sm := m.schemaManager
	id := m.currentID
	m.mu.RUnlock()

	if sm != nil {
		s := sm.GetSchema(id)
		if s != nil {
			return s.Engine.Type == engineType
		}
	}
	return false
}

// GetChaiziSpec 返回当前活跃方案的拆字数据库路径、字体文件路径（均为绝对路径）和 DirectWrite 字体族名称。
// 方案未配置拆字或文件不存在时返回空字符串。
func (m *Manager) GetChaiziSpec() (dbPath, fontPath, fontDWName string) {
	m.mu.RLock()
	sm := m.schemaManager
	id := m.currentID
	dataRoot := m.dataRoot
	m.mu.RUnlock()

	if sm == nil {
		return "", "", ""
	}
	if id == "" {
		id = sm.GetActiveID()
	}
	s := sm.GetSchema(id)
	if s == nil {
		return "", "", ""
	}
	// 混输方案自身不配置拆字，继承主码表方案的拆字配置
	if s.Engine.Chaizi == nil && s.Engine.Type == schema.EngineTypeMixed && s.Engine.Mixed != nil {
		s = sm.GetSchema(s.Engine.Mixed.PrimarySchema)
		if s == nil {
			return "", "", ""
		}
	}
	if s.Engine.Chaizi == nil {
		return "", "", ""
	}
	dataDir := sm.GetDataDir()
	dbPath = schema.ResolveDictPath(dataRoot, dataDir, s.Engine.Chaizi.DBPath)
	fontPath = schema.ResolveDictPath(dataRoot, dataDir, s.Engine.Chaizi.FontFamily)
	fontDWName = s.Engine.Chaizi.FontDWName
	return dbPath, fontPath, fontDWName
}
