package transform

import "testing"

func TestPairTracker_BasicPairing(t *testing.T) {
	pt := NewPairTracker([][]string{{"（", "）"}, {"【", "】"}})

	if !pt.IsLeft('（') {
		t.Error("（ should be left")
	}
	if pt.IsLeft('）') {
		t.Error("） should not be left")
	}

	right, ok := pt.GetRight('（')
	if !ok || right != '）' {
		t.Errorf("GetRight('（') = %c, %v; want ）, true", right, ok)
	}

	if !pt.IsRight('）') {
		t.Error("） should be right")
	}
	if pt.IsRight('（') {
		t.Error("（ should not be right")
	}
}

func TestPairTracker_StackOperations(t *testing.T) {
	pt := NewPairTracker([][]string{{"（", "）"}, {"【", "】"}})

	_, ok := pt.Peek()
	if ok {
		t.Error("Peek on empty stack should return false")
	}

	pt.Push('（', '）')
	entry, ok := pt.Peek()
	if !ok || entry.Right != '）' {
		t.Error("Peek should return ）")
	}

	pt.Push('【', '】')
	entry, ok = pt.Peek()
	if !ok || entry.Right != '】' {
		t.Error("Peek should return 】 (LIFO)")
	}

	entry, ok = pt.Pop()
	if !ok || entry.Right != '】' {
		t.Error("Pop should return 】")
	}
	entry, ok = pt.Pop()
	if !ok || entry.Right != '）' {
		t.Error("Pop should return ）")
	}
	_, ok = pt.Pop()
	if ok {
		t.Error("Pop on empty stack should return false")
	}
}

func TestPairTracker_Clear(t *testing.T) {
	pt := NewPairTracker([][]string{{"（", "）"}})
	pt.Push('（', '）')
	pt.Push('（', '）')
	pt.Clear()
	_, ok := pt.Peek()
	if ok {
		t.Error("Peek after Clear should return false")
	}
}

func TestPairTracker_UpdatePairs(t *testing.T) {
	pt := NewPairTracker([][]string{{"（", "）"}})
	if !pt.IsLeft('（') {
		t.Error("（ should be left before update")
	}

	pt.UpdatePairs([][]string{{"《", "》"}})
	if pt.IsLeft('（') {
		t.Error("（ should not be left after update")
	}
	if !pt.IsLeft('《') {
		t.Error("《 should be left after update")
	}

	pt.Push('《', '》')
	pt.UpdatePairs([][]string{{"《", "》"}})
	_, ok := pt.Peek()
	if ok {
		t.Error("Stack should be cleared after UpdatePairs")
	}
}
