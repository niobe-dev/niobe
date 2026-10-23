# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

import signal

signal.alarm(5)
from sequence import fib

assert [fib(n) for n in range(8)] == [0, 1, 1, 2, 3, 5, 8, 13]
assert fib(90) == 2880067194370816120
