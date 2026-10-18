# UI decisions

Each decision with the reason, so a new project can keep or deliberately break it.

## Layout and navigation

| Decision | Why |
|---|---|
| Dockable tabs (egui_dock) with the graph canvas in the main area and tool panels as tabs beside it. | Analysis tools are used together; the user arranges them. No modal workflows. |
| Tab names are single nouns (for example Data, Layout, Subgraph). | Scan in one glance; fits a 30px tab bar. |
| Tab bar is transparent, the active tab takes the `bg-elevated` fill and joins the panel body below it. | The active tab looks attached to its content, so there is no extra border. |
| Separators are invisible at rest (6px wide, transparent) and turn accent-coloured on hover and drag. | Less visual noise; the affordance appears when needed. |
| Panel content is built from a `panel_body` helper that sizes sliders to 50% and combo boxes to 62% of the width (clamped 70 to 260 and 110 to 320). | Controls follow the panel width instead of fixed sizes. |

## Controls

| Control | Spec | Why |
|---|---|---|
| Switch | 32 x 19 track, 16px white knob, accent when on, grey when off, label to the right. | iOS-style on/off is instantly understood; checkboxes are reserved for lists. |
| Segmented control | 26px tall, `well` track, 2px inset thumb in `bg-control-active`, label 12px (600 when selected). One option is always selected. | A visible set of 2 to 4 mutually exclusive modes without opening a menu. |
| Slider | 4px rail, circular handle, trailing fill in accent. | Matches the switch; shows the value as a filled amount. |
| Buttons | `bg-control`, no stroke, radius 7, 13px Medium. | Flat and quiet; hover and press are lightness steps. |
| Section heading | 11.5px SemiBold, `text-secondary`, 8px space above and 2px pulled in below. | Structure without shouting. |
| Inner card | `inner-card-fill`, 1px `inner-card-stroke`, radius 9, padding 10 x 8. | Groups one rule (for example one filter) inside a panel. |

## Behaviour

- **Rules, not magic.** Colour, size and filter choices are explicit rules the user edits (an attribute, an operator, a value). Defaults are sensible but visible.
- **Diagnostics beat silence.** When nothing is drawn, a line says why and shows the ranges so the user can fix the data.
- **Safe on bad input.** Missing or non-numeric values are skipped and counted, never fatal.
- **Everything persists.** Settings save with the project and restore on open; columns that no longer exist are dropped.
- **Export what you see.** PNG and SVG exports draw the same list the canvas draws.
- **Mode-specific controls.** 3D-only options (boxes) appear only in 3D; in 2D they are absent rather than disabled.
- **Speed.** Heavy work (layouts) uses a thread pool (rayon); the canvas redraws from a prepared draw list.

## Canvas

- Background follows the theme; edges are semi-transparent (opacity auto-scales with edge count: 1200 / edges, clamped 0.05 to 0.45) so dense graphs stay readable.
- Selected items are emphasised, everything else fades.
- Nodes draw on top of boxes; boxes draw far to near; faces are filled, edges are stroked.
- Direction can be set per edge from a column in the edge file.

## Not done on purpose

- No icon set, no illustrations, no gradients, no glass blur.
- No tooltips that carry required information.
- No second accent colour.
- No animations beyond the switch.
