from pathlib import Path
import math
import json
import xml.etree.ElementTree as ET

OUT = Path(__file__).resolve().parent
FOLDER = OUT / 'ink-refined'
FOLDER.mkdir(exist_ok=True)
CLAY, PAPER, INK, MUTED, LINE = '#B86C68', '#F8F9F3', '#292D28', '#697565', '#D8DFD2'
GUIDE, HANDLE = '#3C6258', '#798F84'
TAU = math.tau


def n(value):
    return f'{value:.6f}'.rstrip('0').rstrip('.')


def pair(p):
    return ' '.join(n(v) for v in p)


def ellipse_point(center, rx, ry, theta):
    return center[0] + rx * math.cos(theta), center[1] + ry * math.sin(theta)


def ellipse_equation(p, center, rx, ry):
    return ((p[0] - center[0]) / rx) ** 2 + ((p[1] - center[1]) / ry) ** 2 - 1


def angle(p, center, rx, ry):
    return math.atan2((p[1] - center[1]) / ry, (p[0] - center[0]) / rx)


def arc_flags(start, end, center, rx, ry, predicate):
    a, b = angle(start, center, rx, ry), angle(end, center, rx, ry)
    cw = (b - a) % TAU
    if predicate(ellipse_point(center, rx, ry, a + cw / 2)):
        return int(cw > math.pi), 1
    ccw = TAU - cw
    return int(ccw > math.pi), 0


head_center, head_radius = (210, 132), 36
head_start = ellipse_point(head_center, 36, 36, math.radians(-135))
head_end = (210, 168)
back_center, back_radius = (144, 210), 66
tip_radius = 6
virtual_tip = (210 + 24 * math.sqrt(3), 192)
tip_center = (virtual_tip[0] - 12, 192)
tip_upper = (tip_center[0] + 3, 192 - 3 * math.sqrt(3))
tip_lower = (tip_center[0] + 3, 192 + 3 * math.sqrt(3))
shoulder = (216, 192 + (virtual_tip[0] - 216) / math.sqrt(3))
shoulder_handle = (198, shoulder[1] + 18 / math.sqrt(3))
back_handle = (86, 153 + 27 * 18 / 22)

# Circular back -> two tangent Beziers -> lower snout ray -> R6 tip.
# The crest remains bespoke; each join on the crest/back has aligned handles.
outer = (
    'M144 276 C164 276 178 268 184 252 '
    f'C190 236 {pair(shoulder_handle)} {pair(shoulder)} '
    f'L{pair(tip_lower)} A6 6 0 0 0 {pair(tip_upper)} '
    f'L{pair(head_end)} A36 36 0 1 0 {pair(head_start)} '
    'C182 82 168 62 144 56 '
    'C136 54 128 52 126 58 '
    'C124 64 128 68 132 76 '
    'C144 100 126 126 104 153 '
    f'C{pair(back_handle)} 78 190 78 210 '
    'A66 66 0 0 0 144 276 Z'
)

# The carved curl is the difference of two offset ellipses, using their true
# intersection points rather than unrelated control points that merely touch.
curl_outer, curl_outer_r = (128, 219), (18, 27)
curl_inner, curl_inner_r = (143, 225), (15, 21)


def overlap(theta):
    p = ellipse_point(curl_outer, *curl_outer_r, theta)
    return ellipse_equation(p, curl_inner, *curl_inner_r)


roots = []
samples = 720
for i in range(samples):
    lo, hi = TAU * i / samples, TAU * (i + 1) / samples
    if overlap(lo) * overlap(hi) >= 0:
        continue
    for _ in range(60):
        mid = (lo + hi) / 2
        if overlap(lo) * overlap(mid) <= 0:
            hi = mid
        else:
            lo = mid
    roots.append((lo + hi) / 2)
assert len(roots) == 2
intersections = sorted((ellipse_point(curl_outer, *curl_outer_r, t) for t in roots), key=lambda p: p[1])
curl_top, curl_bottom = intersections
outer_flags = arc_flags(curl_top, curl_bottom, curl_outer, *curl_outer_r,
                        lambda p: ellipse_equation(p, curl_inner, *curl_inner_r) > 0)
inner_flags = arc_flags(curl_bottom, curl_top, curl_inner, *curl_inner_r,
                        lambda p: ellipse_equation(p, curl_outer, *curl_outer_r) < 0)
counter = (f'M{pair(curl_top)} A18 27 0 {outer_flags[0]} {outer_flags[1]} {pair(curl_bottom)} '
           f'A15 21 0 {inner_flags[0]} {inner_flags[1]} {pair(curl_top)} Z')


def eye(rx=12, ry=6):
    return f'M{216+rx} 126 A{rx} {ry} 0 1 0 {216-rx} 126 A{rx} {ry} 0 1 0 {216+rx} 126 Z'


def mark_body(small=False, color=CLAY):
    return f'<path fill="{color}" fill-rule="evenodd" d="{outer} {counter} {eye(14,8) if small else eye()}"/>'


for suffix, small in [('', False), ('-small', True)]:
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="60 36 210 258" role="img" aria-labelledby="title desc">
<title id="title">Studycall — Scout Ink refined{' / optical icon' if small else ''}</title>
<desc id="desc">Scout Ink with a circular head, a tangent circular back, a rounded thirty-degree snout, an elliptical eye, and a carved curl made from offset ellipses.</desc>
{mark_body(small)}
</svg>'''
    ET.fromstring(svg)
    (FOLDER / f'scout-ink-refined{suffix}.svg').write_text(svg)

eye_top = 132 - math.sqrt(36**2 - (216-210)**2)
back_at_counter = 144 - math.sqrt(66**2 - (219-210)**2)
base_at_counter = 210 + math.sqrt(66**2 - (128-144)**2)
clearances = {
    'eye_rim': 120 - eye_top,
    'left_wall_at_curl_axis': 110 - back_at_counter,
    'base_wall_at_curl_axis': base_at_counter - 246,
}
metadata = dict(head_center=head_center, head_radius=head_radius, head_start=head_start,
                head_end=head_end, back_center=back_center, back_radius=back_radius,
                nose_angle=30, nose_radius=tip_radius, virtual_tip=virtual_tip,
                nose_center=tip_center, nose_upper=tip_upper, nose_lower=tip_lower,
                shoulder=shoulder, shoulder_handle=shoulder_handle,
                curl_outer=dict(center=curl_outer, radii=curl_outer_r),
                curl_inner=dict(center=curl_inner, radii=curl_inner_r),
                curl_intersections=intersections, eye_center=(216,126), eye_radii=(12,6),
                clearances=clearances)
(FOLDER / 'geometry.json').write_text(json.dumps(metadata, indent=2))


def line(a, b, color=GUIDE, width=.55, extra=''):
    return f'<path d="M{pair(a)}L{pair(b)}" fill="none" stroke="{color}" stroke-width="{width}" {extra}/>'


def cross(center, size=3):
    x,y = center
    return line((x-size,y),(x+size,y), width=.45) + line((x,y-size),(x,y+size), width=.45)


def label(x,y,value,size=7.5,extra=''):
    return f'<text x="{x}" y="{y}" fill="{INK}" font-family="Avenir Next, Avenir, sans-serif" font-size="{size}" {extra}>{value}</text>'


def node(p, anchor=True):
    return f'<circle cx="{n(p[0])}" cy="{n(p[1])}" r="{1.6 if anchor else 1.1}" fill="{GUIDE if anchor else PAPER}" stroke="{GUIDE}" stroke-width=".5"/>'


def tick(a,b,number,callout):
    x1,y1=a; x2,y2=b
    if abs(x1-x2) < 1e-3:
        ticks = line((x1-2.2,y1),(x1+2.2,y1)) + line((x2-2.2,y2),(x2+2.2,y2))
    else:
        ticks = line((x1,y1-2.2),(x1,y1+2.2)) + line((x2,y2-2.2),(x2,y2+2.2))
    cx,cy=callout
    mid=((x1+x2)/2,(y1+y2)/2)
    return (line(a,b,width=.75)+ticks+line(mid,callout,width=.45)+
            f'<circle cx="{cx}" cy="{cy}" r="5" fill="{PAPER}" stroke="{GUIDE}" stroke-width=".5"/>'+
            label(cx,cy+2.3,str(number),6.5,'text-anchor="middle"'))


defs = f'''<defs>
<pattern id="minor" width="6" height="6" patternUnits="userSpaceOnUse"><path d="M6 0H0V6" fill="none" stroke="#DCE1D6" stroke-width=".25"/></pattern>
<pattern id="major" width="36" height="36" patternUnits="userSpaceOnUse"><rect width="36" height="36" fill="url(#minor)"/><path d="M36 0H0V36" fill="none" stroke="#C9D1C2" stroke-width=".6"/></pattern>
</defs>'''
construction = [defs,
    f'<rect width="336" height="336" fill="{PAPER}"/>',
    '<rect width="336" height="336" fill="url(#major)"/>',
    line((0,168),(336,168),width=.45,extra='stroke-dasharray="3 3" opacity=".5"'),
    line((168,0),(168,336),width=.45,extra='stroke-dasharray="3 3" opacity=".5"'),
    mark_body(),
    f'<circle cx="210" cy="132" r="36" fill="none" stroke="{GUIDE}" stroke-width=".55" stroke-dasharray="2 2"/>',
    f'<circle cx="144" cy="210" r="66" fill="none" stroke="{GUIDE}" stroke-width=".55" stroke-dasharray="2 2"/>',
    f'<path d="M210 168 A36 36 0 1 0 {pair(head_start)} M78 210 A66 66 0 0 0 144 276" fill="none" stroke="{GUIDE}" stroke-width=".95"/>',
    cross(head_center), cross(back_center),
    f'<ellipse cx="128" cy="219" rx="18" ry="27" fill="none" stroke="{GUIDE}" stroke-width=".55" stroke-dasharray="2 2"/>',
    f'<ellipse cx="143" cy="225" rx="15" ry="21" fill="none" stroke="{GUIDE}" stroke-width=".55" stroke-dasharray="2 2"/>',
    f'<path d="{counter}" fill="none" stroke="{GUIDE}" stroke-width=".8"/>',
    cross(curl_outer,2), cross(curl_inner,2), node(curl_top), node(curl_bottom),
    f'<ellipse cx="216" cy="126" rx="12" ry="6" fill="none" stroke="{GUIDE}" stroke-width=".6"/>',
    line((194,126),(258,126),width=.4,extra='stroke-dasharray="2 2"'),
    line((246,80),(246,224),width=.45,extra='stroke-dasharray="3 2"'),
    line((204,192),(274,192),width=.45,extra='stroke-dasharray="3 2"'),
    line(head_end,virtual_tip,width=.6), line(virtual_tip,shoulder,width=.6),
    f'<circle cx="{n(tip_center[0])}" cy="192" r="6" fill="none" stroke="{GUIDE}" stroke-width=".6"/>',
    cross(tip_center,1.8), line(tip_center,tip_upper,width=.45), line(tip_center,tip_lower,width=.45),
    node(head_start), node(head_end), node(tip_upper), node(tip_lower),
    line((236,106),(262,88),width=.45), label(266,88,'R36'),
    line((88,244),(49,253),width=.45), label(23,256,'R66'),
    line((228,126),(260,126),width=.45), label(264,129,'2:1 eye'),
    line((128,192),(106,174),width=.45), label(81,171,'2:3'),
    label(264,179,'30°'), label(256,211,'R6'),
]

handles = [
    ((128,68),(132,76),(144,100)),
    ((126,126),(104,153),back_handle),
    ((178,268),(184,252),(190,236)),
    (shoulder_handle,shoulder,tip_lower),
]
for before,anchor,after in handles:
    construction.extend([line(before,after,HANDLE,.45),node(before,False),node(anchor),node(after,False)])
construction.extend([
    tick((216,eye_top),(216,120),1,(265,66)),
    tick((back_at_counter,219),(110,219),2,(47,202)),
    tick((128,246),(128,base_at_counter),3,(200,294)),
    label(168,326,'u = 6 / ACTIVE ARCS SOLID / GUIDES DASHED',6.6,'text-anchor="middle" letter-spacing=".55"'),
])
grid_body='\n'.join(construction)
grid=f'''<svg xmlns="http://www.w3.org/2000/svg" width="1344" height="1344" viewBox="0 0 336 336" role="img" aria-labelledby="title desc">
<title id="title">Scout Ink refined — geometric construction</title>
<desc id="desc">True circular head and back arcs, thirty-degree snout rays with a six-unit fillet, an elliptical eye, intersecting ellipse curl, aligned Bezier handles and measured wall clearances.</desc>
{grid_body}
</svg>'''
ET.fromstring(grid)
(FOLDER / 'scout-ink-construction.svg').write_text(grid)


def text(x,y,value,size=16,color=INK,weight=400,extra=''):
    return f'<text x="{x}" y="{y}" font-size="{size}" fill="{color}" font-weight="{weight}" {extra}>{value}</text>'


def positioned(body,x,y,w,h,viewbox='60 36 210 258'):
    return f'<svg x="{x}" y="{y}" width="{w}" height="{h}" viewBox="{viewbox}">{body}</svg>'


old=(OUT/'scout-drawn/ink.svg').read_text()
old_body=old[old.find('>')+1:old.rfind('</svg>')]
old_body=old_body.replace('id="title"','id="old-title"').replace('id="desc"','id="old-desc"')
sheet=[
    '<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="1180" viewBox="0 0 1440 1180" role="img" aria-labelledby="plate-title plate-desc">',
    '<title id="plate-title">Scout Ink — refined geometry and construction</title>',
    '<desc id="plate-desc">A large annotated construction drawing, before-and-after silhouettes, geometric dimensions, measured clearances and small-size proofs.</desc>',
    f'<rect width="1440" height="1180" fill="{PAPER}"/>',
    '<g font-family="Avenir Next, Avenir, sans-serif">',
    text(64,57,'STUDYCALL / SCOUT INK',13,MUTED,500,'letter-spacing="2"'),
    text(64,119,'Scout Ink, refined.',44,INK,500),
    text(1376,59,'CLAY ROSE / #B86C68',12,MUTED,500,'letter-spacing="1.2" text-anchor="end"'),
    f'<path d="M64 158H1376" stroke="{LINE}"/>',
    text(64,195,'GEOMETRIC CONSTRUCTION',13,MUTED,500,'letter-spacing="1.5"'),
    positioned(grid_body,64,218,736,736,'0 0 336 336'),
    text(896,195,'THE REFINEMENT',13,MUTED,500,'letter-spacing="1.5"'),
    text(934,242,'Earlier Ink',14,MUTED),text(1166,242,'Refined',14,MUTED),
    positioned(old_body,912,258,172,196), positioned(mark_body(),1144,258,172,196),
    f'<path d="M896 490H1376" stroke="{LINE}"/>',
    text(896,534,'CONTROLLING GEOMETRY',12,MUTED,500,'letter-spacing="1.5"'),
    text(896,573,'Head circle',14,MUTED),text(896,604,'R36',27,INK,500),
    text(1150,573,'Back arc',14,MUTED),text(1150,604,'R66',27,INK,500),
    text(896,651,'Eye ellipse',14,MUTED),text(896,682,'24 × 12',27,INK,500),
    text(1150,651,'Snout / tip',14,MUTED),text(1150,682,'30° / R6',27,INK,500),
    f'<path d="M896 716H1376" stroke="{LINE}"/>',
    text(896,757,'MEASURED CLEARANCES',12,MUTED,500,'letter-spacing="1.5"'),
    text(896,797,'1   Eye rim',16),text(1376,797,f'{clearances["eye_rim"]:.1f}',18,INK,500,'text-anchor="end"'),
    text(896,833,'2   Left wall',16),text(1376,833,f'{clearances["left_wall_at_curl_axis"]:.1f}',18,INK,500,'text-anchor="end"'),
    text(896,869,'3   Base wall',16),text(1376,869,f'{clearances["base_wall_at_curl_axis"]:.1f}',18,INK,500,'text-anchor="end"'),
    text(896,911,'Drawing units at the marked guide positions.',13,MUTED),
    text(64,992,'Shared tangents smooth the back and shoulder. The swept crest keeps its own gesture.',16,MUTED),
    f'<path d="M64 1022H1376" stroke="{LINE}"/>',
    positioned(mark_body(),64,1049,70,80),text(157,1105,'Studycall',42,INK,500,'letter-spacing="-1.5"'),
    text(832,1059,'SIZE CHECK',12,MUTED,500,'letter-spacing="1.5"'),
]
for x,size in [(852,16),(954,24),(1066,32),(1198,48)]:
    sheet.extend([positioned(mark_body(size<=24),x,1114-size,size,size),
                  text(x+size/2,1141,f'{size}px',13,MUTED,extra='text-anchor="middle"')])
sheet.extend([text(832,1167,'An enlarged eye keeps the 16–24px optical cut clear.',12,MUTED),'</g></svg>'])
board='\n'.join(sheet)
ET.fromstring(board)
(OUT/'studycall-ink-refined.svg').write_text(board)
print('Generated refined master, optical cut, exact construction drawing and review plate.')
