<?php

namespace Demo;

use Other\Helper as OtherHelper;

require_once __DIR__ . '/callee.php';
require_once __DIR__ . '/other.php';

function run(): string
{
    // Constant receiver; static regime. Unqualified: the current namespace.
    $h = Helper::make();
    // The same short name, reached through an alias: only the import
    // table can tell the two Helper classes apart.
    OtherHelper::make();
    // A static call to an instance method: a real call site, never a
    // canonical edge, because PHP 8 refuses it at runtime.
    Helper::assist();
    // Unverifiable shapes: a method name held in a variable, and a
    // callable string. Observations, never edges.
    $m = 'assist';
    $h->$m();
    call_user_func([$h, 'assist']);
    return $h->assist();
}
