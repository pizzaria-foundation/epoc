# The component catalogue

What `symbian-decl-ui` offers a screen, and what each piece owns. One line per component, with the
arithmetic it delegates to and the decision that is easy to get wrong.

For *how* the layer works — the `Node`/`Group` split, the cache, the slot table, the key resolution
order — read [`decl-ui.md`](decl-ui.md) first. This file is the index, not the design.

## The rule every component follows

Eight things, and the eighth is what makes the rest checkable:

1. **Pure arithmetic lives in `symbian-ui`**, `no_std`, with its own tests. A widget that grows its
   own `i32` calculations is a second implementation of the same bugs, arriving later and diverging
   quietly. The widget owns only *where the state lives* and *when the children are built*.
2. One widget per file in `crates/symbian-decl-ui/src/widgets/`.
3. **`content_hash` is never `0`.** Zero means "re-measure every frame" and puts the whole screen's
   subtree on the slow path. It folds in what changes the *size* and nothing else — not focus, not a
   value, not a colour.
4. State that is not the application's goes in the [`SlotTable`](../crates/symbian-decl-ui/src/slot.rs),
   with `begin_group(key)` around any conditional.
5. Tests: measure inside its offer, digest moves when the size would, keys ignored when unfocused,
   `OnKey::refused() == 0` on any screen with a hatch.
6. **Parity against the imperative toolkit** where a component replaces something, with a negative
   control. A parity test whose control does not fire is a constant, not a test.
7. A sheet in `tools/preview` → `docs/screenshots/`.
8. A row in this table.

### The containers were tested by looking, not by pressing

Counted, after a field shipped that nobody could type into:

| | tests | that press a key |
|---|---|---|
| every widget that *is* a control (`Button`, `Switch`, `Select`, `Stepper`, …) | 11–24 each | **yes**, all of them |
| `FieldRow` — a container holding one control | 16 | **0** |
| `ListItem` — a container holding up to two | 14 | **0** |

Both are now covered, and the shape of the tests is the point: **place, then dispatch.**
`dispatch_key` matches a key against the rect a widget was *drawn* at, so a dispatch into an empty
cache reaches nobody — and that failure looks like a dead keypad rather than like a missing layout
pass.

A container test needs three assertions, not one: the focused control answers, an unfocused one
stays quiet, and — where there are two slots — only the focused *end* answers. The middle one is
what stops "the key was taken" from silently coming to mean "by something".

### Two facts about testing that cost real time to learn

**The test atlas has one glyph — and that is sharper than "it cannot see text".**
`symbian_ui::testing::with_theme` loads an atlas containing exactly `'a'`, and every font role in it
is the same face at the same size. So `draw_text` paints *nothing* for most strings: a `Stepper`
(`‹ 4 ›`) or a date field is invisible under it.

But every glyph in that atlas has the same **advance**, so the atlas can still see *geometry* — where
a string ends, how wide a label measures, what a layout does with it. What it cannot see is **ink**.
The distinction matters because a guard written the wrong way round passes: one agent's first
negative control compared inked-pixel *counts* and went green with the two answers swapped, because
the dialog's scrim touches every pixel and swamped the text entirely.

The reliable shape is to compare whole buffers for two **same-length** strings containing no `'a'`,
and require them to differ under `Atlases::load()` and be identical under `testing::with_theme`.

**So typography is proved on the real atlases.** `symbian_preview::Atlases::load()` loads the `.sbf`
files the device links, chained through `WithFallback` the same way.
[`tests/list_item_parity.rs`](../crates/symbian-decl-ui/tests/list_item_parity.rs) is the pattern,
including `the_real_atlas_actually_draws_text` — the guard that fails loudly if the whole file ever
goes back to comparing blank screens.

## Foundation

| component | owns | delegates to | the decision |
|---|---|---|---|
| `Row`, `Column` | axis, gap, padding, alignment | `layout` | `CrossAlign::Stretch` on a list row is load-bearing: without it a 17-pixel line in a 38-pixel band draws ten pixels high |
| `Flow` | wrapping runs — chips, tags, key hints | `symbian_ui::flow::Packer` | flex weights and `justify` do **nothing** here; there is no leftover when a child that does not fit opens a line |
| `Stack` | overlays | `layout` | who is on top owns the keys |
| `Spacer` | fixed or flexible empty space | — | |
| `Divider` | a rule *between sections*, taking space | `paint::separator_for` | not `Group::border_bottom`, which is a rule *belonging to a row* and takes no slot |
| `FocusScope` | one cursor over unlike controls | `symbian_ui::focus::FocusRing` | `stop` vs `fixed`: a heading is not a stop, and a cursor parked on one is a key that does nothing |
| `Gap`, `Pad` | distances named by role | `theme.metrics.space` | the digest holds the **role**, never the resolved pixels |
| `RowHeight` | row heights named by kind | `theme.metrics.row_h` | `RowHeight::Header` *is* `SectionHeader::height`, pinned by a test — a divergence is a list that scrolls a fraction short of its last row |

## Chrome and structure

| component | owns | delegates to | the decision |
|---|---|---|---|
| `Screen` | the three bands and the softkey bar | `chrome::Frame`, `keys::Softkeys` | a softkey's label and its message are one declaration — and with `.out(outbox)` that declaration is the *only* one, so `keys()` is optional rather than a second copy |
| `TitleBar`, `SoftkeyBar` | the bands themselves | `chrome` | the middle slot is not a softkey — it is the D-pad centre wearing a label, and arrives as `Key::Select` |
| `OnKey` | keys the softkey convention does not cover | — | refuses `Select`/`Enter`/`Softkey`/`End`; assert `refused() == 0` |
| `Imperative` | an existing imperative screen, unchanged | the screen | permanent architecture, not scaffolding: express the frame, leave the ink alone |

## Lists

| component | owns | delegates to | the decision |
|---|---|---|---|
| `ScrollList` | which rows are built, where the offset lives | `symbian_ui::list::ListState` | selection is the model's, scroll is the slot's — an offset cannot be computed without a viewport height the model does not know |
| `Grid` | a two-dimensional cursor | `symbian_ui::grid` | `GridEdge` has four names: `Left` on a month's first column means "the previous month" |
| `ListItem` | every shape a row takes | `Text`, `Row`, `Column` | a second line is **two stacked lines**, not three columns — modelled as columns the preview stops short of the timestamp |
| `SectionHeader` | a heading between groups | `paint::band` | not a stop, and shorter than a row, or it reads as one of them |

## Content

| component | owns | delegates to | the decision |
|---|---|---|---|
| `Text` | one or more lines, truncated | `Font::fit`, `Font::wrap` | the whole string is hashed: two messages of the same length that differ late would otherwise share a measured height |
| `Marquee` | a focused line too long for its box | `symbian_ui::marquee::offset` | the phase is the **model's** — a slot cannot advance itself, and the phase is deliberately not in the digest |
| `Icon` | one of twenty drawn glyphs | `symbian_ui::icon` | the width is **asked** of `width_for`, never reconstructed — the `Badge` bug measured half a pill too wide and the symptom was a row truncating early |
| `Tile` | a rounded square with one letter — the icon a row has when it has no icon | `symbian_ui::tile` | **not** an `Avatar`: a circle of initials is a *person*, a square is a *thing you can open*, and the two have different palettes so the same seed is a different colour in each. Squares and centres itself in `draw`, not only in `measure` — under `CrossAlign::Stretch` a row hands it the whole band and `letter_tile` fills what it is given |
| `Avatar`, `Badge` | a round tile, an unread pill | `chrome::avatar`, `chrome::badge` | `Badge` reaches into the line above on purpose; that overlap is the design, which is why `Widget::overflow_visible` exists |

## Form controls

All of these follow one shape. The value comes from the model, the widget owns nothing and changes
nothing, and a press pushes a message that `update` acts on:

```rust
Widget::new(value_from_model)
    .focused(bool)                       // only a focused widget answers a key
    .out(outbox.clone(), Msg::Something) // a value, never a callback
```

| component | owns | delegates to | the decision |
|---|---|---|---|
| `Button` | a label and the message it sends | `keys::Softkeys::dispatch` | it does not match on `Select` — which key fires it is platform trivia that has already been got wrong once |
| `Switch` | the pill and knob only | `symbian_ui::toggle::{switch_track, draw_switch}` | *only* the pill: `ListItem` already owns the band, the label and the margins, with a parity test behind them |
| `Checkbox` / `Checkbox::radio` | a square or a circle | `symbian_ui::tick` | a radio reports "set this", never "toggle this" — a group whose chosen option could be pressed off leaves the model with no value and the user no way back |
| `Stepper` | a bounded count, `‹ 4 ›` | `symbian_ui::stepper` | `Left` at the floor is consumed and reports nothing: "set it to what it already is" is a repaint per keypress for nothing |
| `Slider` | a bounded quantity as a track | `symbian_ui::slider` | an arrow at the end is **consumed**; and it takes `SLIDER_W` unless flexed, because a fixed child that answers with the whole offer eats the label beside it |
| `Select` | a drop-down and its popup | `symbian_ui::select` | the popup is a sibling in a `Stack`, never a child of the row — an ancestor that clips still clips |
| `DateTime` | a date or a time, field by field | `symbian_ui::calendar` | Up/Down move the cursor and Left/Right change the value, the inverse of the native editor — because a field here *is* a `Stepper` and every stepper in this SDK answers Left/Right |
| `TextField`, `TextArea` | a caret over a buffer | `symbian_ui::edit` | the caret is the slot's; it is a consequence of having drawn the field here |
| `SearchField` | a query and its caret, together | `symbian_ui::match_filter` | one buffer, not a `String` in the model beside a caret in a slot — `view` would reassign it every frame and the caret would land at zero mid-word |
| `FieldRow` | a labelled field with a hint or an error | composition | an error **replaces** the hint: 200 pixels of body is four fields, and a stacked line would slide the field the user is standing in off the bottom |

### Every control knows whether it is on the selection band

`chrome::control_colors(theme, selected)` returns `(ground, ink, quiet)` — what is behind the
control, its "on" colour and its "off" colour. All four drawn controls go through it.

They did not, and the consequence was visible rather than theoretical: on `HIGH_CONTRAST`, whose
selection band is white and whose `dim` is *also* white, a focused row's switch became a black dot
floating in nothing. The track had vanished into the band. It was wrong in every palette — `accent`,
`dim` and `bg` are all chosen against the *page* — and only visible in one, which is the argument for
sweeping all five rather than the one that looks right.

One function and not four, for the reason `unread_colors` above it gives: four answers to "what
colour goes on the band" is four chances to disagree, and the disagreement shows as one control
looking wrong beside three that look right.

### The message channel

A widget that reports a *number* takes `fn(i32) -> M`, not `Outbox<i32>` through
[`Outbox::wrapped`](../crates/symbian-decl-ui/src/outbox.rs). `wrapped` allocates an `Rc` **and**
boxes a closure per call, and the call is in `view`, which is rebuilt every frame — four such
controls on a settings screen is eight heap allocations a frame on a 128 MB handset. A tuple-variant
constructor coerces to a `fn` pointer, which is `Copy` and allocates nothing.

## Overlays

Four layers, and the recurring question is *stack or replace*. The answer is always stack, and for
one reason that has nothing to do with pixels: [`crate::slot`] drops a group not entered on a frame,
with everything under it — so replacing the screen behind an overlay reclaims the list's scroll
offset and lands the reader back at the top of a list they were forty rows into.

| component | owns | delegates to | the decision |
|---|---|---|---|
| `Dialog` | a question over the whole screen | `symbian_ui::Modal` | one node, not a pair like `Select`: a closed dialog occupies nothing, so there is no half to leave in a row — `Select` is two nodes because a `ScrollList` row **clips**, not because overlays come in pairs |
| `OptionMenu` | the list that rises from the left softkey | `symbian_ui::menu` | also one node, for a sharper reason — what it leaves behind is the word *"Options"*, and that word already has an owner in `Screen::on_options`, declared beside the message it fires |
| `DetailSheet` | one thing's detail, over the list that named it | `chrome::Frame` | it covers every pixel and is **still** a layer; the screen underneath contributes nothing to the frame, so only the slot argument settles it |
| `Drawer` | which subject the app is on, and the way to another | `symbian_ui::Drawer` | two thirds of the *content band* only — the title bar and softkey bar are not covered, so the screen behind has to be **painted**, not merely remembered |

## Feedback

| component | owns | delegates to | the decision |
|---|---|---|---|
| `ProgressBar` | a known fraction | `symbian_ui::meter` | contains no arithmetic at all — the clamp on a server that oversends, and the floor that keeps one percent from being a hairline nobody sees on this panel, are the toolkit's |
| `Spinner` | something is happening, amount unknown | `symbian_ui::meter` | not a bar at zero: `Content-Length` is optional, and a bar stuck at 0% reads as **broken** rather than as **unknown** — the worse of the two, because the person holding the phone stops waiting |
| `Chip` | a state, as a coloured pill | `symbian_ui::Chip` | the pill only; what this adds is being *measured* beside the rest of the line — and `.selected()`, without which a calm chip on the selection band is a pill-shaped hole |
| `EmptyState` | what a list says when it has nothing in it | composition | a title bar, two softkeys and nothing between them is indistinguishable from a screen that failed to load |
| `Notice`, `Notice::toast` | a line or two at the top of the screen | `paint::band` | the timer is `update`'s, through `Cmd::SetTimer` — a counted phase only advances while *something else* is advancing it, so on a still screen the toast would never leave, and a message that stays for ever looks like a working feature |

`Notice` never takes a key, not even to dismiss itself: the softkey bar owns `Select`
unconditionally, so a notice that claimed it would be claiming a key it can never receive.

## Navigation

| component | owns | delegates to | the decision |
|---|---|---|---|
| `Tabs` | the strip S60 puts under the title bar | `symbian_ui::Tabs` | it takes Left/Right from **everything below it** — correct, because a screen you cannot leave is worse than a control needing another key, but it is why `Slider` grew a `Select` fallback |
| `Card` | a band with a ground of its own | — | it had to be a `Widget`, and that is a hole in `Group`: `background` takes a `Color`, a `view` has no theme, so the only colour it can pass is a literal. `.selected(sel)` is not optional — measured, a default card draws **zero** pixels differing from the `HIGH_CONTRAST` band |
| `Collapsible` | a heading that folds its section away | `use_state_with` | `CollapsibleHead` **takes keys**, unlike `SectionHeader`, which is deliberately not a stop; a closed body returns *nothing*, not a zero-height node |

### There is no `TabView`, and no `DataRow`

Both were in the plan and both were argued out, which is the catalogue working rather than the
catalogue shrinking.

A **`TabView`** has no arithmetic of its own, and could not hold the panels without either building
every tab's subtree each frame or storing a closure per tab — the exact cost `Outbox::wrapped` is
refused for. The index has to live in the model regardless, because `view` runs before dispatch and
it is `view` that picks the panel.

A **`DataRow`** was jQuery Mobile's reflow table, and the reflow is the part that does not port:
a browser viewport varies, `E72_SCREEN` is 320×240 and has no second width. Both its states already
exist on `ListItem` — `.trailing_value` is the un-reflowed line, `.secondary` the reflowed one — and
a row in a `ScrollList` is `RowHeight::Row` by construction, so one that reflowed on its own would be
clipped in silence. It survives as `tests/data_row.rs`, which pins that the two shapes compose.


## Every colour is a colour *on* something

`Ink::Text`, `Ink::Dim`, `Ink::Accent` and `Ink::Error` were all chosen against the **page**. Put one
on a different ground and the choice is void — and on `HIGH_CONTRAST`, where the selection band and
`dim` are both pure white, the caption on the *focused* row is the one that cannot be read.

That defect was found and fixed **four separate times** before it had a name: `chrome::control_colors`
for the drawn controls, `Chip::selected` for the pill, `CardSurface::resolve_on` for the card, and two
helpers in `apps/uigallery` for text. Every fix was correct and local, and the fifth site would have
been found by someone holding the phone.

`symbian_ui::Ground` is those four fixes said once. It is a field on `Theme` — which already reaches
every `draw` in both layers — and it has three values: `Page`, `Band`, `Chrome`.

| who sets it | to what |
|---|---|
| `Group::selection_band(true)` | `Band`, for everything inside |
| `Group::surface(role)` | that role's ground |
| `ScrollList`, on the selected row | `Band`, for that row's subtree only |
| `Card` | its role's ground, or `Band` when the card is `selected` |

The answer it carries was not invented either. Three places had already written it out by hand —
`symbian_ui::drawer`, the parity reference in `compare.rs`, and the declarative side of that same
comparison — and all three say the same thing: **on a band there is one legible ink, and `dim`
collapses into it.**

The proof that it works is that all three of those hand-written cases could be deleted and the
parity comparison still reports `identical`. `compare.rs` used to smuggle a literal
`Ink::Fixed(SELECTED_TEXT)` past the theme because a row is built before it has one; it now says
`Ink::Text` and produces the same bytes.

`Ink::Chrome` and `Ink::Selection` deliberately ignore the ground. They are a caller saying *"I know
what I am on"*, and a ground that overrode them would remove the only way to be explicit.

## The phone's own theme

A sixth palette, derived from the theme the *user* chose. `docs/reference/skinprobe.txt` is the
measurement; this is what came of it.

| piece | what it does |
|---|---|
| `symbian::skin` | `color(table, index)` over `AknsUtils::GetCachedColor`; the `TAknsItemID` table is Rust data with host tests |
| `shim/src/shim_skin.cpp` | one generic accessor, no `TRAP` (`GetCachedColor` cannot Leave), behind `USE_SKIN=1` |
| `Palette::from_seed` | 20 properties from **4 colours and 3 knobs**, using `Surface::raised` and `readable_on` |
| `Palette::check` | the seven legibility predicates, as runtime code rather than as tests |
| `Palette::count`/`at` | the offer *including* the sixth — never `Palette::ALL.len()` in a cycler |

Three things worth knowing before touching it:

**The contrast guard is a theorem, not an assertion.** Every text colour comes from `readable_on`,
whose threshold is 140 — so below it the text is white and the delta is at least 115, at or above it
the text is black and the delta is at least 140. Both clear the 70 `check` wants. A property test
sweeps 2744 seed combinations to check the reasoning rather than trust it.

**The derivation tempers what it is given.** A seed is not a palette. An accent that collides with the
page is pushed away from it, keeping its hue; a fill that lands in `readable_on`'s low-contrast window
is pulled out of it; and furniture further from the page than the built-ins ever go is blended back.
That last one came from a person looking at the handset and saying the selection band was wrong — the
band sat 109 luma above the page where `DARK` uses 72 and `IRC` 51.

**`Palette::ALL` is the built-ins only.** A sixth palette outside a `const` array is one that every
`% ALL.len()` cycler steps over for ever, with no compile error and no symptom but a key that appears
to work. Three cyclers use `count`/`at`, and each has a test that requires the sixth to be *reached*.

## Known gaps, written down rather than remembered

- ~~**No control knew it was on the selection band**~~ — fixed by `chrome::control_colors`; see above.
- **The theme's background bitmaps are not reachable.** `GetCachedBitmap` returns NULL for all four
  background IDs on the E72, `AknsDrawUtils::Background` needs a `CWindowGc` so there is no off-screen
  sampling path, and `CreateMaskedBitmapL` goes through `CApaMaskedBitmap`, which has an unresolved
  panic on this handset. The palette does not need them — the colour table carries hue — but a theme
  that ships bitmaps has no way in.
- **The colour table has no detectable end.** `QsnTextColors` answers every index probed, past the
  last one `AknsConstants.h` documents. "Where does this table stop" cannot be learned by asking.
- **Nobody has checked whether the colours change with the theme.** `skinprobe` is installed; changing
  the theme in Settings and re-running is the whole test, and it needs a person.
- ~~**Softkeys were declared twice**~~ — fixed. A declarative app used to write its bar in
  `DeclarativeApp::keys` (which routed) *and* in `Screen::on_*` (which drew), with nothing checking
  that the two agreed: the exact defect [`keys.rs`](../crates/symbian-decl-ui/src/keys.rs) exists to
  prevent, reappearing one layer up, in this crate's own example app. `Screen::out(outbox)` closes
  it. The old path still works unchanged — `on_key` runs before the tree — which is what made the
  fix landable under eight screens in two other repositories.
- ~~**No `Ink::Error` role**~~ — added, with a per-palette colour, a derivation in `from_seed` and
  a `check` predicate. Old text, kept because the reasoning still applies:  `Ink::Unread` carries an apology for this and `FieldRow` now carries a
  second. An error line has no colour of its own, which means a form cannot be re-themed into one.
- **`ListState` does not know a heading is unselectable.** A screen with headings inside a
  `ScrollList::mixed` has to skip them in its own `update`. When a second screen wants it, the
  arithmetic belongs beside `symbian_ui::focus`.
- **`fmt_stepper` clamps negatives to zero**, so a stepper with a negative range draws `‹ 0 ›` for
  every negative value. The declarative widget holds and reports negatives correctly; only the
  display is wrong. Fixing it moves pixels, so it is recorded rather than done.
- ~~**`chrome::text_field` gives focus no visual weight**~~ — fixed. A focused field now carries an
  accent outline, drawn *on* the band's edge so nothing below it moves by a pixel. It was recorded
  here and then found on the handset before it was acted on, which is the argument for acting on the
  list: the caret was 18 pixels of difference, and the negative control for the new guard prints
  exactly that number.
- **A container that paints a focus cue must ask, not be told.** `Widget::focus_state()` is a
  defaulted method returning `Option<bool>`; the ten widgets that take focus answer it, and
  `FieldRow` uses the *control's* answer for its caption. A duplicated parameter is a trap exactly
  when it carries no information: `FieldRow` has **one** control slot, so its two flags could only
  ever agree and a disagreement *was* the bug — twice, in this SDK's own gallery. `ListItem` has
  **two** (`leading` and `trailing`) and only one can hold the cursor, so its flags carry a real fact
  and it is deliberately left alone. The closure form, `control(|focused| Node)`, was rejected on
  this crate's own precedent: a builder here must not depend on call order, so the closure would have
  to be *stored*, and storing it is a `Box` per field per frame.
- **`UiCache` is not keyed by theme.** A digest holds a `Gap`'s *role*, so a theme that changed
  `Space` between two frames would leave every group holding a size measured against the old
  spacing. Unreachable today: `Metrics::default()` is the only construction of metrics in the tree.
- ~~**A ground has no ink of its own**~~ — fixed by `symbian_ui::Ground`. See below.
- ~~**`Group` has no `surface(role)`**~~ — it does now, and it takes `SurfaceRole`, the same
  vocabulary a `Card` paints. `Card` stays, because it also rounds itself, pads itself and owns a
  cache; it is no longer the *only* way to get a ground that follows the theme.
- **No `Ink` word for "quiet, on this ground".** `Ink::Dim` on a band resolves to the band's full
  ink, because a highlight is one row tall and a second level of emphasis inside it is a hint at the
  contrast the band exists to avoid. That is the right answer for a row and an assumption for
  anything larger. If a card ever grows a two-level layout, this is where it will hurt.
- **The `tg` repository's parity reference is stale.** Fixing the leaf-flex-by-axis bug moved the
  second line of a two-line `ListItem` by about two pixels, so `tg/examples/chats_parity.rs` fails
  until its reference scene is regenerated. Correct movement, stale baseline — but it fails closed,
  which is the right way round.
- ~~**Nothing here has run on a handset**~~ — `apps/uigallery` closed that. Eight pages, six
  palettes, on the E72, including the phone's own theme.

## Verifying a change

```
cargo test --workspace                            # 2024 tests today
cargo run -p symbian-decl-ui --example compare    # "identical" three times, or a diff PNG
cargo test -p symbian-decl-ui --test list_item_parity
cargo run -p preview                              # the design system's sheets, into preview-out/
cargo run -p uigallery --example sheets           # the catalogue, 8 pages x 6 palettes, gallery-out/
```
