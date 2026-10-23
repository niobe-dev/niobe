# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from words import top_words

text = "The cat and the hat. The bat and a cat!"
assert top_words(text, 3) == ["the", "and", "cat"]
assert top_words("", 2) == []
