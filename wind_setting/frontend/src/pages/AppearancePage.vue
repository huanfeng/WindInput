<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import type { Config } from "../api/settings";
import type { ThemeInfo, ThemePreview } from "../api/wails";

const props = defineProps<{
  formData: Config;
  isWailsEnv: boolean;
  availableThemes: ThemeInfo[];
  themePreview: ThemePreview | null;
}>();

const emit = defineEmits<{
  themeSelect: [themeName: string];
}>();

const themeSelectOpen = ref(false);
const themeDropdownRef = ref<HTMLElement | null>(null);

const themeOptions = computed(() => {
  return props.availableThemes.map((theme) => ({
    name: theme.name,
    label: theme.display_name || theme.name,
    description: theme.author ? `作者 ${theme.author}` : "暂无描述",
    version: theme.version || "",
    isActive: theme.is_active,
    isBuiltin: theme.is_builtin,
  }));
});

const currentThemeOption = computed(() => {
  return themeOptions.value.find(
    (option) => option.name === props.formData.ui.theme,
  );
});

function onThemeSelect(themeName: string) {
  props.formData.ui.theme = themeName;
  emit("themeSelect", themeName);
  themeSelectOpen.value = false;
}

function handleDocumentClick(event: MouseEvent) {
  const target = event.target as Node;
  if (themeDropdownRef.value && !themeDropdownRef.value.contains(target)) {
    themeSelectOpen.value = false;
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
      <h2>外观设置</h2>
      <p class="section-desc">定制候选窗口的视觉呈现</p>
    </div>

    <!-- 主题选择 -->
    <div class="settings-card" v-if="isWailsEnv">
      <div class="card-title">主题</div>
      <div class="setting-item align-start">
        <div class="setting-info">
          <label>主题选择</label>
          <p class="setting-hint">显示主题作者与版本信息</p>
        </div>
        <div class="setting-control">
          <div class="theme-dropdown" ref="themeDropdownRef">
            <button
              class="theme-select select-strong"
              type="button"
              @click="themeSelectOpen = !themeSelectOpen"
            >
              <div class="theme-select-main">
                <div class="theme-select-title">
                  {{ currentThemeOption?.label || "选择主题" }}
                </div>
                <div class="theme-select-sub">
                  <span>{{
                    currentThemeOption?.description || "暂无描述"
                  }}</span>
                  <span
                    v-if="currentThemeOption?.version"
                    class="theme-select-version"
                    >v{{ currentThemeOption?.version }}</span
                  >
                </div>
              </div>
              <span class="theme-select-arrow">&#9662;</span>
            </button>
            <div v-if="themeSelectOpen" class="theme-options">
              <button
                v-for="theme in themeOptions"
                :key="theme.name"
                type="button"
                class="theme-option"
                :class="{ selected: formData.ui.theme === theme.name }"
                @click="onThemeSelect(theme.name)"
              >
                <div class="theme-option-title">
                  <span class="theme-option-name">{{ theme.label }}</span>
                  <span v-if="theme.isBuiltin" class="theme-badge builtin"
                    >内置</span
                  >
                  <span v-if="theme.isActive" class="theme-badge active"
                    >当前</span
                  >
                </div>
                <div class="theme-option-sub">
                  <span>{{ theme.description }}</span>
                  <span v-if="theme.version" class="theme-option-version"
                    >v{{ theme.version }}</span
                  >
                </div>
              </button>
              <div v-if="themeOptions.length === 0" class="theme-option-empty">
                暂无主题
              </div>
            </div>
          </div>
        </div>
      </div>

      <div class="setting-item align-start" v-if="themePreview">
        <div class="setting-info">
          <label>主题预览</label>
          <p class="setting-hint">候选窗口与工具栏预览</p>
        </div>
        <div class="setting-control">
          <div class="theme-preview preview-rows">
            <div class="preview-row">
              <div class="preview-row-label">候选窗口</div>
              <div
                class="preview-candidate-window"
                :style="{
                  backgroundColor:
                    themePreview.candidate_window?.background_color,
                  borderColor: themePreview.candidate_window?.border_color,
                }"
              >
                <div class="preview-candidate-item">
                  <span
                    class="preview-index"
                    :style="{
                      backgroundColor:
                        themePreview.candidate_window?.index_bg_color,
                      color: themePreview.candidate_window?.index_color,
                    }"
                    >1</span
                  >
                  <span
                    class="preview-text"
                    :style="{
                      color: themePreview.candidate_window?.text_color,
                    }"
                    >中文</span
                  >
                </div>
                <div
                  class="preview-candidate-item preview-hover"
                  :style="{
                    backgroundColor:
                      themePreview.candidate_window?.hover_bg_color,
                  }"
                >
                  <span
                    class="preview-index"
                    :style="{
                      backgroundColor:
                        themePreview.candidate_window?.index_bg_color,
                      color: themePreview.candidate_window?.index_color,
                    }"
                    >2</span
                  >
                  <span
                    class="preview-text"
                    :style="{
                      color: themePreview.candidate_window?.text_color,
                    }"
                    >输入</span
                  >
                </div>
              </div>
            </div>
            <div class="preview-row">
              <div class="preview-row-label">工具栏</div>
              <div
                class="preview-toolbar"
                :style="{
                  backgroundColor: themePreview.toolbar?.background_color,
                  borderColor: themePreview.toolbar?.border_color,
                }"
              >
                <span
                  class="preview-toolbar-item"
                  :style="{
                    backgroundColor:
                      themePreview.toolbar?.mode_chinese_bg_color,
                  }"
                  >中</span
                >
                <span
                  class="preview-toolbar-item"
                  :style="{
                    backgroundColor:
                      themePreview.toolbar?.full_width_on_bg_color,
                  }"
                  >全</span
                >
                <span
                  class="preview-toolbar-item"
                  :style="{
                    backgroundColor:
                      themePreview.toolbar?.punct_chinese_bg_color,
                  }"
                  >。</span
                >
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">候选窗口</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>字体大小</label>
          <p class="setting-hint">候选词的显示大小</p>
        </div>
        <div class="setting-control range-control">
          <input
            type="range"
            min="12"
            max="36"
            step="1"
            v-model.number="formData.ui.font_size"
          />
          <span class="range-value">{{ formData.ui.font_size }}px</span>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>每页候选数</label>
          <p class="setting-hint">每页显示的候选词数量</p>
        </div>
        <div class="setting-control range-control">
          <input
            type="range"
            min="3"
            max="9"
            step="1"
            v-model.number="formData.ui.candidates_per_page"
          />
          <span class="range-value"
            >{{ formData.ui.candidates_per_page }} 个</span
          >
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>自定义字体</label>
          <p class="setting-hint">留空使用系统默认字体</p>
        </div>
        <div class="setting-control">
          <input
            type="text"
            v-model="formData.ui.font_path"
            class="input"
            placeholder="字体文件路径"
          />
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>隐藏候选窗口</label>
          <p class="setting-hint">隐藏候选窗口渲染</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.ui.hide_candidate_window"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">编码显示</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>嵌入式编码行</label>
          <p class="setting-hint">
            输入码直接显示在光标处，而非候选窗上方
          </p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input type="checkbox" v-model="formData.ui.inline_preedit" />
            <span class="slider"></span>
          </label>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>候选布局</label>
          <p class="setting-hint">候选词的排列方式</p>
        </div>
        <div class="setting-control">
          <select v-model="formData.ui.candidate_layout" class="select">
            <option value="horizontal">横向</option>
            <option value="vertical">纵向</option>
          </select>
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">状态提示</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>显示时长</label>
          <p class="setting-hint">中英切换等状态提示的显示时间</p>
        </div>
        <div class="setting-control range-control">
          <input
            type="range"
            min="200"
            max="2000"
            step="100"
            v-model.number="formData.ui.status_indicator_duration"
          />
          <span class="range-value"
            >{{ formData.ui.status_indicator_duration }}ms</span
          >
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>水平偏移</label>
          <p class="setting-hint">状态提示相对光标的水平偏移</p>
        </div>
        <div class="setting-control range-control">
          <input
            type="range"
            min="-50"
            max="50"
            step="5"
            v-model.number="formData.ui.status_indicator_offset_x"
          />
          <span class="range-value"
            >{{ formData.ui.status_indicator_offset_x }}px</span
          >
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>垂直偏移</label>
          <p class="setting-hint">
            状态提示相对光标的垂直偏移（负值=向上）
          </p>
        </div>
        <div class="setting-control range-control">
          <input
            type="range"
            min="-100"
            max="100"
            step="5"
            v-model.number="formData.ui.status_indicator_offset_y"
          />
          <span class="range-value"
            >{{ formData.ui.status_indicator_offset_y }}px</span
          >
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">工具栏</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>显示工具栏</label>
          <p class="setting-hint">在屏幕上显示可拖动的输入法状态栏</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input type="checkbox" v-model="formData.toolbar.visible" />
            <span class="slider"></span>
          </label>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.theme-preview {
  background: #f9fafb;
  border-radius: 8px;
  padding: 16px;
}
.theme-preview.preview-rows {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.preview-row {
  display: flex;
  align-items: center;
  gap: 12px;
}
.preview-row-label {
  font-size: 12px;
  color: #6b7280;
  width: 80px;
  flex-shrink: 0;
  text-align: left;
}
.preview-candidate-window {
  display: inline-flex;
  gap: 12px;
  padding: 10px 14px;
  border: 1px solid #ccc;
  border-radius: 6px;
  background: #fff;
}
.preview-candidate-item {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  border-radius: 4px;
}
.preview-hover {
  background: #e6f0ff;
}
.preview-index {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  font-size: 11px;
  font-weight: 500;
  border-radius: 3px;
}
.preview-text {
  font-size: 14px;
}
.preview-toolbar {
  display: inline-flex;
  gap: 6px;
  padding: 6px 10px;
  border: 1px solid #ccc;
  border-radius: 6px;
  background: #fff;
}
.preview-toolbar-item {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  font-size: 12px;
  border-radius: 4px;
  color: #fff;
}
@media (max-width: 768px) {
  .theme-preview {
    min-width: auto;
  }
}
</style>
