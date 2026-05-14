//go:build !debug

package coordinator

// startGoroutineWatchdog is a no-op in non-debug builds.
func (c *Coordinator) startGoroutineWatchdog() {}
