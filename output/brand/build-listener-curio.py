from pathlib import Path
import re
import xml.etree.ElementTree as ET

OUT = Path(__file__).resolve().parent
FOLDER = OUT / 'refinements'
INK, MUTED, LINE, PAPER, CLAY = '#292D28', '#737B70', '#DEE3D7', '#F8F9F3', '#B86C68'

FAMILIES = [
    ('Listener', 'Two forms. A shared conversation.', 'listener', [
        ('listener-close', 'Close', 'Related curves. A quieter, balanced rhythm.'),
        ('listener-call', 'Dialogue', 'A full voice and a smaller answering voice.'),
        ('listener-pair', 'Friends', 'A little character in each half.'),
    ]),
    ('Curio', 'One curious little companion.', 'curio', [
        ('curio-soft', 'Soft', 'A calm gaze and a gentler stance.'),
        ('curio-spark', 'Spark', 'A lifted crest. A question on its mind.'),
        ('curio-wink', 'Wink', 'A warmer, more mischievous expression.'),
    ]),
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


def mark(path, x, y, size, prefix, compact=False):
    viewbox = '40 48 256 240' if compact else '0 0 336 336'
    return f'<svg x="{x}" y="{y}" width="{size}" height="{size}" viewBox="{viewbox}">{inner(path, prefix)}</svg>'


sheet = [
    '<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="1308" viewBox="0 0 1440 1308" role="img" aria-labelledby="title desc">',
    '<title id="title">Studycall — Listener and Curio refinements</title>',
    '<desc id="desc">Three Listener variations explore balance, dialogue, and a pair of characters. Three Curio variations explore calmness, curiosity, and a playful wink. All use the original clay rose color.</desc>',
    f'<rect width="1440" height="1308" fill="{PAPER}"/>',
    '<g font-family="Avenir Next, Avenir, sans-serif">',
    text(64, 57, 'STUDYCALL / SELECTED DIRECTIONS', 13, MUTED, 500, 'letter-spacing="2"'),
    text(64, 119, 'A little more character.', 44, INK, 500),
    f'<circle cx="1152" cy="54" r="5" fill="{CLAY}"/>',
    text(1376, 59, 'CLAY ROSE / #B86C68', 12, MUTED, 500, 'letter-spacing="1.2" text-anchor="end"'),
    f'<path d="M64 158H1376" stroke="{LINE}"/>',
]

for row, (family, description, seed, choices) in enumerate(FAMILIES):
    top = 198 + row * 500
    sheet.append(text(64, top, family.upper(), 14, INK, 500, 'letter-spacing="2"'))
    sheet.append(text(234, top, description, 15, MUTED))
    sheet.append(mark(OUT / 'alternatives' / f'{seed}.svg', 1230, top - 33, 50, f'seed-{seed}'))
    sheet.append(text(1290, top - 4, 'Original', 13, MUTED))
    for col, (filename, title, detail) in enumerate(choices):
        x = 64 + col * 448
        path = FOLDER / f'{filename}.svg'
        sheet.append(mark(path, x + 56, top + 17, 304, f'hero-{filename}'))
        sheet.append(text(x, top + 347, title, 29, INK, 500))
        sheet.append(text(x, top + 379, detail, 15, MUTED))
        sheet.append(mark(path, x, top + 407, 42, f'lockup-{filename}'))
        sheet.append(text(x + 50, top + 435, 'Studycall', 23, INK, 500, 'letter-spacing="-.7"'))
        sheet.append(mark(path, x + 336, top + 416, 24, f'icon-{filename}', compact=True))
        sheet.append(text(x + 368, top + 434, '24px', 12, MUTED))
    sheet.append(f'<path d="M64 {top + 474}H1376" stroke="{LINE}"/>')

sheet.extend([
    text(64, 1260, 'Two directions, explored through shape and expression.', 16, MUTED),
    text(1376, 1260, 'SIX REFINEMENTS', 12, MUTED, 500, 'letter-spacing="1.5" text-anchor="end"'),
    '</g></svg>',
])
content = '\n'.join(sheet)
ET.fromstring(content)
(OUT / 'studycall-listener-curio-refinements.svg').write_text(content)
export_folder = FOLDER / 'export'
export_folder.mkdir(exist_ok=True)
for family, _, _, choices in FAMILIES:
    for filename, title, _ in choices:
        source = (FOLDER / f'{filename}.svg').read_text()
        source = source.replace('viewBox="0 0 336 336"', 'viewBox="40 48 256 240"')
        target = export_folder / f'{family.lower()}-{title.lower()}.svg'
        target.write_text(source)
print('Created Listener + Curio refinement sheet.')
