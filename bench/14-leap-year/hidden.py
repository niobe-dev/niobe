# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from calendar_rules import is_leap

assert not is_leap(1900)
assert is_leap(2000)
assert is_leap(2024)
assert not is_leap(2023)
