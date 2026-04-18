<template>
  <section class="section">
    <div class="section-header">
      <h2>输入习惯</h2>
      <p class="section-desc">定制您的打字体验</p>
    </div>

    <!-- 字符与标点 -->
    <div class="settings-card">
      <div class="card-title">字符与标点</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>候选检索范围</label>
          <p class="setting-hint">过滤候选词中的生僻字</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.filter_mode"
            @update:model-value="selectFilterMode($event)"
          >
            <SelectTrigger class="w-[200px]">
              <SelectValue :placeholder="'选择范围'">
                {{ currentFilterOption?.label || "选择范围" }}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                v-for="opt in filterModeOptions"
                :key="opt.value"
                :value="opt.value"
              >
                <div class="flex flex-col gap-0.5">
                  <div class="flex items-center gap-2">
                    <span>{{ opt.label }}</span>
                    <span
                      v-if="opt.tag"
                      class="text-[10px] px-1 rounded bg-primary/10 text-primary"
                      >{{ opt.tag }}</span
                    >
                  </div>
                  <span class="text-xs text-muted-foreground">{{
                    opt.desc
                  }}</span>
                </div>
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>标点随中英文切换</label>
          <p class="setting-hint">切换到中文模式时自动切换中文标点</p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.input.punct_follow_mode"
            @update:checked="formData.input.punct_follow_mode = $event"
          />
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>数字后智能标点</label>
          <p class="setting-hint">
            数字后句号输出点号、逗号输出英文逗号，方便输入 IP、小数、千分位等
          </p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.input.smart_punct_after_digit"
            @update:checked="formData.input.smart_punct_after_digit = $event"
          />
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>自定义标点映射</label>
          <p class="setting-hint">自定义英文标点对应的中文标点、全角符号</p>
        </div>
        <div class="setting-control inline-control">
          <label class="checkbox-label">
            <input
              type="checkbox"
              v-model="formData.input.punct_custom.enabled"
            />
            启用
          </label>
          <Button
            variant="outline"
            size="sm"
            :disabled="!formData.input.punct_custom.enabled"
            @click="openPunctCustomDialog()"
          >
            配置
          </Button>
        </div>
      </div>
    </div>

    <!-- 自定义标点映射对话框 -->
    <Dialog
      :open="showPunctCustomDialog"
      @update:open="
        (v: boolean) => {
          if (!v) cancelPunctCustom();
        }
      "
    >
      <DialogContent class="max-w-[600px]">
        <DialogHeader>
          <DialogTitle>自定义标点设置</DialogTitle>
        </DialogHeader>
        <div>
          <p class="dialog-hint">双击单元格修改，长度 1-8</p>
          <div class="punct-table-wrap">
            <table class="punct-table">
              <thead>
                <tr>
                  <th class="col-src">英文半角</th>
                  <th>中文半角</th>
                  <th>英文全角</th>
                  <th>中文全角</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="(row, ri) in punctEditRows" :key="row.key">
                  <td class="col-src">{{ row.src }}</td>
                  <td
                    v-for="(_, ci) in 3"
                    :key="ci"
                    class="col-edit"
                    :class="{
                      editing:
                        editingCell?.row === ri && editingCell?.col === ci,
                      modified: row.values[ci] !== row.defaults[ci],
                    }"
                    @dblclick="startEditCell(ri, ci)"
                  >
                    <input
                      v-if="editingCell?.row === ri && editingCell?.col === ci"
                      class="cell-input"
                      v-model="editingCell.value"
                      maxlength="8"
                      @keydown.enter="commitEditCell()"
                      @keydown.escape="cancelEditCell()"
                      @blur="commitEditCell()"
                      ref="cellInputRef"
                    />
                    <span v-else class="cell-text">{{ row.values[ci] }}</span>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
        <DialogFooter class="flex !justify-between">
          <Button
            variant="outline"
            size="sm"
            @click="resetPunctCustomDefaults()"
          >
            恢复默认
          </Button>
          <div class="flex gap-2">
            <Button variant="outline" size="sm" @click="cancelPunctCustom()"
              >取消</Button
            >
            <Button size="sm" @click="confirmPunctCustom()">确定</Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <!-- 按键行为 -->
    <div class="settings-card">
      <div class="card-title">按键行为</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>回车键功能</label>
          <p class="setting-hint">有编码时按回车键的处理方式</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.enter_behavior"
            @update:model-value="formData.input.enter_behavior = $event"
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="commit">上屏编码</SelectItem>
              <SelectItem value="clear">清空编码</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>空码时空格键功能</label>
          <p class="setting-hint">无候选词时按空格键的处理方式</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.space_on_empty_behavior"
            @update:model-value="
              formData.input.space_on_empty_behavior = $event
            "
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="commit">上屏编码</SelectItem>
              <SelectItem value="clear">清空编码</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>数字小键盘功能</label>
          <p class="setting-hint">
            控制小键盘数字键的行为，选择"同主键盘区数字"后可用于候选选择和快捷输入
          </p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.numpad_behavior || 'direct'"
            @update:model-value="formData.input.numpad_behavior = $event"
          >
            <SelectTrigger class="w-[200px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="direct">直接输入数字</SelectItem>
              <SelectItem value="follow_main">同主键盘区数字</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
    </div>

    <!-- 候选无效按键 -->
    <div class="settings-card">
      <div class="card-title">候选无效按键</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>数字键无效时</label>
          <p class="setting-hint">按的数字超出当前页候选数量时的处理方式</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.overflow_behavior.number_key"
            @update:model-value="
              formData.input.overflow_behavior.number_key = $event
            "
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="ignore">不起作用</SelectItem>
              <SelectItem value="commit">候选上屏</SelectItem>
              <SelectItem value="commit_and_input">顶码上屏</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>次选三选键无效时</label>
          <p class="setting-hint">候选数量不足时按次选或三选键的处理方式</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.overflow_behavior.select_key"
            @update:model-value="
              formData.input.overflow_behavior.select_key = $event
            "
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="ignore">不起作用</SelectItem>
              <SelectItem value="commit">候选上屏</SelectItem>
              <SelectItem value="commit_and_input">顶码上屏</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>以词定字键无效时</label>
          <p class="setting-hint">候选词长度不足时按以词定字键的处理方式</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.overflow_behavior.select_char_key"
            @update:model-value="
              formData.input.overflow_behavior.select_char_key = $event
            "
          >
            <SelectTrigger class="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="ignore">不起作用</SelectItem>
              <SelectItem value="commit">候选上屏</SelectItem>
              <SelectItem value="commit_and_input">顶码上屏</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
    </div>

    <!-- 标点配对 -->
    <div class="settings-card">
      <div class="card-title">标点配对</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>中文标点自动配对</label>
          <p class="setting-hint">
            输入左括号类标点时自动补全右标点（已启用
            {{ getEnabledPairCount("chinese") }} 组）
          </p>
        </div>
        <div class="setting-control inline-control">
          <label class="checkbox-label">
            <input type="checkbox" v-model="formData.input.auto_pair.chinese" />
            启用
          </label>
          <Button
            variant="outline"
            size="sm"
            :disabled="!formData.input.auto_pair.chinese"
            @click="openPairDialog('chinese')"
          >
            配置
          </Button>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>英文标点自动配对</label>
          <p class="setting-hint">
            英文模式或英文标点下自动配对括号（已启用
            {{ getEnabledPairCount("english") }} 组）
          </p>
        </div>
        <div class="setting-control inline-control">
          <label class="checkbox-label">
            <input type="checkbox" v-model="formData.input.auto_pair.english" />
            启用
          </label>
          <Button
            variant="outline"
            size="sm"
            :disabled="!formData.input.auto_pair.english"
            @click="openPairDialog('english')"
          >
            配置
          </Button>
        </div>
      </div>
    </div>

    <!-- 标点配对配置对话框 -->
    <Dialog :open="showPairDialog" @update:open="showPairDialog = $event">
      <DialogContent>
        <DialogHeader>
          <DialogTitle
            >{{
              pairDialogType === "chinese" ? "中文" : "英文"
            }}配对配置</DialogTitle
          >
        </DialogHeader>
        <div class="pair-items-grid">
          <label
            class="pair-item"
            v-for="item in currentPairOptions"
            :key="item.pair"
          >
            <input
              type="checkbox"
              :checked="isPairEnabled(item.pair)"
              @change="togglePair(item.pair)"
            />
            <span class="pair-symbol">{{ item.left }} {{ item.right }}</span>
            <span class="pair-desc">{{ item.desc }}</span>
          </label>
        </div>
        <DialogFooter>
          <Button variant="outline" size="sm" @click="setAllPairs(true)"
            >全选</Button
          >
          <Button variant="outline" size="sm" @click="setAllPairs(false)"
            >全不选</Button
          >
          <Button size="sm" @click="showPairDialog = false">确定</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <!-- 快捷输入 -->
    <div class="settings-card">
      <div class="card-title">快捷输入</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>启用快捷输入</label>
          <p class="setting-hint">
            空码时按触发键进入快捷输入模式，支持数字转大小写、金额、计算器、日期等
          </p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.input.quick_input.enabled"
            @update:checked="formData.input.quick_input.enabled = $event"
          />
        </div>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': !formData.input.quick_input.enabled }"
      >
        <div class="setting-info">
          <label>触发键</label>
          <p class="setting-hint">用于启动快捷输入的按键</p>
        </div>
        <div class="setting-control">
          <TriggerKeySelect
            :options="triggerKeyOptions"
            :model-value="[formData.input.quick_input.trigger_key]"
            @update:model-value="
              formData.input.quick_input.trigger_key = $event[0] || 'semicolon'
            "
            :single="true"
            :disabled="!formData.input.quick_input.enabled"
            :conflicts="quickInputConflicts"
            placeholder="选择触发键"
          />
        </div>
        <p v-if="quickInputConflictMsg" class="setting-item-warning">
          ⚠ {{ quickInputConflictMsg }}
        </p>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': !formData.input.quick_input.enabled }"
      >
        <div class="setting-info">
          <label>强制竖排显示</label>
          <p class="setting-hint">
            快捷输入时候选窗口强制使用竖排布局，退出后恢复原布局
          </p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.input.quick_input.force_vertical"
            @update:checked="formData.input.quick_input.force_vertical = $event"
            :disabled="!formData.input.quick_input.enabled"
          />
        </div>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': !formData.input.quick_input.enabled }"
      >
        <div class="setting-info">
          <label>小数保留位数</label>
          <p class="setting-hint">计算结果最多保留的小数位数（0 表示取整）</p>
        </div>
        <div class="setting-control">
          <input
            type="number"
            class="number-input"
            v-model.number="formData.input.quick_input.decimal_places"
            :disabled="!formData.input.quick_input.enabled"
            min="0"
            max="6"
          />
        </div>
      </div>
    </div>

    <!-- 临时拼音 -->
    <div class="settings-card">
      <div class="card-title">临时拼音</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>拼音分隔符</label>
          <p class="setting-hint">
            拼音模式下用于消歧的分隔符，如输入 xi'an 得到「西安」
          </p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.pinyin_separator"
            @update:model-value="formData.input.pinyin_separator = $event"
          >
            <SelectTrigger class="w-[280px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="auto"
                >自动（' 被选择键占用时改用 `）</SelectItem
              >
              <SelectItem value="quote">单引号 ( ' )</SelectItem>
              <SelectItem value="backtick">反引号 ( ` )</SelectItem>
              <SelectItem value="none">不使用</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>触发键</label>
          <p class="setting-hint">码表模式下按触发键临时切换拼音输入</p>
        </div>
        <div class="setting-control">
          <TriggerKeySelect
            :options="tempPinyinKeyOptions"
            :model-value="formData.input.temp_pinyin.trigger_keys"
            @update:model-value="
              formData.input.temp_pinyin.trigger_keys = $event
            "
            :conflicts="tempPinyinConflictMap"
            placeholder="选择触发键"
          />
        </div>
        <p
          v-if="formData.input.temp_pinyin.trigger_keys.includes('z')"
          class="setting-item-warning"
        >
          ⚠ z 键启用后，z 开头的编码将无法输入
        </p>
        <p v-if="tempPinyinConflictMsg" class="setting-item-warning">
          ⚠ {{ tempPinyinConflictMsg }}
        </p>
      </div>
    </div>

    <!-- 临时英文 -->
    <div class="settings-card">
      <div class="card-title">临时英文</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>Shift+字母行为</label>
          <p class="setting-hint">中文模式下按 Shift+字母时的行为</p>
        </div>
        <div class="setting-control">
          <Select
            :model-value="formData.input.shift_temp_english.shift_behavior"
            @update:model-value="
              formData.input.shift_temp_english.shift_behavior = $event
            "
          >
            <SelectTrigger class="w-[240px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="temp_english">进入临时英文模式</SelectItem>
              <SelectItem value="direct_commit">直接上屏大写字母</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>显示英文候选</label>
          <p class="setting-hint">临时英文模式下查询英文词库显示候选词</p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.input.shift_temp_english.show_english_candidates"
            @update:checked="
              formData.input.shift_temp_english.show_english_candidates = $event
            "
          />
        </div>
      </div>
      <div class="setting-item">
        <div class="setting-info">
          <label>触发键</label>
          <p class="setting-hint">按触发键进入临时英文模式（输入全小写字母）</p>
        </div>
        <div class="setting-control">
          <TriggerKeySelect
            :options="triggerKeyOptions"
            :model-value="formData.input.shift_temp_english.trigger_keys"
            @update:model-value="
              formData.input.shift_temp_english.trigger_keys = $event
            "
            :conflicts="tempEnglishConflictMap"
            placeholder="选择触发键"
          />
        </div>
        <p v-if="tempEnglishConflictMsg" class="setting-item-warning">
          ⚠ {{ tempEnglishConflictMsg }}
        </p>
      </div>
    </div>

    <!-- 默认状态 -->
    <div class="settings-card">
      <div class="card-title">默认状态</div>
      <div class="setting-item">
        <div class="setting-info">
          <label>记忆前次状态</label>
          <p class="setting-hint">启用后恢复上次的中英文、全半角和标点状态</p>
        </div>
        <div class="setting-control">
          <Switch
            :checked="formData.startup.remember_last_state"
            @update:checked="formData.startup.remember_last_state = $event"
          />
        </div>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': formData.startup.remember_last_state }"
      >
        <div class="setting-info">
          <label>初始语言模式</label>
          <p class="setting-hint">每次激活输入法时的默认语言</p>
        </div>
        <div class="setting-control">
          <div class="segmented-control">
            <button
              :class="{ active: formData.startup.default_chinese_mode }"
              @click="formData.startup.default_chinese_mode = true"
              :disabled="formData.startup.remember_last_state"
            >
              中文
            </button>
            <button
              :class="{ active: !formData.startup.default_chinese_mode }"
              @click="formData.startup.default_chinese_mode = false"
              :disabled="formData.startup.remember_last_state"
            >
              英文
            </button>
          </div>
        </div>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': formData.startup.remember_last_state }"
      >
        <div class="setting-info">
          <label>初始字符宽度</label>
          <p class="setting-hint">每次激活输入法时的默认字符宽度</p>
        </div>
        <div class="setting-control">
          <div class="segmented-control">
            <button
              :class="{ active: !formData.startup.default_full_width }"
              @click="formData.startup.default_full_width = false"
              :disabled="formData.startup.remember_last_state"
            >
              半角
            </button>
            <button
              :class="{ active: formData.startup.default_full_width }"
              @click="formData.startup.default_full_width = true"
              :disabled="formData.startup.remember_last_state"
            >
              全角
            </button>
          </div>
        </div>
      </div>
      <div
        class="setting-item"
        :class="{ 'item-disabled': formData.startup.remember_last_state }"
      >
        <div class="setting-info">
          <label>初始标点模式</label>
          <p class="setting-hint">每次激活输入法时的默认标点类型</p>
        </div>
        <div class="setting-control">
          <div class="segmented-control">
            <button
              :class="{ active: formData.startup.default_chinese_punct }"
              @click="formData.startup.default_chinese_punct = true"
              :disabled="formData.startup.remember_last_state"
            >
              中文标点
            </button>
            <button
              :class="{ active: !formData.startup.default_chinese_punct }"
              @click="formData.startup.default_chinese_punct = false"
              :disabled="formData.startup.remember_last_state"
            >
              英文标点
            </button>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref, computed, nextTick } from "vue";
import type { Config } from "../api/settings";
import { Switch } from "@/components/ui/switch";
import TriggerKeySelect from "@/components/TriggerKeySelect.vue";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";

const props = defineProps<{
  formData: Config;
}>();

// 标点配对配置
const showPairDialog = ref(false);
const pairDialogType = ref<"chinese" | "english">("chinese");

const chinesePairOptions = [
  { pair: "（）", left: "（", right: "）", desc: "圆括号" },
  { pair: "【】", left: "【", right: "】", desc: "方括号" },
  { pair: "｛｝", left: "｛", right: "｝", desc: "花括号" },
  { pair: "《》", left: "《", right: "》", desc: "书名号" },
  { pair: "〈〉", left: "〈", right: "〉", desc: "尖括号" },
  { pair: "\u2018\u2019", left: "\u2018", right: "\u2019", desc: "单引号" },
  { pair: "\u201C\u201D", left: "\u201C", right: "\u201D", desc: "双引号" },
];

const englishPairOptions = [
  { pair: "()", left: "(", right: ")", desc: "圆括号" },
  { pair: "[]", left: "[", right: "]", desc: "方括号" },
  { pair: "{}", left: "{", right: "}", desc: "花括号" },
  { pair: "''", left: "'", right: "'", desc: "单引号" },
  { pair: '""', left: '"', right: '"', desc: "双引号" },
];

const currentPairOptions = computed(() =>
  pairDialogType.value === "chinese" ? chinesePairOptions : englishPairOptions,
);

function getEnabledPairCount(type: "chinese" | "english") {
  const pairs =
    type === "chinese"
      ? props.formData.input.auto_pair.chinese_pairs
      : props.formData.input.auto_pair.english_pairs;
  return pairs ? pairs.length : 0;
}

function openPairDialog(type: "chinese" | "english") {
  pairDialogType.value = type;
  showPairDialog.value = true;
}

function isPairEnabled(pair: string) {
  const pairs =
    pairDialogType.value === "chinese"
      ? props.formData.input.auto_pair.chinese_pairs
      : props.formData.input.auto_pair.english_pairs;
  return pairs ? pairs.includes(pair) : false;
}

function togglePair(pair: string) {
  const key =
    pairDialogType.value === "chinese" ? "chinese_pairs" : "english_pairs";
  if (!props.formData.input.auto_pair[key]) {
    props.formData.input.auto_pair[key] = [];
  }
  const pairs = props.formData.input.auto_pair[key];
  const idx = pairs.indexOf(pair);
  if (idx >= 0) {
    pairs.splice(idx, 1);
  } else {
    pairs.push(pair);
  }
}

function setAllPairs(enabled: boolean) {
  const key =
    pairDialogType.value === "chinese" ? "chinese_pairs" : "english_pairs";
  const options =
    pairDialogType.value === "chinese"
      ? chinesePairOptions
      : englishPairOptions;
  if (enabled) {
    props.formData.input.auto_pair[key] = options.map((o) => o.pair);
  } else {
    props.formData.input.auto_pair[key] = [];
  }
}

// ========== 自定义标点映射 ==========

interface PunctRow {
  src: string;
  key: string;
  defaults: [string, string, string];
  values: [string, string, string];
}

// 默认标点映射表（完整 34 行）
const defaultPunctTable: {
  src: string;
  key: string;
  defaults: [string, string, string];
}[] = [
  { src: "!", key: "!", defaults: ["！", "！", "！"] },
  { src: "@", key: "@", defaults: ["@", "＠", "＠"] },
  { src: "#", key: "#", defaults: ["#", "＃", "＃"] },
  { src: "$", key: "$", defaults: ["￥", "＄", "￥"] },
  { src: "%", key: "%", defaults: ["%", "％", "％"] },
  { src: "^", key: "^", defaults: ["……", "＾", "……"] },
  { src: "&", key: "&", defaults: ["&", "＆", "＆"] },
  { src: "*", key: "*", defaults: ["*", "＊", "＊"] },
  { src: "(", key: "(", defaults: ["（", "（", "（"] },
  { src: ")", key: ")", defaults: ["）", "）", "）"] },
  { src: "_", key: "_", defaults: ["——", "＿", "——"] },
  { src: "-", key: "-", defaults: ["-", "－", "－"] },
  { src: "+", key: "+", defaults: ["+", "＋", "＋"] },
  { src: "=", key: "=", defaults: ["=", "＝", "＝"] },
  { src: "[", key: "[", defaults: ["【", "［", "【"] },
  { src: "]", key: "]", defaults: ["】", "］", "】"] },
  { src: "{", key: "{", defaults: ["｛", "｛", "｛"] },
  { src: "}", key: "}", defaults: ["｝", "｝", "｝"] },
  { src: "\\", key: "\\", defaults: ["、", "＼", "、"] },
  { src: "|", key: "|", defaults: ["|", "｜", "｜"] },
  { src: ";", key: ";", defaults: ["；", "；", "；"] },
  { src: ":", key: ":", defaults: ["：", "：", "："] },
  { src: '" 第一次', key: '"1', defaults: ["\u201C", "\uFF02", "\u201C"] },
  { src: '" 第二次', key: '"2', defaults: ["\u201D", "\uFF02", "\u201D"] },
  { src: "' 第一次", key: "'1", defaults: ["\u2018", "\uFF07", "\u2018"] },
  { src: "' 第二次", key: "'2", defaults: ["\u2019", "\uFF07", "\u2019"] },
  { src: ",", key: ",", defaults: ["，", "，", "，"] },
  { src: ".", key: ".", defaults: ["。", "．", "。"] },
  { src: "<", key: "<", defaults: ["《", "＜", "《"] },
  { src: ">", key: ">", defaults: ["》", "＞", "》"] },
  { src: "/", key: "/", defaults: ["/", "／", "／"] },
  { src: "?", key: "?", defaults: ["？", "？", "？"] },
  { src: "~", key: "~", defaults: ["～", "～", "～"] },
  { src: "`", key: "`", defaults: ["·", "｀", "·"] },
];

const showPunctCustomDialog = ref(false);
const punctEditRows = ref<PunctRow[]>([]);
const editingCell = ref<{ row: number; col: number; value: string } | null>(
  null,
);
const cellInputRef = ref<HTMLInputElement[] | null>(null);
// 快照：打开对话框时保存，用于取消恢复
let punctCustomSnapshot: Record<string, string[]> | null = null;

function ensurePunctCustom() {
  if (!props.formData.input.punct_custom) {
    props.formData.input.punct_custom = { enabled: false, mappings: {} };
  }
  if (!props.formData.input.punct_custom.mappings) {
    props.formData.input.punct_custom.mappings = {};
  }
}

function buildEditRows(): PunctRow[] {
  ensurePunctCustom();
  const mappings = props.formData.input.punct_custom.mappings || {};
  return defaultPunctTable.map((def) => {
    const custom = mappings[def.key];
    const values: [string, string, string] = [
      custom?.[0] || def.defaults[0],
      custom?.[1] || def.defaults[1],
      custom?.[2] || def.defaults[2],
    ];
    return {
      src: def.src,
      key: def.key,
      defaults: [...def.defaults] as [string, string, string],
      values,
    };
  });
}

function openPunctCustomDialog() {
  ensurePunctCustom();
  // 快照当前配置
  punctCustomSnapshot = JSON.parse(
    JSON.stringify(props.formData.input.punct_custom.mappings || {}),
  );
  punctEditRows.value = buildEditRows();
  editingCell.value = null;
  showPunctCustomDialog.value = true;
}

function startEditCell(row: number, col: number) {
  editingCell.value = {
    row,
    col,
    value: punctEditRows.value[row].values[col],
  };
  nextTick(() => {
    const inputs = cellInputRef.value;
    if (inputs && inputs.length > 0) {
      inputs[0].focus();
      inputs[0].select();
    }
  });
}

function commitEditCell() {
  if (!editingCell.value) return;
  const { row, col, value } = editingCell.value;
  const trimmed = value.trim();
  if (trimmed.length > 0 && trimmed.length <= 8) {
    punctEditRows.value[row].values[col] = trimmed;
  }
  // 空值恢复默认
  if (trimmed.length === 0) {
    punctEditRows.value[row].values[col] =
      punctEditRows.value[row].defaults[col];
  }
  editingCell.value = null;
}

function cancelEditCell() {
  editingCell.value = null;
}

function confirmPunctCustom() {
  // 从编辑行提取覆盖项（与默认不同的值才存储）
  const mappings: Record<string, string[]> = {};
  for (const row of punctEditRows.value) {
    const overrides: string[] = ["", "", ""];
    let hasOverride = false;
    for (let i = 0; i < 3; i++) {
      if (row.values[i] !== row.defaults[i]) {
        overrides[i] = row.values[i];
        hasOverride = true;
      }
    }
    if (hasOverride) {
      mappings[row.key] = overrides;
    }
  }
  props.formData.input.punct_custom.mappings = mappings;
  punctCustomSnapshot = null;
  showPunctCustomDialog.value = false;
}

function cancelPunctCustom() {
  // 恢复快照
  if (punctCustomSnapshot !== null) {
    props.formData.input.punct_custom.mappings = punctCustomSnapshot;
    punctCustomSnapshot = null;
  }
  showPunctCustomDialog.value = false;
}

function resetPunctCustomDefaults() {
  punctEditRows.value = defaultPunctTable.map((def) => ({
    src: def.src,
    key: def.key,
    defaults: [...def.defaults] as [string, string, string],
    values: [...def.defaults] as [string, string, string],
  }));
  editingCell.value = null;
}

// 触发键选项列表
const triggerKeyOptions = [
  { value: "backtick", label: "反引号 ( ` )" },
  { value: "semicolon", label: "分号 ( ; )" },
  { value: "quote", label: "单引号 ( ' )" },
  { value: "comma", label: "逗号 ( , )" },
  { value: "period", label: "句号 ( . )" },
  { value: "slash", label: "斜杠 ( / )" },
  { value: "backslash", label: "反斜杠 ( \\ )" },
  { value: "open_bracket", label: "左方括号 ( [ )" },
  { value: "close_bracket", label: "右方括号 ( ] )" },
];

// 临时拼音额外支持 z 键
const tempPinyinKeyOptions = [
  ...triggerKeyOptions,
  { value: "z", label: "z 键" },
];

// --- 冲突检测（Map 形式供 TriggerKeySelect 内联显示 + 汇总文案） ---

// 快捷输入冲突 Map
const quickInputConflicts = computed(() => {
  const map = new Map<string, string>();
  const key = props.formData.input.quick_input.trigger_key;
  const pinyinKeys = props.formData.input.temp_pinyin?.trigger_keys || [];
  const englishKeys =
    props.formData.input.shift_temp_english?.trigger_keys || [];
  if (pinyinKeys.includes(key)) map.set(key, "与临时拼音冲突");
  if (englishKeys.includes(key)) map.set(key, "与临时英文冲突");
  return map;
});
const quickInputConflictMsg = computed(() => {
  const msgs = [...quickInputConflicts.value.values()];
  return msgs.length > 0 ? msgs.join("，") : "";
});

// 临时拼音冲突 Map
const tempPinyinConflictMap = computed(() => {
  const map = new Map<string, string>();
  const pinyinKeys = props.formData.input.temp_pinyin?.trigger_keys || [];
  const englishKeys =
    props.formData.input.shift_temp_english?.trigger_keys || [];
  const quickKey = props.formData.input.quick_input?.trigger_key;
  for (const pk of pinyinKeys) {
    const conflicts: string[] = [];
    if (englishKeys.includes(pk)) conflicts.push("临时英文");
    if (pk === quickKey) conflicts.push("快捷输入");
    if (conflicts.length > 0) map.set(pk, `与${conflicts.join("、")}冲突`);
  }
  return map;
});
const tempPinyinConflictMsg = computed(() => {
  const msgs = [...tempPinyinConflictMap.value.entries()].map(([, msg]) => msg);
  return msgs.length > 0 ? msgs.join("；") : "";
});

// 临时英文冲突 Map
const tempEnglishConflictMap = computed(() => {
  const map = new Map<string, string>();
  const englishKeys =
    props.formData.input.shift_temp_english?.trigger_keys || [];
  const pinyinKeys = props.formData.input.temp_pinyin?.trigger_keys || [];
  const quickKey = props.formData.input.quick_input?.trigger_key;
  for (const ek of englishKeys) {
    const conflicts: string[] = [];
    if (pinyinKeys.includes(ek)) conflicts.push("临时拼音");
    if (ek === quickKey) conflicts.push("快捷输入");
    if (conflicts.length > 0) map.set(ek, `与${conflicts.join("、")}冲突`);
  }
  return map;
});
const tempEnglishConflictMsg = computed(() => {
  const msgs = [...tempEnglishConflictMap.value.entries()].map(
    ([, msg]) => msg,
  );
  return msgs.length > 0 ? msgs.join("；") : "";
});

const filterModeOptions = [
  {
    value: "smart",
    label: "智能模式",
    desc: "优先常用字，无结果时自动扩展到全部字符",
    tag: "推荐",
  },
  {
    value: "general",
    label: "仅常用字",
    desc: "只显示通用规范汉字表中的常用汉字",
  },
  {
    value: "gb18030",
    label: "全部字符",
    desc: "不限制字符范围，包含生僻字",
  },
];

const currentFilterOption = computed(
  () =>
    filterModeOptions.find(
      (o) => o.value === props.formData.input.filter_mode,
    ) || filterModeOptions[0],
);

function selectFilterMode(value: string) {
  props.formData.input.filter_mode = value;
}
</script>

<style scoped>
/* ========== 自定义标点对话框 ========== */
.dialog-wide {
  min-width: 520px;
  max-width: 600px;
}
.dialog-hint {
  font-size: 12px;
  color: var(--text-secondary, #9ca3af);
  margin: 0 0 10px;
}
.punct-table-wrap {
  max-height: 320px;
  overflow-y: auto;
  border: 1px solid var(--border-color, #e5e7eb);
  border-radius: 6px;
}
.punct-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
  table-layout: fixed;
}
.punct-table thead {
  position: sticky;
  top: 0;
  z-index: 1;
}
.punct-table th {
  background: var(--bg-secondary, #f9fafb);
  color: var(--text-primary, #374151);
  font-weight: 600;
  padding: 8px 10px;
  text-align: center;
  border-bottom: 1px solid var(--border-color, #e5e7eb);
  font-size: 12px;
}
.punct-table td {
  padding: 5px 10px;
  text-align: center;
  border-bottom: 1px solid var(--border-color, #f3f4f6);
  white-space: nowrap;
}
.punct-table tbody tr:hover {
  background: var(--bg-hover, #f9fafb);
}
.col-src {
  width: 90px;
  font-weight: 500;
  color: var(--text-primary, #1f2937);
  user-select: none;
}
.col-edit {
  cursor: default;
  position: relative;
}
.col-edit.modified {
  color: var(--accent-color, #2563eb);
  font-weight: 500;
}
.col-edit.editing {
  padding: 2px 4px;
}
.cell-text {
  display: inline-block;
  min-width: 20px;
  min-height: 18px;
}
.cell-input {
  width: 100%;
  padding: 3px 6px;
  border: 1px solid var(--accent-color, #2563eb);
  border-radius: 4px;
  font-size: 13px;
  text-align: center;
  outline: none;
  box-sizing: border-box;
  background: var(--bg-card, #fff);
  color: var(--text-primary, #1f2937);
}
.dialog-footer-spacer {
  flex: 1;
}

/* ========== 触发键冲突提示（占满整行，在 setting-item flex 布局中换行） ========== */
.setting-item-warning {
  width: 100%;
  font-size: 12px;
  color: hsl(var(--warning));
  margin: -4px 0 0;
  padding: 0;
  line-height: 1.5;
}
/* 包含 warning 的 setting-item 允许换行 */
.setting-item:has(.setting-item-warning) {
  flex-wrap: wrap;
}

/* ========== 数字输入框 ========== */
.number-input {
  width: 70px;
  padding: 6px 10px;
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  font-size: 13px;
  color: hsl(var(--foreground));
  background: hsl(var(--card));
  text-align: center;
  transition:
    border-color 0.15s,
    box-shadow 0.15s;
}
.number-input:hover:not(:disabled) {
  border-color: hsl(var(--muted-foreground));
}
.number-input:focus {
  outline: none;
  border-color: hsl(var(--primary));
  box-shadow: 0 0 0 2px hsl(var(--ring) / 0.15);
}
.number-input:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
