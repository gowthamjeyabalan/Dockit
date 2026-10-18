# Porting

## To egui (Rust)

The whole theme is one file, `theme.rs`, shipped in the downloadable bundle as `egui/theme.rs`. It needs `egui` and optionally `egui_dock`, plus the three Inter font files embedded with `include_bytes!`.

```rust
// main.rs / app setup
mod theme;
theme::apply(&cc.egui_ctx);          // fonts, text styles, spacing, visuals

// in a panel
use theme::UiExt;
theme::heading(ui, "APPEARANCE");
ui.switch(&mut on, "Show nodes and edges");
ui.segmented(&mut mode, &[(Mode::A, "2D"), (Mode::B, "3D")]);
theme::inner_card().show(ui, |ui| { /* one rule */ });
ui.label(theme::secondary("Hint text"));
```

Palette constants (RGB, from `theme.rs`):

| Constant | RGB |
|---|---|
| BG | 28, 28, 31 |
| BG_ELEVATED | 38, 38, 42 |
| BG_CONTROL | 56, 56, 60 |
| BG_CONTROL_HOVER | 70, 70, 75 |
| BG_CONTROL_ACTIVE | 84, 84, 90 |
| HAIRLINE | 52, 52, 57 |
| TEXT | 240, 240, 245 |
| TEXT_SECONDARY | 152, 152, 162 |
| ACCENT | 10, 132, 255 |
| ACCENT_HOVER | 64, 156, 255 |
| DANGER | 255, 105, 97 |

Key recipe:

- Register Inter Regular as the first Proportional face and keep egui's built-in fonts behind it as a fallback for symbols and arrows. Add two named families, `medium` and `semibold`, each starting with its Inter face.
- Text styles: Small 11, Body 13, Button 13 (medium), Heading 16 (semibold), Monospace 12.5.
- Widgets: all four states use radius 7 and no stroke. `inactive` is BG_CONTROL, hover BG_CONTROL_HOVER, active and open BG_CONTROL_ACTIVE. `noninteractive` stroke is HAIRLINE.
- Selection fill is ACCENT at 55% opacity; slider trailing fill on; handle shape Circle; striped rows off.
- Window radius 12, menu radius 10, window shadow offset 10, blur 30, black at alpha 110; popup offset 6, blur 20, alpha 100.
- Dock: tab bar 30px, transparent, hairline underline; separators 6px transparent, accent at 60% on hover, accent when dragged; tab body BG_ELEVATED with 10px bottom corners.

## To the web (CSS)

`tokens.css` gives every colour, spacing and radius as a variable. Minimal setup:

```css
@import url("tokens.css");
body { background: var(--bg-app); color: var(--text); font: 13px/18px var(--font-sans); }
.card { background: var(--bg-elevated); border-radius: var(--radius-panel); padding: var(--pad-card-y) var(--pad-card-x); }
.btn { background: var(--bg-control); border: 0; border-radius: var(--radius-control); padding: var(--pad-button-y) var(--pad-button-x); font-weight: 500; color: var(--text); }
.btn:hover { background: var(--bg-control-hover); }
.hint { color: var(--text-secondary); }
```

Set `data-theme="light"` on a wrapper for the derived light theme.

## Checklist for a new project

1. Copy `theme.rs` (or `tokens.css`) and the three Inter files.
2. Keep one accent. Pick data colours from the palettes in Data visualisation.
3. Reuse the switch and segmented control before inventing a new control.
4. Add a one-line diagnostic wherever a result can be empty.
5. Use the logo files in Logos; to make a new project's mark, keep the 10-unit round stroke and one blue dot.
