package rpc

import (
	"fmt"
	"log/slog"
	"path/filepath"
	"strings"

	"github.com/huanfeng/wind_input/internal/coordinator"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
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

// Shutdown 请求服务优雅关闭
func (s *SystemService) Shutdown(args *rpcapi.Empty, reply *rpcapi.SystemShutdownReply) error {
	s.logger.Info("RPC System.Shutdown: graceful shutdown requested")
	reply.OK = true
	go coordinator.RequestExit()
	return nil
}

// Pause 暂停服务（关闭数据库释放文件锁，但保留进程和 RPC 通道）
func (s *SystemService) Pause(args *rpcapi.Empty, reply *rpcapi.SystemPauseReply) error {
	s.logger.Info("RPC System.Pause: pausing service")

	// 关闭数据库
	if s.store != nil {
		if err := s.store.Pause(); err != nil {
			return fmt.Errorf("pause store: %w", err)
		}
	}

	// 设置服务暂停状态（拒绝非系统请求）
	s.server.SetPaused(true)

	reply.OK = true
	s.logger.Info("RPC System.Pause: service paused")
	return nil
}

// Resume 恢复服务（重新打开数据库）
func (s *SystemService) Resume(args *rpcapi.SystemResumeArgs, reply *rpcapi.SystemResumeReply) error {
	s.logger.Info("RPC System.Resume: resuming service", "newDataDir", args.NewDataDir)

	// 如果指定了新数据目录，需要更新数据库路径
	newDBPath := ""
	if args.NewDataDir != "" {
		newDBPath = filepath.Join(args.NewDataDir, "user_data.db")
	}

	// 重新打开数据库
	if s.store != nil {
		if err := s.store.Resume(newDBPath); err != nil {
			return fmt.Errorf("resume store: %w", err)
		}
	}

	// 清除暂停状态
	s.server.SetPaused(false)

	reply.OK = true
	s.logger.Info("RPC System.Resume: service resumed")
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

	// 获取配置中启用的方案（从内存中持有的活配置读取，wind_input 是 config.yaml 的唯一 owner）
	if s.server == nil || s.server.cfg == nil {
		return fmt.Errorf("config not available")
	}
	s.server.cfgMu.RLock()
	available := append([]string(nil), s.server.cfg.Schema.Available...)
	s.server.cfgMu.RUnlock()

	enabledSet := make(map[string]bool, len(available))
	for _, id := range available {
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
	for _, id := range available {
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
