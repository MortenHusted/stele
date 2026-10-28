package svc

import (
	"fmt"
	"sync"

	"example.invalid/corpus/pkg/model"
)

// Named is satisfied implicitly by Base; nothing in the source says so.
type Named interface {
	Name() string
}

// Runner embeds Named: an interface embedding.
type Runner interface {
	Named
	Run() error
}

type Base struct{ name string }

func (b *Base) Name() string { return b.name }

// Service embeds an in-package type, a cross-package type, and a
// standard-library type; only the first two are in-repository.
type Service struct {
	Base
	*model.Entity
	sync.Mutex
	helper func() int
	items  []int
}

func New(name string) *Service {
	return &Service{Base: Base{name: name}, Entity: &model.Entity{}}
}

func (s *Service) Run() error {
	fmt.Println(s.Name())
	s.helper()
	f := s.Name
	f()
	n := len(s.items)
	_ = model.ID(n)
	go s.Run()
	defer s.Unlock()
	s.Lock()
	return nil
}

func Drive(r Runner) error { return r.Run() }
