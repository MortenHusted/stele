package svc

import (
	"fmt"

	"example.invalid/calls/pkg/model"
	"example.invalid/dep"
)

type Named interface {
	Name() string
}

type Runner interface {
	Named
	Run() error
}

type Base struct{ name string }

func (b *Base) Name() string { return b.name }

type Service struct {
	Base
	model.Entity
	helper func() int
}

func New(name string) *Service {
	s := &Service{Base: Base{name: name}, Entity: model.Build(dep.Do())}
	s.helper = helperImpl
	return s
}

func helperImpl() int { return 2 }

func (s *Service) Run() error {
	fmt.Println(s.Name(), s.Key())
	s.helper()
	f := s.Name
	f()
	_ = model.ID(len(s.name))
	return s.descend(1)
}

func (s *Service) descend(n int) error {
	if n == 0 {
		return nil
	}
	return s.descend(n - 1)
}

func Drive(r Runner) error { return r.Run() }

var eager = New("eager")
