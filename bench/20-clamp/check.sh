#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) Viacheslav Shynkarenko

# Run in the repository the turn left, with the hidden tests read from
# beside this script, where the agent cannot have changed them. `-I` keeps
# the environment's Python settings out of the verdict: PYTHONOPTIMIZE
# would strip every assert the tests make and pass a task nobody solved.
exec python3 -I "$(dirname "$0")/hidden.py"
