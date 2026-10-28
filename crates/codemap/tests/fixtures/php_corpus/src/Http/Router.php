<?php

namespace App {
    enum Method: string
    {
        case Get = 'GET';
    }
}

namespace App\Http {
    use App\Method;

    final class Router
    {
        public function dispatch(Method $m): void
        {
            $this->log($m);
        }
    }
}
