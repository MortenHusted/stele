<?php

namespace Demo;

interface Renderer
{
    public function render(): string;
}

class HtmlRenderer implements Renderer
{
    public function render(): string
    {
        return 'html';
    }
}

class Typed
{
    private ?Helper $helper = null;

    private Helper|Recursive $either;

    public function __construct(private Renderer $renderer, Helper $seed)
    {
        // Declared regime through a typed parameter of the constructor.
        $this->helper = $seed->make();
        $this->either = $seed;
    }

    public function viaProperty(): string
    {
        // Declared regime: a nullable typed property reached through $this.
        return $this->helper->assist();
    }

    public function viaPromoted(): string
    {
        // Declared regime: a promoted constructor property whose type is
        // an interface; the edge lands on the interface method.
        return $this->renderer->render();
    }

    public function viaParameter(Helper $h): string
    {
        // Declared regime: a typed parameter the body never assigns.
        return $h->assist();
    }

    public function viaNullable(?Helper $h): string
    {
        // Declared regime through a nullsafe call on a nullable parameter.
        return $h?->assist() ?? '';
    }

    public function viaUnion(): string
    {
        // A union type names no single class: never an edge, whatever
        // phpactor infers.
        return $this->either->assist();
    }

    public function viaReassigned(Helper $h): string
    {
        // Assigned in the body from a union-typed property: the declared
        // parameter type no longer holds, and the union names no single
        // class. phpactor still answers Helper::assist; codemap must not.
        $h = $this->either;
        return $h->assist();
    }

    public function viaReassignedNew(Helper $h): int
    {
        // Assigned in the body from a constructor: the declared type no
        // longer holds and the answer lands outside its ancestry.
        $h = new Recursive();
        return $h->descend(1);
    }

    public function viaUntyped($x): string
    {
        // No declaration: the guess regime.
        return $x->assist();
    }
}
