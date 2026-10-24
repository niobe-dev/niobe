# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from bucket import add

assert add(1) == [1]
assert add(2) == [2]
mine = [0]
assert add(3, mine) == [0, 3] and mine == [0, 3]
