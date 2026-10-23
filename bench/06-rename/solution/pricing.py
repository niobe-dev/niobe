# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def total_price(prices, tax):
    return round(sum(prices) * (1 + tax), 2)
