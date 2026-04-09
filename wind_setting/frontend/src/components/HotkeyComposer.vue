<template>
  <div class="setting-item">
    <div class="setting-info">
      <label>{{ label }}</label>
      <p class="setting-hint">{{ hint }}</p>
    </div>
    <div class="setting-control">
      <label class="composer-enable">
        <input type="checkbox" :checked="enabled" @change="toggleEnabled" />
        <span>启用</span>
      </label>
      <div class="hotkey-composer" :class="{ disabled: !enabled }">
        <label class="composer-modifier">
          <input
            type="checkbox"
            :checked="parsed.ctrl"
            :disabled="!enabled"
            @change="
              updateModifier(
                'ctrl',
                ($event.target as HTMLInputElement).checked,
              )
            "
          />
          <span>Ctrl</span>
        </label>
        <span class="composer-plus" :class="{ dimmed: !enabled }">+</span>
        <label class="composer-modifier">
          <input
            type="checkbox"
            :checked="parsed.shift"
            :disabled="!enabled"
            @change="
              updateModifier(
                'shift',
                ($event.target as HTMLInputElement).checked,
              )
            "
          />
          <span>Shift</span>
        </label>
        <span class="composer-plus" :class="{ dimmed: !enabled }">+</span>
        <input
          class="composer-key-input"
          :class="{ empty: !parsed.key }"
          :disabled="!enabled"
          :value="keyDisplay"
          placeholder=""
          readonly
          @focus="($event.target as HTMLInputElement).select()"
          @keydown="handleKeydown"
        />
      </div>
      <button
        class="composer-reset-btn"
        :class="{ changed: hasChanged }"
        :disabled="!hasChanged"
        @click.stop="restoreDefault"
        title="恢复默认"
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 14 14"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <path d="M1.5 2.5v4h4" />
          <path d="M2.2 8.5a5 5 0 1 0 1.05-4.95L1.5 6.5" />
        </svg>
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";

const props = defineProps<{
  label: string;
  hint: string;
  modelValue: string;
  defaultValue: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
}>();

interface ParsedHotkey {
  ctrl: boolean;
  shift: boolean;
  key: string;
}

function parseHotkey(value: string): ParsedHotkey {
  if (!value || value === "none") {
    const def = parseHotkeyStr(props.defaultValue);
    return def;
  }
  return parseHotkeyStr(value);
}

function parseHotkeyStr(value: string): ParsedHotkey {
  if (!value || value === "none") {
    return { ctrl: false, shift: false, key: "" };
  }
  const parts = value.split("+");
  const result: ParsedHotkey = { ctrl: false, shift: false, key: "" };
  for (const part of parts) {
    const p = part.trim().toLowerCase();
    if (p === "ctrl") result.ctrl = true;
    else if (p === "shift") result.shift = true;
    else result.key = p;
  }
  return result;
}

const enabled = computed(() => props.modelValue !== "none");

const parsed = computed(() => parseHotkey(props.modelValue));

const hasChanged = computed(() => props.modelValue !== props.defaultValue);

// 特殊键的显示名称映射
const keyDisplayLabels: Record<string, string> = {
  space: "Space",
  tab: "Tab",
  equal: "=",
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

const keyDisplay = computed(() => {
  if (!parsed.value.key) return "";
  const label = keyDisplayLabels[parsed.value.key];
  if (label) return label;
  // F1-F12
  const fnMatch = parsed.value.key.match(/^f(\d{1,2})$/);
  if (fnMatch) return `F${fnMatch[1]}`;
  if (parsed.value.key.length === 1 && /[a-z]/.test(parsed.value.key))
    return parsed.value.key.toUpperCase();
  return parsed.value.key;
});

function buildHotkeyString(p: ParsedHotkey): string {
  if (!p.key) return "none";
  if (!p.ctrl && !p.shift) return "none";
  const parts: string[] = [];
  if (p.ctrl) parts.push("ctrl");
  if (p.shift) parts.push("shift");
  parts.push(p.key);
  return parts.join("+");
}

function toggleEnabled() {
  if (enabled.value) {
    emit("update:modelValue", "none");
  } else {
    emit("update:modelValue", props.defaultValue);
  }
}

function updateModifier(mod: "ctrl" | "shift", checked: boolean) {
  const p = { ...parsed.value, [mod]: checked };
  emit("update:modelValue", buildHotkeyString(p));
}

function handleKeydown(e: KeyboardEvent) {
  e.preventDefault();

  // 忽略纯修饰键和功能键
  if (
    [
      "Control",
      "Shift",
      "Alt",
      "Meta",
      "CapsLock",
      "NumLock",
      "ScrollLock",
    ].includes(e.key)
  ) {
    return;
  }

  // 忽略不应影响配置的键
  if (["Delete", "Backspace", "Insert", "Escape", "Enter"].includes(e.key)) {
    return;
  }

  // F1-F12 功能键
  const fnMatch = e.key.match(/^F(\d{1,2})$/);
  if (fnMatch) {
    const n = parseInt(fnMatch[1]);
    if (n >= 1 && n <= 12) {
      const p = { ...parsed.value, key: `f${n}` };
      emit("update:modelValue", buildHotkeyString(p));
      return;
    }
  }

  // 特殊键映射
  const specialKeys: Record<string, string> = {
    " ": "space",
    Tab: "tab",
  };
  const mapped = specialKeys[e.key];
  if (mapped) {
    const p = { ...parsed.value, key: mapped };
    emit("update:modelValue", buildHotkeyString(p));
    return;
  }

  // 普通字符
  const keyName = mapCharToName(e.key.length === 1 ? e.key.toLowerCase() : "");
  if (keyName) {
    const p = { ...parsed.value, key: keyName };
    emit("update:modelValue", buildHotkeyString(p));
  }
}

function mapCharToName(ch: string): string | null {
  if (/[a-z]/.test(ch)) return ch;
  if (/[0-9]/.test(ch)) return ch;
  const symbols: Record<string, string> = {
    "`": "`",
    "~": "`",
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
  return symbols[ch] || null;
}

function restoreDefault() {
  emit("update:modelValue", props.defaultValue);
}
</script>

<style scoped>
.composer-enable {
  display: flex;
  align-items: center;
  gap: 4px;
  cursor: pointer;
  font-size: 13px;
  color: #374151;
  user-select: none;
  margin-right: 8px;
  white-space: nowrap;
}
.composer-enable input {
  width: 15px;
  height: 15px;
  cursor: pointer;
  accent-color: var(--accent-color, #2563eb);
}
.hotkey-composer {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 4px 8px;
  border: 1px solid var(--border-color, #e5e7eb);
  border-radius: 8px;
  background: var(--input-bg, #fff);
  transition: opacity 0.2s;
}
.hotkey-composer.disabled {
  opacity: 0.4;
  pointer-events: none;
}
.composer-modifier {
  display: flex;
  align-items: center;
  gap: 3px;
  font-size: 12px;
  color: #374151;
  cursor: pointer;
  padding: 2px 4px;
  border-radius: 4px;
  user-select: none;
  transition: background 0.15s;
}
.composer-modifier:hover {
  background: #f3f4f6;
}
.composer-modifier input {
  width: 14px;
  height: 14px;
  cursor: pointer;
  accent-color: var(--accent-color, #2563eb);
}
.composer-modifier input:disabled {
  cursor: not-allowed;
}
.composer-plus {
  font-size: 12px;
  color: #9ca3af;
  user-select: none;
}
.composer-plus.dimmed {
  color: #d1d5db;
}
.composer-key-input {
  width: 48px;
  height: 28px;
  text-align: center;
  padding: 0 4px;
  border: 1px solid #d1d5db;
  border-radius: 5px;
  font-size: 13px;
  font-weight: 500;
  color: #1f2937;
  background: #f9fafb;
  outline: none;
  transition:
    border-color 0.2s,
    box-shadow 0.2s;
}
.composer-key-input:focus {
  border-color: var(--primary-color, #2563eb);
  box-shadow: 0 0 0 2px rgba(37, 99, 235, 0.18);
}
.composer-key-input.empty {
  color: #d1d5db;
}
.composer-key-input:disabled {
  cursor: not-allowed;
  background: #f3f4f6;
}
.composer-reset-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  padding: 0;
  background: none;
  border: 1px solid transparent;
  border-radius: 4px;
  cursor: default;
  color: #d1d5db;
  transition:
    color 0.15s,
    background 0.15s;
  flex-shrink: 0;
  margin-left: 4px;
}
.composer-reset-btn.changed {
  color: var(--text-secondary, #9ca3af);
  cursor: pointer;
}
.composer-reset-btn.changed:hover {
  color: var(--accent-color, #2563eb);
  background: rgba(37, 99, 235, 0.08);
}
.composer-reset-btn:disabled {
  cursor: default;
}
</style>
