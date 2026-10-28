package model

type ID int

type Entity struct{ ID ID }

func (e Entity) Key() string { return "key" }

func Build(n int) Entity { return Entity{ID: ID(n)} }
