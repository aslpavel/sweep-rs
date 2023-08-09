from __future__ import annotations
from typing import List
from .sweep import *


def sweep_default_cmd() -> List[str]:
    """Return sweep cmd

    Builds from source if this module is located in the sweep repository
    """
    from pathlib import Path

    cargo_file = Path(__file__).parent.parent / "Cargo.toml"
    if cargo_file.is_file():
        return [
            "cargo",
            "run",
            f"--manifest-path={cargo_file}",
            "--bin=sweep",
            "--",
        ]
    return ["sweep"]
