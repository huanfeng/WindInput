// z_decision_test.go — z 键混合模式回退决策的核心用例.
//
// 覆盖 zHybridFallback 的 8 种典型场景, 这是 fix(temp-pinyin) 渐进决策修复
// 的回归测试. 用例命中条件:
//   - 是否处于 z 触发模式
//   - inputBuffer 是否以 z 开头
//   - inputBuffer+新键 是否还能扩展出码表/短语前缀
package coordinator

import "testing"

func TestZHybridFallback(t *testing.T) {
	type entry struct{ code, text string }
	cases := []struct {
		name        string
		zTrigger    bool    // 是否启用 z 触发 (withZHybridSchema)
		zRepeat     bool    // 仅在 zTrigger=true 时有意义
		entries     []entry // stub 码表条目
		inputBuffer string
		key         string

		wantOk     bool
		wantBuffer string
	}{
		{
			// 1. 未启用 z 触发 → 不切
			name:        "no_z_trigger",
			zTrigger:    false,
			entries:     []entry{{"zhang", "张"}},
			inputBuffer: "z",
			key:         "h",
			wantOk:      false,
		},
		{
			// 2. inputBuffer 不以 z 开头 → 不切
			name:        "buffer_not_z_prefixed",
			zTrigger:    true,
			zRepeat:     true,
			entries:     []entry{{"abc", "X"}},
			inputBuffer: "abc",
			key:         "d",
			wantOk:      false,
		},
		{
			// 3. inputBuffer="z" + 新键 = "zh", 码表里有 zhang → 仍有前缀 → 不切
			name:        "single_z_extendable",
			zTrigger:    true,
			zRepeat:     true,
			entries:     []entry{{"zhang", "张"}},
			inputBuffer: "z",
			key:         "h",
			wantOk:      false,
		},
		{
			// 4. inputBuffer="z" + 新键 = "zx", 无 zx 前缀 → 切, buffer="x"
			name:        "single_z_no_prefix",
			zTrigger:    true,
			zRepeat:     true,
			entries:     []entry{{"zhang", "张"}},
			inputBuffer: "z",
			key:         "x",
			wantOk:      true,
			wantBuffer:  "x",
		},
		{
			// 5. 核心 bug 场景: inputBuffer="zz", 有 zz 短语前缀,
			//    新键 'h' 后 "zzh" 无匹配 → 切, buffer="zh"
			name:        "zz_prefix_then_pinyin",
			zTrigger:    true,
			zRepeat:     true,
			entries:     []entry{{"zz", "占位短语"}},
			inputBuffer: "zz",
			key:         "h",
			wantOk:      true,
			wantBuffer:  "zh",
		},
		{
			// 6. inputBuffer="zz" + 新键 'b' 仍能扩展 (zzb 是 zzbd 前缀) → 不切
			name:        "zz_prefix_still_extendable",
			zTrigger:    true,
			zRepeat:     true,
			entries:     []entry{{"zzbd", "标点"}},
			inputBuffer: "zz",
			key:         "b",
			wantOk:      false,
		},
		{
			// 7. inputBuffer="zr" (zrq 短语前缀), 新键 'a' 后 "zra" 无匹配 →
			//    切, buffer="ra"
			name:        "zr_prefix_pinyin_fallback",
			zTrigger:    true,
			zRepeat:     true,
			entries:     []entry{{"zrq", "今天日期"}},
			inputBuffer: "zr",
			key:         "a",
			wantOk:      true,
			wantBuffer:  "ra",
		},
		{
			// 8. 仅 z 触发 (无 z-repeat), 行为应与 z-repeat 模式一致
			name:        "z_trigger_only_no_repeat",
			zTrigger:    true,
			zRepeat:     false,
			entries:     []entry{{"zz", "占位"}},
			inputBuffer: "zz",
			key:         "h",
			wantOk:      true,
			wantBuffer:  "zh",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			engineOpts := make([]engineOption, 0, len(tc.entries))
			for _, e := range tc.entries {
				engineOpts = append(engineOpts, withCodetableEntry(e.code, e.text))
			}
			opts := []testOption{withEngineMgr(engineOpts...)}
			if tc.zTrigger {
				opts = append(opts, withZHybridSchema(tc.zRepeat))
			}

			h := newTestCoordinator(t, opts...)
			h.inputBuffer = tc.inputBuffer

			gotBuf, gotOk := h.zHybridFallback(tc.key)
			if gotOk != tc.wantOk {
				t.Errorf("ok = %v, want %v", gotOk, tc.wantOk)
			}
			if gotBuf != tc.wantBuffer {
				t.Errorf("pinyinBuffer = %q, want %q", gotBuf, tc.wantBuffer)
			}
		})
	}
}
