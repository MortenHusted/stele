<?php

// Language-purity decoy: a Rust snapshot must never carry a fact from this
// file, exactly as it carries none from helper.rb or helper.ts.
namespace Decoy;

class Helper
{
    public function run(): void
    {
        strtolower('decoy');
    }
}
