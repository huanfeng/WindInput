<template>
  <section class="section dict-page">
    <div class="section-header">
      <h2>词库管理</h2>
      <p class="section-desc">管理您的词库数据（修改即时生效）</p>
    </div>

    <!-- 文件变化提示 -->
    <div
      v-if="showFileChangeAlert"
      class="settings-card warning-card"
      style="margin-bottom: 12px"
    >
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
        <button class="btn btn-sm btn-primary" @click="handleReloadAllFiles">
          重新加载
        </button>
        <button class="btn btn-sm" @click="showFileChangeAlert = false">
          忽略
        </button>
      </div>
    </div>

    <!-- 消息提示已迁移至全局 Toast -->

    <!-- 子标签页 -->
    <div class="sub-tabs">
      <button
        :class="['sub-tab', { active: dictSubTab === 'phrases' }]"
        @click="switchSubTab('phrases')"
      >
        快捷短语 ({{ phraseCount }})
      </button>
      <button
        :class="['sub-tab', { active: dictSubTab === 'userdict' }]"
        @click="switchSubTab('userdict')"
      >
        用户词库 ({{ totalWordCount }})
      </button>
      <button
        :class="['sub-tab', { active: dictSubTab === 'temp' }]"
        @click="switchSubTab('temp')"
      >
        临时词库 ({{ totalTempCount }})
      </button>
      <button
        :class="['sub-tab', { active: dictSubTab === 'shadow' }]"
        @click="switchSubTab('shadow')"
      >
        候选调整 ({{ totalShadowCount }})
      </button>
    </div>

    <!-- 非 Wails 环境提示 -->
    <div v-if="!isWailsEnv" class="settings-card">
      <div class="dict-note-center">
        <p>词库管理功能需要在桌面应用中使用</p>
        <p class="dict-note">请使用 <code>wails dev</code> 或编译后的应用</p>
      </div>
    </div>

    <template v-else>
      <!-- ========== 快捷短语 ========== -->
      <div v-if="dictSubTab === 'phrases'" class="dict-split-layout">
        <!-- 左侧：分类面板 -->
        <div class="dict-schema-panel">
          <div class="schema-panel-title">短语分类</div>
          <div
            :class="['schema-item', { active: phraseCategory === 'user' }]"
            @click="switchPhraseCategory('user')"
          >
            <div class="schema-item-info">
              <span class="schema-item-label">用</span>
              <span class="schema-item-name">用户短语</span>
            </div>
            <span class="schema-item-count">{{ phrases.length }}</span>
          </div>
          <div
            :class="['schema-item', { active: phraseCategory === 'system' }]"
            @click="switchPhraseCategory('system')"
          >
            <div class="schema-item-info">
              <span class="schema-item-label">系</span>
              <span class="schema-item-name">系统短语</span>
            </div>
            <span class="schema-item-count">{{ systemPhrases.length }}</span>
          </div>
        </div>

        <!-- 右侧：内容面板 -->
        <div class="dict-main-panel" style="position: relative">
          <!-- loading 遮罩 -->
          <div v-if="dictLoading" class="content-loading-overlay">
            <div class="spinner"></div>
          </div>

          <!-- 工具栏 -->
          <div class="dict-toolbar">
            <label
              class="toolbar-checkbox-wrap"
              v-if="phraseCategory === 'user'"
            >
              <input
                type="checkbox"
                :checked="allPhraseSelected"
                @change="toggleAllPhrases"
              />
              <span>全选</span>
            </label>
            <button
              v-if="phraseCategory === 'user'"
              class="btn btn-primary btn-sm"
              @click="openAddPhraseDialog"
            >
              + 添加
            </button>
            <button
              v-if="phraseCategory === 'user'"
              class="btn btn-sm btn-danger-outline"
              :disabled="selectedPhraseKeys.size === 0"
              @click="handleBatchRemovePhrases"
            >
              删除{{
                selectedPhraseKeys.size > 0
                  ? ` (${selectedPhraseKeys.size})`
                  : ""
              }}
            </button>
            <button
              v-if="phraseCategory === 'system'"
              class="btn btn-sm"
              @click="handleResetAllSystemPhrases"
              title="删除所有覆盖，恢复为系统默认"
            >
              恢复默认
            </button>
            <div class="toolbar-spacer"></div>
            <div
              class="toolbar-more"
              @click.stop="showPhraseMenu = !showPhraseMenu"
            >
              <button class="btn btn-sm">...</button>
              <div v-if="showPhraseMenu" class="toolbar-dropdown">
                <div
                  class="dropdown-item"
                  @click.stop="handleImportPhrases(); showPhraseMenu = false"
                >
                  {{ phraseCategory === 'user' ? '导入用户短语' : '导入系统短语' }}
                </div>
                <div
                  class="dropdown-item"
                  @click.stop="handleExportPhrases(); showPhraseMenu = false"
                >
                  {{ phraseCategory === 'user' ? '导出用户短语' : '导出系统短语' }}
                </div>
              </div>
            </div>
          </div>

          <!-- 用户短语列表 -->
          <div v-if="phraseCategory === 'user'" class="dict-list-wrapper">
            <div
              v-if="phrases.length > 0"
              class="dict-list dict-list-scrollable"
            >
              <div
                class="dict-list-item"
                v-for="(item, idx) in phrases"
                :key="idx"
                :class="{ selected: selectedPhraseKeys.has(phraseKey(item)) }"
              >
                <input
                  type="checkbox"
                  class="item-checkbox"
                  :checked="selectedPhraseKeys.has(phraseKey(item))"
                  @change="togglePhraseSelect(item)"
                />
                <div class="dict-item-main">
                  <span class="dict-item-code">{{ item.code }}</span>
                  <span v-if="item.texts" class="dict-item-text">{{ item.name || item.code }}（{{ item.texts.length }}字符）</span>
                  <span v-else class="dict-item-text">{{ item.text }}</span>
                  <span v-if="item.texts" class="tag-mapping">数组</span>
                  <span v-else-if="item.text.includes('$')" class="tag-dynamic"
                    >动态</span
                  >
                  <span class="dict-item-weight">{{ item.position }}</span>
                </div>
                <div class="dict-item-actions">
                  <button
                    class="btn-icon"
                    @click="openEditPhraseDialog(item)"
                    title="编辑"
                  >
                    ✎
                  </button>
                  <button
                    class="btn-icon btn-delete"
                    @click="handleRemovePhrase(item)"
                    title="删除"
                  >
                    &times;
                  </button>
                </div>
              </div>
            </div>
            <div v-else class="dict-empty">暂无用户短语</div>
          </div>

          <!-- 系统短语列表 -->
          <div v-else class="dict-list-wrapper">
            <div
              v-if="systemPhrases.length > 0"
              class="dict-list dict-list-scrollable"
            >
              <div
                class="dict-list-item"
                v-for="(item, idx) in systemPhrases"
                :key="idx"
                :class="{ 'item-disabled': item.disabled }"
              >
                <label class="item-switch-mini" @click.stop>
                  <input
                    type="checkbox"
                    :checked="!item.disabled"
                    @change="handleToggleSystemPhrase(item)"
                  />
                </label>
                <div class="dict-item-main">
                  <span class="dict-item-code">{{ item.code }}</span>
                  <span
                    v-if="item.texts"
                    class="dict-item-text"
                    :class="{ 'text-disabled': item.disabled }"
                    >{{ item.name || item.code }}（{{ item.texts.length }}字符）</span
                  >
                  <span
                    v-else
                    class="dict-item-text"
                    :class="{ 'text-disabled': item.disabled }"
                    >{{ item.text }}</span
                  >
                  <span v-if="item.texts" class="tag-mapping">数组</span>
                  <span v-else-if="item.text && item.text.includes('$')" class="tag-dynamic"
                    >动态</span
                  >
                  <span class="dict-item-weight">{{ item.position }}</span>
                  <span class="tag-system">系统</span>
                </div>
                <div class="dict-item-actions">
                  <button
                    class="btn-icon"
                    @click="openEditSystemPhraseDialog(item)"
                    title="编辑位置"
                  >
                    ✎
                  </button>
                </div>
              </div>
            </div>
            <div v-else class="dict-empty">暂无系统短语</div>
          </div>
        </div>
      </div>

      <!-- ========== 用户词库 ========== -->
      <div v-else-if="dictSubTab === 'userdict'" class="dict-split-layout">
        <!-- 左侧：方案面板 -->
        <div class="dict-schema-panel">
          <div class="schema-panel-title">输入方案</div>
          <div
            v-for="schema in schemaList"
            :key="schema.schema_id"
            :class="[
              'schema-item',
              { active: selectedSchemaID === schema.schema_id },
            ]"
            @click="handleSelectSchema(schema.schema_id)"
          >
            <div class="schema-item-info">
              <span class="schema-item-label">{{
                schema.icon_label || schema.schema_name.charAt(0)
              }}</span>
              <span class="schema-item-name">{{ schema.schema_name }}</span>
            </div>
            <span class="schema-item-count">{{ schema.word_count }}</span>
          </div>
        </div>

        <!-- 右侧：词库内容 -->
        <div class="dict-main-panel" style="position: relative">
          <!-- loading 遮罩 -->
          <div v-if="dictLoading" class="content-loading-overlay">
            <div class="spinner"></div>
          </div>

          <!-- 工具栏 -->
          <div class="dict-toolbar">
            <label class="toolbar-checkbox-wrap">
              <input
                type="checkbox"
                :checked="allWordSelected"
                @change="toggleAllWords"
              />
              <span>全选</span>
            </label>
            <button class="btn btn-primary btn-sm" @click="openAddWordDialog">
              + 添加
            </button>
            <button
              class="btn btn-sm btn-danger-outline"
              :disabled="selectedWordKeys.size === 0"
              @click="handleBatchRemoveWords"
            >
              删除{{
                selectedWordKeys.size > 0 ? ` (${selectedWordKeys.size})` : ""
              }}
            </button>
            <div class="toolbar-spacer"></div>
            <input
              type="text"
              v-model="wordSearchQuery"
              class="input input-sm toolbar-search"
              placeholder="搜索..."
            />
            <div
              class="toolbar-more"
              @click.stop="showWordMenu = !showWordMenu"
            >
              <button class="btn btn-sm">...</button>
              <div v-if="showWordMenu" class="toolbar-dropdown">
                <div
                  class="dropdown-item"
                  @click.stop="
                    handleImportUserDict();
                    showWordMenu = false;
                  "
                >
                  导入用户词库
                </div>
                <div
                  class="dropdown-item"
                  @click.stop="
                    handleExportUserDict();
                    showWordMenu = false;
                  "
                >
                  导出用户词库
                </div>
              </div>
            </div>
          </div>

          <!-- 词库表格 -->
          <div v-if="filteredUserDict.length > 0" class="dict-table-wrap">
            <table class="dict-table">
              <colgroup>
                <col class="col-check" />
                <col class="col-code" />
                <col />
                <col class="col-weight" />
                <col class="col-action" />
              </colgroup>
              <thead>
                <tr>
                  <th></th>
                  <th @click="toggleSort('code')" class="sortable-th">
                    编码
                    <span class="sort-icon">{{
                      sortKey === "code" ? (sortAsc ? "▲" : "▼") : "⇅"
                    }}</span>
                  </th>
                  <th @click="toggleSort('text')" class="sortable-th">
                    词条
                    <span class="sort-icon">{{
                      sortKey === "text" ? (sortAsc ? "▲" : "▼") : "⇅"
                    }}</span>
                  </th>
                  <th @click="toggleSort('weight')" class="sortable-th">
                    权重
                    <span class="sort-icon">{{
                      sortKey === "weight" ? (sortAsc ? "▲" : "▼") : "⇅"
                    }}</span>
                  </th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="(item, idx) in filteredUserDict"
                  :key="idx"
                  :class="{ selected: selectedWordKeys.has(wordKey(item)) }"
                  @dblclick="openEditWordDialog(item)"
                >
                  <td>
                    <input
                      type="checkbox"
                      class="item-checkbox"
                      :checked="selectedWordKeys.has(wordKey(item))"
                      @change="toggleWordSelect(item)"
                    />
                  </td>
                  <td>
                    <span class="dict-item-code">{{ item.code }}</span>
                  </td>
                  <td>{{ item.text }}</td>
                  <td class="td-weight">{{ item.weight || 0 }}</td>
                  <td>
                    <button
                      class="btn-icon btn-delete"
                      @click="handleRemoveUserWord(item)"
                      title="删除"
                    >
                      &times;
                    </button>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <div v-else-if="wordSearchQuery" class="dict-empty">
            未找到匹配词条
          </div>
          <div v-else class="dict-empty">暂无用户词条</div>
        </div>
      </div>

      <!-- ========== 候选调整 ========== -->
      <div v-else-if="dictSubTab === 'shadow'" class="dict-split-layout">
        <!-- 左侧：方案面板 -->
        <div class="dict-schema-panel">
          <div class="schema-panel-title">输入方案</div>
          <div
            v-for="schema in schemaList"
            :key="schema.schema_id"
            :class="[
              'schema-item',
              { active: selectedSchemaID === schema.schema_id },
            ]"
            @click="handleSelectSchema(schema.schema_id)"
          >
            <div class="schema-item-info">
              <span class="schema-item-label">{{
                schema.icon_label || schema.schema_name.charAt(0)
              }}</span>
              <span class="schema-item-name">{{ schema.schema_name }}</span>
            </div>
            <span class="schema-item-count">{{ schema.shadow_count }}</span>
          </div>
        </div>

        <!-- 右侧：规则内容 -->
        <div class="dict-main-panel" style="position: relative">
          <!-- loading 遮罩 -->
          <div v-if="dictLoading" class="content-loading-overlay">
            <div class="spinner"></div>
          </div>

          <div class="dict-toolbar">
            <label class="toolbar-checkbox-wrap">
              <input
                type="checkbox"
                :checked="allShadowSelected"
                @change="toggleAllShadows"
              />
              <span>全选</span>
            </label>
            <button class="btn btn-primary btn-sm" @click="openShadowDialog()">
              + 添加
            </button>
            <button
              class="btn btn-sm btn-danger-outline"
              :disabled="selectedShadowKeys.size === 0"
              @click="handleBatchRemoveShadows"
            >
              删除{{
                selectedShadowKeys.size > 0
                  ? ` (${selectedShadowKeys.size})`
                  : ""
              }}
            </button>
          </div>

          <div
            v-if="shadowRules.length > 0"
            class="dict-list dict-list-scrollable"
          >
            <div
              class="dict-list-item"
              v-for="(item, idx) in shadowRules"
              :key="idx"
              :class="{ selected: selectedShadowKeys.has(shadowKey(item)) }"
            >
              <input
                type="checkbox"
                class="item-checkbox"
                :checked="selectedShadowKeys.has(shadowKey(item))"
                @change="toggleShadowSelect(item)"
              />
              <div class="dict-item-main">
                <span class="dict-item-code">{{ item.code }}</span>
                <span class="dict-item-text">{{ item.word }}</span>
                <span class="dict-item-tag" :class="'tag-' + item.type">{{
                  getShadowActionLabel(item.type)
                }}</span>
                <span class="dict-item-weight" v-if="item.type === 'pin'"
                  >位置: {{ item.position }}</span
                >
              </div>
              <div class="dict-item-actions">
                <button
                  class="btn-icon"
                  @click="openShadowDialog(item)"
                  title="编辑"
                >
                  ✎
                </button>
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
      </div>

      <!-- ========== 临时词库 ========== -->
      <div v-else-if="dictSubTab === 'temp'" class="dict-split-layout">
        <!-- 左侧：方案面板 -->
        <div class="dict-schema-panel">
          <div class="schema-panel-title">输入方案</div>
          <div
            v-for="schema in schemaList"
            :key="schema.schema_id"
            :class="[
              'schema-item',
              { active: selectedSchemaID === schema.schema_id },
            ]"
            @click="handleSelectSchema(schema.schema_id)"
          >
            <div class="schema-item-info">
              <span class="schema-item-label">{{
                schema.icon_label || schema.schema_name.charAt(0)
              }}</span>
              <span class="schema-item-name">{{ schema.schema_name }}</span>
            </div>
            <span class="schema-item-count">{{ schema.temp_word_count }}</span>
          </div>
        </div>

        <!-- 右侧：临时词库内容 -->
        <div class="dict-main-panel" style="position: relative">
          <!-- loading 遮罩 -->
          <div v-if="dictLoading" class="content-loading-overlay">
            <div class="spinner"></div>
          </div>

          <div class="dict-toolbar">
            <label class="toolbar-checkbox-wrap">
              <input
                type="checkbox"
                :checked="allTempSelected"
                @change="toggleAllTemps"
              />
              <span>全选</span>
            </label>
            <button
              class="btn btn-primary btn-sm"
              @click="handlePromoteAllTemp"
              :disabled="tempDict.length === 0"
            >
              全部转正
            </button>
            <button
              class="btn btn-sm btn-danger-outline"
              :disabled="selectedTempKeys.size === 0"
              @click="handleBatchRemoveTemps"
            >
              删除{{
                selectedTempKeys.size > 0 ? ` (${selectedTempKeys.size})` : ""
              }}
            </button>
            <div class="toolbar-spacer"></div>
            <div
              class="toolbar-more"
              @click.stop="showTempMenu = !showTempMenu"
            >
              <button class="btn btn-sm">...</button>
              <div v-if="showTempMenu" class="toolbar-dropdown">
                <div
                  class="dropdown-item"
                  @click.stop="
                    handleClearTempDict();
                    showTempMenu = false;
                  "
                >
                  清空临时词库
                </div>
              </div>
            </div>
          </div>

          <div
            v-if="tempDict.length > 0"
            class="dict-list dict-list-scrollable"
          >
            <div
              class="dict-list-item"
              v-for="(item, idx) in tempDict"
              :key="idx"
              :class="{ selected: selectedTempKeys.has(tempKey(item)) }"
            >
              <input
                type="checkbox"
                class="item-checkbox"
                :checked="selectedTempKeys.has(tempKey(item))"
                @change="toggleTempSelect(item)"
              />
              <div class="dict-item-main">
                <span class="dict-item-code">{{ item.code }}</span>
                <span class="dict-item-text">{{ item.text }}</span>
                <span class="dict-item-weight">w:{{ item.weight }}</span>
                <span class="dict-item-weight">×{{ item.count }}</span>
              </div>
              <div class="dict-item-actions">
                <button
                  class="btn btn-sm"
                  @click="handlePromoteTempWord(item)"
                  title="转为永久词条"
                >
                  转正
                </button>
                <button
                  class="btn-icon btn-delete"
                  @click="handleRemoveTempWord(item)"
                  title="删除"
                >
                  &times;
                </button>
              </div>
            </div>
          </div>
          <div v-else class="dict-empty">
            <p>暂无临时词条</p>
            <p class="dict-note">自动学习的词条将在此显示</p>
          </div>
        </div>
      </div>
    </template>

    <!-- ========== 添加/编辑短语对话框 ========== -->
    <div
      v-if="addPhraseDialogVisible"
      class="dialog-overlay"
      @click.self="addPhraseDialogVisible = false"
    >
      <div class="dialog-box">
        <div class="dialog-title">
          {{ editingPhrase ? "编辑短语" : "添加短语" }}
        </div>
        <div class="form-row">
          <label>编码</label>
          <input
            type="text"
            v-model="newPhrase.code"
            class="input"
            placeholder="如: dz"
          />
        </div>
        <div class="form-row">
          <label>类型</label>
          <label class="radio-inline">
            <input type="radio" :value="false" v-model="phraseIsArray" /> 普通
          </label>
          <label class="radio-inline">
            <input type="radio" :value="true" v-model="phraseIsArray" /> 数组
          </label>
        </div>
        <template v-if="phraseIsArray">
          <div class="form-row">
            <label>名称</label>
            <input
              type="text"
              v-model="newPhrase.name"
              class="input"
              placeholder="如: 标点符号"
            />
          </div>
          <div class="form-row">
            <label>字符列表</label>
            <textarea
              v-model="newPhrase.texts"
              class="input"
              rows="3"
              placeholder="每个字符展开为独立候选，如: ①②③④⑤"
            ></textarea>
          </div>
        </template>
        <template v-else>
          <div class="form-row">
            <label>文本</label>
            <input
              type="text"
              v-model="newPhrase.text"
              class="input"
              placeholder="如: 我的地址是xxx 或 $Y-$MM-$DD"
            />
          </div>
        </template>
        <div class="form-row">
          <label>位置</label>
          <input
            type="number"
            v-model.number="newPhrase.position"
            class="input input-sm"
            min="1"
          />
        </div>
        <div class="dialog-actions">
          <button class="btn btn-sm" @click="addPhraseDialogVisible = false">
            取消
          </button>
          <button class="btn btn-primary btn-sm" @click="handleSavePhrase">
            {{ editingPhrase ? "保存" : "添加" }}
          </button>
        </div>
      </div>
    </div>

    <!-- ========== 编辑系统短语对话框 ========== -->
    <div
      v-if="sysEditDialogVisible"
      class="dialog-overlay"
      @click.self="sysEditDialogVisible = false"
    >
      <div class="dialog-box">
        <div class="dialog-title">编辑系统短语</div>
        <div class="form-row">
          <label>编码</label>
          <input type="text" class="input" :value="sysEditForm.code" disabled />
        </div>
        <div v-if="sysEditForm.texts" class="form-row">
          <label>名称</label>
          <input type="text" class="input" :value="sysEditForm.name" disabled />
        </div>
        <div v-if="sysEditForm.texts" class="form-row">
          <label>字符列表</label>
          <textarea class="input" rows="3" :value="sysEditForm.texts" disabled></textarea>
        </div>
        <div v-else class="form-row">
          <label>文本</label>
          <input type="text" class="input" :value="sysEditForm.text" disabled />
        </div>
        <div class="form-row">
          <label>位置</label>
          <input
            type="number"
            v-model.number="sysEditForm.position"
            class="input input-sm"
            min="1"
          />
        </div>
        <div class="dialog-actions">
          <button
            class="btn btn-sm"
            @click="handleResetSystemPhrase"
            v-if="sysEditForm.hasOverride"
          >
            恢复默认
          </button>
          <div class="toolbar-spacer"></div>
          <button class="btn btn-sm" @click="sysEditDialogVisible = false">
            取消
          </button>
          <button
            class="btn btn-primary btn-sm"
            @click="handleSaveSystemPhrase"
          >
            保存
          </button>
        </div>
      </div>
    </div>

    <!-- ========== 添加/编辑词条对话框 ========== -->
    <AddWordPage
      v-if="addWordDialogVisible"
      :initialText="editWordText"
      :initialCode="editWordCode"
      :initialSchema="selectedSchemaID"
      @close="handleAddWordDialogClose"
    />

    <!-- ========== Shadow 规则对话框 ========== -->
    <div
      v-if="shadowDialogVisible"
      class="dialog-overlay"
      @click.self="shadowDialogVisible = false"
    >
      <div class="dialog-box">
        <div class="dialog-title">
          {{ shadowDialogEditing ? "编辑规则" : "添加规则" }}
        </div>
        <div class="form-row">
          <label>编码</label>
          <input
            type="text"
            v-model="shadowForm.code"
            class="input"
            placeholder="如: sf"
            :disabled="shadowDialogEditing"
          />
        </div>
        <div class="form-row">
          <label>词条</label>
          <input
            type="text"
            v-model="shadowForm.word"
            class="input"
            placeholder="如: 村"
            :disabled="shadowDialogEditing"
          />
        </div>
        <div class="form-row">
          <label>操作</label>
          <select v-model="shadowForm.action" class="select">
            <option value="pin">固定位置</option>
            <option value="delete">隐藏词条</option>
          </select>
        </div>
        <div class="form-row" v-if="shadowForm.action === 'pin'">
          <label>目标位置</label>
          <input
            type="number"
            v-model.number="shadowForm.position"
            class="input input-sm"
            min="0"
            placeholder="0=首位"
          />
        </div>
        <div class="dialog-actions">
          <button class="btn btn-sm" @click="shadowDialogVisible = false">
            取消
          </button>
          <button class="btn btn-primary btn-sm" @click="handleSaveShadowRule">
            {{ shadowDialogEditing ? "保存" : "添加" }}
          </button>
        </div>
      </div>
    </div>
    <!-- ========== 确认对话框 ========== -->
    <div v-if="confirmVisible" class="dialog-overlay" @click.self="handleCancel">
      <div class="dialog-box" style="max-width: 360px">
        <div class="dialog-title">确认</div>
        <div style="padding: 8px 0 16px; font-size: 14px; color: #374151; white-space: pre-line">
          {{ confirmMessage }}
        </div>
        <div class="dialog-actions">
          <button class="btn btn-sm" @click="handleCancel">取消</button>
          <button class="btn btn-primary btn-sm" @click="handleConfirm">确定</button>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import * as wailsApi from "../api/wails";
import { useToast } from "../composables/useToast";
import { useConfirm } from "../composables/useConfirm";
import AddWordPage from "./AddWordPage.vue";
import type {
  PhraseItem,
  UserWordItem,
  ShadowRuleItem,
  FileChangeStatus,
  SchemaDictStatsItem,
  TempWordItem,
} from "../api/wails";

const props = defineProps<{
  isWailsEnv: boolean;
}>();

// ===== 全局 Toast =====
const { toast } = useToast();
const { confirmVisible, confirmMessage, confirm, handleConfirm, handleCancel } = useConfirm();

// ===== 通用状态 =====
const dictSubTab = ref<"phrases" | "userdict" | "shadow" | "temp">("phrases");
const dictLoading = ref(false);

// ===== 方案列表 =====
const schemaList = ref<SchemaDictStatsItem[]>([]);
const selectedSchemaID = ref("");

const totalWordCount = computed(() =>
  schemaList.value.reduce((sum, s) => sum + s.word_count, 0),
);
const totalShadowCount = computed(() =>
  schemaList.value.reduce((sum, s) => sum + s.shadow_count, 0),
);
const totalTempCount = computed(() =>
  schemaList.value.reduce((sum, s) => sum + (s.temp_word_count || 0), 0),
);

// ===== 短语 =====
const phraseCategory = ref<"user" | "system">("user");
const phrases = ref<PhraseItem[]>([]);
const systemPhrases = ref<PhraseItem[]>([]);
const phraseCount = computed(
  () => phrases.value.length + systemPhrases.value.length,
);
const addPhraseDialogVisible = ref(false);
const newPhrase = ref({ code: "", text: "", texts: "", name: "", position: 1 });
const editingPhrase = ref<PhraseItem | null>(null); // 非null时为编辑模式
const phraseIsArray = ref(false); // 是否为数组类型短语

// 系统短语编辑对话框
const sysEditDialogVisible = ref(false);
const sysEditForm = ref({
  code: "",
  text: "",
  texts: "",
  name: "",
  position: 1,
  hasOverride: false,
});

// 短语多选
const selectedPhraseKeys = ref(new Set<string>());
const allPhraseSelected = computed(
  () =>
    phrases.value.length > 0 &&
    phrases.value.every((p) => selectedPhraseKeys.value.has(phraseKey(p))),
);
function phraseKey(item: PhraseItem) {
  return `${item.code}||${item.text}`;
}
function togglePhraseSelect(item: PhraseItem) {
  const k = phraseKey(item);
  if (selectedPhraseKeys.value.has(k)) selectedPhraseKeys.value.delete(k);
  else selectedPhraseKeys.value.add(k);
  selectedPhraseKeys.value = new Set(selectedPhraseKeys.value);
}
function toggleAllPhrases() {
  if (allPhraseSelected.value) {
    selectedPhraseKeys.value = new Set();
  } else {
    selectedPhraseKeys.value = new Set(phrases.value.map(phraseKey));
  }
}

// ===== 用户词库 =====
const userDict = ref<UserWordItem[]>([]);
const addWordDialogVisible = ref(false);
const wordSearchQuery = ref("");
const sortKey = ref<"code" | "text" | "weight">("code");
const sortAsc = ref(true);

const filteredUserDict = computed(() => {
  let list = userDict.value;
  if (wordSearchQuery.value.trim()) {
    const q = wordSearchQuery.value.trim().toLowerCase();
    list = list.filter(
      (w) =>
        w.code.toLowerCase().includes(q) || w.text.toLowerCase().includes(q),
    );
  }
  return [...list].sort((a, b) => {
    const ka = sortKey.value;
    const av = a[ka];
    const bv = b[ka];
    if (typeof av === "number" && typeof bv === "number") {
      return sortAsc.value ? av - bv : bv - av;
    }
    const as = String(av ?? "");
    const bs = String(bv ?? "");
    return sortAsc.value ? as.localeCompare(bs) : bs.localeCompare(as);
  });
});

function toggleSort(key: "code" | "text" | "weight") {
  if (sortKey.value === key) sortAsc.value = !sortAsc.value;
  else {
    sortKey.value = key;
    sortAsc.value = true;
  }
}

// 词条多选
const selectedWordKeys = ref(new Set<string>());
const uniqueFilteredWordKeys = computed(
  () => new Set(filteredUserDict.value.map(wordKey)),
);
const allWordSelected = computed(
  () =>
    uniqueFilteredWordKeys.value.size > 0 &&
    [...uniqueFilteredWordKeys.value].every((k) =>
      selectedWordKeys.value.has(k),
    ),
);
function wordKey(item: UserWordItem) {
  return `${item.code}||${item.text}`;
}
function toggleWordSelect(item: UserWordItem) {
  const k = wordKey(item);
  if (selectedWordKeys.value.has(k)) selectedWordKeys.value.delete(k);
  else selectedWordKeys.value.add(k);
  selectedWordKeys.value = new Set(selectedWordKeys.value);
}
function toggleAllWords() {
  if (allWordSelected.value) {
    selectedWordKeys.value = new Set();
  } else {
    selectedWordKeys.value = new Set(filteredUserDict.value.map(wordKey));
  }
}

// ===== 临时词库 =====
const tempDict = ref<TempWordItem[]>([]);

// 临时词库多选
const selectedTempKeys = ref(new Set<string>());
const allTempSelected = computed(
  () =>
    tempDict.value.length > 0 &&
    tempDict.value.every((t) => selectedTempKeys.value.has(tempKey(t))),
);
function tempKey(item: TempWordItem) {
  return `${item.code}||${item.text}`;
}
function toggleTempSelect(item: TempWordItem) {
  const k = tempKey(item);
  if (selectedTempKeys.value.has(k)) selectedTempKeys.value.delete(k);
  else selectedTempKeys.value.add(k);
  selectedTempKeys.value = new Set(selectedTempKeys.value);
}
function toggleAllTemps() {
  if (allTempSelected.value) {
    selectedTempKeys.value = new Set();
  } else {
    selectedTempKeys.value = new Set(tempDict.value.map(tempKey));
  }
}

// 下拉菜单
const showWordMenu = ref(false);
const showTempMenu = ref(false);
const showPhraseMenu = ref(false);

// ===== 候选调整 =====
const shadowRules = ref<ShadowRuleItem[]>([]);
const shadowDialogVisible = ref(false);
const shadowDialogEditing = ref(false);
const shadowForm = ref({ code: "", word: "", action: "pin", position: 0 });

// Shadow 多选
const selectedShadowKeys = ref(new Set<string>());
const allShadowSelected = computed(
  () =>
    shadowRules.value.length > 0 &&
    shadowRules.value.every((s) => selectedShadowKeys.value.has(shadowKey(s))),
);
function shadowKey(item: ShadowRuleItem) {
  return `${item.code}||${item.word}`;
}
function toggleShadowSelect(item: ShadowRuleItem) {
  const k = shadowKey(item);
  if (selectedShadowKeys.value.has(k)) selectedShadowKeys.value.delete(k);
  else selectedShadowKeys.value.add(k);
  selectedShadowKeys.value = new Set(selectedShadowKeys.value);
}
function toggleAllShadows() {
  if (allShadowSelected.value) {
    selectedShadowKeys.value = new Set();
  } else {
    selectedShadowKeys.value = new Set(shadowRules.value.map(shadowKey));
  }
}

// ===== 文件变化 =====
const fileChangeStatus = ref<FileChangeStatus | null>(null);
const showFileChangeAlert = ref(false);

// ===== 工具函数 =====
function showDictMessage(msg: string, type: "success" | "error") {
  toast(msg, type);
}

function getShadowActionLabel(type: string): string {
  const labels: Record<string, string> = { pin: "固定位置", delete: "删除" };
  return labels[type] || type;
}

// ===== 数据加载 =====
async function loadSchemaList() {
  try {
    const list = await wailsApi.getEnabledSchemasWithDictStats();
    schemaList.value = list || [];
    if (!selectedSchemaID.value && schemaList.value.length > 0) {
      selectedSchemaID.value = schemaList.value[0].schema_id;
    }
  } catch (e) {
    console.error("加载方案列表失败", e);
  }
}

async function loadPhraseData() {
  try {
    const [userData, sysData] = await Promise.all([
      wailsApi.getPhrases(),
      wailsApi.getSystemPhrases(),
    ]);
    phrases.value = userData || [];
    systemPhrases.value = sysData || [];
  } catch (e) {
    console.error("加载短语数据失败", e);
  }
}

async function loadSchemaData() {
  if (!selectedSchemaID.value) return;
  dictLoading.value = true;
  try {
    if (dictSubTab.value === "userdict") {
      const data = await wailsApi.getUserDictBySchema(selectedSchemaID.value);
      userDict.value = data || [];
      selectedWordKeys.value = new Set();
    } else if (dictSubTab.value === "shadow") {
      const data = await wailsApi.getShadowBySchema(selectedSchemaID.value);
      shadowRules.value = data || [];
      selectedShadowKeys.value = new Set();
    } else if (dictSubTab.value === "temp") {
      const data = await wailsApi.getTempDictBySchema(selectedSchemaID.value);
      tempDict.value = data || [];
    }
  } catch (e) {
    console.error("加载方案数据失败", e);
    showDictMessage("加载数据失败", "error");
  } finally {
    dictLoading.value = false;
  }
}

async function loadAllData() {
  if (!props.isWailsEnv) return;
  try {
    await loadSchemaList();
    await loadPhraseData();
    // 加载所有 tab 的数据，而不仅是当前 tab
    if (selectedSchemaID.value) {
      const [words, shadow, temp] = await Promise.all([
        wailsApi.getUserDictBySchema(selectedSchemaID.value).catch(() => []),
        wailsApi.getShadowBySchema(selectedSchemaID.value).catch(() => []),
        wailsApi.getTempDictBySchema(selectedSchemaID.value).catch(() => []),
      ]);
      userDict.value = words || [];
      shadowRules.value = shadow || [];
      tempDict.value = temp || [];
    }
  } catch (e) {
    console.error("初始化数据失败", e);
  }
}

// ===== 标签页切换 =====
async function switchSubTab(tab: "phrases" | "userdict" | "shadow" | "temp") {
  dictSubTab.value = tab;
  selectedWordKeys.value = new Set();
  selectedShadowKeys.value = new Set();
  selectedPhraseKeys.value = new Set();
  if (tab === "phrases") {
    await loadPhraseData();
  } else {
    await loadSchemaData();
  }
}

// ===== 短语分类切换 =====
async function switchPhraseCategory(cat: "user" | "system") {
  phraseCategory.value = cat;
  selectedPhraseKeys.value = new Set();
}

// ===== 方案选择 =====
async function handleSelectSchema(schemaID: string) {
  if (schemaID === selectedSchemaID.value) return;
  selectedSchemaID.value = schemaID;
  await loadSchemaData();
}

// ===== 短语操作 =====
function openAddPhraseDialog() {
  editingPhrase.value = null;
  newPhrase.value = { code: "", text: "", texts: "", name: "", position: 1 };
  phraseIsArray.value = false;
  addPhraseDialogVisible.value = true;
}

function openEditPhraseDialog(item: PhraseItem) {
  editingPhrase.value = item;
  phraseIsArray.value = !!item.texts;
  newPhrase.value = {
    code: item.code,
    text: item.text || "",
    texts: item.texts || "",
    name: item.name || "",
    position: item.position,
  };
  addPhraseDialogVisible.value = true;
}

async function handleSavePhrase() {
  const isArr = phraseIsArray.value;
  if (!newPhrase.value.code || (!isArr && !newPhrase.value.text) || (isArr && !newPhrase.value.texts)) {
    showDictMessage(isArr ? "请填写编码和字符列表" : "请填写编码和文本", "error");
    return;
  }
  try {
    const isArr = phraseIsArray.value;
    const item: PhraseItem = {
      code: newPhrase.value.code,
      text: isArr ? "" : newPhrase.value.text,
      texts: isArr ? newPhrase.value.texts : "",
      name: isArr ? newPhrase.value.name : "",
      position: newPhrase.value.position,
      is_system: false,
      disabled: false,
    };

    if (editingPhrase.value) {
      // 编辑：在列表中替换
      const idx = phrases.value.findIndex(
        (p) => p.code === editingPhrase.value!.code && p.text === editingPhrase.value!.text && p.texts === editingPhrase.value!.texts,
      );
      if (idx >= 0) {
        phrases.value[idx] = item;
      }
      await wailsApi.savePhrases(phrases.value);
      showDictMessage("修改成功", "success");
    } else {
      // 新增
      phrases.value.push(item);
      await wailsApi.savePhrases(phrases.value);
      showDictMessage("添加成功", "success");
    }
    addPhraseDialogVisible.value = false;
    editingPhrase.value = null;
    await loadPhraseData();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "操作失败", "error");
  }
}

// ===== 系统短语操作 =====
function openEditSystemPhraseDialog(item: PhraseItem) {
  sysEditForm.value = {
    code: item.code,
    text: item.text || "",
    texts: item.texts || "",
    name: item.name || "",
    position: item.position,
    hasOverride: false, // TODO: 检测是否已有覆盖
  };
  sysEditDialogVisible.value = true;
}

async function handleToggleSystemPhrase(item: PhraseItem) {
  try {
    await wailsApi.overrideSystemPhrase(
      item.code,
      item.text,
      item.position,
      !item.disabled,
    );
    await loadPhraseData();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "操作失败", "error");
  }
}

async function handleSaveSystemPhrase() {
  try {
    await wailsApi.overrideSystemPhrase(
      sysEditForm.value.code,
      sysEditForm.value.text,
      sysEditForm.value.position,
      false,
    );
    showDictMessage("已保存系统短语覆盖", "success");
    sysEditDialogVisible.value = false;
    await loadPhraseData();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "保存失败", "error");
  }
}

async function handleResetSystemPhrase() {
  try {
    await wailsApi.removeSystemPhraseOverride(
      sysEditForm.value.code,
      sysEditForm.value.text,
    );
    showDictMessage("已恢复为系统默认", "success");
    sysEditDialogVisible.value = false;
    await loadPhraseData();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "操作失败", "error");
  }
}

// ===== 恢复系统默认 =====
async function handleResetAllSystemPhrases() {
  if (!(await confirm("确定恢复所有系统短语为默认设置？\n这将删除所有对系统短语的禁用和位置调整。"))) return;
  try {
    await wailsApi.resetAllSystemPhraseOverrides();
    showDictMessage("已恢复为系统默认", "success");
    await loadPhraseData();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "操作失败", "error");
  }
}

// ===== 短语导入导出 =====
async function handleExportPhrases() {
  try {
    if (phraseCategory.value === "user") {
      const path = await wailsApi.exportUserPhrases();
      if (path) showDictMessage("用户短语已导出", "success");
    } else {
      const path = await wailsApi.exportSystemPhrases();
      if (path) showDictMessage("系统短语已导出", "success");
    }
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "导出失败", "error");
  }
}

async function handleImportPhrases() {
  try {
    if (phraseCategory.value === "user") {
      await wailsApi.importUserPhrases();
      showDictMessage("用户短语已导入", "success");
    } else {
      await wailsApi.importSystemPhrases();
      showDictMessage("系统短语已导入", "success");
    }
    await loadPhraseData();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "导入失败", "error");
  }
}

async function handleRemovePhrase(item: PhraseItem) {
  if (!confirm(`确定删除短语 "${item.text}" 吗？`)) return;
  try {
    await wailsApi.removePhrase(item.code, item.text);
    selectedPhraseKeys.value.delete(phraseKey(item));
    selectedPhraseKeys.value = new Set(selectedPhraseKeys.value);
    showDictMessage("删除成功", "success");
    await loadPhraseData();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "删除失败", "error");
  }
}

async function handleBatchRemovePhrases() {
  if (selectedPhraseKeys.value.size === 0) return;
  if (!confirm(`确定删除选中的 ${selectedPhraseKeys.value.size} 条短语吗？`))
    return;
  const toDelete = phrases.value.filter((p) =>
    selectedPhraseKeys.value.has(phraseKey(p)),
  );
  try {
    for (const item of toDelete) {
      await wailsApi.removePhrase(item.code, item.text);
    }
    selectedPhraseKeys.value = new Set();
    showDictMessage(`已删除 ${toDelete.length} 条短语`, "success");
    await loadPhraseData();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "批量删除失败", "error");
  }
}

// ===== 用户词库操作 =====
const editWordText = ref("");
const editWordCode = ref("");

function openAddWordDialog() {
  editWordText.value = "";
  editWordCode.value = "";
  addWordDialogVisible.value = true;
}

function openEditWordDialog(item: UserWordItem) {
  editWordText.value = item.text;
  editWordCode.value = item.code;
  addWordDialogVisible.value = true;
}

async function handleAddWordDialogClose() {
  addWordDialogVisible.value = false;
  editWordText.value = "";
  editWordCode.value = "";
  // 重新加载数据以反映新增/修改的词条
  await loadSchemaData();
  await loadSchemaList();
}

async function handleRemoveUserWord(item: UserWordItem) {
  if (!confirm(`确定删除词条 "${item.text}" 吗？`)) return;
  try {
    await wailsApi.removeUserWordForSchema(
      selectedSchemaID.value,
      item.code,
      item.text,
    );
    selectedWordKeys.value.delete(wordKey(item));
    selectedWordKeys.value = new Set(selectedWordKeys.value);
    showDictMessage("删除成功", "success");
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "删除失败", "error");
  }
}

async function handleBatchRemoveWords() {
  if (selectedWordKeys.value.size === 0) return;
  if (!confirm(`确定删除选中的 ${selectedWordKeys.value.size} 条词条吗？`))
    return;

  // 按 key 去重，避免重复条目导致多次删除同一词
  const seen = new Set<string>();
  const toDelete: { code: string; text: string }[] = [];
  for (const item of userDict.value) {
    const k = wordKey(item);
    if (selectedWordKeys.value.has(k) && !seen.has(k)) {
      seen.add(k);
      toDelete.push({ code: item.code, text: item.text });
    }
  }

  let deleted = 0;
  let failed = 0;
  for (const item of toDelete) {
    try {
      await wailsApi.removeUserWordForSchema(
        selectedSchemaID.value,
        item.code,
        item.text,
      );
      deleted++;
    } catch {
      failed++;
    }
  }
  selectedWordKeys.value = new Set();
  if (failed > 0) {
    showDictMessage(`已删除 ${deleted} 条，${failed} 条失败`, "error");
  } else {
    showDictMessage(`已删除 ${deleted} 条词条`, "success");
  }
  await loadSchemaData();
  await loadSchemaList();
}

async function handleImportUserDict() {
  dictLoading.value = true;
  try {
    const result = await wailsApi.importUserDictForSchema(
      selectedSchemaID.value,
    );
    if (result.cancelled) return;
    showDictMessage(
      `导入成功，新增 ${result.count} 条，共 ${result.total} 条`,
      "success",
    );
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "导入失败", "error");
  } finally {
    dictLoading.value = false;
  }
}

async function handleExportUserDict() {
  dictLoading.value = true;
  try {
    const result = await wailsApi.exportUserDictForSchema(
      selectedSchemaID.value,
    );
    if (result.cancelled) return;
    showDictMessage(`导出成功，共 ${result.count} 条`, "success");
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "导出失败", "error");
  } finally {
    dictLoading.value = false;
  }
}

// ===== 临时词库操作 =====
async function handlePromoteTempWord(item: TempWordItem) {
  try {
    await wailsApi.promoteTempWordForSchema(
      selectedSchemaID.value,
      item.code,
      item.text,
    );
    showDictMessage(`已将 "${item.text}" 转为永久词条`, "success");
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "转正失败", "error");
  }
}

async function handlePromoteAllTemp() {
  if (!(await confirm("确定将所有临时词条转为永久词条吗？"))) return;
  try {
    const count = await wailsApi.promoteAllTempWordsForSchema(
      selectedSchemaID.value,
    );
    showDictMessage(`已转正 ${count} 条词条`, "success");
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "转正失败", "error");
  }
}

async function handleClearTempDict() {
  if (!(await confirm("确定清空临时词库吗？此操作不可恢复。"))) return;
  try {
    const count = await wailsApi.clearTempDictForSchema(selectedSchemaID.value);
    showDictMessage(`已清空 ${count} 条临时词条`, "success");
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "清空失败", "error");
  }
}

async function handleBatchRemoveTemps() {
  if (selectedTempKeys.value.size === 0) return;
  if (!confirm(`确定删除选中的 ${selectedTempKeys.value.size} 条临时词条吗？`))
    return;
  try {
    for (const key of selectedTempKeys.value) {
      const [code, text] = key.split("||");
      await wailsApi.removeTempWordForSchema(
        selectedSchemaID.value,
        code,
        text,
      );
    }
    selectedTempKeys.value = new Set();
    showDictMessage("批量删除成功", "success");
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "批量删除失败", "error");
  }
}

async function handleRemoveTempWord(item: TempWordItem) {
  if (!confirm(`确定删除临时词条 "${item.text}" 吗？`)) return;
  try {
    await wailsApi.removeTempWordForSchema(
      selectedSchemaID.value,
      item.code,
      item.text,
    );
    showDictMessage("删除成功", "success");
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "删除失败", "error");
  }
}

// ===== 候选调整操作 =====
function openShadowDialog(item?: ShadowRuleItem) {
  if (item) {
    shadowDialogEditing.value = true;
    shadowForm.value = {
      code: item.code,
      word: item.word,
      action: item.type,
      position: item.position || 0,
    };
  } else {
    shadowDialogEditing.value = false;
    shadowForm.value = { code: "", word: "", action: "pin", position: 0 };
  }
  shadowDialogVisible.value = true;
}

async function handleSaveShadowRule() {
  if (!shadowForm.value.code || !shadowForm.value.word) {
    showDictMessage("请填写编码和词条", "error");
    return;
  }
  try {
    if (shadowDialogEditing.value) {
      await wailsApi.removeShadowRuleForSchema(
        selectedSchemaID.value,
        shadowForm.value.code,
        shadowForm.value.word,
      );
    }
    if (shadowForm.value.action === "pin") {
      await wailsApi.pinShadowWordForSchema(
        selectedSchemaID.value,
        shadowForm.value.code,
        shadowForm.value.word,
        shadowForm.value.position,
      );
    } else {
      await wailsApi.deleteShadowWordForSchema(
        selectedSchemaID.value,
        shadowForm.value.code,
        shadowForm.value.word,
      );
    }
    showDictMessage(
      shadowDialogEditing.value ? "保存成功" : "添加成功",
      "success",
    );
    shadowDialogVisible.value = false;
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "操作失败", "error");
  }
}

async function handleRemoveShadowRule(item: ShadowRuleItem) {
  if (!confirm(`确定删除规则 "${item.word}" 吗？`)) return;
  try {
    await wailsApi.removeShadowRuleForSchema(
      selectedSchemaID.value,
      item.code,
      item.word,
    );
    selectedShadowKeys.value.delete(shadowKey(item));
    selectedShadowKeys.value = new Set(selectedShadowKeys.value);
    showDictMessage("删除成功", "success");
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "删除失败", "error");
  }
}

async function handleBatchRemoveShadows() {
  if (selectedShadowKeys.value.size === 0) return;
  if (!confirm(`确定删除选中的 ${selectedShadowKeys.value.size} 条规则吗？`))
    return;
  const toDelete = shadowRules.value.filter((s) =>
    selectedShadowKeys.value.has(shadowKey(s)),
  );
  try {
    for (const item of toDelete) {
      await wailsApi.removeShadowRuleForSchema(
        selectedSchemaID.value,
        item.code,
        item.word,
      );
    }
    selectedShadowKeys.value = new Set();
    showDictMessage(`已删除 ${toDelete.length} 条规则`, "success");
    await loadSchemaData();
    await loadSchemaList();
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "批量删除失败", "error");
  }
}

// ===== 文件变化检测 =====
async function handleReloadAllFiles() {
  try {
    await wailsApi.reloadAllFiles();
    showFileChangeAlert.value = false;
    fileChangeStatus.value = null;
    await loadAllData();
    showDictMessage("已重新加载所有文件", "success");
  } catch (e: unknown) {
    showDictMessage((e as Error).message || "重新加载失败", "error");
  }
}

// ===== 初始化 =====
function closeDropdowns() {
  showWordMenu.value = false;
  showTempMenu.value = false;
  showPhraseMenu.value = false;
}

onMounted(() => {
  loadAllData();
  document.addEventListener("click", closeDropdowns);
});

onUnmounted(() => {
  document.removeEventListener("click", closeDropdowns);
});
</script>

<style scoped>
/* ===== 整体布局：不产生页面级滚动 ===== */
.dict-page {
  display: flex;
  flex-direction: column;
  height: 100%;
  overflow: hidden;
}

/* ===== 左右分栏布局 ===== */
.dict-split-layout {
  display: flex;
  gap: 16px;
  flex: 1;
  min-height: 0;
  overflow: hidden;
}

/* 左侧方案/分类面板 */
.dict-schema-panel {
  width: 160px;
  flex-shrink: 0;
  background: #f9fafb;
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  padding: 8px;
  overflow-y: auto;
}
.schema-panel-title {
  font-size: 12px;
  color: #9ca3af;
  padding: 4px 8px 8px;
  border-bottom: 1px solid #e5e7eb;
  margin-bottom: 4px;
}
.schema-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 10px;
  border-radius: 6px;
  cursor: pointer;
  transition: all 0.15s;
  margin-bottom: 2px;
}
.schema-item:hover {
  background: #e5e7eb;
}
.schema-item.active {
  background: #2563eb;
  color: #fff;
}
.schema-item-info {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.schema-item-label {
  font-size: 11px;
  font-weight: 600;
  width: 20px;
  height: 20px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #e0e7ff;
  color: #3730a3;
  border-radius: 4px;
  flex-shrink: 0;
}
.schema-item.active .schema-item-label {
  background: rgba(255, 255, 255, 0.2);
  color: #fff;
}
.schema-item-name {
  font-size: 13px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.schema-item-count {
  font-size: 11px;
  color: #9ca3af;
  flex-shrink: 0;
}
.schema-item.active .schema-item-count {
  color: rgba(255, 255, 255, 0.7);
}

/* 右侧主面板 */
.dict-main-panel {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

/* loading 遮罩 */
.content-loading-overlay {
  position: absolute;
  inset: 0;
  background: rgba(255, 255, 255, 0.7);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 10;
  border-radius: 8px;
}

/* ===== 工具栏 ===== */
.dict-toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
  flex-shrink: 0;
  flex-wrap: nowrap;
}
.toolbar-spacer {
  flex: 1;
  min-width: 4px;
}
.toolbar-search {
  width: 100px !important;
  min-width: 60px;
  flex-shrink: 1;
}
.toolbar-more {
  position: relative;
  display: inline-block;
}
.toolbar-dropdown {
  position: absolute;
  right: 0;
  top: 100%;
  margin-top: 4px;
  background: #fff;
  border: 1px solid #e5e7eb;
  border-radius: 6px;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.1);
  min-width: 120px;
  z-index: 20;
  overflow: hidden;
}
.dropdown-item {
  padding: 8px 14px;
  font-size: 13px;
  color: #374151;
  cursor: pointer;
  white-space: nowrap;
}
.dropdown-item:hover {
  background: #f3f4f6;
}
.toolbar-checkbox-wrap {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: #6b7280;
  cursor: pointer;
  user-select: none;
}
.toolbar-readonly-hint {
  font-size: 13px;
  color: #9ca3af;
  font-style: italic;
}

/* ===== 列表外层包裹（传递 flex 链） ===== */
.dict-list-wrapper {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
}

/* ===== 滚动条（圆角容器适配） ===== */
.dict-list-scrollable::-webkit-scrollbar,
.dict-table-wrap::-webkit-scrollbar,
.dict-schema-panel::-webkit-scrollbar {
  width: 6px;
}
.dict-list-scrollable::-webkit-scrollbar-track,
.dict-table-wrap::-webkit-scrollbar-track,
.dict-schema-panel::-webkit-scrollbar-track {
  background: transparent;
  margin: 4px 0;
}
.dict-list-scrollable::-webkit-scrollbar-thumb,
.dict-table-wrap::-webkit-scrollbar-thumb,
.dict-schema-panel::-webkit-scrollbar-thumb {
  background: #d1d5db;
  border-radius: 3px;
}
.dict-list-scrollable::-webkit-scrollbar-thumb:hover,
.dict-table-wrap::-webkit-scrollbar-thumb:hover,
.dict-schema-panel::-webkit-scrollbar-thumb:hover {
  background: #9ca3af;
}

/* ===== 列表（内部滚动） ===== */
.dict-list {
  background: #fff;
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  overflow: hidden;
}
.dict-list-scrollable {
  overflow-y: auto;
  flex: 1;
  min-height: 0;
}
.dict-list-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
  border-bottom: 1px solid #f3f4f6;
  transition: background 0.15s;
}
.dict-list-item:last-child {
  border-bottom: none;
}
.dict-list-item:hover {
  background: #f9fafb;
}
.dict-list-item.selected {
  background: #eff6ff;
}

.item-checkbox {
  width: 15px;
  height: 15px;
  cursor: pointer;
  accent-color: #2563eb;
  flex-shrink: 0;
}

.dict-item-main {
  display: flex;
  align-items: center;
  gap: 10px;
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
  min-width: 50px;
  flex-shrink: 0;
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
  flex-shrink: 0;
}
.dict-item-tag.tag-pin {
  background: #dcfce7;
  color: #166534;
}
.dict-item-tag.tag-delete {
  background: #fee2e2;
  color: #991b1b;
}
.dict-item-weight {
  font-size: 12px;
  color: #9ca3af;
  flex-shrink: 0;
}
.dict-item-actions {
  display: flex;
  gap: 6px;
  flex-shrink: 0;
}

/* 动态/系统标签 */
.tag-dynamic {
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 4px;
  background: #fef3c7;
  color: #92400e;
  flex-shrink: 0;
}
.tag-system {
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 4px;
  background: #f3f4f6;
  color: #6b7280;
  flex-shrink: 0;
}
.tag-mapping {
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 4px;
  background: #ede9fe;
  color: #5b21b6;
  flex-shrink: 0;
}

/* 系统短语禁用状态 */
.item-disabled {
  opacity: 0.5;
}
.text-disabled {
  text-decoration: line-through;
}
.item-switch-mini {
  flex-shrink: 0;
  cursor: pointer;
}
.item-switch-mini input {
  cursor: pointer;
}

/* ===== 词库表格 ===== */
.dict-table-wrap {
  overflow-y: auto;
  flex: 1;
  min-height: 0;
  border: 1px solid #e5e7eb;
  border-radius: 8px;
}
.dict-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
  table-layout: fixed;
}
.dict-table col.col-check {
  width: 32px;
}
.dict-table col.col-code {
  width: 100px;
}
.dict-table col.col-weight {
  width: 60px;
}
.dict-table col.col-action {
  width: 50px;
}
.dict-table thead {
  position: sticky;
  top: 0;
  background: #f9fafb;
  z-index: 1;
}
.dict-table th {
  padding: 9px 12px;
  text-align: left;
  font-weight: 600;
  color: #6b7280;
  border-bottom: 1px solid #e5e7eb;
}
.sortable-th {
  cursor: pointer;
  user-select: none;
}
.sortable-th:hover {
  color: #374151;
}
.sort-icon {
  font-size: 11px;
  margin-left: 4px;
  color: #9ca3af;
}
.dict-table td {
  padding: 9px 12px;
  border-bottom: 1px solid #f3f4f6;
  color: #1f2937;
  background: #fff;
}
.dict-table tr:last-child td {
  border-bottom: none;
}
.dict-table tbody tr:hover td {
  background: #f9fafb;
}
.dict-table tbody tr.selected td {
  background: #eff6ff;
}
.dict-table tbody tr {
  cursor: default;
}
.td-weight {
  color: #9ca3af;
  font-size: 12px;
}

/* ===== 空状态 ===== */
.dict-empty {
  text-align: center;
  padding: 48px 24px;
  color: #9ca3af;
  background: #f9fafb;
  border-radius: 8px;
  flex: 1;
}

/* ===== 危险按钮（outline） ===== */
.btn-danger-outline {
  color: #dc2626;
  border-color: #fca5a5;
}
.btn-danger-outline:hover {
  background: #fee2e2;
  border-color: #dc2626;
}

/* ===== 其他 ===== */
.dict-note {
  font-size: 12px;
  color: #9ca3af;
  font-style: italic;
  margin-top: 6px;
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
