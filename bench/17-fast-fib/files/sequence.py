# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def fib(n):
    """fib(0) is 0, fib(1) is 1."""
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)
