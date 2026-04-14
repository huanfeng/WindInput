package store

import (
	"fmt"
	"sync"
	"time"

	bolt "go.etcd.io/bbolt"
)

// WriteOp represents a single put or delete operation destined for a nested
// bucket path.  A nil Value means delete.
type WriteOp struct {
	Bucket [][]byte // nested bucket path, e.g. [bucketSchemas, schemaID, "Words"]
	Key    string
	Value  []byte // nil → delete key
}

// WriteBufferConfig controls flush behaviour.
type WriteBufferConfig struct {
	FlushSize     int           // flush when pending ops reach this count
	FlushInterval time.Duration // flush at least this often
}

// DefaultWriteBufferConfig returns sensible production defaults.
func DefaultWriteBufferConfig() WriteBufferConfig {
	return WriteBufferConfig{
		FlushSize:     50,
		FlushInterval: 30 * time.Second,
	}
}

// WriteBuffer batches writes and flushes them to bbolt in a single transaction,
// either when the pending count reaches FlushSize or after FlushInterval.
type WriteBuffer struct {
	db     *bolt.DB
	config WriteBufferConfig

	mu      sync.Mutex
	pending []WriteOp

	flushCh chan struct{}
	done    chan struct{}
	wg      sync.WaitGroup
}

// NewWriteBuffer creates a WriteBuffer and starts its background flush goroutine.
func NewWriteBuffer(db *bolt.DB, config WriteBufferConfig) *WriteBuffer {
	wb := &WriteBuffer{
		db:      db,
		config:  config,
		flushCh: make(chan struct{}, 1),
		done:    make(chan struct{}),
	}
	wb.wg.Add(1)
	go wb.loop()
	return wb
}

// Enqueue adds an operation to the buffer.  If the pending count reaches
// FlushSize a flush is triggered immediately.
func (wb *WriteBuffer) Enqueue(op WriteOp) {
	wb.mu.Lock()
	wb.pending = append(wb.pending, op)
	trigger := len(wb.pending) >= wb.config.FlushSize
	wb.mu.Unlock()

	if trigger {
		select {
		case wb.flushCh <- struct{}{}:
		default:
		}
	}
}

// Pending returns the number of buffered (un-flushed) operations.
func (wb *WriteBuffer) Pending() int {
	wb.mu.Lock()
	defer wb.mu.Unlock()
	return len(wb.pending)
}

// Close flushes remaining operations and stops the background goroutine.
func (wb *WriteBuffer) Close() {
	close(wb.done)
	wb.wg.Wait()
	// Final flush of anything that arrived before the goroutine stopped.
	_ = wb.flush()
}

// loop is the background goroutine that drives timer-based flushes.
func (wb *WriteBuffer) loop() {
	defer wb.wg.Done()
	ticker := time.NewTicker(wb.config.FlushInterval)
	defer ticker.Stop()
	for {
		select {
		case <-wb.done:
			return
		case <-wb.flushCh:
			_ = wb.flush()
		case <-ticker.C:
			_ = wb.flush()
		}
	}
}

// flush writes all pending ops in a single bbolt transaction.
// On failure the failed ops are re-enqueued at the front of the buffer.
func (wb *WriteBuffer) flush() error {
	wb.mu.Lock()
	if len(wb.pending) == 0 {
		wb.mu.Unlock()
		return nil
	}
	ops := wb.pending
	wb.pending = nil
	wb.mu.Unlock()

	err := wb.db.Update(func(tx *bolt.Tx) error {
		for _, op := range ops {
			b, err := navigateBuckets(tx, op.Bucket, true)
			if err != nil {
				return fmt.Errorf("navigateBuckets: %w", err)
			}
			if op.Value == nil {
				if err := b.Delete([]byte(op.Key)); err != nil {
					return fmt.Errorf("delete %q: %w", op.Key, err)
				}
			} else {
				if err := b.Put([]byte(op.Key), op.Value); err != nil {
					return fmt.Errorf("put %q: %w", op.Key, err)
				}
			}
		}
		return nil
	})

	if err != nil {
		// Re-enqueue failed ops at the front so they are retried next flush.
		wb.mu.Lock()
		wb.pending = append(ops, wb.pending...)
		wb.mu.Unlock()
	}
	return err
}

// navigateBuckets walks (and optionally creates) a nested bucket path inside
// the given transaction, returning the leaf bucket.
func navigateBuckets(tx *bolt.Tx, path [][]byte, create bool) (*bolt.Bucket, error) {
	if len(path) == 0 {
		return nil, fmt.Errorf("navigateBuckets: empty path")
	}

	var cur *bolt.Bucket
	// First segment is always a top-level bucket.
	if create {
		b, err := tx.CreateBucketIfNotExists(path[0])
		if err != nil {
			return nil, fmt.Errorf("create top-level bucket %q: %w", path[0], err)
		}
		cur = b
	} else {
		cur = tx.Bucket(path[0])
		if cur == nil {
			return nil, fmt.Errorf("bucket %q not found", path[0])
		}
	}

	for _, seg := range path[1:] {
		if create {
			b, err := cur.CreateBucketIfNotExists(seg)
			if err != nil {
				return nil, fmt.Errorf("create bucket %q: %w", seg, err)
			}
			cur = b
		} else {
			next := cur.Bucket(seg)
			if next == nil {
				return nil, fmt.Errorf("bucket %q not found", seg)
			}
			cur = next
		}
	}
	return cur, nil
}
