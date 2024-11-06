"""Application launcher

Lists all available desktop entries on the system
"""

# pyright: strict
from __future__ import annotations

import argparse
import asyncio
import copy
import io
import os
import re
import shlex
import traceback
from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from typing import Any, NamedTuple, cast
from collections.abc import AsyncIterator, Awaitable, Callable, Iterable, Sequence

from PIL import Image as PILImage
from PIL.Image import Resampling

from .. import (
    Align,
    BindHandler,
    Candidate,
    Container,
    Event,
    Field,
    Flex,
    Icon,
    IconFrame,
    Image,
    Sweep,
    SweepBind,
    SweepSelect,
    SweepWindow,
    Text,
    View,
    ViewRef,
)
from . import sweep_default_cmd

_last_ref = 0


def alloc_ref() -> int:
    global _last_ref
    _last_ref += 1
    return _last_ref


FRAME_ON = (
    IconFrame()
    .border_width(2)
    .border_radius(15)
    .border_color("accent")
    .margin(7, 12)
    .padding(5)
    .fill_color("accent/.3")
)
FRAME_OFF = copy.deepcopy(FRAME_ON).fill_color("accent/.1")


def frame_icon(icon: Icon) -> tuple[View, View]:
    icon_on = Container(copy.deepcopy(icon).frame(FRAME_ON)).face("")
    icon_off = Container(icon.frame(FRAME_OFF)).face("fg=accent")
    return icon_on, icon_off


# tabler-player-play
PLAY_ICON_REF = alloc_ref()
PLAY_ICON = Icon(
    view_box=(0, 0, 100, 100),
    size=(1, 3),
    frame=FRAME_ON,
    path="M22.95,87.35Q21.45,87.25 20.55,86.35Q19.95,85.85 19.05,84.35L19.05,16.15Q20.45,14.25 21.25,13.65Q22.55,12.65 24.45,13.35Q24.55,13.35 51.95,30.1Q79.35,46.85 79.65,47.15Q80.25,47.65 80.6,48.55Q80.95,49.45 80.95,50.25Q80.95,51.05 80.6,51.95Q80.25,52.85 79.65,53.35Q79.35,53.65 51.95,70.4Q24.55,87.15 24.45,87.15L24.25,87.25Q23.45,87.35 22.95,87.35ZM27.35,50.25Q27.35,75.95 27.55,75.75L48.25,63.05Q68.85,50.35 68.85,50.25Q68.85,50.15 48.25,37.45L27.55,24.75Q27.35,24.55 27.35,50.25Z",
)
# tabler-player-pause
PAUSE_ICON_REF = alloc_ref()
PAUSE_ICON = Icon(
    view_box=(0, 0, 100, 100),
    size=(1, 3),
    frame=FRAME_ON,
    path="M21.1,23.95Q21.1,23.95 21.1,23.95Q22.1,21.15 23.2,19.85Q25.3,17.35 28.6,17.05L29.4,17.05Q32.1,16.95 34,16.95L38,17.05Q39,17.05 39.4,17.15Q41.8,17.75 43.45,19.45Q45.1,21.15 45.7,23.65Q45.8,23.95 45.9,25.35L45.9,74.55Q45.8,76.05 45.7,76.45Q45.2,78.95 43.5,80.65Q41.8,82.35 39.3,82.95Q38.8,83.05 36.7,83.05L28.9,82.95Q28.1,82.95 27.4,82.85Q24.6,82.05 23,79.95Q22.1,78.75 21.1,76.15L21.1,23.95ZM61.6,17.05Q61.7,17.05 62.4,17.05Q65.2,16.95 67,16.95Q70.9,16.95 72,17.1Q73.1,17.25 74.4,17.95Q76.2,18.85 77.3,20.55Q78,21.55 78.9,23.95L78.9,76.15Q77.9,78.75 77,79.95Q75.4,82.05 72.6,82.85Q71.9,82.95 71.1,82.95L63.2,83.05Q61.2,83.05 60.7,82.95Q58.2,82.35 56.5,80.65Q54.8,78.95 54.3,76.45Q54.2,76.15 54.1,74.75L54.1,25.35Q54.2,23.95 54.3,23.65Q54.8,20.95 56.85,19.15Q58.9,17.35 61.6,17.05ZM29.4,25.25L29.4,74.75L37.6,74.75L37.6,25.25L29.4,25.25ZM62.4,50.05L62.4,74.75L70.6,74.75L70.6,25.25L62.4,25.25L62.4,50.05Z",
)
# tabler-player-stop
STOP_ICON_REF = alloc_ref()
STOP_ICON = Icon(
    view_box=(0, 0, 100, 100),
    size=(1, 3),
    frame=FRAME_ON,
    path="M28.1,17Q28.2,17 29.2,17Q40.7,16.9 50.6,16.9L72.2,17Q76,17.9 78.6,20Q82,22.7 82.9,27.2Q83,27.9 83,31.1L83,68.8Q83,72.1 82.9,72.8Q82.3,75.5 80.7,77.8Q78,81.6 73.1,82.8L72,83.1L31.1,83Q27.9,83 27.2,82.9Q24.5,82.4 22.2,80.7Q18.1,77.9 17.1,72.8Q17,72.1 17,68.8L17,31.1Q17,27.9 17.1,27.2Q18,22.9 21.2,20.1Q24.2,17.5 28.1,17ZM28.4,25.3Q27.2,25.6 26.4,26.5Q25.6,27.4 25.3,28.6Q25.2,29 25.2,50.4L25.2,71.4Q26.1,73.1 26.5,73.5Q27,74.2 28.1,74.5Q28.5,74.7 31.6,74.7L71.4,74.7Q73.1,73.9 73.6,73.45Q74.1,73 74.5,72L74.7,71.4L74.7,28.6Q73.9,26.9 73.45,26.4Q73,25.9 72,25.5L71.4,25.2L50.2,25.2Q28.8,25.2 28.4,25.3Z",
)
# tabler-playlist
PLAYLIST_ICON_REF = alloc_ref()
PLAYLIST_ICON = Icon(
    view_box=(0, 0, 100, 100),
    size=(1, 3),
    path="M66.6,15.8Q67.8,13.9 68.15,13.5Q68.5,13.1 69.6,12.8Q70,12.6 71.4,12.6L79.3,12.6L88.4,12.7Q89.8,13.4 90.2,13.8Q91.4,15 91.4,16.6Q91.4,18.2 90.4,19.4Q89.4,20.6 87.8,20.8Q87.4,20.9 81.1,20.9L74.9,20.9L74.9,72.5L74.7,73.5Q73.7,78.1 70.5,81.5Q67.5,84.8 63.2,86.1Q58.9,87.4 54.7,86.4Q50,85.3 46.5,81.65Q43,78 42.2,73.1Q41.5,68.6 43.2,64.25Q44.9,59.9 48.5,57.2Q52.3,54.2 57.2,53.9Q59.8,53.7 62,54.2Q64.2,54.7 66.5,55.9L66.6,35.9L66.6,15.8ZM55.1,16.6Q56.9,17.5 57.5,18.1Q58.4,19.1 58.4,20.8Q58.4,21.8 58,22.7Q57.3,24.2 55.5,24.7Q55.1,24.9 52.2,24.9L12,24.9Q10.1,23.7 9.6,23Q8.7,21.9 9,20.2Q9.2,18.9 10.2,17.95Q11.2,17 12.5,16.7L13.1,16.7Q24.1,16.6 34.1,16.6L55.1,16.6ZM12.1,33.3Q12.5,33.1 33.55,33.1Q54.6,33.1 55.1,33.3Q57.2,33.6 58.05,35.45Q58.9,37.3 58.05,39.15Q57.2,41 55.1,41.4Q54.6,41.5 33.6,41.5Q12.6,41.5 12.2,41.4Q10.1,40.9 9.35,39.1Q8.6,37.3 9.4,35.5Q10.2,33.7 12.1,33.3ZM12,49.8Q12.3,49.7 25.6,49.7L38.9,49.7L40,50.4Q40.4,50.6 40.7,50.9Q41.9,52.1 41.9,53.7Q41.9,55.3 40.85,56.5Q39.8,57.7 38.3,57.9Q37.6,58 25.4,58Q13.2,58 12.6,57.9Q10.5,57.7 9.55,55.95Q8.6,54.2 9.25,52.3Q9.9,50.4 12,49.8ZM57.3,62.2Q54.2,62.7 52.3,64.9Q50.5,66.9 50.3,69.7Q50.1,72.5 51.5,74.8Q53.1,77.2 56.15,78.1Q59.2,79 61.9,77.7Q64.6,76.4 65.9,73.7Q67.2,71 66.3,67.9Q65.8,66.1 64.3,64.6Q61.6,61.6 57.3,62.2Z",
)
# tabler-music-search
DATABASE_ICON_REF = alloc_ref()
DATABASE_ICON = Icon(
    view_box=(0, 0, 100, 100),
    size=(1, 3),
    path="M34.7,8.9Q34.8,8.9 35.1,8.9Q45.7,8.8 56.3,8.8L74.9,8.8Q77.8,8.8 78.2,9Q79.6,9.5 80.4,10.5L80.5,10.7Q80.8,11.2 80.9,11.9Q81,12.9 81,16.2L81.1,40Q81,42.3 80.9,42.9Q80.8,43.5 80.5,44L80.4,44.1Q79.6,45.4 78.15,45.8Q76.7,46.2 75.3,45.55Q73.9,44.9 73.2,43.4L72.8,42.7L72.8,33.5L39.9,33.5L39.9,50.3Q39.9,67.4 39.8,68.1Q39.3,72.3 37,75.7Q34.7,79.1 31.1,81.05Q27.5,83 23.3,83Q18.4,83 14.4,80.4Q10.6,77.9 8.6,73.8Q6.6,69.7 6.8,65.2Q7.2,60.5 10.1,56.7Q12.1,53.9 15.2,52.2Q18.8,50.1 22.85,50Q26.9,49.9 31.5,52.1L31.6,32.4Q31.6,12.5 31.7,12.1Q31.9,11 32.75,10.05Q33.6,9.1 34.7,8.9ZM39.9,17L39.9,25.3L72.8,25.3L72.8,17L39.9,17ZM93.4,87.1Q93.4,88.9 92.25,90.05Q91.1,91.2 89.3,91.2Q88.4,91.2 87.9,91.05Q87.4,90.9 86.5,90.1Q85.9,89.6 84,87.7L81.2,84.9L80.2,85.4Q76.1,87.4 71.65,87.05Q67.2,86.7 63.5,84.25Q59.8,81.8 58,77.8Q55.6,72.8 56.6,67.6Q57.6,62.8 61.1,59.15Q64.6,55.5 69.5,54.5Q74.7,53.4 80,55.8Q83.2,57.3 85.6,60.2Q89.3,64.6 89.3,70.6Q89.3,74.8 87.7,78L87.1,79L89.9,81.8Q91.8,83.7 92.3,84.3Q93.1,85.2 93.25,85.7Q93.4,86.2 93.4,87.1ZM25.3,58.6Q21.4,57.7 18.6,59.7Q16.1,61.4 15.4,64.65Q14.7,67.9 16.2,70.6Q17.8,73.6 21.5,74.5Q25.2,75.4 27.9,73.4Q30.4,71.7 31.2,68.4Q32,65.1 30.6,62.4Q29,59.4 25.3,58.6ZM71.1,62.6Q68.6,63.2 66.9,64.95Q65.2,66.7 64.75,69.1Q64.3,71.5 65.2,73.8Q66.3,76.4 68.7,77.75Q71.1,79.1 73.85,78.85Q76.6,78.6 78.6,76.6Q80.6,74.6 80.95,71.95Q81.3,69.3 80.15,66.9Q79,64.5 76.7,63.3Q74.2,62 71.1,62.6Z",
)
# tabler-arrows-shuffle
SHUFFLE_ON_ICON_REF = ViewRef(alloc_ref())
SHUFFLE_OFF_ICON_REF = ViewRef(alloc_ref())
SHUFFLE_ON_ICON, SHUFFLE_OFF_ICON = frame_icon(
    Icon(
        view_box=(0, 0, 100, 100),
        size=(1, 3),
        path="M71.05,18.8Q70.15,16.9 71,15.2Q71.85,13.5 73.65,13Q75.45,12.5 76.95,13.6Q77.45,13.9 84,20.45Q90.55,27 90.75,27.4Q91.65,29.4 90.85,31.1Q90.65,31.5 89.95,32.2L86.75,35.5L77.65,44.5Q76.65,45.4 76.2,45.6Q75.75,45.8 74.95,45.8L74.65,45.8Q72.95,45.8 71.95,44.9Q71.05,44.1 70.75,42.8Q70.55,41.8 70.65,40.95Q70.75,40.1 71.25,39.5Q71.75,38.9 74.25,36.4L77.05,33.5L70.95,33.5Q65.45,33.5 63.25,33.8Q59.95,34.4 56.85,36.5Q54.75,38 52.85,37.5Q51.25,37.1 50.45,35.45Q49.65,33.8 50.15,32.3Q50.85,30.1 55.15,27.9Q58.45,26.3 61.25,25.7Q62.95,25.4 64.1,25.3Q65.25,25.2 70.45,25.2L77.05,25.2L71.75,19.8Q71.15,19.2 71.05,18.8ZM11.65,25.4Q11.95,25.3 13.25,25.3L26.55,25.3Q28.15,25.3 28.75,25.4Q34.15,26.3 38.75,29.3Q43.65,32.4 46.55,37.4Q49.75,42.8 49.95,49.6Q50.05,52.5 50.45,54Q51.65,58.7 55.2,62Q58.75,65.3 63.65,66.2L63.65,66.2Q64.55,66.4 65.55,66.5Q67.05,66.5 70.95,66.5L77.05,66.5L74.25,63.6Q71.75,61.1 71.25,60.5Q70.75,59.9 70.65,59.05Q70.55,58.2 70.75,57.2Q71.05,55.9 71.95,55.1Q72.95,54.2 74.65,54.2L74.95,54.2Q75.75,54.1 76.2,54.3Q76.65,54.5 77.65,55.5L80.95,58.8L89.95,67.8Q90.65,68.5 90.85,68.9Q91.65,70.6 90.75,72.6Q90.55,72.9 84,79.5Q77.45,86.1 76.95,86.4Q75.45,87.5 73.65,86.95Q71.85,86.4 70.95,84.75Q70.05,83.1 70.95,81.2Q71.05,80.8 72.75,79.2L77.05,74.7L70.45,74.7Q65.75,74.7 64.25,74.6Q63.05,74.6 61.55,74.2L61.25,74.2Q56.25,73.1 51.85,69.9Q47.45,66.7 44.75,61.9Q41.95,56.8 41.75,50.8Q41.65,47.6 41.25,46Q40.05,41.3 36.45,37.95Q32.85,34.6 27.95,33.8Q27.15,33.6 26.05,33.6L11.75,33.4Q9.65,32.7 9,31Q8.35,29.3 9.1,27.65Q9.85,26 11.65,25.4ZM36.25,62.6Q35.95,62.6 35.05,63.3Q33.45,64.3 32.25,64.9Q30.25,65.9 27.95,66.2Q27.15,66.4 25.95,66.4L11.75,66.6Q9.95,67.2 9.2,68.65Q8.45,70.1 8.85,71.65Q9.25,73.2 10.55,74.1L10.65,74.1Q11.15,74.5 11.7,74.6Q12.25,74.7 14.05,74.7L26.55,74.7Q28.15,74.7 28.75,74.5Q32.95,73.9 36.65,72Q41.25,69.7 41.65,67Q41.85,65.4 41,64.25Q40.15,63.1 38.8,62.6Q37.45,62.1 36.25,62.6Z",
    )
)
# tabler-repeat
REPEAT_ICON_REF = ViewRef(alloc_ref())
REPEAT_ICON = Icon(
    view_box=(0, 0, 100, 100),
    size=(1, 3),
    frame=FRAME_ON,
    path="M66.8,11.3Q67.8,9.5 69.55,9.05Q71.3,8.6 72.9,9.5L81,17.5Q84.4,20.9 85.3,21.8Q86.5,23.2 86.75,23.7Q87,24.2 87,25L87,26.3Q86.9,26.6 86.75,27.05Q86.6,27.5 85.9,28.2L74.2,40Q72.9,41.1 72.45,41.4Q72,41.7 71.4,41.8L71.3,41.8Q69.9,42 68.65,41.35Q67.4,40.7 66.8,39.4Q66.6,39 66.6,38.3L66.6,37.6Q66.6,36.7 66.75,36.25Q66.9,35.8 67.7,35Q68.2,34.4 70.1,32.5L73,29.5L50.4,29.5L27.8,29.6Q25.7,30.2 24.5,31.1Q22.1,32.7 21.4,35.7Q21.2,36.3 21.2,37.4L21.1,43.4L21.1,51.3Q20,52.9 19.3,53.5Q18,54.5 16.3,54.2Q14.9,53.9 14,52.9Q13.5,52.3 13,51.1L12.9,44Q12.9,37.8 13,36.1Q13.1,34.4 13.8,32.5Q15.3,27.6 19.6,24.5Q23.4,21.6 27.8,21.3Q28.5,21.2 50.9,21.2L73,21.2L70.1,18.2Q68.2,16.3 67.7,15.7Q66.9,14.9 66.75,14.45Q66.6,14 66.6,13.1L66.6,12.3Q66.6,11.7 66.8,11.3ZM82,46.1Q80.9,46.4 80.1,47.2Q79.6,47.7 78.9,48.9L78.9,56.7L78.8,62.7Q78.8,63.8 78.6,64.4Q77.9,67.2 75.9,68.8Q74.5,69.9 72.2,70.6L49.6,70.7L27,70.7L29.9,67.7Q31.8,65.8 32.3,65.2Q33.1,64.4 33.25,63.95Q33.4,63.5 33.4,62.6L33.4,62.5Q33.4,60.6 32.3,59.5Q31.4,58.7 30.1,58.5Q29.2,58.3 28.7,58.4L28.6,58.4Q28,58.5 27.5,58.8Q27,59.1 25.8,60.2L14,72Q13.4,72.7 13.2,73.15Q13,73.6 13,73.8L13,74.8L13,75.9Q13,76.1 13.2,76.55Q13.4,77 14,77.7L25.8,89.5Q27,90.6 27.5,90.9Q28,91.2 28.6,91.3L28.7,91.3Q29.2,91.4 30.1,91.2Q31.4,91 32.3,90.2Q33.4,89.1 33.4,87.2L33.4,87.1Q33.4,86.2 33.25,85.75Q33.1,85.3 32.3,84.5Q31.8,83.9 29.9,82L27,79L49.1,79Q71.5,79 72.2,78.9Q78,78.3 82.2,74.2Q86.4,70.1 87,64.3Q87.1,63.7 87.1,56.2L87,49.1Q86.4,47.7 85.5,46.9Q84,45.6 82,46.1Z",
)
# tabler-repeat-off
REPEAT_OFF_ICON_REF = ViewRef(alloc_ref())
REPEAT_OFF_ICON = Icon(
    view_box=(0, 0, 100, 100),
    size=(1, 3),
    frame=FRAME_ON,
    path="M8.75,13.7Q8.45,12.3 9.25,10.9Q10.05,9.5 11.55,9Q13.05,8.5 14.65,9.3Q15.35,9.6 52.8,47.1Q90.25,84.6 90.75,85.3Q91.55,87 91,88.5Q90.45,90 89.15,90.75Q87.85,91.5 86.35,91.3L86.25,91.3Q85.55,91.1 85.15,90.8Q84.45,90.4 83.15,89.1Q81.95,88 78.65,84.7L72.75,78.8L72.05,78.9L71.15,78.9Q59.45,79 49.15,79L26.95,79L29.85,82Q31.75,83.9 32.25,84.5Q33.05,85.3 33.2,85.75Q33.35,86.2 33.35,87.1L33.35,87.2Q33.35,89.1 32.25,90.2Q31.35,91 30.05,91.2Q29.15,91.4 28.65,91.3L28.55,91.3Q27.95,91.2 27.45,90.9Q26.95,90.6 25.75,89.5L13.95,77.7Q13.35,77 13.15,76.55Q12.95,76.1 12.95,75.8L12.95,74.8L12.95,73.8Q12.95,73.6 13.15,73.15Q13.35,72.7 13.95,72L25.75,60.2Q26.95,59.1 27.45,58.8Q27.95,58.5 28.55,58.4L28.65,58.4Q29.15,58.3 30.05,58.5Q31.35,58.7 32.25,59.5Q33.35,60.6 33.35,62.5L33.35,62.6Q33.35,63.5 33.2,63.95Q33.05,64.4 32.25,65.2Q31.75,65.8 29.85,67.7L26.95,70.7L64.55,70.7L44.75,50.8Q24.85,30.9 24.75,30.9Q24.65,30.9 24.05,31.4Q23.35,32 22.65,32.9Q21.75,34.2 21.35,35.7Q21.15,36.3 21.15,37.4L21.05,43.4L21.05,51.3Q19.95,52.9 19.25,53.5Q17.95,54.5 16.25,54.2Q14.85,53.9 13.95,52.9Q13.45,52.3 12.95,51.1L12.85,38.6Q12.85,35.4 13.05,34.8Q14.05,29.6 17.85,25.9L18.85,24.9L10.55,16.5Q9.45,15.4 9.2,14.9Q8.95,14.4 8.75,13.7ZM66.75,11.3Q67.75,9.5 69.5,9.05Q71.25,8.6 72.85,9.5L80.95,17.5Q84.35,20.9 85.25,21.8Q86.45,23.2 86.7,23.7Q86.95,24.2 86.95,25L86.95,26.3Q86.85,26.6 86.7,27.05Q86.55,27.5 85.85,28.2L74.15,40Q72.85,41.1 72.4,41.4Q71.95,41.7 71.35,41.8L71.25,41.8Q69.85,42 68.6,41.35Q67.35,40.7 66.75,39.4Q66.55,39 66.55,38.3L66.55,37.6Q66.55,36.7 66.7,36.25Q66.85,35.8 67.65,35Q68.15,34.4 70.05,32.5L72.95,29.5L57.15,29.5Q41.25,29.5 40.95,29.4Q40.15,29.3 39.25,28.7Q37.65,27.5 37.65,25.35Q37.65,23.2 39.25,22Q40.15,21.4 40.95,21.3Q41.25,21.2 57.15,21.2L72.95,21.2L70.05,18.2Q68.15,16.3 67.65,15.7Q66.85,14.9 66.7,14.45Q66.55,14 66.55,13.1L66.55,12.3Q66.55,11.7 66.75,11.3ZM81.95,46.1Q80.85,46.4 80.05,47.2Q79.55,47.7 78.85,48.9L78.85,56.2L78.75,63.6L78.55,64.6Q77.95,66.9 78.9,68.35Q79.85,69.8 81.5,70.15Q83.15,70.5 84.65,69.55Q86.15,68.6 86.55,66.5Q86.95,65.2 87.05,63.5Q87.05,62.1 87.05,56.1L86.95,49.1Q86.35,47.7 85.45,46.9Q83.95,45.6 81.95,46.1Z",
)
# tabler-repeat-once
REPEAT_ONCE_ICON_REF = ViewRef(alloc_ref())
REPEAT_ONCE_ICON = Icon(
    view_box=(0, 0, 100, 100),
    size=(1, 3),
    frame=FRAME_ON,
    path="M66.8,11.3Q67.8,9.5 69.55,9.05Q71.3,8.6 72.9,9.5L81,17.5Q84.4,20.9 85.3,21.8Q86.5,23.2 86.75,23.7Q87,24.2 87,25L87,26.3Q86.9,26.6 86.75,27.05Q86.6,27.5 85.9,28.2L74.2,40Q72.9,41.1 72.45,41.4Q72,41.7 71.4,41.8L71.3,41.8Q69.9,42 68.65,41.35Q67.4,40.7 66.8,39.4Q66.6,39 66.6,38.3L66.6,37.6Q66.6,36.7 66.75,36.25Q66.9,35.8 67.7,35Q68.2,34.4 70.1,32.5L73,29.5L50.4,29.5L27.8,29.6Q25.7,30.2 24.5,31.1Q22.1,32.7 21.4,35.7Q21.2,36.3 21.2,37.4L21.1,43.4L21.1,51.3Q20,52.9 19.3,53.5Q18,54.5 16.3,54.2Q14.9,53.9 14,52.9Q13.5,52.3 13,51.1L12.9,44Q12.9,37.8 13,36.1Q13.1,34.4 13.8,32.5Q15.3,27.6 19.6,24.5Q23.4,21.6 27.8,21.3Q28.5,21.2 50.9,21.2L73,21.2L70.1,18.2Q68.2,16.3 67.7,15.7Q66.9,14.9 66.75,14.45Q66.6,14 66.6,13.1L66.6,12.3Q66.6,11.7 66.8,11.3ZM45.5,50.1Q45.2,50.1 44.5,49.9Q43.4,49.5 42.7,48.7Q41.8,47.5 41.8,45.7Q41.8,44.7 42.3,43.95Q42.8,43.2 45,41Q46.8,39.2 47.4,38.7Q48.1,38.1 48.65,37.9Q49.2,37.7 50.2,37.7Q51.5,37.7 52.7,38.75Q53.9,39.8 54.1,41.1Q54.1,41.6 54.1,50.1Q54.1,58.6 54.1,59.1Q53.9,59.9 53.3,60.8Q52.2,62.4 50,62.4Q47.8,62.4 46.7,60.8Q46.1,59.9 45.9,59.1Q45.8,58.8 45.8,54.4L45.8,50.1L45.5,50.1ZM82,46.1Q80.9,46.4 80.1,47.2Q79.6,47.7 78.9,48.9L78.9,56.7L78.8,62.7Q78.8,63.8 78.6,64.4Q77.9,67.2 75.9,68.8Q74.5,69.9 72.2,70.6L49.6,70.7L27,70.7L29.9,67.7Q31.8,65.8 32.3,65.2Q33.1,64.4 33.25,63.95Q33.4,63.5 33.4,62.6L33.4,62.5Q33.4,60.6 32.3,59.5Q31.4,58.7 30.1,58.5Q29.2,58.3 28.7,58.4L28.6,58.4Q28,58.5 27.5,58.8Q27,59.1 25.8,60.2L14,72Q13.4,72.7 13.2,73.15Q13,73.6 13,73.8L13,74.8L13,75.9Q13,76.1 13.2,76.55Q13.4,77 14,77.7L25.8,89.5Q27,90.6 27.5,90.9Q28,91.2 28.6,91.3L28.7,91.3Q29.2,91.4 30.1,91.2Q31.4,91 32.3,90.2Q33.4,89.1 33.4,87.2L33.4,87.1Q33.4,86.2 33.25,85.75Q33.1,85.3 32.3,84.5Q31.8,83.9 29.9,82L27,79L49.1,79Q71.5,79 72.2,78.9Q78,78.3 82.2,74.2Q86.4,70.1 87,64.3Q87.1,63.7 87.1,56.2L87,49.1Q86.4,47.7 85.5,46.9Q84,45.6 82,46.1Z",
)
REPAT_TOGGLE_TAG = "repeat-toggle-tag"
RANDOM_TOGGLE_TAG = "random-toggle-tag"
DATE_RE = re.compile("(\\d{4})-?(\\d{2})?-?(\\d{2})?")


class PlayState(Enum):
    PAUSE = "pause"
    PLAY = "play"
    STOP = "stop"

    def icon(self) -> View:
        match self:
            case PlayState.PAUSE:
                return ViewRef(PAUSE_ICON_REF)
            case PlayState.PLAY:
                return ViewRef(PLAY_ICON_REF)
            case PlayState.STOP:
                return ViewRef(STOP_ICON_REF)


@dataclass
class Song:
    file: str
    duration: float
    artist: str | None
    album: str | None
    title: str | None
    date: datetime | None
    track: int | None
    attrs: dict[str, str]
    pos: int | None  # position in the playlist
    id: int | None  # song id in the playlist
    current: MPDStatus | None  # if song is currently playing

    def __init__(self, file: str) -> None:
        self.file = file
        self.duration = 0.0
        self.artist = None
        self.album = None
        self.title = None
        self.date = None
        self.track = None
        self.attrs = {}
        self.pos = None
        self.id = None
        self.current = None

    def __eq__(self, other: Any) -> bool:
        if not isinstance(other, Song):
            return False
        return self.file == other.file

    def __hash__(self) -> int:
        return hash(self.file)

    def album_id(self) -> int:
        return abs(hash(self.attrs.get("MUSICBRAINZ_ALBUMID") or self.album or ""))

    @staticmethod
    async def from_chunks(chunks: AsyncIterator[MPDChunk]) -> AsyncIterator[Song]:
        """Parse songs from *info commands"""
        song = Song("")
        async for chunk in chunks:
            match chunk.name:
                case "file":
                    if song.file:
                        yield song
                    song = Song(cast(str, chunk.data))
                case "duration":
                    song.duration = float(chunk.data)
                case "Artist":
                    song.artist = cast(str, chunk.data)
                case "Album":
                    song.album = cast(str, chunk.data)
                case "Title":
                    song.title = cast(str, chunk.data)
                case "Time":
                    continue
                case "Date" | "OriginalDate":
                    match = DATE_RE.match(cast(str, chunk.data).strip())
                    if match is None:
                        continue
                    year = int(match.group(1))
                    month = int(match.group(2)) if match.group(2) else 1
                    if month > 12 or month < 1:
                        month = 1
                    day = int(match.group(3)) if match.group(3) else 1
                    if day > 31 or day < 1:
                        day = 1
                    if song.date and chunk.name == "Date":
                        continue
                    song.date = datetime(year, month, day)
                case "Track":
                    song.track = int(chunk.data)
                case "Pos":
                    song.pos = int(chunk.data)
                case "Id":
                    song.id = int(chunk.data)
                case name:
                    song.attrs[name] = cast(str, chunk.data)
        if song.file:
            yield song

    def to_candidate(self) -> Candidate:
        result = Candidate()

        # target
        face = "underline" if self.current is not None else None
        if self.track:
            result.target_push(f"{self.track:>02}. ", active=False)
        if self.title:
            result.target_push(self.title, face=face)
        else:
            result.target_push(os.path.basename(self.file), face=face)

        # right
        if self.current is not None:
            if self.current.state == PlayState.PAUSE:
                result.right_push(ref=PAUSE_ICON_REF)
            elif self.current.state == PlayState.PLAY:
                result.right_push(ref=PLAY_ICON_REF)
        result.right_push(duration_fmt(self.duration))

        # preview
        if self.artist:
            result.preview_push("Artist: ", face="bold").preview_push(
                f"{self.artist}\n", active=True
            )
        if self.album:
            result.preview_push("Album : ", face="bold")
            date = self.date
            if date is not None:
                result.preview_push(f"[{date.year}] ")
            result.preview_push(f"{self.album}\n", active=True)
        result.preview_push(ref=self.album_id()).preview_flex_set(1)

        return result


@dataclass
class Album:
    name: str
    date: datetime | None
    songs: list[Song]

    def __init__(self, name: str, date: datetime | None) -> None:
        self.name = name
        self.date = date
        self.songs = []


@dataclass
class Database:
    artists: dict[str, dict[str, Album]]

    def __init__(self) -> None:
        self.artists = {}

    def add(self, song: Song) -> None:
        artist = song.artist or ""
        album_name = song.album or ""
        albums = self.artists.get(artist)
        if albums is None:
            albums = dict[str, Album]()
            self.artists[artist] = albums
        album = albums.get(album_name)
        if album is None:
            album = Album(album_name, song.date)
            albums[album_name] = album
        album.songs.append(song)

    def songs(
        self,
        artist: str | None = None,
        album: str | None = None,
    ) -> list[Song]:
        artists: Iterable[dict[str, Album]]
        if artist is None:
            artists = self.artists.values()
        else:
            artist_albums = self.artists.get(artist)
            if not artist_albums:
                return []
            artists = [artist_albums]
        albums: Iterable[Album] = (
            album for albums in artists for album in albums.values()
        )
        if album is not None:
            albums = (album_obj for album_obj in albums if album_obj.name == album)
        return [song for album in albums for song in album.songs]


class MPDEvent(Enum):
    DATABASE = "database"
    UPDATE = "update"
    STORED_PLAYLIST = "stored_playlist"
    PLAYLIST = "playlist"
    PLAYER = "player"
    MIXER = "mixer"
    OUTPUT = "output"
    OPTIONS = "options"
    PARTITION = "partition"
    STICKER = "sticker"
    SUBSCRIPTION = "subscription"
    MESSAGE = "message"
    NEIGHBOUR = "neighbour"
    MOUNT = "mount"


class MPDStatus(NamedTuple):
    volume: int
    repeat: bool
    random: bool
    single: bool
    playlist_version: int
    playlist_length: int
    state: PlayState
    elapsed: float | None
    duration: float | None
    playlist_song: int | None
    playlist_song_id: int | None


class MPDState(Enum):
    WAIT = 0
    IDLE = 1
    REQUEST = 2


class MPDRepeat(Enum):
    OFF = 0
    ON = 1
    SINGLE = 2

    @classmethod
    def from_status(cls, status: MPDStatus) -> MPDRepeat:
        match (status.repeat, status.single):
            case (False, _):
                return MPDRepeat.OFF
            case (True, False):
                return MPDRepeat.ON
            case (True, True):
                return MPDRepeat.SINGLE

    def icon(self) -> View:
        match self:
            case MPDRepeat.OFF:
                return REPEAT_OFF_ICON_REF
            case MPDRepeat.ON:
                return REPEAT_ICON_REF
            case MPDRepeat.SINGLE:
                return REPEAT_ONCE_ICON_REF

    def next(self) -> MPDRepeat:
        return MPDRepeat((self.value + 1) % len(MPDRepeat))


class MPDChunk(NamedTuple):
    """Single chunk of data returned as response"""

    name: str
    data: str | bytes

    def get_bytes(self) -> bytes:
        """Return bytes if it was binary data"""
        if self.name == "binary":
            return cast(bytes, self.data)
        return b""

    def __repr__(self) -> str:
        if self.name == "binary":
            return f"binary={len(self.data)}"
        return f"{self.name}={self.data}"


class MPD:
    """MPD Client implementation

    Reference: https://mpd.readthedocs.io/en/latest/protocol.html
    """

    __slots__ = [
        "events",
        "_host",
        "_port",
        "_database",
        "_reader",
        "_writer",
        "_state",
        "_state_cond",
        "_idle_task",
        "_album_id_to_song",
    ]

    def __init__(self, host: str = "localhost", port: int = 6600):
        self.events = Event[MPDEvent]()
        self._host = host
        self._port = port
        self._database: Database | None = None

        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None

        self._state = MPDState.WAIT
        self._state_cond = asyncio.Condition()
        self._idle_task: asyncio.Task[None] | None = None

        self._album_id_to_song: dict[int, Song] = {}

    async def __aenter__(self) -> MPD:
        self._reader, self._writer = await asyncio.open_connection(
            self._host, self._port
        )
        init = await self._reader.readline()
        init = init.strip()
        if not init.startswith(b"OK MPD"):
            raise RuntimeError(f"invalid initial response from the MPD: {init}")
        self._idle_task = asyncio.create_task(self._idle_coro(), name="mpd-idle")
        return self

    async def __aexit__(self, _et: Any, _eo: Any, _tb: Any) -> bool:
        if self._writer:
            self._writer.close()
        self._writer, self._reader = None, None
        return False

    async def _send_request(self, cmd: str, args: Sequence[str]) -> None:
        """Send MPD request"""
        if self._writer is None:
            raise RuntimeError("MPD is not connected")
        self._writer.write(cmd.encode())
        for arg in args:
            self._writer.write(b" ")
            self._writer.write(mpd_escape(arg).encode())
        self._writer.write(b"\n")
        await self._writer.drain()

    async def _recv_response(self) -> AsyncIterator[MPDChunk]:
        """Receive MPD response"""
        if self._reader is None:
            raise RuntimeError("MPD is not connected")
        while True:
            line = await self._reader.readline()
            line = line.strip()
            if line == b"OK":
                break
            elif line.startswith(b"ACK "):
                raise ValueError(line[4:].strip().decode())
            name, value = line.split(b": ", maxsplit=1)
            if name == b"binary":
                data = await self._reader.readexactly(int(value))
                yield MPDChunk("binary", data)
                await self._reader.readline()
            else:
                yield MPDChunk(name.decode(), value.decode())

    async def _idle_coro(self) -> None:
        """Client needs to be in IDLE state if there is no request to avoid timeout"""
        while self._reader is not None and self._writer is not None:
            await asyncio.sleep(0.1)
            async for chunk in self._call("idle"):
                if chunk.name == "changed":
                    try:
                        event = MPDEvent(chunk.data)
                        if event == MPDEvent.DATABASE:
                            self._database = None
                        self.events(event)
                    except ValueError:
                        pass

    async def _call(self, cmd: str, *args: str) -> AsyncIterator[MPDChunk]:
        """Issue MPD command"""
        async with self._state_cond:
            # interrupt idle state
            if self._state == MPDState.IDLE:
                await self._send_request("noidle", [])
            # wait for client to transition into WAIT state
            while self._state != MPDState.WAIT:
                await self._state_cond.wait()
            # change state
            self._state = MPDState.IDLE if cmd == "idle" else MPDState.REQUEST
            await self._send_request(cmd, args)
        try:
            async for chunk in self._recv_response():
                yield chunk
        finally:
            # transition to WAIT state and wake up other tasks
            async with self._state_cond:
                self._state = MPDState.WAIT
                self._state_cond.notify_all()

    async def _call_dict(self, cmd: str, *args: str) -> dict[str, str]:
        """Issue MPD command and collect result to a dictionary"""
        attrs: dict[str, str] = {}
        async for chunk in self._call(cmd, *args):
            attrs[chunk.name] = cast(str, chunk.data)
        return attrs

    def song_by_id(self, id: int) -> Song | None:
        return self._album_id_to_song.get(id)

    async def database(self) -> Database:
        if self._database is not None:
            return self._database
        database = Database()
        async for song in Song.from_chunks(self._call("listallinfo")):
            database.add(song)
            self._album_id_to_song[song.album_id()] = song
        self._database = database
        return database

    async def play(self, song: Song) -> None:
        if song.id is None:
            return
        await self._call_dict("playid", str(song.id))

    async def pause(self, pause: bool | None = None) -> None:
        """Pause/Resume playback, if pause is not set then toggle"""
        if pause is None:
            await self._call_dict("pause")
        else:
            await self._call_dict("pause", str(int(pause)))

    async def repeat(self, repeat: MPDRepeat | None = None) -> MPDRepeat:
        """Toggle repeat mode"""
        if repeat is None:
            repeat = MPDRepeat.from_status(await self.status()).next()
        match repeat:
            case MPDRepeat.OFF:
                repeat_flag, once_flag = False, False
            case MPDRepeat.ON:
                repeat_flag, once_flag = True, False
            case MPDRepeat.SINGLE:
                repeat_flag, once_flag = True, True
        await self._call_dict("single", "1" if once_flag else "0")
        await self._call_dict("repeat", "1" if repeat_flag else "0")
        return repeat

    async def random(self, random: bool | None = None) -> bool:
        """Toggle random mode"""
        if random is None:
            random = not (await self.status()).random
        await self._call_dict("random", "1" if random else "0")
        return random

    async def seekcur(self, offset: float, absolute: bool = False) -> None:
        """Seek to the position within the current song"""
        if absolute:
            await self._call_dict("seekcur", str(abs(offset)))
        else:
            await self._call_dict(
                "seekcur", "{}{}".format("+" if offset > 0 else "", offset)
            )

    async def add(
        self,
        song: Song,
        pos: int | None = None,
        relative: bool = False,
        allow_dup: bool = False,
    ) -> int | None:
        """Add song to the playlist"""
        if not allow_dup:
            files = {song.file for song in await self.playlistinfo()}
            if song.file in files:
                return

        cmd = "addid"
        if pos is None:
            attrs = await self._call_dict(cmd, song.file)
        elif relative:
            pos_str = str(pos) if pos < 0 else f"+{pos}"
            attrs = await self._call_dict(cmd, song.file, pos_str)
        else:
            if pos < 0:
                raise ValueError("position must positivie if relative is not set")
            attrs = await self._call_dict(cmd, song.file, str(pos))
        return int(attrs["Id"])

    async def delete(self, song: Song) -> None:
        """Remove song from the playlist"""
        if song.id is None:
            return
        await self._call_dict("deleteid", str(song.id))

    async def move(self, song: Song, pos: int, relative: bool = True) -> None:
        if song.pos is None:
            return
        pos = song.pos + pos if relative else pos
        status = await self.status()
        pos = min(max(0, pos), status.playlist_length - 1)
        await self._call_dict("move", str(song.pos), str(pos))

    async def status(self) -> MPDStatus:
        attrs = await self._call_dict("status")
        elapsed_opt = attrs.get("elapsed")
        duration_opt = attrs.get("duration")
        playlist_song_opt = attrs.get("song")
        playlist_song_id_opt = attrs.get("songid")
        return MPDStatus(
            volume=int(attrs["volume"]),
            repeat=bool(int(attrs["repeat"])),
            random=bool(int(attrs["random"])),
            single=bool(int(attrs["single"])),
            playlist_version=int(attrs["playlist"]),
            playlist_length=int(attrs["playlistlength"]),
            state=PlayState(attrs["state"]),
            elapsed=float(elapsed_opt) if elapsed_opt else None,
            duration=float(duration_opt) if duration_opt else None,
            playlist_song=int(playlist_song_opt) if playlist_song_opt else None,
            playlist_song_id=int(playlist_song_id_opt)
            if playlist_song_id_opt
            else None,
        )

    async def currentsong(self) -> Song | None:
        async for song in Song.from_chunks(self._call("currentsong")):
            return song

    async def playlistinfo(self) -> list[Song]:
        status = await self.status()
        songs: list[Song] = []
        async for song in Song.from_chunks(self._call("playlistinfo")):
            if song.id == status.playlist_song_id:
                song.current = status
            self._album_id_to_song[song.album_id()] = song
            songs.append(song)
        return songs

    async def listallinfo(self) -> list[Song]:
        database = await self.database()
        return database.songs()

    async def readpicture(self, file: str, width: int = 500) -> PILImage.Image | None:
        """Read picture embedded in music file"""
        cmd = "readpicture"
        size = 0
        data = io.BytesIO()
        async for chunk in self._call(cmd, file, "0"):
            if chunk.name == "size":
                size = int(chunk.data)
            else:
                data.write(chunk.get_bytes())
        while data.tell() < size:
            async for chunk in self._call(cmd, file, str(data.tell())):
                data.write(chunk.get_bytes())
        if data.tell() == 0:
            return None
        data.seek(0)
        img = PILImage.open(data)
        if img.width != width:
            img = img.resize(
                (width, round(width * img.height / img.width)),
                resample=Resampling.BILINEAR,
            )
        return img


def mpd_escape(value: str) -> str:
    if " " not in value and "'" not in value and '"' not in value:
        return value
    value_escaped = value.replace("\\", "\\\\").replace("'", "\\'").replace('"', '\\"')
    return f'"{value_escaped}"'


def duration_fmt(duration: float) -> str:
    mins, secs = divmod(duration, 60)
    hours, mins = divmod(mins, 60)
    result = f"{mins:>02.0f}:{secs:>02.0f}"
    if hours:
        result = f"{hours:>02.0f}:{result}"
    return result


class MPDSweepView(Enum):
    PLAYLIST = 0
    SONGS = 1
    MAX = 2


class MPDSweep:
    def __init__(self, mpd: MPD, sweep: Sweep[Song]) -> None:
        self._mpd = mpd
        self._sweep = sweep
        self._view = MPDSweepView.MAX
        self._events_queue = asyncio.Queue[MPDEvent]()

    async def run(self) -> None:
        # fields
        await self._sweep.field_register(Field(glyph=PLAY_ICON), PLAY_ICON_REF)
        await self._sweep.field_register(Field(glyph=PAUSE_ICON), PAUSE_ICON_REF)
        await self._sweep.field_register(Field(glyph=PLAYLIST_ICON), PLAYLIST_ICON_REF)
        await self._sweep.field_register(Field(glyph=DATABASE_ICON), DATABASE_ICON_REF)
        self._sweep.field_resolver_set(self._field_resolver)

        await self._sweep.view_register(PLAY_ICON, PLAY_ICON_REF)
        await self._sweep.view_register(PAUSE_ICON, PAUSE_ICON_REF)
        await self._sweep.view_register(STOP_ICON, STOP_ICON_REF)
        await self._sweep.view_register(SHUFFLE_ON_ICON, SHUFFLE_ON_ICON_REF)
        await self._sweep.view_register(SHUFFLE_OFF_ICON, SHUFFLE_OFF_ICON_REF)
        await self._sweep.view_register(REPEAT_ICON, REPEAT_ICON_REF)
        await self._sweep.view_register(REPEAT_OFF_ICON, REPEAT_OFF_ICON_REF)
        await self._sweep.view_register(REPEAT_ONCE_ICON, REPEAT_ONCE_ICON_REF)

        await self._update_footer()

        # binds
        def handler(fn: Callable[[], Awaitable[None]]) -> BindHandler[Song]:
            return lambda _sweep, _tag: fn()

        await self._sweep.bind(
            key="ctrl+i",
            tag="mpd.switch.view",
            desc="Switch between different views",
            handler=handler(self.view_switch),
        )
        await self._sweep.bind(
            key="alt+g",
            tag="mpd.goto",
            desc="Goto different view",
            handler=self._goto,
        )
        await self._sweep.bind(
            key="alt+d",
            tag="mpd.song.delete",
            desc="Delete song from the playlist",
            handler=handler(self._playlist_song_delete),
        )
        await self._sweep.bind(
            key="shift+up",
            tag="mpd.song.moveup",
            desc="Move song up in the playlist",
            handler=handler(self._playlist_song_move_up),
        )
        await self._sweep.bind(
            key="shift+down",
            tag="mpd.song.movedown",
            desc="Move song down in the playlist",
            handler=handler(self._playlist_song_move_down),
        )
        await self._sweep.bind(
            key="shift+right",
            tag="mpd.song.seekfwd",
            desc="Seek forward in the current song",
            handler=handler(lambda: self._mpd.seekcur(10.0, False)),
        )
        await self._sweep.bind(
            key="shift+left",
            tag="mpd.song.seekbwd",
            desc="Seek backward in the current song",
            handler=handler(lambda: self._mpd.seekcur(-10.0, False)),
        )

        # events enqueue
        @self._mpd.events.on
        def _(event: MPDEvent) -> bool:
            self._events_queue.put_nowait(event)
            return True

        update_task = asyncio.create_task(self._updater_coro())
        update_footer_task = asyncio.create_task(self._update_footer_coro())
        await self.view_playlist()
        try:
            async for event in self._sweep:
                match event:
                    case SweepSelect(items=items):
                        await self._on_select(items)
                    case SweepBind(tag=tag) if tag == REPAT_TOGGLE_TAG:
                        await self._mpd.repeat()
                    case SweepBind(tag=tag) if tag == RANDOM_TOGGLE_TAG:
                        await self._mpd.random()
                    case SweepWindow(uid_to=uid_to):
                        self._view = (
                            MPDSweepView.SONGS
                            if uid_to == "songs"
                            else MPDSweepView.PLAYLIST
                        )
                    case _:
                        pass
        finally:
            update_footer_task.cancel()
            update_task.cancel()

    async def view_switch(self, view: MPDSweepView | None = None) -> None:
        match view:
            case None | MPDSweepView.MAX:
                view = MPDSweepView((self._view.value + 1) % MPDSweepView.MAX.value)
                await self.view_switch(view)
            case MPDSweepView.SONGS:
                await self.view_songs()
            case MPDSweepView.PLAYLIST:
                await self.view_playlist()

    async def view_playlist(self) -> None:
        """Switch to Playlist view"""
        songs = await self._mpd.playlistinfo()
        status = await self._mpd.status()

        self._view = MPDSweepView.PLAYLIST
        async with self._sweep.render_suppress():
            await self._sweep.prompt_set("Playlist", icon=PLAYLIST_ICON)
            await self._sweep.items_clear()
            await self._sweep.query_set("")
            await self._sweep.items_extend(songs)
        if status.playlist_song is not None and songs:
            await self._sweep.cursor_set(status.playlist_song)

    async def view_songs(
        self,
        songs: Sequence[Song] | None = None,
        prompt: str | None = None,
    ) -> None:
        """Switch to set view"""
        if songs is None:
            songs = await self._mpd.listallinfo()
        self._view = MPDSweepView.SONGS
        await self._sweep.stack_push("songs")
        async with self._sweep.render_suppress():
            await self._sweep.prompt_set(prompt or "Songs", icon=DATABASE_ICON)
            await self._sweep.items_clear()
            await self._sweep.query_set("")
            await self._sweep.items_extend(songs)

    async def _on_select(self, songs: list[Song]) -> None:
        match self._view:
            case MPDSweepView.PLAYLIST:
                if len(songs) != 1:
                    return
                song = songs[0]
                if song.id is not None:
                    current = await self._mpd.currentsong()
                    if current == song:
                        await self._mpd.pause()
                    else:
                        await self._mpd.play(song)
            case MPDSweepView.SONGS:
                for song in songs:
                    await self._mpd.add(song)
            case _:
                pass

    async def _update_footer_coro(self) -> None:
        while True:
            await self._update_footer()
            await asyncio.sleep(1.0)

    async def _update_footer(self) -> None:
        status = await self._mpd.status()
        repeat_icon = MPDRepeat.from_status(status).icon()

        left = (
            Flex.row()
            .push(status.state.icon())
            .push(
                Text(
                    f"{duration_fmt(status.elapsed or 0)}/{duration_fmt(status.duration or 0)}"
                )
            )
        )
        shuffle_icon = SHUFFLE_ON_ICON_REF if status.random else SHUFFLE_OFF_ICON_REF
        right = (
            Flex.row()
            # items
            .push(shuffle_icon.tag(RANDOM_TOGGLE_TAG))
            .push(repeat_icon.tag(REPAT_TOGGLE_TAG))
        )
        await self._sweep.footer_set(
            Container(
                Flex.row()
                .push(Container(left).horizontal(Align.EXPAND), flex=1.0)
                .push(right)
            )
            .face(face="bg=accent/.4")
            .horizontal(Align.EXPAND)
        )

    async def _updater_coro(self) -> None:
        try:
            while True:
                event = await self._events_queue.get()
                if self._view == MPDSweepView.PLAYLIST:
                    if event not in {
                        MPDEvent.PLAYER,
                        MPDEvent.PLAYLIST,
                        MPDEvent.OPTIONS,
                    }:
                        continue
                    await self._update_footer()
                    await self.view_playlist()
        except Exception:
            await self._sweep.terminate()
            traceback.print_exc()

    async def _field_resolver(self, ref: int) -> Field:
        song = self._mpd.song_by_id(ref)
        if song is None:
            return Field()
        cover = await self._mpd.readpicture(song.file)
        if cover is None:
            return Field()
        view = Flex.col().push(
            Container(Image(cover)).horizontal(Align.CENTER).margins(top=1)
        )
        return Field(view=view)

    async def _playlist_song_delete(self) -> None:
        if self._view != MPDSweepView.PLAYLIST:
            return
        songs = await self._sweep.items_marked() or [await self._sweep.items_current()]
        for song in songs:
            if song is None or song.id is None:
                return
            await self._mpd.delete(song)

    async def _playlist_song_move_up(self) -> None:
        if self._view != MPDSweepView.PLAYLIST:
            return
        song = await self._sweep.items_current()
        if song is None:
            return
        await self._mpd.move(song, -1)

    async def _playlist_song_move_down(self) -> None:
        if self._view != MPDSweepView.PLAYLIST:
            return
        song = await self._sweep.items_current()
        if song is None:
            return
        await self._mpd.move(song, 1)

    async def _goto(self, sweep: Sweep[Song], tag: str) -> None:
        song = await self._sweep.items_current()
        if song is None:
            return None
        selected = await sweep.quick_select(
            [
                Candidate()
                .target_push("Goto ")
                .target_push("a", face="underline,bold")
                .target_push(f"lbum : {song.album}")
                .hotkey_set("a")
                .wrap("album"),
                Candidate()
                .target_push("Goto a")
                .target_push("r", face="underline,bold")
                .target_push(f"tist: {song.artist}")
                .hotkey_set("r")
                .wrap("artist"),
            ],
            prompt="GOTO",
            window_uid="goto",
        )
        if not selected:
            return None
        db = await self._mpd.database()
        match selected[0].value:
            case "album":
                await self.view_songs(
                    db.songs(artist=song.artist or "", album=song.album or ""),
                    song.album,
                )
            case "artist":
                await self.view_songs(db.songs(artist=song.artist or ""), song.artist)
            case _:
                pass


async def main(args: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=__doc__,
    )
    parser.add_argument("--theme", help="sweep theme")
    parser.add_argument("--sweep", help="path to the sweep command")
    parser.add_argument("--tty", help="path to the tty")
    parser.add_argument("--log", help="log file")
    parser.add_argument(
        "--term",
        default="kitty",
        choices=["kitty", "foot", "none"],
        help="terminal window used to show UI",
    )
    parser.add_argument(
        "--no-window", action="store_true", help="do not create new terminal window"
    )
    opts = parser.parse_args(args)

    sweep_args: dict[str, Any] = {"layout": "full"}
    sweep_cmd: list[str] = []
    if opts.term != "none" and opts.tty is None:
        sweep_args.update(tmp_socket=True)
        if opts.term == "kitty":
            sweep_cmd.extend(["kitty", "--class", "org.aslpavel.sweep.mpd"])
        elif opts.term == "foot":
            sweep_cmd.extend(["foot", "--app-id", "org.aslpavel.sweep.mpd"])
    sweep_cmd.extend(shlex.split(opts.sweep) if opts.sweep else sweep_default_cmd())

    async with MPD() as mpd:
        async with Sweep[Song](
            sweep=sweep_cmd,
            scorer="substr",
            tty=opts.tty,
            theme=opts.theme,
            log=opts.log,
            title="MPD Client",
            window_uid="playlist",
            keep_order=True,
            **sweep_args,
        ) as sweep:
            await MPDSweep(mpd, sweep).run()


if __name__ == "__main__":
    asyncio.run(main())
