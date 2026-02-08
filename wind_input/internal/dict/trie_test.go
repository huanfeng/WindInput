package dict

import (
	"testing"

	"github.com/huanfeng/wind_input/internal/candidate"
)

func TestTrieInsertAndSearch(t *testing.T) {
	trie := NewTrie()

	trie.Insert("ni", candidate.Candidate{Text: "你", Weight: 100})
	trie.Insert("ni", candidate.Candidate{Text: "泥", Weight: 50})
	trie.Insert("nihao", candidate.Candidate{Text: "你好", Weight: 200})
	trie.Insert("nimen", candidate.Candidate{Text: "你们", Weight: 150})
	trie.Insert("hao", candidate.Candidate{Text: "好", Weight: 100})

	// 精确查找
	results := trie.Search("ni")
	if len(results) != 2 {
		t.Errorf("Search(ni) = %d条, want 2", len(results))
	}

	results = trie.Search("nihao")
	if len(results) != 1 || results[0].Text != "你好" {
		t.Errorf("Search(nihao) 结果不正确")
	}

	results = trie.Search("nih")
	if len(results) != 0 {
		t.Errorf("Search(nih) 应返回空, 得到 %d条", len(results))
	}
}

func TestTrieSearchPrefix(t *testing.T) {
	trie := NewTrie()

	trie.Insert("ni", candidate.Candidate{Text: "你", Weight: 100})
	trie.Insert("nihao", candidate.Candidate{Text: "你好", Weight: 200})
	trie.Insert("nimen", candidate.Candidate{Text: "你们", Weight: 150})
	trie.Insert("hao", candidate.Candidate{Text: "好", Weight: 100})

	// 前缀查找
	results := trie.SearchPrefix("ni", 0)
	if len(results) != 3 {
		t.Errorf("SearchPrefix(ni) = %d条, want 3", len(results))
	}

	// 带 limit
	results = trie.SearchPrefix("ni", 2)
	if len(results) != 2 {
		t.Errorf("SearchPrefix(ni, 2) = %d条, want 2", len(results))
	}
	// 应按权重降序
	if results[0].Weight < results[1].Weight {
		t.Errorf("SearchPrefix 结果未按权重排序")
	}

	// 完整匹配也是前缀
	results = trie.SearchPrefix("nihao", 0)
	if len(results) != 1 {
		t.Errorf("SearchPrefix(nihao) = %d条, want 1", len(results))
	}
}

func TestTrieSearchPrefixDeterministicWithLimit(t *testing.T) {
	trie := NewTrie()

	// 构造同权重候选，验证 limit 场景下结果稳定（不受 map 迭代顺序影响）。
	trie.Insert("sa", candidate.Candidate{Text: "司", Code: "sa", Weight: 100})
	trie.Insert("sb", candidate.Candidate{Text: "法", Code: "sb", Weight: 100})
	trie.Insert("sc", candidate.Candidate{Text: "官", Code: "sc", Weight: 100})

	first := trie.SearchPrefix("s", 2)
	if len(first) != 2 {
		t.Fatalf("first SearchPrefix(s, 2) = %d条, want 2", len(first))
	}

	for i := 0; i < 10; i++ {
		got := trie.SearchPrefix("s", 2)
		if len(got) != 2 {
			t.Fatalf("SearchPrefix(s, 2) = %d条, want 2", len(got))
		}
		if got[0].Text != first[0].Text || got[1].Text != first[1].Text {
			t.Fatalf("SearchPrefix(s, 2) not deterministic: first=%v got=%v",
				[]string{first[0].Text, first[1].Text},
				[]string{got[0].Text, got[1].Text},
			)
		}
	}
}

func TestTrieHasPrefix(t *testing.T) {
	trie := NewTrie()

	trie.Insert("nihao", candidate.Candidate{Text: "你好", Weight: 200})

	if !trie.HasPrefix("n") {
		t.Error("HasPrefix(n) = false, want true")
	}
	if !trie.HasPrefix("ni") {
		t.Error("HasPrefix(ni) = false, want true")
	}
	if !trie.HasPrefix("nih") {
		t.Error("HasPrefix(nih) = false, want true")
	}
	if trie.HasPrefix("x") {
		t.Error("HasPrefix(x) = true, want false")
	}
}

func TestTrieEntryCount(t *testing.T) {
	trie := NewTrie()

	trie.Insert("a", candidate.Candidate{Text: "啊"})
	trie.Insert("a", candidate.Candidate{Text: "阿"})
	trie.Insert("ni", candidate.Candidate{Text: "你"})

	if trie.EntryCount() != 3 {
		t.Errorf("EntryCount() = %d, want 3", trie.EntryCount())
	}
}
