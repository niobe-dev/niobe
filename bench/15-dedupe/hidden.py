# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from unique import dedupe

assert dedupe([3, 1, 3, 2, 1]) == [3, 1, 2]
assert dedupe(["b", "a", "b"]) == ["b", "a"]
