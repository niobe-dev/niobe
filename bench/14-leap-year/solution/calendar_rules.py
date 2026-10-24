# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def is_leap(year):
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
