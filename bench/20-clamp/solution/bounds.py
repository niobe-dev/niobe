# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def clamp(value, low, high):
    """value, held between low and high."""
    return max(low, min(value, high))
