from pathlib import Path
import re
import xml.etree.ElementTree as ET
from PIL import Image

OUT = Path(__file__).resolve().parent
FOLDER = OUT / 'spark-distinct'
INK, MUTED, LINE, PAPER, CLAY = '#292D28', '#737B70', '#DEE3D7', '#F8F9F3', '#B86C68'
CHOICES = [
    ('flick', 'Flick', 'A calligraphic S. One sweeping gesture.', 'SWEPT RIBBON'),
    ('folio', 'Folio', 'A folded little character, caught mid-thought.', 'SOFT FACETS'),
    ('scout', 'Scout', 'A curious profile with a curled listening space.', 'CURIOUS PROFILE'),
]


def inner(path, prefix):
    content = path.read_text()
    ET.fromstring(content)
    content = re.sub(r'^.*?<svg\b[^>]*>', '', content, count=1, flags=re.S)
    content = re.sub(r'</svg>\s*$', '', content)
    for value in re.findall(r'\bid="([^"]+)"', content):
        content = content.replace(f'id="{value}"', f'id="{prefix}-{value}"')
        content = content.replace(f'url(#{value})', f'url(#{prefix}-{value})')
    return content


def text(x, y, value, size=16, fill=INK, weight=400, extra=''):
    return f'<text x="{x}" y="{y}" font-size="{size}" fill="{fill}" font-weight="{weight}" {extra}>{value}</text>'


def mark(path, x, y, size, prefix, viewbox='0 0 336 336'):
    return f'<svg x="{x}" y="{y}" width="{size}" height="{size}" viewBox="{viewbox}">{inner(path, prefix)}</svg>'


def grid_definitions(prefix):
    return f'''<defs>
<pattern id="{prefix}-minor" width="6" height="6" patternUnits="userSpaceOnUse"><path d="M6 0H0V6" fill="none" stroke="#DCE1D6" stroke-width=".25"/></pattern>
<pattern id="{prefix}-major" width="36" height="36" patternUnits="userSpaceOnUse"><rect width="36" height="36" fill="url(#{prefix}-minor)"/><path d="M36 0H0V36" fill="none" stroke="#C9D1C2" stroke-width=".6"/></pattern>
</defs>'''


def grid_body(path, prefix):
    with Image.open(path.with_suffix('.png')) as image:
        bounds = image.getchannel('A').getbbox()
        scale = 336 / image.width
        x1, y1, x2, y2 = [value * scale for value in bounds]
    return f'''{grid_definitions(prefix)}
<rect width="336" height="336" fill="{PAPER}"/>
<rect width="336" height="336" fill="url(#{prefix}-major)"/>
<path d="M0 168H336M168 0V336" fill="none" stroke="{MUTED}" stroke-width=".65" stroke-dasharray="3 3" opacity=".65"/>
{inner(path, prefix)}
<rect x="{x1:.3f}" y="{y1:.3f}" width="{x2-x1:.3f}" height="{y2-y1:.3f}" fill="none" stroke="{MUTED}" stroke-width=".55" stroke-dasharray="2 3" opacity=".6"/>'''


sheet = [
    '<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="1016" viewBox="0 0 1440 1016" role="img" aria-labelledby="title desc">',
    '<title id="title">Studycall — Spark silhouette studies on a construction grid</title>',
    '<desc id="desc">Three new directions on the same six-unit grid with common centerlines and measured silhouette bounds: a calligraphic ribbon, a folded character, and a curious profile.</desc>',
    f'<rect width="1440" height="1016" fill="{PAPER}"/>',
    '<g font-family="Avenir Next, Avenir, sans-serif">',
    text(64, 57, 'STUDYCALL / SPARK REWORK', 13, MUTED, 500, 'letter-spacing="2"'),
    text(64, 119, 'Spark, with its own silhouette.', 42, INK, 500),
    f'<circle cx="1152" cy="54" r="5" fill="{CLAY}"/>',
    text(1376, 59, 'CLAY ROSE / #B86C68', 12, MUTED, 500, 'letter-spacing="1.2" text-anchor="end"'),
    f'<path d="M64 158H1376" stroke="{LINE}"/>',
]

for col, (filename, title, detail, theme) in enumerate(CHOICES):
    x = 64 + col * 448
    path = FOLDER / f'{filename}.svg'
    sheet.append(text(x, 204, f'{col + 1:02d} / {theme}', 12, MUTED, 500, 'letter-spacing="1.5"'))
    sheet.append(f'<svg x="{x + 34}" y="224" width="348" height="348" viewBox="0 0 336 336">{grid_body(path, f"grid-{filename}")}</svg>')
    sheet.append(text(x + 34, 592, '6u GRID / CENTERLINES / SHAPE BOUNDS', 11, MUTED, 500, 'letter-spacing=".7"'))
    sheet.append(text(x, 641, title, 30, INK, 500))
    sheet.append(text(x, 674, detail, 15, MUTED))
    sheet.append(mark(path, x, 709, 50, f'lockup-{filename}', '40 36 256 264'))
    sheet.append(text(x + 62, 744, 'Studycall', 26, INK, 500, 'letter-spacing="-.7"'))
    sheet.append(mark(path, x + 322, 725, 24, f'icon-{filename}', '40 36 256 264'))
    sheet.append(text(x + 354, 744, '24px', 12, MUTED))

sheet.extend([
    f'<path d="M64 773H1376" stroke="{LINE}"/>',
    mark(OUT / 'refinements/curio-spark.svg', 64, 817, 82, 'previous'),
    text(170, 843, 'Earlier Spark', 17, INK, 500),
    text(170, 870, 'The warmth and curiosity carry forward.', 15, MUTED),
    text(832, 843, 'SHARED CONSTRUCTION GRID', 12, MUTED, 500, 'letter-spacing="1.5"'),
    text(832, 873, 'An expressive cut, a swept crest, a continuous tail.', 16, INK),
    text(832, 900, 'A recognizable outline at small sizes.', 15, MUTED),
    '</g></svg>',
])
content = '\n'.join(sheet)
ET.fromstring(content)
(OUT / 'studycall-spark-distinct.svg').write_text(content)
for name, _, _, _ in CHOICES:
    path = FOLDER / f'{name}.svg'
    grid = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1008" height="1008" viewBox="0 0 336 336" role="img" aria-label="{name} construction grid">
{grid_body(path, f"construction-{name}")}
</svg>'''
    ET.fromstring(grid)
    (FOLDER / f'{name}-grid.svg').write_text(grid)
    export_folder = FOLDER / 'export'
    export_folder.mkdir(exist_ok=True)
    source = path.read_text().replace('viewBox="0 0 336 336"', 'viewBox="40 36 256 264"')
    source = source.replace('width="336" height="336" ', '')
    ET.fromstring(source)
    (export_folder / f'{name}.svg').write_text(source)
print('Created three-direction Spark silhouette sheet.')
