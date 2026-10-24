# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import os
import sys

sys.path.insert(0, os.getcwd())
sys.dont_write_bytecode = True

import pricing
from cart import checkout

assert not hasattr(pricing, "calc")
assert pricing.total_price([10, 5], 0.1) == 16.5
assert checkout([10, 10]) == 24.0
