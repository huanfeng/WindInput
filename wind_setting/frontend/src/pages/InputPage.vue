<template>
  <section class="section">
    <div class="section-header">
      <h2>输入习惯</h2>
      <p class="section-desc">定制您的打字体验</p>
    </div>

    <!-- 字符与标点 -->
    <div class="settings-card">
      <div class="card-title">字符与标点</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>候选字符范围</label>
          <p class="setting-hint">控制候选词中显示的字符范围</p>
        </div>
        <div class="setting-control">
          <select v-model="formData.engine.filter_mode" class="select">
            <option value="smart">智能模式（推荐）</option>
            <option value="general">仅常用字</option>
            <option value="gb18030">大字符集</option>
          </select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>标点随中英文切换</label>
          <p class="setting-hint">切换到英文模式时自动切换英文标点</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input type="checkbox" v-model="formData.input.punct_follow_mode" />
            <span class="slider"></span>
          </label>
        </div>
      </div>
    </div>

    <!-- 五笔设置 -->
    <div class="settings-card">
      <div class="card-title">五笔设置</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>四码唯一时自动上屏</label>
          <p class="setting-hint">输入满四码且只有一个候选时，自动提交首选</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.engine.wubi.auto_commit_at_4"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>四码为空时清空</label>
          <p class="setting-hint">输入满四码但无候选时，自动清空编码</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.engine.wubi.clear_on_empty_at_4"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>五码顶字</label>
          <p class="setting-hint">输入第五码时自动上屏首选</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.engine.wubi.top_code_commit"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>标点顶字</label>
          <p class="setting-hint">输入标点时自动上屏首选</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.engine.wubi.punct_commit"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>逐字键入</label>
          <p class="setting-hint">仅显示精确匹配候选，关闭逐码前缀匹配</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.engine.wubi.single_code_input"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>编码提示</label>
          <p class="setting-hint">在逐码候选后显示剩余编码，帮助学习全码</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.engine.wubi.show_code_hint"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>候选排序</label>
          <p class="setting-hint">控制候选词的排列顺序</p>
        </div>
        <div class="setting-control">
          <select
            v-model="formData.engine.wubi.candidate_sort_mode"
            class="select"
          >
            <option value="frequency">词频排序</option>
            <option value="natural">自然顺序</option>
          </select>
        </div>
      </div>
    </div>

    <!-- 拼音设置 -->
    <div class="settings-card">
      <div class="card-title">拼音设置</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>五笔反查提示</label>
          <p class="setting-hint">在候选词旁显示对应的五笔编码</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.engine.pinyin.show_wubi_hint"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>模糊音</label>
          <p class="setting-hint">
            允许使用近似发音输入（已启用 {{ fuzzyEnabledCount }} 组）
          </p>
        </div>
        <div class="setting-control inline-controls">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.engine.pinyin.fuzzy.enabled"
            />
            <span class="slider"></span>
          </label>
          <button class="btn btn-sm" @click="showFuzzyDialog = true">
            配置
          </button>
        </div>
      </div>
    </div>

    <!-- 模糊音配置对话框 -->
    <div
      class="fuzzy-dialog-overlay"
      v-if="showFuzzyDialog"
      @click.self="showFuzzyDialog = false"
    >
      <div class="fuzzy-dialog">
        <div class="fuzzy-dialog-header">
          <h3>模糊音配置</h3>
          <button class="fuzzy-dialog-close" @click="showFuzzyDialog = false">
            &times;
          </button>
        </div>
        <div class="fuzzy-dialog-body">
          <div class="fuzzy-pairs-grid">
            <label
              class="fuzzy-pair-item"
              v-for="pair in fuzzyPairs"
              :key="pair.field"
            >
              <input
                type="checkbox"
                v-model="formData.engine.pinyin.fuzzy[pair.field]"
              />
              <span>{{ pair.label }}</span>
            </label>
          </div>
        </div>
        <div class="fuzzy-dialog-footer">
          <button class="btn btn-sm" @click="setAllFuzzyPairs(true)">
            全选
          </button>
          <button class="btn btn-sm" @click="setAllFuzzyPairs(false)">
            全不选
          </button>
          <button
            class="btn btn-sm btn-primary"
            @click="showFuzzyDialog = false"
          >
            确定
          </button>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref, computed } from "vue";
import type { Config } from "../api/settings";

const props = defineProps<{
  formData: Config;
}>();

const showFuzzyDialog = ref(false);
const fuzzyPairs = [
  { field: "zh_z", label: "zh ↔ z" },
  { field: "ch_c", label: "ch ↔ c" },
  { field: "sh_s", label: "sh ↔ s" },
  { field: "n_l", label: "n ↔ l" },
  { field: "f_h", label: "f ↔ h" },
  { field: "r_l", label: "r ↔ l" },
  { field: "an_ang", label: "an ↔ ang" },
  { field: "en_eng", label: "en ↔ eng" },
  { field: "in_ing", label: "in ↔ ing" },
  { field: "ian_iang", label: "ian ↔ iang" },
  { field: "uan_uang", label: "uan ↔ uang" },
] as const;

const fuzzyEnabledCount = computed(() => {
  if (!props.formData?.engine?.pinyin?.fuzzy) return 0;
  const f = props.formData.engine.pinyin.fuzzy as any;
  return fuzzyPairs.filter((p) => f[p.field]).length;
});

function setAllFuzzyPairs(value: boolean) {
  if (!props.formData?.engine?.pinyin?.fuzzy) return;
  const f = props.formData.engine.pinyin.fuzzy as any;
  for (const pair of fuzzyPairs) {
    f[pair.field] = value;
  }
}
</script>

<style scoped>
.inline-controls {
  display: flex;
  align-items: center;
  gap: 10px;
}
.fuzzy-dialog-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(0, 0, 0, 0.4);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}
.fuzzy-dialog {
  background: #fff;
  color: #1f2937;
  border-radius: 12px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.15);
  width: 380px;
  max-width: 90vw;
}
.fuzzy-dialog-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 20px 12px;
  border-bottom: 1px solid #e5e7eb;
}
.fuzzy-dialog-header h3 {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
  color: #111827;
}
.fuzzy-dialog-close {
  background: none;
  border: none;
  font-size: 20px;
  color: #6b7280;
  cursor: pointer;
  padding: 0 4px;
  line-height: 1;
}
.fuzzy-dialog-close:hover {
  color: #111827;
}
.fuzzy-dialog-body {
  padding: 16px 20px;
}
.fuzzy-pairs-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 8px 16px;
}
.fuzzy-pair-item {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  font-size: 13px;
  padding: 6px 8px;
  border-radius: 6px;
  transition: background-color 0.15s;
}
.fuzzy-pair-item:hover {
  background-color: #f3f4f6;
}
.fuzzy-pair-item input {
  width: 16px;
  height: 16px;
  cursor: pointer;
  accent-color: #2563eb;
  flex-shrink: 0;
}
.fuzzy-dialog-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 20px 16px;
  border-top: 1px solid #e5e7eb;
}
</style>
