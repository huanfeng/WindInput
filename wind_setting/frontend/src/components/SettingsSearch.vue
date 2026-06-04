<script setup lang="ts">
import { ref, computed } from "vue";
import { searchIndex } from "@/searchIndex";
import { filterEntries, type SearchEntry } from "@/schemas/searchEntry";

const emit = defineEmits<{ (e: "jump", entry: SearchEntry): void }>();

const query = ref("");
const open = ref(false);
const activeIndex = ref(0);
const inputRef = ref<HTMLInputElement | null>(null);

const results = computed(() =>
  filterEntries(searchIndex, query.value).slice(0, 30),
);

function onInput() {
  open.value = query.value.trim().length > 0;
  activeIndex.value = 0;
}

function choose(entry: SearchEntry) {
  emit("jump", entry);
  open.value = false;
}

function move(delta: number) {
  if (!results.value.length) return;
  const n = results.value.length;
  activeIndex.value = (activeIndex.value + delta + n) % n;
}

function onEnter() {
  const e = results.value[activeIndex.value];
  if (e) choose(e);
}

function onEsc() {
  if (open.value) {
    open.value = false;
  } else {
    query.value = "";
  }
}

function clearQuery() {
  query.value = "";
  open.value = false;
  inputRef.value?.focus();
}

function focus() {
  inputRef.value?.focus();
}

defineExpose({ focus });

// 失焦延迟关闭，避免点击结果项前下拉先消失
function onBlur() {
  setTimeout(() => (open.value = false), 120);
}
</script>

<template>
  <div class="settings-search">
    <div class="search-input-wrap">
      <input
        ref="inputRef"
        v-model="query"
        class="search-input"
        type="text"
        placeholder="🔍 搜索设置…"
        @input="onInput"
        @focus="onInput"
        @blur="onBlur"
        @keydown.down.prevent="move(1)"
        @keydown.up.prevent="move(-1)"
        @keydown.enter.prevent="onEnter"
        @keydown.esc.prevent="onEsc"
      />
      <button
        v-if="query"
        type="button"
        class="search-clear-btn"
        @mousedown.prevent="clearQuery"
      >
        ×
      </button>
    </div>

    <div v-if="open" class="search-results">
      <div v-if="!results.length" class="search-empty">未找到匹配设置</div>
      <button
        v-for="(entry, i) in results"
        :key="entry.id"
        class="search-result-item"
        :class="{ active: i === activeIndex }"
        @mousedown.prevent="choose(entry)"
        @mouseenter="activeIndex = i"
      >
        <span class="result-title">{{ entry.title }}</span>
        <span class="result-crumb"
          >{{ entry.tabLabel }} › {{ entry.card }}</span
        >
      </button>
    </div>
  </div>
</template>

<style scoped>
.settings-search {
  position: relative;
  padding: 8px 12px;
}
.search-input-wrap {
  position: relative;
  display: flex;
  align-items: center;
}
.search-input {
  width: 100%;
  box-sizing: border-box;
  padding: 6px 28px 6px 10px;
  font-size: 13px;
  border: 1px solid hsl(var(--border, 0 0% 85%));
  border-radius: 6px;
  background: var(--bg-card, #fff);
  color: inherit;
  outline: none;
}
.search-input:focus {
  border-color: hsl(var(--primary, 220 90% 56%));
}
.search-clear-btn {
  position: absolute;
  right: 6px;
  top: 50%;
  transform: translateY(-50%);
  border: none;
  background: transparent;
  padding: 0 2px;
  font-size: 14px;
  line-height: 1;
  cursor: pointer;
  color: hsl(var(--muted-foreground, 0 0% 45%));
}
.search-clear-btn:hover {
  color: hsl(var(--foreground, 0 0% 10%));
}
.search-results {
  position: absolute;
  left: 12px;
  right: 12px;
  top: calc(100% - 2px);
  z-index: 50;
  max-height: 320px;
  overflow-y: auto;
  background: var(--bg-card, #fff);
  border: 1px solid hsl(var(--border, 0 0% 85%));
  border-radius: 6px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.12);
}
.search-empty {
  padding: 10px 12px;
  font-size: 12px;
  color: hsl(var(--muted-foreground, 0 0% 45%));
}
.search-result-item {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  width: 100%;
  padding: 7px 12px;
  border: none;
  background: transparent;
  text-align: left;
  cursor: pointer;
}
.search-result-item.active,
.search-result-item:hover {
  background: hsl(var(--accent, 220 14% 96%));
}
.result-title {
  font-size: 13px;
}
.result-crumb {
  font-size: 11px;
  color: hsl(var(--muted-foreground, 0 0% 45%));
}
</style>
