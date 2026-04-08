package transform

// PairEntry 记录一次配对插入
type PairEntry struct {
	Left  rune
	Right rune
}

// PairTracker 追踪自动配对状态，用于智能跳过和智能删除
type PairTracker struct {
	stack    []PairEntry
	pairMap  map[rune]rune // 左→右
	rightSet map[rune]bool // 右标点集合
}

// NewPairTracker 创建配对追踪器，pairs 格式为 [["（","）"], ...]
func NewPairTracker(pairs [][]string) *PairTracker {
	pt := &PairTracker{}
	pt.buildMaps(pairs)
	return pt
}

func (pt *PairTracker) buildMaps(pairs [][]string) {
	pt.pairMap = make(map[rune]rune, len(pairs))
	pt.rightSet = make(map[rune]bool, len(pairs))
	for _, p := range pairs {
		if len(p) != 2 {
			continue
		}
		left := []rune(p[0])
		right := []rune(p[1])
		if len(left) != 1 || len(right) != 1 {
			continue
		}
		pt.pairMap[left[0]] = right[0]
		pt.rightSet[right[0]] = true
	}
}

// UpdatePairs 更新配对映射（配置热更新时调用），同时清空栈
func (pt *PairTracker) UpdatePairs(pairs [][]string) {
	pt.buildMaps(pairs)
	pt.stack = nil
}

// IsLeft 判断是否为左标点
func (pt *PairTracker) IsLeft(r rune) bool {
	_, ok := pt.pairMap[r]
	return ok
}

// IsRight 判断是否为右标点
func (pt *PairTracker) IsRight(r rune) bool {
	return pt.rightSet[r]
}

// GetRight 获取左标点对应的右标点
func (pt *PairTracker) GetRight(left rune) (rune, bool) {
	r, ok := pt.pairMap[left]
	return r, ok
}

// Push 记录一次配对插入
func (pt *PairTracker) Push(left, right rune) {
	pt.stack = append(pt.stack, PairEntry{Left: left, Right: right})
}

// Peek 查看栈顶
func (pt *PairTracker) Peek() (PairEntry, bool) {
	if len(pt.stack) == 0 {
		return PairEntry{}, false
	}
	return pt.stack[len(pt.stack)-1], true
}

// Pop 弹出栈顶
func (pt *PairTracker) Pop() (PairEntry, bool) {
	if len(pt.stack) == 0 {
		return PairEntry{}, false
	}
	entry := pt.stack[len(pt.stack)-1]
	pt.stack = pt.stack[:len(pt.stack)-1]
	return entry, true
}

// Clear 清空栈
func (pt *PairTracker) Clear() {
	pt.stack = nil
}

// IsEmpty 判断栈是否为空
func (pt *PairTracker) IsEmpty() bool {
	return len(pt.stack) == 0
}
