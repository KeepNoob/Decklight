# Decklight

> [!CAUTION]
> This repo is a vibe coding toy, not a polished product. Expect bugs, rough edges, and breaking changes.

Decklight is a dual-screen PDF presentation app written in Rust.  
It shows a presenter view (notes/tools) and a projector view (audience slide), with annotation and timing tools.

## Install and Run

### Local run (from repo root)
```powershell
cargo run --bin decklight -- "Example.pdf"
```

### Install as CLI
```powershell
cargo install --path . --force
decklight "Example.pdf"
```

## Core Features and Usage

### 1) Slide Navigation and Dual Screen
- Presenter window: current slide, next slide, notes panel, controls.
- Projector window: clean content view, optional auto-advance badge, unsaved badge.
- Swap displays with `S` (or `Swap Screens` button).

### 2) Notes Handling
- Modes: `Auto`, `PDF annotations`, `Beamer second screen`, `LibreOffice notes`.
- Manual override supports:
  - Axis: vertical/horizontal
  - Notes side: left/right/top/bottom
  - Split: `0%..100%` (for decks without notes, set to `100%` content).

### 3) Text Annotation Editing
- Toggle edit mode: `A`.
- Select note in current slide preview, then:
  - drag to move
  - drag corner handles to resize
  - edit text + metadata (author, color, tags, priority, status)
  - `Apply Transform`, `Revert Transform`, `Update Selected`, `Delete Selected`.
- Persist to PDF with `Ctrl+S` (or `Save` button).

### 4) Highlight / Pen Toolkit
- Toggle highlight mode: `H`.
- Draw directly on current slide preview; hold `Shift` for straight lines.
- Pen presets include color + width; cycle presets with `Q` / `W`.
- Eraser mode: `E`.
- Lasso selection mode: `M`, then:
  - delete selected strokes: `Delete`
  - apply active preset to selected strokes: `K`.
- Real-time highlight overlay is mirrored to projector.

### 5) Laser Pointer
- Toggle persistent laser: `L`.
- Temporary laser: hold `Ctrl` and click/drag on slide.

### 6) Deck Overview and Timing
- Deck overview grid: `D` (navigate with arrows, `Enter` commit, `Esc` cancel).
- Timing breakdown panel: `B`.
- Talk timer reset: `R`.

### 7) Auto-Advance
- Toggle: `T`.
- Configure global interval and per-slide override in UI.
- Active state shows a small `AUTO` badge on projector.

### 8) Unsaved Change Guard
- Unsaved annotations/highlights show `UNSAVED` state.
- Loading another file or closing prompts `Save / Discard / Cancel`.

### 9) Auto Reload
- If source PDF changes on disk, Decklight reloads it and keeps presentation state aligned.

## Default Shortcuts (All Configurable)

| Action ID | Default | Purpose |
|---|---|---|
| `toggle_deck_overview` | `D` | Open/close deck overview |
| `toggle_timing_breakdown` | `B` | Open/close timing breakdown |
| `next` | `ArrowRight` | Next slide |
| `next_page_down` | `PageDown` | Next slide |
| `next_space` | `Space` | Next slide |
| `prev` | `ArrowLeft` | Previous slide |
| `prev_page_up` | `PageUp` | Previous slide |
| `first` | `Home` | First slide |
| `last` | `End` | Last slide |
| `swap_screens` | `S` | Swap presenter/projector roles |
| `toggle_label_mode` | `J` | Page number vs label navigation |
| `toggle_laser` | `L` | Toggle persistent laser |
| `toggle_auto_advance` | `T` | Toggle auto-advance |
| `toggle_highlight` | `H` | Toggle highlight draw mode |
| `toggle_annotation_edit` | `A` | Toggle text annotation edit mode |
| `toggle_projector_fullscreen` | `F` | Toggle projector fullscreen |
| `reset_timer` | `R` | Reset talk timer |
| `goto` | `G` | Jump to page/label |
| `overview_cancel` | `Escape` | Cancel overview selection |
| `overview_commit` | `Enter` | Commit overview selection |
| `overview_up` | `ArrowUp` | Move overview selection up |
| `overview_down` | `ArrowDown` | Move overview selection down |
| `undo_stroke` | `U` | Undo last stroke |
| `redo_stroke` | `Y` | Redo stroke |
| `clear_current_stroke` | `X` | Clear in-progress stroke |
| `clear_slide_highlights` | `C` | Clear session highlights on current slide |
| `duplicate_selected_note` | `N` | Duplicate selected text note |
| `delete_selected_note` | `Backspace` | Delete selected text note |
| `apply_transform` | `P` | Apply selected note transform |
| `revert_transform` | `V` | Revert selected note transform |
| `prev_pen_preset` | `Q` | Previous pen preset |
| `next_pen_preset` | `W` | Next pen preset |
| `toggle_eraser` | `E` | Toggle eraser mode |
| `toggle_lasso_select` | `M` | Toggle lasso mode |
| `delete_selected_strokes` | `Delete` | Delete lasso-selected strokes |
| `apply_preset_to_selected_strokes` | `K` | Restyle selected strokes |

Notes:
- `Ctrl+S` is reserved for save and not configurable.
- Keybindings can be changed in-app (Keybindings section) or in config TOML.

## Configuration

Decklight auto-creates a config file at:
- `ProjectDirs("com", "decklight", "decklight")/config.toml`

Useful config groups:
- `[keybindings].bindings`
- `[pen_toolkit]`
- `[notes_overrides.<document_fingerprint>]`
- `[auto_advance_per_document.<document_fingerprint>]`

## Backend Behavior and Limitations

- Loader path:
  1. `lopdf` backend (full annotation read/write)
  2. PDFium fallback if `lopdf` parse fails
- PDFium fallback is render-first and currently read-only for annotation writes/saves.
- Some malformed PDFs may still render as blank in all engines; repair externally (`qpdf`, Acrobat/Chrome re-save) and reopen.

## Development Commands

```powershell
cargo fmt
cargo check
cargo test -q
```
