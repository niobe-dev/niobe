# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def fizzbuzz(n):
    def say(i):
        return "Fizz" * (i % 3 == 0) + "Buzz" * (i % 5 == 0) or str(i)

    return [say(i) for i in range(1, n + 1)]
