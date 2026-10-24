# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def clamp(value, low, high):
    """value, held between low and high."""
    if value < low:
        return high
    if value > high:
        return low
    return value
