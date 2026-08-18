# Migrating the screens to `symbian-decl-ui`

Handoff document. Written at the end of the stage that unblocked the work, for whoever picks it up
next. `docs/decl-ui.md` is what the layer *is*; this is what remains to be done with it, in order.

## Where this stands

**Done — the key path.** Keys reach widgets. `layout::dispatch_key` walks the tree in pre-order at
the rects the last frame laid out, stopping at the first `Consumed`; the bridge runs it only after
`DeclarativeApp::on_key` declines, so the softkey bar and the app's hatches keep winning. `Screen`
forwards into its footer and content bands (it used to ask only its own bar, which is where every
key died, one band short of the field). What a widget needs beyond the key travels in a `KeyCtx`
— the theme and a `Clipboard` — and the bridge holds the clipboard (`with_clipboard`). Six
integration tests in `crates/symbian-decl-ui/tests/screen.rs` cover it, including paste.

**Done — the comparison harness.** `symbian_preview::Parity`, the prerequisite for every screen
below. It is described in the next section because using it is step one of every remaining task.

**Not started — the apps.** `tg` has one translated screen (`src/chats_decl.rs`) that is a renderer
and nothing else: it handles no keys, owns no state, and its `extra_key`/`softkeys`/`Msg::Select` are
dead code. `home` does not depend on the crate at all. Neither app implements `DeclarativeApp`.

## The harness, first

```rust
let atlases = Atlases::load();
let mut p = Parity::new("parity-out");
atlases.with_themes(|dark, light| {
    for (name, store, selected, theme) in scenes {
        p.check(name, theme,
                |c| render_by_hand(c, store, selected, theme),
                |c| render_declared(c, store, selected, theme));
    }
});
assert_eq!(p.checked(), N);   // a scene that stops being built must fail, not pass quietly
p.finish();                   // prints the report; panics if anything differed
```

`tg/examples/chats_parity.rs` is the worked example: nine scenes, both themes, and a second test
(`every_scene_renders_something_different`) asserting the scene parameters actually reach the render
— nine identical comparisons of one state would pass and prove nothing.

**Two rules that are the whole point.**

1. **Scenes are chosen against branches, not against the look.** The first version of that example
   compared one frame — one store, dark, selection 0, scroll 0 — and reported "identical". A scene
   per branch means: a selection that is not the first row, a scrolled list, an empty list, a loading
   state, a list too short for a scrollbar, a count past a formatting cutoff, and both themes.
2. **The reference side is what ships, not what is correct.** Where the two differ, read the
   difference before adjusting either. Nudging the new side until the numbers agree proves only that
   two things can be made identical.

## Decisions already made

- **The model owns a list's cursor; a field owns its own caret.** `ScrollList::focused` defaults to
  `false`, so navigation stays in `update` and one selection never gets two drivers. A field's text
  lives in the slot table, and the app keeps a handle on it — `TextField::buffer()` — because a key a
  *widget* answers does not rebuild the tree, so there is no `view` in which to read it.
- **Migrate a screen at a time behind an adapter.** An app is one `symbian_ui::App`, so becoming a
  `DeclarativeApp` looks like a big bang. It is not, with a leaf widget wrapping an old screen —
  `draw` calls the old `draw`, `handle_key` calls the old `handle_key`, state behind a `RefCell`. The
  app becomes MVU on day one and screens leave the adapter as they are finished and compared. **This
  widget does not exist yet and is the first thing to write** (in `symbian-decl-ui`, since both apps
  need it).
- **`tg` keeps a thin shell around the bridge.** `Cmd` covers sockets, timers, navigation and
  batching; anything else — KDF, ModPow, file writes — comes out of `take_effects()` for the shell to
  perform, and results go back in through `bridge.send(msg)`. The shell keeps `handle_raw`, the
  driver and the worker. The model is `Store` plus UI state.

## What remains, per screen

Each one is: express it declaratively → add its scenes → make the comparison green → then delete the
old screen, not before.

### `tg` — Chats (`src/chats.rs` → `src/chats_decl.rs`)

The renderer already matches in nine scenes. What is missing is everything else:

- **The clone must go first.** `chats_decl.rs:90` does `store.chats.clone()` per rebuild, and a
  `Chat` carries `Vec<Message>` with inline JPEG previews. Against a 4 MB heap and 200 dialogs this
  is an allocator failure waiting for a real account; the mock store has seven chats and no previews,
  so nothing has noticed. Project to a light row struct (name, preview, time, unread, flags) or share
  an `Rc<[Chat]>`.
- `PAD = 5` and `row_height() = 38` are hardcoded copies of `theme.metrics.pad` / `row_h`
  (`chats_decl.rs:199-205`). The file's own comment records that the first version had `PAD = 4` and
  every line of text was a pixel off.
- **Behaviour that does not exist yet:** selection movement (`update`, clamped against
  `store.chats.len()`); `Key::Call` opening the highlighted chat, which `Softkeys::dispatch` does not
  include; and `LoadMore` on the *hand-written* condition (`chats.rs:29`: last row **and** scrolled to
  the bottom), not the current `extra_key`, which fires on a list that fits the screen and never
  paginates on one that does not.
- Two tests read `l.state.selected` out of `Screen::Chats(l)` (`src/lib.rs:1525`, `:1557`) and the
  back-out path writes it (`:389`). Selection moving into the model rewrites all three.
- **A trap, not a bug:** `chats.rs` asks for the scrollbar gutter with `bar.is_some()` and
  `chats_decl` with `true`. They agree only because `chrome::scrollbar_gutter` ignores that argument
  (`chrome.rs:183`). The `chats-short-no-scrollbar` scene stands in front of it.

### `tg` — Login (`src/login.rs`)

Three `Screen`s with a declarative `TextField` each; the first screen that needs the key path. The
digits filter is already inside the buffer (`TextField::accepting`, and `login.rs`'s `digits_field`),
so it survives a paste. Submit reads the text through `TextField::buffer()`. The "show password" eye
stays on the left softkey. Scenes: each screen empty and filled, masked and revealed, the error line,
`Waiting` with a status, a visible text selection, both themes.

### `tg` — Conversation (`src/conv.rs`)

The transcript stays imperative — bubbles, link runs and media labels are custom drawing, which is
the case `docs/decl-ui.md` says to leave alone — as a leaf `Widget`. The rest is declarative:
`Screen` with the title, the transcript as content, and the composer as `footer` (that band exists
for exactly this shape). Focus between transcript and composer is model state. `Ctrl+C` copying the
highlighted message or focused link stays in the screen, not the field. Scenes: focus in each place,
a focused link, media, wrapped text, the note line, an empty chat, both themes.

### `tg` — Viewer

`symbian_ui::Viewer` is a toolkit widget; wrap it as a leaf. Scenes: loaded, loading, error.

### `home` — Recents, Menu, Settings, Home

`home` needs the dependency first (pinned rev plus the local `[patch]`, as `tg` does), then the
adapter, then screens in this order: **Recents** (a list with a picker, closest to work already
done), **Menu**, **Settings**, **Home** last.

Home and Settings need widgets the catalogue does not have: a **`Grid`** (the home is configurable
columns × rows) and wrappers for `Tabs`, `Toggle`, `Stepper` and `AppPicker` — all of which exist in
`symbian-ui` underneath, so these are shells, not new arithmetic. Scenes: Home (full grid, empty
grid, status bar with and without signal, an unread count); Menu (filter typed, no filter, empty);
Settings (each tab, a toggle both ways, a stepper at its limit); Recents (running, closed history,
the unclosable `(home)` row, CPU measured and unmeasured).

## Verification, every time

1. `cargo test --workspace` in all three repos. Baseline at handoff: **SDK 1103, tg 267, home 97** — and the parity example is
   registered `test = true`, so a plain `cargo test` runs the comparison and a divergence fails the
   build rather than waiting to be asked.
2. `cargo test -p <app> --example <screen>_parity` green, with the scene count asserted.
3. Device build (`../SDK/tools/epoc build .`) and the migrated screen exercised by hand — keyboard,
   softkeys, paste into a field. `epoc logcat` for what it reports.
4. Per stage, one measurement: binary size and allocations per frame. `tests/screen.rs` already
   counts allocations with its own global allocator; "≤ 5% bigger" was the original acceptance
   criterion and nobody has measured it.

## The standing risk

`docs/decl-ui.md` argues against this migration: *"a screen that already works does not need
porting; a working screen rewritten declaratively is a working screen with new bugs in it."* All
eight screens work today. The owner chose to migrate anyway, and what makes that defensible is the
harness: every screen is compared against the one it replaces, in the states that have branches.
Without a scene, a claim of "identical" is worth what the first one turned out to be worth.
