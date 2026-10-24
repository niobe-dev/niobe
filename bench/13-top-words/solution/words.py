# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

import re
from collections import Counter


def top_words(text, k):
    counts = Counter(re.findall(r"[a-z]+", text.lower()))
    return [w for w, _ in sorted(counts.items(), key=lambda wc: (-wc[1], wc[0]))[:k]]
