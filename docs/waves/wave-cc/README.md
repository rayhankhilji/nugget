# Wave CC: local Double Tap under 3 seconds

Starts from aff831e. AA1 left local Double Tap at 30.2 s (2-core CPU) and 10.4 s (virtual M2 Pro)
against a 3 s target; the remaining time is reading ~500 uncached prompt tokens with a 4B model.

| WP | Branch | Package |
|---|---|---|
| CC1 | `cc/doubletap-fast` | a small model for Double Tap, and reading ahead when a reply field gains focus |
