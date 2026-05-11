<template>
  <div class="addword-overlay" @click.self="handleCancel">
    <div class="addword-dialog">
      <h3 class="addword-title">快捷加词</h3>

      <div class="addword-form">
        <div class="addword-field">
          <label class="addword-label">词语</label>
          <input
            ref="textInput"
            class="addword-input"
            v-model="wordText"
            placeholder="输入要添加的词语"
            @input="onTextInput"
          />
        </div>

        <!-- 有自动编码支持的方案（拼音/码表）：显示可编辑编码字段 + 刷新按钮 -->
        <div v-if="hasAutoEncode" class="addword-field">
          <label class="addword-label">
            {{ codeLabel }}
            <span class="addword-hint">{{ codeHint }}</span>
          </label>
          <div class="addword-code-row">
            <input
              class="addword-input addword-input-grow"
              v-model="wordCode"
              :placeholder="codePlaceholder"
              :class="{ 'addword-input-generating': generatingCode }"
            />
            <button
              class="addword-gen-btn"
              type="button"
              @click="autoGenerateCode"
              :disabled="!wordText.trim() || generatingCode"
              title="重新生成编码"
            >
              ↺
            </button>
          </div>
        </div>

        <!-- 其他方案：编码纯手动必填 -->
        <div v-else class="addword-field">
          <label class="addword-label">编码</label>
          <input
            class="addword-input"
            v-model="wordCode"
            placeholder="输入编码"
          />
        </div>

        <div class="addword-field">
          <label class="addword-label">方案</label>
          <Select
            :model-value="schemaID"
            @update:model-value="schemaID = $event"
          >
            <SelectTrigger class="w-full">
              <SelectValue placeholder="选择方案" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem v-for="s in schemas" :key="s.id" :value="s.id">
                {{ s.name }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div class="addword-field">
          <label class="addword-label">
            权重
            <span class="addword-hint"
              >值越大排序越靠前，系统词中位约 1000</span
            >
          </label>
          <input
            class="addword-input addword-weight"
            type="number"
            v-model.number="wordWeight"
            min="1"
            max="10000"
          />
        </div>
      </div>

      <div class="addword-actions">
        <Button variant="outline" @click="handleCancel">取消</Button>
        <Button @click="handleAdd" :disabled="!canAdd || adding">
          {{ adding ? "添加中..." : "添加" }}
        </Button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, nextTick, watch } from "vue";
import * as wailsApi from "../api/wails";
import { useToast } from "../composables/useToast";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from "@/components/ui/select";

interface SchemaItem {
  id: string;
  name: string;
  engineType: string;
  aliasIds: string[];
}

const props = defineProps<{
  initialText?: string;
  initialCode?: string;
  initialSchema?: string;
  standalone?: boolean;
}>();

const emit = defineEmits<{
  close: [];
}>();

const wordText = ref(props.initialText ?? "");
const wordCode = ref(props.initialCode ?? "");
const schemaID = ref(props.initialSchema ?? "");
const wordWeight = ref(1200);
const schemas = ref<SchemaItem[]>([]);
const { toast } = useToast();
const adding = ref(false);
const generatingCode = ref(false);
const textInput = ref<HTMLInputElement | null>(null);

const currentSchema = computed(() =>
  schemas.value.find((s) => s.id === schemaID.value),
);
const isPinyin = computed(() => currentSchema.value?.engineType === "pinyin");
const isCodetable = computed(
  () => currentSchema.value?.engineType === "codetable",
);

// 有自动编码支持的方案
const hasAutoEncode = computed(() => isPinyin.value || isCodetable.value);

const codeLabel = computed(() => (isPinyin.value ? "拼音" : "编码"));
const codeHint = computed(() =>
  isPinyin.value ? "自动生成，多音字可手动修改" : "自动生成，可手动修改",
);
const codePlaceholder = computed(() =>
  isPinyin.value ? "全拼（如 nihao）" : "编码（如 abcd）",
);

const canAdd = computed(() => {
  const hasText = wordText.value.trim().length >= 1;
  const hasWeight = wordWeight.value > 0;
  if (isPinyin.value) {
    // 拼音方案：编码可空（服务端自动生成）
    return hasText && hasWeight;
  }
  // 码表及其他方案：编码必填
  return hasText && wordCode.value.trim().length > 0 && hasWeight;
});

async function autoGenerateCode() {
  const text = wordText.value.trim();
  if (!text) return;
  generatingCode.value = true;
  try {
    let code = "";
    if (isPinyin.value) {
      code = await wailsApi.generatePinyinCode(text);
    } else if (isCodetable.value) {
      code = await wailsApi.encodeWordForSchema(schemaID.value, text);
    }
    wordCode.value = code;
  } catch {
    // 生成失败时保留用户输入，不强制清空
  } finally {
    generatingCode.value = false;
  }
}

let autoGenTimer: ReturnType<typeof setTimeout> | null = null;

function onTextInput() {
  if (!hasAutoEncode.value) return;
  if (autoGenTimer) clearTimeout(autoGenTimer);
  autoGenTimer = setTimeout(() => {
    autoGenerateCode();
  }, 300);
}

// 切换方案时重新生成编码
watch(schemaID, () => {
  if (hasAutoEncode.value && wordText.value.trim()) {
    autoGenerateCode();
  } else if (!hasAutoEncode.value) {
    if (wordCode.value && !props.initialCode) {
      wordCode.value = "";
    }
  }
});

async function handleAdd() {
  if (!canAdd.value || adding.value) return;

  const text = wordText.value.trim();
  const code = wordCode.value.trim();
  const weight = wordWeight.value;

  adding.value = true;
  try {
    if (schemaID.value) {
      const existing = await wailsApi.getUserDictBySchema(schemaID.value);
      const found = existing.find((w) => w.code === code && w.text === text);
      if (found) {
        toast(`该词已存在 (${text}: ${code})，已更新权重`);
        await wailsApi.addUserWordForSchema(schemaID.value, code, text, weight);
        await wailsApi.notifyReload("userdict");
        adding.value = false;
        return;
      }
      await wailsApi.addUserWordForSchema(schemaID.value, code, text, weight);
    } else {
      await wailsApi.addUserWord(code, text, weight);
    }
    await wailsApi.notifyReload("userdict");
    const displayCode = code || "(自动生成)";
    toast(`已添加: ${text} (${displayCode})`);

    wordText.value = "";
    wordCode.value = "";
    await nextTick();
    textInput.value?.focus();
  } catch (e: any) {
    toast(`添加失败: ${e.message || e}`, "error");
  } finally {
    adding.value = false;
  }
}

function handleCancel() {
  emit("close");
}

onMounted(async () => {
  try {
    const list = await wailsApi.getEnabledSchemasWithDictStats();
    schemas.value = list.map((s) => ({
      id: s.schema_id,
      name: s.schema_name,
      engineType: s.engine_type,
      aliasIds: s.alias_ids || [],
    }));
  } catch {
    schemas.value = [];
  }

  // 用 schemas 列表做别名匹配，修正 schemaID（双拼方案合并后 id 可能变为 "pinyin"）
  if (props.initialSchema) {
    const matched =
      schemas.value.find((s) => s.id === props.initialSchema) ||
      schemas.value.find((s) => s.aliasIds.includes(props.initialSchema!));
    schemaID.value = matched ? matched.id : schemas.value[0]?.id || "";
  } else if (schemas.value.length > 0 && !schemaID.value) {
    schemaID.value = schemas.value[0].id;
  }

  // 初始化时若有词语但无编码，自动生成
  if (hasAutoEncode.value && wordText.value.trim() && !wordCode.value) {
    await autoGenerateCode();
  }

  await nextTick();
  textInput.value?.focus();
  textInput.value?.select();
});
</script>

<style scoped>
.addword-overlay {
  position: fixed;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.3);
  z-index: 1000;
}

.addword-dialog {
  background: hsl(var(--card));
  border-radius: 8px;
  padding: 24px;
  width: 340px;
  box-shadow: 0 4px 24px rgba(0, 0, 0, 0.15);
}

.addword-title {
  font-size: 16px;
  font-weight: 600;
  margin-bottom: 18px;
  color: hsl(var(--foreground));
}

.addword-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.addword-field {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.addword-label {
  font-size: 13px;
  font-weight: 500;
  color: hsl(var(--muted-foreground));
  display: flex;
  align-items: baseline;
  gap: 6px;
}

.addword-hint {
  font-size: 11px;
  font-weight: 400;
  color: hsl(var(--muted-foreground));
}

.addword-input {
  padding: 7px 10px;
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  font-size: 14px;
  color: hsl(var(--foreground));
  background: hsl(var(--card));
  outline: none;
  transition: border-color 0.15s;
}

.addword-input:focus {
  border-color: hsl(var(--primary));
  box-shadow: 0 0 0 2px hsl(var(--ring) / 0.1);
}

.addword-input::placeholder {
  color: hsl(var(--muted-foreground));
}

.addword-input-generating {
  opacity: 0.6;
}

.addword-weight {
  width: 120px;
}

.addword-code-row {
  display: flex;
  gap: 6px;
  align-items: center;
}

.addword-input-grow {
  flex: 1;
}

.addword-gen-btn {
  padding: 7px 10px;
  border: 1px solid hsl(var(--border));
  border-radius: 6px;
  font-size: 14px;
  background: hsl(var(--muted));
  color: hsl(var(--foreground));
  cursor: pointer;
  transition: background 0.15s;
  line-height: 1;
}

.addword-gen-btn:hover:not(:disabled) {
  background: hsl(var(--accent));
}

.addword-gen-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.addword-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 18px;
}
</style>
