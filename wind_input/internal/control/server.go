// Package control 提供控制管道服务端实现
package control

import (
	"bufio"
	"fmt"
	"log/slog"
	"net"
	"strings"
	"sync"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/pkg/config"
	"github.com/huanfeng/wind_input/pkg/control"
)

// ReloadHandler 重载处理器接口
type ReloadHandler interface {
	// ReloadConfig 重载配置文件
	ReloadConfig() error
	// GetStatus 获取服务状态
	GetStatus() *control.ServiceStatus
}

// Server 控制管道服务端
type Server struct {
	logger        *slog.Logger
	dictManager   *dict.DictManager
	reloadHandler ReloadHandler

	listener net.Listener
	wg       sync.WaitGroup
	stopCh   chan struct{}
	running  bool
	mu       sync.Mutex
}

// NewServer 创建控制管道服务端
func NewServer(logger *slog.Logger, dictManager *dict.DictManager) *Server {
	return &Server{
		logger:      logger,
		dictManager: dictManager,
		stopCh:      make(chan struct{}),
	}
}

// SetReloadHandler 设置重载处理器
func (s *Server) SetReloadHandler(handler ReloadHandler) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.reloadHandler = handler
}

// Start 启动服务
func (s *Server) Start() error {
	s.mu.Lock()
	if s.running {
		s.mu.Unlock()
		return fmt.Errorf("server already running")
	}
	s.running = true
	s.mu.Unlock()

	// 创建 Windows Named Pipe 监听器
	pipeConfig := &winio.PipeConfig{
		SecurityDescriptor: "", // 默认安全描述符，允许当前用户访问
		MessageMode:        false,
		InputBufferSize:    4096,
		OutputBufferSize:   4096,
	}
	listener, err := winio.ListenPipe(control.PipeName, pipeConfig)
	if err != nil {
		s.mu.Lock()
		s.running = false
		s.mu.Unlock()
		return fmt.Errorf("failed to create pipe listener: %w", err)
	}
	s.listener = listener

	s.logger.Info("Control pipe server started", "pipe", control.PipeName)

	// 启动接受连接的 goroutine
	s.wg.Add(1)
	go s.acceptLoop()

	return nil
}

// StartAsync 异步启动服务
func (s *Server) StartAsync() {
	go func() {
		if err := s.Start(); err != nil {
			s.logger.Error("Failed to start control pipe server", "error", err)
		}
	}()
}

// Stop 停止服务
func (s *Server) Stop() error {
	s.mu.Lock()
	if !s.running {
		s.mu.Unlock()
		return nil
	}
	s.running = false
	s.mu.Unlock()

	// 关闭停止通道
	close(s.stopCh)

	// 关闭监听器
	if s.listener != nil {
		s.listener.Close()
	}

	// 等待所有连接处理完成
	s.wg.Wait()

	s.logger.Info("Control pipe server stopped")
	return nil
}

// acceptLoop 接受连接循环
func (s *Server) acceptLoop() {
	defer s.wg.Done()

	for {
		conn, err := s.listener.Accept()
		if err != nil {
			select {
			case <-s.stopCh:
				return
			default:
				s.logger.Error("Failed to accept connection", "error", err)
				continue
			}
		}

		// 处理连接
		s.wg.Add(1)
		go s.handleConnection(conn)
	}
}

// handleConnection 处理单个连接
func (s *Server) handleConnection(conn net.Conn) {
	defer s.wg.Done()
	defer conn.Close()

	reader := bufio.NewReader(conn)

	// 读取请求
	line, err := reader.ReadString('\n')
	if err != nil {
		s.logger.Debug("Failed to read request", "error", err)
		return
	}

	// 解析请求
	req, err := control.ParseRequest(line)
	if err != nil {
		s.logger.Debug("Failed to parse request", "error", err)
		s.sendResponse(conn, control.ErrorResponse(err))
		return
	}

	s.logger.Debug("Received control command", "command", req.Command)

	// 处理请求
	response := s.handleRequest(req)
	s.sendResponse(conn, response)
}

// sendResponse 发送响应
func (s *Server) sendResponse(conn net.Conn, response string) {
	fmt.Fprintf(conn, "%s\n", response)
}

// handleRequest 处理请求
func (s *Server) handleRequest(req *control.Request) string {
	switch req.Command {
	case control.CmdPing:
		return control.OkResponse()

	case control.CmdReloadConfig:
		return s.handleReloadConfig()

	case control.CmdReloadPhrases:
		return s.handleReloadPhrases()

	case control.CmdReloadShadow:
		return s.handleReloadShadow()

	case control.CmdReloadUserDict:
		return s.handleReloadUserDict()

	case control.CmdReloadAll:
		return s.handleReloadAll()

	case control.CmdGetStatus:
		return s.handleGetStatus()

	default:
		return control.ErrorResponse(fmt.Errorf("unknown command: %s", req.Command))
	}
}

// handleReloadConfig 处理重载配置
func (s *Server) handleReloadConfig() string {
	s.mu.Lock()
	handler := s.reloadHandler
	s.mu.Unlock()

	if handler != nil {
		if err := handler.ReloadConfig(); err != nil {
			s.logger.Error("Failed to reload config", "error", err)
			return control.ErrorResponse(err)
		}
	} else {
		// 如果没有处理器，直接重新加载配置文件
		_, err := config.Load()
		if err != nil {
			s.logger.Error("Failed to reload config", "error", err)
			return control.ErrorResponse(err)
		}
	}

	s.logger.Info("Config reloaded via control pipe")
	return control.OkResponse()
}

// handleReloadPhrases 处理重载短语
func (s *Server) handleReloadPhrases() string {
	if s.dictManager == nil {
		return control.ErrorResponse(fmt.Errorf("dict manager not initialized"))
	}

	if err := s.dictManager.ReloadPhrases(); err != nil {
		s.logger.Error("Failed to reload phrases", "error", err)
		return control.ErrorResponse(err)
	}

	s.logger.Info("Phrases reloaded via control pipe")
	return control.OkResponse()
}

// handleReloadShadow 处理重载 Shadow 规则
func (s *Server) handleReloadShadow() string {
	if s.dictManager == nil {
		return control.ErrorResponse(fmt.Errorf("dict manager not initialized"))
	}

	if err := s.dictManager.ReloadShadow(); err != nil {
		s.logger.Error("Failed to reload shadow", "error", err)
		return control.ErrorResponse(err)
	}

	s.logger.Info("Shadow rules reloaded via control pipe")
	return control.OkResponse()
}

// handleReloadUserDict 处理重载用户词库
func (s *Server) handleReloadUserDict() string {
	if s.dictManager == nil {
		return control.ErrorResponse(fmt.Errorf("dict manager not initialized"))
	}

	userDict := s.dictManager.GetUserDict()
	if userDict == nil {
		return control.ErrorResponse(fmt.Errorf("user dict not initialized"))
	}

	if err := userDict.Load(); err != nil {
		s.logger.Error("Failed to reload user dict", "error", err)
		return control.ErrorResponse(err)
	}

	s.logger.Info("User dict reloaded via control pipe")
	return control.OkResponse()
}

// handleReloadAll 处理重载所有
func (s *Server) handleReloadAll() string {
	var errors []string

	// 重载配置
	s.mu.Lock()
	handler := s.reloadHandler
	s.mu.Unlock()

	if handler != nil {
		if err := handler.ReloadConfig(); err != nil {
			errors = append(errors, fmt.Sprintf("config: %v", err))
		}
	}

	// 重载词库
	if s.dictManager != nil {
		if err := s.dictManager.ReloadPhrases(); err != nil {
			errors = append(errors, fmt.Sprintf("phrases: %v", err))
		}
		if err := s.dictManager.ReloadShadow(); err != nil {
			errors = append(errors, fmt.Sprintf("shadow: %v", err))
		}
		userDict := s.dictManager.GetUserDict()
		if userDict != nil {
			if err := userDict.Load(); err != nil {
				errors = append(errors, fmt.Sprintf("userdict: %v", err))
			}
		}
	}

	if len(errors) > 0 {
		errMsg := strings.Join(errors, "; ")
		s.logger.Error("Partial reload failures", "errors", errMsg)
		return control.ErrorResponse(fmt.Errorf(errMsg))
	}

	s.logger.Info("All configs and dicts reloaded via control pipe")
	return control.OkResponse()
}

// handleGetStatus 处理获取状态
func (s *Server) handleGetStatus() string {
	status := &control.ServiceStatus{
		Running: true,
	}

	// 从处理器获取状态
	s.mu.Lock()
	handler := s.reloadHandler
	s.mu.Unlock()

	if handler != nil {
		handlerStatus := handler.GetStatus()
		if handlerStatus != nil {
			status = handlerStatus
		}
	}

	// 补充词库统计
	if s.dictManager != nil {
		stats := s.dictManager.GetStats()
		status.UserDictCount = stats["user_words"]
		status.PhraseCount = stats["phrases"]
		status.ShadowCount = stats["shadow_rules"]
	}

	resp, err := control.DataResponse(status)
	if err != nil {
		return control.ErrorResponse(err)
	}
	return resp
}
