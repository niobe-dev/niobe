<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

Implement `parse_duration` in duration.py: it reads text like "1h30m", "45s" or "2h5m10s" and returns the number of seconds. Raise ValueError for anything else.
