<script setup lang="ts">
// SchemaEngineRenderer — 方案引擎设置 Schema 渲染器
//
// 渲染 SchemaSettingsDialog 内部的引擎设置项。
// 根据 engineType 和 activeTab 过滤字段，直接 mutation localConfig。

import { computed } from 'vue'
import type { EngineSchema, SchemaFieldDef, EngineType } from '@/schemas/schema-engine-types'
import { filterEngineSchema, getPath, setPath } from '@/schemas/schema-engine-types'
import { Switch } from '@/components/ui/switch'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

const props = defineProps<{
  schema: EngineSchema
  modelValue: Record<string, any>        // localConfig（SchemaConfig）
  engineType: EngineType
  activeTab?: 'basic' | 'advanced'
}>()

const visibleFields = computed(() =>
  filterEngineSchema(
    props.schema,
    props.engineType,
    props.activeTab ?? 'basic',
    props.modelValue as any,
  ),
)

function getValue(key: string): any {
  return getPath(props.modelValue, key)
}

function setValue(key: string, v: any): void {
  // 数字字段保持数字类型
  const orig = getPath(props.modelValue, key)
  if (typeof orig === 'number' && typeof v === 'string') {
    setPath(props.modelValue, key, Number(v))
  } else {
    setPath(props.modelValue, key, v)
  }
}

const EMPTY_SENTINEL = '__empty_select_value__'

function optVal(v: string | number): string {
  return v === '' ? EMPTY_SENTINEL : String(v)
}

function selectVal(field: { options: { value: string | number }[] }, key: string): string {
  const v = getValue(key)
  // null/undefined 或空字符串：若选项中无空字符串值，回退到第一个选项
  const s = v == null ? '' : String(v)
  if (s === '') {
    const hasEmptyOpt = field.options.some((o) => o.value === '')
    if (hasEmptyOpt) return EMPTY_SENTINEL
    return field.options.length > 0 ? optVal(field.options[0].value) : EMPTY_SENTINEL
  }
  return s
}

function onSelectChange(key: string, raw: string): void {
  const actual = raw === EMPTY_SENTINEL ? '' : raw
  setValue(key, actual)
}

function isDisabled(field: SchemaFieldDef): boolean {
  if (field.type === 'section') return false
  return field.dependsOn ? !field.dependsOn(props.modelValue as any) : false
}

function resolveHint(field: SchemaFieldDef): string {
  if (field.type === 'section') return ''
  const h = field.hint
  if (!h) return ''
  return typeof h === 'function' ? h(props.modelValue as any) : h
}
</script>

<template>
  <template v-for="field in visibleFields" :key="'key' in field ? field.key : field.label">
    <!-- 分节标题 -->
    <div v-if="field.type === 'section'" class="setting-section-title">{{ field.label }}</div>

    <!-- Toggle -->
    <div
      v-else-if="field.type === 'toggle'"
      class="setting-item"
      :class="{ 'item-disabled': isDisabled(field) }"
    >
      <div class="setting-info">
        <label>{{ field.label }}</label>
        <p v-if="resolveHint(field)" class="setting-hint">{{ resolveHint(field) }}</p>
      </div>
      <div class="setting-control">
        <Switch
          :checked="!!getValue(field.key)"
          :disabled="isDisabled(field)"
          @update:checked="setValue(field.key, $event)"
        />
      </div>
    </div>

    <!-- Select -->
    <div
      v-else-if="field.type === 'select'"
      class="setting-item"
      :class="{ 'item-disabled': isDisabled(field) }"
    >
      <div class="setting-info">
        <label>{{ field.label }}</label>
        <p v-if="resolveHint(field)" class="setting-hint">{{ resolveHint(field) }}</p>
      </div>
      <div class="setting-control">
        <Select
          :model-value="selectVal(field, field.key)"
          :disabled="isDisabled(field)"
          @update:model-value="onSelectChange(field.key, $event)"
        >
          <SelectTrigger :style="{ width: field.width || '140px' }">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem
              v-for="opt in field.options"
              :key="opt.value"
              :value="optVal(opt.value)"
            >
              {{ opt.label }}
            </SelectItem>
          </SelectContent>
        </Select>
      </div>
    </div>
  </template>
</template>
