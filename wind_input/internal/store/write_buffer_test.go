package store

import (
	"path/filepath"
	"testing"
	"time"

	bolt "go.etcd.io/bbolt"
)

func openTestDB(t *testing.T) *bolt.DB {
	t.Helper()
	dir := t.TempDir()
	db, err := bolt.Open(filepath.Join(dir, "wb_test.db"), 0600, nil)
	if err != nil {
		t.Fatalf("bolt.Open: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db
}

func readValue(t *testing.T, db *bolt.DB, bucketPath [][]byte, key string) []byte {
	t.Helper()
	var val []byte
	err := db.View(func(tx *bolt.Tx) error {
		b, err := navigateBuckets(tx, bucketPath, false)
		if err != nil {
			return err
		}
		v := b.Get([]byte(key))
		if v != nil {
			val = make([]byte, len(v))
			copy(val, v)
		}
		return nil
	})
	if err != nil {
		t.Fatalf("readValue: %v", err)
	}
	return val
}

func keyExists(t *testing.T, db *bolt.DB, bucketPath [][]byte, key string) bool {
	t.Helper()
	var exists bool
	err := db.View(func(tx *bolt.Tx) error {
		b, err := navigateBuckets(tx, bucketPath, false)
		if err != nil {
			// Bucket doesn't exist → key doesn't exist.
			return nil
		}
		exists = b.Get([]byte(key)) != nil
		return nil
	})
	if err != nil {
		t.Fatalf("keyExists: %v", err)
	}
	return exists
}

var testBucket = [][]byte{[]byte("TestData")}

// TestWriteBuffer_BasicFlush: enqueue FlushSize items, verify auto-flush.
func TestWriteBuffer_BasicFlush(t *testing.T) {
	db := openTestDB(t)
	cfg := WriteBufferConfig{
		FlushSize:     5,
		FlushInterval: 10 * time.Second, // long interval so only size triggers flush
	}
	wb := NewWriteBuffer(db, cfg)
	defer wb.Close()

	for i := 0; i < cfg.FlushSize; i++ {
		wb.Enqueue(WriteOp{
			Bucket: testBucket,
			Key:    string(rune('a' + i)),
			Value:  []byte("v"),
		})
	}

	// Give the flush goroutine time to complete.
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if wb.Pending() == 0 {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	if wb.Pending() != 0 {
		t.Errorf("expected 0 pending after auto-flush, got %d", wb.Pending())
	}

	// Verify data in db.
	val := readValue(t, db, testBucket, "a")
	if string(val) != "v" {
		t.Errorf("expected value 'v', got %q", val)
	}
}

// TestWriteBuffer_TimerFlush: enqueue 1 item, verify timer triggers flush.
func TestWriteBuffer_TimerFlush(t *testing.T) {
	db := openTestDB(t)
	cfg := WriteBufferConfig{
		FlushSize:     100,
		FlushInterval: 100 * time.Millisecond,
	}
	wb := NewWriteBuffer(db, cfg)
	defer wb.Close()

	wb.Enqueue(WriteOp{
		Bucket: testBucket,
		Key:    "timer_key",
		Value:  []byte("timer_val"),
	})

	// Wait for timer-driven flush (interval = 100ms, allow 2s).
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if wb.Pending() == 0 {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}

	if wb.Pending() != 0 {
		t.Errorf("expected 0 pending after timer flush, got %d", wb.Pending())
	}

	val := readValue(t, db, testBucket, "timer_key")
	if string(val) != "timer_val" {
		t.Errorf("expected timer_val, got %q", val)
	}
}

// TestWriteBuffer_CloseFlushesRemaining: enqueue items, close, verify flushed.
func TestWriteBuffer_CloseFlushesRemaining(t *testing.T) {
	db := openTestDB(t)
	cfg := WriteBufferConfig{
		FlushSize:     1000,             // never auto-flush by size
		FlushInterval: 10 * time.Second, // never auto-flush by timer in test
	}
	wb := NewWriteBuffer(db, cfg)

	for i := 0; i < 10; i++ {
		wb.Enqueue(WriteOp{
			Bucket: testBucket,
			Key:    string(rune('A' + i)),
			Value:  []byte("close_test"),
		})
	}

	wb.Close()

	val := readValue(t, db, testBucket, "A")
	if string(val) != "close_test" {
		t.Errorf("expected close_test after Close(), got %q", val)
	}
}

// TestWriteBuffer_Delete: verify Value=nil deletes a key.
func TestWriteBuffer_Delete(t *testing.T) {
	db := openTestDB(t)

	// Pre-populate directly.
	err := db.Update(func(tx *bolt.Tx) error {
		b, err := tx.CreateBucketIfNotExists(testBucket[0])
		if err != nil {
			return err
		}
		return b.Put([]byte("del_key"), []byte("will_be_deleted"))
	})
	if err != nil {
		t.Fatalf("pre-populate: %v", err)
	}

	cfg := WriteBufferConfig{
		FlushSize:     1,
		FlushInterval: 10 * time.Second,
	}
	wb := NewWriteBuffer(db, cfg)
	defer wb.Close()

	// Enqueue a delete (nil Value).
	wb.Enqueue(WriteOp{
		Bucket: testBucket,
		Key:    "del_key",
		Value:  nil,
	})

	// Wait for flush.
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if wb.Pending() == 0 {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	if keyExists(t, db, testBucket, "del_key") {
		t.Error("expected key to be deleted, but it still exists")
	}
}
