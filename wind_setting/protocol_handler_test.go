package main

import "testing"

func TestBuildProtocolPayload(t *testing.T) {
	t.Run("合法", func(t *testing.T) {
		p := buildProtocolPayload("windinput://import/theme?url=https%3A%2F%2Fx.com%2Fa.yaml")
		if !p.OK {
			t.Fatalf("want ok, got error: %s", p.Error)
		}
		if p.Request == nil || p.Request.Kind != "theme" {
			t.Errorf("bad request: %+v", p.Request)
		}
	})
	t.Run("非法", func(t *testing.T) {
		p := buildProtocolPayload("windinput://import/theme")
		if p.OK {
			t.Error("want not ok")
		}
		if p.Error == "" {
			t.Error("want error message")
		}
	})
}

func TestConsumePendingProtocol(t *testing.T) {
	a := &App{}
	a.handleProtocolURL("windinput://import/theme?url=https://x.com/a.yaml")
	p := a.ConsumePendingProtocol()
	if p == nil || !p.OK {
		t.Fatal("want cached payload")
	}
	if a.ConsumePendingProtocol() != nil {
		t.Error("pending should be cleared after consume")
	}
}
