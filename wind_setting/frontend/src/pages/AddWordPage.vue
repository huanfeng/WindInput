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
          />
        </div>

        <div class="addword-field">
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
import { ref, computed, onMounted, nextTick } from "vue";
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

const wordText = ref("");
const wordCode = ref("");
const schemaID = ref("");
const wordWeight = ref(1200);
const schemas = ref<SchemaItem[]>([]);
const { toast } = useToast();
const adding = ref(false);
const textInput = ref<HTMLInputElement | null>(null);

const canAdd = computed(() => {
  return (
    wordText.value.trim().length >= 1 &&
    wordCode.value.trim().length > 0 &&
    wordWeight.value > 0
  );
});

async function handleAdd() {
  if (!canAdd.value || adding.value) return;

  const text = wordText.value.trim();
  const code = wordCode.value.trim();
  const weight = wordWeight.value;

  adding.value = true;
  try {
    // 先检查是否已存在（通过搜索用户词库）
    if (schemaID.value) {
      const existing = await wailsApi.getUserDictBySchema(schemaID.value);
      const found = existing.find((w) => w.code === code && w.text === text);
      if (found) {
        toast(`该词已存在 (${text}: ${code})，已更新权重`);
        // 更新权重（AddUserWord 内部会覆盖已有条目）
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
    toast(`已添加: ${text} (${code})`);

    // 添加成功后清空输入，方便继续加词
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
    schemas.value = list.map((s) => ({ id: s.schema_id, name: s.schema_name }));
  } catch {
    schemas.value = [];
  }

  if (props.initialText) wordText.value = props.initialText;
  if (props.initialCode) wordCode.value = props.initialCode;
  if (props.initialSchema) {
    schemaID.value = props.initialSchema;
  } else if (schemas.value.length > 0) {
    schemaID.value = schemas.value[0].id;
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

.addword-weight {
  width: 120px;
}

.addword-select {
  cursor: pointer;
}

.addword-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 18px;
}
</style>
