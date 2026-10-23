# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def add(item, bucket=None):
    bucket = [] if bucket is None else bucket
    bucket.append(item)
    return bucket
