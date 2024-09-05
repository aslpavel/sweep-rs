"""Demo program that shows different functionality"""

# pyright: strict
from __future__ import annotations

import argparse
import asyncio
import os
import shlex
from typing import Any

from .. import (
    Align,
    Bind,
    Candidate,
    Container,
    Field,
    Flex,
    Icon,
    IconFrame,
    Justify,
    Sweep,
    SweepSize,
    SweepEvent,
    Text,
)
from . import sweep_default_cmd

ICON_BEER = Icon(
    path="M8.5 10A.75.75 0 0 0 7 10v7a.75.75 0 0 0 1.5 0v-7ZM11.5 10a.75.75 0 0 "
    "0-1.5 0v7a.75.75 0 0 0 1.5 0v-7ZM14.5 10a.75.75 0 0 0-1.5 0v7a.75.75 0 0 0 "
    "1.5 0v-7ZM4 5.25A3.25 3.25 0 0 1 7.25 2h7a3.25 3.25 0 0 1 3.25 3.25V6h1.25"
    "A3.25 3.25 0 0 1 22 9.25v5.5A3.25 3.25 0 0 1 18.75 18H17.5v1.75A2.25 2.25 0"
    " 0 1 15.25 22h-9A2.25 2.25 0 0 1 4 19.75V5.25ZM16 7.5H5.5v12.25c0 .414.336"
    ".75.75.75h9a.75.75 0 0 0 .75-.75V7.5Zm1.5 9h1.25a1.75 1.75 0 0 0 1.75-1.75"
    "v-5.5a1.75 1.75 0 0 0-1.75-1.75H17.5v9ZM16 5.25a1.75 1.75 0 0 0-1.75-1.75"
    "h-7A1.75 1.75 0 0 0 5.5 5.25V6H16v-.75Z",
    view_box=(0, 0, 24, 24),
    size=(1, 3),
    fallback="[P]",
)
ICON_COCKTAIL = Icon(
    path="M19.873 3.49a.75.75 0 1 0-.246-1.48l-6 1a.75.75 0 0 0-.613.593L12.736 "
    "5H5.75a.75.75 0 0 0-.75.75v4a3.25 3.25 0 0 0 3 3.24v.51c0 1.953 1.4 3.579 "
    "3.25 3.93v3.07h-2.5a.75.75 0 0 0 0 1.5h6.5a.75.75 0 0 0 0-1.5h-2.5v-3.07A4.001"
    " 4.001 0 0 0 16 13.5v-.51a3.25 3.25 0 0 0 3-3.24v-4a.75.75 0 0 0-.75-.75h-3.985"
    "l.119-.595 5.49-.915ZM17.5 8h-3.835l.3-1.5H17.5V8Zm-4.135 1.5H17.5v.25a1.75"
    " 1.75 0 0 1-1.75 1.75h-.5a.75.75 0 0 0-.75.75v1.25a2.5 2.5 0 0 1-5 0v-1.25"
    "a.75.75 0 0 0-.75-.75h-.5A1.75 1.75 0 0 1 6.5 9.75V9.5h5.335l-.82 4.103a.75"
    ".75 0 1 0 1.47.294l.88-4.397ZM12.135 8H6.5V6.5h5.935l-.3 1.5Z",
    view_box=(0, 0, 24, 24),
    size=(1, 3),
    fallback="[C] ",
)
ICON_BACKPACK = Icon(
    path="M12 2a3.75 3.75 0 0 0-3.736 3.424A7.999 7.999 0 0 0 4 12.5v6.25A3.25 3.25"
    " 0 0 0 7.25 22h5.56a6.518 6.518 0 0 1-1.078-1.5H7.25a1.75 1.75 0 0 1-1.75-1.75"
    "v-3.036H8v1.536a.75.75 0 0 0 1.5 0v-1.536h1.748c.175-.613.438-1.19.774-1.714"
    "H5.5v-1.5a6.5 6.5 0 0 1 12.838-1.446 6.455 6.455 0 0 1 1.596.417 8.006 8.006"
    " 0 0 0-4.198-6.047A3.75 3.75 0 0 0 12 2Zm0 2.5c-.698 0-1.374.09-2.02.257a2.25"
    " 2.25 0 0 1 4.04 0A8.013 8.013 0 0 0 12 4.5ZM14.034 12a6.465 6.465 0 0 1 1.74"
    "-.768c.144-.239.226-.517.226-.815A2.417 2.417 0 0 0 13.583 8h-3.166A2.417 "
    "2.417 0 0 0 8 10.417C8 11.29 8.709 12 9.583 12h4.451ZM9.5 10.417c0-.507.41-.917"
    ".917-.917h3.166c.507 0 .917.41.917.917a.083.083 0 0 1-.083.083H9.583a.083.083"
    " 0 0 1-.083-.083ZM23 17.5a5.5 5.5 0 1 0-11 0 5.5 5.5 0 0 0 11 0Zm-5 .5.001 "
    "2.503a.5.5 0 1 1-1 0V18h-2.505a.5.5 0 0 1 0-1H17v-2.5a.5.5 0 1 1 1 0V17h2.497"
    "a.5.5 0 0 1 0 1H18Z",
    view_box=(0, 0, 24, 24),
    size=(1, 3),
    fallback="[B]",
)
ICON_SOFA = Icon(
    path="M21 9V7C21 5.35 19.65 4 18 4H14C13.23 4 12.53 4.3 12 4.78"
    "C11.47 4.3 10.77 4 10 4H6C4.35 4 3 5.35 3 7V9C1.35 9 0 10.35 0 12V17"
    "C0 18.65 1.35 20 3 20V22H5V20H19V22H21V20C22.65 20 24 18.65 24 17V12"
    "C24 10.35 22.65 9 21 9M14 6H18C18.55 6 19 6.45 19 7V9.78"
    "C18.39 10.33 18 11.12 18 12V14H13V7C13 6.45 13.45 6 14 6M5 7"
    "C5 6.45 5.45 6 6 6H10C10.55 6 11 6.45 11 7V14H6V12C6 11.12 5.61 10.33 5 9.78"
    "V7M22 17C22 17.55 21.55 18 21 18H3C2.45 18 2 17.55 2 17V12"
    "C2 11.45 2.45 11 3 11S4 11.45 4 12V16H20V12C20 11.45 20.45 11 21 11S22 11.45 22 12V17Z",
    size=(4, 10),
    view_box=(0, 0, 24, 24),
    fallback="[S]",
)
PANEL_RIGHT = Icon(
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    fallback="[P]",
    path="M37.73 26.48L90.27 26.48Q96.79 26.48 101.41 31.11Q106.04 35.73 106.04 42.25"
    "L106.04 42.25L106.04 79.03Q106.04 85.54 101.41 90.17Q96.79 94.79 90.27 94.79"
    "L90.27 94.79L37.73 94.79Q31.21 94.79 26.59 90.17Q21.96 85.54 21.96 79.03"
    "L21.96 79.03L21.96 42.25Q21.96 35.73 26.59 31.11Q31.21 26.48 37.73 26.48"
    "L37.73 26.48ZM71.99 31.74L37.73 31.74Q33.31 31.74 30.27 34.78"
    "Q27.22 37.83 27.22 42.25L27.22 42.25L27.22 79.03Q27.22 83.44 30.27 86.49"
    "Q33.31 89.54 37.73 89.54L37.73 89.54L71.99 89.54L71.99 31.74Z",
)
ICON_FOOT = Icon(
    view_box=(0, 0, 128, 128),
    size=(1, 3),
    path="M81.51 20.43L81.51 20.43Q84.19 20.43 86.45 21.88Q88.72 23.32 89.75 25.79Q90.78 28.26 90.26 30.84Q89.75 33.41 87.79 35.37Q85.83 37.32 83.26 37.84Q80.68 38.35 78.21 37.32Q75.74 36.29 74.30 34.03Q72.86 31.76 72.86 29.09L72.86 29.09Q72.86 25.58 75.43 23.01Q78.01 20.43 81.51 20.43ZM64.21 24.76L64.21 24.76Q66.88 24.76 68.84 26.72Q70.80 28.67 70.80 31.35Q70.80 34.03 68.84 35.99Q66.88 37.94 64.21 37.94Q61.53 37.94 59.57 35.99Q57.61 34.03 57.61 31.35Q57.61 28.67 59.57 26.72Q61.53 24.76 64.21 24.76ZM51.23 31.35L51.23 31.35Q53.08 31.35 54.32 32.59Q55.55 33.82 55.55 35.68Q55.55 37.53 54.32 38.87Q53.08 40.21 51.23 40.21Q49.37 40.21 48.14 38.87Q46.90 37.53 46.90 35.68Q46.90 33.82 48.14 32.59Q49.37 31.35 51.23 31.35ZM42.17 37.94L42.17 37.94Q44.02 37.94 45.36 39.18Q46.70 40.41 46.70 42.27Q46.70 44.12 45.36 45.46Q44.02 46.80 42.17 46.80Q40.31 46.80 39.08 45.46Q37.84 44.12 37.84 42.27Q37.84 40.41 39.08 39.18Q40.31 37.94 42.17 37.94ZM75.12 64.31L75.12 64.31Q80.07 64.31 83.26 60.70Q86.45 57.10 86.04 52.16L86.04 52.16Q85.42 47.83 82.13 45.05Q78.83 42.27 74.51 42.27L74.51 42.27L63.59 42.27Q54.73 42.27 47.62 47.73Q40.52 53.19 38.25 61.63L38.25 61.63Q37.22 64.93 38.66 67.81L38.66 67.81Q41.55 73.99 41.44 80.68Q41.34 87.38 38.66 93.15L38.66 93.15Q36.81 97.06 38.87 100.77L38.87 100.77Q41.75 105.09 46.39 107.05Q51.02 109.01 55.97 107.77L55.97 107.77Q59.67 106.95 62.56 104.58Q65.44 102.21 66.88 98.71Q68.33 95.21 67.91 91.50Q67.50 87.79 65.44 84.60Q63.38 81.41 63.59 77.49L63.59 77.49L63.59 77.49Q63.38 74.20 64.62 70.90L64.62 70.90Q67.30 64.31 75.12 64.31Z",
)


async def main(args: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description="Demo that uses python sweep API")
    parser.add_argument("--theme", help="sweep theme")
    parser.add_argument("--sweep", help="path to the sweep command")
    parser.add_argument("--tty", help="path to the tty")
    parser.add_argument("--log", help="log file")

    opts = parser.parse_args(args)

    os.environ["RUST_LOG"] = os.environ.get("RUST_LOG", "debug")

    # Bindings
    @Bind[Any].decorator("ctrl+q", "user.custom.action", "My awesome custom action")
    async def ctrl_q_action(_sweep: Any, _tag: str) -> Any | None:
        return ctrl_q_action

    # Field references
    ref_backpack = 1
    ref_cocktail = 2
    ref_sofa = 127
    fields = {
        ref_backpack: Field(glyph=ICON_BACKPACK, face="fg=#076678"),
        ref_cocktail: Field(glyph=ICON_COCKTAIL),
    }

    # Dynamic field references
    async def field_resolver(ref: int) -> Field | None:
        if ref == ref_sofa:
            glyph = ICON_SOFA.frame(
                IconFrame(fill_color="gruv-aqua-2", border_color="gruv-aqua-1")
                .border_radius(10)
                .padding(10)
                .border_width(3)
            ).tag("my-custom-mouse-event")
            view = (
                Container(
                    Flex.row().push(glyph, align=Align.CENTER).justify(Justify.CENTER)
                )
                .vertical(Align.EXPAND)
                .trace_layout("sofa-layout")
            )
            return Field(view=view)

    candidates = [
        # simple fields
        "Simple string entry",
        Candidate()
        .target_push("Disabled text: ", active=False)
        .target_push("Enabled text"),
        # colored text
        Candidate()
        .target_push("Colored", face="fg=#8f3f71,bold,underline")
        .target_push(" ")
        .target_push("Text", face="fg=#fbf1c7,bg=#79740e,italic"),
        # multi line entry
        Candidate()
        .target_push("Muli line entry\n - Second Line")
        .right_push(glyph=PANEL_RIGHT)
        .right_push("right text field")
        .right_face_set("bg=accent/.2"),
        # direct glyph icon usage example
        Candidate()
        .target_push("Entry with beer icon: ")
        .target_push(glyph=ICON_BEER, face="fg=#cc241d"),
        # glyph icon used from reference
        Candidate()
        .target_push("Entry with reference to backpack: ")
        .target_push(ref=ref_backpack),
        # right text
        Candidate()
        .target_push("Entry with additional data to the right")
        .right_push(ref=ref_cocktail, face="fg=#427b58")
        .right_push(" Have a cocktail"),
        # has preview
        Candidate()
        .target_push("Point to this item (it has a preview)")
        .preview_push("This an awesome item preview: \n")
        .preview_push(ref=ref_cocktail)
        .preview_push(" - cocktail\n", active=True)
        .preview_push(glyph=ICON_BEER)
        .preview_push(" - beer\n", active=True)
        .preview_push(glyph=ICON_BACKPACK)
        .preview_push(" - backpack", active=True),
        # dynamic preview
        Candidate()
        .target_push("Item with lazily fetched preview")
        .preview_push("This icon is lazy loaded\n")
        .preview_flex_set(0.5)
        .preview_push(ref=ref_sofa),
    ]

    result: SweepEvent[Candidate | str] | None = None
    async with Sweep[Candidate | str](
        field_resolver=field_resolver,
        sweep=shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd(),
        tty=opts.tty,
        theme=opts.theme,
        log=opts.log,
    ) as sweep:
        view = (
            Container(Text(glyph=ICON_FOOT, face="fg=bg").push("Nice Footer"))
            .face(face="bg=accent/.8")
            .horizontal(Align.EXPAND)
        )
        await sweep.footer_set(view)
        await sweep.prompt_set(icon=ICON_COCKTAIL)
        await sweep.field_register_many(fields)
        await sweep.bind_struct(ctrl_q_action)
        await sweep.items_extend(candidates)

        async for event in sweep:
            if isinstance(event, SweepSize):
                continue
            result = event
            break

    print(result)


if __name__ == "__main__":
    asyncio.run(main())
