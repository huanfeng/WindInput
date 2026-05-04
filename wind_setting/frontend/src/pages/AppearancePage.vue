<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from "vue";
import { ChevronDown } from "lucide-vue-next";
import type { Config } from "../api/settings";
import type { ThemeInfo, ThemePreview, SystemFontInfo } from "../api/wails";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";
import SchemaRenderer from "@/components/SchemaRenderer.vue";
import {
  themeExtraSchema,
  candidateWindowSchema,
  statusIndicatorSchema,
  candidateTooltipSchema,
  toolbarSchema,
} from "@/schemas/appearance.schema";

const props = defineProps<{
  formData: Config;
  isWailsEnv: boolean;
  availableThemes: ThemeInfo[];
  themePreview: ThemePreview | null;
  systemFonts: SystemFontInfo[];
}>();

const emit = defineEmits<{
  themeSelect: [themeName: string];
  themeStyleChange: [themeStyle: string];
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
  }));
});

const currentThemeOption = computed(() => {
  return themeOptions.value.find(
    (option) => option.name === props.formData.ui.theme,
  );
});

const systemFontOptions = computed(() => {
  return props.systemFonts.map((font) => ({
    value: font.family,
    label: font.display_name || font.family,
  }));
});

function onThemeSelect(themeName: string) {
  props.formData.ui.theme = themeName;
  emit("themeSelect", themeName);
  themeSelectOpen.value = false;
}

watch(
  () => props.formData.ui.theme_style,
  (val) => emit("themeStyleChange", val),
);

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
              <ChevronDown
                class="h-4 w-4 text-muted-foreground flex-shrink-0"
              />
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

      <SchemaRenderer
        :schema="themeExtraSchema"
        :form-data="formData"
        mode="bare"
      />

      <div class="setting-item align-start" v-if="themePreview">
        <div class="setting-info">
          <label class="inline-flex items-center gap-1">
            主题预览
            <span class="hint-tip" data-tip="预览效果可能和实际有所差异"
              >?</span
            >
          </label>
          <p class="setting-hint">候选窗口与工具栏预览</p>
        </div>
        <div class="setting-control">
          <div
            class="theme-preview"
            :style="{
              background: themePreview.is_dark?.active ? '#1a1a1a' : '#f0f0f0',
            }"
          >
            <div class="preview-layout">
              <!-- 候选窗口 -->
              <div class="preview-block">
                <div class="preview-section-label">候选窗口</div>
                <div
                  class="preview-candidate-window"
                  :style="{
                    backgroundColor:
                      themePreview.candidate_window?.background_color,
                    borderColor: themePreview.candidate_window?.border_color,
                    boxShadow: themePreview.candidate_window?.shadow_color
                      ? '0 3px 8px ' +
                        themePreview.candidate_window.shadow_color
                      : '0 3px 8px rgba(0,0,0,0.06)',
                  }"
                >
                  <!-- 输入行（嵌入编码模式下隐藏） -->
                  <div
                    v-if="!formData.ui.inline_preedit"
                    class="preview-input-bar"
                    :style="{
                      backgroundColor:
                        themePreview.candidate_window?.input_bg_color,
                    }"
                  >
                    <span
                      :style="{
                        color: themePreview.candidate_window?.input_text_color,
                      }"
                      >zhong'wen</span
                    >
                  </div>
                  <!-- 候选项 -->
                  <div class="preview-candidates">
                    <div
                      v-for="(item, idx) in [
                        { n: '1', text: '中文', hover: true },
                        { n: '2', text: '清风', comment: 'igmq' },
                        { n: '3', text: '输入' },
                      ]"
                      :key="idx"
                      class="preview-candidate-item"
                      :style="{
                        backgroundColor: item.hover
                          ? themePreview.candidate_window?.hover_bg_color
                          : undefined,
                      }"
                    >
                      <!-- accent bar（微软风格：仅高亮项显示） -->
                      <div
                        v-if="
                          themePreview.style?.accent_bar_color && item.hover
                        "
                        class="preview-item-accent"
                        :style="{
                          backgroundColor: themePreview.style.accent_bar_color,
                        }"
                      ></div>
                      <span
                        class="preview-index"
                        :class="{
                          'preview-index-circle':
                            themePreview.style?.index_style !== 'text',
                          'preview-index-text':
                            themePreview.style?.index_style === 'text',
                        }"
                        :style="
                          themePreview.style?.index_style === 'text'
                            ? {
                                color:
                                  themePreview.candidate_window?.index_color,
                              }
                            : {
                                backgroundColor:
                                  themePreview.candidate_window?.index_bg_color,
                                color:
                                  themePreview.candidate_window?.index_color,
                              }
                        "
                        >{{ item.n }}</span
                      >
                      <span
                        class="preview-text"
                        :style="{
                          color: themePreview.candidate_window?.text_color,
                        }"
                        >{{ item.text }}</span
                      >
                      <span
                        v-if="item.comment"
                        class="preview-comment"
                        :style="{
                          color: themePreview.candidate_window?.comment_color,
                        }"
                        >{{ item.comment }}</span
                      >
                    </div>
                  </div>
                </div>
              </div>

              <!-- 工具栏 -->
              <div class="preview-block">
                <div class="preview-section-label">工具栏</div>
                <div
                  class="preview-toolbar"
                  :style="{
                    backgroundColor: themePreview.toolbar?.background_color,
                    borderColor: themePreview.toolbar?.border_color,
                  }"
                >
                  <span
                    class="preview-toolbar-grip"
                    :style="{
                      color: themePreview.toolbar?.grip_color || '#c0c0c0',
                    }"
                    >⠿</span
                  >
                  <span
                    class="preview-toolbar-item"
                    :style="{
                      backgroundColor:
                        themePreview.toolbar?.mode_chinese_bg_color,
                      color: themePreview.toolbar?.mode_text_color || '#fff',
                    }"
                    >中</span
                  >
                  <span
                    class="preview-toolbar-item"
                    :style="{
                      backgroundColor:
                        themePreview.toolbar?.full_width_off_bg_color,
                      color:
                        themePreview.toolbar?.full_width_off_color || '#666',
                    }"
                    >半</span
                  >
                  <span
                    class="preview-toolbar-item"
                    :style="{
                      backgroundColor:
                        themePreview.toolbar?.punct_chinese_bg_color,
                      color:
                        themePreview.toolbar?.punct_chinese_color || '#fff',
                    }"
                    >。</span
                  >
                  <span
                    class="preview-toolbar-item"
                    :style="{
                      backgroundColor: themePreview.toolbar?.settings_bg_color,
                      color:
                        themePreview.toolbar?.settings_icon_color || '#666',
                    }"
                    >⚙</span
                  >
                </div>
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
      <div class="setting-item" v-if="isWailsEnv">
        <div class="setting-info">
          <label>候选字体</label>
          <p class="setting-hint">
            设置候选词的显示字体，从系统已安装字体中选择并自动回退
          </p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.ui.font_family || '__default__'"
            @update:model-value="
              formData.ui.font_family = $event === '__default__' ? '' : $event
            "
          >
            <SelectTrigger class="w-[200px]">
              <SelectValue placeholder="跟随系统默认" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__default__">跟随系统默认</SelectItem>
              <SelectItem
                v-for="font in systemFontOptions"
                :key="font.value"
                :value="font.value"
              >
                {{ font.label }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <SchemaRenderer
        :schema="candidateWindowSchema"
        :form-data="formData"
        mode="bare"
      />
    </div>

    <div class="settings-card">
      <div class="card-title">状态提示</div>
      <SchemaRenderer
        :schema="statusIndicatorSchema"
        :form-data="formData"
        mode="bare"
      />
      <!-- 显示内容（复选框组，无对应 schema 类型） -->
      <div class="setting-item" v-if="formData.ui.status_indicator.enabled">
        <div class="setting-info">
          <label>显示内容</label>
          <p class="setting-hint">选择状态提示中显示的信息</p>
        </div>
        <div class="setting-control inline-control">
          <label class="checkbox-label">
            <input
              type="checkbox"
              v-model="formData.ui.status_indicator.show_mode"
            />
            模式
          </label>
          <label class="checkbox-label">
            <input
              type="checkbox"
              v-model="formData.ui.status_indicator.show_punct"
            />
            标点
          </label>
          <label class="checkbox-label">
            <input
              type="checkbox"
              v-model="formData.ui.status_indicator.show_full_width"
            />
            全半角
          </label>
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">候选项提示信息</div>
      <SchemaRenderer
        :schema="candidateTooltipSchema"
        :form-data="formData"
        mode="bare"
      />
    </div>

    <div class="settings-card">
      <div class="card-title">工具栏</div>
      <SchemaRenderer
        :schema="toolbarSchema"
        :form-data="formData"
        mode="bare"
      />
    </div>
  </section>
</template>

<style scoped>
/* 问号提示图标 */
.preview-hint-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  font-size: 10px;
  font-weight: 600;
  border-radius: 50%;
  background: hsl(var(--muted-foreground));
  color: hsl(var(--card));
  margin-left: 4px;
  cursor: help;
  vertical-align: middle;
}
/* 预览容器 */
.theme-preview {
  border-radius: 10px;
  padding: 16px;
  transition: background 0.2s;
}
.preview-layout {
  display: flex;
  gap: 24px;
  align-items: flex-start;
}
.preview-block {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.preview-section-label {
  font-size: 11px;
  color: hsl(var(--muted-foreground));
  letter-spacing: 0.5px;
}
/* 候选窗口 */
.preview-candidate-window {
  display: flex;
  flex-direction: column;
  border: 1px solid hsl(var(--border));
  border-radius: 8px;
  overflow: hidden;
}
.preview-input-bar {
  padding: 4px 10px;
  font-size: 11px;
  font-family: monospace;
}
.preview-candidates {
  display: flex;
  align-items: center;
  gap: 1px;
  padding: 5px 6px;
}
.preview-candidate-item {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 2px 5px;
  border-radius: 4px;
  position: relative;
}
/* accent bar（微软风格：绑定在每个候选项左侧） */
.preview-item-accent {
  position: absolute;
  left: 0;
  top: 3px;
  bottom: 3px;
  width: 2px;
  border-radius: 0 1px 1px 0;
}
/* 圆形序号（默认主题） */
.preview-index {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 9px;
  font-weight: 500;
  flex-shrink: 0;
}
.preview-index.preview-index-circle {
  width: 15px;
  height: 15px;
  border-radius: 50%;
}
/* 文字序号（微软风格） */
.preview-index.preview-index-text {
  background: transparent !important;
  font-size: 11px;
  font-weight: 600;
  width: auto;
  padding: 0 1px;
}
.preview-text {
  font-size: 12px;
  white-space: nowrap;
}
.preview-comment {
  font-size: 10px;
  margin-left: 2px;
  white-space: nowrap;
}
/* 工具栏 */
.preview-toolbar {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  padding: 3px 6px;
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
}
.preview-toolbar-grip {
  font-size: 9px;
  margin-right: 1px;
  opacity: 0.7;
  user-select: none;
}
.preview-toolbar-item {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  font-size: 10px;
  border-radius: 4px;
}
@media (max-width: 768px) {
  .preview-layout {
    flex-direction: column;
    gap: 12px;
  }
}

.font-family-select {
  max-width: 200px;
  max-height: 300px;
  overflow-y: auto;
  text-overflow: ellipsis;
}

.checkbox-label {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 13px;
  cursor: pointer;
  white-space: nowrap;
}

.checkbox-label input[type="checkbox"] {
  cursor: pointer;
}
</style>
