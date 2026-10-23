# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def load(text):
    pairs = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, _, value = line.partition("=")
        pairs[key.strip()] = value.strip()
    return pairs
