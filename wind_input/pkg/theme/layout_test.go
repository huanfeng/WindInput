package theme

import "testing"

// TestBuildIndexLabelsFromSlots 验证序号槽位拼接：满槽 / 部分回退默认 / 空槽回退 / 超 10 截断。
func TestBuildIndexLabelsFromSlots(t *testing.T) {
	cases := []struct {
		name   string
		labels []string
		want   string
	}{
		{
			name:   "full emoji set",
			labels: []string{"🍎", "🍊", "🍇", "🍉", "🍓", "🍑", "🍒", "🥝", "🍍", "🥥"},
			want:   "🍎/🍊/🍇/🍉/🍓/🍑/🍒/🥝/🍍/🥥",
		},
		{
			name:   "partial fills rest with default digits",
			labels: []string{"壹", "贰", "叁"},
			want:   "壹/贰/叁/4/5/6/7/8/9/0",
		},
		{
			name:   "empty slot falls back to default digit",
			labels: []string{"A", "", "C"},
			want:   "A/2/C/4/5/6/7/8/9/0",
		},
		{
			name:   "more than 10 ignores extras",
			labels: []string{"1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "x", "y"},
			want:   "1/2/3/4/5/6/7/8/9/0",
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := BuildIndexLabelsFromSlots(c.labels); got != c.want {
				t.Errorf("got %q, want %q", got, c.want)
			}
		})
	}
}
