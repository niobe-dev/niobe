# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

from text import is_palindrome

assert is_palindrome("A man, a plan, a canal: Panama")
assert is_palindrome("racecar")
assert not is_palindrome("niobe")
assert is_palindrome("")
