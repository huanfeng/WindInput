package dict

import (
	"sort"

	"github.com/huanfeng/wind_input/internal/candidate"
)

// TrieNode Trie 树节点
type TrieNode struct {
	children   map[byte]*TrieNode
	candidates []candidate.Candidate
	isEnd      bool
}

// Trie 前缀树
type Trie struct {
	root       *TrieNode
	entryCount int
}

// NewTrie 创建空的 Trie 树
func NewTrie() *Trie {
	return &Trie{
		root: &TrieNode{
			children: make(map[byte]*TrieNode),
		},
	}
}

// Insert 插入一个词条
func (t *Trie) Insert(code string, cand candidate.Candidate) {
	node := t.root
	for i := 0; i < len(code); i++ {
		c := code[i]
		if node.children == nil {
			node.children = make(map[byte]*TrieNode)
		}
		child, ok := node.children[c]
		if !ok {
			child = &TrieNode{
				children: make(map[byte]*TrieNode),
			}
			node.children[c] = child
		}
		node = child
	}
	node.isEnd = true
	node.candidates = append(node.candidates, cand)
	t.entryCount++
}

// Search 精确查找
func (t *Trie) Search(code string) []candidate.Candidate {
	node := t.findNode(code)
	if node == nil || !node.isEnd {
		return nil
	}
	return node.candidates
}

// SearchPrefix 前缀查找，返回所有以 prefix 为前缀的词条。
//
// 跨子树叶节点权重无序，按 DFS 字典序提前 break 会盲选导致召回退化。
// limit > 0 时遍历整棵子树并用 min-heap top-K 选取最优；limit == 0 时
// 完整收集后排序，保留"无限制"语义。
func (t *Trie) SearchPrefix(prefix string, limit int) []candidate.Candidate {
	node := t.findNode(prefix)
	if node == nil {
		return nil
	}

	if limit > 0 {
		picker := newMemTopKPicker(limit)
		t.streamAll(node, picker.offer)
		return picker.sorted()
	}

	var results []candidate.Candidate
	t.collectAll(node, &results, 0)
	sort.SliceStable(results, func(i, j int) bool {
		return candidate.Better(results[i], results[j])
	})
	return results
}

// HasPrefix 检查是否有以 prefix 开头的词条
func (t *Trie) HasPrefix(prefix string) bool {
	return t.findNode(prefix) != nil
}

// EntryCount 返回总词条数
func (t *Trie) EntryCount() int {
	return t.entryCount
}

// findNode 沿 code 路径查找节点
func (t *Trie) findNode(code string) *TrieNode {
	node := t.root
	for i := 0; i < len(code); i++ {
		child, ok := node.children[code[i]]
		if !ok {
			return nil
		}
		node = child
	}
	return node
}

// streamAll 遍历节点及其所有子节点的候选词，逐个传给 fn。
// 不应用任何 limit，由调用方自行裁剪（top-K picker 等）。
func (t *Trie) streamAll(node *TrieNode, fn func(candidate.Candidate)) {
	if node.isEnd {
		for _, c := range node.candidates {
			fn(c)
		}
	}
	for _, child := range node.children {
		t.streamAll(child, fn)
	}
}

// collectAll 收集节点及其所有子节点的候选词
func (t *Trie) collectAll(node *TrieNode, results *[]candidate.Candidate, limit int) {
	if limit > 0 && len(*results) >= limit {
		return
	}
	if node.isEnd {
		*results = append(*results, node.candidates...)
	}
	keys := make([]byte, 0, len(node.children))
	for k := range node.children {
		keys = append(keys, k)
	}
	sort.Slice(keys, func(i, j int) bool { return keys[i] < keys[j] })
	for _, k := range keys {
		if limit > 0 && len(*results) >= limit {
			return
		}
		child := node.children[k]
		t.collectAll(child, results, limit)
	}
}
