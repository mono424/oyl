from pathlib import Path
import re
import xml.etree.ElementTree as ET

OUT = Path(__file__).resolve().parent
ALT = OUT / 'alternatives'
CLAY = '#B86C68'
INK = '#292D28'
MUTED = '#737B70'
LINE = '#DEE3D7'
PAPER = '#F8F9F3'

CONCEPTS = [
    ('tangle', 'Tangle', 'Ideas connecting. A little beautifully tangled.', 'CONNECT'),
    ('turn', 'Turn', 'Three voices. One conversation in motion.', 'EXCHANGE'),
    ('listener', 'Listener', 'Two little characters leaning in.', 'COMPANION'),
    ('unfold', 'Unfold', 'A thought opening. A new way through.', 'DISCOVER'),
    ('curio', 'Curio', 'A curious little study sidekick.', 'CHARACTER'),
    ('detour', 'Detour', 'An unexpected route to understanding.', 'RETHINK'),
]


def body(name, prefix):
    svg = (ALT / f'{name}.svg').read_text()
    ET.fromstring(svg)
    svg = re.sub(r'^.*?<svg\b[^>]*>', '', svg, count=1, flags=re.S)
    svg = re.sub(r'</svg>\s*$', '', svg)
    ids = re.findall(r'\bid="([^"]+)"', svg)
    for value in ids:
        svg = svg.replace(f'id="{value}"', f'id="{prefix}-{value}"')
        svg = svg.replace(f'url(#{value})', f'url(#{prefix}-{value})')
        svg = svg.replace(f'href="#{value}"', f'href="#{prefix}-{value}"')
    return svg


def text(x, y, content, size=16, fill=INK, weight=400, extra=''):
    return f'<text x="{x}" y="{y}" font-size="{size}" fill="{fill}" font-weight="{weight}" {extra}>{content}</text>'


sheet = [
    '<svg xmlns="http://www.w3.org/2000/svg" width="1536" height="1264" viewBox="0 0 1536 1264" role="img" aria-labelledby="title desc">',
    '<title id="title">Studycall — six bolder logo alternatives</title>',
    '<desc id="desc">Six original clay rose symbols explore connected ideas, turn taking, companionship, discovery, a curious character, and a new path through a problem.</desc>',
    f'<rect width="1536" height="1264" fill="{PAPER}"/>',
    '<g font-family="Avenir Next, Avenir, sans-serif">',
    text(64, 57, 'STUDYCALL / NEW DIRECTIONS', 13, MUTED, 500, 'letter-spacing="2"'),
    text(64, 119, 'A little further off the grid.', 44, INK, 500),
    text(1472, 61, 'CLAY ROSE / #B86C68', 12, MUTED, 500, 'letter-spacing="1.5" text-anchor="end"'),
    f'<path d="M64 158H1472" stroke="{LINE}"/>',
]

for index, (name, title, description, theme) in enumerate(CONCEPTS):
    x = 64 + (index % 3) * 480
    y = 196 + (index // 3) * 480
    sheet.append(text(x, y + 4, f'{index + 1:02d} / {theme}', 12, MUTED, 500, 'letter-spacing="1.5"'))
    sheet.append(f'<svg x="{x + 72}" y="{y + 10}" width="304" height="304" viewBox="0 0 336 336">{body(name, f"hero-{name}")}</svg>')
    sheet.append(text(x, y + 342, title, 29, INK, 500))
    sheet.append(text(x, y + 372, description, 15, MUTED))
    sheet.append(f'<svg x="{x}" y="{y + 398}" width="38" height="38" viewBox="0 0 336 336">{body(name, f"small-{name}")}</svg>')
    sheet.append(text(x + 47, y + 424, 'Studycall', 23, INK, 500, 'letter-spacing="-.7"'))
    if index < 3:
        sheet.append(f'<path d="M{x} {y + 459}H{x + 448}" stroke="{LINE}"/>')

sheet.extend([
    f'<path d="M64 1172H1472" stroke="{LINE}"/>',
    text(64, 1217, 'Same warmth. More personality. Six different ways to say “let’s figure it out.”', 16, MUTED),
    text(1472, 1217, 'SYMBOL EXPLORATION', 12, MUTED, 500, 'letter-spacing="1.5" text-anchor="end"'),
    '</g>',
    '</svg>',
])
board = '\n'.join(sheet)
ET.fromstring(board)
(OUT / 'studycall-logo-alternatives.svg').write_text(board)
print('Wrote six-concept comparison sheet.')
