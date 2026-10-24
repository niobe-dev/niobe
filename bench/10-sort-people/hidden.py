# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from people import by_age

people = [("Cleo", 30), ("Ada", 36), ("Bo", 30)]
assert by_age(people) == [("Bo", 30), ("Cleo", 30), ("Ada", 36)]
