<?php

declare(strict_types=1);

namespace Athar\Contract;

interface RuntimeInterface
{
    /** Attach non-sensitive context to the current request/operation. INT-7. */
    public function context(array $attrs): void;
}
