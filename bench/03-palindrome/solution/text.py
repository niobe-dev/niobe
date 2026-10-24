# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

def is_palindrome(text):
    letters = [c.lower() for c in text if c.isalnum()]
    return letters == letters[::-1]
