package perf

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestRingBufferOverwrites(t *testing.T) {
	Clear()
	SetCapacity(4)
	for i := 0; i < 10; i++ {
		Record(Sample{Total: time.Duration(i) * time.Millisecond, InputLen: i})
	}
	got := Snapshot()
	if len(got) != 4 {
		t.Fatalf("snapshot len=%d want 4", len(got))
	}
	// 最旧→最新：6,7,8,9
	want := []int{6, 7, 8, 9}
	for i, s := range got {
		if s.InputLen != want[i] {
			t.Errorf("idx %d: got InputLen=%d want %d", i, s.InputLen, want[i])
		}
	}
}

func TestComputeStatsSplitsFirstAndContinuation(t *testing.T) {
	Clear()
	SetCapacity(64)
	Record(Sample{Total: 100 * time.Millisecond, FirstKey: true})
	Record(Sample{Total: 5 * time.Millisecond, FirstKey: false})
	Record(Sample{Total: 7 * time.Millisecond, FirstKey: false})
	stats := ComputeStats()
	if stats.First.Count != 1 || stats.First.Avg != 100*time.Millisecond {
		t.Errorf("first stats unexpected: %+v", stats.First)
	}
	if stats.Continuation.Count != 2 {
		t.Errorf("cont count=%d want 2", stats.Continuation.Count)
	}
	if stats.All.Count != 3 {
		t.Errorf("all count=%d want 3", stats.All.Count)
	}
}

func TestExportJSONLWritesHeaderAndSamples(t *testing.T) {
	Clear()
	SetCapacity(8)
	Record(Sample{Total: 3 * time.Millisecond})
	Record(Sample{Total: 4 * time.Millisecond})

	path := filepath.Join(t.TempDir(), "perf.jsonl")
	n, err := ExportJSONL(path)
	if err != nil {
		t.Fatal(err)
	}
	if n != 2 {
		t.Errorf("exported %d want 2", n)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	// header + 2 samples = 3 lines
	lines := 0
	for _, b := range data {
		if b == '\n' {
			lines++
		}
	}
	if lines != 3 {
		t.Errorf("got %d lines want 3", lines)
	}
}
