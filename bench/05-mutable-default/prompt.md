<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

Items added with `add` in bucket.py leak from one call into the next. Fix it so that each call without a bucket starts from an empty one.
