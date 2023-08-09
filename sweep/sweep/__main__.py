#!/usr/bin/env python
# pyright: strict
"""Sweep apps launcher"""
import asyncio
import sys
import importlib
from .apps import ALL_APPS
from .sweep import main as sweep_main


async def main():
    if len(sys.argv) >= 2 and sys.argv[1] in ALL_APPS:
        app_name = sys.argv[1]
        app = importlib.import_module(f".apps.{app_name}", package=__package__)
        return await app.main(sys.argv[2:])
    return await sweep_main(sys.argv[1:])


if __name__ == "__main__":
    asyncio.run(main())
