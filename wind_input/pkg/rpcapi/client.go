package rpcapi

import (
	"fmt"
	"net/rpc"
	"net/rpc/jsonrpc"
	"time"

	"github.com/Microsoft/go-winio"
)

// Client JSON-RPC 客户端
// 每次调用建立新连接（短连接模式，适合设置端低频操作）
type Client struct {
	pipeName string
	timeout  time.Duration
}

// NewClient 创建 RPC 客户端
func NewClient() *Client {
	return &Client{
		pipeName: RPCPipeName,
		timeout:  5 * time.Second,
	}
}

// NewClientWithPipe 使用指定管道名创建客户端（测试用）
func NewClientWithPipe(pipeName string) *Client {
	return &Client{
		pipeName: pipeName,
		timeout:  5 * time.Second,
	}
}

// connect 建立连接并返回 RPC 客户端
func (c *Client) connect() (*rpc.Client, error) {
	conn, err := winio.DialPipe(c.pipeName, &c.timeout)
	if err != nil {
		return nil, fmt.Errorf("connect to rpc pipe: %w", err)
	}
	return jsonrpc.NewClient(conn), nil
}

// call 执行单次 RPC 调用（连接→调用→关闭）
func (c *Client) call(method string, args, reply interface{}) error {
	client, err := c.connect()
	if err != nil {
		return err
	}
	defer client.Close()
	return client.Call(method, args, reply)
}

// IsAvailable 检查 RPC 服务是否可用
func (c *Client) IsAvailable() bool {
	err := c.call("System.Ping", &Empty{}, &Empty{})
	return err == nil
}

// ── Dict 方法 ──

// DictSearch 前缀搜索用户词库
func (c *Client) DictSearch(schemaID, prefix string, limit, offset int) (*DictSearchReply, error) {
	var reply DictSearchReply
	err := c.call("Dict.Search", &DictSearchArgs{
		SchemaID: schemaID,
		Prefix:   prefix,
		Limit:    limit,
		Offset:   offset,
	}, &reply)
	return &reply, err
}

// DictSearchByCode 精确编码查询
func (c *Client) DictSearchByCode(schemaID, code string) (*DictSearchReply, error) {
	var reply DictSearchReply
	err := c.call("Dict.SearchByCode", &DictSearchArgs{
		SchemaID: schemaID,
		Prefix:   code,
	}, &reply)
	return &reply, err
}

// DictAdd 添加用户词条
func (c *Client) DictAdd(schemaID, code, text string, weight int) error {
	return c.call("Dict.Add", &DictAddArgs{
		SchemaID: schemaID,
		Code:     code,
		Text:     text,
		Weight:   weight,
	}, &Empty{})
}

// DictRemove 删除用户词条
func (c *Client) DictRemove(schemaID, code, text string) error {
	return c.call("Dict.Remove", &DictRemoveArgs{
		SchemaID: schemaID,
		Code:     code,
		Text:     text,
	}, &Empty{})
}

// DictUpdate 更新词条权重
func (c *Client) DictUpdate(schemaID, code, text string, newWeight int) error {
	return c.call("Dict.Update", &DictUpdateArgs{
		SchemaID: schemaID,
		Code:     code,
		Text:     text,
		NewWeight: newWeight,
	}, &Empty{})
}

// DictBatchAdd 批量添加词条
func (c *Client) DictBatchAdd(schemaID string, words []WordEntry) (int, error) {
	var reply DictBatchAddReply
	err := c.call("Dict.BatchAdd", &DictBatchAddArgs{
		SchemaID: schemaID,
		Words:    words,
	}, &reply)
	return reply.Count, err
}

// DictGetStats 获取词库统计
func (c *Client) DictGetStats() (map[string]int, error) {
	var reply DictStatsReply
	err := c.call("Dict.GetStats", &Empty{}, &reply)
	return reply.Stats, err
}

// DictGetSchemaStats 获取方案统计
func (c *Client) DictGetSchemaStats(schemaID string) (*DictSchemaStatsReply, error) {
	var reply DictSchemaStatsReply
	err := c.call("Dict.GetSchemaStats", &DictSchemaStatsArgs{
		SchemaID: schemaID,
	}, &reply)
	return &reply, err
}

// ── 临时词库方法 ──

// DictGetTemp 查询临时词库
func (c *Client) DictGetTemp(schemaID, prefix string, limit, offset int) (*DictSearchReply, error) {
	var reply DictSearchReply
	err := c.call("Dict.GetTemp", &DictGetTempArgs{
		SchemaID: schemaID,
		Prefix:   prefix,
		Limit:    limit,
		Offset:   offset,
	}, &reply)
	return &reply, err
}

// DictRemoveTemp 删除临时词条
func (c *Client) DictRemoveTemp(schemaID, code, text string) error {
	return c.call("Dict.RemoveTemp", &DictRemoveTempArgs{
		SchemaID: schemaID,
		Code:     code,
		Text:     text,
	}, &Empty{})
}

// DictClearTemp 清空临时词库
func (c *Client) DictClearTemp(schemaID string) (int, error) {
	var reply DictClearTempReply
	err := c.call("Dict.ClearTemp", &DictClearTempArgs{
		SchemaID: schemaID,
	}, &reply)
	return reply.Count, err
}

// DictPromoteTemp 晋升单个临时词条
func (c *Client) DictPromoteTemp(schemaID, code, text string) error {
	return c.call("Dict.PromoteTemp", &DictPromoteTempArgs{
		SchemaID: schemaID,
		Code:     code,
		Text:     text,
	}, &Empty{})
}

// DictPromoteAllTemp 晋升所有临时词条
func (c *Client) DictPromoteAllTemp(schemaID string) (int, error) {
	var reply DictPromoteAllTempReply
	err := c.call("Dict.PromoteAllTemp", &DictPromoteAllTempArgs{
		SchemaID: schemaID,
	}, &reply)
	return reply.Count, err
}

// ── Shadow 方法 ──

// ShadowPin 固定词到指定位置
func (c *Client) ShadowPin(schemaID, code, word string, position int) error {
	return c.call("Shadow.Pin", &ShadowPinArgs{
		SchemaID: schemaID,
		Code:     code,
		Word:     word,
		Position: position,
	}, &Empty{})
}

// ShadowDelete 隐藏词条
func (c *Client) ShadowDelete(schemaID, code, word string) error {
	return c.call("Shadow.Delete", &ShadowDeleteArgs{
		SchemaID: schemaID,
		Code:     code,
		Word:     word,
	}, &Empty{})
}

// ShadowRemoveRule 移除所有规则
func (c *Client) ShadowRemoveRule(schemaID, code, word string) error {
	return c.call("Shadow.RemoveRule", &ShadowDeleteArgs{
		SchemaID: schemaID,
		Code:     code,
		Word:     word,
	}, &Empty{})
}

// ShadowGetRules 获取指定编码的规则
func (c *Client) ShadowGetRules(schemaID, code string) (*ShadowRulesReply, error) {
	var reply ShadowRulesReply
	err := c.call("Shadow.GetRules", &ShadowGetRulesArgs{
		SchemaID: schemaID,
		Code:     code,
	}, &reply)
	return &reply, err
}

// ShadowGetAllRules 获取所有规则
func (c *Client) ShadowGetAllRules(schemaID string) (*ShadowGetAllRulesReply, error) {
	var reply ShadowGetAllRulesReply
	err := c.call("Shadow.GetAllRules", &ShadowGetAllRulesArgs{
		SchemaID: schemaID,
	}, &reply)
	return &reply, err
}

// ── System 方法 ──

// SystemPing 心跳
func (c *Client) SystemPing() error {
	return c.call("System.Ping", &Empty{}, &Empty{})
}

// SystemGetStatus 获取状态
func (c *Client) SystemGetStatus() (*SystemStatusReply, error) {
	var reply SystemStatusReply
	err := c.call("System.GetStatus", &Empty{}, &reply)
	return &reply, err
}

// SystemReloadPhrases 重载短语
func (c *Client) SystemReloadPhrases() error {
	return c.call("System.ReloadPhrases", &Empty{}, &Empty{})
}

// SystemReloadAll 重载所有
func (c *Client) SystemReloadAll() error {
	return c.call("System.ReloadAll", &Empty{}, &Empty{})
}
