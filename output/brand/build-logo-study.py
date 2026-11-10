from base64 import b64encode
from pathlib import Path
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
OUT = Path(__file__).resolve().parent
CLAY = '#B86C68'
INK = '#292D28'
MUTED = '#6F786D'
GRID = '#DCE1D6'
MAJOR = '#C9D1C2'
PAPER = '#F8F9F3'
CENTERS = [60, 96, 132, 168, 204, 240, 276]
FINAL_HEIGHTS = [36, 78, 126, 162, 114, 72, 30]
FINAL_AXIS = [174, 168, 168, 162, 168, 168, 174]


def number(value):
    return f'{value:.3f}'.rstrip('0').rstrip('.')


def geometry(heights, axes, bend, ease_ends=False):
    rows = []
    for x, height, axis in zip(CENTERS, heights, axes):
        top, bottom = axis - height / 2, axis + height / 2
        offset = bend * height / 162
        if ease_ends and x in (CENTERS[0], CENTERS[-1]):
            offset *= .4
        points = [x, top, x + offset, top + height / 3,
                  x - offset, bottom - height / 3, x, bottom]
        p = list(map(number, points))
        path = f'M{p[0]} {p[1]}C{p[2]} {p[3]} {p[4]} {p[5]} {p[6]} {p[7]}'
        rows.append((path, points))
    return rows


def paths(rows, color=CLAY, weight=18):
    body = ''.join(f'<path d="{path}"/>' for path, _ in rows)
    return f'<g fill="none" stroke="{color}" stroke-width="{weight}" stroke-linecap="round">{body}</g>'


def text(x, y, content, size=14, fill=INK, weight=400, extra=''):
    return f'<text x="{x}" y="{y}" font-size="{size}" fill="{fill}" font-weight="{weight}" {extra}>{content}</text>'


def horizontal(y, x1=64, x2=1376):
    return f'<path d="M{x1} {y}H{x2}" stroke="{GRID}"/>'


def placed_logo(body, x, y, width, height):
    return f'<svg x="{x}" y="{y}" width="{width}" height="{height}" viewBox="40 60 256 204">{body}</svg>'


final = geometry(FINAL_HEIGHTS, FINAL_AXIS, 24, ease_ends=True)
small = geometry(FINAL_HEIGHTS, FINAL_AXIS, 18, ease_ends=True)
measured = geometry([36, 78, 126, 162, 126, 78, 36], [168] * 7, 12)
original_b64 = b64encode((ROOT / 'output/imagegen/studycall-handdrawn-logo-v1.png').read_bytes()).decode()
original = f'<image href="data:image/png;base64,{original_b64}" width="1254" height="1254"/>'

for name, rows, weight, viewbox in [('cadence', final, 18, '40 60 256 204'), ('cadence-small', small, 22, '46 66 244 192'), ('measured', measured, 18, '40 60 256 204')]:
    logo = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="{viewbox}" role="img" aria-labelledby="title desc">
<title id="title">Studycall — {name}</title>
<desc id="desc">Seven flowing strokes in clay rose. A shared S curve, round ends, and modular spacing form an organic voice waveform.</desc>
{paths(rows, weight=weight)}
</svg>
'''
    (OUT / f'studycall-logo-{name}.svg').write_text(logo)

definitions = f'''<defs>
  <pattern id="minor" width="6" height="6" patternUnits="userSpaceOnUse"><path d="M6 0H0V6" fill="none" stroke="{GRID}" stroke-width=".25"/></pattern>
  <pattern id="major" width="36" height="36" patternUnits="userSpaceOnUse"><rect width="36" height="36" fill="url(#minor)"/><path d="M36 0H0V36" fill="none" stroke="{MAJOR}" stroke-width=".6"/></pattern>
</defs>'''

guides = [f'<rect x="0" y="0" width="336" height="336" fill="url(#major)"/>',
          f'<path d="M0 168H336M168 0V336" stroke="{MUTED}" stroke-width=".55" stroke-dasharray="3 3" opacity=".6"/>',
          f'<path d="M30 81V243M26 81H34M26 243H34" fill="none" stroke="{MUTED}" stroke-width=".6"/>',
          text(23, 167, '27u', 7, MUTED, extra='text-anchor="middle" transform="rotate(-90 23 167)"'),
          f'<path d="M60 54H96M60 50V58M96 50V58" fill="none" stroke="{MUTED}" stroke-width=".6"/>',
          text(78, 47, '6u pitch', 7, MUTED, extra='text-anchor="middle"')]
guides.append(paths(final))
for _, p in final:
    x1, y1, _, _, _, _, x2, y2 = p
    guides.append(f'<g fill="none" stroke="{INK}" stroke-width=".45" opacity=".5"><circle cx="{x1}" cy="{y1}" r="9"/><circle cx="{x2}" cy="{y2}" r="9"/></g>')

_, p = final[3]
x1, y1, cx1, cy1, cx2, cy2, x2, y2 = p
guides.append(f'<path d="M{x1} {y1}L{cx1} {cy1}M{x2} {y2}L{cx2} {cy2}" stroke="{INK}" stroke-width=".65" opacity=".75"/>')
guides.append(f'<g fill="{PAPER}" stroke="{INK}" stroke-width=".7"><circle cx="{cx1}" cy="{cy1}" r="1.8"/><circle cx="{cx2}" cy="{cy2}" r="1.8"/></g>')
guides.append(f'<path d="M177 81H214M177 243H214" stroke="{MUTED}" stroke-width=".6"/>')
guides.append(text(219, 83, 'r = 1.5u', 7, MUTED))
guides.append(text(219, 245, 'round ends', 7, MUTED))
guides.append(text(168, 315, '6-UNIT CONSTRUCTION GRID', 6.5, MUTED, extra='text-anchor="middle" letter-spacing="1"'))

sheet = [f'<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="1140" viewBox="0 0 1440 1140" role="img" aria-labelledby="title desc">',
         '<title id="title">Studycall logo construction and refinement</title>',
         '<desc id="desc">The original waveform, a measured iteration, and the refined Cadence mark. The final mark uses a 6-unit grid, 18-unit round strokes, 36-unit spacing, a common S curve, and a gently asymmetric envelope.</desc>',
         definitions,
         f'<rect width="1440" height="1140" fill="{PAPER}"/>',
         '<g font-family="Avenir Next, Avenir, sans-serif">',
         text(64, 57, 'STUDYCALL / SYMBOL STUDY', 13, MUTED, 500, 'letter-spacing="2"'),
         text(64, 117, 'A voice, with a little rhythm.', 40, INK, 500),
         f'<circle cx="1185" cy="57" r="6" fill="{CLAY}"/>',
         text(1202, 61, 'CLAY ROSE', 12, MUTED, 500, 'letter-spacing="1.5"'),
         text(1376, 118, '#B86C68', 16, MUTED, extra='text-anchor="end"'),
         horizontal(148),
         text(64, 185, 'CADENCE / CONSTRUCTION', 13, MUTED, 500, 'letter-spacing="1.5"'),
         text(832, 185, 'THE ITERATIONS', 13, MUTED, 500, 'letter-spacing="1.5"'),
         '<g transform="translate(64 208) scale(2)">', *guides, '</g>',
         '<svg x="848" y="218" width="220" height="175" viewBox="144 242 967 771">', original, '</svg>',
         text(1112, 260, 'Original', 24, INK, 500),
         text(1112, 292, 'Hand-drawn character.', 15, MUTED),
         text(1112, 316, 'Loose spacing and weight.', 15, MUTED),
         horizontal(401, 832, 1376),
         placed_logo(paths(measured), 848, 418, 220, 175),
         text(1112, 459, 'Measured', 24, INK, 500),
         text(1112, 491, 'A consistent module.', 15, MUTED),
         text(1112, 515, 'Calm, even proportions.', 15, MUTED),
         horizontal(602, 832, 1376),
         placed_logo(paths(final), 848, 618, 220, 175),
         text(1112, 660, 'Cadence', 24, INK, 500),
         text(1112, 692, 'A shared flowing bend.', 15, MUTED),
         text(1112, 716, 'A fuller lead. A softer echo.', 15, MUTED),
         horizontal(803, 832, 1376),
         text(832, 851, 'The center rises by 1u.', 16, INK),
         text(832, 877, 'The right shoulder resolves a little sooner.', 15, MUTED),
         text(64, 916, 'u = 6', 15, INK, 500),
         text(150, 916, '3u ink', 15, MUTED),
         text(260, 916, '3u air', 15, MUTED),
         text(370, 916, '6u pitch', 15, MUTED),
         text(510, 916, 'One curve family', 15, MUTED),
         horizontal(958),
         placed_logo(paths(final), 64, 990, 82, 66),
         text(164, 1040, 'Studycall', 42, INK, 500, 'letter-spacing="-1.5"'),
         text(64, 1102, 'Seven strokes. One shared gesture.', 15, MUTED),
         text(832, 1002, 'SIZE CHECK', 12, MUTED, 500, 'letter-spacing="1.5"')]

for x, size in [(846, 16), (940, 24), (1042, 32), (1160, 48)]:
    if size == 16:
        sheet.append(f'<svg x="{x}" y="{1045 - size / 1.271}" width="{size}" height="{size / 1.271}" viewBox="46 66 244 192">{paths(small, weight=22)}</svg>')
    else:
        sheet.append(placed_logo(paths(final), x, 1045 - size / 1.255, size, size / 1.255))
    sheet.append(text(x + size / 2, 1083, f'{size}px', 13, MUTED, extra='text-anchor="middle"'))

sheet.extend([text(832, 1102, '16px uses a slightly heavier optical cut.', 13, MUTED), '</g>', '</svg>'])
board = '\n'.join(sheet)
ET.fromstring(board)
(OUT / 'studycall-logo-construction.svg').write_text(board)

grid = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1008" height="1008" viewBox="0 0 336 336" role="img" aria-label="Cadence logo construction grid">
{definitions}
<rect width="336" height="336" fill="{PAPER}"/>
<g font-family="Avenir Next, Avenir, sans-serif">{''.join(guides)}</g>
</svg>'''
ET.fromstring(grid)
(OUT / 'studycall-logo-cadence-grid.svg').write_text(grid)

for name, body in [('original', f'<svg x="40" y="60" width="256" height="204" viewBox="144 242 967 771">{original}</svg>'), ('measured', paths(measured))]:
    grid = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1008" height="1008" viewBox="0 0 336 336" role="img" aria-label="{name} waveform on the construction grid">
{definitions}
<rect width="336" height="336" fill="{PAPER}"/>
<rect width="336" height="336" fill="url(#major)"/>
<path d="M0 168H336M168 0V336" stroke="{MUTED}" stroke-width=".55" stroke-dasharray="3 3" opacity=".6"/>
{body}
</svg>'''
    ET.fromstring(grid)
    (OUT / f'studycall-logo-{name}-grid.svg').write_text(grid)
print('Generated construction sheet, three grids, two vector iterations and an optical small-size cut.')
