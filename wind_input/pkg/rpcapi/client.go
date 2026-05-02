package rpcapi

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"sync/atomic"
	"time"

	"github.com/Microsoft/go-winio"
)

var globalID atomic.Uint64

// Client IPC 客户端
// 每次调用建立新连接（短连接模式，适合设置端低频操作）
type Client struct {
	pipeName string
	timeout  time.Duration
}

// NewClient 创建 IPC 客户端
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

// connect 建立管道连接
func (c *Client) connect() (net.Conn, error) {
	conn, err := winio.DialPipe(c.pipeName, &c.timeout)
	if err != nil {
		return nil, fmt.Errorf("connect to rpc pipe: %w", err)
	}
	return conn, nil
}

// call 执行单次 IPC 调用（连接→发送→接收→关闭）
func (c *Client) call(method string, args, reply any) error {
	conn, err := c.connect()
	if err != nil {
		return err
	}
	defer conn.Close()

	// 序列化参数
	params, err := json.Marshal(args)
	if err != nil {
		return fmt.Errorf("marshal args: %w", err)
	}

	// 发送请求
	req := Request{
		Version: ProtocolVersion,
		ID:      globalID.Add(1),
		Method:  method,
		Params:  params,
	}

	conn.SetWriteDeadline(time.Now().Add(c.timeout))
	if err := WriteMessage(conn, &req); err != nil {
		return fmt.Errorf("send request: %w", err)
	}

	// 接收响应
	conn.SetReadDeadline(time.Now().Add(c.timeout))
	var resp Response
	if err := ReadMessage(conn, &resp); err != nil {
		return fmt.Errorf("read response: %w", err)
	}

	// 检查错误
	if resp.Error != "" {
		return fmt.Errorf("%s", resp.Error)
	}

	// 反序列化结果
	if reply != nil && len(resp.Result) > 0 {
		if err := json.Unmarshal(resp.Result, reply); err != nil {
			return fmt.Errorf("unmarshal result: %w", err)
		}
	}

	return nil
}

// IsAvailable 检查 IPC 服务是否可用
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

// DictGeneratePinyinCode 为词语生成全拼编码（拼音方案专用）
// 返回空串表示无法生成（含未知字符或无拼音引擎）
func (c *Client) DictGeneratePinyinCode(text string) (string, error) {
	var reply DictGeneratePinyinCodeReply
	err := c.call("Dict.GeneratePinyinCode", &DictGeneratePinyinCodeArgs{Text: text}, &reply)
	return reply.Code, err
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
		SchemaID:  schemaID,
		Code:      code,
		Text:      text,
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
// DictClearUserWords 清空指定方案的用户词库
func (c *Client) DictClearUserWords(schemaID string) (int, error) {
	var reply DictClearUserWordsReply
	err := c.call("Dict.ClearUserWords", &DictClearUserWordsArgs{SchemaID: schemaID}, &reply)
	return reply.Count, err
}

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

// SystemReloadConfig 重载配置
func (c *Client) SystemReloadConfig() error {
	return c.call("System.ReloadConfig", &Empty{}, &Empty{})
}

// SystemReloadShadow 重载 Shadow 规则
func (c *Client) SystemReloadShadow() error {
	return c.call("System.ReloadShadow", &Empty{}, &Empty{})
}

// SystemReloadUserDict 重载用户词库
func (c *Client) SystemReloadUserDict() error {
	return c.call("System.ReloadUserDict", &Empty{}, &Empty{})
}

// SystemNotifyReload 通知服务重载指定目标
// target: "config", "phrases", "shadow", "userdict", "all"
func (c *Client) SystemNotifyReload(target string) error {
	return c.call("System.NotifyReload", &NotifyReloadArgs{Target: target}, &Empty{})
}

// SystemResetDB 重置数据库（清除指定方案或全部用户数据）
func (c *Client) SystemResetDB(schemaID string) error {
	var reply SystemResetDBReply
	return c.call("System.ResetDB", &SystemResetDBArgs{
		SchemaID: schemaID,
	}, &reply)
}

// SystemDeleteSchema 彻底删除方案 bucket（清理残留）
func (c *Client) SystemDeleteSchema(schemaID string) error {
	var reply SystemResetDBReply
	return c.call("System.DeleteSchema", &SystemResetDBArgs{
		SchemaID: schemaID,
	}, &reply)
}

// SystemShutdown 请求服务优雅关闭（保存数据后退出）
func (c *Client) SystemShutdown() error {
	var reply SystemShutdownReply
	return c.call("System.Shutdown", &Empty{}, &reply)
}

// SystemPause 请求服务暂停（释放文件锁但保持进程）
func (c *Client) SystemPause() error {
	var reply SystemPauseReply
	return c.call("System.Pause", &Empty{}, &reply)
}

// SystemResume 请求服务恢复
func (c *Client) SystemResume(newDataDir string) error {
	var reply SystemResumeReply
	args := &SystemResumeArgs{NewDataDir: newDataDir}
	return c.call("System.Resume", args, &reply)
}

// SystemDumpPerf 导出按键链路性能样本到 JSONL 文件
func (c *Client) SystemDumpPerf(path string, clear bool) (*SystemDumpPerfReply, error) {
	var reply SystemDumpPerfReply
	err := c.call("System.DumpPerf", &SystemDumpPerfArgs{Path: path, Clear: clear}, &reply)
	return &reply, err
}

// SystemGetPerfStats 获取当前性能采样统计摘要（不落盘）
func (c *Client) SystemGetPerfStats() (*SystemPerfStatsReply, error) {
	var reply SystemPerfStatsReply
	err := c.call("System.GetPerfStats", &Empty{}, &reply)
	return &reply, err
}

// ── Phrase 方法 ──

// PhraseList 获取所有短语
func (c *Client) PhraseList() (*PhraseListReply, error) {
	var reply PhraseListReply
	err := c.call("Phrase.List", &Empty{}, &reply)
	return &reply, err
}

// PhraseAdd 添加短语
func (c *Client) PhraseAdd(args PhraseAddArgs) error {
	return c.call("Phrase.Add", &args, &Empty{})
}

// PhraseUpdate 更新短语
func (c *Client) PhraseUpdate(args PhraseUpdateArgs) error {
	return c.call("Phrase.Update", &args, &Empty{})
}

// PhraseRemove 删除短语
func (c *Client) PhraseRemove(code, text, name string) error {
	return c.call("Phrase.Remove", &PhraseRemoveArgs{Code: code, Text: text, Name: name}, &Empty{})
}

// PhraseResetDefaults 重置短语为默认值
func (c *Client) PhraseResetDefaults() error {
	return c.call("Phrase.ResetDefaults", &Empty{}, &Empty{})
}

// ── Freq 方法 ──

// FreqSearch 搜索词频记录
func (c *Client) FreqSearch(schemaID, prefix string, limit, offset int) (*FreqSearchReply, error) {
	var reply FreqSearchReply
	err := c.call("Dict.GetFreqList", &FreqSearchArgs{
		SchemaID: schemaID, Prefix: prefix, Limit: limit, Offset: offset,
	}, &reply)
	return &reply, err
}

// FreqDelete 删除单条词频记录
func (c *Client) FreqDelete(schemaID, code, text string) error {
	return c.call("Dict.DeleteFreq", &FreqDeleteArgs{
		SchemaID: schemaID, Code: code, Text: text,
	}, &Empty{})
}

// FreqClear 清空指定方案的所有词频数据
func (c *Client) FreqClear(schemaID string) (int, error) {
	var reply FreqClearReply
	err := c.call("Dict.ClearFreq", &FreqClearArgs{SchemaID: schemaID}, &reply)
	return reply.Count, err
}

// ── Schema 扩展 ──

// SystemListSchemas 列出所有方案及其状态
func (c *Client) SystemListSchemas() (*ListSchemasReply, error) {
	var reply ListSchemasReply
	err := c.call("System.ListSchemas", &Empty{}, &reply)
	return &reply, err
}

// ── 导入导出扩展方法 ──

// DictBatchEncode 批量反向编码（词语 → 编码）
func (c *Client) DictBatchEncode(schemaID string, words []string) (*BatchEncodeReply, error) {
	var reply BatchEncodeReply
	err := c.call("Dict.BatchEncode", &BatchEncodeArgs{SchemaID: schemaID, Words: words}, &reply)
	return &reply, err
}

// FreqBatchPut 批量写入词频数据
func (c *Client) FreqBatchPut(schemaID string, entries []FreqPutEntry) (*FreqBatchPutReply, error) {
	var reply FreqBatchPutReply
	err := c.call("Dict.FreqBatchPut", &FreqBatchPutArgs{SchemaID: schemaID, Entries: entries}, &reply)
	return &reply, err
}

// ShadowBatchSet 批量写入 Shadow 规则
func (c *Client) ShadowBatchSet(schemaID string, pins []ShadowPinItem, deletes []ShadowDelItem) (*ShadowBatchSetReply, error) {
	var reply ShadowBatchSetReply
	err := c.call("Shadow.BatchSet", &ShadowBatchSetArgs{SchemaID: schemaID, Pins: pins, Deletes: deletes}, &reply)
	return &reply, err
}

// PhraseBatchAdd 批量添加短语
func (c *Client) PhraseBatchAdd(phrases []PhraseAddArgs) (*PhraseBatchAddReply, error) {
	var reply PhraseBatchAddReply
	err := c.call("Phrase.BatchAdd", &PhraseBatchAddArgs{Phrases: phrases}, &reply)
	return &reply, err
}

// ── Stats 方法 ──

func (c *Client) StatsGetSummary() (*StatsSummaryReply, error) {
	var reply StatsSummaryReply
	err := c.call("Stats.GetSummary", &Empty{}, &reply)
	return &reply, err
}

func (c *Client) StatsGetDaily(from, to string) (*StatsGetDailyReply, error) {
	var reply StatsGetDailyReply
	err := c.call("Stats.GetDaily", &StatsGetDailyArgs{From: from, To: to}, &reply)
	return &reply, err
}

func (c *Client) StatsGetConfig() (*StatsConfigReply, error) {
	var reply StatsConfigReply
	err := c.call("Stats.GetConfig", &Empty{}, &reply)
	return &reply, err
}

func (c *Client) StatsUpdateConfig(args StatsConfigUpdateArgs) error {
	return c.call("Stats.UpdateConfig", &args, &Empty{})
}

func (c *Client) StatsClear() error {
	return c.call("Stats.Clear", &Empty{}, &Empty{})
}

func (c *Client) StatsPrune(days int) (*StatsPruneReply, error) {
	var reply StatsPruneReply
	err := c.call("Stats.Prune", &StatsPruneArgs{Days: days}, &reply)
	return &reply, err
}

// ── Config 方法 ──

// ConfigGetAll 获取完整配置（JSON 序列化）
func (c *Client) ConfigGetAll() (*ConfigGetAllReply, error) {
	var reply ConfigGetAllReply
	err := c.call("Config.GetAll", &Empty{}, &reply)
	return &reply, err
}

// ConfigGet 按 key 列表批量获取配置项
func (c *Client) ConfigGet(keys []string) (*ConfigGetReply, error) {
	var reply ConfigGetReply
	err := c.call("Config.Get", &ConfigGetArgs{Keys: keys}, &reply)
	return &reply, err
}

// ConfigSet 设置配置项（校验+持久化+热更新）
func (c *Client) ConfigSet(items []ConfigSetItem) (*ConfigSetReply, error) {
	var reply ConfigSetReply
	err := c.call("Config.Set", &ConfigSetArgs{Items: items}, &reply)
	return &reply, err
}

// ConfigSetAll 覆盖全量配置（内部 diff 后精准热更新）
func (c *Client) ConfigSetAll(configJSON []byte) (*ConfigSetAllReply, error) {
	var reply ConfigSetAllReply
	err := c.call("Config.SetAll", &ConfigSetAllArgs{Config: configJSON}, &reply)
	return &reply, err
}

// ConfigGetDefaults 获取系统默认配置
func (c *Client) ConfigGetDefaults() (*ConfigGetDefaultsReply, error) {
	var reply ConfigGetDefaultsReply
	err := c.call("Config.GetDefaults", &Empty{}, &reply)
	return &reply, err
}

// ConfigReset 重置指定 key 为默认值
func (c *Client) ConfigReset(keys []string) (*ConfigResetReply, error) {
	var reply ConfigResetReply
	err := c.call("Config.Reset", &ConfigResetArgs{Keys: keys}, &reply)
	return &reply, err
}

// ConfigGetSchemaOverride 获取方案覆盖配置
func (c *Client) ConfigGetSchemaOverride(schemaID string) (*SchemaOverrideReply, error) {
	var reply SchemaOverrideReply
	err := c.call("Config.GetSchemaOverride", &SchemaOverrideArgs{SchemaID: schemaID}, &reply)
	return &reply, err
}

// ConfigSetSchemaOverride 设置方案覆盖配置
func (c *Client) ConfigSetSchemaOverride(schemaID string, data map[string]any) error {
	return c.call("Config.SetSchemaOverride", &SchemaOverrideSetArgs{SchemaID: schemaID, Data: data}, &Empty{})
}

// ConfigDeleteSchemaOverride 只删除 Layer 3 override
func (c *Client) ConfigDeleteSchemaOverride(schemaID string) error {
	return c.call("Config.DeleteSchemaOverride", &SchemaOverrideArgs{SchemaID: schemaID}, &Empty{})
}

// ConfigResetSchemaOverride 删除 Layer 3 override + Layer 2 用户 schema diff 文件（如有内置方案）
func (c *Client) ConfigResetSchemaOverride(schemaID string) error {
	return c.call("Config.ResetSchemaOverride", &SchemaOverrideArgs{SchemaID: schemaID}, &Empty{})
}

// ConfigSetActiveSchema 切换活跃方案（原子修改+热更新）
func (c *Client) ConfigSetActiveSchema(schemaID string) error {
	return c.call("Config.SetActiveSchema", &SetActiveSchemaArgs{SchemaID: schemaID}, &Empty{})
}

// ── 备份/还原/重置方法 ──

func (c *Client) SystemPreviewBackup() (*BackupPreview, error) {
	var reply BackupPreview
	err := c.call("System.PreviewBackup", &Empty{}, &reply)
	return &reply, err
}

func (c *Client) SystemPreviewRestore(zipPath string) (*RestorePreview, error) {
	var reply RestorePreview
	err := c.call("System.PreviewRestore", &SystemRestoreArgs{ZipPath: zipPath}, &reply)
	return &reply, err
}

func (c *Client) SystemBackup(zipPath string) (*SystemBackupReply, error) {
	var reply SystemBackupReply
	err := c.call("System.Backup", &SystemBackupArgs{ZipPath: zipPath}, &reply)
	return &reply, err
}

func (c *Client) SystemRestore(zipPath string) error {
	var reply SystemRestoreReply
	return c.call("System.Restore", &SystemRestoreArgs{ZipPath: zipPath}, &reply)
}

func (c *Client) SystemReset() error {
	var reply SystemResetReply
	return c.call("System.Reset", &Empty{}, &reply)
}

// ── Event 方法 ──

// SubscribeEvents connects to the event pipe and calls handler for each event.
// Blocks until context is cancelled or connection error.
func (c *Client) SubscribeEvents(ctx context.Context, handler func(EventMessage)) error {
	conn, err := winio.DialPipe(RPCEventPipeName, &c.timeout)
	if err != nil {
		return fmt.Errorf("connect to event pipe: %w", err)
	}

	// Close connection when context is done
	go func() {
		<-ctx.Done()
		conn.Close()
	}()

	for {
		var msg EventMessage
		if err := ReadMessage(conn, &msg); err != nil {
			select {
			case <-ctx.Done():
				return ctx.Err()
			default:
				return fmt.Errorf("read event: %w", err)
			}
		}
		handler(msg)
	}
}
