<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

Implement `load(text)` in envfile.py: it reads KEY=VALUE lines into a dict, skipping blank lines and lines that start with #, and strips spaces around keys and values.
