package pinyin

// syllableTrieNode 音节 Trie 节点
type syllableTrieNode struct {
	children map[byte]*syllableTrieNode
	isEnd    bool
}

// SyllableTrie 音节 Trie，用于快速判断拼音音节边界
// 包含约 400 个合法拼音音节
type SyllableTrie struct {
	root *syllableTrieNode
}

// NewSyllableTrie 创建并初始化音节 Trie
func NewSyllableTrie() *SyllableTrie {
	st := &SyllableTrie{
		root: &syllableTrieNode{
			children: make(map[byte]*syllableTrieNode),
		},
	}
	st.build()
	return st
}

// allSyllables 所有合法拼音音节（约 400 个）
var allSyllables = []string{
	// 零声母
	"a", "o", "e", "ai", "ei", "ao", "ou", "an", "en", "ang", "eng", "er",

	// b
	"ba", "bo", "bi", "bu", "bai", "bei", "bao", "ban", "ben", "bin",
	"bang", "beng", "bing", "bie", "biao", "bian",

	// p
	"pa", "po", "pi", "pu", "pai", "pei", "pao", "pou", "pan", "pen", "pin",
	"pang", "peng", "ping", "pie", "piao", "pian",

	// m
	"ma", "mo", "me", "mi", "mu", "mai", "mei", "mao", "mou", "man", "men", "min",
	"mang", "meng", "ming", "mie", "miao", "mian", "miu",

	// f
	"fa", "fo", "fu", "fei", "fou", "fan", "fen", "fang", "feng",

	// d
	"da", "de", "di", "du", "dai", "dei", "dao", "dou", "dan", "den",
	"dang", "deng", "ding", "die", "diao", "dian", "diu",
	"duo", "dui", "duan", "dun", "dong",

	// t
	"ta", "te", "ti", "tu", "tai", "tei", "tao", "tou", "tan",
	"tang", "teng", "ting", "tie", "tiao", "tian",
	"tuo", "tui", "tuan", "tun", "tong",

	// n
	"na", "ne", "ni", "nu", "nv", "nai", "nei", "nao", "nou", "nan", "nen", "nin",
	"nang", "neng", "ning", "nie", "niao", "nian", "niang", "niu",
	"nuo", "nuan", "nun", "nong", "nve", "nue",

	// l
	"la", "le", "li", "lu", "lv", "lai", "lei", "lao", "lou", "lan", "lin",
	"lang", "leng", "ling", "lie", "liao", "lian", "liang", "liu",
	"luo", "luan", "lun", "long", "lve", "lue",

	// g
	"ga", "ge", "gu", "gai", "gei", "gao", "gou", "gan", "gen",
	"gang", "geng", "gua", "guo", "gui", "guai", "guan", "gun", "guang", "gong",

	// k
	"ka", "ke", "ku", "kai", "kei", "kao", "kou", "kan", "ken",
	"kang", "keng", "kua", "kuo", "kui", "kuai", "kuan", "kun", "kuang", "kong",

	// h
	"ha", "he", "hu", "hai", "hei", "hao", "hou", "han", "hen",
	"hang", "heng", "hua", "huo", "hui", "huai", "huan", "hun", "huang", "hong",

	// j
	"ji", "ju", "jia", "jie", "jiao", "jiu", "jian", "jin", "jiang", "jing",
	"jiong", "jue", "jun", "juan",

	// q
	"qi", "qu", "qia", "qie", "qiao", "qiu", "qian", "qin", "qiang", "qing",
	"qiong", "que", "qun", "quan",

	// x
	"xi", "xu", "xia", "xie", "xiao", "xiu", "xian", "xin", "xiang", "xing",
	"xiong", "xue", "xun", "xuan",

	// zh
	"zha", "zhe", "zhi", "zhu", "zhai", "zhei", "zhao", "zhou",
	"zhan", "zhen", "zhang", "zheng",
	"zhua", "zhuo", "zhui", "zhuai", "zhuan", "zhun", "zhuang", "zhong",

	// ch
	"cha", "che", "chi", "chu", "chai", "chao", "chou",
	"chan", "chen", "chang", "cheng",
	"chua", "chuo", "chui", "chuai", "chuan", "chun", "chuang", "chong",

	// sh
	"sha", "she", "shi", "shu", "shai", "shei", "shao", "shou",
	"shan", "shen", "shang", "sheng",
	"shua", "shuo", "shui", "shuai", "shuan", "shun", "shuang",

	// r
	"ri", "re", "ru", "rao", "rou", "ran", "ren", "rang", "reng",
	"rua", "ruo", "rui", "ruan", "run", "rong",

	// z
	"za", "ze", "zi", "zu", "zai", "zei", "zao", "zou", "zan", "zen",
	"zang", "zeng", "zuo", "zui", "zuan", "zun", "zong",

	// c
	"ca", "ce", "ci", "cu", "cai", "cao", "cou", "can", "cen",
	"cang", "ceng", "cuo", "cui", "cuan", "cun", "cong",

	// s
	"sa", "se", "si", "su", "sai", "sao", "sou", "san", "sen",
	"sang", "seng", "suo", "sui", "suan", "sun", "song",

	// y
	"ya", "ye", "yi", "yo", "yu", "yao", "you", "yan", "yin",
	"yang", "ying", "yue", "yuan", "yun", "yong",

	// w
	"wa", "wo", "wu", "wai", "wei", "wan", "wen", "wang", "weng",
}

// build 构建音节 Trie
func (st *SyllableTrie) build() {
	for _, s := range allSyllables {
		st.insert(s)
	}
}

// insert 向 Trie 中插入一个音节
func (st *SyllableTrie) insert(syllable string) {
	node := st.root
	for i := 0; i < len(syllable); i++ {
		c := syllable[i]
		if node.children == nil {
			node.children = make(map[byte]*syllableTrieNode)
		}
		child, ok := node.children[c]
		if !ok {
			child = &syllableTrieNode{
				children: make(map[byte]*syllableTrieNode),
			}
			node.children[c] = child
		}
		node = child
	}
	node.isEnd = true
}

// MatchAt 从 input 的 pos 位置开始，匹配所有可能的音节
// 返回所有匹配到的音节（从长到短排序）
func (st *SyllableTrie) MatchAt(input string, pos int) []string {
	var matches []string
	node := st.root

	for i := pos; i < len(input); i++ {
		c := input[i]
		child, ok := node.children[c]
		if !ok {
			break
		}
		node = child
		if node.isEnd {
			matches = append(matches, input[pos:i+1])
		}
	}

	// 从长到短排序（最长匹配优先）
	for i, j := 0, len(matches)-1; i < j; i, j = i+1, j-1 {
		matches[i], matches[j] = matches[j], matches[i]
	}

	return matches
}

// Contains 检查是否包含指定音节
func (st *SyllableTrie) Contains(syllable string) bool {
	node := st.root
	for i := 0; i < len(syllable); i++ {
		child, ok := node.children[syllable[i]]
		if !ok {
			return false
		}
		node = child
	}
	return node.isEnd
}

// HasPrefix 检查是否有以 prefix 开头的音节
func (st *SyllableTrie) HasPrefix(prefix string) bool {
	node := st.root
	for i := 0; i < len(prefix); i++ {
		child, ok := node.children[prefix[i]]
		if !ok {
			return false
		}
		node = child
	}
	return true
}

// MatchPrefixAt 从 input 的 pos 位置开始，匹配前缀并返回可能的续写
// 返回:
//   - prefix: 匹配到的前缀（即使不是完整音节）
//   - isComplete: 该前缀是否为完整音节
//   - possible: 可能的续写后缀列表（能使前缀成为完整音节的后缀）
//
// 例如:
//   - MatchPrefixAt("zhang", 0) -> ("zhang", true, [])
//   - MatchPrefixAt("zh", 0) -> ("zh", false, ["a", "ai", "an", "ang", "ao", "e", "ei", "en", "eng", "i", "o", "ong", "ou", "u", ...])
//   - MatchPrefixAt("xyz", 0) -> ("", false, [])
func (st *SyllableTrie) MatchPrefixAt(input string, pos int) (prefix string, isComplete bool, possible []string) {
	if pos >= len(input) {
		return "", false, nil
	}

	node := st.root
	endPos := pos

	// 沿着 Trie 走，直到无法继续
	for i := pos; i < len(input); i++ {
		c := input[i]
		child, ok := node.children[c]
		if !ok {
			break
		}
		node = child
		endPos = i + 1
	}

	// 如果没有匹配任何字符
	if endPos == pos {
		return "", false, nil
	}

	prefix = input[pos:endPos]
	isComplete = node.isEnd

	// 收集所有可能的续写后缀
	possible = st.collectCompletions(node, "")

	return prefix, isComplete, possible
}

// collectCompletions 从当前节点收集所有可能的续写后缀
// 如果当前节点已是完整音节，则包含空字符串 ""
func (st *SyllableTrie) collectCompletions(node *syllableTrieNode, currentSuffix string) []string {
	var results []string

	// 如果当前节点是完整音节的结尾，添加当前后缀
	if node.isEnd && currentSuffix != "" {
		results = append(results, currentSuffix)
	}

	// 递归收集子节点
	for c, child := range node.children {
		childResults := st.collectCompletions(child, currentSuffix+string(c))
		results = append(results, childResults...)
	}

	return results
}

// GetPossibleSyllables 获取以 prefix 开头的所有完整音节
// 例如: GetPossibleSyllables("zh") -> ["zha", "zhai", "zhan", "zhang", "zhao", ...]
func (st *SyllableTrie) GetPossibleSyllables(prefix string) []string {
	node := st.root

	// 找到前缀对应的节点
	for i := 0; i < len(prefix); i++ {
		child, ok := node.children[prefix[i]]
		if !ok {
			return nil
		}
		node = child
	}

	// 收集所有完整音节
	var results []string
	st.collectFullSyllables(node, prefix, &results)
	return results
}

// collectFullSyllables 从当前节点收集所有完整音节
func (st *SyllableTrie) collectFullSyllables(node *syllableTrieNode, current string, results *[]string) {
	if node.isEnd {
		*results = append(*results, current)
	}

	for c, child := range node.children {
		st.collectFullSyllables(child, current+string(c), results)
	}
}
