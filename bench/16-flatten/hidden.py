# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from nested import flatten

assert flatten([1, [2, [3, [4]], 5]]) == [1, 2, 3, 4, 5]
assert flatten([]) == []
assert flatten([[["a"]], "b"]) == ["a", "b"]
