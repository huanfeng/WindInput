package rpc

import (
	"encoding/json"
	"fmt"
	"sync"
)

// Handler 方法处理函数
type Handler func(params json.RawMessage) (any, error)

// Router 方法路由器
type Router struct {
	mu       sync.RWMutex
	handlers map[string]Handler
}

// NewRouter 创建路由器
func NewRouter() *Router {
	return &Router{handlers: make(map[string]Handler)}
}

// Handle 注册一个方法处理器
func (r *Router) Handle(method string, h Handler) {
	r.mu.Lock()
	r.handlers[method] = h
	r.mu.Unlock()
}

// Dispatch 分发请求到对应的处理器
func (r *Router) Dispatch(method string, params json.RawMessage) (any, error) {
	r.mu.RLock()
	h, ok := r.handlers[method]
	r.mu.RUnlock()
	if !ok {
		return nil, fmt.Errorf("method not found: %s", method)
	}
	return h(params)
}

// RegisterMethod 泛型方法注册：自动做 JSON 编解码，保持 service 方法签名不变
func RegisterMethod[A, R any](r *Router, name string, fn func(a *A, r *R) error) {
	r.Handle(name, func(params json.RawMessage) (any, error) {
		var args A
		if len(params) > 0 {
			if err := json.Unmarshal(params, &args); err != nil {
				return nil, fmt.Errorf("unmarshal params: %w", err)
			}
		}
		var reply R
		if err := fn(&args, &reply); err != nil {
			return nil, err
		}
		return &reply, nil
	})
}
