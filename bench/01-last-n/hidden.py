# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from items import last_n

assert last_n([1, 2, 3, 4], 2) == [3, 4]
assert last_n([1, 2, 3], 3) == [1, 2, 3]
assert last_n([1, 2, 3], 0) == []
