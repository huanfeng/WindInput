package rpc

import (
	"fmt"
	"log/slog"
	"net"
	"sync"

	"github.com/Microsoft/go-winio"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// EventBroadcaster manages event subscribers and broadcasts data change events.
type EventBroadcaster struct {
	mu          sync.RWMutex
	subscribers map[int]chan rpcapi.EventMessage
	nextID      int
	logger      *slog.Logger
}

func NewEventBroadcaster(logger *slog.Logger) *EventBroadcaster {
	return &EventBroadcaster{
		subscribers: make(map[int]chan rpcapi.EventMessage),
		logger:      logger,
	}
}

// Subscribe registers a new subscriber and returns its ID and channel.
func (b *EventBroadcaster) Subscribe() (int, <-chan rpcapi.EventMessage) {
	b.mu.Lock()
	defer b.mu.Unlock()
	id := b.nextID
	b.nextID++
	ch := make(chan rpcapi.EventMessage, 32) // buffered to avoid blocking broadcaster
	b.subscribers[id] = ch
	b.logger.Debug("event subscriber added", "id", id, "total", len(b.subscribers))
	return id, ch
}

// Unsubscribe removes a subscriber.
func (b *EventBroadcaster) Unsubscribe(id int) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if ch, ok := b.subscribers[id]; ok {
		close(ch)
		delete(b.subscribers, id)
	}
	b.logger.Debug("event subscriber removed", "id", id, "total", len(b.subscribers))
}

// Broadcast sends an event to all subscribers (non-blocking).
func (b *EventBroadcaster) Broadcast(msg rpcapi.EventMessage) {
	b.mu.RLock()
	defer b.mu.RUnlock()
	for _, ch := range b.subscribers {
		select {
		case ch <- msg:
		default:
			// Channel full, skip (subscriber too slow)
		}
	}
}

// ── Event Pipe Server ──

// EventPipeServer manages the event streaming named pipe.
type EventPipeServer struct {
	broadcaster *EventBroadcaster
	logger      *slog.Logger
	listener    net.Listener
	wg          sync.WaitGroup
	stopCh      chan struct{}
}

func NewEventPipeServer(broadcaster *EventBroadcaster, logger *slog.Logger) *EventPipeServer {
	return &EventPipeServer{
		broadcaster: broadcaster,
		logger:      logger,
		stopCh:      make(chan struct{}),
	}
}

// Start begins listening on the event pipe.
func (e *EventPipeServer) Start() error {
	pipeConfig := &winio.PipeConfig{
		SecurityDescriptor: "",
		InputBufferSize:    4096,
		OutputBufferSize:   65536,
	}
	listener, err := winio.ListenPipe(rpcapi.RPCEventPipeName, pipeConfig)
	if err != nil {
		return fmt.Errorf("listen event pipe: %w", err)
	}
	e.listener = listener
	e.logger.Info("Event pipe started", "pipe", rpcapi.RPCEventPipeName)

	e.wg.Add(1)
	go e.acceptLoop()
	return nil
}

// Stop closes the event pipe.
func (e *EventPipeServer) Stop() {
	close(e.stopCh)
	if e.listener != nil {
		e.listener.Close()
	}
	e.wg.Wait()
	e.logger.Info("Event pipe stopped")
}

func (e *EventPipeServer) acceptLoop() {
	defer e.wg.Done()
	for {
		conn, err := e.listener.Accept()
		if err != nil {
			select {
			case <-e.stopCh:
				return
			default:
				e.logger.Error("event pipe accept error", "error", err)
				continue
			}
		}
		e.wg.Add(1)
		go e.handleConn(conn)
	}
}

func (e *EventPipeServer) handleConn(conn net.Conn) {
	defer e.wg.Done()
	defer conn.Close()

	id, ch := e.broadcaster.Subscribe()
	defer e.broadcaster.Unsubscribe(id)

	for {
		select {
		case msg, ok := <-ch:
			if !ok {
				return // channel closed
			}
			if err := rpcapi.WriteMessage(conn, &msg); err != nil {
				return // write error, client disconnected
			}
		case <-e.stopCh:
			return
		}
	}
}
