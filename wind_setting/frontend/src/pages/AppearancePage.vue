<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { ChevronDown } from "lucide-vue-next";
import type { Config } from "../api/settings";
import type { ThemeInfo, ThemePreview, SystemFontInfo } from "../api/wails";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";

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

      <div class="setting-item">
        <div class="setting-info">
          <label>主题风格</label>
          <p class="setting-hint">选择亮色、暗色或跟随系统设置</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.ui.theme_style"
            @update:model-value="
              formData.ui.theme_style = $event;
              emit('themeStyleChange', $event);
            "
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="system">跟随系统</SelectItem>
              <SelectItem value="light">亮色</SelectItem>
              <SelectItem value="dark">暗色</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      <div class="setting-item align-start" v-if="themePreview">
        <div class="setting-info">
          <label class="inline-flex items-center gap-1">
            主题预览
            <span class="hint-tip" data-tip="预览效果可能和实际有所差异">?</span>
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
      <div class="setting-item">
        <div class="setting-info">
          <label>每页候选数</label>
          <p class="setting-hint">每页显示的候选词数量</p>
        </div>
        <div class="setting-control range-control">
          <input
            type="range"
            min="3"
            max="10"
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
          <label>隐藏候选窗口</label>
          <p class="setting-hint">不显示候选窗口</p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.ui.hide_candidate_window"
            @update:checked="formData.ui.hide_candidate_window = $event"
          />
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>嵌入式编码行</label>
          <p class="setting-hint">输入码直接显示在光标处，而非候选窗上方</p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.ui.inline_preedit"
            @update:checked="formData.ui.inline_preedit = $event"
          />
        </div>
      </div>
      <div class="setting-item" v-if="!formData.ui.inline_preedit">
        <div class="setting-info">
          <label>编码显示方式</label>
          <p class="setting-hint">未开启嵌入编码时，编码在候选窗中的显示位置</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.ui.preedit_mode"
            @update:model-value="formData.ui.preedit_mode = $event"
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="top">独立编码行</SelectItem>
              <SelectItem value="embedded">嵌入候选行</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>候选布局</label>
          <p class="setting-hint">候选词的排列方式</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.ui.candidate_layout"
            @update:model-value="formData.ui.candidate_layout = $event"
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="horizontal">横向</SelectItem>
              <SelectItem value="vertical">纵向</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
    </div>

    <div class="settings-card">
      <div class="card-title">状态提示</div>

      <!-- 总开关 -->
      <div class="setting-item">
        <div class="setting-info">
          <label>启用状态提示</label>
          <p class="setting-hint">切换输入状态时显示提示</p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.ui.status_indicator.enabled"
            @update:checked="formData.ui.status_indicator.enabled = $event"
          />
        </div>
      </div>

      <template v-if="formData.ui.status_indicator.enabled">
        <!-- 显示模式 -->
        <div class="setting-item">
          <div class="setting-info">
            <label>显示模式</label>
            <p class="setting-hint">
              临时显示在切换时闪现后自动消失，常驻显示在有输入焦点时始终显示
            </p>
          </div>
          <div class="setting-control">
            <Select
              :model-value="formData.ui.status_indicator.display_mode"
              @update:model-value="
                formData.ui.status_indicator.display_mode = $event
              "
            >
              <SelectTrigger class="w-[180px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="temp">临时显示</SelectItem>
                <SelectItem value="always">常驻显示 (beta)</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        <!-- 临时显示时长（仅临时模式） -->
        <div
          class="setting-item"
          v-if="formData.ui.status_indicator.display_mode === 'temp'"
        >
          <div class="setting-info">
            <label>显示时长</label>
            <p class="setting-hint">状态提示的显示时间</p>
          </div>
          <div class="setting-control range-control">
            <input
              type="range"
              min="200"
              max="30000"
              step="100"
              v-model.number="formData.ui.status_indicator.duration"
            />
            <span class="range-value"
              >{{ formData.ui.status_indicator.duration }}ms</span
            >
          </div>
        </div>

        <!-- 方案名风格 -->
        <div class="setting-item">
          <div class="setting-info">
            <label>方案名显示</label>
            <p class="setting-hint">中文模式下显示的方案名称风格</p>
          </div>
          <div class="setting-control">
            <Select
              :model-value="formData.ui.status_indicator.schema_name_style"
              @update:model-value="
                formData.ui.status_indicator.schema_name_style = $event
              "
            >
              <SelectTrigger class="w-[200px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="full">全称（五笔、全拼）</SelectItem>
                <SelectItem value="short">简写（五、拼）</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        <!-- 显示内容 -->
        <div class="setting-item">
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

        <!-- 位置设置 -->
        <div class="setting-item">
          <div class="setting-info">
            <label>位置模式</label>
            <p class="setting-hint">
              跟随光标或固定在自定义位置（可拖动状态窗口定位）
            </p>
          </div>
          <div class="setting-control">
            <Select
              :model-value="formData.ui.status_indicator.position_mode"
              @update:model-value="
                formData.ui.status_indicator.position_mode = $event
              "
            >
              <SelectTrigger class="w-[160px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="follow_caret">跟随光标</SelectItem>
                <SelectItem value="custom">自定义位置</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        <!-- 跟随模式偏移 -->
        <template
          v-if="formData.ui.status_indicator.position_mode === 'follow_caret'"
        >
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
                v-model.number="formData.ui.status_indicator.offset_x"
              />
              <span class="range-value"
                >{{ formData.ui.status_indicator.offset_x }}px</span
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
                v-model.number="formData.ui.status_indicator.offset_y"
              />
              <span class="range-value"
                >{{ formData.ui.status_indicator.offset_y }}px</span
              >
            </div>
          </div>
        </template>

        <!-- 外观设置 -->
        <div class="setting-item">
          <div class="setting-info">
            <label>字体大小</label>
            <p class="setting-hint">状态提示的字体大小</p>
          </div>
          <div class="setting-control range-control">
            <input
              type="range"
              min="10"
              max="24"
              step="1"
              v-model.number="formData.ui.status_indicator.font_size"
            />
            <span class="range-value"
              >{{ formData.ui.status_indicator.font_size }}px</span
            >
          </div>
        </div>

        <div class="setting-item">
          <div class="setting-info">
            <label>透明度</label>
            <p class="setting-hint">状态提示窗口的透明度</p>
          </div>
          <div class="setting-control range-control">
            <input
              type="range"
              min="0.3"
              max="1"
              step="0.05"
              v-model.number="formData.ui.status_indicator.opacity"
            />
            <span class="range-value"
              >{{
                Math.round(formData.ui.status_indicator.opacity * 100)
              }}%</span
            >
          </div>
        </div>

        <div class="setting-item">
          <div class="setting-info">
            <label>圆角</label>
            <p class="setting-hint">状态提示窗口的圆角半径</p>
          </div>
          <div class="setting-control range-control">
            <input
              type="range"
              min="0"
              max="16"
              step="1"
              v-model.number="formData.ui.status_indicator.border_radius"
            />
            <span class="range-value"
              >{{ formData.ui.status_indicator.border_radius }}px</span
            >
          </div>
        </div>
      </template>
    </div>

    <div class="settings-card">
      <div class="card-title">工具栏</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>显示工具栏</label>
          <p class="setting-hint">在屏幕上显示可拖动的输入法状态栏</p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.toolbar.visible"
            @update:checked="formData.toolbar.visible = $event"
          />
        </div>
      </div>
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
