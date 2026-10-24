# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def by_age(people):
    """people is a list of (name, age) pairs."""
    return sorted(people, key=lambda person: person[0])
