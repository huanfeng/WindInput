<template>
  <section class="section section-full">
    <div class="section-header">
      <h2>高级设置</h2>
      <p class="section-desc">故障排查、调试与测试工具</p>
    </div>

    <div class="advanced-tabs">
      <button class="advanced-tab" :class="{ active: advancedSubTab === 'advanced' }" @click="advancedSubTab = 'advanced'">高级</button>
      <button class="advanced-tab" :class="{ active: advancedSubTab === 'test' }" @click="advancedSubTab = 'test'">测试</button>
    </div>

    <template v-if="advancedSubTab === 'advanced'">
      <div class="settings-card">
        <div class="card-title">日志设置</div>
        <div class="setting-item">
          <div class="setting-info">
            <label>日志级别</label>
            <p class="setting-hint">更改日志级别需要重启服务才能生效</p>
          </div>
          <div class="setting-control">
            <select v-model="formData.advanced.log_level" class="select">
              <option value="debug">Debug（调试）</option>
              <option value="info">Info（信息）</option>
              <option value="warn">Warn（警告）</option>
              <option value="error">Error（错误）</option>
            </select>
          </div>
        </div>
        <div class="setting-item">
          <div class="setting-info">
            <label>日志文件位置</label>
            <p class="setting-hint">{{ logPath }}</p>
          </div>
          <div class="setting-control">
            <button class="btn btn-sm" @click="$emit('openLogFolder')">打开文件夹</button>
          </div>
        </div>
      </div>
    </template>

    <template v-else>
      <div class="settings-card">
        <div class="card-title">码表测试</div>
        <div class="test-options">
          <div class="test-option">
            <label>引擎</label>
            <select v-model="testEngine" class="select select-sm">
              <option value="current">当前引擎</option>
              <option value="pinyin">拼音</option>
              <option value="wubi">五笔</option>
            </select>
          </div>
          <div class="test-option">
            <label>过滤</label>
            <select v-model="testFilterMode" class="select select-sm">
              <option value="current">当前设置</option>
              <option value="smart">智能模式</option>
              <option value="general">仅通用字</option>
              <option value="gb18030">全部字符</option>
            </select>
          </div>
        </div>
        <div class="test-input-wrap">
          <input type="text" v-model="testInput" class="test-input" placeholder="输入编码进行测试..." @keydown.enter.prevent />
          <span v-if="testLoading" class="test-loading">加载中...</span>
        </div>
        <div class="test-candidates-wrap" v-if="testCandidates.length > 0">
          <div class="test-candidates">
            <div class="test-candidate" v-for="(cand, idx) in testCandidates" :key="idx">
              <span class="cand-index">{{ idx + 1 }}.</span>
              <span class="cand-text">{{ cand.text }}</span>
              <span class="cand-code" v-if="cand.code">{{ cand.code }}</span>
              <span class="cand-common" v-if="cand.isCommon">通用</span>
              <span class="cand-rare" v-else>生僻</span>
            </div>
          </div>
        </div>
        <div class="test-empty" v-else-if="testInput && !testLoading">无匹配候选词</div>
      </div>
    </template>
  </section>
</template>

<script setup lang="ts">
import { ref, watch } from "vue";
import * as api from "../api/settings";
import type { Config } from "../api/settings";

const props = defineProps<{
  formData: Config;
  isWailsEnv: boolean;
}>();

const emit = defineEmits<{
  openLogFolder: [];
}>();

const advancedSubTab = ref<"advanced" | "test">("advanced");

const testInput = ref("");
const testCandidates = ref<any[]>([]);
const testEngine = ref("current");
const testFilterMode = ref("current");
const testLoading = ref(false);

const logPath = "%LOCALAPPDATA%\\WindInput\\logs\\";

async function handleTestInput() {
  if (!testInput.value.trim()) {
    testCandidates.value = [];
    return;
  }
  testLoading.value = true;
  try {
    const res = await api.testConvert(
      testInput.value,
      testEngine.value,
      testFilterMode.value,
    );
    if (res.success && res.data) {
      testCandidates.value = res.data.candidates || [];
    }
  } catch (e) {
    console.error("测试失败", e);
  } finally {
    testLoading.value = false;
  }
}

watch(testInput, handleTestInput);
watch([testEngine, testFilterMode], () => {
  if (testInput.value) handleTestInput();
});
</script>

<style scoped>
.advanced-tabs {
  display: flex;
  gap: 4px;
  margin-bottom: 16px;
  border-bottom: 1px solid #e5e7eb;
}
.advanced-tab {
  padding: 10px 18px;
  border: none;
  background: none;
  color: #6b7280;
  cursor: pointer;
  border-bottom: 2px solid transparent;
  transition: all 0.15s;
  font-size: 14px;
}
.advanced-tab:hover {
  color: #374151;
}
.advanced-tab.active {
  color: #2563eb;
  border-bottom-color: #2563eb;
  font-weight: 600;
}
.test-options {
  display: flex;
  gap: 16px;
  margin-bottom: 16px;
  flex-wrap: wrap;
}
.test-option {
  display: flex;
  align-items: center;
  gap: 8px;
}
.test-option label {
  font-size: 13px;
  color: #6b7280;
}
.test-input-wrap {
  position: relative;
  margin-bottom: 16px;
}
.test-input {
  width: 100%;
  padding: 12px 16px;
  border: 2px solid #e5e7eb;
  border-radius: 8px;
  font-size: 16px;
  color: #1f2937;
}
.test-input:focus {
  outline: none;
  border-color: #2563eb;
}
.test-loading {
  position: absolute;
  right: 16px;
  top: 50%;
  transform: translateY(-50%);
  font-size: 12px;
  color: #9ca3af;
}
.test-candidates-wrap {
  max-height: 400px;
  overflow-y: auto;
}
.test-candidates {
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  overflow: hidden;
}
.test-candidate {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 16px;
  border-bottom: 1px solid #f3f4f6;
}
.test-candidate:last-child {
  border-bottom: none;
}
.test-candidate:hover {
  background: #f9fafb;
}
.cand-index {
  color: #9ca3af;
  font-size: 13px;
  min-width: 24px;
  flex-shrink: 0;
}
.cand-text {
  font-size: 16px;
  color: #1f2937;
  word-break: break-all;
}
.cand-code {
  font-size: 12px;
  color: #6b7280;
  background: #f3f4f6;
  padding: 2px 6px;
  border-radius: 4px;
  flex-shrink: 0;
}
.cand-common {
  font-size: 11px;
  color: #059669;
  background: #d1fae5;
  padding: 2px 6px;
  border-radius: 4px;
  flex-shrink: 0;
}
.cand-rare {
  font-size: 11px;
  color: #dc2626;
  background: #fee2e2;
  padding: 2px 6px;
  border-radius: 4px;
  flex-shrink: 0;
}
.test-empty {
  text-align: center;
  padding: 32px;
  color: #9ca3af;
}
</style>
