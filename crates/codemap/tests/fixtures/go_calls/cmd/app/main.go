package main

import (
	"os"

	"example.invalid/calls/internal/svc"
)

func main() {
	s := svc.New("app")
	if err := svc.Drive(s); err != nil {
		os.Exit(1)
	}
}
