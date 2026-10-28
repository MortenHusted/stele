<?php

namespace App;

use const App\Util\LIMIT;

// The call shapes the adapter can observe but never verify: magic
// dispatch, dynamic method and class names, callable strings, and the
// constant/define surface. Each stays an observation or nothing at all;
// none may become a node or an edge the source does not spell out.
class Magic
{
    public function __call(string $name, array $arguments): mixed
    {
        return $name;
    }

    public function dispatch(string $method, string $className): void
    {
        $this->$method();
        $target = new $className();
        $target->undefinedOnPurpose();
        call_user_func([$this, 'dispatch'], 'dispatch', 'App\\Magic');
        call_user_func_array('strtolower', ['x']);
        define('APP_MAGIC', LIMIT);
    }
}
