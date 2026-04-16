package rpc

import (
	"fmt"
	"log/slog"
	"strings"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// SystemService 系统管理 RPC 服务
type SystemService struct {
	dm             *dict.DictManager
	store          *store.Store
	server         *Server
	logger         *slog.Logger
	configReloader ConfigReloader
}

// Ping 心跳检测
func (s *SystemService) Ping(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	return nil
}

// GetStatus 获取系统状态
func (s *SystemService) GetStatus(args *rpcapi.Empty, reply *rpcapi.SystemStatusReply) error {
	reply.Running = true
	reply.StoreEnabled = true
	reply.SchemaID = s.dm.GetActiveSchemaID()

	stats := s.dm.GetStats()
	reply.UserWords = stats["user_words"]
	reply.TempWords = stats["temp_words"]
	reply.Phrases = stats["phrases"]
	reply.ShadowRules = stats["shadow_rules"]

	s.server.mu.Lock()
	provider := s.server.statusProvider
	s.server.mu.Unlock()

	if provider != nil {
		reply.EngineType = provider.GetEngineType()
		reply.ChineseMode = provider.IsChineseMode()
		reply.FullWidth = provider.IsFullWidth()
		reply.ChinesePunct = provider.IsChinesePunct()
	}

	return nil
}

// ReloadPhrases 重载短语
func (s *SystemService) ReloadPhrases(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	s.logger.Info("RPC System.ReloadPhrases")
	return s.dm.ReloadPhrases()
}

// ReloadAll 重载所有（配置、短语、Shadow、用户词库）
func (s *SystemService) ReloadAll(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	s.logger.Info("RPC System.ReloadAll")
	var errors []string

	if s.configReloader != nil {
		if err := s.configReloader.ReloadConfig(); err != nil {
			errors = append(errors, fmt.Sprintf("config: %v", err))
		}
	}
	if s.dm != nil {
		if err := s.dm.ReloadPhrases(); err != nil {
			errors = append(errors, fmt.Sprintf("phrases: %v", err))
		}
	}

	if len(errors) > 0 {
		return fmt.Errorf("%s", strings.Join(errors, "; "))
	}
	return nil
}

// ReloadConfig 重载配置文件（触发方案切换、引擎选项更新等）
func (s *SystemService) ReloadConfig(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	s.logger.Info("RPC System.ReloadConfig")
	if s.configReloader == nil {
		return fmt.Errorf("config reloader not available")
	}
	return s.configReloader.ReloadConfig()
}

// ReloadShadow 重载 Shadow 规则
func (s *SystemService) ReloadShadow(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	s.logger.Info("RPC System.ReloadShadow")
	// Store 后端实时读取，无需手动重载
	return nil
}

// ReloadUserDict 重载用户词库
func (s *SystemService) ReloadUserDict(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	s.logger.Info("RPC System.ReloadUserDict")
	if s.dm == nil {
		return fmt.Errorf("dict manager not available")
	}
	// Store 后端实时读取，无需手动重载
	return nil
}

// NotifyReload 通知重载指定目标（统一入口）
func (s *SystemService) NotifyReload(args *rpcapi.NotifyReloadArgs, reply *rpcapi.Empty) error {
	switch args.Target {
	case "config", "schema":
		return s.ReloadConfig(&rpcapi.Empty{}, reply)
	case "phrases":
		return s.ReloadPhrases(&rpcapi.Empty{}, reply)
	case "shadow":
		return s.ReloadShadow(&rpcapi.Empty{}, reply)
	case "userdict":
		return s.ReloadUserDict(&rpcapi.Empty{}, reply)
	case "all":
		return s.ReloadAll(&rpcapi.Empty{}, reply)
	default:
		return fmt.Errorf("unknown reload target: %s", args.Target)
	}
}

// ResetDB 重置数据库（清除用户词库、临时词库、Shadow 规则、词频数据）
func (s *SystemService) ResetDB(args *rpcapi.SystemResetDBArgs, reply *rpcapi.SystemResetDBReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}

	if args.SchemaID != "" {
		s.logger.Info("RPC System.ResetDB: clearing schema", "schemaID", args.SchemaID)
		if err := s.store.ClearSchema(args.SchemaID); err != nil {
			return fmt.Errorf("clear schema: %w", err)
		}
	} else {
		s.logger.Info("RPC System.ResetDB: clearing all schemas")
		if err := s.store.ClearAllSchemas(); err != nil {
			return fmt.Errorf("clear all schemas: %w", err)
		}
	}

	reply.Success = true
	return nil
}

// DeleteSchema 彻底删除方案的 bucket（用于清理残留方案）
func (s *SystemService) DeleteSchema(args *rpcapi.SystemResetDBArgs, reply *rpcapi.SystemResetDBReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.SchemaID == "" {
		return fmt.Errorf("schema_id is required")
	}

	s.logger.Info("RPC System.DeleteSchema", "schemaID", args.SchemaID)
	if err := s.store.DeleteSchema(args.SchemaID); err != nil {
		return fmt.Errorf("delete schema: %w", err)
	}

	reply.Success = true
	return nil
}

// ListSchemas 列出所有方案及其状态
func (s *SystemService) ListSchemas(args *rpcapi.Empty, reply *rpcapi.ListSchemasReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}

	// 获取 bbolt 中已有数据的方案
	storeIDs, err := s.store.ListSchemaIDs()
	if err != nil {
		return fmt.Errorf("list schema IDs: %w", err)
	}

	// 获取配置中启用的方案
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}
	enabledSet := make(map[string]bool, len(cfg.Schema.Available))
	for _, id := range cfg.Schema.Available {
		enabledSet[id] = true
	}

	// 已处理的方案集合
	processed := make(map[string]bool)

	// 处理 store 中的方案
	for _, id := range storeIDs {
		status := "orphaned"
		if enabledSet[id] {
			status = "enabled"
		}

		entry := rpcapi.SchemaStatus{
			SchemaID: id,
			Status:   status,
		}
		entry.UserWords, _ = s.store.UserWordCount(id)
		entry.TempWords, _ = s.store.TempWordCount(id)
		entry.ShadowRules, _ = s.store.ShadowRuleCount(id)

		// 词频记录数
		freqEntries, _ := s.store.SearchFreqPrefix(id, "", 0)
		entry.FreqRecords = len(freqEntries)

		// 跳过数据全为空的 orphaned 方案（已被清除的残留 bucket）
		if status == "orphaned" && entry.UserWords == 0 && entry.TempWords == 0 && entry.ShadowRules == 0 && entry.FreqRecords == 0 {
			processed[id] = true
			continue
		}

		reply.Schemas = append(reply.Schemas, entry)
		processed[id] = true
	}

	// 添加配置中启用但 store 中没有数据的方案
	for _, id := range cfg.Schema.Available {
		if processed[id] {
			continue
		}
		reply.Schemas = append(reply.Schemas, rpcapi.SchemaStatus{
			SchemaID: id,
			Status:   "enabled",
		})
	}

	s.logger.Info("RPC System.ListSchemas", "count", len(reply.Schemas))
	return nil
}
