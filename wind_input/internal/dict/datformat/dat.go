package datformat

import "sort"

// DAT Double-Array Trie 结构体
// base[s] + c = t 为状态 s 经字符 c 的转移目标
// check[t] == s 验证转移合法
// base[t] < 0 时，-base[t]-1 为叶节点关联的数据索引
// 字符串结束用 code=0 表示
type DAT struct {
	Base  []int32
	Check []int32
	Size  int
}

// ExactMatch 精确匹配 key，返回对应的数据索引和是否找到
func (d *DAT) ExactMatch(key string) (leafIndex uint32, found bool) {
	if d.Size == 0 {
		return 0, false
	}
	s := int32(0)
	for i := 0; i < len(key); i++ {
		c := int32(key[i])
		t := d.Base[s] + c
		if t < 0 || int(t) >= d.Size || d.Check[t] != s {
			return 0, false
		}
		s = t
	}
	// 尝试终止符转移（code=0）
	t := d.Base[s] + 0
	if t < 0 || int(t) >= d.Size || d.Check[t] != s {
		return 0, false
	}
	leaf := t
	if d.Base[leaf] >= 0 {
		return 0, false
	}
	return uint32(-d.Base[leaf] - 1), true
}

// trieNode 内部 trie 节点
type trieNode struct {
	children  map[byte]*trieNode
	dataIndex uint32
	isEnd     bool
}

// DATBuilder 构建 Double-Array Trie
type DATBuilder struct {
	root *trieNode
}

// NewDATBuilder 创建新的 Builder
func NewDATBuilder() *DATBuilder {
	return &DATBuilder{
		root: &trieNode{children: make(map[byte]*trieNode)},
	}
}

// Add 向 Builder 中添加一个 key 及其数据索引
func (b *DATBuilder) Add(key string, dataIndex uint32) {
	node := b.root
	for i := 0; i < len(key); i++ {
		c := key[i]
		child, ok := node.children[c]
		if !ok {
			child = &trieNode{children: make(map[byte]*trieNode)}
			node.children[c] = child
		}
		node = child
	}
	node.isEnd = true
	node.dataIndex = dataIndex
}

// Build 将内部 trie 转换为 Double-Array 格式
func (b *DATBuilder) Build() (*DAT, error) {
	// 初始容量
	capacity := 256
	base := make([]int32, capacity)
	check := make([]int32, capacity)
	for i := range check {
		check[i] = -1
	}

	// 确保容量足够
	grow := func(need int) {
		for need >= len(base) {
			newCap := len(base) * 2
			newBase := make([]int32, newCap)
			newCheck := make([]int32, newCap)
			copy(newBase, base)
			copy(newCheck, check)
			for i := len(base); i < newCap; i++ {
				newCheck[i] = -1
			}
			base = newBase
			check = newCheck
		}
	}

	// findBase 为给定的子字符集（codes）找到一个合法的 base 值
	// 要求 base+c 位置的 check 均为 -1（空闲）
	findBase := func(codes []int32) int32 {
		if len(codes) == 0 {
			return 1
		}
		// 从 1 开始搜索，避免 base=0 导致根节点冲突
		for b := int32(1); ; b++ {
			conflict := false
			for _, c := range codes {
				pos := b + c
				grow(int(pos))
				if check[pos] != -1 {
					conflict = true
					break
				}
			}
			if !conflict {
				return b
			}
		}
	}

	// 根节点占用位置 0，check[0] 设为 0 表示已占用（自指）
	check[0] = 0

	// BFS 构建 Double-Array
	type queueItem struct {
		node  *trieNode
		state int32
	}
	queue := []queueItem{{b.root, 0}}

	for len(queue) > 0 {
		item := queue[0]
		queue = queue[1:]
		node := item.node
		s := item.state

		// 收集所有子字符 code（包含终止符 0 如果是叶节点）
		codes := make([]int32, 0, len(node.children)+1)
		if node.isEnd {
			codes = append(codes, 0) // 终止符
		}
		childBytes := make([]byte, 0, len(node.children))
		for c := range node.children {
			childBytes = append(childBytes, c)
		}
		sort.Slice(childBytes, func(i, j int) bool { return childBytes[i] < childBytes[j] })
		for _, c := range childBytes {
			codes = append(codes, int32(c))
		}

		if len(codes) == 0 {
			continue
		}

		bv := findBase(codes)
		base[s] = bv

		// 分配各子节点
		for _, c := range codes {
			t := bv + c
			grow(int(t))
			check[t] = s

			if c == 0 {
				// 终止符叶节点：base 编码 dataIndex
				base[t] = -int32(node.dataIndex) - 1
			} else {
				// 内部节点：入队
				child := node.children[byte(c)]
				queue = append(queue, queueItem{child, t})
			}
		}
	}

	// 裁剪尾部空闲
	size := len(base)
	for size > 1 && check[size-1] == -1 {
		size--
	}

	return &DAT{
		Base:  base[:size],
		Check: check[:size],
		Size:  size,
	}, nil
}

// walkPrefix 从根沿 prefix 路径走到目标节点，返回状态索引
func (d *DAT) walkPrefix(prefix string) (state int, found bool) {
	if d.Size == 0 {
		return 0, false
	}
	s := int32(0)
	for i := 0; i < len(prefix); i++ {
		c := int32(prefix[i])
		t := d.Base[s] + c
		if t < 0 || int(t) >= d.Size || d.Check[t] != s {
			return 0, false
		}
		s = t
	}
	return int(s), true
}

// collectLeaves DFS 收集叶节点的 dataIndex
func (d *DAT) collectLeaves(state int, results *[]uint32, limit int) {
	if limit > 0 && len(*results) >= limit {
		return
	}
	s := int32(state)
	// 检查终止符（c=0）
	t := d.Base[s] + 0
	if t >= 0 && int(t) < d.Size && d.Check[t] == s && d.Base[t] < 0 {
		*results = append(*results, uint32(-d.Base[t]-1))
		if limit > 0 && len(*results) >= limit {
			return
		}
	}
	// 遍历子字符 1-255
	for c := int32(1); c <= 255; c++ {
		t := d.Base[s] + c
		if t < 0 || int(t) >= d.Size || d.Check[t] != s {
			continue
		}
		d.collectLeaves(int(t), results, limit)
		if limit > 0 && len(*results) >= limit {
			return
		}
	}
}

// PrefixCollect 收集前缀下所有叶节点的 dataIndex，limit=0 不限制
func (d *DAT) PrefixCollect(prefix string, limit int) []uint32 {
	state, found := d.walkPrefix(prefix)
	if !found {
		return nil
	}
	var results []uint32
	d.collectLeaves(state, &results, limit)
	return results
}

// datStackFrame 游标栈帧
type datStackFrame struct {
	state int // 当前节点状态
	nextC int // 下一个要遍历的字符 (0=终止标记, 1-255)
}

// DATCursor 前缀遍历游标，支持暂停/恢复
type DATCursor struct {
	dat       *DAT
	stack     []datStackFrame
	exhausted bool
}

// PrefixCursor 创建前缀遍历游标
func (d *DAT) PrefixCursor(prefix string) *DATCursor {
	state, found := d.walkPrefix(prefix)
	if !found {
		return &DATCursor{dat: d, exhausted: true}
	}
	return &DATCursor{
		dat:   d,
		stack: []datStackFrame{{state: state, nextC: 0}},
	}
}

// Next 取下一批 n 个叶节点 dataIndex
func (c *DATCursor) Next(n int) []uint32 {
	if c.exhausted {
		return nil
	}
	var results []uint32
	for len(c.stack) > 0 && len(results) < n {
		top := &c.stack[len(c.stack)-1]
		s := int32(top.state)
		advanced := false
		for top.nextC <= 255 {
			ch := int32(top.nextC)
			top.nextC++
			t := c.dat.Base[s] + ch
			if t < 0 || int(t) >= c.dat.Size || c.dat.Check[t] != s {
				continue
			}
			if ch == 0 {
				// 终止符叶节点
				if c.dat.Base[t] < 0 {
					results = append(results, uint32(-c.dat.Base[t]-1))
					if len(results) >= n {
						return results
					}
				}
			} else {
				// 内部子节点：压栈继续深入
				c.stack = append(c.stack, datStackFrame{state: int(t), nextC: 0})
				advanced = true
				break
			}
		}
		if !advanced && top.nextC > 255 {
			// 当前节点所有子字符遍历完，弹栈
			c.stack = c.stack[:len(c.stack)-1]
		}
	}
	if len(c.stack) == 0 {
		c.exhausted = true
	}
	return results
}

// HasMore 是否还有更多
func (c *DATCursor) HasMore() bool {
	return !c.exhausted
}

// Close 释放资源
func (c *DATCursor) Close() {
	c.stack = nil
	c.exhausted = true
}
