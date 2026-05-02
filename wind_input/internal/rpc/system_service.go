package rpc

import (
	"archive/zip"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/huanfeng/wind_input/internal/backup"
	"github.com/huanfeng/wind_input/internal/coordinator"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/perf"
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
	s.server.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeSystem, Action: rpcapi.EventActionPaused})
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
	s.server.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeSystem, Action: rpcapi.EventActionResumed})
	return nil
}

// DumpPerf 主动导出按键链路性能样本到文件。
// Path 留空时写到日志目录下的 perf_<timestamp>.jsonl。
func (s *SystemService) DumpPerf(args *rpcapi.SystemDumpPerfArgs, reply *rpcapi.SystemDumpPerfReply) error {
	path := args.Path
	if path == "" {
		dir, err := config.GetLogsDir()
		if err != nil || dir == "" {
			return fmt.Errorf("logs dir unavailable: %v", err)
		}
		path = filepath.Join(dir, fmt.Sprintf("perf_%s.jsonl", time.Now().Format("20060102_150405")))
	}
	count, err := perf.ExportJSONL(path)
	if err != nil {
		return fmt.Errorf("export perf jsonl: %w", err)
	}
	if args.Clear {
		perf.Clear()
	}
	reply.Path = path
	reply.Count = count
	reply.Summary = perf.FormatStats(perf.ComputeStats())
	s.logger.Info("RPC System.DumpPerf", "path", path, "count", count, "cleared", args.Clear)
	return nil
}

// GetPerfStats 返回当前内存性能样本的统计摘要（不落盘）。
func (s *SystemService) GetPerfStats(args *rpcapi.Empty, reply *rpcapi.SystemPerfStatsReply) error {
	stats := perf.ComputeStats()
	reply.Count = stats.Count
	reply.Capacity = perf.Capacity()
	reply.Summary = perf.FormatStats(stats)
	return nil
}

// PreviewBackup 返回当前数据统计（只读，无需 Pause）
func (s *SystemService) PreviewBackup(args *rpcapi.Empty, reply *rpcapi.BackupPreview) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaIDs, err := s.store.ListSchemaIDs()
	if err != nil {
		return err
	}
	for _, id := range schemaIDs {
		uw, err := s.store.AllUserWords(id)
		if err != nil {
			s.logger.Debug("PreviewBackup: AllUserWords", "schema", id, "err", err)
		}
		tw, err := s.store.AllTempWords(id)
		if err != nil {
			s.logger.Debug("PreviewBackup: AllTempWords", "schema", id, "err", err)
		}
		freq, err := s.store.AllFreq(id)
		if err != nil {
			s.logger.Debug("PreviewBackup: AllFreq", "schema", id, "err", err)
		}
		phrases, err := s.store.AllSchemaPhrases(id)
		if err != nil {
			s.logger.Debug("PreviewBackup: AllSchemaPhrases", "schema", id, "err", err)
		}
		reply.Schemas = append(reply.Schemas, rpcapi.SchemaBackupStats{
			SchemaID:      id,
			UserWordCount: len(uw),
			TempWordCount: len(tw),
			FreqCount:     len(freq),
			PhraseCount:   len(phrases),
		})
	}
	gp, err := s.store.AllGlobalPhrases()
	if err != nil {
		s.logger.Debug("PreviewBackup: AllGlobalPhrases", "err", err)
	}
	reply.GlobalPhrases = len(gp)
	stats, err := s.store.AllStats()
	if err != nil {
		s.logger.Debug("PreviewBackup: AllStats", "err", err)
	}
	reply.StatsDays = len(stats)
	dataDir := filepath.Dir(s.store.Path())
	reply.ThemeCount = backup.CountThemes(filepath.Join(dataDir, "themes"))
	var total int64
	for _, sc := range reply.Schemas {
		total += int64(sc.UserWordCount*100 + sc.TempWordCount*80 + sc.FreqCount*30)
	}
	reply.EstimatedSize = total + int64(reply.StatsDays*500) + 10*1024
	return nil
}

// PreviewRestore 读取 ZIP manifest 和统计（只读，无需 Pause）
func (s *SystemService) PreviewRestore(args *rpcapi.SystemRestoreArgs, reply *rpcapi.RestorePreview) error {
	m, err := backup.ReadManifestFromZip(args.ZipPath)
	if err != nil {
		return fmt.Errorf("invalid backup file: %w", err)
	}
	reply.CreatedAt = m.CreatedAt
	reply.AppVersion = m.AppVersion
	reply.DataDirMode = m.DataDirMode

	r, err := zip.OpenReader(args.ZipPath)
	if err != nil {
		return fmt.Errorf("open zip: %w", err)
	}
	defer r.Close()

	for _, f := range r.File {
		reply.TotalSize += int64(f.UncompressedSize64)
	}
	for _, id := range backup.ExtractSchemaIDsFromZip(&r.Reader) {
		reply.Schemas = append(reply.Schemas, rpcapi.SchemaBackupStats{SchemaID: id})
	}
	themePrefix := "files/themes/"
	for _, f := range r.File {
		if strings.HasPrefix(f.Name, themePrefix) && !strings.HasSuffix(f.Name, "/") {
			reply.ThemeCount++
		}
	}
	for _, f := range r.File {
		if f.Name == "db/stats.yaml" {
			reply.StatsDays = int(f.UncompressedSize64 / 80)
			break
		}
	}
	return nil
}

// Backup 将所有用户数据备份到 ZIP 文件
func (s *SystemService) Backup(args *rpcapi.SystemBackupArgs, reply *rpcapi.SystemBackupReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	dbPath := s.store.Path()
	dataDir := filepath.Dir(dbPath)

	if err := s.store.Pause(); err != nil {
		return fmt.Errorf("pause store: %w", err)
	}
	defer func() {
		if err := s.store.Resume(""); err != nil {
			s.logger.Error("backup: resume failed", "err", err)
		}
	}()

	tmpPath := args.ZipPath + ".tmp"
	f, err := os.Create(tmpPath)
	if err != nil {
		return fmt.Errorf("create zip: %w", err)
	}

	zw := zip.NewWriter(f)
	m := &backup.Manifest{
		Version:     "1.0",
		AppVersion:  "",
		CreatedAt:   time.Now().Format(time.RFC3339),
		DataDirMode: "standard",
	}

	writeErr := func() error {
		if err := backup.WriteManifestToZip(zw, m); err != nil {
			return fmt.Errorf("write manifest: %w", err)
		}
		if err := backup.CopyDirToZip(zw, dataDir, "files/", []string{"user_data.db"}); err != nil {
			return fmt.Errorf("copy files: %w", err)
		}
		// Pause 后重新打开 DB 读取（db 已关闭，可重新打开）
		tmpStore, err := store.Open(dbPath)
		if err != nil {
			return fmt.Errorf("open db for read: %w", err)
		}
		exportErr := backup.ExportDBToZip(zw, tmpStore)
		closeErr := tmpStore.Close()
		if exportErr == nil {
			exportErr = closeErr
		}
		if exportErr != nil {
			return fmt.Errorf("export db: %w", exportErr)
		}
		return nil
	}()

	zw.Close()
	if err := f.Close(); err != nil && writeErr == nil {
		writeErr = err
	}

	if writeErr != nil {
		os.Remove(tmpPath)
		return writeErr
	}
	if err := os.Rename(tmpPath, args.ZipPath); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("finalize zip: %w", err)
	}
	reply.Path = args.ZipPath
	s.logger.Info("backup completed", "path", args.ZipPath)
	return nil
}

// Restore 从 ZIP 文件还原所有用户数据，完成后触发全量重载
func (s *SystemService) Restore(args *rpcapi.SystemRestoreArgs, reply *rpcapi.SystemRestoreReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	// 校验 ZIP（Pause 前拦截）
	if _, err := backup.ReadManifestFromZip(args.ZipPath); err != nil {
		return fmt.Errorf("invalid backup file: %w", err)
	}
	dataDir := filepath.Dir(s.store.Path())

	if err := s.store.Pause(); err != nil {
		return fmt.Errorf("pause: %w", err)
	}

	r, err := zip.OpenReader(args.ZipPath)
	if err != nil {
		_ = s.store.Resume("")
		return fmt.Errorf("open zip: %w", err)
	}
	defer r.Close()

	tmpDir, err := os.MkdirTemp(filepath.Dir(dataDir), "wind_restore_*")
	if err != nil {
		_ = s.store.Resume("")
		return fmt.Errorf("create temp dir: %w", err)
	}

	restoreOK := false
	defer func() {
		os.RemoveAll(tmpDir)
		if !restoreOK {
			_ = s.store.Resume("")
		}
	}()

	if err := backup.ExtractZipPrefix(&r.Reader, "files/", tmpDir); err != nil {
		return fmt.Errorf("extract files: %w", err)
	}

	newDBPath := filepath.Join(tmpDir, "user_data.db")
	os.Remove(newDBPath) // 确保从空 DB 开始，忽略不存在错误
	newStore, err := store.Open(newDBPath)
	if err != nil {
		return fmt.Errorf("create restore db: %w", err)
	}
	if err := backup.ImportDBFromZip(&r.Reader, newStore); err != nil {
		newStore.Close()
		return fmt.Errorf("import db: %w", err)
	}
	newStore.Close()

	if err := backup.AtomicReplaceDir(tmpDir, dataDir); err != nil {
		return fmt.Errorf("replace data dir: %w", err)
	}
	restoreOK = true

	if err := s.store.Resume(""); err != nil {
		restoreOK = false // 让 defer 再试一次
		return fmt.Errorf("resume after restore: %w", err)
	}

	// 全量重载
	if s.configReloader != nil {
		if err := s.configReloader.ReloadConfig(); err != nil {
			s.logger.Warn("restore: reload config failed", "err", err)
		}
	}
	if s.dm != nil {
		if err := s.dm.ReloadPhrases(); err != nil {
			s.logger.Warn("restore: reload phrases failed", "err", err)
		}
	}

	reply.OK = true
	s.logger.Info("restore completed", "zip", args.ZipPath)
	return nil
}

// Reset 清除所有用户数据，恢复出厂设置，完成后触发全量重载
func (s *SystemService) Reset(args *rpcapi.Empty, reply *rpcapi.SystemResetReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	dataDir := filepath.Dir(s.store.Path())

	if err := s.store.Pause(); err != nil {
		return fmt.Errorf("pause: %w", err)
	}
	resumeDeferred := false
	defer func() {
		if !resumeDeferred {
			if err := s.store.Resume(""); err != nil {
				s.logger.Error("reset: deferred resume failed", "err", err)
			}
		}
	}()

	entries, _ := os.ReadDir(dataDir)
	var lastErr error
	for _, e := range entries {
		if err := os.RemoveAll(filepath.Join(dataDir, e.Name())); err != nil {
			s.logger.Error("reset: remove failed", "name", e.Name(), "err", err)
			lastErr = err
		}
	}

	resumeDeferred = true
	if err := s.store.Resume(""); err != nil {
		s.logger.Error("reset: resume failed, will retry in defer", "err", err)
		resumeDeferred = false // 让 defer 重试
		return fmt.Errorf("resume after reset: %w", err)
	}

	if s.configReloader != nil {
		_ = s.configReloader.ReloadConfig()
	}
	if s.dm != nil {
		_ = s.dm.ReloadPhrases()
	}

	if lastErr != nil {
		return fmt.Errorf("some files could not be deleted: %w", lastErr)
	}
	reply.OK = true
	s.logger.Info("reset completed")
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
