# Grapht

Grapht is a Windows desktop tool (Rust, egui) that turns node and edge CSV files into a graph you can look at and analyse. Its design is a quiet dark interface in the macOS dark-appearance style: neutral greys, one blue, Inter type, rounded controls, and almost no decoration. This system records every choice so a future project can look like a sibling.

## Principles

1. **The data is the colour.** The interface is grey so that the nodes, edges and boxes carry all the colour. The only chrome colour is the blue accent.
2. **One accent.** Blue (`accent`) means "on", "selected", "active" and nothing else. Warnings are yellow text, errors are red text, and neither gets a filled banner.
3. **Quiet by default.** Section titles, hints and units are small and in `text-secondary`. Emphasis comes from weight (500, 600), not from size or colour.
4. **Progressive disclosure.** Advanced options sit behind a switch or a segmented control and only appear when relevant (box options only in the 3D view, filter rules only once a rule is added).
5. **Dock, don't dialog.** Panels are tabs in a dockable area. Users can drag, split and resize; dialogs are for file choices only.
6. **Tell the user what happened.** When a result is empty or surprising, a single secondary-grey line says why (`696 boxes drawn`, `nothing matches`, `3 rows had no value`).
7. **Small and fast.** One typeface family, no icon font, no images in the interface, no animation except the 150 ms switch slide.

## Content fundamentals

- Sentence case everywhere: "Show nodes and edges", never "Show Nodes And Edges".
- Labels are nouns or short verb phrases. Switches read as the state they turn on ("Show nodes and edges").
- Hints are one short sentence in `text-secondary`, no trailing full stop on labels, full stop on sentences.
- Counts use correct plurals (`1 box`, `696 boxes`).
- Numbers: group nothing, trim trailing zeros, keep up to two decimals in tables and info lines.
- Voice: plain and factual. No exclamation marks, no "Oops".
- Examples: `Layouts and dragging nodes are off while coordinates are used.` and `Some boxes are far from the nodes (x: 12 to 40 vs 279530 to 279746).`

## Visual foundations

- **Colour.** Dark ground `bg-app`, one step lighter for cards and the active tab (`bg-elevated`), controls lighter again (`bg-control` then hover then active). Hairlines are `hairline`. See Colors for every token and Brand colours below.
- **Brand colours.** Hub Blue `#0A84FF`, Night `#14161C`, Paper `#F5F5F7`. Blue on Night is the app icon; Paper strokes on Night are the logo on dark.
- **Type.** Inter 13px body, 500 for buttons, 600 for headings. Manrope (600 to 800) only for the wordmark and brand surfaces such as READMEs and slides.
- **Shape.** Radii step 6, 7, 8, 9, 10, 12 from the smallest nested part to the largest container. A nested radius is always smaller than its parent.
- **Depth.** Surfaces are separated by lightness first, hairlines second and shadows last. Only floating windows and popups have shadows.
- **Spacing.** Controls sit 8px apart horizontally and 7px vertically. Containers pad 10 to 14px.
- **Motion.** Only the switch animates (egui `animate_bool_responsive`). Panels never slide.
- **Focus and selection.** Selection fill is the accent at 55% opacity with a white 1px stroke; dock overlay selection is the accent at 35%.

## Iconography

The interface uses no icon font and no bitmap icons. Symbols come from text glyphs (arrows, chevrons, the close cross) rendered by egui's fallback fonts behind Inter. The only pictorial mark is the Grapht mark: a circle, a hooked line and a blue dot, drawn as 10-unit round strokes. See Logos for usage.

## Where to go next

- Decisions and rationale: *UI decisions*.
- Charts, palettes, 3D and bounding-box defaults: *Data visualisation*.
- Rebuilding this look in egui or on the web: *Porting*.
- Live component specs: the Components tab.

## Using this system

Load `tokens.css` for colours, type and spacing as CSS variables. Set `data-theme="light"` on a root element for the derived light theme (values marked "derived" in Colors were not in the app and were chosen to hold 4.5:1 contrast). Use Inter for interface text and Manrope for brand surfaces. For Rust and egui projects use the code in *Porting*.
