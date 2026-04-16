package rpc

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net"
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// testClient 测试用轻量客户端，通过 net.Pipe 直连服务端
type testClient struct {
	conn net.Conn
}

func (c *testClient) call(method string, args, reply any) error {
	params, err := json.Marshal(args)
	if err != nil {
		return err
	}
	req := rpcapi.Request{Version: rpcapi.ProtocolVersion, ID: 1, Method: method, Params: params}
	if err := rpcapi.WriteMessage(c.conn, &req); err != nil {
		return err
	}
	var resp rpcapi.Response
	if err := rpcapi.ReadMessage(c.conn, &resp); err != nil {
		return err
	}
	if resp.Error != "" {
		return fmt.Errorf("%s", resp.Error)
	}
	if reply != nil && len(resp.Result) > 0 {
		return json.Unmarshal(resp.Result, reply)
	}
	return nil
}

func (c *testClient) Close() { c.conn.Close() }

// setupTestRPC 创建测试用服务端和客户端（注册所有方法）
func setupTestRPC(t *testing.T) *testClient {
	t.Helper()
	dir := t.TempDir()

	s, err := store.Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })

	dm := dict.NewDictManager(dir, dir, nil)
	if err := dm.Initialize(); err != nil {
		t.Fatal(err)
	}
	dm.SwitchSchema("test", "", "")

	logger := slog.Default()
	router := NewRouter()
	broadcaster := NewEventBroadcaster(logger)

	dictSvc := &DictService{store: s, dm: dm, logger: logger, broadcaster: broadcaster}
	shadowSvc := &ShadowService{store: s, dm: dm, logger: logger, broadcaster: broadcaster}
	systemSvc := &SystemService{dm: dm, store: s, server: &Server{}, logger: logger}
	phraseSvc := &PhraseService{store: s, dm: dm, logger: logger, broadcaster: broadcaster}

	// Dict
	RegisterMethod(router, "Dict.Search", dictSvc.Search)
	RegisterMethod(router, "Dict.SearchByCode", dictSvc.SearchByCode)
	RegisterMethod(router, "Dict.Add", dictSvc.Add)
	RegisterMethod(router, "Dict.Remove", dictSvc.Remove)
	RegisterMethod(router, "Dict.Update", dictSvc.Update)
	RegisterMethod(router, "Dict.GetStats", dictSvc.GetStats)
	RegisterMethod(router, "Dict.GetSchemaStats", dictSvc.GetSchemaStats)
	RegisterMethod(router, "Dict.BatchAdd", dictSvc.BatchAdd)
	RegisterMethod(router, "Dict.GetTemp", dictSvc.GetTemp)
	RegisterMethod(router, "Dict.RemoveTemp", dictSvc.RemoveTemp)
	RegisterMethod(router, "Dict.ClearTemp", dictSvc.ClearTemp)
	RegisterMethod(router, "Dict.PromoteTemp", dictSvc.PromoteTemp)
	RegisterMethod(router, "Dict.PromoteAllTemp", dictSvc.PromoteAllTemp)
	RegisterMethod(router, "Dict.GetFreqList", dictSvc.GetFreqList)
	RegisterMethod(router, "Dict.DeleteFreq", dictSvc.DeleteFreq)
	RegisterMethod(router, "Dict.ClearFreq", dictSvc.ClearFreq)

	// Shadow
	RegisterMethod(router, "Shadow.Pin", shadowSvc.Pin)
	RegisterMethod(router, "Shadow.Delete", shadowSvc.Delete)
	RegisterMethod(router, "Shadow.RemoveRule", shadowSvc.RemoveRule)
	RegisterMethod(router, "Shadow.GetRules", shadowSvc.GetRules)
	RegisterMethod(router, "Shadow.GetAllRules", shadowSvc.GetAllRules)

	// System
	RegisterMethod(router, "System.Ping", systemSvc.Ping)
	RegisterMethod(router, "System.GetStatus", systemSvc.GetStatus)
	RegisterMethod(router, "System.ReloadPhrases", systemSvc.ReloadPhrases)
	RegisterMethod(router, "System.ReloadAll", systemSvc.ReloadAll)
	RegisterMethod(router, "System.ResetDB", systemSvc.ResetDB)
	RegisterMethod(router, "System.DeleteSchema", systemSvc.DeleteSchema)
	RegisterMethod(router, "System.ListSchemas", systemSvc.ListSchemas)

	// Phrase
	RegisterMethod(router, "Phrase.List", phraseSvc.List)
	RegisterMethod(router, "Phrase.Add", phraseSvc.Add)
	RegisterMethod(router, "Phrase.Update", phraseSvc.Update)
	RegisterMethod(router, "Phrase.Remove", phraseSvc.Remove)
	RegisterMethod(router, "Phrase.ResetDefaults", phraseSvc.ResetDefaults)

	serverConn, clientConn := net.Pipe()

	srv := &Server{router: router, stopCh: make(chan struct{})}
	srv.wg.Add(1)
	go srv.handleConn(serverConn)
	t.Cleanup(func() { clientConn.Close() })

	return &testClient{conn: clientConn}
}

// ── Dict 基础操作 ──

func TestDictAddAndSearch(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	err := client.call("Dict.Add", &rpcapi.DictAddArgs{
		Code: "ggtt", Text: "王国", Weight: 1200,
	}, &empty)
	if err != nil {
		t.Fatalf("Dict.Add: %v", err)
	}

	err = client.call("Dict.Add", &rpcapi.DictAddArgs{
		Code: "ggtt", Text: "国王", Weight: 600,
	}, &empty)
	if err != nil {
		t.Fatalf("Dict.Add 2: %v", err)
	}

	var reply rpcapi.DictSearchReply
	err = client.call("Dict.SearchByCode", &rpcapi.DictSearchArgs{
		Prefix: "ggtt",
	}, &reply)
	if err != nil {
		t.Fatalf("Dict.SearchByCode: %v", err)
	}
	if reply.Total != 2 {
		t.Errorf("expected 2 words, got %d", reply.Total)
	}

	err = client.call("Dict.Add", &rpcapi.DictAddArgs{
		Code: "gg", Text: "王", Weight: 800,
	}, &empty)
	if err != nil {
		t.Fatal(err)
	}

	var prefixReply rpcapi.DictSearchReply
	err = client.call("Dict.Search", &rpcapi.DictSearchArgs{
		Prefix: "gg", Limit: 10,
	}, &prefixReply)
	if err != nil {
		t.Fatalf("Dict.Search: %v", err)
	}
	if prefixReply.Total != 3 {
		t.Errorf("expected 3 words with prefix 'gg', got %d", prefixReply.Total)
	}
}

func TestDictRemoveAndUpdate(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	client.call("Dict.Add", &rpcapi.DictAddArgs{Code: "ab", Text: "测试", Weight: 100}, &empty)

	err := client.call("Dict.Update", &rpcapi.DictUpdateArgs{
		Code: "ab", Text: "测试", NewWeight: 500,
	}, &empty)
	if err != nil {
		t.Fatalf("Dict.Update: %v", err)
	}

	var reply rpcapi.DictSearchReply
	client.call("Dict.SearchByCode", &rpcapi.DictSearchArgs{Prefix: "ab"}, &reply)
	if reply.Words[0].Weight != 500 {
		t.Errorf("expected weight=500, got %d", reply.Words[0].Weight)
	}

	err = client.call("Dict.Remove", &rpcapi.DictRemoveArgs{Code: "ab", Text: "测试"}, &empty)
	if err != nil {
		t.Fatalf("Dict.Remove: %v", err)
	}

	var reply2 rpcapi.DictSearchReply
	client.call("Dict.SearchByCode", &rpcapi.DictSearchArgs{Prefix: "ab"}, &reply2)
	if reply2.Total != 0 {
		t.Errorf("expected 0 after remove, got %d", reply2.Total)
	}
}

func TestDictSearchPagination(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	for i, text := range []string{"词一", "词二", "词三", "词四", "词五"} {
		client.call("Dict.Add", &rpcapi.DictAddArgs{
			Code: "ci", Text: text, Weight: 100 + i*10,
		}, &empty)
	}

	var page1 rpcapi.DictSearchReply
	client.call("Dict.Search", &rpcapi.DictSearchArgs{Prefix: "ci", Limit: 2, Offset: 0}, &page1)
	if page1.Total != 5 {
		t.Errorf("expected total=5, got %d", page1.Total)
	}
	if len(page1.Words) != 2 {
		t.Errorf("expected 2 words on page 1, got %d", len(page1.Words))
	}

	var page2 rpcapi.DictSearchReply
	client.call("Dict.Search", &rpcapi.DictSearchArgs{Prefix: "ci", Limit: 2, Offset: 2}, &page2)
	if len(page2.Words) != 2 {
		t.Errorf("expected 2 words on page 2, got %d", len(page2.Words))
	}

	var page3 rpcapi.DictSearchReply
	client.call("Dict.Search", &rpcapi.DictSearchArgs{Prefix: "ci", Limit: 2, Offset: 4}, &page3)
	if len(page3.Words) != 1 {
		t.Errorf("expected 1 word on page 3, got %d", len(page3.Words))
	}
}

func TestDictBatchAdd(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var reply rpcapi.DictBatchAddReply
	err := client.call("Dict.BatchAdd", &rpcapi.DictBatchAddArgs{
		Words: []rpcapi.WordEntry{
			{Code: "aa", Text: "词一", Weight: 100},
			{Code: "ab", Text: "词二", Weight: 200},
			{Code: "ac", Text: "词三", Weight: 300},
		},
	}, &reply)
	if err != nil {
		t.Fatalf("Dict.BatchAdd: %v", err)
	}
	if reply.Count != 3 {
		t.Errorf("expected count=3, got %d", reply.Count)
	}

	var searchReply rpcapi.DictSearchReply
	client.call("Dict.Search", &rpcapi.DictSearchArgs{Prefix: "a", Limit: 10}, &searchReply)
	if searchReply.Total != 3 {
		t.Errorf("expected 3 words, got %d", searchReply.Total)
	}
}

func TestDictGetSchemaStats(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	client.call("Dict.Add", &rpcapi.DictAddArgs{Code: "ab", Text: "测试", Weight: 100}, &empty)
	client.call("Dict.Add", &rpcapi.DictAddArgs{Code: "cd", Text: "示例", Weight: 200}, &empty)

	var reply rpcapi.DictSchemaStatsReply
	err := client.call("Dict.GetSchemaStats", &rpcapi.DictSchemaStatsArgs{}, &reply)
	if err != nil {
		t.Fatalf("Dict.GetSchemaStats: %v", err)
	}
	if reply.WordCount != 2 {
		t.Errorf("expected 2 user words, got %d", reply.WordCount)
	}
}

func TestDictGetStats(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var reply rpcapi.DictStatsReply
	err := client.call("Dict.GetStats", &rpcapi.Empty{}, &reply)
	if err != nil {
		t.Fatalf("Dict.GetStats: %v", err)
	}
	if reply.Stats == nil {
		t.Error("expected non-nil stats")
	}
}

// ── Dict 临时词库 ──

func TestDictTempWords(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	// 查询空临时词库
	var listReply rpcapi.DictSearchReply
	err := client.call("Dict.GetTemp", &rpcapi.DictGetTempArgs{}, &listReply)
	if err != nil {
		t.Fatalf("Dict.GetTemp: %v", err)
	}
	if listReply.Total != 0 {
		t.Errorf("expected 0 temp words, got %d", listReply.Total)
	}
}

func TestDictClearTemp(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var reply rpcapi.DictClearTempReply
	err := client.call("Dict.ClearTemp", &rpcapi.DictClearTempArgs{}, &reply)
	if err != nil {
		t.Fatalf("Dict.ClearTemp: %v", err)
	}
	// 空库清除应返回 0
	if reply.Count != 0 {
		t.Errorf("expected 0, got %d", reply.Count)
	}
}

func TestDictPromoteAllTemp(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var reply rpcapi.DictPromoteAllTempReply
	err := client.call("Dict.PromoteAllTemp", &rpcapi.DictPromoteAllTempArgs{}, &reply)
	if err != nil {
		t.Fatalf("Dict.PromoteAllTemp: %v", err)
	}
	if reply.Count != 0 {
		t.Errorf("expected 0, got %d", reply.Count)
	}
}

// ── Dict 词频 ──

func TestDictFreqOperations(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	// 查询空词频
	var listReply rpcapi.FreqSearchReply
	err := client.call("Dict.GetFreqList", &rpcapi.FreqSearchArgs{Limit: 10}, &listReply)
	if err != nil {
		t.Fatalf("Dict.GetFreqList: %v", err)
	}
	if listReply.Total != 0 {
		t.Errorf("expected 0 freq entries, got %d", listReply.Total)
	}

	// 清空词频
	var clearReply rpcapi.FreqClearReply
	err = client.call("Dict.ClearFreq", &rpcapi.FreqClearArgs{}, &clearReply)
	if err != nil {
		t.Fatalf("Dict.ClearFreq: %v", err)
	}
	if clearReply.Count != 0 {
		t.Errorf("expected 0, got %d", clearReply.Count)
	}
}

// ── Shadow 操作 ──

func TestShadowPinAndGetRules(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty

	err := client.call("Shadow.Pin", &rpcapi.ShadowPinArgs{
		Code: "gg", Word: "王", Position: 0,
	}, &empty)
	if err != nil {
		t.Fatalf("Shadow.Pin: %v", err)
	}

	err = client.call("Shadow.Delete", &rpcapi.ShadowDeleteArgs{
		Code: "gg", Word: "王国",
	}, &empty)
	if err != nil {
		t.Fatalf("Shadow.Delete: %v", err)
	}

	var reply rpcapi.ShadowRulesReply
	err = client.call("Shadow.GetRules", &rpcapi.ShadowGetRulesArgs{Code: "gg"}, &reply)
	if err != nil {
		t.Fatalf("Shadow.GetRules: %v", err)
	}
	if len(reply.Pinned) != 1 || reply.Pinned[0].Word != "王" {
		t.Errorf("unexpected pinned: %+v", reply.Pinned)
	}
	if len(reply.Deleted) != 1 || reply.Deleted[0] != "王国" {
		t.Errorf("unexpected deleted: %+v", reply.Deleted)
	}

	err = client.call("Shadow.RemoveRule", &rpcapi.ShadowDeleteArgs{
		Code: "gg", Word: "王",
	}, &empty)
	if err != nil {
		t.Fatalf("Shadow.RemoveRule: %v", err)
	}

	var reply2 rpcapi.ShadowRulesReply
	client.call("Shadow.GetRules", &rpcapi.ShadowGetRulesArgs{Code: "gg"}, &reply2)
	if len(reply2.Pinned) != 0 {
		t.Errorf("expected 0 pinned after remove, got %d", len(reply2.Pinned))
	}
}

func TestShadowGetAllRules(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty

	// 添加多个编码的规则
	client.call("Shadow.Pin", &rpcapi.ShadowPinArgs{Code: "aa", Word: "词A", Position: 0}, &empty)
	client.call("Shadow.Pin", &rpcapi.ShadowPinArgs{Code: "bb", Word: "词B", Position: 1}, &empty)
	client.call("Shadow.Delete", &rpcapi.ShadowDeleteArgs{Code: "aa", Word: "删除词"}, &empty)

	var reply rpcapi.ShadowGetAllRulesReply
	err := client.call("Shadow.GetAllRules", &rpcapi.ShadowGetAllRulesArgs{}, &reply)
	if err != nil {
		t.Fatalf("Shadow.GetAllRules: %v", err)
	}

	if len(reply.Rules) < 2 {
		t.Errorf("expected at least 2 codes with rules, got %d", len(reply.Rules))
	}
}

// ── System 操作 ──

func TestSystemPing(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	err := client.call("System.Ping", &rpcapi.Empty{}, &empty)
	if err != nil {
		t.Fatalf("System.Ping: %v", err)
	}
}

func TestSystemGetStatus(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var reply rpcapi.SystemStatusReply
	err := client.call("System.GetStatus", &rpcapi.Empty{}, &reply)
	if err != nil {
		t.Fatalf("System.GetStatus: %v", err)
	}
	if !reply.Running {
		t.Error("expected running=true")
	}
}

func TestSystemReloadPhrases(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	err := client.call("System.ReloadPhrases", &rpcapi.Empty{}, &empty)
	if err != nil {
		t.Fatalf("System.ReloadPhrases: %v", err)
	}
}

func TestSystemReloadAll(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	err := client.call("System.ReloadAll", &rpcapi.Empty{}, &empty)
	if err != nil {
		t.Fatalf("System.ReloadAll: %v", err)
	}
}

func TestSystemResetDB(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	// 先添加一些数据
	var empty rpcapi.Empty
	client.call("Dict.Add", &rpcapi.DictAddArgs{Code: "ab", Text: "测试", Weight: 100}, &empty)

	// 重置指定方案
	var reply rpcapi.SystemResetDBReply
	err := client.call("System.ResetDB", &rpcapi.SystemResetDBArgs{SchemaID: "test"}, &reply)
	if err != nil {
		t.Fatalf("System.ResetDB: %v", err)
	}
	if !reply.Success {
		t.Error("expected success=true")
	}

	// 验证数据已清除
	var searchReply rpcapi.DictSearchReply
	client.call("Dict.Search", &rpcapi.DictSearchArgs{Prefix: "ab", Limit: 10}, &searchReply)
	if searchReply.Total != 0 {
		t.Errorf("expected 0 words after reset, got %d", searchReply.Total)
	}
}

func TestSystemResetDBAll(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	client.call("Dict.Add", &rpcapi.DictAddArgs{Code: "xy", Text: "全部", Weight: 100}, &empty)

	// 不指定 schemaID = 清空全部
	var reply rpcapi.SystemResetDBReply
	err := client.call("System.ResetDB", &rpcapi.SystemResetDBArgs{}, &reply)
	if err != nil {
		t.Fatalf("System.ResetDB all: %v", err)
	}
	if !reply.Success {
		t.Error("expected success=true")
	}
}

func TestSystemDeleteSchema(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	client.call("Dict.Add", &rpcapi.DictAddArgs{Code: "zz", Text: "删除", Weight: 100}, &empty)

	var reply rpcapi.SystemResetDBReply
	err := client.call("System.DeleteSchema", &rpcapi.SystemResetDBArgs{SchemaID: "test"}, &reply)
	if err != nil {
		t.Fatalf("System.DeleteSchema: %v", err)
	}
	if !reply.Success {
		t.Error("expected success=true")
	}
}

func TestSystemDeleteSchemaRequiresID(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var reply rpcapi.SystemResetDBReply
	err := client.call("System.DeleteSchema", &rpcapi.SystemResetDBArgs{}, &reply)
	if err == nil {
		t.Error("expected error when schema_id is empty")
	}
}

func TestSystemListSchemas(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var reply rpcapi.ListSchemasReply
	err := client.call("System.ListSchemas", &rpcapi.Empty{}, &reply)
	if err != nil {
		t.Fatalf("System.ListSchemas: %v", err)
	}
	// 至少应该有 test 方案（从配置中获取）
}

// ── Phrase 操作 ──

func TestPhraseAddAndList(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	err := client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Code: "dh", Text: "电话", Type: "text", Position: 0,
	}, &empty)
	if err != nil {
		t.Fatalf("Phrase.Add: %v", err)
	}

	err = client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Code: "dz", Text: "地址", Type: "text", Position: 1,
	}, &empty)
	if err != nil {
		t.Fatalf("Phrase.Add 2: %v", err)
	}

	var reply rpcapi.PhraseListReply
	err = client.call("Phrase.List", &rpcapi.Empty{}, &reply)
	if err != nil {
		t.Fatalf("Phrase.List: %v", err)
	}
	if reply.Total != 2 {
		t.Errorf("expected 2 phrases, got %d", reply.Total)
	}
}

func TestPhraseUpdate(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Code: "sj", Text: "时间", Type: "text", Position: 0,
	}, &empty)

	// 更新文本
	err := client.call("Phrase.Update", &rpcapi.PhraseUpdateArgs{
		Code: "sj", Text: "时间", NewText: "当前时间",
	}, &empty)
	if err != nil {
		t.Fatalf("Phrase.Update: %v", err)
	}

	var reply rpcapi.PhraseListReply
	client.call("Phrase.List", &rpcapi.Empty{}, &reply)

	found := false
	for _, p := range reply.Phrases {
		if p.Code == "sj" && p.Text == "当前时间" {
			found = true
			break
		}
	}
	if !found {
		t.Error("expected updated phrase with text '当前时间'")
	}
}

func TestPhraseUpdateEnabled(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Code: "xx", Text: "测试", Type: "text", Position: 0,
	}, &empty)

	// 禁用
	disabled := false
	err := client.call("Phrase.Update", &rpcapi.PhraseUpdateArgs{
		Code: "xx", Text: "测试", Enabled: &disabled,
	}, &empty)
	if err != nil {
		t.Fatalf("Phrase.Update enabled: %v", err)
	}

	var reply rpcapi.PhraseListReply
	client.call("Phrase.List", &rpcapi.Empty{}, &reply)
	for _, p := range reply.Phrases {
		if p.Code == "xx" && p.Enabled {
			t.Error("expected phrase to be disabled")
		}
	}
}

func TestPhraseRemove(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Code: "rm", Text: "待删除", Type: "text", Position: 0,
	}, &empty)

	err := client.call("Phrase.Remove", &rpcapi.PhraseRemoveArgs{Code: "rm", Text: "待删除"}, &empty)
	if err != nil {
		t.Fatalf("Phrase.Remove: %v", err)
	}

	var reply rpcapi.PhraseListReply
	client.call("Phrase.List", &rpcapi.Empty{}, &reply)
	for _, p := range reply.Phrases {
		if p.Code == "rm" {
			t.Error("phrase should have been removed")
		}
	}
}

func TestPhraseResetDefaults(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	// 添加自定义短语
	client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Code: "custom", Text: "自定义", Type: "text", Position: 0,
	}, &empty)

	// 重置
	err := client.call("Phrase.ResetDefaults", &rpcapi.Empty{}, &empty)
	if err != nil {
		t.Fatalf("Phrase.ResetDefaults: %v", err)
	}

	// 自定义短语应该被清除
	var reply rpcapi.PhraseListReply
	client.call("Phrase.List", &rpcapi.Empty{}, &reply)
	for _, p := range reply.Phrases {
		if p.Code == "custom" && !p.IsSystem {
			t.Error("custom phrase should have been cleared")
		}
	}
}

func TestPhraseAddValidation(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty

	// 缺少 code
	err := client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Text: "无编码", Type: "text",
	}, &empty)
	if err == nil {
		t.Error("expected error when code is empty")
	}

	// array 类型缺少 texts 和 name
	err = client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Code: "arr", Type: "array",
	}, &empty)
	if err == nil {
		t.Error("expected error for array type without texts/name")
	}

	// text 类型缺少 text
	err = client.call("Phrase.Add", &rpcapi.PhraseAddArgs{
		Code: "tt", Type: "text",
	}, &empty)
	if err == nil {
		t.Error("expected error for text type without text")
	}
}

// ── 错误处理 ──

func TestCallUnknownMethod(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty
	err := client.call("NoSuch.Method", &rpcapi.Empty{}, &empty)
	if err == nil {
		t.Error("expected error for unknown method")
	}
}

func TestDictAddValidation(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty

	// 缺少 code
	err := client.call("Dict.Add", &rpcapi.DictAddArgs{Text: "测试", Weight: 100}, &empty)
	if err == nil {
		t.Error("expected error when code is empty")
	}

	// 缺少 text
	err = client.call("Dict.Add", &rpcapi.DictAddArgs{Code: "ab", Weight: 100}, &empty)
	if err == nil {
		t.Error("expected error when text is empty")
	}
}

// ── 路由器 ──

func TestRouterDispatchUnknown(t *testing.T) {
	r := NewRouter()
	_, err := r.Dispatch("unknown", nil)
	if err == nil {
		t.Error("expected error for unknown method")
	}
}

func TestRouterRegisterAndDispatch(t *testing.T) {
	r := NewRouter()
	RegisterMethod(r, "Test.Echo", func(args *rpcapi.DictAddArgs, reply *rpcapi.Empty) error {
		if args.Code != "hello" {
			return fmt.Errorf("unexpected code: %s", args.Code)
		}
		return nil
	})

	params := []byte(`{"code":"hello","text":"world"}`)
	_, err := r.Dispatch("Test.Echo", params)
	if err != nil {
		t.Fatalf("Dispatch: %v", err)
	}
}

func TestRouterErrorPropagation(t *testing.T) {
	r := NewRouter()
	RegisterMethod(r, "Test.Fail", func(args *rpcapi.Empty, reply *rpcapi.Empty) error {
		return fmt.Errorf("intentional error")
	})

	_, err := r.Dispatch("Test.Fail", nil)
	if err == nil || err.Error() != "intentional error" {
		t.Errorf("expected 'intentional error', got: %v", err)
	}
}
