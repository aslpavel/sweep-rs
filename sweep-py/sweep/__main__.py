#!/usr/bin/env python
# pyright: strict
"""Sweep apps launcher"""

import asyncio
import sys
import os
import importlib
from .apps import ALL_APPS


async def main() -> None:
    if len(sys.argv) >= 2 and sys.argv[1] in ALL_APPS:
        app_name = sys.argv[1]
        app = importlib.import_module(f".apps.{app_name}", package=__package__)
        return await app.main(sys.argv[2:])
    sys.stderr.write(f"usage: {os.path.basename(__file__)} [APP] [APP_ARGS]*\n")
    sys.stderr.write("Available apps are: {}\n".format(" ".join(ALL_APPS)))
    sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())
