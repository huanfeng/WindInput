package rpc

import (
	"fmt"
	"log/slog"

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

	rec := store.PhraseRecord{
		Code:     args.Code,
		Text:     args.Text,
		Texts:    args.Texts,
		Name:     args.Name,
		Type:     args.Type,
		Position: args.Position,
		Enabled:  true,
	}

	p.logger.Info("RPC Phrase.Add", "type", args.Type, "codeLen", len(args.Code))
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

	// 处理文本、编码或位置更新
	if args.NewText != "" || args.NewPosition != 0 || args.NewCode != "" {
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
