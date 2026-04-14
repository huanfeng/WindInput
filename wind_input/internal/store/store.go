package store

import (
	"fmt"

	"github.com/google/uuid"
	bolt "go.etcd.io/bbolt"
)

var (
	bucketMeta    = []byte("Meta")
	bucketSchemas = []byte("Schemas")
)

// Store wraps a bbolt database with helpers for the wind_input schema.
type Store struct {
	db   *bolt.DB
	path string
}

// Open opens (or creates) the bbolt database at path and initialises top-level
// buckets and default Meta values.
func Open(path string) (*Store, error) {
	db, err := bolt.Open(path, 0600, nil)
	if err != nil {
		return nil, fmt.Errorf("store.Open: %w", err)
	}
	s := &Store{db: db, path: path}
	if err := s.init(); err != nil {
		_ = db.Close()
		return nil, err
	}
	return s, nil
}

// init creates required buckets and seeds Meta defaults on first open.
func (s *Store) init() error {
	return s.db.Update(func(tx *bolt.Tx) error {
		meta, err := tx.CreateBucketIfNotExists(bucketMeta)
		if err != nil {
			return fmt.Errorf("create Meta bucket: %w", err)
		}
		if _, err := tx.CreateBucketIfNotExists(bucketSchemas); err != nil {
			return fmt.Errorf("create Schemas bucket: %w", err)
		}

		// Seed version if not yet set.
		if meta.Get([]byte("version")) == nil {
			if err := meta.Put([]byte("version"), []byte("1")); err != nil {
				return fmt.Errorf("set version: %w", err)
			}
		}

		// Seed device_id if not yet set.
		if meta.Get([]byte("device_id")) == nil {
			id := uuid.New().String()
			if err := meta.Put([]byte("device_id"), []byte(id)); err != nil {
				return fmt.Errorf("set device_id: %w", err)
			}
		}

		return nil
	})
}

// Close closes the underlying bbolt database.
func (s *Store) Close() error {
	return s.db.Close()
}

// DB returns the underlying *bolt.DB.
func (s *Store) DB() *bolt.DB {
	return s.db
}

// Path returns the filesystem path of the database file.
func (s *Store) Path() string {
	return s.path
}

// GetMeta reads a value from the Meta bucket.
func (s *Store) GetMeta(key string) (string, error) {
	var value string
	err := s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketMeta)
		if b == nil {
			return fmt.Errorf("Meta bucket not found")
		}
		v := b.Get([]byte(key))
		if v != nil {
			value = string(v)
		}
		return nil
	})
	return value, err
}

// SetMeta writes a value to the Meta bucket.
func (s *Store) SetMeta(key, value string) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		b := tx.Bucket(bucketMeta)
		if b == nil {
			return fmt.Errorf("Meta bucket not found")
		}
		return b.Put([]byte(key), []byte(value))
	})
}

// schemaBucket navigates to Schemas -> {schemaID}.
// If create is true the bucket is created if absent; otherwise an error is
// returned when it does not exist.
func schemaBucket(tx *bolt.Tx, schemaID string, create bool) (*bolt.Bucket, error) {
	schemas := tx.Bucket(bucketSchemas)
	if schemas == nil {
		return nil, fmt.Errorf("Schemas bucket not found")
	}
	key := []byte(schemaID)
	if create {
		b, err := schemas.CreateBucketIfNotExists(key)
		if err != nil {
			return nil, fmt.Errorf("create schema bucket %q: %w", schemaID, err)
		}
		return b, nil
	}
	b := schemas.Bucket(key)
	if b == nil {
		return nil, fmt.Errorf("schema bucket %q not found", schemaID)
	}
	return b, nil
}

// schemaSubBucket navigates to Schemas -> {schemaID} -> {sub}.
func schemaSubBucket(tx *bolt.Tx, schemaID, sub string, create bool) (*bolt.Bucket, error) {
	parent, err := schemaBucket(tx, schemaID, create)
	if err != nil {
		return nil, err
	}
	key := []byte(sub)
	if create {
		b, err := parent.CreateBucketIfNotExists(key)
		if err != nil {
			return nil, fmt.Errorf("create sub-bucket %q/%q: %w", schemaID, sub, err)
		}
		return b, nil
	}
	b := parent.Bucket(key)
	if b == nil {
		return nil, fmt.Errorf("sub-bucket %q/%q not found", schemaID, sub)
	}
	return b, nil
}
