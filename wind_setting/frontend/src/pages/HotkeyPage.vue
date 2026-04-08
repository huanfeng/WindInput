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
          <label>切换输入方案</label>
          <p class="setting-hint">在已启用的输入方案间循环切换</p>
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

    <!-- 候选词管理 -->
    <div class="settings-card">
      <div class="card-title">候选词管理</div>
      <div v-if="candidateActionConflict" class="warning-inline">
        <span class="warning-icon">⚠</span>
        <span>置顶和删除不能使用相同的快捷键</span>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>置顶词条</label>
          <p class="setting-hint">将候选词固定到首位</p>
        </div>
        <div class="setting-control">
          <select v-model="formData.hotkeys.pin_candidate" class="select">
            <option value="ctrl+number">Ctrl + 数字</option>
            <option value="ctrl+shift+number">Ctrl + Shift + 数字</option>
            <option value="none">不使用</option>
          </select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>删除词条</label>
          <p class="setting-hint">隐藏候选词（单字不可删除）</p>
        </div>
        <div class="setting-control">
          <select v-model="formData.hotkeys.delete_candidate" class="select">
            <option value="ctrl+shift+number">Ctrl + Shift + 数字</option>
            <option value="ctrl+number">Ctrl + 数字</option>
            <option value="none">不使用</option>
          </select>
        </div>
      </div>
    </div>

    <!-- 候选操作 -->
    <div class="settings-card">
      <div class="card-title">候选操作</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>次选/三选快捷键</label>
          <p class="setting-hint">选中第2、3位候选词的快捷键，可多选</p>
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

    <!-- 拼音辅助 -->
    <div class="settings-card">
      <div class="card-title">拼音辅助</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>拼音分隔符</label>
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
      <div class="setting-item">
        <div class="setting-info">
          <label>临时拼音触发键</label>
          <p class="setting-hint">码表模式下按触发键临时切换拼音输入</p>
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

    <!-- 更多快捷键 -->
    <div class="settings-card">
      <div class="card-title">更多快捷键</div>
      <div
        class="setting-item"
        v-for="item in genericHotkeyItems"
        :key="item.field"
      >
        <div class="setting-info">
          <label>{{ item.label }}</label>
          <p class="setting-hint">{{ item.hint }}</p>
        </div>
        <div class="setting-control">
          <div
            class="hotkey-recorder"
            :class="{ recording: recordingField === item.field }"
            tabindex="0"
            @click="toggleRecording(item.field)"
            @blur="stopRecording(item.field)"
            @keydown="handleRecordKeydown($event, item.field)"
          >
            <span
              class="hotkey-display"
              :class="{ placeholder: isPlaceholder(item.field) }"
            >
              {{ getRecorderDisplay(item.field) }}
            </span>
            <button
              v-if="
                getHotkeyValue(item.field) !== 'none' &&
                recordingField !== item.field
              "
              class="hotkey-action-btn"
              @click.stop="clearHotkey(item.field)"
              title="清除"
            >
              <svg width="14" height="14" viewBox="0 0 14 14">
                <path
                  d="M3.5 3.5l7 7M10.5 3.5l-7 7"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                />
              </svg>
            </button>
            <button
              v-if="recordingField === item.field"
              class="hotkey-action-btn"
              @mousedown.prevent.stop="stopRecording(item.field)"
              title="取消录入"
            >
              <svg width="14" height="14" viewBox="0 0 14 14">
                <path
                  d="M3.5 3.5l7 7M10.5 3.5l-7 7"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                />
              </svg>
            </button>
            <button
              v-if="
                hasNonDefaultValue(item.field) && recordingField !== item.field
              "
              class="hotkey-action-btn"
              @click.stop="restoreDefault(item.field)"
              title="恢复默认"
            >
              <svg width="14" height="14" viewBox="0 0 14 14">
                <path
                  d="M2.5 7.5a5 5 0 1 1 1 3"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                />
                <path
                  d="M2.5 10.5v-3h3"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
            </button>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref, watch, computed } from "vue";
import type { Config, HotkeyConfig } from "../api/settings";
import { getDefaultConfig } from "../api/settings";

const props = defineProps<{
  formData: Config;
  hotkeyConflicts: string[];
  systemDefaults?: Config;
}>();

const emit = defineEmits<{
  "update:hotkeyConflicts": [conflicts: string[]];
}>();

// 通用快捷键字段定义
const genericHotkeyItems = [
  {
    field: "toggle_toolbar",
    label: "显示/隐藏状态栏",
    hint: "切换状态栏的显示状态",
  },
  { field: "open_settings", label: "打开设置", hint: "打开设置窗口" },
];

// 默认值缓存（优先使用系统默认配置）
const defaults = computed<HotkeyConfig>(
  () => props.systemDefaults?.hotkeys || getDefaultConfig().hotkeys,
);

// 按键录入器状态
const recordingField = ref<string | null>(null);

// 候选操作快捷键冲突检测
const candidateActionConflict = computed(() => {
  const pin = props.formData.hotkeys.pin_candidate;
  const del = props.formData.hotkeys.delete_candidate;
  return pin !== "none" && del !== "none" && pin === del;
});

// --- 按键录入器方法 ---

function getHotkeyValue(field: string): string {
  return (props.formData.hotkeys as any)[field] || "none";
}

function isPlaceholder(field: string): boolean {
  if (recordingField.value === field) return false;
  return getHotkeyValue(field) === "none";
}

function getRecorderDisplay(field: string): string {
  if (recordingField.value === field) return "请按下快捷键...";
  return formatHotkeyDisplay(getHotkeyValue(field));
}

function hasNonDefaultValue(field: string): boolean {
  const current = getHotkeyValue(field);
  const def = (defaults.value as any)[field] || "none";
  return current !== def;
}

function toggleRecording(field: string) {
  if (recordingField.value === field) {
    recordingField.value = null;
  } else {
    recordingField.value = field;
  }
}

function stopRecording(field: string) {
  if (recordingField.value === field) {
    recordingField.value = null;
  }
}

function handleRecordKeydown(e: KeyboardEvent, field: string) {
  if (recordingField.value !== field) return;
  e.preventDefault();
  e.stopPropagation();

  // ESC 取消录入
  if (e.key === "Escape") {
    recordingField.value = null;
    return;
  }

  // 只有修饰键按下时不记录（等待实际按键）
  if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) {
    return;
  }

  // 必须包含至少一个修饰键
  if (!e.ctrlKey && !e.shiftKey && !e.altKey) {
    return;
  }

  // 构建快捷键字符串
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("ctrl");
  if (e.shiftKey) parts.push("shift");
  if (e.altKey) parts.push("alt");

  const keyName = mapKeyToName(e.key, e.code);
  if (!keyName) return;

  parts.push(keyName);
  const hotkeyStr = parts.join("+");

  // 冲突解决：如果其他通用快捷键字段使用了相同的组合，自动清除旧绑定
  for (const item of genericHotkeyItems) {
    if (item.field !== field && getHotkeyValue(item.field) === hotkeyStr) {
      (props.formData.hotkeys as any)[item.field] = "none";
    }
  }

  (props.formData.hotkeys as any)[field] = hotkeyStr;
  recordingField.value = null;
}

function mapKeyToName(key: string, _code: string): string | null {
  if (key.length === 1 && /[a-zA-Z]/.test(key)) {
    return key.toLowerCase();
  }
  if (key.length === 1 && /[0-9]/.test(key)) {
    return key;
  }
  const specialKeys: Record<string, string> = {
    "`": "`",
    "~": "`",
    "-": "-",
    _: "-",
    "=": "=",
    "+": "=",
    "[": "[",
    "{": "[",
    "]": "]",
    "}": "]",
    "\\": "\\",
    "|": "\\",
    ";": ";",
    ":": ";",
    "'": "'",
    '"': "'",
    ",": ",",
    "<": ",",
    ".": ".",
    ">": ".",
    "/": "/",
    "?": "/",
    " ": "space",
    Tab: "tab",
    F1: "f1",
    F2: "f2",
    F3: "f3",
    F4: "f4",
    F5: "f5",
    F6: "f6",
    F7: "f7",
    F8: "f8",
    F9: "f9",
    F10: "f10",
    F11: "f11",
    F12: "f12",
  };
  return specialKeys[key] || null;
}

function clearHotkey(field: string) {
  (props.formData.hotkeys as any)[field] = "none";
  recordingField.value = null;
}

function restoreDefault(field: string) {
  (props.formData.hotkeys as any)[field] = (defaults.value as any)[field] || "none";
  recordingField.value = null;
}

function formatHotkeyDisplay(value: string): string {
  if (!value || value === "none") return "未设置（点击录入）";

  const labels: Record<string, string> = {
    ctrl: "Ctrl",
    shift: "Shift",
    alt: "Alt",
    space: "Space",
    tab: "Tab",
    f1: "F1",
    f2: "F2",
    f3: "F3",
    f4: "F4",
    f5: "F5",
    f6: "F6",
    f7: "F7",
    f8: "F8",
    f9: "F9",
    f10: "F10",
    f11: "F11",
    f12: "F12",
    "`": "`",
    "-": "-",
    "=": "=",
    "[": "[",
    "]": "]",
    "\\": "\\",
    ";": ";",
    "'": "'",
    ",": ",",
    ".": ".",
    "/": "/",
  };

  return value
    .split("+")
    .map((part) => {
      const trimmed = part.trim();
      if (labels[trimmed]) return labels[trimmed];
      if (trimmed.length === 1 && /[a-z]/.test(trimmed))
        return trimmed.toUpperCase();
      if (trimmed.length === 1 && /[0-9]/.test(trimmed)) return trimmed;
      return trimmed;
    })
    .join(" + ");
}

// --- 原有逻辑 ---

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
  if (value === "tab" && props.formData.input.highlight_keys.includes("tab")) {
    const idx = props.formData.input.page_keys.indexOf("shift_tab");
    if (idx >= 0) {
      props.formData.input.page_keys.splice(idx, 1);
    }
  }
}

function togglePageKey(value: string) {
  toggleArrayValue(props.formData.input.page_keys, value);
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

<style scoped>
.hotkey-recorder {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  min-width: 200px;
  padding: 6px 10px;
  border: 1px solid var(--border-color, #d0d0d0);
  border-radius: 6px;
  cursor: pointer;
  font-size: 13px;
  transition:
    border-color 0.2s,
    box-shadow 0.2s;
  background: var(--input-bg, #fff);
}
.hotkey-recorder:focus,
.hotkey-recorder.recording {
  outline: none;
  border-color: var(--primary-color, #4a90d9);
  box-shadow: 0 0 0 2px rgba(74, 144, 217, 0.2);
}
.hotkey-recorder.recording .hotkey-display {
  color: var(--primary-color, #4a90d9);
}
.hotkey-display {
  flex: 1;
  color: var(--text-color, #333);
  user-select: none;
}
.hotkey-display.placeholder {
  color: var(--text-secondary, #999);
}
.hotkey-action-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  padding: 0;
  background: none;
  border: 1px solid transparent;
  border-radius: 4px;
  cursor: pointer;
  color: var(--text-secondary, #999);
  transition:
    color 0.15s,
    background 0.15s;
  flex-shrink: 0;
}
.hotkey-action-btn:hover {
  color: var(--text-color, #333);
  background: var(--hover-bg, rgba(0, 0, 0, 0.06));
}
.warning-inline {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 12px;
  margin-bottom: 8px;
  background: rgba(255, 152, 0, 0.08);
  border-radius: 6px;
  font-size: 13px;
  color: var(--warning-color, #e65100);
}
</style>
