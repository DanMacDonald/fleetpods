#!/usr/bin/env python3
"""Rebuild the device alpha masks with Python 3 and rsvg-convert."""

import math
from pathlib import Path
import subprocess

DESTINATION = Path(__file__).resolve().parent
PADDING = 15
SCALE = 4
# The case matrix has a 12-pixel pitch across 633 pixels, displayed at 128.
DOT_PITCH = 12 * 128 / 633
HALO_REACH = 13
HALO_OPACITY = 0.86


def raster(name, width, height, shapes):
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" '
           f'width="{width * SCALE}" height="{height * SCALE}" '
           f'viewBox="{-PADDING} {-PADDING} {width} {height}">'
           + shapes + '</svg>')
    subprocess.run(['rsvg-convert', '-o', str(DESTINATION / name)],
                   input=svg.encode(), check=True)


for kind, width, height, radius in [('phone', 20, 32.5, 5), ('laptop', 35, 22.5, 2.5)]:
    screen_width = width if kind == 'phone' else 30
    screen_height = height if kind == 'phone' else 17.5
    screen_x = (width - screen_width) / 2
    outline = (f'<rect x="{screen_x}" y="0" width="{screen_width}" '
               f'height="{screen_height}" rx="{radius}"')
    surface = outline + ' fill="white"/>'
    strokes = outline + ' fill="none" stroke="white" stroke-width="1.5"/>'
    if kind == 'phone':
        strokes += '<path d="M 8.125 3.125 H 11.875" stroke="white" stroke-width="1.25" stroke-linecap="round"/>'
    else:
        base = '<path d="M 0 20 H 35 L 32.5 22.5 H 2.5 Z" fill="white"/>'
        surface += base
        strokes += base

    dots = ''
    halo_width = width / 2 + HALO_REACH
    halo_height = height / 2 + HALO_REACH
    # The tall phone covers more of the bright center than the laptop screen.
    halo_falloff = 2.4 if kind == 'phone' else 2
    rows = math.ceil(halo_height / DOT_PITCH)
    columns = math.ceil(halo_width / DOT_PITCH)
    for row in range(-rows, rows + 1):
        for column in range(-columns, columns + 1):
            x = width / 2 + column * DOT_PITCH
            y = height / 2 + row * DOT_PITCH
            distance = math.hypot((x - width / 2) / halo_width,
                                  (y - height / 2) / halo_height)
            if distance >= 1:
                continue
            strength = min(1, (1 - distance) * halo_falloff)
            dot_radius = 0.22 + 0.85 * strength
            opacity = HALO_OPACITY * strength
            dots += (f'<circle cx="{x:.3f}" cy="{y:.3f}" r="{dot_radius:.3f}" '
                     f'fill="white" opacity="{opacity:.3f}"/>')
    canvas_width, canvas_height = width + PADDING * 2, height + PADDING * 2
    raster(f'{kind}-screen.png', canvas_width, canvas_height, surface)
    raster(f'{kind}-unselected.png', canvas_width, canvas_height, strokes)
    # The opaque screen and outline cover the center of the selected dot field.
    raster(f'{kind}-selected.png', canvas_width, canvas_height, dots)
