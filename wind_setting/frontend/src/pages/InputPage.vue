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
          <label>候选检索范围</label>
          <p class="setting-hint">过滤候选词中的生僻字</p>
        </div>
        <div class="setting-control">
          <div class="filter-dropdown" ref="filterDropdownRef">
            <button
              class="filter-select"
              type="button"
              @click="filterDropdownOpen = !filterDropdownOpen"
            >
              <span class="filter-select-label">{{
                currentFilterOption.label
              }}</span>
              <span v-if="currentFilterOption.tag" class="filter-select-tag">{{
                currentFilterOption.tag
              }}</span>
              <span class="filter-select-arrow">&#9662;</span>
            </button>
            <div v-if="filterDropdownOpen" class="filter-menu">
              <div
                v-for="opt in filterModeOptions"
                :key="opt.value"
                class="filter-option"
                :class="{ selected: formData.input.filter_mode === opt.value }"
                @click="selectFilterMode(opt.value)"
              >
                <div class="filter-option-main">
                  <span class="filter-option-name">{{ opt.label }}</span>
                  <span v-if="opt.tag" class="filter-option-tag">{{
                    opt.tag
                  }}</span>
                </div>
                <div class="filter-option-desc">{{ opt.desc }}</div>
              </div>
            </div>
          </div>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>标点随中英文切换</label>
          <p class="setting-hint">切换到中文模式时自动切换中文标点</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input type="checkbox" v-model="formData.input.punct_follow_mode" />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>数字后智能标点</label>
          <p class="setting-hint">
            数字后句号输出点号、逗号输出英文逗号，方便输入 IP、小数、千分位等
          </p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.input.smart_punct_after_digit"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
    </div>

    <!-- 默认状态 -->
    <div class="settings-card">
      <div class="card-title">默认状态</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>记忆前次状态</label>
          <p class="setting-hint">启用后恢复上次的中英文、全半角和标点状态</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.startup.remember_last_state"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': formData.startup.remember_last_state }"
      >
        <div class="setting-info">
          <label>初始语言模式</label>
          <p class="setting-hint">每次激活输入法时的默认语言</p>
        </div>
        <div class="setting-control">
          <div class="segmented-control">
            <button
              :class="{ active: formData.startup.default_chinese_mode }"
              @click="formData.startup.default_chinese_mode = true"
              :disabled="formData.startup.remember_last_state"
            >
              中文
            </button>
            <button
              :class="{ active: !formData.startup.default_chinese_mode }"
              @click="formData.startup.default_chinese_mode = false"
              :disabled="formData.startup.remember_last_state"
            >
              英文
            </button>
          </div>
        </div>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': formData.startup.remember_last_state }"
      >
        <div class="setting-info">
          <label>初始字符宽度</label>
          <p class="setting-hint">每次激活输入法时的默认字符宽度</p>
        </div>
        <div class="setting-control">
          <div class="segmented-control">
            <button
              :class="{ active: !formData.startup.default_full_width }"
              @click="formData.startup.default_full_width = false"
              :disabled="formData.startup.remember_last_state"
            >
              半角
            </button>
            <button
              :class="{ active: formData.startup.default_full_width }"
              @click="formData.startup.default_full_width = true"
              :disabled="formData.startup.remember_last_state"
            >
              全角
            </button>
          </div>
        </div>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': formData.startup.remember_last_state }"
      >
        <div class="setting-info">
          <label>初始标点模式</label>
          <p class="setting-hint">每次激活输入法时的默认标点类型</p>
        </div>
        <div class="setting-control">
          <div class="segmented-control">
            <button
              :class="{ active: formData.startup.default_chinese_punct }"
              @click="formData.startup.default_chinese_punct = true"
              :disabled="formData.startup.remember_last_state"
            >
              中文标点
            </button>
            <button
              :class="{ active: !formData.startup.default_chinese_punct }"
              @click="formData.startup.default_chinese_punct = false"
              :disabled="formData.startup.remember_last_state"
            >
              英文标点
            </button>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import type { Config } from "../api/settings";

const props = defineProps<{
  formData: Config;
}>();

const filterDropdownOpen = ref(false);
const filterDropdownRef = ref<HTMLElement | null>(null);

const filterModeOptions = [
  {
    value: "smart",
    label: "智能模式",
    desc: "优先常用字，无结果时自动扩展到全部字符",
    tag: "推荐",
  },
  {
    value: "general",
    label: "仅常用字",
    desc: "只显示通用规范汉字表中的常用汉字",
  },
  {
    value: "gb18030",
    label: "全部字符",
    desc: "不限制字符范围，包含生僻字",
  },
];

const currentFilterOption = computed(
  () =>
    filterModeOptions.find(
      (o) => o.value === props.formData.input.filter_mode,
    ) || filterModeOptions[0],
);

function selectFilterMode(value: string) {
  props.formData.input.filter_mode = value;
  filterDropdownOpen.value = false;
}

function handleDocumentClick(event: MouseEvent) {
  if (
    filterDropdownRef.value &&
    !filterDropdownRef.value.contains(event.target as Node)
  ) {
    filterDropdownOpen.value = false;
  }
}

onMounted(() => {
  document.addEventListener("click", handleDocumentClick);
});

onUnmounted(() => {
  document.removeEventListener("click", handleDocumentClick);
});
</script>

<style scoped>
.filter-dropdown {
  position: relative;
}
.filter-select {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 12px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  background: #fff;
  cursor: pointer;
  font-size: 13px;
  color: #1f2937;
  transition:
    border-color 0.15s,
    box-shadow 0.15s;
  min-width: 160px;
}
.filter-select:hover {
  border-color: #9ca3af;
}
.filter-select:focus {
  outline: none;
  border-color: #2563eb;
  box-shadow: 0 0 0 2px rgba(37, 99, 235, 0.15);
}
.filter-select-label {
  flex: 1;
  text-align: left;
}
.filter-select-tag {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 3px;
  background: #dcfce7;
  color: #166534;
  font-weight: 500;
}
.filter-select-arrow {
  color: #6b7280;
  font-size: 11px;
}
.filter-menu {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 10;
  background: #fff;
  border: 1px solid #e5e7eb;
  border-radius: 10px;
  box-shadow: 0 10px 30px rgba(15, 23, 42, 0.08);
  min-width: 280px;
  padding: 6px;
}
.filter-option {
  padding: 10px 12px;
  border-radius: 8px;
  cursor: pointer;
  transition: background-color 0.15s;
}
.filter-option:hover {
  background-color: #f3f4f6;
}
.filter-option.selected {
  background-color: #eff6ff;
}
.filter-option-main {
  display: flex;
  align-items: center;
  gap: 8px;
}
.filter-option-name {
  font-size: 13px;
  font-weight: 500;
  color: #1f2937;
}
.filter-option-tag {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 3px;
  background: #dcfce7;
  color: #166534;
  font-weight: 500;
}
.filter-option-desc {
  font-size: 12px;
  color: #9ca3af;
  margin-top: 3px;
}
</style>
