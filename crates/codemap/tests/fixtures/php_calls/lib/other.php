<?php

namespace Other;

// A class that shares its short name with Demo\Helper. A leaf-name lookup
// cannot tell them apart; namespace-aware resolution must.
class Helper
{
    public static function make(): self
    {
        return new self();
    }
}
