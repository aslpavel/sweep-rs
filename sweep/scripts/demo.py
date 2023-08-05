#!/usr/bin/env python3
# pyright: strict
"""Demo program that shows different functionality
"""
from __future__ import annotations
import asyncio
from typing import Any
from sweep import *
import os

ICON_BEER = SweepIcon(
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


ICON_COCKTAIL = SweepIcon(
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
    fallback="[C]",
)


ICON_BACKPACK = SweepIcon(
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


async def main():
    os.environ["RUST_LOG"] = os.environ.get("RUST_LOG", "debug")

    async with Sweep[Any](
        tty="/dev/tty",  # use different tty obtained with tty call "/dev/pts/0",
        sweep=["cargo", "run", "--bin=sweep", "--"],
        log="/tmp/sweep.log",  # nosec
    ) as sweep:
        await sweep.prompt_set(prompt="Demo", icon=ICON_COCKTAIL)
        ref_backpack = await sweep.field_register(
            Field(glyph=ICON_BACKPACK, face="fg=#076678")
        )
        ref_cocktail = await sweep.field_register(Field(glyph=ICON_COCKTAIL))
        await sweep.bind("ctrl+q", "ctrl+q was pressed")

        # simple fields
        await sweep.items_extend(
            [
                Candidate().target_push("Simple string entry"),
                Candidate()
                .target_push("Disabled text: ", active=False)
                .target_push("Enabled text"),
            ]
        )

        # colored text
        await sweep.items_extend(
            [
                Candidate()
                .target_push("Colored", face="fg=#8f3f71,bold,underline")
                .target_push(" ")
                .target_push("Text", face="fg=#fbf1c7,bg=#79740e,italic"),
            ]
        )

        # multi line entry
        await sweep.items_extend(
            [Candidate().target_push("Muli line entry\n - Second Line")]
        )

        # direct glyph icon usage example
        await sweep.items_extend(
            [
                Candidate()
                .target_push("Entry with beer icon: ")
                .target_push(glyph=ICON_BEER, face="fg=#cc241d")
            ]
        )

        # glyph icon used from reference
        await sweep.items_extend(
            [
                Candidate()
                .target_push("Entry with reference to backpack: ")
                .target_push(ref=ref_backpack)
            ]
        )

        # right text
        await sweep.items_extend(
            [
                Candidate()
                .target_push("Entry with additional data to the right")
                .right_push(ref=ref_cocktail, face="fg=#427b58")
                .right_push(" Have a cocktail")
            ]
        )

        # has preview
        await sweep.items_extend(
            [
                Candidate()
                .target_push("Point to this item (has preview)")
                .preview_push("This an awesome item preview: \n")
                .preview_push(ref=ref_cocktail)
                .preview_push(" - cocktail\n")
                .preview_push(glyph=ICON_BEER)
                .preview_push(" - beer\n")
                .preview_push(glyph=ICON_BACKPACK)
                .preview_push(" - backpack")
                # .preview_flex_set(0.5)
            ]
        )

        async for event in sweep:
            return event


if __name__ == "__main__":
    print(asyncio.run(main()))
