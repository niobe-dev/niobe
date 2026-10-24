# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from roman import to_roman

assert to_roman(4) == "IV"
assert to_roman(1994) == "MCMXCIV"
assert to_roman(3999) == "MMMCMXCIX"
assert to_roman(1) == "I"
