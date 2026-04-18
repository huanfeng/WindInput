<script setup lang="ts">
import { computed } from "vue";
import { Badge } from "@/components/ui/badge";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuCheckboxItem,
} from "@/components/ui/dropdown-menu";

interface KeyOption {
  value: string;
  label: string;
}

const props = withDefaults(
  defineProps<{
    /** 所有可选的按键列表 */
    options: KeyOption[];
    /** 当前选中的按键（多选） */
    modelValue: string[];
    /** 是否单选模式（快捷输入用） */
    single?: boolean;
    /** 占位文本 */
    placeholder?: string;
    /** 是否禁用 */
    disabled?: boolean;
    /** 冲突按键 → 冲突说明 */
    conflicts?: Map<string, string>;
  }>(),
  {
    single: false,
    placeholder: "点击选择触发键",
    disabled: false,
    conflicts: () => new Map(),
  },
);

const emit = defineEmits<{
  "update:modelValue": [value: string[]];
}>();

const selectedLabels = computed(() => {
  return props.modelValue
    .map((v) => {
      const opt = props.options.find((o) => o.value === v);
      return opt ? opt.label : v;
    })
    .filter(Boolean);
});

function toggleKey(key: string) {
  if (props.disabled) return;
  if (props.single) {
    // 单选：直接替换
    emit("update:modelValue", [key]);
    return;
  }
  const arr = [...props.modelValue];
  const idx = arr.indexOf(key);
  if (idx >= 0) {
    arr.splice(idx, 1);
  } else {
    arr.push(key);
  }
  emit("update:modelValue", arr);
}

function removeKey(key: string) {
  if (props.disabled) return;
  if (props.single) return; // 单选不能移除
  const arr = props.modelValue.filter((v) => v !== key);
  emit("update:modelValue", arr);
}

function getConflict(key: string): string | undefined {
  return props.conflicts?.get(key);
}
</script>

<template>
  <div class="trigger-key-select" :class="{ disabled }">
    <DropdownMenu>
      <DropdownMenuTrigger as-child :disabled="disabled">
        <button class="trigger-btn" :disabled="disabled">
          <span v-if="modelValue.length === 0" class="placeholder">{{
            placeholder
          }}</span>
          <span v-else class="tags">
            <Badge
              v-for="key in modelValue"
              :key="key"
              variant="secondary"
              class="tag-badge"
            >
              {{ options.find((o) => o.value === key)?.label || key }}
              <span
                v-if="!single"
                class="tag-remove"
                @click.stop="removeKey(key)"
                >&times;</span
              >
            </Badge>
          </span>
          <svg
            class="chevron"
            xmlns="http://www.w3.org/2000/svg"
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <polyline points="6 9 12 15 18 9" />
          </svg>
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent class="trigger-key-dropdown" align="start">
        <DropdownMenuCheckboxItem
          v-for="opt in options"
          :key="opt.value"
          :checked="modelValue.includes(opt.value)"
          @update:checked="toggleKey(opt.value)"
          class="key-option-item"
        >
          <span class="key-option-content">
            <span class="key-option-label">{{ opt.label }}</span>
            <span v-if="getConflict(opt.value)" class="key-conflict-hint">
              {{ getConflict(opt.value) }}
            </span>
          </span>
        </DropdownMenuCheckboxItem>
      </DropdownMenuContent>
    </DropdownMenu>
  </div>
</template>

<style scoped>
.trigger-key-select {
  min-width: 200px;
}
.trigger-key-select.disabled {
  opacity: 0.5;
  pointer-events: none;
}
.trigger-btn {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  min-height: 36px;
  padding: 6px 12px;
  border: 1px solid hsl(var(--input));
  border-radius: calc(var(--radius) - 2px);
  background: transparent;
  cursor: pointer;
  font-size: 14px;
  color: inherit;
  box-shadow: 0 1px 2px 0 rgb(0 0 0 / 0.05);
  transition:
    border-color 0.15s,
    box-shadow 0.15s;
}
.trigger-btn:hover {
  border-color: hsl(var(--ring));
}
.trigger-btn:focus {
  outline: none;
  box-shadow: 0 0 0 1px hsl(var(--ring));
}
.trigger-btn:disabled {
  cursor: not-allowed;
  opacity: 0.5;
}
.placeholder {
  color: var(--text-muted, #999);
  flex: 1;
}
.tags {
  display: grid;
  grid-template-columns: repeat(2, auto);
  gap: 4px;
  flex: 1;
  justify-content: start;
}
.tag-badge {
  font-size: 12px;
  padding: 1px 6px;
  gap: 2px;
  white-space: nowrap;
}
.tag-remove {
  cursor: pointer;
  margin-left: 2px;
  font-size: 14px;
  line-height: 1;
  opacity: 0.6;
}
.tag-remove:hover {
  opacity: 1;
}
.chevron {
  flex-shrink: 0;
  opacity: 0.5;
}
.trigger-key-dropdown {
  min-width: 220px;
}
:deep(.key-option-item) {
  align-items: flex-start !important;
}
.key-option-content {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.key-option-label {
  white-space: nowrap;
}
.key-conflict-hint {
  font-size: 11px;
  color: hsl(var(--warning, 25 95% 40%));
  line-height: 1.3;
}
</style>
