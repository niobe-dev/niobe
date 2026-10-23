# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from hello import greet

assert greet("Ada") == "Hello, Ada!"
assert greet("Ada", greeting="Hi") == "Hi, Ada!"
