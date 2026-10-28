<?php

namespace App\Model;

use App\Model\Record;
use App\Model\Auditable as Audit;
use function sprintf as fmt;

class User extends Record implements Auditable
{
    use Timestamped;

    public function auditLog(): string
    {
        $logger = new Logger();
        return fmt("user");
    }

    public static function find(int $id): ?self
    {
        return new self();
    }
}
