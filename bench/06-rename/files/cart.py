# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

from pricing import calc


def checkout(prices):
    return calc(prices, 0.2)
