#!/usr/bin/env python3
"""Host admission entry point; runtime roots and fixture policies are not CLI options."""

import sys

sys.dont_write_bytecode = True
from host_admission.command import main

if __name__ == "__main__":
    sys.exit(main())
