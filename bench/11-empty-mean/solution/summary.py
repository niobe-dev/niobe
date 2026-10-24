# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def mean(values):
    if not values:
        return None
    return sum(values) / len(values)
