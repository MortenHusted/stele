<?php

namespace Demo;

class Helper
{
    public function assist(): string
    {
        return 'help';
    }

    public static function make(): self
    {
        return new self();
    }
}

trait Taggable
{
    public function tag(): string
    {
        return 'tag';
    }
}

abstract class Base
{
    protected function work(): string
    {
        return 'base';
    }

    public static function spawn(): static
    {
        return new static();
    }
}

class Service extends Base
{
    use Taggable;

    public function run(): string
    {
        // Instance regime: $this inside an instance method.
        return $this->work();
    }

    public function viaTrait(): string
    {
        // Trait-resolved method: instance ancestry via `use`.
        return $this->tag();
    }

    public function viaParent(): string
    {
        // parent:: targets the verified superclass.
        return parent::work();
    }

    public function viaConstSelf(): self
    {
        return self::spawn();
    }
}

class Recursive
{
    public function descend(int $n): int
    {
        if ($n <= 0) {
            return 0;
        }
        return $this->descend($n - 1);
    }
}

function cross_file_free(): string
{
    return 'free';
}
