// Package rpc 提供 JSON-RPC 服务端实现
// 通过独立命名管道为 Wails 设置端提供词库管理、Shadow 规则和系统状态查询
package rpc

import (
	"fmt"
	"log/slog"
	"net"
	"net/rpc"
	"net/rpc/jsonrpc"
	"sync"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// Server JSON-RPC 服务端
type Server struct {
	logger      *slog.Logger
	dictManager *dict.DictManager
	store       *store.Store
	rpcServer   *rpc.Server

	listener    net.Listener
	wg          sync.WaitGroup
	stopCh      chan struct{}
	mu          sync.Mutex
	running     bool
	broadcaster *EventBroadcaster
	eventServer *EventPipeServer

	statusProvider StatusProvider
}

// StatusProvider 系统状态提供者接口
type StatusProvider interface {
	GetSchemaID() string
	GetEngineType() string
	IsChineseMode() bool
}

// NewServer 创建 RPC 服务端
func NewServer(logger *slog.Logger, dm *dict.DictManager, s *store.Store) *Server {
	return &Server{
		logger:      logger,
		dictManager: dm,
		store:       s,
		broadcaster: NewEventBroadcaster(logger),
		rpcServer:   rpc.NewServer(),
		stopCh:      make(chan struct{}),
	}
}

// SetStatusProvider 设置系统状态提供者
func (s *Server) SetStatusProvider(provider StatusProvider) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.statusProvider = provider
}

// Start 启动 RPC 服务
func (s *Server) Start() error {
	s.mu.Lock()
	if s.running {
		s.mu.Unlock()
		return fmt.Errorf("rpc server already running")
	}
	s.mu.Unlock()

	// 注册服务
	dictSvc := &DictService{store: s.store, dm: s.dictManager, logger: s.logger, broadcaster: s.broadcaster}
	shadowSvc := &ShadowService{store: s.store, dm: s.dictManager, logger: s.logger, broadcaster: s.broadcaster}
	systemSvc := &SystemService{dm: s.dictManager, store: s.store, server: s, logger: s.logger}

	if err := s.rpcServer.RegisterName("Dict", dictSvc); err != nil {
		return fmt.Errorf("register Dict service: %w", err)
	}
	if err := s.rpcServer.RegisterName("Shadow", shadowSvc); err != nil {
		return fmt.Errorf("register Shadow service: %w", err)
	}
	if err := s.rpcServer.RegisterName("System", systemSvc); err != nil {
		return fmt.Errorf("register System service: %w", err)
	}

	phraseSvc := &PhraseService{store: s.store, dm: s.dictManager, logger: s.logger, broadcaster: s.broadcaster}
	if err := s.rpcServer.RegisterName("Phrase", phraseSvc); err != nil {
		return fmt.Errorf("register Phrase service: %w", err)
	}

	// 创建命名管道监听器
	pipeConfig := &winio.PipeConfig{
		SecurityDescriptor: "",
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
		// Non-fatal: RPC still works without events
	}

	s.logger.Info("RPC server started", "pipe", rpcapi.RPCPipeName)

	s.wg.Add(1)
	go s.acceptLoop()

	return nil
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
	s.rpcServer.ServeCodec(jsonrpc.NewServerCodec(conn))
}
