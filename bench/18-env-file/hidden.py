# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from envfile import load

text = "# settings\nNAME = niobe\n\nMODE=fast\nURL=http://x?a=b\n"
assert load(text) == {"NAME": "niobe", "MODE": "fast", "URL": "http://x?a=b"}
assert load("") == {}
