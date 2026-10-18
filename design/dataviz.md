# Data visualisation

## Categorical palette

Twenty hand-picked colours (Tableau 10 first, then the D3 category20 style set), tokens `cat-01` to `cat-20`. From the 21st category on, colours come from golden-ratio hue steps so neighbours stay different:

```
hue = 0.13 + k * 0.618034        (k = index - 20)
saturation = 0.55 + 0.25 * frac(k * 0.37)
value = 0.75 + 0.2 * frac(k * 0.53)
```

Items with no value use `missing-value` (#80848c).

## Sequential and diverging palettes

Colour is interpolated linearly between these stops, `t` from 0 to 1. A log-like curve `ln(1 + 50 t) / ln(51)` can compress skewed data.

| Palette | Kind | Stops (low to high) |
|---|---|---|
| Viridis | sequential, default | 440154 472d7b 3b528b 2c728e 21918c 28ae80 5ec962 addc30 fde725 |
| Plasma | sequential | 0d0887 46039f 7201a8 9c179e bd3786 d8576b ed7953 fb9f3a fdca26 f0f921 |
| Magma | sequential | 000004 140e36 3b0f70 641a80 8c2981 b73779 de4968 f7705c fe9f6d fecf92 fcfdbf |
| Yellow-Orange-Red | sequential | ffffcc ffeda0 fed976 feb24c fd8d3c fc4e2a e31a1c bd0026 800026 |
| Blues | sequential | f7fbff deebf7 c6dbef 9ecae1 6baed6 4292c6 2171b5 08519c 08306b |
| Cool-Warm | diverging | 3b4cc0 7b9ff9 c0d4f5 f2cbb7 ee8468 b40426 |

Rule of thumb: Viridis for numbers, categorical for groups, Cool-Warm only when there is a meaningful midpoint.

## Graph defaults

- Default node colour `#569cd6` (`node-default`).
- Edge colour is the text colour at low opacity; edge width 1.0 (0.4 to 5.0 range); a highlighted edge is 2.4 times wider.
- Edge opacity default 0.35, auto-scaled for large graphs.

## 3D view

- Camera: orbit, Z up, field of view 0.7 rad, yaw, pitch and distance; orbit by dragging.
- Framing is robust: far or placeholder boxes are ignored when fitting the camera.
- Near-plane clipping, so objects the camera is inside of still draw.
- Layouts and node dragging are off while real coordinates are used.

## Bounding boxes

- One box per node, built from min and max columns for each axis, same units as the node coordinates.
- A rule decides which nodes get a box; colour by one colour or by an attribute (categorical or sequential).
- Opacity slider for fills; separate outline width and opacity; fill of a box covering most of the scene is dropped (outline only).
- Boxes draw back to front. "Show nodes and edges" can hide the graph to see boxes alone.
- A box with a missing value is skipped and counted in the info line.
