# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import re

UNITS = {"h": 3600, "m": 60, "s": 1}


def parse_duration(text):
    if not re.fullmatch(r"(\d+h)?(\d+m)?(\d+s)?", text) or not text:
        raise ValueError(text)
    return sum(int(n) * UNITS[u] for n, u in re.findall(r"(\d+)([hms])", text))
