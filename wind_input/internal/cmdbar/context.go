// Package cmdbar wires together the command-bar parser, evaluator and
// function registry. See docs/design/command-bar-design.md.
package cmdbar

import (
	"sync"
	"time"
)

// EvalContext is the runtime environment exposed to evaluator functions.
// Implementations must be safe for use from the evaluator goroutine. The
// MemoryContext type below is the default test-friendly implementation;
// production wiring (P3+) will adapt the live IME state to this
// interface.
type EvalContext interface {
	// Input returns the user's current composition / typed code.
	Input() string
	// Last returns the n-th most recent committed text (1-based; 1 is
	// the most recent). Out-of-range queries return an empty string.
	Last(n int) string
	// Clip returns the current clipboard contents when n == 0 or 1, or
	// the n-th historical clipboard entry (1-based). Implementations
	// may return "" when n is out of range or history is unavailable.
	Clip(n int) string
	// Sel returns the currently selected text in the foreground app, or
	// "" when there is no selection.
	Sel() string
	// App returns the foreground process basename.
	App() string
	// Title returns the foreground window title.
	Title() string
	// Env returns the value of the named environment variable.
	Env(name string) string
	// Now returns the current time. Tests inject a fixed clock.
	Now() time.Time
	// Services returns the bundle of injected side-effect dependencies
	// available to action functions. May return nil when running in a
	// pure-evaluation context (e.g. P2-era tests); action functions
	// must defend against that.
	Services() *Services
}

// MemoryContext is an in-memory EvalContext used for testing. All
// fields may be set directly; History maintains the last-N commits and
// is queried via Last.
type MemoryContext struct {
	InputStr  string
	History   *History
	ClipStr   string
	ClipStack []string // index 0 = most recent
	SelStr    string
	AppName   string
	TitleStr  string
	EnvMap    map[string]string
	Clock     time.Time
	Svcs      *Services
}

// NewMemoryContext returns a zero-value MemoryContext with a 16-entry
// history buffer.
func NewMemoryContext() *MemoryContext {
	return &MemoryContext{History: NewHistory(16)}
}

func (c *MemoryContext) Input() string { return c.InputStr }
func (c *MemoryContext) Last(n int) string {
	if c.History == nil {
		return ""
	}
	return c.History.Get(n)
}
func (c *MemoryContext) Clip(n int) string {
	if n <= 1 {
		// clip() and clip(1) both refer to the current clipboard.
		if c.ClipStr != "" || len(c.ClipStack) == 0 {
			return c.ClipStr
		}
		return c.ClipStack[0]
	}
	idx := n - 1
	if idx < 0 || idx >= len(c.ClipStack) {
		return ""
	}
	return c.ClipStack[idx]
}
func (c *MemoryContext) Sel() string   { return c.SelStr }
func (c *MemoryContext) App() string   { return c.AppName }
func (c *MemoryContext) Title() string { return c.TitleStr }
func (c *MemoryContext) Env(name string) string {
	if c.EnvMap == nil {
		return ""
	}
	return c.EnvMap[name]
}
func (c *MemoryContext) Services() *Services { return c.Svcs }
func (c *MemoryContext) Now() time.Time {
	if c.Clock.IsZero() {
		return time.Now()
	}
	return c.Clock
}

// History is a small fixed-capacity ring buffer of committed text
// snippets. Index 1 is the most recent push; index N (== capacity) is
// the oldest still-retained entry.
type History struct {
	mu   sync.Mutex
	buf  []string
	cap  int
	head int // points at next write slot
	full bool
}

// NewHistory constructs a History with the given capacity (clamped to
// minimum 1).
func NewHistory(capacity int) *History {
	if capacity < 1 {
		capacity = 1
	}
	return &History{buf: make([]string, capacity), cap: capacity}
}

// Push records s as the most recent entry.
func (h *History) Push(s string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.buf[h.head] = s
	h.head = (h.head + 1) % h.cap
	if h.head == 0 {
		h.full = true
	}
}

// Get returns the n-th most recent entry (1-based). Out-of-range
// queries return "".
func (h *History) Get(n int) string {
	h.mu.Lock()
	defer h.mu.Unlock()
	if n < 1 {
		return ""
	}
	size := h.cap
	if !h.full {
		size = h.head
	}
	if n > size {
		return ""
	}
	idx := (h.head - n + h.cap) % h.cap
	return h.buf[idx]
}

// Len returns the current number of stored entries.
func (h *History) Len() int {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.full {
		return h.cap
	}
	return h.head
}
