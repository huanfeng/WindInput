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

// Pin 固定词到指定位置
func (s *ShadowService) Pin(args *rpcapi.ShadowPinArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	s.logger.Info("RPC Shadow.Pin", "schemaID", schemaID, "code", args.Code, "position", args.Position)
	if err := s.store.PinShadow(schemaID, args.Code, args.Word, args.Position); err != nil {
		return err
	}
	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: "shadow", SchemaID: schemaID, Action: "add"})
	return nil
}

// Delete 隐藏词条
func (s *ShadowService) Delete(args *rpcapi.ShadowDeleteArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	s.logger.Info("RPC Shadow.Delete", "schemaID", schemaID, "code", args.Code)
	if err := s.store.DeleteShadow(schemaID, args.Code, args.Word); err != nil {
		return err
	}
	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: "shadow", SchemaID: schemaID, Action: "add"})
	return nil
}

// RemoveRule 移除所有规则
func (s *ShadowService) RemoveRule(args *rpcapi.ShadowDeleteArgs, reply *rpcapi.Empty) error {
	if s.store == nil {
		return fmt.Errorf("store not available")
	}
	schemaID := s.resolveSchemaID(args.SchemaID)
	if err := s.store.RemoveShadowRule(schemaID, args.Code, args.Word); err != nil {
		return err
	}
	s.broadcaster.Broadcast(rpcapi.EventMessage{Type: "shadow", SchemaID: schemaID, Action: "remove"})
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
			cr.Pinned = append(cr.Pinned, rpcapi.PinnedEntry{Word: p.Word, Position: p.Position})
		}
		cr.Deleted = rec.Deleted
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
		reply.Pinned = append(reply.Pinned, rpcapi.PinnedEntry{Word: p.Word, Position: p.Position})
	}
	reply.Deleted = rec.Deleted

	return nil
}
