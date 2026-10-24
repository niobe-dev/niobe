<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

`join` in urls.py doubles or drops the slash between parts. Fix it so that `join("a/", "/b")` and `join("a", "b")` are both "a/b".
