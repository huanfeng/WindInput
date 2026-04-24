# 移除文件后端，统一 Store 存储层

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 移除所有文件后端 fallback 代码，让 DictManager 仅使用 bbolt Store 后端，同时将拼音用户词频从文件迁移到 Store，清理 schema 配置中已废弃的 `user_data` 文件路径字段。

**Architecture:** DictManager 移除 `useStore` 双路径设计，直接使用 Store 后端。`SwitchSchemaFull` 签名简化为只接受 `schemaID`，混输方案的数据映射改为从 schema config 的 `primary_schema` 推导。拼音 `UnigramModel` 的 `LoadUserFreqs`/`SaveUserFreqs` 改为读写 Store 的 Freq bucket。

**Tech Stack:** Go, bbolt, YAML schema config

---

## 文件变更概览

### 删除文件
| 文件 | 原因 |
|------|------|
| `wind_input/internal/dict/user_dict.go` | 文件后端用户词库，被 `StoreUserLayer` 替代 |
| `wind_input/internal/dict/user_dict_freq_test.go` | 依赖 `UserDict` 的测试 |
| `wind_input/internal/dict/temp_dict.go` | 文件后端临时词库，被 `StoreTempLayer` 替代 |
| `wind_input/internal/dict/shadow.go` | 文件后端 Shadow 层，被 `StoreShadowLayer` 替代 |
| `wind_input/internal/dict/shadow_test.go` | 依赖 `ShadowLayer` 的测试 |
| `wind_input/internal/dict/shadow_order_test.go` | 依赖 `ShadowLayer` 的测试 |
| `wind_input/internal/dict/manager_test.go` | 测试文件后端，需重写为 Store 后端 |
| `wind_setting/internal/editor/shadow.go` | 设置端旧 Shadow 编辑器（已改用 RPC） |
| `wind_setting/internal/editor/userdict.go` | 设置端旧用户词库编辑器（已改用 RPC） |
| `wind_setting/internal/editor/phrase.go` | 设置端旧短语编辑器（已改用 RPC） |
| `wind_setting/internal/editor/AGENTS.md` | editor 包文档 |
| `wind_input/pkg/dictfile/types.go` | dictfile 包类型（仅被 editor 引用） |
| `wind_input/pkg/dictfile/userdict.go` | dictfile 包用户词库操作 |
| `wind_input/pkg/dictfile/shadow.go` | dictfile 包 Shadow 操作 |
| `wind_input/pkg/dictfile/phrase.go` | dictfile 包短语操作 |
| `wind_input/pkg/dictfile/AGENTS.md` | dictfile 包文档 |

### 修改文件
| 文件 | 变更 |
|------|------|
| `wind_input/internal/dict/manager.go` | 移除所有文件后端字段、`useStore` 标志、`switchSchemaFile`、`resolveDataPath`、`GetUserDict()`、`GetTempDict()`、`GetShadowLayer()` 等；简化 `SwitchSchemaFull` 签名 |
| `wind_input/internal/dict/phrase.go` | 移除 `Load()`、`loadFile()`、`GetUserFilePath()`、`GetSystemFilePath()` 等文件操作方法；保留 `parsePhraseYAMLFile`（SeedDefaultPhrases 使用） |
| `wind_input/internal/engine/pinyin/pinyin.go` | 移除 `onCandidateSelectedFile()`；简化 `OnCandidateSelected` |
| `wind_input/internal/engine/codetable/codetable.go` | 移除文件后端选词分支 |
| `wind_input/internal/rpc/system_service.go` | 移除 `ReloadUserDict` 中文件后端分支 |
| `wind_input/cmd/service/main.go` | 调整 `SwitchSchemaFull` 调用签名 |
| `wind_input/internal/coordinator/reload_handler.go` | 调整 `SwitchSchemaFull` 调用签名 |
| `wind_input/internal/schema/schema.go` | 移除 `UserDataSpec` 中已废弃字段，保留 `UserFreqFile` → 改为在 Store 后端也兼容 |
| `wind_input/internal/schema/loader.go` | 更新 `resolveMixedSchemaUserData` 和验证逻辑 |
| `wind_input/internal/schema/factory.go` | 更新 pinyin 用户词频加载为 Store 后端 |
| `wind_input/internal/engine/manager_convert.go` | 更新 `savePinyinUserFreqsLocked` 为 Store 后端 |
| `wind_input/internal/engine/pinyin/lm.go` | 新增 `LoadUserFreqsFromStore`/`SaveUserFreqsToStore` 方法 |
| `data/schemas/pinyin.schema.yaml` | 移除 `user_data` 中的文件路径字段 |
| `data/schemas/wubi86.schema.yaml` | 同上 |
| `data/schemas/shuangpin.schema.yaml` | 同上 |

### 新建文件
| 文件 | 说明 |
|------|------|
| `wind_input/internal/dict/manager_test.go` | 重写为基于 Store 后端的测试 |

---

## Task 1: 删除已确认无引用的旧编辑器和 dictfile 包

**Files:**
- Delete: `wind_setting/internal/editor/shadow.go`
- Delete: `wind_setting/internal/editor/userdict.go`
- Delete: `wind_setting/internal/editor/phrase.go`
- Delete: `wind_setting/internal/editor/AGENTS.md`
- Delete: `wind_input/pkg/dictfile/types.go`
- Delete: `wind_input/pkg/dictfile/userdict.go`
- Delete: `wind_input/pkg/dictfile/shadow.go`
- Delete: `wind_input/pkg/dictfile/phrase.go`
- Delete: `wind_input/pkg/dictfile/AGENTS.md`

- [ ] **Step 1: 删除 editor 包中的旧编辑器文件**

```bash
rm wind_setting/internal/editor/shadow.go
rm wind_setting/internal/editor/userdict.go
rm wind_setting/internal/editor/phrase.go
rm wind_setting/internal/editor/AGENTS.md
```

保留 `base.go`（`BaseEditor`）和 `config.go`（`ConfigEditor`），这两个仍被 `app.go` 使用。

- [ ] **Step 2: 删除 dictfile 包**

```bash
rm wind_input/pkg/dictfile/types.go
rm wind_input/pkg/dictfile/userdict.go
rm wind_input/pkg/dictfile/shadow.go
rm wind_input/pkg/dictfile/phrase.go
rm wind_input/pkg/dictfile/AGENTS.md
```

如果 dictfile 目录中只剩 AGENTS.md 以外没有其他文件，删除整个目录。

- [ ] **Step 3: 验证编译**

```bash
cd wind_input && go build ./...
cd wind_setting && go build ./...
```

Expected: 编译通过，无引用错误。

- [ ] **Step 4: 运行测试**

```bash
cd wind_input && go test ./...
cd wind_setting && go test ./...
```

---

## Task 2: 删除文件后端实现文件

**Files:**
- Delete: `wind_input/internal/dict/user_dict.go`
- Delete: `wind_input/internal/dict/user_dict_freq_test.go`
- Delete: `wind_input/internal/dict/temp_dict.go`
- Delete: `wind_input/internal/dict/shadow.go`
- Delete: `wind_input/internal/dict/shadow_test.go`
- Delete: `wind_input/internal/dict/shadow_order_test.go`
- Delete: `wind_input/internal/dict/manager_test.go`

**注意:** 此 Task 会导致编译失败，因为 `manager.go` 和引擎层仍引用 `UserDict`、`TempDict`、`ShadowLayer`。Task 3 将修复这些引用。Task 2 和 Task 3 应作为一个原子操作执行。

- [ ] **Step 1: 删除文件后端实现和相关测试**

```bash
rm wind_input/internal/dict/user_dict.go
rm wind_input/internal/dict/user_dict_freq_test.go
rm wind_input/internal/dict/temp_dict.go
rm wind_input/internal/dict/shadow.go
rm wind_input/internal/dict/shadow_test.go
rm wind_input/internal/dict/shadow_order_test.go
rm wind_input/internal/dict/manager_test.go
```

---

## Task 3: 清理 manager.go — 移除文件后端代码

**Files:**
- Modify: `wind_input/internal/dict/manager.go`

- [ ] **Step 1: 移除文件后端字段和初始化**

从 `DictManager` 结构体中移除：
```go
// 删除以下字段:
shadowLayers map[string]*ShadowLayer
userDicts    map[string]*UserDict
tempDicts    map[string]*TempDict
activeShadow   *ShadowLayer
activeUserDict *UserDict
activeTempDict *TempDict
```

从 `NewDictManager` 中移除对应初始化：
```go
// 删除:
shadowLayers:      make(map[string]*ShadowLayer),
userDicts:         make(map[string]*UserDict),
tempDicts:         make(map[string]*TempDict),
```

- [ ] **Step 2: 移除 `useStore` 标志和 `OpenStore` 改造**

将 `useStore` 字段移除。`OpenStore` 只保留打开 Store 的逻辑，不再设置标志。所有 `if dm.useStore` 判断都移除，直接走 Store 路径。

移除 `UseStore() bool` 方法。

- [ ] **Step 3: 简化 `Initialize()`**

移除 `else` 文件后端分支，只保留 Store 路径：

```go
func (dm *DictManager) Initialize() error {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	systemPhrasePath := filepath.Join(dm.systemDir, "system.phrases.yaml")
	systemPhraseUserPath := filepath.Join(dm.dataDir, "system.phrases.yaml")
	userPhrasePath := filepath.Join(dm.dataDir, "user.phrases.yaml")
	dm.phraseLayer = NewPhraseLayerEx("phrases", systemPhrasePath, systemPhraseUserPath, userPhrasePath)

	if err := dm.SeedDefaultPhrases(); err != nil {
		dm.logger.Error("种子默认短语失败", "error", err)
	}
	if err := dm.phraseLayer.LoadFromStore(dm.store); err != nil {
		dm.logger.Warn("从 Store 加载短语失败", "error", err)
	} else {
		dm.logger.Info("短语层从 Store 加载成功",
			"phrases", dm.phraseLayer.GetPhraseCount(),
			"commands", dm.phraseLayer.GetCommandCount())
	}

	dm.compositeDict.AddLayer(dm.phraseLayer)
	return nil
}
```

- [ ] **Step 4: 简化 `SwitchSchemaFull` 签名**

旧签名：`SwitchSchemaFull(schemaID, shadowFile, userDictFile, tempDictFile string, ...)`
新签名：`SwitchSchemaFull(schemaID, dataSchemaID string, tempMaxEntries, tempPromoteCount int)`

`dataSchemaID` 由调用方从 schema config 推导（混输方案取 `primary_schema`，普通方案取自身 ID）。

同时移除旧的 `SwitchSchema` 三参数便捷方法、`switchSchemaFile`、`resolveDataPath`、`deriveDataSchemaID`。

将 `switchSchemaStore` 内联到 `SwitchSchemaFull`（它现在是唯一路径）。

```go
func (dm *DictManager) SwitchSchemaFull(schemaID, dataSchemaID string, tempMaxEntries, tempPromoteCount int) {
	dm.mu.Lock()
	defer dm.mu.Unlock()

	if schemaID == dm.activeSchemaID {
		return
	}

	dm.logger.Info("Store 方案切换", "schemaID", schemaID, "dataSchemaID", dataSchemaID)
	dm.activeDataSchemaID = dataSchemaID

	// (原 switchSchemaStore 的完整逻辑，直接内联)
	// 1. 移除旧 Store 用户词库层
	if dm.activeStoreUser != nil {
		dm.compositeDict.RemoveLayer(dm.activeStoreUser.Name())
	}
	// 2. 懒加载 StoreShadowLayer
	shadowLayer, ok := dm.storeShadowLayers[dataSchemaID]
	if !ok {
		shadowLayer = NewStoreShadowLayer(dm.store, dataSchemaID)
		dm.storeShadowLayers[dataSchemaID] = shadowLayer
	}
	dm.compositeDict.SetShadowProvider(shadowLayer)
	dm.activeStoreShadow = shadowLayer
	// 3. 懒加载 StoreUserLayer
	userLayer, ok := dm.storeUserLayers[dataSchemaID]
	if !ok {
		userLayer = NewStoreUserLayer(dm.store, dataSchemaID)
		dm.storeUserLayers[dataSchemaID] = userLayer
	}
	dm.compositeDict.AddLayer(userLayer)
	dm.activeStoreUser = userLayer
	// 4. 词频评分器
	scorer, ok := dm.freqScorers[dataSchemaID]
	if !ok {
		scorer = NewStoreFreqScorer(dm.store, dataSchemaID)
		dm.freqScorers[dataSchemaID] = scorer
	}
	dm.compositeDict.SetFreqScorer(scorer)
	// 5. 懒加载 StoreTempLayer
	if dm.activeStoreTemp != nil {
		dm.compositeDict.RemoveLayer(dm.activeStoreTemp.Name())
	}
	tempLayer, ok := dm.storeTempLayers[dataSchemaID]
	if !ok {
		tempLayer = NewStoreTempLayer(dm.store, dataSchemaID)
		tempLayer.SetLimits(tempMaxEntries, tempPromoteCount)
		dm.storeTempLayers[dataSchemaID] = tempLayer
	}
	dm.compositeDict.AddLayer(tempLayer)
	dm.activeStoreTemp = tempLayer

	dm.activeSchemaID = schemaID
	dm.logger.Info("切换到方案", "schemaID", schemaID)
}
```

- [ ] **Step 5: 移除旧的访问器方法**

删除以下方法：
- `GetUserDict() *UserDict`
- `GetTempDict() *TempDict`
- `GetShadowLayer() *ShadowLayer`

简化 `GetShadowProvider()`，移除 `useStore` 判断：
```go
func (dm *DictManager) GetShadowProvider() ShadowProvider {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	return dm.activeStoreShadow
}
```

简化 `GetActiveSchemaID()`：
```go
func (dm *DictManager) GetActiveSchemaID() string {
	dm.mu.RLock()
	defer dm.mu.RUnlock()
	if dm.activeDataSchemaID != "" {
		return dm.activeDataSchemaID
	}
	return dm.activeSchemaID
}
```

- [ ] **Step 6: 简化 Save / Close / 其他方法**

`Save()` — Store 后端自动持久化，方法体简化为 `return nil`。

`Close()` — 移除文件后端清理（`userDicts`、`tempDicts`、`shadowLayers` 的遍历），只保留 Store 关闭。

`SaveShadow()` — 简化为 `return nil`。

`AddUserWord`、`PinWord`、`DeleteWord`、`RemoveShadowRule`、`HasShadowRule` — 移除 `!useStore` 分支。

`ReloadPhrases` — 移除文件后端分支。

`ReloadShadow` — 移除整个方法（仅操作文件后端）。

`GetStats` — 移除 `!useStore` 分支。

- [ ] **Step 7: 清理 import**

移除不再需要的 `"os"` import（`resolveDataPath` 删除后不再需要）。

- [ ] **Step 8: 验证编译**

```bash
cd wind_input && go build ./...
```

此步可能因引擎层和 RPC 层仍引用旧方法而失败，Task 4 修复。

---

## Task 4: 清理引擎层和 RPC 层的文件后端代码

**Files:**
- Modify: `wind_input/internal/engine/pinyin/pinyin.go`
- Modify: `wind_input/internal/engine/codetable/codetable.go`
- Modify: `wind_input/internal/rpc/system_service.go`

- [ ] **Step 1: 清理 pinyin 引擎 `OnCandidateSelected`**

移除 `onCandidateSelectedFile` 方法。简化 `OnCandidateSelected`，移除 `UseStore()` 判断，直接调用 `onCandidateSelectedStore`（可内联）：

```go
func (e *Engine) OnCandidateSelected(code, text string) {
	if e.config == nil || !e.config.EnableUserFreq {
		return
	}
	if e.dictManager == nil {
		return
	}

	runes := []rune(text)
	if len(runes) < 2 {
		if e.unigram != nil {
			e.unigram.BoostUserFreq(text, 1)
		}
		return
	}

	// 记录独立词频
	if s := e.dictManager.GetStore(); s != nil {
		s.IncrementFreq(e.dictManager.GetActiveSchemaID(), code, text)
	}

	if e.config.FrequencyOnly {
		if userLayer := e.dictManager.GetStoreUserLayer(); userLayer != nil {
			userLayer.IncreaseWeight(code, text, 20)
		}
	} else {
		tempLayer := e.dictManager.GetStoreTempLayer()
		if tempLayer != nil {
			promoted := tempLayer.LearnWord(code, text, 20)
			if promoted {
				tempLayer.PromoteWord(code, text)
			}
		} else {
			if userLayer := e.dictManager.GetStoreUserLayer(); userLayer != nil {
				userLayer.OnWordSelected(code, text, 800, 20, 2)
			}
		}
	}

	if e.unigram != nil {
		e.unigram.BoostUserFreq(text, 1)
	}
}
```

- [ ] **Step 2: 清理 codetable 引擎 `OnCandidateSelected`**

移除文件后端分支（`GetUserDict()`/`GetTempDict()` 调用），移除 `UseStore()` 判断，直接调用 `onCandidateSelectedStore`。

- [ ] **Step 3: 清理 RPC `ReloadUserDict`**

移除文件后端分支：
```go
func (s *SystemService) ReloadUserDict(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	s.logger.Info("RPC System.ReloadUserDict")
	if s.dm == nil {
		return fmt.Errorf("dict manager not available")
	}
	// Store 后端实时读取，无需手动重载
	return nil
}
```

- [ ] **Step 4: 验证编译**

```bash
cd wind_input && go build ./...
```

Expected: 编译通过。

---

## Task 5: 清理 phrase.go 文件操作方法

**Files:**
- Modify: `wind_input/internal/dict/phrase.go`

- [ ] **Step 1: 移除文件后端方法**

删除以下方法和函数：
- `Load()` 方法（第253-290行）— Store 后端使用 `LoadFromStore()`
- `loadFile()` 私有方法（第389-465行）
- `removeEntryByID()` 私有方法（第467-491行）— 仅被 `loadFile` 使用
- `GetUserFilePath()` 和 `GetSystemFilePath()` — 暴露文件路径的 getter

保留：
- `parsePhraseYAMLFile()` — 被 `SeedDefaultPhrases()` 使用（从系统打包文件种子到 Store）
- `detectPhraseType()` — 被 `SeedDefaultPhrases()` 使用
- `LoadFromStore()` — 当前的主加载路径
- `NewPhraseLayerEx()` — 仍需要文件路径用于 `SeedDefaultPhrases` 定位系统短语文件

保留 `PhraseLayer` 结构体中的 `systemFilePath`、`systemUserFilePath`、`userFilePath` 字段，因为 `SeedDefaultPhrases` 通过 manager 间接需要这些路径（实际路径在 manager 中硬编码，但 PhraseLayer 构造函数需要它们）。

- [ ] **Step 2: 清理 import**

移除不再需要的 `"github.com/huanfeng/wind_input/pkg/fileutil"` 和 `"os"` import（如果 `parsePhraseYAMLFile` 仍需要 `os` 则保留）。

- [ ] **Step 3: 验证编译和测试**

```bash
cd wind_input && go build ./... && go test ./internal/dict/...
```

---

## Task 6: 更新 SwitchSchemaFull 调用方

**Files:**
- Modify: `wind_input/cmd/service/main.go`
- Modify: `wind_input/internal/coordinator/reload_handler.go`
- Modify: `wind_input/internal/schema/schema.go`
- Modify: `wind_input/internal/schema/loader.go`

- [ ] **Step 1: 添加 dataSchemaID 推导辅助函数**

在 `schema.go` 中新增方法，用于从 Schema 推导数据方案 ID：

```go
// DataSchemaID 返回数据方案 ID
// 混输方案返回主方案 ID（与主方案共享用户数据），其他返回自身 ID
func (s *Schema) DataSchemaID() string {
	if s.Engine.Type == EngineTypeMixed && s.Engine.Mixed != nil && s.Engine.Mixed.PrimarySchema != "" {
		return s.Engine.Mixed.PrimarySchema
	}
	return s.Schema.ID
}
```

- [ ] **Step 2: 更新 main.go 的调用**

```go
// 旧：
dictManager.SwitchSchemaFull(activeSchemaID, activeSchema.UserData.ShadowFile, activeSchema.UserData.UserDictFile,
    activeSchema.UserData.TempDictFile, activeSchema.Learning.TempMaxEntries, activeSchema.Learning.TempPromoteCount)

// 新：
dictManager.SwitchSchemaFull(activeSchemaID, activeSchema.DataSchemaID(),
    activeSchema.Learning.TempMaxEntries, activeSchema.Learning.TempPromoteCount)
```

- [ ] **Step 3: 更新 reload_handler.go 的调用**

```go
// 旧：
h.dictMgr.SwitchSchemaFull(newSchemaID, s.UserData.ShadowFile, s.UserData.UserDictFile,
    s.UserData.TempDictFile, s.Learning.TempMaxEntries, s.Learning.TempPromoteCount)

// 新：
h.dictMgr.SwitchSchemaFull(newSchemaID, s.DataSchemaID(),
    s.Learning.TempMaxEntries, s.Learning.TempPromoteCount)
```

- [ ] **Step 4: 清理 UserDataSpec**

从 `schema.go` 的 `UserDataSpec` 中移除已废弃字段：

```go
// 旧：
type UserDataSpec struct {
	ShadowFile   string `yaml:"shadow_file"`
	UserDictFile string `yaml:"user_dict_file"`
	TempDictFile string `yaml:"temp_dict_file,omitempty"`
	UserFreqFile string `yaml:"user_freq_file,omitempty"`
}

// 新（仅保留拼音词频，待 Task 8 也迁移后可完全移除）：
type UserDataSpec struct {
	UserFreqFile string `yaml:"user_freq_file,omitempty"`
}
```

- [ ] **Step 5: 更新 loader.go 验证和继承逻辑**

移除 `resolveMixedSchemaUserData` 中对 `ShadowFile`、`UserDictFile`、`TempDictFile` 的继承逻辑，只保留 `UserFreqFile` 的继承。

移除 `validateSchema` 中对 `ShadowFile`、`UserDictFile` 非空的校验。

- [ ] **Step 6: 验证编译**

```bash
cd wind_input && go build ./...
```

---

## Task 7: 更新 Schema YAML 配置文件

**Files:**
- Modify: `data/schemas/pinyin.schema.yaml`
- Modify: `data/schemas/wubi86.schema.yaml`
- Modify: `data/schemas/shuangpin.schema.yaml`

- [ ] **Step 1: 清理 pinyin.schema.yaml**

```yaml
# 旧：
user_data:
  shadow_file: "pinyin.shadow.yaml"
  user_dict_file: "pinyin.userwords.txt"
  user_freq_file: "pinyin.userfreq.txt"
  temp_dict_file: "pinyin.tempdict.txt"

# 新（仅保留拼音词频路径，Task 8 迁移后也可移除）：
user_data:
  user_freq_file: "pinyin.userfreq.txt"
```

- [ ] **Step 2: 清理 wubi86.schema.yaml**

移除整个 `user_data` 节（五笔无 userFreqFile）。

- [ ] **Step 3: 清理 shuangpin.schema.yaml**

类似 pinyin，保留 `user_freq_file`（如有），移除其余。

- [ ] **Step 4: 更新 schema_test.go**

`schema_test.go` 中检查 `UserData.ShadowFile` 的测试用例需要更新或移除。

- [ ] **Step 5: 验证编译和测试**

```bash
cd wind_input && go build ./... && go test ./internal/schema/...
```

---

## Task 8: 迁移拼音用户词频到 Store

**Files:**
- Modify: `wind_input/internal/engine/pinyin/lm.go` — 新增 Store 读写方法
- Modify: `wind_input/internal/schema/factory.go` — 改用 Store 加载词频
- Modify: `wind_input/internal/engine/manager_convert.go` — 改用 Store 保存词频
- Modify: `wind_input/internal/store/freq.go` — 如需新增拼音词频专用方法

- [ ] **Step 1: 为 UnigramModel 添加 Store 读写方法**

在 `lm.go` 中新增：

```go
// LoadUserFreqsFromStore 从 Store 的 Freq bucket 加载拼音用户词频
func (m *UnigramModel) LoadUserFreqsFromStore(s *store.Store, schemaID string) error {
	records, err := s.GetFreqByPrefix(schemaID, "")
	if err != nil {
		return err
	}
	if m.userFreqs == nil {
		m.userFreqs = make(map[string]int)
	}
	for _, rec := range records {
		m.userFreqs[rec.Text] = rec.Count
	}
	return nil
}

// SaveUserFreqsToStore 将用户词频保存到 Store
func (m *UnigramModel) SaveUserFreqsToStore(s *store.Store, schemaID string) error {
	if m.userFreqs == nil || len(m.userFreqs) == 0 {
		return nil
	}
	for word, freq := range m.userFreqs {
		if err := s.SetFreq(schemaID, "", word, freq); err != nil {
			return err
		}
	}
	return nil
}
```

同样为 `BinaryUnigramModel` 添加相同方法。

注意：需要确认 Store 的 Freq bucket 的 key 设计是否支持按 `schemaID` + 空 code + text 的格式。如果 Freq bucket 的 key 是 `schemaID:code:text`，拼音词频可用空 code 或特殊前缀（如 `__unigram__`）作为 code。

- [ ] **Step 2: 更新 factory.go 加载逻辑**

将 `loadPinyinUserFreqs` 改为从 Store 加载：

```go
func loadPinyinUserFreqs(engine *pinyin.Engine, s *store.Store, schemaID string, logger *slog.Logger) {
	if engine.GetUnigram() == nil {
		return
	}
	if m := engine.GetUnigramModel(); m != nil {
		if err := m.LoadUserFreqsFromStore(s, schemaID); err != nil {
			logger.Warn("加载拼音用户词频失败", "error", err)
		}
	}
}
```

更新 `createPinyinEngine` 和 `createMixedEngine` 中的调用，传入 Store 而非文件路径。

这需要 `createPinyinEngine` / `createMixedEngine` 的参数中添加 `*store.Store`，或通过 DictManager 获取。

- [ ] **Step 3: 更新 manager_convert.go 保存逻辑**

将 `savePinyinUserFreqsLocked` 改为写入 Store：

```go
func (m *Manager) savePinyinUserFreqsLocked(schemaID string, pinyinEngine *pinyin.Engine) {
	cfg := pinyinEngine.GetConfig()
	if cfg == nil || !cfg.EnableUserFreq {
		return
	}
	if m.dictManager == nil || m.dictManager.GetStore() == nil {
		return
	}
	if model := pinyinEngine.GetUnigramModel(); model != nil {
		if err := model.SaveUserFreqsToStore(m.dictManager.GetStore(), schemaID); err != nil {
			slog.Error("保存拼音用户词频失败", "error", err)
		}
	}
}
```

- [ ] **Step 4: 添加首次迁移逻辑（可选）**

类似 `SeedDefaultPhrases`，可在首次启动时检查 Store 中是否有拼音词频数据，如果没有则从旧文件 `pinyin.userfreq.txt` 导入。由于用户确认可接受轻微数据丢失，此步为可选。

- [ ] **Step 5: 清理 UserDataSpec 和 schema YAML**

迁移完成后，从 `UserDataSpec` 移除 `UserFreqFile`，从 schema YAML 移除 `user_data` 整个节。

如果 `UserDataSpec` 变为空结构体，从 `Schema` 中移除该字段。

- [ ] **Step 6: 清理旧的文件 Load/Save 方法**

从 `lm.go` 中移除：
- `UnigramModel.LoadUserFreqs(path string)`
- `UnigramModel.SaveUserFreqs(path string)`
- `BinaryUnigramModel.LoadUserFreqs(path string)`
- `BinaryUnigramModel.SaveUserFreqs(path string)`

从 `factory.go` 中移除 `SavePinyinUserFreqs` 的文件版本（如有导出函数）。

- [ ] **Step 7: 验证编译和测试**

```bash
cd wind_input && go build ./... && go test ./...
```

---

## Task 9: 重写 manager_test.go

**Files:**
- Create: `wind_input/internal/dict/manager_test.go`

- [ ] **Step 1: 编写基于 Store 的测试**

```go
package dict

import (
	"path/filepath"
	"testing"

	"github.com/huanfeng/wind_input/internal/store"
)

func setupTestManager(t *testing.T) (*DictManager, func()) {
	t.Helper()
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "test.db")

	dm := NewDictManager(tmpDir, tmpDir, nil)
	if err := dm.OpenStore(dbPath); err != nil {
		t.Fatalf("OpenStore failed: %v", err)
	}
	if err := dm.Initialize(); err != nil {
		t.Fatalf("Initialize failed: %v", err)
	}

	return dm, func() { dm.Close() }
}

func TestDictManager_SwitchSchema(t *testing.T) {
	dm, cleanup := setupTestManager(t)
	defer cleanup()

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)

	if dm.GetActiveSchemaID() != "wubi86" {
		t.Errorf("expected wubi86, got %s", dm.GetActiveSchemaID())
	}
	if dm.GetStoreShadowLayer() == nil {
		t.Error("StoreShadowLayer should not be nil")
	}
	if dm.GetStoreUserLayer() == nil {
		t.Error("StoreUserLayer should not be nil")
	}

	// 添加用户词
	if err := dm.AddUserWord("test", "测试", 100); err != nil {
		t.Fatalf("AddUserWord failed: %v", err)
	}

	// 切换到 pinyin
	dm.SwitchSchemaFull("pinyin", "pinyin", 5000, 5)
	if dm.GetActiveSchemaID() != "pinyin" {
		t.Errorf("expected pinyin, got %s", dm.GetActiveSchemaID())
	}
	if dm.GetStoreUserLayer().EntryCount() != 0 {
		t.Errorf("pinyin should have 0 entries, got %d", dm.GetStoreUserLayer().EntryCount())
	}

	// 切换回 wubi86，数据应保留
	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	if dm.GetStoreUserLayer().EntryCount() != 1 {
		t.Errorf("wubi86 should have 1 entry, got %d", dm.GetStoreUserLayer().EntryCount())
	}
}

func TestDictManager_ShadowIsolation(t *testing.T) {
	dm, cleanup := setupTestManager(t)
	defer cleanup()

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	dm.PinWord("abc", "测试", 0)

	rules := dm.GetStoreShadowLayer().GetShadowRules("abc")
	if rules == nil || len(rules.Pinned) != 1 {
		t.Fatal("wubi86 should have 1 pin rule")
	}

	dm.SwitchSchemaFull("pinyin", "pinyin", 5000, 5)
	pinyinRules := dm.GetStoreShadowLayer().GetShadowRules("abc")
	if pinyinRules != nil && (len(pinyinRules.Pinned) > 0 || len(pinyinRules.Deleted) > 0) {
		t.Error("pinyin should have no shadow rules")
	}

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	rules2 := dm.GetStoreShadowLayer().GetShadowRules("abc")
	if rules2 == nil || len(rules2.Pinned) != 1 {
		t.Error("wubi86 shadow rules should persist")
	}
}

func TestDictManager_SameSchemaNoOp(t *testing.T) {
	dm, cleanup := setupTestManager(t)
	defer cleanup()

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5)
	dm.AddUserWord("a", "甲", 100)

	dm.SwitchSchemaFull("wubi86", "wubi86", 5000, 5) // no-op
	if dm.GetStoreUserLayer().EntryCount() != 1 {
		t.Error("same-schema switch should not lose data")
	}
}
```

- [ ] **Step 2: 运行测试**

```bash
cd wind_input && go test ./internal/dict/... -v
```

---

## Task 10: 全量编译和测试验证

- [ ] **Step 1: Go fmt**

```bash
cd wind_input && go fmt ./...
```

- [ ] **Step 2: 全量编译**

```bash
cd wind_input && go build ./...
cd wind_setting && go build ./...
```

- [ ] **Step 3: 全量测试**

```bash
cd wind_input && go test ./...
```

- [ ] **Step 4: 前端编译**

```bash
cd wind_setting/frontend && pnpm build
```
