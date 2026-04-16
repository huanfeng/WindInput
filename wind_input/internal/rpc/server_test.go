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
	req := rpcapi.Request{ID: 1, Method: method, Params: params}
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

// setupTestRPC 创建测试用服务端和客户端（通过 net.Pipe 模拟管道连接）
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
	systemSvc := &SystemService{dm: dm, server: &Server{}, logger: logger}

	RegisterMethod(router, "Dict.Search", dictSvc.Search)
	RegisterMethod(router, "Dict.SearchByCode", dictSvc.SearchByCode)
	RegisterMethod(router, "Dict.Add", dictSvc.Add)
	RegisterMethod(router, "Dict.Remove", dictSvc.Remove)
	RegisterMethod(router, "Dict.Update", dictSvc.Update)
	RegisterMethod(router, "Dict.GetStats", dictSvc.GetStats)
	RegisterMethod(router, "Dict.GetSchemaStats", dictSvc.GetSchemaStats)
	RegisterMethod(router, "Dict.BatchAdd", dictSvc.BatchAdd)
	RegisterMethod(router, "Shadow.Pin", shadowSvc.Pin)
	RegisterMethod(router, "Shadow.Delete", shadowSvc.Delete)
	RegisterMethod(router, "Shadow.RemoveRule", shadowSvc.RemoveRule)
	RegisterMethod(router, "Shadow.GetRules", shadowSvc.GetRules)
	RegisterMethod(router, "System.Ping", systemSvc.Ping)
	RegisterMethod(router, "System.GetStatus", systemSvc.GetStatus)

	serverConn, clientConn := net.Pipe()

	// 在后台运行服务端处理循环
	srv := &Server{router: router, stopCh: make(chan struct{})}
	srv.wg.Add(1)
	go srv.handleConn(serverConn)
	t.Cleanup(func() { clientConn.Close() })

	return &testClient{conn: clientConn}
}

func TestDictAddAndSearch(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	// 添加词条
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

	// 精确查询
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

	// 前缀搜索
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

	// 更新权重
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

	// 删除
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

	// 第一页（limit=2）
	var page1 rpcapi.DictSearchReply
	client.call("Dict.Search", &rpcapi.DictSearchArgs{Prefix: "ci", Limit: 2, Offset: 0}, &page1)
	if page1.Total != 5 {
		t.Errorf("expected total=5, got %d", page1.Total)
	}
	if len(page1.Words) != 2 {
		t.Errorf("expected 2 words on page 1, got %d", len(page1.Words))
	}

	// 第二页
	var page2 rpcapi.DictSearchReply
	client.call("Dict.Search", &rpcapi.DictSearchArgs{Prefix: "ci", Limit: 2, Offset: 2}, &page2)
	if len(page2.Words) != 2 {
		t.Errorf("expected 2 words on page 2, got %d", len(page2.Words))
	}

	// 第三页
	var page3 rpcapi.DictSearchReply
	client.call("Dict.Search", &rpcapi.DictSearchArgs{Prefix: "ci", Limit: 2, Offset: 4}, &page3)
	if len(page3.Words) != 1 {
		t.Errorf("expected 1 word on page 3, got %d", len(page3.Words))
	}
}

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

func TestShadowPinAndGetRules(t *testing.T) {
	client := setupTestRPC(t)
	defer client.Close()

	var empty rpcapi.Empty

	// Pin
	err := client.call("Shadow.Pin", &rpcapi.ShadowPinArgs{
		Code: "gg", Word: "王", Position: 0,
	}, &empty)
	if err != nil {
		t.Fatalf("Shadow.Pin: %v", err)
	}

	// Delete
	err = client.call("Shadow.Delete", &rpcapi.ShadowDeleteArgs{
		Code: "gg", Word: "王国",
	}, &empty)
	if err != nil {
		t.Fatalf("Shadow.Delete: %v", err)
	}

	// GetRules
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

	// RemoveRule
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
