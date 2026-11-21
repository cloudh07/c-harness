# C-Harness brand

The mark is three arcs closing on a core: work from several agents narrowing to one change you
can review and land.

> Review, land and trust what your coding agents ship.

## Files

| File | Use it for |
|---|---|
| `mark-on-dark.svg` | The full mark on dark backgrounds, 48 px and up |
| `mark-on-light.svg` | The full mark on light backgrounds, 48 px and up |
| `mark-compact.svg` | 32 px and below: favicons, tray, tab icons. Paints in `currentColor` |
| `app-icon.svg` | The app icon tile, 48 px and up |
| `app-icon-small.svg` | The app icon tile with the compact mark, 32 px and below |

`crates/ch-app/resources/c-harness.ico` holds both tiles rendered with resvg: the small one at
16, 24 and 32 px, the full one at 48, 64, 128 and 256 px. Re-render it whenever either SVG
changes.

The app draws the same shapes from `crates/ch-app/assets/brand`, one file per tone, because
GPUI paints each SVG in a single color. A test in `crates/ch-app/src/brand.rs` fails if the two
sets drift apart.

## Rules

- Grayscale only. The mark takes no accent or status color, so it never reads as a state.
- Don't rotate, skew, outline or add effects. The opening of the arcs faces right.
- Below 32 px use the compact mark; the three-arc version blurs into a ring.
- Leave clear space of at least a quarter of the mark's width on every side.
- The wordmark is "C-Harness", with the hyphen, set in Sora Bold. The app UI uses Inter or the
  system UI font.

## Colors

| Lane | On dark | On light |
|---|---|---|
| Outer arc | `#F4F4F5` | `#09090B` |
| Middle arc | `#A1A1AA` | `#3F3F46` |
| Inner arc | `#71717A` | `#71717A` |
| Core | `#FFFFFF` | `#09090B` |

These come from `crates/ch-ui`, which also checks that each lane stands out from the one
outside it.
