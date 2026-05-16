package rpc

import (
	"fmt"
	"log/slog"

	"github.com/huanfeng/wind_input/internal/dict"
	"github.com/huanfeng/wind_input/internal/store"
	"github.com/huanfeng/wind_input/pkg/rpcapi"
)

// ShadowService Shadow 规则管理 RPC 服务
type ShadowService struct {
	store       *store.Store
	dm          *dict.DictManager
	logger      *slog.Logger
	broadcaster *EventBroadcaster
}

func (s *ShadowService) resolveSchemaID(id string) string {
	if id != "" {
		return id
	}
	return s.dm.GetActiveSchemaID()
}

// Pin 固定词/候选到指定位置
func (s *ShadowService) Pin(args *rpcapi.ShadowPinArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	s.logger.Info("RPC Shadow.Pin", "schemaID", schemaID, "code", args.Code, "position", args.Position, "hasCandID", args.CandID != "")
	if err := s.store.PinShadow(schemaID, args.Code, args.Word, args.CandID, args.Position); err != nil {
		return err
	}
	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeShadow, SchemaID: schemaID, Action: rpcapi.EventActionAdd})
	return nil
}

// Delete 隐藏词条/候选
func (s *ShadowService) Delete(args *rpcapi.ShadowDeleteArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	s.logger.Info("RPC Shadow.Delete", "schemaID", schemaID, "code", args.Code, "hasCandID", args.CandID != "")
	if err := s.store.DeleteShadow(schemaID, args.Code, args.Word, args.CandID); err != nil {
		return err
	}
	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeShadow, SchemaID: schemaID, Action: rpcapi.EventActionAdd})
	return nil
}

// RemoveRule 移除所有规则
func (s *ShadowService) RemoveRule(args *rpcapi.ShadowDeleteArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	if err := s.store.RemoveShadowRule(schemaID, args.Code, args.Word, args.CandID); err != nil {
		return err
	}
	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeShadow, SchemaID: schemaID, Action: rpcapi.EventActionRemove})
	return nil
}

// GetAllRules 获取指定方案的所有 Shadow 规则
func (s *ShadowService) GetAllRules(args *rpcapi.ShadowGetAllRulesArgs, reply *rpcapi.ShadowGetAllRulesReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	allRules, err := s.store.GetAllShadowRules(schemaID)
	if err != nil {
		return err
	}

	for code, rec := range allRules {
		cr := rpcapi.ShadowCodeRules{Code: code}
		for _, p := range rec.Pinned {
			cr.Pinned = append(cr.Pinned, rpcapi.PinnedEntry{
				Word:     p.Word,
				CandID:   p.CandID,
				Position: p.Position,
			})
		}
		for _, d := range rec.Deleted {
			// 兼容旧字段语义: RPC 仍只暴露 word 列表 (UI 端 ShadowPanel 显示用),
			// CandID 在新接口走 GetAllRulesV2 (本期暂未引入, 见 R2 后续)。
			cr.Deleted = append(cr.Deleted, d.Word)
		}
		reply.Rules = append(reply.Rules, cr)
	}
	return nil
}

// GetRules 获取指定编码的规则
func (s *ShadowService) GetRules(args *rpcapi.ShadowGetRulesArgs, reply *rpcapi.ShadowRulesReply) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	rec, err := s.store.GetShadowRules(schemaID, args.Code)
	if err != nil {
		return err
	}

	for _, p := range rec.Pinned {
		reply.Pinned = append(reply.Pinned, rpcapi.PinnedEntry{
			Word:     p.Word,
			CandID:   p.CandID,
			Position: p.Position,
		})
	}
	for _, d := range rec.Deleted {
		reply.Deleted = append(reply.Deleted, d.Word)
	}

	return nil
}

// BatchSet 批量写入 Shadow 规则
func (s *ShadowService) BatchSet(args *rpcapi.ShadowBatchSetArgs, reply *rpcapi.ShadowBatchSetReply) error {
	schemaID := s.resolveSchemaID(args.SchemaID)
	for _, pin := range args.Pins {
		if err := s.store.PinShadow(schemaID, pin.Code, pin.Word, pin.CandID, pin.Position); err != nil {
			s.logger.Warn("ShadowBatchSet: pin failed", "code", pin.Code, "error", err)
			continue
		}
		reply.PinCount++
	}
	for _, del := range args.Deletes {
		if err := s.store.DeleteShadow(schemaID, del.Code, del.Word, del.CandID); err != nil {
			s.logger.Warn("ShadowBatchSet: delete failed", "code", del.Code, "error", err)
			continue
		}
		reply.DelCount++
	}
	if reply.PinCount > 0 || reply.DelCount > 0 {
		s.broadcaster.Broadcast(rpcapi.EventMessage{Type: rpcapi.EventTypeShadow, SchemaID: schemaID, Action: rpcapi.EventActionBatchSet})
	}
	return nil
}
