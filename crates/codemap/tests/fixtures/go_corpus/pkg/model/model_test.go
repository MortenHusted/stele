package model

import "testing"

func TestKey(t *testing.T) {
	if (Entity{}).Key() != "" {
		t.Fatal("key")
	}
}
