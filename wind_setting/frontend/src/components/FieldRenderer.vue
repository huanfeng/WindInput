<script setup lang="ts">
import { computed } from "vue";
import type { FieldDef } from "@/schemas/types";
import { getPath, setPath } from "@/schemas/types";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const props = defineProps<{
  field: Exclude<FieldDef, { type: "card" } | { type: "section" }>;
  formData: Record<string, any>;
}>();

const value = computed(() => getPath(props.formData, props.field.key));

const isDisabled = computed(() => {
  const f = props.field as any;
  if (!f.dependsOn) return false;
  return !f.dependsOn(props.formData);
});

const resolvedHint = computed(() => {
  const hint = (props.field as any).hint;
  if (!hint) return "";
  return typeof hint === "function" ? hint(props.formData) : hint;
});

function setValue(v: any) {
  setPath(props.formData, props.field.key, v);
}

// Slider 显示值
const sliderDisplay = computed(() => {
  if (props.field.type !== "slider") return "";
  const f = props.field;
  const v = value.value as number;
  if (f.displayValue) return f.displayValue(v);
  return f.unit ? `${v}${f.unit}` : String(v);
});

// Radix Select 不支持 value=""（空字符串表示"未选中"），用哨兵替代
const EMPTY_SENTINEL = "__empty_select_value__";

function selectValue(v: any): string {
  const fallbackFirst = (): string => {
    if (props.field.type === "select" && props.field.options.length > 0) {
      return optValue(props.field.options[0].value);
    }
    return EMPTY_SENTINEL;
  };

  if (v == null) return fallbackFirst();

  const s = String(v);
  if (s === "") {
    // 只有选项中存在 value="" 时才使用哨兵（如 pager_display_mode Default 选项）
    // 否则空字符串视为"未配置"，回退到第一个选项作为视觉默认
    const hasEmptyOpt =
      props.field.type === "select" &&
      props.field.options.some((o) => o.value === "");
    return hasEmptyOpt ? EMPTY_SENTINEL : fallbackFirst();
  }
  return s;
}

function optValue(v: string | number): string {
  return v === "" ? EMPTY_SENTINEL : String(v);
}

function onSelectChange(raw: string) {
  const actual = raw === EMPTY_SENTINEL ? "" : raw;
  const orig = getPath(props.formData, props.field.key);
  if (typeof orig === "number") {
    setValue(Number(actual));
  } else {
    setValue(actual);
  }
}
</script>

<template>
  <div
    v-if="field.type === 'toggle'"
    class="setting-item"
    :class="{ 'item-disabled': isDisabled }"
  >
    <div class="setting-info">
      <label>{{ field.label }}</label>
      <p v-if="resolvedHint" class="setting-hint">{{ resolvedHint }}</p>
    </div>
    <div class="setting-control">
      <Switch
        :checked="!!value"
        :disabled="isDisabled"
        @update:checked="setValue($event)"
      />
    </div>
  </div>

  <div
    v-else-if="field.type === 'select'"
    class="setting-item"
    :class="{ 'item-disabled': isDisabled }"
  >
    <div class="setting-info">
      <label>{{ field.label }}</label>
      <p v-if="resolvedHint" class="setting-hint">{{ resolvedHint }}</p>
    </div>
    <div class="setting-control">
      <Select
        :model-value="selectValue(value)"
        :disabled="isDisabled"
        @update:model-value="onSelectChange($event)"
      >
        <SelectTrigger :style="{ width: field.width || '160px' }">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem
            v-for="opt in field.options"
            :key="opt.value"
            :value="optValue(opt.value)"
          >
            <template v-if="opt.description || opt.tag">
              <div class="flex flex-col gap-0.5">
                <div class="flex items-center gap-2">
                  <span>{{ opt.label }}</span>
                  <span
                    v-if="opt.tag"
                    class="text-[10px] px-1 rounded bg-primary/10 text-primary"
                    >{{ opt.tag }}</span
                  >
                </div>
                <span
                  v-if="opt.description"
                  class="text-xs text-muted-foreground"
                  >{{ opt.description }}</span
                >
              </div>
            </template>
            <template v-else>{{ opt.label }}</template>
          </SelectItem>
        </SelectContent>
      </Select>
    </div>
  </div>

  <div
    v-else-if="field.type === 'slider'"
    class="setting-item"
    :class="{ 'item-disabled': isDisabled }"
  >
    <div class="setting-info">
      <label>{{ field.label }}</label>
      <p v-if="resolvedHint" class="setting-hint">{{ resolvedHint }}</p>
    </div>
    <div class="setting-control range-control">
      <input
        type="range"
        :min="field.min"
        :max="field.max"
        :step="field.step"
        :value="value"
        :disabled="isDisabled"
        @input="setValue(Number(($event.target as HTMLInputElement).value))"
      />
      <span class="range-value">{{ sliderDisplay }}</span>
    </div>
  </div>

  <div
    v-else-if="field.type === 'number-input'"
    class="setting-item"
    :class="{ 'item-disabled': isDisabled }"
  >
    <div class="setting-info">
      <label>{{ field.label }}</label>
      <p v-if="resolvedHint" class="setting-hint">{{ resolvedHint }}</p>
    </div>
    <div class="setting-control">
      <input
        type="number"
        class="number-input"
        :value="value"
        :min="field.min"
        :max="field.max"
        :step="field.step ?? 1"
        :disabled="isDisabled"
        @change="setValue(Number(($event.target as HTMLInputElement).value))"
      />
    </div>
  </div>
</template>
