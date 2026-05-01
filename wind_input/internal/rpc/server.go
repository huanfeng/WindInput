// Package rpc 提供轻量 IPC 服务端实现
// 通过独立命名管道为 Wails 设置端提供词库管理、Shadow 规则和系统状态查询
// 使用 length-prefix 帧协议替代 net/rpc，避免引入 net/http 等重量级依赖
package rpc

import (
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net"
	"sync"
	"time"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

const connReadTimeout = 30 * time.Second

// Server IPC 服务端
type Server struct {
	logger      *slog.Logger
	dictManager *dict.DictManager
	store       *store.Store
	router      *Router

	listener    net.Listener
	wg          sync.WaitGroup
	stopCh      chan struct{}
	mu          sync.Mutex
	running     bool
	broadcaster *EventBroadcaster
	eventServer *EventPipeServer

	statusProvider StatusProvider
	configReloader ConfigReloader
	batchEncoder   BatchEncoder

	paused bool // 服务暂停状态

	statCollector *store.StatCollector

	cfg       *config.Config         // 活配置指针，与 ReloadHandler 共享
	cfgMu     sync.RWMutex           // 守护 *cfg 的读写，被 ReloadHandler / ConfigService / SystemService / StatsService 共享
	schemaMgr SchemaOverrideResetter // 用于 ResetSchemaOverride 的 Layer 2 文件操作
}

// CfgMu 返回守护活配置的读写锁，供 ReloadHandler 等共享同一指针的组件使用
func (s *Server) CfgMu() *sync.RWMutex {
	return &s.cfgMu
}

// Broadcaster 返回事件广播器，供需要旁路 RPC 路径直接广播事件的组件使用
// （如 coordinator 在热键切换方案、快捷加词后通知设置端订阅者）
func (s *Server) Broadcaster() *EventBroadcaster {
	return s.broadcaster
}

// StatusProvider 系统状态提供者接口
type StatusProvider interface {
	GetSchemaID() string
	GetEngineType() string
	IsChineseMode() bool
	IsFullWidth() bool
	IsChinesePunct() bool
}

// ConfigReloader 配置重载接口（由 coordinator.ReloadHandler 实现）
type ConfigReloader interface {
	ReloadConfig() error
	// ApplyConfigUpdate 增量应用配置变更，返回是否需要重启生效
	ApplyConfigUpdate(oldCfg, newCfg *config.Config, changedSections map[string]bool) (requiresRestart bool, err error)
}

// SchemaOverrideResetter 用于 Config.ResetSchemaOverride 的 Layer 2 文件操作
type SchemaOverrideResetter interface {
	GetBuiltinSchemaPath(schemaID string) (string, bool)
	GetUserSchemaPath(schemaID string) (string, bool)
}

// BatchEncoder 批量反向编码接口（由 engine.Manager 通过适配器实现）
type BatchEncoder interface {
	// BatchEncode 将词语列表批量编码为 (word, code) 对
	BatchEncode(words []string) []rpcapi.EncodeResultItem
}

// NewServer 创建 IPC 服务端
func NewServer(logger *slog.Logger, dm *dict.DictManager, s *store.Store) *Server {
	return &Server{
		logger:      logger,
		dictManager: dm,
		store:       s,
		broadcaster: NewEventBroadcaster(logger),
		router:      NewRouter(),
		stopCh:      make(chan struct{}),
	}
}

// SetStatusProvider 设置系统状态提供者
func (s *Server) SetStatusProvider(provider StatusProvider) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.statusProvider = provider
}

// SetConfigReloader 设置配置重载处理器
func (s *Server) SetConfigReloader(reloader ConfigReloader) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.configReloader = reloader
}

// SetBatchEncoder 设置批量编码器
func (s *Server) SetBatchEncoder(encoder BatchEncoder) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.batchEncoder = encoder
}

// SetStatCollector 设置统计采集器
func (s *Server) SetStatCollector(sc *store.StatCollector) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.statCollector = sc
}

// SetConfig 设置活配置指针（与 ReloadHandler 共享同一块内存）
func (s *Server) SetConfig(cfg *config.Config) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cfg = cfg
}

// SetSchemaOverrideResetter 设置 schema 文件路径查询接口
func (s *Server) SetSchemaOverrideResetter(r SchemaOverrideResetter) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.schemaMgr = r
}

// Start 启动 IPC 服务
func (s *Server) Start() error {
	s.mu.Lock()
	if s.running {
		s.mu.Unlock()
		return fmt.Errorf("rpc server already running")
	}
	s.mu.Unlock()

	// 创建服务实例
	dictSvc := &DictService{store: s.store, dm: s.dictManager, logger: s.logger, broadcaster: s.broadcaster, batchEncoder: s.batchEncoder}
	shadowSvc := &ShadowService{store: s.store, dm: s.dictManager, logger: s.logger, broadcaster: s.broadcaster}
	systemSvc := &SystemService{dm: s.dictManager, store: s.store, server: s, logger: s.logger, configReloader: s.configReloader}
	phraseSvc := &PhraseService{store: s.store, dm: s.dictManager, logger: s.logger, broadcaster: s.broadcaster}

	// 注册 Dict 方法
	RegisterMethod(s.router, "Dict.Search", dictSvc.Search)
	RegisterMethod(s.router, "Dict.SearchByCode", dictSvc.SearchByCode)
	RegisterMethod(s.router, "Dict.Add", dictSvc.Add)
	RegisterMethod(s.router, "Dict.Remove", dictSvc.Remove)
	RegisterMethod(s.router, "Dict.Update", dictSvc.Update)
	RegisterMethod(s.router, "Dict.GetStats", dictSvc.GetStats)
	RegisterMethod(s.router, "Dict.GetSchemaStats", dictSvc.GetSchemaStats)
	RegisterMethod(s.router, "Dict.BatchAdd", dictSvc.BatchAdd)
	RegisterMethod(s.router, "Dict.GetTemp", dictSvc.GetTemp)
	RegisterMethod(s.router, "Dict.RemoveTemp", dictSvc.RemoveTemp)
	RegisterMethod(s.router, "Dict.ClearUserWords", dictSvc.ClearUserWords)
	RegisterMethod(s.router, "Dict.ClearTemp", dictSvc.ClearTemp)
	RegisterMethod(s.router, "Dict.PromoteTemp", dictSvc.PromoteTemp)
	RegisterMethod(s.router, "Dict.PromoteAllTemp", dictSvc.PromoteAllTemp)
	RegisterMethod(s.router, "Dict.GetFreqList", dictSvc.GetFreqList)
	RegisterMethod(s.router, "Dict.DeleteFreq", dictSvc.DeleteFreq)
	RegisterMethod(s.router, "Dict.ClearFreq", dictSvc.ClearFreq)
	RegisterMethod(s.router, "Dict.BatchEncode", dictSvc.BatchEncode)
	RegisterMethod(s.router, "Dict.FreqBatchPut", dictSvc.FreqBatchPut)

	// 注册 Shadow 方法
	RegisterMethod(s.router, "Shadow.Pin", shadowSvc.Pin)
	RegisterMethod(s.router, "Shadow.Delete", shadowSvc.Delete)
	RegisterMethod(s.router, "Shadow.RemoveRule", shadowSvc.RemoveRule)
	RegisterMethod(s.router, "Shadow.GetRules", shadowSvc.GetRules)
	RegisterMethod(s.router, "Shadow.GetAllRules", shadowSvc.GetAllRules)
	RegisterMethod(s.router, "Shadow.BatchSet", shadowSvc.BatchSet)

	// 注册 System 方法
	RegisterMethod(s.router, "System.Ping", systemSvc.Ping)
	RegisterMethod(s.router, "System.GetStatus", systemSvc.GetStatus)
	RegisterMethod(s.router, "System.ReloadPhrases", systemSvc.ReloadPhrases)
	RegisterMethod(s.router, "System.ReloadAll", systemSvc.ReloadAll)
	RegisterMethod(s.router, "System.ResetDB", systemSvc.ResetDB)
	RegisterMethod(s.router, "System.DeleteSchema", systemSvc.DeleteSchema)
	RegisterMethod(s.router, "System.ListSchemas", systemSvc.ListSchemas)
	RegisterMethod(s.router, "System.ReloadConfig", systemSvc.ReloadConfig)
	RegisterMethod(s.router, "System.ReloadShadow", systemSvc.ReloadShadow)
	RegisterMethod(s.router, "System.ReloadUserDict", systemSvc.ReloadUserDict)
	RegisterMethod(s.router, "System.NotifyReload", systemSvc.NotifyReload)
	RegisterMethod(s.router, "System.Pause", systemSvc.Pause)
	RegisterMethod(s.router, "System.Resume", systemSvc.Resume)
	RegisterMethod(s.router, "System.Shutdown", systemSvc.Shutdown)
	RegisterMethod(s.router, "System.DumpPerf", systemSvc.DumpPerf)
	RegisterMethod(s.router, "System.GetPerfStats", systemSvc.GetPerfStats)

	// 注册 Stats 方法
	statsSvc := &StatsService{store: s.store, logger: s.logger, statCollector: s.statCollector, server: s, broadcaster: s.broadcaster}
	RegisterMethod(s.router, "Stats.GetSummary", statsSvc.GetSummary)
	RegisterMethod(s.router, "Stats.GetDaily", statsSvc.GetDaily)
	RegisterMethod(s.router, "Stats.GetConfig", statsSvc.GetConfig)
	RegisterMethod(s.router, "Stats.UpdateConfig", statsSvc.UpdateConfig)
	RegisterMethod(s.router, "Stats.Clear", statsSvc.Clear)
	RegisterMethod(s.router, "Stats.Prune", statsSvc.Prune)

	// 注册 Phrase 方法
	RegisterMethod(s.router, "Phrase.List", phraseSvc.List)
	RegisterMethod(s.router, "Phrase.Add", phraseSvc.Add)
	RegisterMethod(s.router, "Phrase.Update", phraseSvc.Update)
	RegisterMethod(s.router, "Phrase.Remove", phraseSvc.Remove)
	RegisterMethod(s.router, "Phrase.ResetDefaults", phraseSvc.ResetDefaults)
	RegisterMethod(s.router, "Phrase.BatchAdd", phraseSvc.BatchAdd)

	// 注册 Config 方法
	configSvc := &ConfigService{
		cfg:            s.cfg,
		cfgMu:          &s.cfgMu,
		configReloader: s.configReloader,
		broadcaster:    s.broadcaster,
		schemaMgr:      s.schemaMgr,
		logger:         s.logger,
		saveFn:         config.Save,
	}
	RegisterMethod(s.router, "Config.GetAll", configSvc.GetAll)
	RegisterMethod(s.router, "Config.Get", configSvc.Get)
	RegisterMethod(s.router, "Config.Set", configSvc.Set)
	RegisterMethod(s.router, "Config.SetAll", configSvc.SetAll)
	RegisterMethod(s.router, "Config.GetDefaults", configSvc.GetDefaults)
	RegisterMethod(s.router, "Config.Reset", configSvc.Reset)
	RegisterMethod(s.router, "Config.GetSchemaOverride", configSvc.GetSchemaOverride)
	RegisterMethod(s.router, "Config.SetSchemaOverride", configSvc.SetSchemaOverride)
	RegisterMethod(s.router, "Config.DeleteSchemaOverride", configSvc.DeleteSchemaOverride)
	RegisterMethod(s.router, "Config.ResetSchemaOverride", configSvc.ResetSchemaOverride)
	RegisterMethod(s.router, "Config.SetActiveSchema", configSvc.SetActiveSchema)

	// 创建命名管道监听器
	// SDDL: 允许 SYSTEM(SY)、管理员(BA)和所有已认证用户(AU)完全访问
	// 解决提升权限进程创建的管道默认 DACL 阻止非提升进程连接的问题
	pipeConfig := &winio.PipeConfig{
		SecurityDescriptor: "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)",
		InputBufferSize:    65536,
		OutputBufferSize:   65536,
	}
	listener, err := winio.ListenPipe(rpcapi.RPCPipeName, pipeConfig)
	if err != nil {
		return fmt.Errorf("listen rpc pipe: %w", err)
	}
	s.listener = listener

	s.mu.Lock()
	s.running = true
	s.mu.Unlock()

	// 启动事件推送管道
	s.eventServer = NewEventPipeServer(s.broadcaster, s.logger)
	if err := s.eventServer.Start(); err != nil {
		s.logger.Error("Failed to start event pipe", "error", err)
	}

	s.logger.Info("RPC server started", "pipe", rpcapi.RPCPipeName)

	s.wg.Add(1)
	go s.acceptLoop()

	s.wg.Add(1)
	go s.statsThrottler()

	return nil
}

// statsThrottler 每 5 秒在有订阅者时广播一次统计心跳，驱动设置端自动刷新。
// 仅在有活跃订阅者（设置页面已打开）时才发送，否则直接跳过，不影响输入性能。
func (s *Server) statsThrottler() {
	defer s.wg.Done()
	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-ticker.C:
			if s.broadcaster.HasSubscribers() {
				s.broadcaster.Broadcast(rpcapi.EventMessage{
					Type:   rpcapi.EventTypeStats,
					Action: rpcapi.EventActionUpdated,
				})
			}
		case <-s.stopCh:
			return
		}
	}
}

// StartAsync 异步启动
func (s *Server) StartAsync() {
	go func() {
		if err := s.Start(); err != nil {
			s.logger.Error("Failed to start RPC server", "error", err)
		}
	}()
}

// Stop 停止服务
func (s *Server) Stop() {
	s.mu.Lock()
	if !s.running {
		s.mu.Unlock()
		return
	}
	s.running = false
	s.mu.Unlock()

	close(s.stopCh)
	if s.eventServer != nil {
		s.eventServer.Stop()
	}
	if s.listener != nil {
		s.listener.Close()
	}
	s.wg.Wait()
	s.logger.Info("RPC server stopped")
}

func (s *Server) acceptLoop() {
	defer s.wg.Done()
	for {
		conn, err := s.listener.Accept()
		if err != nil {
			select {
			case <-s.stopCh:
				return
			default:
				s.logger.Error("RPC accept error", "error", err)
				continue
			}
		}
		s.wg.Add(1)
		go s.handleConn(conn)
	}
}

func (s *Server) handleConn(conn net.Conn) {
	defer s.wg.Done()
	defer conn.Close()

	for {
		// 设置读超时：如果客户端长时间不发请求，释放连接
		conn.SetReadDeadline(time.Now().Add(connReadTimeout))

		var req rpcapi.Request
		if err := rpcapi.ReadMessage(conn, &req); err != nil {
			if err != io.EOF {
				// 超时或其他读取错误，静默关闭
				select {
				case <-s.stopCh:
				default:
					if !isTimeoutError(err) {
						s.logger.Debug("RPC read error", "error", err)
					}
				}
			}
			return
		}

		// 清除写超时
		conn.SetWriteDeadline(time.Now().Add(10 * time.Second))

		// 校验协议版本
		if req.Version != rpcapi.ProtocolVersion {
			resp := rpcapi.Response{
				ID:    req.ID,
				Error: fmt.Sprintf("protocol version mismatch: client=%d, server=%d", req.Version, rpcapi.ProtocolVersion),
			}
			rpcapi.WriteMessage(conn, &resp)
			return
		}

		// 暂停状态下仅允许系统管理方法
		s.mu.Lock()
		isPaused := s.paused
		s.mu.Unlock()
		if isPaused && !isSystemMethod(req.Method) {
			resp := rpcapi.Response{
				ID:    req.ID,
				Error: "服务已暂停，请等待操作完成",
			}
			rpcapi.WriteMessage(conn, &resp)
			continue
		}

		result, err := s.router.Dispatch(req.Method, req.Params)

		var resp rpcapi.Response
		resp.ID = req.ID
		if err != nil {
			resp.Error = err.Error()
		} else {
			data, marshalErr := json.Marshal(result)
			if marshalErr != nil {
				resp.Error = fmt.Sprintf("marshal result: %v", marshalErr)
			} else {
				resp.Result = data
			}
		}

		if writeErr := rpcapi.WriteMessage(conn, &resp); writeErr != nil {
			s.logger.Debug("RPC write error", "error", writeErr)
			return
		}
	}
}

func isTimeoutError(err error) bool {
	if ne, ok := err.(net.Error); ok {
		return ne.Timeout()
	}
	return false
}

// isSystemMethod 检查是否为系统管理方法（暂停状态下允许调用）
func isSystemMethod(method string) bool {
	switch method {
	case "System.Ping", "System.GetStatus", "System.Resume", "System.Pause", "System.Shutdown",
		"Config.GetAll", "Config.Get", "Config.GetDefaults":
		return true
	}
	return false
}

// SetPaused 设置暂停状态
func (s *Server) SetPaused(paused bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.paused = paused
}

// IsPaused 返回暂停状态
func (s *Server) IsPaused() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.paused
}
