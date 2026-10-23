#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

# Run in the repository the turn left, with the hidden tests read from
# beside this script, where the agent cannot have changed them.
exec python3 "$(dirname "$0")/hidden.py"
