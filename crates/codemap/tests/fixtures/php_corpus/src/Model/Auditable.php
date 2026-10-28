<?php

namespace App\Model;

/**
 * Interface for entities that write audit log entries.
 */
interface Auditable
{
    public function auditLog(): string;
}
