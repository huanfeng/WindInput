package store

import (
	"path/filepath"
	"testing"

	bolt "go.etcd.io/bbolt"
)

func TestOpenClose(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "test.db")

	// Open (creates db).
	s, err := Open(dbPath)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}

	// version should be "1".
	ver, err := s.GetMeta("version")
	if err != nil {
		t.Fatalf("GetMeta version: %v", err)
	}
	if ver != "1" {
		t.Errorf("expected version=1, got %q", ver)
	}

	// device_id should be non-empty.
	devID, err := s.GetMeta("device_id")
	if err != nil {
		t.Fatalf("GetMeta device_id: %v", err)
	}
	if devID == "" {
		t.Error("device_id should be non-empty")
	}

	// Close.
	if err := s.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	// Reopen and verify data persists.
	s2, err := Open(dbPath)
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	defer s2.Close()

	ver2, err := s2.GetMeta("version")
	if err != nil {
		t.Fatalf("GetMeta version after reopen: %v", err)
	}
	if ver2 != "1" {
		t.Errorf("after reopen: expected version=1, got %q", ver2)
	}

	devID2, err := s2.GetMeta("device_id")
	if err != nil {
		t.Fatalf("GetMeta device_id after reopen: %v", err)
	}
	if devID2 != devID {
		t.Errorf("device_id changed after reopen: was %q, now %q", devID, devID2)
	}
}

func TestSetMeta(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	defer s.Close()

	if err := s.SetMeta("foo", "bar"); err != nil {
		t.Fatalf("SetMeta: %v", err)
	}
	val, err := s.GetMeta("foo")
	if err != nil {
		t.Fatalf("GetMeta: %v", err)
	}
	if val != "bar" {
		t.Errorf("expected bar, got %q", val)
	}
}

func TestSchemaBuckets(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	defer s.Close()

	err = s.DB().Update(func(tx *bolt.Tx) error {
		b, err := schemaBucket(tx, "wubi86", true)
		if err != nil {
			return err
		}
		if b == nil {
			t.Error("schemaBucket returned nil")
		}
		sub, err := schemaSubBucket(tx, "wubi86", "Words", true)
		if err != nil {
			return err
		}
		if sub == nil {
			t.Error("schemaSubBucket returned nil")
		}
		return nil
	})
	if err != nil {
		t.Fatalf("schema bucket test: %v", err)
	}
}

func TestSchemaBucket_NotFound(t *testing.T) {
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "test.db"))
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	defer s.Close()

	err = s.DB().View(func(tx *bolt.Tx) error {
		_, err := schemaBucket(tx, "nonexistent", false)
		return err
	})
	if err == nil {
		t.Error("expected error for missing schema bucket, got nil")
	}
}
