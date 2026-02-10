<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import type { Config, EngineInfo } from "../api/settings";

const props = defineProps<{
  formData: Config;
  engines: EngineInfo[];
}>();

const emit = defineEmits<{
  switchEngine: [type: string];
}>();

const inputModeSelectOpen = ref(false);
const inputModeDropdownRef = ref<HTMLElement | null>(null);

const inputModeOptions = computed(() => {
  const base =
    props.engines.length > 0
      ? props.engines.map((engine) => ({
          value: engine.type,
          label: engine.displayName,
          description: engine.description,
          disabled: false,
        }))
      : [
          {
            value: "wubi",
            label: "五笔输入",
            description: "86版五笔",
            disabled: false,
          },
          {
            value: "pinyin",
            label: "拼音输入",
            description: "全拼输入法",
            disabled: false,
          },
        ];

  const preferredOrder = ["wubi", "pinyin"];
  const ordered = [...base].sort((a, b) => {
    const ai = preferredOrder.indexOf(a.value);
    const bi = preferredOrder.indexOf(b.value);
    if (ai === -1 && bi === -1)
      return a.label.localeCompare(b.label, "zh-Hans-CN");
    if (ai === -1) return 1;
    if (bi === -1) return -1;
    return ai - bi;
  });

  return [
    ...ordered,
    {
      value: "shuangpin",
      label: "双拼输入",
      description: "开发中",
      disabled: true,
    },
    {
      value: "mixed",
      label: "混合输入",
      description: "开发中",
      disabled: true,
    },
  ];
});

const currentInputMode = computed(() => {
  return inputModeOptions.value.find(
    (option) => option.value === props.formData.engine.type,
  );
});

function onInputModeSelect(mode: string) {
  props.formData.engine.type = mode;
  // Update engines isActive flags
  props.engines.forEach((engine) => {
    engine.isActive = engine.type === mode;
  });
  inputModeSelectOpen.value = false;
  emit("switchEngine", mode);
}

function handleDocumentClick(event: MouseEvent) {
  if (
    inputModeDropdownRef.value &&
    !inputModeDropdownRef.value.contains(event.target as Node)
  ) {
    inputModeSelectOpen.value = false;
  }
}

onMounted(() => {
  document.addEventListener("click", handleDocumentClick);
});

onUnmounted(() => {
  document.removeEventListener("click", handleDocumentClick);
});
</script>

<template>
  <section class="section">
    <div class="section-header">
      <h2>常用设置</h2>
      <p class="section-desc">最基本的输入法设置</p>
    </div>

    <!-- 输入模式 -->
    <div class="settings-card">
      <div class="card-title">输入模式</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>输入方案</label>
          <p class="setting-hint">
            {{ currentInputMode?.description || "选择适合的输入方案" }}
          </p>
        </div>
        <div class="setting-control">
          <div
            class="theme-dropdown input-mode-dropdown"
            ref="inputModeDropdownRef"
          >
            <button
              class="theme-select select-strong"
              type="button"
              @click="inputModeSelectOpen = !inputModeSelectOpen"
            >
              <div class="theme-select-main">
                <div class="theme-select-title">
                  {{ currentInputMode?.label || "选择输入方案" }}
                </div>
                <div class="theme-select-sub">
                  <span>{{
                    currentInputMode?.description || "暂无描述"
                  }}</span>
                </div>
              </div>
              <span class="theme-select-arrow">&#9662;</span>
            </button>
            <div v-if="inputModeSelectOpen" class="theme-options">
              <button
                v-for="option in inputModeOptions"
                :key="option.value"
                type="button"
                class="theme-option"
                :class="{ selected: formData.engine.type === option.value }"
                :disabled="option.disabled"
                @click="!option.disabled && onInputModeSelect(option.value)"
              >
                <div class="theme-option-title">
                  <span class="theme-option-name">{{ option.label }}</span>
                  <span v-if="option.disabled" class="theme-badge builtin"
                    >开发中</span
                  >
                </div>
                <div class="theme-option-sub">
                  <span>{{ option.description }}</span>
                </div>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 默认/启动状态 -->
    <div class="settings-card">
      <div class="card-title">默认状态</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>记忆前次状态</label>
          <p class="setting-hint">
            启用后将使用上次退出时的状态，忽略以下默认设置
          </p>
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
