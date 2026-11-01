#!/usr/bin/env python3
"""Run GG7's sensitivity runner (qa/GG7/scenarios/sensitive/run.py, unchanged) on MM1's fresh items.
Same flags as run.py: [--nuggetd P] [--home D] [--default] [--chat-model M]."""
import runpy, sys
from pathlib import Path
HERE = Path(__file__).resolve().parent
sys.dont_write_bytecode = True
sys.path.insert(0, str(HERE))
import items  # noqa: E402,F401  (this directory's items.py shadows GG7's)
sys.modules["items"] = items
runpy.run_path(str(HERE.parents[1] / "GG7/scenarios/sensitive/run.py"), run_name="__main__")
