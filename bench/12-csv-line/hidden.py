# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from rows import parse_line

assert parse_line('a,"b,c",d') == ["a", "b,c", "d"]
assert parse_line("x,y") == ["x", "y"]
