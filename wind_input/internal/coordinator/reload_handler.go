package coordinator

import (
	"log/slog"
	"sync"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/engine"
	"github.com/huanfeng/wind_input/internal/schema"
	"github.com/huanfeng/wind_input/pkg/config"
)

// ReloadHandler 实现 rpc.ConfigReloader 接口，负责配置热重载。
// 协调 schema/engine/dict 等子系统的配置变更。
//
// 锁契约（cfgMu，由 rpc.Server 持有并跨组件共享）：
//   - ReloadConfig：方法内部获取 cfgMu 写锁；调用方不得已持有该锁
//   - ApplyConfigUpdate：约定调用方已持有 cfgMu 写锁；方法内部不再获取
//     （rpc.ConfigService.Set/SetAll 与 rpc.StatsService.UpdateConfig 即按此约定）
type ReloadHandler struct {
	coord     *Coordinator
	cfg       *config.Config
	cfgMu     *sync.RWMutex
	schemaMgr *schema.SchemaManager
	engineMgr *engine.Manager
	dictMgr   *dict.DictManager
	logger    *slog.Logger
}

// NewReloadHandler 创建配置重载处理器
func NewReloadHandler(coord *Coordinator, cfg *config.Config, cfgMu *sync.RWMutex, schemaMgr *schema.SchemaManager, engineMgr *engine.Manager, dictMgr *dict.DictManager, logger *slog.Logger) *ReloadHandler {
	return &ReloadHandler{
		coord:     coord,
		cfg:       cfg,
		cfgMu:     cfgMu,
		schemaMgr: schemaMgr,
		engineMgr: engineMgr,
		dictMgr:   dictMgr,
		logger:    logger,
	}
}

// ReloadConfig 重载配置（处理 config.yaml 变更和 schema 文件变更）
func (h *ReloadHandler) ReloadConfig() error {
	newCfg, err := config.Load()
	if err != nil {
		return err
	}

	h.cfgMu.Lock()
	defer h.cfgMu.Unlock()

	oldCfg := *h.cfg
	allSections := map[string]bool{
		"startup": true, "schema": true, "hotkeys": true, "ui": true,
		"toolbar": true, "input": true, "advanced": true, "stats": true,
		"s2t": true,
	}
	_, err = h.ApplyConfigUpdate(&oldCfg, newCfg, allSections)
	if err == nil {
		h.logger.Info("Config reloaded successfully",
			"schema", newCfg.Schema.Active,
			"toggleModeKeys", newCfg.Hotkeys.ToggleModeKeys)
	}
	return err
}

// ApplyConfigUpdate 增量应用配置变更，返回是否需要重启生效
func (h *ReloadHandler) ApplyConfigUpdate(oldCfg, newCfg *config.Config, changedSections map[string]bool) (bool, error) {
	// schema.active 变更：切换方案
	if changedSections["schema"] && newCfg.Schema.Active != oldCfg.Schema.Active {
		h.logger.Info("Schema changed via config update", "from", oldCfg.Schema.Active, "to", newCfg.Schema.Active)
		if err := h.engineMgr.SwitchSchema(newCfg.Schema.Active); err != nil {
			h.logger.Error("Failed to switch schema", "error", err)
		} else {
			h.schemaMgr.SetActive(newCfg.Schema.Active)
			s := h.schemaMgr.GetSchema(newCfg.Schema.Active)
			if s != nil && h.dictMgr != nil {
				h.dictMgr.SwitchSchemaFull(newCfg.Schema.Active, s.DataSchemaID(),
					s.Learning.TempMaxEntries, s.Learning.TempPromoteCount)
			}
		}
	}

	// 主码表/主拼音变更
	if changedSections["schema"] {
		if newCfg.Schema.PrimaryCodetable != oldCfg.Schema.PrimaryCodetable ||
			newCfg.Schema.PrimaryPinyin != oldCfg.Schema.PrimaryPinyin {
			h.engineMgr.SetPrimarySchemas(newCfg.Schema.PrimaryCodetable, newCfg.Schema.PrimaryPinyin)
		}
		// 重新加载 schema 文件，应用引擎选项热更新
		h.reloadActiveSchemaConfig()
	}

	// 按 section 精准热更新
	if h.coord != nil {
		if changedSections["hotkeys"] {
			h.coord.UpdateHotkeyConfig(&newCfg.Hotkeys)
		}
		if changedSections["startup"] {
			h.coord.UpdateStartupConfig(&newCfg.Startup)
		}
		if changedSections["ui"] {
			h.coord.UpdateUIConfig(&newCfg.UI)
		}
		if changedSections["toolbar"] {
			h.coord.UpdateToolbarConfig(&newCfg.Toolbar)
		}
		if changedSections["input"] {
			h.coord.UpdateInputConfig(&newCfg.Input)
			if newCfg.Input.FilterMode != "" {
				h.engineMgr.UpdateFilterMode(newCfg.Input.FilterMode)
			}
		}
		if changedSections["stats"] {
			h.coord.UpdateStatsConfig(&newCfg.Stats)
		}
		if changedSections["s2t"] {
			h.coord.UpdateS2TConfig(&newCfg.S2T)
		}
	}

	// 替换活配置
	*h.cfg = *newCfg

	// advanced 变更需重启
	return changedSections["advanced"], nil
}

// reloadActiveSchemaConfig 从 schema 文件重新加载引擎选项并热更新
func (h *ReloadHandler) reloadActiveSchemaConfig() {
	if h.schemaMgr == nil {
		return
	}

	// 重新加载 schema 文件
	if err := h.schemaMgr.LoadSchemas(); err != nil {
		h.logger.Error("Failed to reload schemas", "error", err)
		return
	}

	activeID := h.schemaMgr.GetActiveID()
	s := h.schemaMgr.GetSchema(activeID)
	if s == nil {
		return
	}

	// 根据引擎类型应用配置
	switch s.Engine.Type {
	case schema.EngineTypeCodeTable:
		if spec := s.Engine.CodeTable; spec != nil {
			h.engineMgr.UpdateCodetableOptions(spec)
		}

	case schema.EngineTypePinyin:
		if spec := s.Engine.Pinyin; spec != nil {
			h.applyPinyinSpec(spec, false) // 纯拼音模式：简拼始终开启
		}

	case schema.EngineTypeMixed:
		// 混输方案：拼音配置可能在自身的 Engine.Pinyin 或引用的次方案中
		pinyinSpec := s.Engine.Pinyin
		if pinyinSpec == nil && s.Engine.Mixed != nil && s.Engine.Mixed.SecondarySchema != "" {
			if secSchema := h.schemaMgr.GetSchema(s.Engine.Mixed.SecondarySchema); secSchema != nil {
				pinyinSpec = secSchema.Engine.Pinyin
			}
		}
		// enable_abbrev_match 在 MixedSpec 中，默认关闭（skipAbbrev=true）
		skipAbbrev := true
		if s.Engine.Mixed != nil && s.Engine.Mixed.EnableAbbrevMatch != nil && *s.Engine.Mixed.EnableAbbrevMatch {
			skipAbbrev = false
		}
		if pinyinSpec != nil {
			h.applyPinyinSpec(pinyinSpec, skipAbbrev)
		} else {
			h.applyPinyinSpec(&schema.PinyinSpec{}, skipAbbrev)
		}
		// 码表子引擎配置
		if s.Engine.Mixed != nil && s.Engine.Mixed.PrimarySchema != "" {
			if priSchema := h.schemaMgr.GetSchema(s.Engine.Mixed.PrimarySchema); priSchema != nil {
				if spec := priSchema.Engine.CodeTable; spec != nil {
					h.engineMgr.UpdateCodetableOptions(spec)
				}
			}
		}
	}

	// 学习配置热更新（调频 + 造词）
	h.engineMgr.UpdateLearningConfig(&s.Learning)

	h.logger.Debug("Schema config reloaded", "schema", activeID, "engineType", s.Engine.Type)
}

// applyPinyinSpec 将 PinyinSpec 转换为 PinyinConfig 并更新引擎。
// skipAbbrev：混输模式专用，true 表示关闭简拼匹配；纯拼音模式传 false。
func (h *ReloadHandler) applyPinyinSpec(spec *schema.PinyinSpec, skipAbbrev bool) {
	pinyinCfg := &config.PinyinConfig{
		ShowCodeHint:    spec.ShowCodeHint,
		UseSmartCompose: spec.UseSmartCompose,
		CandidateOrder:  spec.CandidateOrder,
		SkipAbbrev:      skipAbbrev,
	}
	if spec.Fuzzy != nil {
		pinyinCfg.Fuzzy = config.FuzzyPinyinConfig{
			Enabled: spec.Fuzzy.Enabled,
			ZhZ:     spec.Fuzzy.ZhZ,
			ChC:     spec.Fuzzy.ChC,
			ShS:     spec.Fuzzy.ShS,
			NL:      spec.Fuzzy.NL,
			FH:      spec.Fuzzy.FH,
			RL:      spec.Fuzzy.RL,
			AnAng:   spec.Fuzzy.AnAng,
			EnEng:   spec.Fuzzy.EnEng,
			InIng:   spec.Fuzzy.InIng,
			IanIang: spec.Fuzzy.IanIang,
			UanUang: spec.Fuzzy.UanUang,
		}
	}
	h.engineMgr.UpdatePinyinOptions(pinyinCfg)
}
