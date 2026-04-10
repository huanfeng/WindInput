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

    <!-- 标点配对 -->
    <div class="settings-card">
      <div class="card-title">标点配对</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>中文标点自动配对</label>
          <p class="setting-hint">
            输入左括号类标点时自动补全右标点（已启用
            {{ getEnabledPairCount("chinese") }} 组）
          </p>
        </div>
        <div class="setting-control inline-control">
          <label class="checkbox-label">
            <input type="checkbox" v-model="formData.input.auto_pair.chinese" />
            启用
          </label>
          <button
            class="btn btn-sm"
            :disabled="!formData.input.auto_pair.chinese"
            @click="openPairDialog('chinese')"
          >
            配置
          </button>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>英文标点自动配对</label>
          <p class="setting-hint">
            英文模式或英文标点下自动配对括号（已启用
            {{ getEnabledPairCount("english") }} 组）
          </p>
        </div>
        <div class="setting-control inline-control">
          <label class="checkbox-label">
            <input type="checkbox" v-model="formData.input.auto_pair.english" />
            启用
          </label>
          <button
            class="btn btn-sm"
            :disabled="!formData.input.auto_pair.english"
            @click="openPairDialog('english')"
          >
            配置
          </button>
        </div>
      </div>
    </div>

    <!-- 标点配对配置对话框 -->
    <div
      class="dialog-overlay"
      v-if="showPairDialog"
      @click.self="showPairDialog = false"
    >
      <div class="dialog-box dialog-sectioned">
        <div class="dialog-header">
          <h3>{{ pairDialogType === "chinese" ? "中文" : "英文" }}配对配置</h3>
          <button class="dialog-close" @click="showPairDialog = false">
            ×
          </button>
        </div>
        <div class="dialog-body">
          <div class="pair-items-grid">
            <label
              class="pair-item"
              v-for="item in currentPairOptions"
              :key="item.pair"
            >
              <input
                type="checkbox"
                :checked="isPairEnabled(item.pair)"
                @change="togglePair(item.pair)"
              />
              <span class="pair-symbol">{{ item.left }} {{ item.right }}</span>
              <span class="pair-desc">{{ item.desc }}</span>
            </label>
          </div>
        </div>
        <div class="dialog-footer">
          <button class="btn btn-sm" @click="setAllPairs(true)">全选</button>
          <button class="btn btn-sm" @click="setAllPairs(false)">全不选</button>
          <button
            class="btn btn-sm btn-primary"
            @click="showPairDialog = false"
          >
            确定
          </button>
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

// 标点配对配置
const showPairDialog = ref(false);
const pairDialogType = ref<"chinese" | "english">("chinese");

const chinesePairOptions = [
  { pair: "（）", left: "（", right: "）", desc: "圆括号" },
  { pair: "【】", left: "【", right: "】", desc: "方括号" },
  { pair: "｛｝", left: "｛", right: "｝", desc: "花括号" },
  { pair: "《》", left: "《", right: "》", desc: "书名号" },
  { pair: "〈〉", left: "〈", right: "〉", desc: "尖括号" },
  { pair: "\u2018\u2019", left: "\u2018", right: "\u2019", desc: "单引号" },
  { pair: "\u201C\u201D", left: "\u201C", right: "\u201D", desc: "双引号" },
];

const englishPairOptions = [
  { pair: "()", left: "(", right: ")", desc: "圆括号" },
  { pair: "[]", left: "[", right: "]", desc: "方括号" },
  { pair: "{}", left: "{", right: "}", desc: "花括号" },
  { pair: "''", left: "'", right: "'", desc: "单引号" },
  { pair: '""', left: '"', right: '"', desc: "双引号" },
];

const currentPairOptions = computed(() =>
  pairDialogType.value === "chinese" ? chinesePairOptions : englishPairOptions,
);

function getEnabledPairCount(type: "chinese" | "english") {
  const pairs =
    type === "chinese"
      ? props.formData.input.auto_pair.chinese_pairs
      : props.formData.input.auto_pair.english_pairs;
  return pairs ? pairs.length : 0;
}

function openPairDialog(type: "chinese" | "english") {
  pairDialogType.value = type;
  showPairDialog.value = true;
}

function isPairEnabled(pair: string) {
  const pairs =
    pairDialogType.value === "chinese"
      ? props.formData.input.auto_pair.chinese_pairs
      : props.formData.input.auto_pair.english_pairs;
  return pairs ? pairs.includes(pair) : false;
}

function togglePair(pair: string) {
  const key =
    pairDialogType.value === "chinese" ? "chinese_pairs" : "english_pairs";
  if (!props.formData.input.auto_pair[key]) {
    props.formData.input.auto_pair[key] = [];
  }
  const pairs = props.formData.input.auto_pair[key];
  const idx = pairs.indexOf(pair);
  if (idx >= 0) {
    pairs.splice(idx, 1);
  } else {
    pairs.push(pair);
  }
}

function setAllPairs(enabled: boolean) {
  const key =
    pairDialogType.value === "chinese" ? "chinese_pairs" : "english_pairs";
  const options =
    pairDialogType.value === "chinese"
      ? chinesePairOptions
      : englishPairOptions;
  if (enabled) {
    props.formData.input.auto_pair[key] = options.map((o) => o.pair);
  } else {
    props.formData.input.auto_pair[key] = [];
  }
}

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
