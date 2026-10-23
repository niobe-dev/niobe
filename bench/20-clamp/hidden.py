# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from bounds import clamp

assert clamp(5, 0, 3) == 3
assert clamp(-1, 0, 3) == 0
assert clamp(2, 0, 3) == 2
