package rpc

import (
	"fmt"
	"log/slog"
	"strings"

	"github.com/huanfeng/wind_input/internal/cmdbar"
	cmdbareval "github.com/huanfeng/wind_input/internal/cmdbar/eval"
	cmdbarparser "github.com/huanfeng/wind_input/internal/cmdbar/parser"
	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// PhraseService 短语管理 RPC 服务
type PhraseService struct {
	store       *store.Store
	dm          *dict.DictManager
	logger      *slog.Logger
	broadcaster *EventBroadcaster
}

// reloadPhrases 通知引擎重新从 Store 加载短语到内存
func (p *PhraseService) reloadPhrases() {
	if p.dm != nil {
		if err := p.dm.ReloadPhrases(); err != nil {
			p.logger.Error("重载短语失败", "error", err)
		}
	}
}

// List 获取所有短语
func (p *PhraseService) List(args *rpcapi.Empty, reply *rpcapi.PhraseListReply) error {
	if p.store == nil {
		return fmt.Errorf("store not available")
	}

	records, err := p.store.GetAllPhrases()
	if err != nil {
		return fmt.Errorf("list phrases: %w", err)
	}

	reply.Total = len(records)
	reply.Phrases = make([]rpcapi.PhraseEntry, len(records))
	for i, rec := range records {
		reply.Phrases[i] = rpcapi.PhraseEntry{
			Code:     rec.Code,
			Text:     rec.Text,
			Texts:    rec.Texts,
			Name:     rec.Name,
			Type:     rec.Type,
			Weight:   rec.Weight,
			Position: rec.Position,
			Enabled:  rec.Enabled,
			IsSystem: rec.IsSystem,
		}
	}

	p.logger.Info("RPC Phrase.List", "count", reply.Total)
	return nil
}

// Add 添加短语
func (p *PhraseService) Add(args *rpcapi.PhraseAddArgs, reply *rpcapi.Empty) error {
	if p.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" {
		return fmt.Errorf("code is required")
	}
	if args.Type == "array" {
		if args.Texts == "" || args.Name == "" {
			return fmt.Errorf("texts and name are required for array type")
		}
	} else {
		if args.Text == "" {
			return fmt.Errorf("text is required")
		}
	}

	// 自动从 Text 推断 Type / 拆出字符组 marker, 供新版 yaml 导入路径
	// (该路径只填 Code/Text/Weight, Type/Texts/Name 留空)。
	pType := args.Type
	pText := args.Text
	pTexts := args.Texts
	pName := args.Name
	if pType == "" {
		if name, chars, ok := dict.ParseAAMarker(pText); ok {
			pType = "array"
			pTexts = chars
			pName = name
			pText = "" // array 类型 store 不再保存原始 marker 字符串
		} else if dict.HasVariable(pText) {
			pType = "dynamic"
		} else {
			pType = "static"
		}
	}

	rec := store.PhraseRecord{
		Code:     args.Code,
		Text:     pText,
		Texts:    pTexts,
		Name:     pName,
		Type:     pType,
		Weight:   args.Weight,
		Position: args.Position,
		Enabled:  true,
	}

	p.logger.Info("RPC Phrase.Add", "type", pType, "codeLen", len(args.Code))
	if err := p.store.AddPhrase(rec); err != nil {
		return err
	}
	p.reloadPhrases()
	p.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypePhrase, Action: rpcapi.EventActionAdd})
	return nil
}

// Update 更新短语
func (p *PhraseService) Update(args *rpcapi.PhraseUpdateArgs, reply *rpcapi.Empty) error {
	if p.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" {
		return fmt.Errorf("code is required")
	}

	// 处理启用/禁用切换
	if args.Enabled != nil {
		if err := p.store.SetPhraseEnabled(args.Code, args.Text, args.Name, *args.Enabled); err != nil {
			return fmt.Errorf("set phrase enabled: %w", err)
		}
		p.logger.Info("RPC Phrase.Update enabled", "codeLen", len(args.Code), "enabled", *args.Enabled)
	}

	// 处理文本、编码、位置或权重更新
	if args.NewText != "" || args.NewPosition != 0 || args.NewCode != "" || args.NewWeight != nil {
		// 读取现有记录
		records, err := p.store.GetPhrasesByCode(args.Code)
		if err != nil {
			return fmt.Errorf("get phrases by code: %w", err)
		}

		// 找到匹配的记录
		var found *store.PhraseRecord
		for i := range records {
			rec := &records[i]
			if args.Name != "" {
				if rec.Name == args.Name {
					found = rec
					break
				}
			} else if rec.Text == args.Text {
				found = rec
				break
			}
		}
		if found == nil {
			return fmt.Errorf("phrase not found")
		}

		needDelete := false
		if args.NewText != "" {
			found.Text = args.NewText
			needDelete = true
		}
		if args.NewCode != "" && args.NewCode != args.Code {
			found.Code = args.NewCode
			needDelete = true
		}
		if args.NewPosition != 0 {
			found.Position = args.NewPosition
		}
		if args.NewWeight != nil {
			// 显式覆盖 weight: clamp 到 [0, NormalizedWeightMax]
			w := *args.NewWeight
			if w < 0 {
				w = 0
			}
			if w > 10000 {
				w = 10000
			}
			found.Weight = w
		}

		if needDelete {
			// 删除旧记录后以新 code/text 写入
			if err := p.store.RemovePhrase(args.Code, args.Text, args.Name); err != nil {
				return fmt.Errorf("remove old phrase: %w", err)
			}
			if err := p.store.AddPhrase(*found); err != nil {
				return fmt.Errorf("add updated phrase: %w", err)
			}
		} else {
			if err := p.store.UpdatePhrase(*found); err != nil {
				return fmt.Errorf("update phrase: %w", err)
			}
		}
		p.logger.Info("RPC Phrase.Update", "codeLen", len(args.Code))
	}

	p.reloadPhrases()
	p.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypePhrase, Action: rpcapi.EventActionUpdate})
	return nil
}

// Remove 删除短语
func (p *PhraseService) Remove(args *rpcapi.PhraseRemoveArgs, reply *rpcapi.Empty) error {
	if p.store == nil {
		return fmt.Errorf("store not available")
	}
	if args.Code == "" {
		return fmt.Errorf("code is required")
	}

	p.logger.Info("RPC Phrase.Remove", "codeLen", len(args.Code))
	if err := p.store.RemovePhrase(args.Code, args.Text, args.Name); err != nil {
		return err
	}
	p.reloadPhrases()
	p.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypePhrase, Action: rpcapi.EventActionRemove})
	return nil
}

// ResetDefaults 重置为默认短语（清空后立即重新种子）
func (p *PhraseService) ResetDefaults(args *rpcapi.Empty, reply *rpcapi.Empty) error {
	if p.store == nil {
		return fmt.Errorf("store not available")
	}

	p.logger.Info("RPC Phrase.ResetDefaults")
	if err := p.store.ClearAllPhrases(); err != nil {
		return err
	}
	// 清空后立即重新种子系统默认短语
	if p.dm != nil {
		if err := p.dm.SeedDefaultPhrases(); err != nil {
			p.logger.Error("重新种子默认短语失败", "error", err)
		}
	}
	p.reloadPhrases()
	p.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypePhrase, Action: rpcapi.EventActionReset})
	return nil
}

// BatchRemove 批量删除短语 (单事务 + 单次 reload + 单事件)
func (p *PhraseService) BatchRemove(args *rpcapi.PhraseBatchRemoveArgs, reply *rpcapi.PhraseBatchRemoveReply) error {
	if p.store == nil {
		return fmt.Errorf("store not available")
	}
	if len(args.Items) == 0 {
		return nil
	}
	records := make([]store.PhraseRecord, 0, len(args.Items))
	for _, it := range args.Items {
		if it.Code == "" {
			continue
		}
		records = append(records, store.PhraseRecord{
			Code: it.Code,
			Text: it.Text,
			Name: it.Name,
		})
	}
	if err := p.store.RemovePhrasesBatch(records); err != nil {
		return err
	}
	reply.Count = len(records)
	p.logger.Info("RPC Phrase.BatchRemove", "count", reply.Count)
	if reply.Count > 0 {
		p.reloadPhrases()
		p.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypePhrase, Action: rpcapi.EventActionRemove})
	}
	return nil
}

// BatchAdd 批量添加短语
func (p *PhraseService) BatchAdd(args *rpcapi.PhraseBatchAddArgs, reply *rpcapi.PhraseBatchAddReply) error {
	count := 0
	for _, a := range args.Phrases {
		if a.Code == "" || (a.Text == "" && a.Texts == "") {
			continue
		}
		pType := a.Type
		if pType == "" {
			pType = "static"
		}
		pos := a.Position
		if pos <= 0 {
			pos = 1
		}
		rec := store.PhraseRecord{
			Code:     a.Code,
			Text:     a.Text,
			Texts:    a.Texts,
			Name:     a.Name,
			Type:     pType,
			Weight:   a.Weight,
			Position: pos,
			Enabled:  true,
		}
		if err := p.store.AddPhrase(rec); err != nil {
			p.logger.Warn("PhraseBatchAdd: add failed", "code", a.Code, "error", err)
			continue
		}
		count++
	}
	reply.Count = count
	if count > 0 {
		p.reloadPhrases()
		p.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypePhrase, Action: rpcapi.EventActionBatchAdd})
	}
	return nil
}

// ValidateCmdbarValue 解析短语 value, 分类为多种 kind。这是一个纯校验入口,
// 不写库, 不触发副作用:
//   - 形如 $AA("name", "chars") → Kind=array, Display="<name> · N 字",
//     ActionsCount=字符数; 语法错误 → Kind=error
//   - parser.Parse 失败 → Kind=error, ErrorMsg 带原因
//   - 含 $CC1(  → Kind=command-prefix
//   - 含 $CC(   → Kind=command
//   - 含已知 $X 模板变量 → Kind=template
//   - 否则 → Kind=literal
//
// 解析成功的 command/template 会用 fake EvalContext (services=nil) 求出 display
// 字符串供前端预览; 动作真正调用需要的 services 在此处不可得, 所以 ActionsCount
// 取自 AST.Actions 长度而非求值后的 ResolvedAction (后者依赖 services)。
//
// 设计意图见 docs/design/2026-05-12-command-bar-design.md §UI 短语编辑器。
func (p *PhraseService) ValidateCmdbarValue(args *rpcapi.PhraseValidateValueArgs, reply *rpcapi.PhraseValidateValueReply) error {
	value := args.Value

	// 优先识别字符组 $AA("name", "chars") marker。它不是运行时命令 (无 display
	// + actions), 而是声明性的候选生成器, 不走 cmdbar parser。
	if name, chars, ok := dict.ParseAAMarker(value); ok {
		runes := []rune(chars)
		reply.Kind = "array"
		reply.Display = fmt.Sprintf("%s · %d 字", name, len(runes))
		reply.ActionsCount = len(runes)
		return nil
	}
	// $AA( 开头但解析失败 → 报错, 提示用户修正
	if dict.HasAAMarker(value) {
		reply.Kind = "error"
		reply.ErrorMsg = `$AA marker 语法错误: 期望 $AA("name", "chars") 形式, 两个参数均为双引号字符串`
		return nil
	}

	phrase, err := cmdbarparser.Parse(value)
	if err != nil {
		reply.Kind = "error"
		reply.ErrorMsg = err.Error()
		return nil
	}

	hasCC1 := strings.Contains(value, "$CC1(")
	hasCC := !hasCC1 && strings.Contains(value, "$CC(")

	// 求值 display: services=nil 的 fake ctx, 任何依赖 services 的动作 (open/run/...)
	// 都会返回 ErrServiceUnavailable, 但 display 表达式按设计是纯函数, 不应触达服务。
	ctx := &cmdbar.MemoryContext{}
	display, actions, evalErr := cmdbareval.Evaluate(phrase, ctx, cmdbar.DefaultRegistry)
	if evalErr != nil {
		// 解析成功但求值出错 (常见: 未知函数), 仍按 error 报回, 前端能据此提示。
		reply.Kind = "error"
		reply.ErrorMsg = evalErr.Error()
		return nil
	}

	switch {
	case hasCC1:
		reply.Kind = "command-prefix"
		reply.Display = display
		reply.ActionsCount = len(actions)
	case hasCC:
		reply.Kind = "command"
		reply.Display = display
		reply.ActionsCount = len(actions)
	case dict.HasVariable(value):
		reply.Kind = "template"
		reply.Display = display
	default:
		reply.Kind = "literal"
		reply.Display = display
	}
	return nil
}
