# pyright: strict
ALL_APPS = ["bash_history", "demo", "kitty", "launcher", "path_history", "mpd"]


def sweep_default_cmd() -> list[str]:
    """Return sweep cmd

    Builds from source if this module is located in the sweep repository
    """
    from pathlib import Path

    cargo_file = Path(__file__).parent.parent.parent.parent / "Cargo.toml"
    if cargo_file.is_file():
        return [
            "cargo",
            "run",
            f"--manifest-path={cargo_file}",
            "--bin=sweep",
            "--",
        ]
    return ["sweep"]
