<script setup lang="ts">
import type { SchemaConfig, SchemaInfo, SchemaReference } from "../api/wails";

const props = defineProps<{
  schema: SchemaInfo;
  config?: SchemaConfig;
  references?: SchemaReference;
}>();

const engineTypeLabels: Record<string, string> = {
  codetable: "码表",
  pinyin: "拼音",
  mixed: "混输",
};

const sourceLabels: Record<string, string> = {
  builtin: "内置",
  user: "用户",
  online: "在线",
};

function getEngineLabel(): string {
  return engineTypeLabels[props.schema.engine_type] || props.schema.engine_type || "未知";
}

function getSourceLabel(): string {
  return sourceLabels[(props.schema as any).source] || "未知";
}

function getDictCount(): number {
  return props.config?.dictionaries?.length || 0;
}

function getDictSummary(): string {
  const dicts = props.config?.dictionaries;
  if (!dicts || dicts.length === 0) return "无";
  return dicts.map((d) => d.id || d.path).join(", ");
}

function getReferenceInfo(): string {
  const ref = props.references;
  if (!ref) return "";
  const parts: string[] = [];
  if (ref.primary_schema) parts.push(`主方案: ${ref.primary_schema}`);
  if (ref.secondary_schema) parts.push(`副方案: ${ref.secondary_schema}`);
  if (ref.temp_pinyin_schema) parts.push(`临时拼音: ${ref.temp_pinyin_schema}`);
  return parts.join(", ");
}

function getReferencedByInfo(): string {
  const ref = props.references;
  if (!ref?.referenced_by?.length) return "";
  return ref.referenced_by.join(", ");
}
</script>

<template>
  <div class="schema-detail">
    <div class="schema-detail-grid">
      <div class="schema-detail-row">
        <span class="schema-detail-label">方案 ID</span>
        <span class="schema-detail-value">{{ schema.id }}</span>
      </div>
      <div class="schema-detail-row">
        <span class="schema-detail-label">名称</span>
        <span class="schema-detail-value">{{ schema.name }}</span>
      </div>
      <div class="schema-detail-row">
        <span class="schema-detail-label">版本</span>
        <span class="schema-detail-value">{{ schema.version || "-" }}</span>
      </div>
      <div v-if="config?.schema?.author" class="schema-detail-row">
        <span class="schema-detail-label">作者</span>
        <span class="schema-detail-value">{{ config.schema.author }}</span>
      </div>
      <div class="schema-detail-row">
        <span class="schema-detail-label">引擎类型</span>
        <span class="schema-detail-value">{{ getEngineLabel() }}</span>
      </div>
      <div class="schema-detail-row">
        <span class="schema-detail-label">来源</span>
        <span class="schema-detail-value">
          <span class="schema-detail-source-badge" :class="'source-' + ((schema as any).source || 'builtin')">
            {{ getSourceLabel() }}
          </span>
        </span>
      </div>
      <div v-if="schema.description" class="schema-detail-row">
        <span class="schema-detail-label">描述</span>
        <span class="schema-detail-value">{{ schema.description }}</span>
      </div>
      <div class="schema-detail-row">
        <span class="schema-detail-label">词典</span>
        <span class="schema-detail-value">{{ getDictCount() }} 个<template v-if="getDictCount() > 0"> ({{ getDictSummary() }})</template></span>
      </div>
      <div v-if="getReferenceInfo()" class="schema-detail-row">
        <span class="schema-detail-label">引用</span>
        <span class="schema-detail-value">{{ getReferenceInfo() }}</span>
      </div>
      <div v-if="getReferencedByInfo()" class="schema-detail-row">
        <span class="schema-detail-label">被引用</span>
        <span class="schema-detail-value">{{ getReferencedByInfo() }}</span>
      </div>
      <div v-if="schema.error" class="schema-detail-row">
        <span class="schema-detail-label">异常</span>
        <span class="schema-detail-value schema-detail-error">{{ schema.error }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.schema-detail {
  padding: 12px 14px;
  background: var(--bg-secondary, #f9fafb);
  border-radius: 6px;
}
.schema-detail-grid {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.schema-detail-row {
  display: flex;
  align-items: baseline;
  gap: 12px;
  font-size: 13px;
  line-height: 1.5;
}
.schema-detail-label {
  flex-shrink: 0;
  width: 60px;
  color: #9ca3af;
  text-align: right;
}
.schema-detail-value {
  color: #374151;
  word-break: break-all;
}
.schema-detail-error {
  color: #dc2626;
}
.schema-detail-source-badge {
  display: inline-block;
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 4px;
}
.schema-detail-source-badge.source-builtin {
  background: #eff6ff;
  color: #2563eb;
}
.schema-detail-source-badge.source-user {
  background: #f0fdf4;
  color: #16a34a;
}
.schema-detail-source-badge.source-online {
  background: #fefce8;
  color: #ca8a04;
}
</style>
