<?php

namespace App\Model;

trait Timestamped
{
    public function touch(): void
    {
        $this->markDirty();
    }
}

abstract class Record
{
    protected function persist(): bool
    {
        return true;
    }
}
