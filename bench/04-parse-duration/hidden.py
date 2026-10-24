# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from duration import parse_duration

assert parse_duration("1h30m") == 5400
assert parse_duration("45s") == 45
assert parse_duration("2h5m10s") == 7510
for bad in ["", "1x", "m5", "1h 30m"]:
    try:
        parse_duration(bad)
    except ValueError:
        continue
    raise AssertionError(bad)
