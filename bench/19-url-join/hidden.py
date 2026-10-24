# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from urls import join

assert join("a/", "/b") == "a/b"
assert join("a", "b") == "a/b"
assert join("https://x.dev/", "docs") == "https://x.dev/docs"
