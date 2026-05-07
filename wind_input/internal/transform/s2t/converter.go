package s2t

import (
	"container/list"
	"sync"
)

// Converter 串行执行多个步骤；每个步骤是一组词典（"group"），扫描输入时在
// 所有 group 成员上各取最长前缀匹配，跨成员选最长一个（OpenCC 语义）。
// Converter 本身的查询路径只读多步词典，可被多协程并发调用。
type Converter struct {
	steps [][]*Dict

	cacheMu  sync.Mutex
	cacheCap int
	cache    map[string]*list.Element // key = 输入字符串
	lru      *list.List               // front = 最近使用
}

type cacheEntry struct {
	in  string
	out string
}

// NewConverter 用按链路顺序排列的若干步骤（每个步骤为一组词典）构造转换器。
// cacheCap=0 表示不缓存。
func NewConverter(steps [][]*Dict, cacheCap int) *Converter {
	c := &Converter{
		steps:    steps,
		cacheCap: cacheCap,
	}
	if cacheCap > 0 {
		c.cache = make(map[string]*list.Element, cacheCap)
		c.lru = list.New()
	}
	return c
}

// Convert 执行一次完整链路转换。空字符串直接返回。
func (c *Converter) Convert(s string) string {
	if s == "" || len(c.steps) == 0 {
		return s
	}
	// 缓存（仅对不太长的字符串生效，避免大串污染缓存）
	const cacheMaxBytes = 64
	canCache := c.cacheCap > 0 && len(s) <= cacheMaxBytes
	if canCache {
		if v, ok := c.cacheGet(s); ok {
			return v
		}
	}

	cur := []byte(s)
	for _, group := range c.steps {
		cur = applyStep(group, cur)
	}
	out := string(cur)

	if canCache {
		c.cachePut(s, out)
	}
	return out
}

// ResetCache 清空内部缓存（变体切换/词典释放时调用）。
func (c *Converter) ResetCache() {
	if c.cacheCap == 0 {
		return
	}
	c.cacheMu.Lock()
	defer c.cacheMu.Unlock()
	c.cache = make(map[string]*list.Element, c.cacheCap)
	c.lru = list.New()
}

func (c *Converter) cacheGet(k string) (string, bool) {
	c.cacheMu.Lock()
	defer c.cacheMu.Unlock()
	if c.cache == nil {
		return "", false
	}
	if elem, ok := c.cache[k]; ok {
		c.lru.MoveToFront(elem)
		return elem.Value.(*cacheEntry).out, true
	}
	return "", false
}

func (c *Converter) cachePut(k, v string) {
	c.cacheMu.Lock()
	defer c.cacheMu.Unlock()
	if c.cache == nil {
		return
	}
	if elem, ok := c.cache[k]; ok {
		elem.Value.(*cacheEntry).out = v
		c.lru.MoveToFront(elem)
		return
	}
	elem := c.lru.PushFront(&cacheEntry{in: k, out: v})
	c.cache[k] = elem
	for c.lru.Len() > c.cacheCap {
		tail := c.lru.Back()
		if tail == nil {
			break
		}
		ent := tail.Value.(*cacheEntry)
		delete(c.cache, ent.in)
		c.lru.Remove(tail)
	}
}

// applyStep 用一组词典做最长前缀匹配 + 替换：扫描输入时在所有 group 成员上各取
// 最长匹配，跨成员选最长一个（OpenCC 语义）。空 group 直接返回输入。
func applyStep(group []*Dict, in []byte) []byte {
	if len(group) == 0 || len(in) == 0 {
		return in
	}
	// 先扫一遍确认是否有命中；无命中直接返回避免分配。
	hit := false
	for i := 0; i < len(in); {
		n, _, ok := groupLongestPrefix(group, in[i:])
		if ok {
			hit = true
			break
		}
		_ = n
		i += utf8Step(in[i])
	}
	if !hit {
		return in
	}

	out := make([]byte, 0, len(in)+8)
	for i := 0; i < len(in); {
		n, val, ok := groupLongestPrefix(group, in[i:])
		if ok {
			out = append(out, val...)
			i += n
			continue
		}
		step := utf8Step(in[i])
		out = append(out, in[i:i+step]...)
		i += step
	}
	return out
}

// groupLongestPrefix 在 group 的多个词典上各做最长前缀匹配，跨成员取最长。
func groupLongestPrefix(group []*Dict, input []byte) (int, []byte, bool) {
	bestLen := 0
	var bestVal []byte
	found := false
	for _, d := range group {
		if d == nil {
			continue
		}
		n, val, ok := d.LongestPrefix(input)
		if !ok {
			continue
		}
		if n > bestLen {
			bestLen = n
			bestVal = val
			found = true
		}
	}
	return bestLen, bestVal, found
}

// utf8Step 根据首字节判断当前 UTF-8 字符的字节数。
func utf8Step(b byte) int {
	switch {
	case b < 0x80:
		return 1
	case b < 0xC0:
		// 错误的中间字节，按 1 跳过避免死循环
		return 1
	case b < 0xE0:
		return 2
	case b < 0xF0:
		return 3
	default:
		return 4
	}
}
