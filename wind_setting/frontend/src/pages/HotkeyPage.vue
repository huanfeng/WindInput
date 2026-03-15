<template>
  <section class="section">
    <div class="section-header">
      <h2>按键设置</h2>
      <p class="section-desc">自定义快捷键和候选操作</p>
    </div>

    <!-- 冲突警告 -->
    <div v-if="hotkeyConflicts.length > 0" class="settings-card warning-card">
      <div class="warning-content">
        <span class="warning-icon">⚠</span>
        <div>
          <p class="warning-title">快捷键冲突</p>
          <ul class="warning-list">
            <li v-for="(c, i) in hotkeyConflicts" :key="i">{{ c }}</li>
          </ul>
        </div>
      </div>
    </div>

    <!-- 中英文切换 -->
    <div class="settings-card">
      <div class="card-title">中英文切换</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>切换按键</label>
          <p class="setting-hint">可多选，按下任意一个即切换</p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group two-columns">
            <label
              class="checkbox-item"
              v-for="key in ['lshift', 'rshift', 'lctrl', 'rctrl', 'capslock']"
              :key="key"
            >
              <input
                type="checkbox"
                :checked="formData.hotkeys.toggle_mode_keys.includes(key)"
                @change="
                  toggleArrayValue(formData.hotkeys.toggle_mode_keys, key)
                "
              />
              <span>{{ getKeyLabel(key) }}</span>
            </label>
          </div>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>切换时编码上屏</label>
          <p class="setting-hint">中文切换为英文时，将已输入的编码直接上屏</p>
        </div>
        <div class="setting-control">
          <label class="switch">
            <input
              type="checkbox"
              v-model="formData.hotkeys.commit_on_switch"
            />
            <span class="slider"></span>
          </label>
        </div>
      </div>
    </div>

    <!-- 功能快捷键 -->
    <div class="settings-card">
      <div class="card-title">功能快捷键</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>切换拼音/五笔</label>
          <p class="setting-hint">在拼音和五笔引擎间切换</p>
        </div>
        <div class="setting-control">
          <select v-model="formData.hotkeys.switch_engine" class="select">
            <option value="ctrl+`">Ctrl + `</option>
            <option value="ctrl+shift+e">Ctrl + Shift + E</option>
            <option value="none">不使用</option>
          </select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>切换全角/半角</label>
          <p class="setting-hint">切换字符宽度模式</p>
        </div>
        <div class="setting-control">
          <select v-model="formData.hotkeys.toggle_full_width" class="select">
            <option value="shift+space">Shift + Space</option>
            <option value="ctrl+shift+space">Ctrl + Shift + Space</option>
            <option value="none">不使用</option>
          </select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>切换中/英文标点</label>
          <p class="setting-hint">切换标点符号类型</p>
        </div>
        <div class="setting-control">
          <select v-model="formData.hotkeys.toggle_punct" class="select">
            <option value="ctrl+.">Ctrl + .</option>
            <option value="ctrl+,">Ctrl + ,</option>
            <option value="none">不使用</option>
          </select>
        </div>
      </div>
    </div>

    <!-- 候选选择键 -->
    <div class="settings-card">
      <div class="card-title">候选选择键</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>2/3候选快捷键组</label>
          <p class="setting-hint">可多选，同时启用多组快捷键</p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group two-columns">
            <label
              class="checkbox-item"
              v-for="group in [
                { value: 'semicolon_quote', label: '; \' 键' },
                { value: 'comma_period', label: ', . 键' },
                { value: 'lrshift', label: 'L/R Shift' },
                { value: 'lrctrl', label: 'L/R Ctrl' },
              ]"
              :key="group.value"
            >
              <input
                type="checkbox"
                :checked="
                  formData.input.select_key_groups.includes(group.value)
                "
                @change="
                  toggleArrayValue(
                    formData.input.select_key_groups,
                    group.value,
                  )
                "
              />
              <span>{{ group.label }}</span>
            </label>
          </div>
        </div>
      </div>
    </div>

    <!-- 拼音分隔符 -->
    <div class="settings-card">
      <div class="card-title">拼音分隔符</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>分隔符按键</label>
          <p class="setting-hint">
            拼音模式下用于消歧的分隔符，如输入 xi'an 得到「西安」
          </p>
        </div>
        <div class="setting-control">
          <select v-model="formData.input.pinyin_separator" class="select">
            <option value="auto">自动（' 被选择键占用时改用 `）</option>
            <option value="quote">' 单引号</option>
            <option value="backtick">` 反引号</option>
            <option value="none">不使用</option>
          </select>
        </div>
      </div>
    </div>

    <!-- 移动高亮候选项 -->
    <div class="settings-card">
      <div class="card-title">移动高亮候选项</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>高亮移动按键</label>
          <p class="setting-hint">
            可多选，用于在候选列表中移动选中项。Tab/Shift+Tab 与翻页键互斥
          </p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group">
            <label
              class="checkbox-item"
              v-for="hk in [
                { value: 'arrows', label: '上/下方向键' },
                { value: 'tab', label: 'Tab / Shift+Tab' },
              ]"
              :key="hk.value"
            >
              <input
                type="checkbox"
                :checked="formData.input.highlight_keys.includes(hk.value)"
                @change="toggleHighlightKey(hk.value)"
              />
              <span>{{ hk.label }}</span>
            </label>
          </div>
        </div>
      </div>
    </div>

    <!-- 翻页键 -->
    <div class="settings-card">
      <div class="card-title">翻页键</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>翻页快捷键</label>
          <p class="setting-hint">可多选，同时启用多组翻页键</p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group two-columns">
            <label
              class="checkbox-item"
              v-for="pk in [
                { value: 'pageupdown', label: 'PgUp/PgDn' },
                { value: 'minus_equal', label: '- / =' },
                { value: 'brackets', label: '[ / ]' },
                { value: 'shift_tab', label: 'Shift+Tab / Tab' },
              ]"
              :key="pk.value"
            >
              <input
                type="checkbox"
                :checked="formData.input.page_keys.includes(pk.value)"
                @change="togglePageKey(pk.value)"
              />
              <span>{{ pk.label }}</span>
            </label>
          </div>
        </div>
      </div>
    </div>

    <!-- 临时拼音 -->
    <div class="settings-card">
      <div class="card-title">临时拼音</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>触发键</label>
          <p class="setting-hint">五笔模式下按触发键临时切换拼音输入</p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group">
            <label
              class="checkbox-item"
              v-for="tk in [
                { value: 'backtick', label: '` 反引号' },
                { value: 'semicolon', label: '; 分号' },
              ]"
              :key="tk.value"
            >
              <input
                type="checkbox"
                :checked="
                  formData.input.temp_pinyin.trigger_keys.includes(tk.value)
                "
                @change="
                  toggleArrayValue(
                    formData.input.temp_pinyin.trigger_keys,
                    tk.value,
                  )
                "
              />
              <span>{{ tk.label }}</span>
            </label>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { watch } from "vue";
import type { Config } from "../api/settings";

const props = defineProps<{
  formData: Config;
  hotkeyConflicts: string[];
}>();

const emit = defineEmits<{
  "update:hotkeyConflicts": [conflicts: string[]];
}>();

function checkConflicts() {
  const conflicts: string[] = [];
  const usedKeys = new Map<string, string>();

  for (const key of props.formData.hotkeys.toggle_mode_keys) {
    if (usedKeys.has(key)) {
      conflicts.push(
        `按键 "${getKeyLabel(key)}" 同时用于: ${usedKeys.get(key)} 和 中英切换`,
      );
    } else {
      usedKeys.set(key, "中英切换");
    }
  }

  for (const group of props.formData.input.select_key_groups) {
    const keys = getGroupKeys(group);
    for (const key of keys) {
      if (usedKeys.has(key)) {
        conflicts.push(
          `按键 "${getKeyLabel(key)}" 同时用于: ${usedKeys.get(key)} 和 候选选择`,
        );
      } else {
        usedKeys.set(key, "候选选择");
      }
    }
  }

  emit("update:hotkeyConflicts", conflicts);
}

function getGroupKeys(group: string): string[] {
  switch (group) {
    case "semicolon_quote":
      return ["semicolon", "quote"];
    case "comma_period":
      return ["comma", "period"];
    case "lrshift":
      return ["lshift", "rshift"];
    case "lrctrl":
      return ["lctrl", "rctrl"];
    default:
      return [];
  }
}

function getKeyLabel(key: string): string {
  const labels: Record<string, string> = {
    lshift: "左Shift",
    rshift: "右Shift",
    lctrl: "左Ctrl",
    rctrl: "右Ctrl",
    capslock: "CapsLock",
    semicolon: ";",
    quote: "'",
    comma: ",",
    period: ".",
  };
  return labels[key] || key;
}

function toggleArrayValue(arr: string[], value: string) {
  const idx = arr.indexOf(value);
  if (idx >= 0) {
    arr.splice(idx, 1);
  } else {
    arr.push(value);
  }
  checkConflicts();
}

function toggleHighlightKey(value: string) {
  toggleArrayValue(props.formData.input.highlight_keys, value);
  // Tab/Shift+Tab 与翻页互斥
  if (value === "tab" && props.formData.input.highlight_keys.includes("tab")) {
    const idx = props.formData.input.page_keys.indexOf("shift_tab");
    if (idx >= 0) {
      props.formData.input.page_keys.splice(idx, 1);
    }
  }
}

function togglePageKey(value: string) {
  toggleArrayValue(props.formData.input.page_keys, value);
  // Tab/Shift+Tab 与高亮移动互斥
  if (
    value === "shift_tab" &&
    props.formData.input.page_keys.includes("shift_tab")
  ) {
    const idx = props.formData.input.highlight_keys.indexOf("tab");
    if (idx >= 0) {
      props.formData.input.highlight_keys.splice(idx, 1);
    }
  }
}

watch(
  () => [
    props.formData.hotkeys.toggle_mode_keys,
    props.formData.input.select_key_groups,
    props.formData.input.highlight_keys,
  ],
  checkConflicts,
  { deep: true },
);
</script>
