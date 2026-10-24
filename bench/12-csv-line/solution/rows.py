# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import csv


def parse_line(line):
    return next(csv.reader([line]))
