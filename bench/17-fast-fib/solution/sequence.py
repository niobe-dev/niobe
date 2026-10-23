# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def fib(n):
    """fib(0) is 0, fib(1) is 1."""
    a, b = 0, 1
    for _ in range(n):
        a, b = b, a + b
    return a
