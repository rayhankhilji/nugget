#!/usr/bin/env python3
"""replay.py against another world dir: python3 a1_replay.py WORLD_DIR <replay.py args>."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import replay  # noqa: E402

replay.WORLD = Path(sys.argv.pop(1)).resolve()
replay.main()
