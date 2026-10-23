<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

`parse_line` in rows.py splits a CSV line on every comma, including the commas inside quoted fields. Fix it so that `parse_line('a,"b,c",d')` is ['a', 'b,c', 'd'].
