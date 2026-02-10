<template>
  <section class="section section-wide">
    <div class="section-header">
      <h2>词库管理</h2>
      <p class="section-desc">管理您的词库数据</p>
    </div>

    <!-- 文件变化提示 -->
    <div v-if="showFileChangeAlert" class="settings-card warning-card">
      <div class="warning-content">
        <span class="warning-icon">!</span>
        <div class="warning-text">
          <p class="warning-title">检测到文件被外部修改</p>
          <p class="warning-desc">
            <span v-if="fileChangeStatus?.config_changed">配置文件 </span>
            <span v-if="fileChangeStatus?.phrases_changed">短语文件 </span>
            <span v-if="fileChangeStatus?.shadow_changed">Shadow文件 </span>
            <span v-if="fileChangeStatus?.userdict_changed">用户词库 </span>
            已被修改
          </p>
        </div>
        <button class="btn btn-sm btn-primary" @click="handleReloadAllFiles">重新加载</button>
        <button class="btn btn-sm" @click="showFileChangeAlert = false">忽略</button>
      </div>
    </div>

    <!-- 消息提示 -->
    <div v-if="dictMessage" :class="['dict-message', dictMessageType]">{{ dictMessage }}</div>

    <!-- 子标签页 -->
    <div class="dict-tabs">
      <button
        :class="['dict-tab', { active: dictSubTab === 'phrases' }]"
        @click="dictSubTab = 'phrases'"
      >
        用户短语 ({{ dictStats.phrase_count }})
      </button>
      <button
        :class="['dict-tab', { active: dictSubTab === 'userdict' }]"
        @click="dictSubTab = 'userdict'"
      >
        用户词库 ({{ dictStats.word_count }})
      </button>
      <button
        :class="['dict-tab', { active: dictSubTab === 'shadow' }]"
        @click="dictSubTab = 'shadow'"
      >
        候选调整 ({{ dictStats.shadow_count }})
      </button>
    </div>

    <!-- 非 Wails 环境提示 -->
    <div v-if="!isWailsEnv" class="settings-card">
      <div class="dict-note-center">
        <p>词库管理功能需要在桌面应用中使用</p>
        <p class="dict-note">请使用 <code>wails dev</code> 或编译后的应用</p>
      </div>
    </div>

    <!-- 用户短语 -->
    <div v-else-if="dictSubTab === 'phrases'" class="dict-content">
      <div class="dict-toolbar">
        <button class="btn btn-primary btn-sm" @click="showAddPhraseForm = true">+ 添加短语</button>
        <button class="btn btn-sm" @click="loadDictData" :disabled="dictLoading">
          {{ dictLoading ? "加载中..." : "刷新" }}
        </button>
      </div>
      <div v-if="showAddPhraseForm" class="dict-form-card">
        <div class="form-row">
          <label>编码</label>
          <input type="text" v-model="newPhrase.code" class="input" placeholder="如: rq" />
        </div>
        <div class="form-row">
          <label>文本</label>
          <input
            type="text"
            v-model="newPhrase.text"
            class="input"
            placeholder="如: {{date}}"
          />
        </div>
        <div class="form-row">
          <label>权重</label>
          <input type="number" v-model.number="newPhrase.weight" class="input input-sm" />
        </div>
        <div class="form-actions">
          <button class="btn btn-sm" @click="showAddPhraseForm = false">取消</button>
          <button class="btn btn-primary btn-sm" @click="handleAddPhrase">添加</button>
        </div>
      </div>
      <div class="dict-list" v-if="phrases.length > 0">
        <div class="dict-list-item" v-for="(item, idx) in phrases" :key="idx">
          <div class="dict-item-main">
            <span class="dict-item-code">{{ item.code }}</span>
            <span class="dict-item-text">{{ item.text }}</span>
            <span v-if="item.type" class="dict-item-tag">{{ item.type }}</span>
          </div>
          <div class="dict-item-actions">
            <button class="btn-icon btn-delete" @click="handleRemovePhrase(item)" title="删除">
              &times;
            </button>
          </div>
        </div>
      </div>
      <div v-else class="dict-empty">暂无用户短语</div>
    </div>

    <!-- 用户词库 -->
    <div v-else-if="dictSubTab === 'userdict'" class="dict-content">
      <div class="dict-engine-switcher">
        <span class="dict-engine-label">词库类型：</span>
        <button
          :class="['dict-engine-btn', { active: userDictEngine === 'wubi' }]"
          @click="handleSwitchUserDictEngine('wubi')"
          :disabled="dictLoading"
        >
          五笔
        </button>
        <button
          :class="['dict-engine-btn', { active: userDictEngine === 'pinyin' }]"
          @click="handleSwitchUserDictEngine('pinyin')"
          :disabled="dictLoading"
        >
          拼音
        </button>
      </div>
      <div class="dict-toolbar">
        <button class="btn btn-primary btn-sm" @click="showAddWordForm = true">+ 添加词条</button>
        <button class="btn btn-sm" @click="loadDictData" :disabled="dictLoading">
          {{ dictLoading ? "加载中..." : "刷新" }}
        </button>
      </div>
      <div v-if="showAddWordForm" class="dict-form-card">
        <div class="form-row">
          <label>编码</label>
          <input type="text" v-model="newWord.code" class="input" placeholder="如: nihao" />
        </div>
        <div class="form-row">
          <label>词条</label>
          <input type="text" v-model="newWord.text" class="input" placeholder="如: 你好" />
        </div>
        <div class="form-row">
          <label>权重</label>
          <input type="number" v-model.number="newWord.weight" class="input input-sm" />
        </div>
        <div class="form-actions">
          <button class="btn btn-sm" @click="showAddWordForm = false">取消</button>
          <button class="btn btn-primary btn-sm" @click="handleAddUserWord">添加</button>
        </div>
      </div>
      <div class="dict-list" v-if="userDict.length > 0">
        <div class="dict-list-item" v-for="(item, idx) in userDict" :key="idx">
          <div class="dict-item-main">
            <span class="dict-item-code">{{ item.code }}</span>
            <span class="dict-item-text">{{ item.text }}</span>
            <span class="dict-item-weight" v-if="item.weight">{{ item.weight }}</span>
          </div>
          <div class="dict-item-actions">
            <button class="btn-icon btn-delete" @click="handleRemoveUserWord(item)" title="删除">
              &times;
            </button>
          </div>
        </div>
      </div>
      <div v-else class="dict-empty">暂无用户词条</div>
    </div>

    <!-- 候选调整 (Shadow) -->
    <div v-else-if="dictSubTab === 'shadow'" class="dict-content">
      <div class="dict-toolbar">
        <button class="btn btn-primary btn-sm" @click="showAddShadowForm = true">
          + 添加规则
        </button>
        <button class="btn btn-sm" @click="loadDictData" :disabled="dictLoading">
          {{ dictLoading ? "加载中..." : "刷新" }}
        </button>
      </div>
      <div v-if="showAddShadowForm" class="dict-form-card">
        <div class="form-row">
          <label>编码</label>
          <input type="text" v-model="newShadow.code" class="input" placeholder="如: zg" />
        </div>
        <div class="form-row">
          <label>词条</label>
          <input type="text" v-model="newShadow.word" class="input" placeholder="如: 中国" />
        </div>
        <div class="form-row">
          <label>操作</label>
          <select v-model="newShadow.action" class="select">
            <option value="pin">置顶</option>
            <option value="delete">删除</option>
            <option value="adjust">调整权重</option>
          </select>
        </div>
        <div class="form-row" v-if="newShadow.action === 'adjust' || newShadow.action === 'pin'">
          <label>权重</label>
          <input type="number" v-model.number="newShadow.weight" class="input input-sm" />
        </div>
        <div class="form-actions">
          <button class="btn btn-sm" @click="showAddShadowForm = false">取消</button>
          <button class="btn btn-primary btn-sm" @click="handleAddShadowRule">添加</button>
        </div>
      </div>
      <div class="dict-list" v-if="shadowRules.length > 0">
        <div class="dict-list-item" v-for="(item, idx) in shadowRules" :key="idx">
          <div class="dict-item-main">
            <span class="dict-item-code">{{ item.code }}</span>
            <span class="dict-item-text">{{ item.word }}</span>
            <span class="dict-item-tag" :class="'tag-' + item.action">
              {{ getShadowActionLabel(item.action) }}
            </span>
            <span class="dict-item-weight" v-if="item.weight">{{ item.weight }}</span>
          </div>
          <div class="dict-item-actions">
            <button
              class="btn-icon btn-delete"
              @click="handleRemoveShadowRule(item)"
              title="删除"
            >
              &times;
            </button>
          </div>
        </div>
      </div>
      <div v-else class="dict-empty">暂无调整规则</div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref, onMounted } from "vue";
import * as wailsApi from "../api/wails";
import type {
  PhraseItem,
  UserWordItem,
  ShadowRuleItem,
  DictStats,
  FileChangeStatus,
} from "../api/wails";

const props = defineProps<{
  isWailsEnv: boolean;
}>();

const dictSubTab = ref<"phrases" | "userdict" | "shadow">("phrases");
const phrases = ref<PhraseItem[]>([]);
const userDict = ref<UserWordItem[]>([]);
const shadowRules = ref<ShadowRuleItem[]>([]);
const dictStats = ref<DictStats>({ word_count: 0, phrase_count: 0, shadow_count: 0 });
const dictLoading = ref(false);
const dictMessage = ref("");
const dictMessageType = ref<"success" | "error">("success");

const showAddPhraseForm = ref(false);
const newPhrase = ref({ code: "", text: "", weight: 0 });

const userDictEngine = ref<"wubi" | "pinyin">("wubi");
const showAddWordForm = ref(false);
const newWord = ref({ code: "", text: "", weight: 0 });

const showAddShadowForm = ref(false);
const newShadow = ref({ code: "", word: "", action: "pin", weight: 100 });

const fileChangeStatus = ref<FileChangeStatus | null>(null);
const showFileChangeAlert = ref(false);

function showDictMessage(msg: string, type: "success" | "error") {
  dictMessage.value = msg;
  dictMessageType.value = type;
  setTimeout(() => {
    dictMessage.value = "";
  }, 3000);
}

async function loadDictData() {
  if (!props.isWailsEnv) return;
  dictLoading.value = true;
  try {
    const [phrasesData, userDictData, shadowData, stats, engineType] = await Promise.all([
      wailsApi.getPhrases(),
      wailsApi.getUserDict(),
      wailsApi.getShadowRules(),
      wailsApi.getUserDictStats(),
      wailsApi.getUserDictEngineType(),
    ]);
    phrases.value = phrasesData || [];
    userDict.value = userDictData || [];
    shadowRules.value = shadowData || [];
    dictStats.value = stats || { word_count: 0, phrase_count: 0, shadow_count: 0 };
    if (engineType === "pinyin" || engineType === "wubi") {
      userDictEngine.value = engineType;
    }
  } catch (e) {
    console.error("加载词库数据失败", e);
    showDictMessage("加载词库数据失败", "error");
  } finally {
    dictLoading.value = false;
  }
}

async function handleSwitchUserDictEngine(engineType: "wubi" | "pinyin") {
  if (engineType === userDictEngine.value) return;
  dictLoading.value = true;
  try {
    await wailsApi.switchUserDictEngine(engineType);
    userDictEngine.value = engineType;
    const [userDictData, stats] = await Promise.all([
      wailsApi.getUserDict(),
      wailsApi.getUserDictStats(),
    ]);
    userDict.value = userDictData || [];
    dictStats.value = { ...dictStats.value, word_count: stats?.word_count || 0 };
  } catch (e) {
    console.error("切换词库失败", e);
    showDictMessage("切换词库失败", "error");
  } finally {
    dictLoading.value = false;
  }
}

async function handleAddPhrase() {
  if (!newPhrase.value.code || !newPhrase.value.text) {
    showDictMessage("请填写编码和文本", "error");
    return;
  }
  try {
    await wailsApi.addPhrase(newPhrase.value.code, newPhrase.value.text, newPhrase.value.weight);
    showDictMessage("添加成功", "success");
    showAddPhraseForm.value = false;
    newPhrase.value = { code: "", text: "", weight: 0 };
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "添加失败", "error");
  }
}

async function handleRemovePhrase(item: PhraseItem) {
  if (!confirm(`确定删除短语 "${item.text}" 吗？`)) return;
  try {
    await wailsApi.removePhrase(item.code, item.text);
    showDictMessage("删除成功", "success");
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "删除失败", "error");
  }
}

async function handleAddUserWord() {
  if (!newWord.value.code || !newWord.value.text) {
    showDictMessage("请填写编码和文本", "error");
    return;
  }
  try {
    await wailsApi.addUserWord(newWord.value.code, newWord.value.text, newWord.value.weight);
    showDictMessage("添加成功", "success");
    showAddWordForm.value = false;
    newWord.value = { code: "", text: "", weight: 0 };
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "添加失败", "error");
  }
}

async function handleRemoveUserWord(item: UserWordItem) {
  if (!confirm(`确定删除词条 "${item.text}" 吗？`)) return;
  try {
    await wailsApi.removeUserWord(item.code, item.text);
    showDictMessage("删除成功", "success");
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "删除失败", "error");
  }
}

async function handleAddShadowRule() {
  if (!newShadow.value.code || !newShadow.value.word) {
    showDictMessage("请填写编码和词条", "error");
    return;
  }
  try {
    await wailsApi.addShadowRule(
      newShadow.value.code,
      newShadow.value.word,
      newShadow.value.action,
      newShadow.value.weight,
    );
    showDictMessage("添加成功", "success");
    showAddShadowForm.value = false;
    newShadow.value = { code: "", word: "", action: "pin", weight: 100 };
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "添加失败", "error");
  }
}

async function handleRemoveShadowRule(item: ShadowRuleItem) {
  if (!confirm(`确定删除规则 "${item.word}" 吗？`)) return;
  try {
    await wailsApi.removeShadowRule(item.code, item.word);
    showDictMessage("删除成功", "success");
    await loadDictData();
  } catch (e: any) {
    showDictMessage(e.message || "删除失败", "error");
  }
}

async function checkFileChanges() {
  if (!props.isWailsEnv) return;
  try {
    const status = await wailsApi.checkAllFilesModified();
    fileChangeStatus.value = status;
    if (
      status.config_changed ||
      status.phrases_changed ||
      status.shadow_changed ||
      status.userdict_changed
    ) {
      showFileChangeAlert.value = true;
    }
  } catch (e) {
    console.error("检查文件变化失败", e);
  }
}

async function handleReloadAllFiles() {
  try {
    await wailsApi.reloadAllFiles();
    showFileChangeAlert.value = false;
    fileChangeStatus.value = null;
    await loadDictData();
    showDictMessage("已重新加载所有文件", "success");
  } catch (e: any) {
    showDictMessage(e.message || "重新加载失败", "error");
  }
}

function getShadowActionLabel(action: string): string {
  const labels: Record<string, string> = { pin: "置顶", delete: "删除", adjust: "调整权重" };
  return labels[action] || action;
}

onMounted(() => {
  loadDictData();
});
</script>

<style scoped>
.dict-message {
  padding: 10px 16px;
  border-radius: 8px;
  margin-bottom: 16px;
  font-size: 14px;
}
.dict-message.success {
  background: #dcfce7;
  color: #166534;
}
.dict-message.error {
  background: #fee2e2;
  color: #991b1b;
}
.dict-tabs {
  display: flex;
  gap: 4px;
  margin-bottom: 16px;
  border-bottom: 1px solid #e5e7eb;
  padding-bottom: 0;
}
.dict-tab {
  padding: 10px 20px;
  border: none;
  background: none;
  cursor: pointer;
  font-size: 14px;
  color: #6b7280;
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
  transition: all 0.15s;
}
.dict-tab:hover {
  color: #374151;
}
.dict-tab.active {
  color: #2563eb;
  border-bottom-color: #2563eb;
  font-weight: 500;
}
.dict-content {
  min-height: 300px;
}
.dict-engine-switcher {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 12px;
}
.dict-engine-label {
  font-size: 13px;
  color: #6b7280;
}
.dict-engine-btn {
  padding: 4px 14px;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  background: #fff;
  cursor: pointer;
  font-size: 13px;
  color: #374151;
  transition: all 0.15s;
}
.dict-engine-btn:hover:not(:disabled) {
  border-color: #93c5fd;
  color: #2563eb;
}
.dict-engine-btn.active {
  background: #2563eb;
  color: #fff;
  border-color: #2563eb;
}
.dict-engine-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.dict-toolbar {
  display: flex;
  gap: 12px;
  margin-bottom: 16px;
}
.dict-empty {
  text-align: center;
  padding: 48px;
  color: #9ca3af;
  background: #f9fafb;
  border-radius: 8px;
}
.dict-form-card {
  background: #f9fafb;
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  padding: 16px;
  margin-bottom: 16px;
}
.dict-list {
  background: #fff;
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  overflow: hidden;
}
.dict-list-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  border-bottom: 1px solid #f3f4f6;
  transition: background 0.15s;
}
.dict-list-item:last-child {
  border-bottom: none;
}
.dict-list-item:hover {
  background: #f9fafb;
}
.dict-item-main {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 1;
  min-width: 0;
}
.dict-item-code {
  font-family: "Consolas", "Monaco", monospace;
  font-size: 13px;
  color: #6b7280;
  background: #f3f4f6;
  padding: 2px 8px;
  border-radius: 4px;
  min-width: 60px;
}
.dict-item-text {
  font-size: 14px;
  color: #1f2937;
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dict-item-tag {
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 4px;
  background: #e0e7ff;
  color: #3730a3;
}
.dict-item-tag.tag-pin {
  background: #dcfce7;
  color: #166534;
}
.dict-item-tag.tag-delete {
  background: #fee2e2;
  color: #991b1b;
}
.dict-item-tag.tag-adjust {
  background: #fef3c7;
  color: #92400e;
}
.dict-item-weight {
  font-size: 12px;
  color: #9ca3af;
}
.dict-item-actions {
  display: flex;
  gap: 8px;
  flex-shrink: 0;
}
.dict-note {
  font-size: 12px;
  color: #9ca3af;
  font-style: italic;
}
.dict-note-center {
  text-align: center;
  padding: 32px;
  color: #6b7280;
}
.dict-note-center code {
  background: #f3f4f6;
  padding: 2px 6px;
  border-radius: 4px;
}
</style>
