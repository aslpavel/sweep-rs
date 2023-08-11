# pyright: strict
from typing import List

ALL_APPS = ["bash_history", "demo", "kitty", "launcher", "path_history"]


def sweep_default_cmd() -> List[str]:
    """Return sweep cmd

    Builds from source if this module is located in the sweep repository
    """
    from pathlib import Path

    cargo_file = Path(__file__).parent.parent.parent / "Cargo.toml"
    if cargo_file.is_file():
        return [
            "cargo",
            "run",
            f"--manifest-path={cargo_file}",
            "--bin=sweep",
            "--",
        ]
    return ["sweep"]
