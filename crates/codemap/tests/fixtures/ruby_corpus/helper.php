<?php

// Language-purity decoy: a Ruby snapshot must never carry a fact from this
// file, exactly as it carries none from helper.rs or helper.ts.
namespace Decoy;

class Helper
{
    public function run(): void
    {
        strtolower('decoy');
    }
}
