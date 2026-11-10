# Oyl's look on the web

The desktop app defines Oyl's design tokens in
[`crates/oyl-ui/src/theme.rs`](../../crates/oyl-ui/src/theme.rs). The web app
([`apps/web/src/styles/global.css`](../../apps/web/src/styles/global.css)) and
the landing page
([`apps/landing/src/styles/global.css`](../../apps/landing/src/styles/global.css))
mirror them as CSS variables, light and dark. Change a token in all three.

Frosted glass, warm ink, one amber accent. Floating things are near-opaque
sheets with hairline edges; the text being written sits in a calmer card; color
means something (the accent, danger, warning, success) and nothing else is
colored. No heavy shadows.

## Light and dark

`data-theme="light" | "dark"` on `<html>` picks the palette. An inline script
in each page's `<head>` sets it before the first paint from the stored choice
(`localStorage["oyl:appearance"]`: `light`, `dark`, or absent for the system's).
The web app's toggle (sun / moon in the top bar, and Settings, Appearance) and
the landing page's toggle flip it, like the desktop app's. A subtree can force
dark with `data-theme="dark"` (the knowledge graph's scene does).

## Tokens

| `theme.rs` (`Tokens`) | Light | Dark | Web CSS variable | Tailwind (web) |
| --- | --- | --- | --- | --- |
| `veil` | `#F3F1EC` 74% | `#15151A` 72% | `--veil` (80%, over `--canvas`) | `bg-veil`, `.oyl-window` |
| `veil_strong` | `#FBFAF7` 80% | `#111114` 78% | `--veil-strong` | |
| `card` / `card_edge` | white 78% / black 6% | `#0C0C0F` 82% / white 7% | `--card` / `--card-edge` | `.oyl-card` |
| `card_solid` | `#FCFBF9` | `#141417` | `--surface` (cards, inputs: `#FDFCFA` / `#1C1C21`) | `bg-surface` |
| `surface` / `_hover` / `_active` | white 55/75/90% | white 6/10/15% | `--soft` / `--hover` / `--active` (as ink tints, so they work on any sheet) | `bg-soft`, `bg-hover`, `bg-active` |
| `raised` | white 95% | `#222227` 95% | `--raised` | `bg-raised`, `.oyl-glass` |
| `hairline` / `hairline_strong` | black 8% / 15% | white 9% / 17% | `--line` / `--line-strong` | `border-line`, `border-line-strong` |
| `text` / `text_soft` / `text_faint` | `#1D1C1A` 100/66/40% | `#EDECEA` 100/65/38% | `--fg` / `--muted` / `--faint` | `text-fg`, `text-muted`, `text-faint` |
| `accent` / `accent_soft` / `on_accent` | `#AD6500` / 14% / white | `#E9B76A` / 17% / `#1C1407` | `--accent` / `--accent-soft` / `--on-accent` (`--accent-hover` = 88%) | `bg-accent`, `text-accent`, `bg-accent-soft`, `text-on-accent` |
| `danger` (+ `_soft`) | `#C6392F` | `#FF7B72` | `--danger` / `--danger-soft` | `text-danger`, `bg-danger-soft` |
| `warning` (+ `_soft`) | `#9A6200` | `#F0C355` | `--warning` / `--warning-soft` | `text-warning`, `bg-warning-soft` |
| `success` (+ `_soft`) | `#2B7A4B` | `#74D19B` | `--success` / `--success-soft` | `text-success`, `bg-success-soft` |
| `paper` / `paper_dim` | white | `#FCFBF8`, washed | `--paper`, `.oyl-paper` (PDF pages) | `bg-paper` |
| `selection`, `caret` | accent 20% | accent 28% | `--selection`, caret = `--accent` | |
| `radius_s` / `_m` / `_l` | 6 / 10 / 16 px | | `--radius-md` 6, `--radius-lg` and `--radius-xl` 10, `--radius-2xl` and `--radius-3xl` 16 | `rounded-md`, `rounded-lg`/`-xl`, `rounded-2xl` |
| `ui_font` | `.SystemUIFont` | | `--font-sans`: `-apple-system, BlinkMacSystemFont, system-ui, ...` | `font-sans` |
| `mono_font` | Menlo | | `--font-mono`: `Menlo, ui-monospace, ...` | `font-mono` |
| `serif_font` | Charter | | `--font-display`: `Charter, 'Bitstream Charter', 'Sitka Text', Cambria, 'Iowan Old Style', Georgia, serif` | `font-display` |

The landing page uses the same values under the desktop names (`--veil`,
`--surface`, `--hairline`, `--text-soft`, `--accent`, ...).

Where a browser has no desktop to blur, the window sits on `--canvas` (a
warm off-white or near-black) with a faint amber light (`--glow`) behind it,
so the glass still reads as glass.

The knowledge graph is drawn by WebGL, which cannot read CSS variables: its
scene carries the dark tokens as values (`apps/web/src/routes/Graph.tsx`).
Course colors (`apps/web/src/lib/palette.ts`) are muted, dusty tones that do
not compete with the accent.

## Logo and icons

The mark is [`assets/logo.png`](../../assets/logo.png); the macOS app icon is
[`assets/icon/oyl-1024.png`](../../assets/icon/oyl-1024.png). The web copies
are derived from them:

| File | From | Use |
| --- | --- | --- |
| `assets/brand/oyl-logo.png`, `oyl-logo-small.png` | `assets/logo.png` (512 px, 128 px) | The canonical web mark |
| `apps/{web,landing}/public/oyl-logo.png` | `assets/logo.png` at 256 px | Header, guide, wordmarks |
| `apps/landing/public/oyl-logo-512.png` | `assets/logo.png` | Landing hero |
| `apps/{web,landing}/public/favicon.png` | `assets/logo.png`, 64 px square | Favicon |
| `apps/{web,landing}/public/apple-touch-icon.png` | `assets/logo.png` on the light veil, 180 px | iOS home screen |
| `apps/web/public/icon-192.png`, `icon-512.png` | `assets/icon/oyl-1024.png` | Web manifest, MCP server icon |
