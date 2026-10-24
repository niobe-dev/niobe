# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

from pricing import total_price


def checkout(prices):
    return total_price(prices, 0.2)
