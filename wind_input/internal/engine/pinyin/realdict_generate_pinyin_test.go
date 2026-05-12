package pinyin

import (
	"testing"
)

// TestRealDict_GenerateWordPinyin_MultiPron 用真实词典验证多音字词的拼音生成。
//
// 分类：
//   - 强断言：常用度差距悬殊的词（重庆/银行/倔强等），整词命中应稳定生效，
//     测试失败说明 P1 整词消歧路径出了问题。
//   - 软断言：长词靠子词切分继承读音，列出来用于人工 review；
//     当前实现未必命中（依赖真实词典里有对应子词整词），用 t.Logf 而非 t.Errorf。
//   - 信息记录：剩余的常见多音字词，仅记录结果，便于发现真实词典里的坏 case。
//
// 测试在 CI / 无真实词典环境下会被跳过（getRealDictDir 用 t.Skip）。
func TestRealDict_GenerateWordPinyin_MultiPron(t *testing.T) {
	engine := loadRealEngine(t)

	// 必须正确：常用读音权重远高于其他读音
	mustCases := []struct {
		text string
		code string
	}{
		{"重庆", "chongqing"},
		{"银行", "yinhang"},
		{"倔强", "juejiang"},
		{"重点", "zhongdian"},
		{"重新", "chongxin"},
		{"长江", "changjiang"},
		{"行为", "xingwei"},
		{"中国", "zhongguo"},
		{"你好", "nihao"},
	}

	t.Run("strict_assertions", func(t *testing.T) {
		for _, tc := range mustCases {
			got := engine.GenerateWordPinyin(tc.text)
			if got != tc.code {
				t.Errorf("GenerateWordPinyin(%q) = %q, want %q", tc.text, got, tc.code)
			} else {
				t.Logf("✓ %s → %s", tc.text, got)
			}
		}
	})

	// 子词切分继承：靠 inferBySubwordSegmentation 路径
	// 软断言：列出来便于 review，但若真实词典中"长江"/"三角洲"等子词存在并被正确推断，应能通过
	subwordCases := []struct {
		text     string
		expected string
		comment  string
	}{
		{"长江三角洲", "changjiangsanjiaozhou", "长江+三角洲"},
		{"重庆市", "chongqingshi", "重庆+市"},
		{"中华人民共和国", "zhonghuarenmingongheguo", "中华+人民+共和国 或 中华人民共和国 整词"},
		{"银行卡", "yinhangka", "银行+卡"},
	}

	t.Run("subword_inheritance", func(t *testing.T) {
		for _, tc := range subwordCases {
			got := engine.GenerateWordPinyin(tc.text)
			marker := "✓"
			if got != tc.expected {
				marker = "✗"
			}
			t.Logf("%s %s → %s (want %s, %s)", marker, tc.text, got, tc.expected, tc.comment)
		}
	})

	// 信息记录：复盘真实词典在常见多音字词上的实际表现
	// 这些不做断言（不同词典权重分布不同），打印结果用于人工审查
	informationCases := []string{
		"朝阳", "朝代", "朝鲜",
		"还款", "还有", "还是",
		"差别", "差点", "出差",
		"行长", "行业", "外行",
		"长大", "长度", "长辈",
		"重要", "重复", "尊重",
		"转变", "转身", "运转",
		"分子", "分明", "成分",
		"答应", "回答",
		"曲折", "歌曲", "弯曲",
		"乐意", "音乐", "快乐",
		"了解", "完了",
		"几乎", "几个",
		"假期", "请假", "假装",
		"散步", "散开", "松散",
		"种类", "种植", "种子",
	}

	t.Run("information_only", func(t *testing.T) {
		for _, text := range informationCases {
			got := engine.GenerateWordPinyin(text)
			if got == "" {
				t.Logf("? %s → <empty>", text)
			} else {
				t.Logf("  %s → %s", text, got)
			}
		}
	})
}
