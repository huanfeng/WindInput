package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io/fs"
	"log"
	"net"
	"net/http"
	"reflect"
	"strconv"
	"sync"
	"time"

	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

const (
	webServerBasePort = 18923
	webServerMaxTries = 3
)

// Web 模式空闲退出参数（声明为 var 以便测试覆盖为更短时长）。
//
// 心跳模型：前端每 ~10s POST /api/ping 刷新 lastSeen（与 rpcClient/SSE 是否工作无关，
// 只表示「页面还开着」）；后台每 webIdleCheck 检查一次，超过 webIdleTimeout 无心跳即退出。
// 浏览器后台标签会被节流（最慢约 1 次/分钟），故 webIdleTimeout 取较大值避免误退。
// 关闭页面时前端 pagehide 发 beacon 到 /api/bye，webByeGrace 后退出（其间若有新 ping
// 则取消，用于覆盖「刷新」——刷新也会触发 pagehide 但随即重新 ping）。
var (
	webIdleTimeout = 120 * time.Second
	webIdleCheck   = 5 * time.Second
	webByeGrace    = 3 * time.Second
)

var errType = reflect.TypeFor[error]()

// callReflect 在 target 上按名查找导出方法并调用，复刻 Wails runtime 对 Bind
// 对象的反射分发。rawArgs 按各形参类型 JSON 反序列化。返回值约定：若最后一个
// 返回值是 error 则作为 error；其余作为 data（0 个 -> nil，1 个 -> 该值，多个 -> []any）。
//
// 被反射调用的方法若 panic（例如对占位 ctx 调 wailsRuntime.Log*），由 defer
// recover 捕获并转为 error 返回，绝不外泄崩溃 handler。
func callReflect(target any, method string, rawArgs []json.RawMessage) (data any, err error) {
	defer func() {
		if r := recover(); r != nil {
			data = nil
			err = fmt.Errorf("method %s panicked: %v", method, r)
		}
	}()

	m := reflect.ValueOf(target).MethodByName(method)
	if !m.IsValid() {
		return nil, fmt.Errorf("unknown method: %s", method)
	}
	mt := m.Type()
	if len(rawArgs) != mt.NumIn() {
		return nil, fmt.Errorf("method %s expects %d args, got %d", method, mt.NumIn(), len(rawArgs))
	}
	in := make([]reflect.Value, mt.NumIn())
	for i := 0; i < mt.NumIn(); i++ {
		p := reflect.New(mt.In(i)) // *ParamType
		if e := json.Unmarshal(rawArgs[i], p.Interface()); e != nil {
			return nil, fmt.Errorf("arg %d (%s): %w", i, mt.In(i), e)
		}
		in[i] = p.Elem()
	}
	out := m.Call(in)

	// 分离尾部 error 返回值
	if n := len(out); n > 0 && out[n-1].Type().Implements(errType) {
		if e := out[n-1]; !e.IsNil() {
			return nil, e.Interface().(error)
		}
		out = out[:n-1]
	}
	switch len(out) {
	case 0:
		return nil, nil
	case 1:
		return out[0].Interface(), nil
	default:
		arr := make([]any, len(out))
		for i, v := range out {
			arr[i] = v.Interface()
		}
		return arr, nil
	}
}

// webServer 是 Web 形态的 HTTP 服务（不依赖 WebView2）。
// handler 复用既有 *App 方法，最终经 rpcClient 走命名管道到主程序。
type webServer struct {
	app    *App
	server *http.Server
	port   int

	// 空闲自动退出（心跳模型，详见 webIdleTimeout 注释）：前端 ping 刷新 lastSeen，
	// 后台 monitorIdle 超时未见心跳即关闭 done。runWebMode 等待 done 即退出进程，
	// 避免 Web 进程常驻锁定 exe。与 SSE/rpcClient 状态解耦，仅表示「页面还开着」。
	mu        sync.Mutex
	lastSeen  time.Time
	byeTimer  *time.Timer
	connected bool // 仅用于「浏览器首次连上」记一次日志
	done      chan struct{}
	doneOnce  sync.Once

	// SSE 事件广播：维护活跃连接，broadcast 把任意命名事件（含 App 直接 emit 的
	// update:* 等）投递到所有浏览器。复刻桌面 wailsRuntime.EventsEmit 的投递语义。
	sseMu    sync.Mutex
	sseConns map[*sseConn]struct{}
}

// sseConn 是单个 SSE 连接的帧队列。所有写出由该连接的 handler goroutine 单独完成，
// 广播/订阅回调只往 ch 投帧，避免对同一 http.ResponseWriter 并发写。
type sseConn struct {
	ch chan sseFrame
}

type sseFrame struct {
	event string
	data  []byte
}

func (ws *webServer) addConn(c *sseConn) {
	ws.sseMu.Lock()
	if ws.sseConns == nil {
		ws.sseConns = make(map[*sseConn]struct{})
	}
	ws.sseConns[c] = struct{}{}
	ws.sseMu.Unlock()
}

func (ws *webServer) removeConn(c *sseConn) {
	ws.sseMu.Lock()
	delete(ws.sseConns, c)
	ws.sseMu.Unlock()
}

// broadcast 把一个命名事件投递给所有活跃 SSE 连接。data 语义与 wailsRuntime.EventsEmit
// 对齐：0 个 -> null，1 个 -> 该值，多个 -> 数组。慢消费者满队列时丢弃该帧（不阻塞）。
func (ws *webServer) broadcast(event string, data ...any) {
	var payload any
	switch len(data) {
	case 0:
		payload = nil
	case 1:
		payload = data[0]
	default:
		payload = data
	}
	b, err := json.Marshal(payload)
	if err != nil {
		return
	}
	frame := sseFrame{event: event, data: b}
	ws.sseMu.Lock()
	for c := range ws.sseConns {
		select {
		case c.ch <- frame:
		default: // 队列满则丢弃，避免慢连接阻塞广播
		}
	}
	ws.sseMu.Unlock()
}

// touch 刷新心跳时间戳。首次刷新记一条日志，便于诊断「浏览器是否真的连上」。
func (ws *webServer) touch() {
	ws.mu.Lock()
	ws.lastSeen = time.Now()
	if ws.byeTimer != nil {
		ws.byeTimer.Stop()
		ws.byeTimer = nil
	}
	first := !ws.connected
	ws.connected = true
	ws.mu.Unlock()
	if first {
		log.Printf("[web] 浏览器已连接，心跳开始")
	}
}

// handlePing 接收前端心跳：刷新 lastSeen。
func (ws *webServer) handlePing(w http.ResponseWriter, r *http.Request) {
	ws.touch()
	w.WriteHeader(http.StatusNoContent)
}

// handleBye 接收页面关闭信标（pagehide 时 sendBeacon）：webByeGrace 后退出，
// 其间若有新 ping 刷新 lastSeen 则取消（覆盖「刷新页面」场景）。
func (ws *webServer) handleBye(w http.ResponseWriter, r *http.Request) {
	ws.mu.Lock()
	if ws.byeTimer != nil {
		ws.byeTimer.Stop()
	}
	ws.byeTimer = time.AfterFunc(webByeGrace, func() {
		ws.mu.Lock()
		stale := time.Since(ws.lastSeen) >= webByeGrace
		ws.mu.Unlock()
		if stale {
			log.Printf("[web] 页面已关闭，退出 Web 设置服务")
			ws.triggerDone()
		}
	})
	ws.mu.Unlock()
	w.WriteHeader(http.StatusNoContent)
}

// monitorIdle 后台轮询：超过 webIdleTimeout 无心跳则触发退出（信标丢失时的兜底）。
func (ws *webServer) monitorIdle() {
	t := time.NewTicker(webIdleCheck)
	defer t.Stop()
	for {
		select {
		case <-ws.done:
			return
		case <-t.C:
			ws.mu.Lock()
			idle := time.Since(ws.lastSeen) > webIdleTimeout
			ws.mu.Unlock()
			if idle {
				log.Printf("[web] %v 无心跳，自动退出 Web 设置服务", webIdleTimeout)
				ws.triggerDone()
				return
			}
		}
	}
}

func (ws *webServer) triggerDone() { ws.doneOnce.Do(func() { close(ws.done) }) }

// Wait 返回退出信号通道；done 关闭表示应退出进程。
func (ws *webServer) Wait() <-chan struct{} { return ws.done }

// callRequest 是 /api/call 的请求体，镜像 Wails 前端的 window.go.main.App.XXX(args)。
type callRequest struct {
	Method string            `json:"method"`
	Args   []json.RawMessage `json:"args"`
}

// callResponse 是统一响应信封，前端 shim 据此 resolve/reject，模拟 Wails Promise。
type callResponse struct {
	Data  any    `json:"data"`
	Error string `json:"error,omitempty"`
}

func writeCall(w http.ResponseWriter, data any, errStr string) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(callResponse{Data: data, Error: errStr})
}

// handleCall 反射网关：把 {method, args} 分发到 *App 导出方法。
func (ws *webServer) handleCall(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeCall(w, nil, "method not allowed")
		return
	}
	var req callRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeCall(w, nil, "bad request: "+err.Error())
		return
	}
	// 服务端方法黑名单：拒绝任意文件路径读写 / 不可逆全局破坏类方法（详见 web_security.go）。
	// 独立于前端 webShim 黑名单强制，防其被绕过。
	if webCallDenied[req.Method] {
		writeCall(w, nil, "method not permitted in web mode")
		return
	}
	data, err := callReflect(ws.app, req.Method, req.Args)
	if err != nil {
		writeCall(w, nil, err.Error())
		return
	}
	writeCall(w, data, "")
}

// muxWithStatic 装配路由。
//
// 安全：本 server 同源，刻意不包 CORS 中间件，尤其不设 Access-Control-Allow-Origin: *。
// 所有 /api/* 端点经 ws.guard 包裹，抵御恶意网页跨站请求与 DNS rebinding（详见
// web_security.go）。/api/call 反射网关另有服务端方法黑名单（webCallDenied）。
// 静态文件（/）无需 guard：用户直接导航无副作用，且 EventSource 等仍受各自 guard 约束。
func (ws *webServer) muxWithStatic(staticFS fs.FS) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/api/call", ws.guard(ws.handleCall))
	mux.HandleFunc("/api/events", ws.guard(ws.handleEvents))
	mux.HandleFunc("/api/ping", ws.guard(ws.handlePing))
	mux.HandleFunc("/api/bye", ws.guard(ws.handleBye))
	if staticFS != nil {
		mux.Handle("/", http.FileServer(http.FS(staticFS)))
	}
	return mux
}

// handleEvents 把 RPC 事件以 SSE 推给浏览器，复刻 app.go startEventListener 的
// 事件映射（config/stats/system/dict），shim 端以 window.runtime.EventsOn 消费。
//
// 已知限制（待核实 SubscribeEvents 是否支持并发多订阅）：每个 EventSource 连接
// 各自订阅一次；若管道独占，多标签会冲突——首版假定单标签使用。若需支持多标签，
// 改为 server 持有单一订阅 + 维护 SSE 连接集合 fan-out。
func (ws *webServer) handleEvents(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	flusher, ok := w.(http.Flusher)
	if !ok {
		return
	}
	// 注意：SSE 不再作为存活心跳（心跳改由 /api/ping 独立维护，与 rpcClient 是否
	// 可用解耦）。这里承载两类事件：rpcClient 转发的 config/stats/system/dict，以及
	// App 直接 emit 经 broadcast 投递的 update:* 等。
	conn := &sseConn{ch: make(chan sseFrame, 32)}
	ws.addConn(conn)
	defer ws.removeConn(conn)

	// rpcClient 事件 -> 本连接帧队列（rpcClient 不可用时跳过，update:* 仍可经广播到达）。
	if ws.app != nil && ws.app.rpcClient != nil {
		go func() {
			_ = ws.app.rpcClient.SubscribeEvents(r.Context(), func(msg rpcapi.EventMessage) {
				payload := map[string]string{
					"type":      string(msg.Type),
					"schema_id": msg.SchemaID,
					"action":    string(msg.Action),
				}
				name := rpcapi.WailsEventDict
				switch msg.Type {
				case rpcapi.EventTypeConfig:
					name = rpcapi.WailsEventConfig
				case rpcapi.EventTypeStats:
					name = rpcapi.WailsEventStats
				case rpcapi.EventTypeSystem:
					name = rpcapi.WailsEventSystem
				}
				b, _ := json.Marshal(payload)
				select {
				case conn.ch <- sseFrame{event: name, data: b}:
				default:
				}
			})
		}()
	}

	// 主循环：唯一写出者，浏览器断开（r.Context 取消）即退出。
	for {
		select {
		case <-r.Context().Done():
			return
		case fr := <-conn.ch:
			fmt.Fprintf(w, "event: %s\ndata: %s\n\n", fr.event, fr.data)
			flusher.Flush()
		}
	}
}

// Start 从 webServerBasePort 起探测可用端口（顺延 webServerMaxTries 次）并启动服务。
// staticFS 为前端 dist 子文件系统；可为 nil（仅用于测试端口绑定）。
func (ws *webServer) Start(staticFS fs.FS) error {
	handler := ws.muxWithStatic(staticFS)
	var lastErr error
	for i := range webServerMaxTries {
		port := webServerBasePort + i
		ln, err := net.Listen("tcp", "127.0.0.1:"+strconv.Itoa(port))
		if err != nil {
			lastErr = err
			continue
		}
		ws.port = port
		srv := &http.Server{Handler: handler}
		ws.server = srv
		ws.done = make(chan struct{})
		ws.lastSeen = time.Now() // 启动即记一次，给浏览器首个 ping 的宽限（webIdleTimeout）
		go func() { _ = srv.Serve(ln) }()
		go ws.monitorIdle()
		return nil
	}
	return fmt.Errorf("端口 %d-%d 均被占用：%w", webServerBasePort, webServerBasePort+webServerMaxTries-1, lastErr)
}

// Stop 优雅关闭（3s 超时）。
func (ws *webServer) Stop() {
	ws.mu.Lock()
	if ws.byeTimer != nil {
		ws.byeTimer.Stop()
		ws.byeTimer = nil
	}
	ws.mu.Unlock()
	if ws.server == nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	_ = ws.server.Shutdown(ctx)
	ws.server = nil
	ws.port = 0
}

// url 返回当前服务地址。
func (ws *webServer) url() string {
	return "http://127.0.0.1:" + strconv.Itoa(ws.port)
}
