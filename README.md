# Dockit

A PDF editor for Windows, written in Rust with egui, in the Grapht design system (see `design/`).

- **Edit what is on the page.** Select text, images and shapes; move and resize them; change colours, outline width and fill; retype text; swap images; delete.
- **Add content.** Text, images, rectangles, ellipses and lines.
- **Pages.** Thumbnails you can drag to reorder; rotate, duplicate, insert blank pages or pages from other files, delete, extract.
- **Combine and split.** Join several PDFs into one; split a file every few pages or by page ranges.
- **Review.** Highlight, underline or strike through text; comments; freehand drawing; stamps.
- **Forms and signing.** Fill text fields, checkboxes, option buttons and lists; draw, type or pick an image of your signature and place it, including in signature fields.
- Undo and redo for everything, dark and light appearance, dockable panels.

## Running

```
cargo run --release
```

`pdfium.dll` (in `pdfium/`) must sit next to `dockit.exe`; the build script copies it into `target/<profile>/` for you. A PDF path given on the command line is opened at start.

## How it works

PDFium (Chrome's PDF engine, build 7881 from [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries)) reads, draws and writes files, fills forms and creates annotations. Dockit calls its C API through the raw function table of the `pdfium-render` crate (`src/pdf.rs`).

Changes to content that is already on a page do **not** go through PDFium's page rewriting, which loses things real files rely on (CMYK colours, some font set-ups). Instead `src/content.rs` tokenizes the page's content stream with byte positions, matches each drawing command to the object PDFium shows, and rewrites only the commands of the edited objects. `src/incremental.rs` appends the result as a standard PDF incremental update. New content (text, shapes, images, signatures) is added by PDFium in a separate stream, which leaves the original untouched.

| File | What it does |
|---|---|
| `src/pdf.rs` | Safe layer over PDFium: documents, pages, rendering, objects, annotations, form fields, page operations |
| `src/content.rs` | Content-stream tokenizer, object matching and surgical edits |
| `src/incremental.rs` | Page content and resources via `lopdf`; incremental update writer; image XObjects |
| `src/editor.rs` | The open document, history (whole-file states), selection and every edit |
| `src/canvas.rs` | The page view, tools and on-page editing |
| `src/panels.rs`, `src/dialogs.rs`, `src/app.rs` | Panels, windows and the app shell |
| `src/fonts.rs` | Finds installed fonts that match a PDF font, for text the original font cannot show |
| `src/theme.rs`, `src/mark.rs` | Grapht theme with the derived light mode; the Dockit mark and icon |

## Known limits

- Edited text keeps its original font only when every typed character already appears in that font on the page; otherwise the line is redrawn in the closest installed font (the status line says which).
- Text does not reflow across lines or paragraphs.
- Objects inside a form XObject (a "Group" in Properties) move and resize as one.
- Editing existing content of a password-protected file saves it without protection.
- Pictures stored inline in the page's drawing commands can be moved or deleted but not swapped.
