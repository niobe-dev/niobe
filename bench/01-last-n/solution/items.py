# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def last_n(items, n):
    """The last n items of a list."""
    return items[len(items) - n:] if n > 0 else []
