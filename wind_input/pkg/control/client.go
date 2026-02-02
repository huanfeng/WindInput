package control

import (
	"bufio"
	"context"
	"fmt"
	"net"
	"time"

	"github.com/Microsoft/go-winio"
)

// Client 控制管道客户端
type Client struct {
	timeout time.Duration
}

// NewClient 创建客户端
func NewClient() *Client {
	return &Client{
		timeout: 5 * time.Second,
	}
}

// SetTimeout 设置超时时间
func (c *Client) SetTimeout(timeout time.Duration) {
	c.timeout = timeout
}

// Send 发送命令并获取响应
func (c *Client) Send(cmd string, args interface{}) (*Response, error) {
	// 格式化请求
	reqLine, err := FormatRequest(cmd, args)
	if err != nil {
		return nil, err
	}

	// 连接管道
	conn, err := c.connect()
	if err != nil {
		return nil, err
	}
	defer conn.Close()

	// 设置超时
	if c.timeout > 0 {
		conn.SetDeadline(time.Now().Add(c.timeout))
	}

	// 发送请求
	if _, err := fmt.Fprintf(conn, "%s\n", reqLine); err != nil {
		return nil, fmt.Errorf("failed to send request: %w", err)
	}

	// 读取响应
	reader := bufio.NewReader(conn)
	respLine, err := reader.ReadString('\n')
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	return ParseResponse(respLine)
}

// connect 连接到管道
func (c *Client) connect() (net.Conn, error) {
	ctx, cancel := context.WithTimeout(context.Background(), c.timeout)
	defer cancel()

	conn, err := winio.DialPipeContext(ctx, PipeName)
	if err != nil {
		return nil, fmt.Errorf("failed to connect to pipe: %w", err)
	}
	return conn, nil
}

// Ping 发送心跳检测
func (c *Client) Ping() error {
	resp, err := c.Send(CmdPing, nil)
	if err != nil {
		return err
	}
	if !resp.IsOk() {
		return fmt.Errorf("ping failed: %s", resp.Message)
	}
	return nil
}

// ReloadConfig 重载配置
func (c *Client) ReloadConfig() error {
	resp, err := c.Send(CmdReloadConfig, nil)
	if err != nil {
		return err
	}
	if !resp.IsOk() {
		return fmt.Errorf("reload config failed: %s", resp.Message)
	}
	return nil
}

// ReloadPhrases 重载短语
func (c *Client) ReloadPhrases() error {
	resp, err := c.Send(CmdReloadPhrases, nil)
	if err != nil {
		return err
	}
	if !resp.IsOk() {
		return fmt.Errorf("reload phrases failed: %s", resp.Message)
	}
	return nil
}

// ReloadShadow 重载 Shadow 规则
func (c *Client) ReloadShadow() error {
	resp, err := c.Send(CmdReloadShadow, nil)
	if err != nil {
		return err
	}
	if !resp.IsOk() {
		return fmt.Errorf("reload shadow failed: %s", resp.Message)
	}
	return nil
}

// ReloadUserDict 重载用户词库
func (c *Client) ReloadUserDict() error {
	resp, err := c.Send(CmdReloadUserDict, nil)
	if err != nil {
		return err
	}
	if !resp.IsOk() {
		return fmt.Errorf("reload user dict failed: %s", resp.Message)
	}
	return nil
}

// ReloadAll 重载所有
func (c *Client) ReloadAll() error {
	resp, err := c.Send(CmdReloadAll, nil)
	if err != nil {
		return err
	}
	if !resp.IsOk() {
		return fmt.Errorf("reload all failed: %s", resp.Message)
	}
	return nil
}

// GetStatus 获取服务状态
func (c *Client) GetStatus() (*ServiceStatus, error) {
	resp, err := c.Send(CmdGetStatus, nil)
	if err != nil {
		return nil, err
	}
	if !resp.IsOk() {
		return nil, fmt.Errorf("get status failed: %s", resp.Message)
	}

	var status ServiceStatus
	if err := resp.GetData(&status); err != nil {
		return nil, fmt.Errorf("failed to parse status: %w", err)
	}

	return &status, nil
}

// IsServiceRunning 检查服务是否运行
func (c *Client) IsServiceRunning() bool {
	err := c.Ping()
	return err == nil
}

// NotifyReload 通知服务重载指定目标
// target: "config", "phrases", "shadow", "userdict", "all"
func (c *Client) NotifyReload(target string) error {
	switch target {
	case "config":
		return c.ReloadConfig()
	case "phrases":
		return c.ReloadPhrases()
	case "shadow":
		return c.ReloadShadow()
	case "userdict":
		return c.ReloadUserDict()
	case "all":
		return c.ReloadAll()
	default:
		return fmt.Errorf("unknown reload target: %s", target)
	}
}
