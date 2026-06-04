package theme

import "strings"

// BuildIndexLabelsFromSlots 把序号槽位 []string 拼成 "/" 分隔串（候选窗 IndexLabels）。
// 槽位 0→候选序号 1、…、槽位 9→第 10 个候选；不足 10 或空槽回退默认数字（1..9,0）。
// 约束：单个标签不应含 '/'（渲染器以 '/' 切分槽位），此处不做转义。
func BuildIndexLabelsFromSlots(labels []string) string {
	digits := []string{"1", "2", "3", "4", "5", "6", "7", "8", "9", "0"}
	parts := make([]string, 10)
	for i := range 10 {
		if i < len(labels) && labels[i] != "" {
			parts[i] = labels[i]
		} else {
			parts[i] = digits[i]
		}
	}
	return strings.Join(parts, "/")
}

// Padding 通用四边内边距（逻辑像素）。
//
// V3-D：原 layout/density 几何机制（LayoutSchema/Raw*Layout/*Layout/density 基线）已随
// P8 几何 View 化删除——其它窗口几何走 views 节点或 internal/ui 内置常量。本类型作为
// margin/padding/nine_slice 切片的统一边距表示保留（被 views.Fill.Slice、bgimage、
// internal/ui 的 Edges 别名等复用）。
type Padding struct {
	Top    int `yaml:"top" json:"top"`
	Right  int `yaml:"right" json:"right"`
	Bottom int `yaml:"bottom" json:"bottom"`
	Left   int `yaml:"left" json:"left"`
}
