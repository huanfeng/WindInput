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
              v-for="key in [Key.LShift, Key.RShift, Key.LCtrl, Key.RCtrl, Key.CapsLock]"
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
          <Switch
            :checked="formData.hotkeys.commit_on_switch"
            @update:checked="formData.hotkeys.commit_on_switch = $event"
          />
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
          <Select
            :model-value="formData.hotkeys.pin_candidate"
            @update:model-value="formData.hotkeys.pin_candidate = $event"
          >
            <SelectTrigger class="w-[200px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="ctrl+number">Ctrl + 数字</SelectItem>
              <SelectItem value="ctrl+shift+number"
                >Ctrl + Shift + 数字</SelectItem
              >
              <SelectItem value="none">不使用</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>删除词条</label>
          <p class="setting-hint">隐藏候选词（单字不可删除）</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.hotkeys.delete_candidate"
            @update:model-value="formData.hotkeys.delete_candidate = $event"
          >
            <SelectTrigger class="w-[200px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="ctrl+shift+number"
                >Ctrl + Shift + 数字</SelectItem
              >
              <SelectItem value="ctrl+number">Ctrl + 数字</SelectItem>
              <SelectItem value="none">不使用</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
    </div>

    <!-- 候选操作 -->
    <div class="settings-card">
      <div class="card-title">候选操作</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>次选/三选快捷键</label>
          <p class="setting-hint">选中第2、3位候选词的快捷键</p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group key-group-grid">
            <label
              class="checkbox-item"
              v-for="group in [
                { value: PairGroup.SemicolonQuote, label: '; / \'', tip: '分号/引号' },
                { value: PairGroup.CommaPeriod, label: ', / .', tip: '逗号/句号' },
                {
                  value: PairGroup.LRShift,
                  label: 'L / R Shift',
                  tip: '左Shift/右Shift',
                },
                { value: PairGroup.LRCtrl, label: 'L / R Ctrl', tip: '左Ctrl/右Ctrl' },
              ]"
              :key="group.value"
              :title="group.tip"
            >
              <input
                type="checkbox"
                :checked="
                  formData.input.select_key_groups.includes(group.value)
                "
                @change="toggleSelectKeyGroup(group.value)"
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
            在候选列表中移动选中项
            <br />Tab/Shift+Tab 与翻页键互斥
          </p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group key-group-grid">
            <label
              class="checkbox-item"
              v-for="hk in [
                { value: PairGroup.Arrows, label: '↑ / ↓', tip: '上/下方向键' },
                {
                  value: PairGroup.Tab,
                  label: 'Tab / Shift+Tab',
                  tip: 'Tab键/Shift+Tab键',
                },
              ]"
              :key="hk.value"
              :title="hk.tip"
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
          <p class="setting-hint">同时启用多组翻页键</p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group key-group-grid">
            <label
              class="checkbox-item"
              v-for="pk in [
                {
                  value: PairGroup.PageUpDown,
                  label: 'PgUp / PgDn',
                  tip: '上翻页/下翻页',
                },
                { value: PairGroup.MinusEqual, label: '- / =', tip: '减号/等号' },
                { value: PairGroup.Brackets, label: '[ / ]', tip: '左方括号/右方括号' },
                {
                  value: PairGroup.ShiftTab,
                  label: 'Shift+Tab / Tab',
                  tip: 'Shift+Tab键/Tab键',
                },
              ]"
              :key="pk.value"
              :title="pk.tip"
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
      <div class="setting-item">
        <div class="setting-info">
          <label>以词定字</label>
          <p class="setting-hint">
            输入词组后按指定键只取第1或第2个字
            <br />与翻页/候选键互斥，启用后自动取消冲突项
          </p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group key-group-grid">
            <label
              class="checkbox-item"
              v-for="sc in [
                { value: PairGroup.CommaPeriod, label: ', / .', tip: '逗号/句号' },
                { value: PairGroup.MinusEqual, label: '- / =', tip: '减号/等号' },
                { value: PairGroup.Brackets, label: '[ / ]', tip: '左方括号/右方括号' },
              ]"
              :key="sc.value"
              :title="sc.tip"
            >
              <input
                type="checkbox"
                :checked="formData.input.select_char_keys.includes(sc.value)"
                @change="toggleSelectCharKey(sc.value)"
              />
              <span>{{ sc.label }}</span>
            </label>
          </div>
        </div>
      </div>
    </div>

    <!-- 功能快捷键 -->
    <div class="settings-card">
      <div class="card-title">功能快捷键</div>
      <HotkeyComposer
        v-for="item in composerItems"
        :key="item.field"
        :label="item.label"
        :hint="item.hint"
        :model-value="getHotkeyValue(item.field)"
        :default-value="getDefaultValue(item.field)"
        :show-global="item.field === 'open_settings'"
        :is-global="isGlobalHotkey(item.field)"
        @update:model-value="setHotkeyValue(item.field, $event)"
        @update:global="setGlobalHotkey(item.field, $event)"
      />
    </div>
  </section>
</template>

<script setup lang="ts">
import { watch, computed } from "vue";
import type { Config, HotkeyConfig } from "../api/settings";
import { getDefaultConfig } from "../api/settings";
import HotkeyComposer from "../components/HotkeyComposer.vue";
import { Key, PairGroup } from "@/lib/enums";
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
  hotkeyConflicts: string[];
  systemDefaults?: Config;
}>();

const emit = defineEmits<{
  "update:hotkeyConflicts": [conflicts: string[]];
}>();

// 功能快捷键定义（统一使用 HotkeyComposer）
const composerItems = [
  {
    field: "switch_engine",
    label: "切换输入方案",
    hint: "在已启用的输入方案间循环切换",
  },
  {
    field: "toggle_full_width",
    label: "切换全角/半角",
    hint: "切换字符宽度模式",
  },
  {
    field: "toggle_punct",
    label: "切换中/英文标点",
    hint: "切换标点符号类型",
  },
  {
    field: "toggle_toolbar",
    label: "显示/隐藏状态栏",
    hint: "切换状态栏的显示状态",
  },
  { field: "open_settings", label: "打开设置", hint: "打开设置窗口" },
  {
    field: "add_word",
    label: "快捷加词",
    hint: "快速将输入的内容加入用户词库",
  },
  {
    field: "toggle_s2t",
    label: "切换简入繁出",
    hint: "开关简体输入→繁体输出（变体在「输入 → 简入繁出」中选择）",
  },
];

// 默认值缓存（优先使用系统默认配置）
const defaults = computed<HotkeyConfig>(
  () => props.systemDefaults?.hotkeys || getDefaultConfig().hotkeys,
);

// 候选操作快捷键冲突检测
const candidateActionConflict = computed(() => {
  const pin = props.formData.hotkeys.pin_candidate;
  const del = props.formData.hotkeys.delete_candidate;
  return pin !== "none" && del !== "none" && pin === del;
});

// --- HotkeyComposer 辅助方法 ---

function getHotkeyValue(field: string): string {
  return (props.formData.hotkeys as any)[field] || "none";
}

function getDefaultValue(field: string): string {
  return (defaults.value as any)[field] || "none";
}

function setHotkeyValue(field: string, value: string) {
  // 冲突解决：如果其他功能快捷键使用了相同的组合，自动清除旧绑定
  if (value !== "none") {
    for (const item of composerItems) {
      if (item.field !== field && getHotkeyValue(item.field) === value) {
        (props.formData.hotkeys as any)[item.field] = "none";
      }
    }
  }
  (props.formData.hotkeys as any)[field] = value;
}

function isGlobalHotkey(field: string): boolean {
  return props.formData.hotkeys.global_hotkeys.includes(field);
}

function setGlobalHotkey(field: string, enabled: boolean) {
  const list = props.formData.hotkeys.global_hotkeys;
  const idx = list.indexOf(field);
  if (enabled && idx < 0) {
    list.push(field);
  } else if (!enabled && idx >= 0) {
    list.splice(idx, 1);
  }
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
    case PairGroup.SemicolonQuote:
      return [Key.Semicolon, Key.Quote];
    case PairGroup.CommaPeriod:
      return [Key.Comma, Key.Period];
    case PairGroup.LRShift:
      return [Key.LShift, Key.RShift];
    case PairGroup.LRCtrl:
      return [Key.LCtrl, Key.RCtrl];
    default:
      return [];
  }
}

function getKeyLabel(key: string): string {
  const labels: Record<string, string> = {
    [Key.LShift]: "左Shift",
    [Key.RShift]: "右Shift",
    [Key.LCtrl]: "左Ctrl",
    [Key.RCtrl]: "右Ctrl",
    [Key.CapsLock]: "CapsLock",
    [Key.Semicolon]: ";",
    [Key.Quote]: "'",
    [Key.Comma]: ",",
    [Key.Period]: ".",
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

function toggleSelectKeyGroup(value: string) {
  toggleArrayValue(props.formData.input.select_key_groups, value);
  // 二三候选键 comma_period 与以词定字 comma_period 互斥
  if (
    value === PairGroup.CommaPeriod &&
    props.formData.input.select_key_groups.includes(PairGroup.CommaPeriod)
  ) {
    removeFromArray(props.formData.input.select_char_keys, PairGroup.CommaPeriod);
  }
}

function toggleHighlightKey(value: string) {
  toggleArrayValue(props.formData.input.highlight_keys, value);
  if (value === PairGroup.Tab && props.formData.input.highlight_keys.includes(PairGroup.Tab)) {
    const idx = props.formData.input.page_keys.indexOf(PairGroup.ShiftTab);
    if (idx >= 0) {
      props.formData.input.page_keys.splice(idx, 1);
    }
  }
}

function togglePageKey(value: string) {
  toggleArrayValue(props.formData.input.page_keys, value);
  if (
    value === PairGroup.ShiftTab &&
    props.formData.input.page_keys.includes(PairGroup.ShiftTab)
  ) {
    const idx = props.formData.input.highlight_keys.indexOf(PairGroup.Tab);
    if (idx >= 0) {
      props.formData.input.highlight_keys.splice(idx, 1);
    }
  }
  // 翻页键与以词定字互斥: minus_equal / brackets
  if (
    (value === PairGroup.MinusEqual || value === PairGroup.Brackets) &&
    props.formData.input.page_keys.includes(value)
  ) {
    removeFromArray(props.formData.input.select_char_keys, value);
  }
}

function toggleSelectCharKey(value: string) {
  toggleArrayValue(props.formData.input.select_char_keys, value);
  if (!props.formData.input.select_char_keys.includes(value)) {
    // 取消选择，无需处理冲突
    checkConflicts();
    return;
  }
  // 启用以词定字时，自动移除冲突的按键绑定
  if (value === PairGroup.CommaPeriod) {
    // 与二三候选键 comma_period 冲突
    removeFromArray(props.formData.input.select_key_groups, PairGroup.CommaPeriod);
  } else if (value === PairGroup.MinusEqual) {
    // 与翻页键 minus_equal 冲突
    removeFromArray(props.formData.input.page_keys, PairGroup.MinusEqual);
  } else if (value === PairGroup.Brackets) {
    // 与翻页键 brackets 冲突
    removeFromArray(props.formData.input.page_keys, PairGroup.Brackets);
  }
  checkConflicts();
}

function removeFromArray(arr: string[], value: string) {
  const idx = arr.indexOf(value);
  if (idx >= 0) {
    arr.splice(idx, 1);
  }
}

watch(
  () => [
    props.formData.hotkeys.toggle_mode_keys,
    props.formData.input.select_key_groups,
    props.formData.input.highlight_keys,
    props.formData.input.select_char_keys,
  ],
  checkConflicts,
  { deep: true },
);
</script>

<style scoped>
.key-group-grid {
  grid-template-columns: repeat(2, 130px);
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
