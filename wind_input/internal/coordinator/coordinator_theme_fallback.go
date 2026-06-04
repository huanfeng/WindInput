package coordinator

import "fmt"

// notifyThemeFallbackIfAny 若上次 LoadTheme 因主题不合法（非 v3 / 解析失败）而自动回退了默认主题，
// 弹 Toast 告知用户该主题不受支持已回退（v3 一刀切、不兼容旧主题，用户决策 2026-06-04）。
// 经 ConsumeFallbackNotice 一次性读取信号；无回退则静默。
func (c *Coordinator) notifyThemeFallbackIfAny() {
	if from := c.uiManager.ConsumeThemeFallbackNotice(); from != "" {
		c.uiManager.ShowToastError("主题不兼容", fmt.Sprintf("主题「%s」格式不受支持，已回退默认主题", from))
	}
}
