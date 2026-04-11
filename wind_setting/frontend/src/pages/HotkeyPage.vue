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
          <p class="setting-hint">选中第2、3位候选词的快捷键</p>
        </div>
        <div class="setting-control">
          <div class="checkbox-group key-group-grid">
            <label
              class="checkbox-item"
              v-for="group in [
                { value: 'semicolon_quote', label: '; / \'', tip: '分号/引号' },
                { value: 'comma_period', label: ', / .', tip: '逗号/句号' },
                { value: 'lrshift', label: 'L / R Shift', tip: '左Shift/右Shift' },
                { value: 'lrctrl', label: 'L / R Ctrl', tip: '左Ctrl/右Ctrl' },
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
                { value: 'arrows', label: '↑ / ↓', tip: '上/下方向键' },
                { value: 'tab', label: 'Tab / Shift+Tab', tip: 'Tab键/Shift+Tab键' },
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
                { value: 'pageupdown', label: 'PgUp / PgDn', tip: '上翻页/下翻页' },
                { value: 'minus_equal', label: '- / =', tip: '减号/等号' },
                { value: 'brackets', label: '[ / ]', tip: '左方括号/右方括号' },
                { value: 'shift_tab', label: 'Shift+Tab / Tab', tip: 'Shift+Tab键/Tab键' },
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
                { value: 'comma_period', label: ', / .', tip: '逗号/句号' },
                { value: 'minus_equal', label: '- / =', tip: '减号/等号' },
                { value: 'brackets', label: '[ / ]', tip: '左方括号/右方括号' },
              ]"
              :key="sc.value"
              :title="sc.tip"
            >
              <input
                type="checkbox"
                :checked="
                  formData.input.select_char_keys.includes(sc.value)
                "
                @change="toggleSelectCharKey(sc.value)"
              />
              <span>{{ sc.label }}</span>
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
        <div
          class="setting-control"
          style="flex-direction: column; align-items: flex-start"
        >
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
          <div style="margin-top: 4px">
            <div class="checkbox-group">
              <label class="checkbox-item">
                <input
                  type="checkbox"
                  :checked="
                    formData.input.temp_pinyin.trigger_keys.includes('z')
                  "
                  @change="
                    toggleArrayValue(
                      formData.input.temp_pinyin.trigger_keys,
                      'z',
                    )
                  "
                />
                <span>z 键</span>
              </label>
            </div>
            <p
              v-if="formData.input.temp_pinyin.trigger_keys.includes('z')"
              class="setting-hint warning-hint"
            >
              z 开头的编码将无法输入
            </p>
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

function toggleSelectKeyGroup(value: string) {
  toggleArrayValue(props.formData.input.select_key_groups, value);
  // 二三候选键 comma_period 与以词定字 comma_period 互斥
  if (
    value === "comma_period" &&
    props.formData.input.select_key_groups.includes("comma_period")
  ) {
    removeFromArray(props.formData.input.select_char_keys, "comma_period");
  }
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
  // 翻页键与以词定字互斥: minus_equal / brackets
  if (
    (value === "minus_equal" || value === "brackets") &&
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
  if (value === "comma_period") {
    // 与二三候选键 comma_period 冲突
    removeFromArray(props.formData.input.select_key_groups, "comma_period");
  } else if (value === "minus_equal") {
    // 与翻页键 minus_equal 冲突
    removeFromArray(props.formData.input.page_keys, "minus_equal");
  } else if (value === "brackets") {
    // 与翻页键 brackets 冲突
    removeFromArray(props.formData.input.page_keys, "brackets");
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
