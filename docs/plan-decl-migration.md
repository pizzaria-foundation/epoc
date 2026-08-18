# Migrating the screens to `symbian-decl-ui`

Handoff document. Written at the end of the stage that unblocked the work, for whoever picks it up
next. `docs/decl-ui.md` is what the layer *is*; this is what remains to be done with it, in order.

## Where this stands

**Done — the key path.** Keys reach widgets. `layout::dispatch_key` walks the tree in pre-order at
the rects the last frame laid out, stopping at the first `Consumed`; the bridge runs it only after
`DeclarativeApp::on_key` declines, so the softkey bar and the app's hatches keep winning. `Screen`
forwards into its footer and content bands. What a widget needs beyond the key travels in a `KeyCtx`
— the theme and a `Clipboard` — and the bridge holds the clipboard (`with_clipboard`).

**Done — a key never waits for a frame.** A key that arrives with no tree in hand used to be
dropped; it is now answered against a tree built *and placed* on the spot (`layout::place_frame`,
called from the bridge's `handle_key`). This was not a tidy-up. The platform hands the host a whole
*batch* of events and the host draws once at the end of it, so every press after the first press that
changed the model would have reached widgets with no rects and been answered by nobody — a held
direction key would advance a list one row per frame instead of one per press, and no screenshot
would say why. `every_key_in_a_batch_reaches_the_screen` in `tests/imperative.rs` is that case.

**Done — the comparison harness.** `symbian_preview::Parity`, the prerequisite for every screen
below. It is described in the next section because using it is step one of every remaining task.

**Done — the adapter, and the return path a widget needs.** Four pieces, all in `symbian-decl-ui`:

- `widgets::Imperative` — a leaf that draws by calling an old screen's `draw` and answers keys by
  calling its `handle_key`, with its state in an `Rc<RefCell<S>>` the caller owns. This is what makes
  "one screen at a time" possible; `tests/imperative.rs` drives a whole app through it.
- `outbox::Outbox<M>` — a queue from a widget to `update`. A widget answers a key with `Handled` and
  nothing else, which is all the catalogue needs and not enough for a widget that answers with a
  *decision*: an old screen hands back `(Handled, Action)`. The app puts the queue on its model, says
  where it is (`DeclarativeApp::outbox`), and the bridge drains it immediately after the key walk and
  feeds each message through the same path a softkey takes. `Outbox::wrapped(Msg::Chats)` is how a
  screen with its own message enum reaches an app with another.
- `ScrollList::on_move` / `on_edge` — a list that moves its own cursor and reports where it went, and
  reports a navigation key that had nowhere to go. The second is the dialog list's pagination. The
  first bends the decision below about who owns a cursor, and the reason is in `on_move`'s doc: `Left`
  and `Right` are *page* keys, a page is "how many rows fit", and only the layout knows that. One
  owner still: the list moves it, the model records it, `.selected(..)` feeds it back.
- `DeclarativeAppBridge::with_model`, and `Softkeys::map` — the shell's door, for an app whose first
  model comes from the world rather than from a constant, and a bar that can be readdressed into an
  application's message type without a slot going missing.

**Done — `tg` is model-update-view, and its dialog list is declarative.** `tg::mvu` is the whole of
it: the model is the old `App` behind an `Rc<RefCell<..>>`, every screen still written by hand is
reached through `Imperative`, and the chat list is `chats_decl`. `mvu::Shell` is what the hosts run —
`symbian_sim::run(tg::mvu::mock())`, `entry!(tg::mvu::live())` — and it keeps `handle_raw`, because
the driver's completions are not keys and are not the bridge's to route. After a raw event it sends
`Msg::Touched`, which means only "the imperative side ran and the view no longer describes the
model"; the adapter pushes the same message after a key it consumed, and that is what stops backing
out of a conversation from showing a blank screen.

**Measured, per stage, as the plan asks.** Device build, `telegram.exe`: 404,708 bytes before any of
this; **425,568 (+5.2%)** with the adapter, the bridge and the dialog list; **431,488 (+1.4%)** with
the login screens; **435,136 (+0.8%)** with the conversation — **+7.5% in total**. Only the first
stage is past the "≤ 5%" criterion, and it is the one worth reading before anyone trims anything: it
paid for the whole layer at once.
The baseline has `chats.rs` linked and all of `symbian-decl-ui` swept out by `--gc-sections`, because
nothing referenced it; this build has the layout engine, the widget catalogue, the bridge and the
adapter linked *for real* and `chats.rs` swept out instead. It is the cost of the first screen and
almost all of it is the cost of the only screen — the next four add rows to a table that is already
there — the second stage's 5,920 bytes for a whole screen is what that looks like once the table
exists. Idle frames still measure nothing: `an_idle_frame_does_not_re_measure_the_adapter`.

**Done — `tg`'s login screens.** All three, plus the waiting screen, compared in **seventeen** states
by `examples/login_parity.rs` and identical in every one. What the stage needed beyond the screens
themselves:

- **One drawing of a text field.** `chrome::text_field` — the box, the `+`, the mask, the selection
  and the caret — called by `login.rs` *and* by `symbian-decl-ui`'s `TextField`. They drew different
  fields before (a stroked rectangle, a caret in another place), so the declarative login screen could
  never have been compared with the one it replaces. Two drawings of one control is the same defect as
  two routings of one key. The refactor was verified by rendering the three login previews before and
  after: byte-identical.
- **`widgets::Stack`.** The panel is centred in the whole content band and the status line is written
  *over* the bottom of it; as a column the two compete for the axis and the block sits half a line
  high. Layers are what the hand-written screen actually does.
- **`Screen::keep_softkey_band`.** With no connection there is no "Avançar" to offer, and the
  hand-written screen still draws the bar — on S60 it is furniture. Without this the band's seventeen
  pixels went to the content and everything centred in it moved.
- **`TextField::with_buffer`**, and `login::Field` — the field's buffer is an `Rc` the application
  holds. The submit key is a *softkey*, so it is answered by `on_key` and turned into a message, and by
  the time `update` runs there is no widget to read the text from. A slot cannot be reached from
  `update`; a handle can. One buffer serves the declarative screen and the reference.
- **A clipboard on the bridge.** The hand-written screen reached for `symbian_app::SystemClipboard`
  itself; a declarative field pastes through `KeyCtx`. Without `Shell::new` handing one over, the login
  field would have been the one field on the phone that cannot paste — silently. The simulator now gets
  a `MemClipboard`, so paste works there too.

Two behaviours are preserved *and* flagged rather than tidied, both in `login_decl::on_key`: the phone
screen answers an unlabelled right softkey, and the middle key submits even when its label is hidden
for want of a connection. Both are keys doing what the bar does not say, which is what this crate's
`keys` module exists to prevent; both are the original's, and fixing either changes the pixels the
comparison is measuring against. The fix belongs in its own change, with the scenes updated on purpose.

**Done — `tg`'s conversation.** Compared in **thirteen** states and identical in every one. The
transcript and the composer stay hand-written, as the plan says: they are drawn by
`Conversation::draw_transcript` and `draw_composer` — the same functions the shipping screen calls —
inside two leaf widgets that do nothing but place them. What became declarative is the chrome: the
title bar, the two bands and the softkey bar, with the band arithmetic now `Frame::split` plus the
footer's measured height instead of a second copy of it.

Three findings, in order of how much they cost:

- **`Screen`'s footer band was inverted, and had no test at all.** `split_bottom` answers *bottom
  first*; `content_and_footer` read it the other way round, so the footer got the whole panel and the
  content got a strip as tall as the footer wanted. The band was added for "the one screen shape that
  needs it" and this is that screen — the first frame drew a composer over the whole conversation with
  one chat bubble beneath it. Fixed, with four tests that pin the geometry rather than the names.
- **A widget can go stale in the *description*, not just in its own state.** The bridge does not
  rebuild the view for a key a widget consumed, which is right for a caret and wrong for the first
  character typed into this composer: the "Enviar" label lives in the tree. `conv_decl::ViewState`
  lists everything `view` reads out of the conversation — the note and whether the composer is empty —
  and the transcript widget compares it across a key. Anything added to that view has to be added
  there too. The comparison harness *cannot* find this: it renders one state and builds a fresh tree
  for it. `examples/preview.rs` found it, because it presses keys and then draws, which is the order
  the device uses.
- **The red key stopped closing the application**, on every screen that had left the adapter, because
  the global `End` arm lived in `App::on_key`. It is `Tg::on_key`'s first arm now. The login code
  screen is what makes it sharp: its left softkey is "Voltar", so the *back* slot is empty and `End`
  fell through to a field that ignores it.

Also removed on the way past: `App::paint` cloned the entire chat — message window, inline JPEGs and
all — on every frame of a conversation, to satisfy a borrow that splits perfectly well by field.

**Not started.** `tg`'s viewer runs through the adapter, unchanged. `home` does not depend on the
crate at all.

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

  **Amended by the first screen, and the amendment is small.** The model still *owns* the cursor; it
  no longer *moves* it. Moving one correctly needs the viewport — `Left` and `Right` page by a
  screenful, and how many rows fit is a layout fact an `update` would have to guess at, which is the
  same objection `slot.rs` makes about scroll offsets. So the list moves it and reports where it went
  (`ScrollList::on_move`), and `update` records that. There is still exactly one thing changing the
  value, which is what the decision was protecting.
- **Migrate a screen at a time behind an adapter.** An app is one `symbian_ui::App`, so becoming a
  `DeclarativeApp` looks like a big bang. It is not, with a leaf widget wrapping an old screen —
  `draw` calls the old `draw`, `handle_key` calls the old `handle_key`, state behind a `RefCell`. The
  app becomes MVU on day one and screens leave the adapter as they are finished and compared. **This
  is `widgets::Imperative`, and it is written** — with the return path an adapter turned out to need
  as well (`Outbox`), which is the one thing writing it taught: a widget that answers a key with a
  *decision* had nowhere to put it.
- **`tg` keeps a thin shell around the bridge.** `Cmd` covers sockets, timers, navigation and
  batching; anything else — KDF, ModPow, file writes — comes out of `take_effects()` for the shell to
  perform, and results go back in through `bridge.send(msg)`. The shell keeps `handle_raw`, the
  driver and the worker. The model is `Store` plus UI state.

  **`tg::mvu::Shell` exists and keeps `handle_raw`; the rest is the endpoint, not the state today.**
  The model is the whole old `App` — store, login machine, driver and three imperative screens — and
  `update` reaches it through the `RefCell`. `take_effects()` is wired and always empty, because every
  message so far is answered by calling a method on the old app. The split the decision describes
  happens one screen at a time: the effects a screen needs come out of the adapter with it.

## What remains, per screen

Each one is: express it declaratively → add its scenes → make the comparison green → then delete the
old screen, not before.

### `tg` — Chats — **done**

On screen, driven by `tg::mvu`, and compared in nine scenes. What the list was missing, and what it
now does:

- **The clone is gone.** `store.chats.clone()` per rebuild — every `Vec<Message>`, inline JPEGs and
  all — is now a projection to `Rc<[chats_decl::ChatRow]>`: seven small values a row, the initials and
  the avatar tint computed once rather than per visible row per frame.
  `an_inline_photo_does_not_travel_into_the_row` is the test that says so. The projection still runs
  once per view rebuild; caching it on the model behind a dirty flag is available and was not taken,
  because a stale dialog list is a worse bug than a bounded allocation and nothing has measured the
  allocation yet.
- **`PAD` and `row_height()` come from `Metrics`**, the same struct every `Theme` is constructed with,
  rather than from two numbers typed by eye — which is how `PAD = 4` against a real 5 moved every line
  of text one pixel left. They are not read from `theme.metrics`, because a view is built without a
  theme; that leaves an assumption, and `the_metrics_here_are_the_metrics_the_theme_uses` is the guard
  that fails the day a theme carries its own.
- **Selection, `Key::Call`, and `LoadMore`.** The cursor moves through `ScrollList::on_move` and is
  recorded (and clamped) in `update`. `Key::Call` opens the highlighted chat — `Softkeys::dispatch`
  does not include it, and `ChatList::activate` always honoured it. Pagination is
  `ScrollList::on_edge(Edge::Bottom)`: Down with the cursor already on the last row.
  `down_on_the_last_row_is_exactly_the_hand_written_pagination_condition` proves that is the same
  condition as `chats.rs`'s "last row **and** scrolled to the bottom", against list lengths either
  side of the viewport — the offset is derived by `ensure_visible`, which parks the last row on the
  bottom edge whether the content overflows or not.
- **The three sites that read the selection out of the screen** now read `App::chats_selected`; the
  two tests go through `mvu::Shell`, which is what the hosts run.
- **The scrollbar-gutter trap is untouched and still guarded.** `chats.rs` asks with `bar.is_some()`
  and `chats_decl` with `true`; they agree only because `chrome::scrollbar_gutter` ignores that
  argument. The `chats-short-no-scrollbar` scene stands in front of it.

**`chats.rs` was not deleted, against the recipe above, and that is deliberate.** It is the reference
the comparison measures against; deleting it deletes the only evidence that the screen still looks
like the screen. It is unreferenced by the application and referenced by the parity example, and its
header says so. Nothing in it should change again — a reference that moves to agree with what it is
checking is not a reference.

### `tg` — Login — **done**

Three screens and the waiting screen, on screen and compared in seventeen states. `login.rs` stays for
the same reason `chats.rs` does: it is the reference the comparison measures against, and it is now
also the home of the login *machine*, which never moved — `login_decl` owns a description of what is
drawn and nothing else.

The digits filter did survive the translation, and it survives a paste, because it was already inside
the buffer (`TextField::accepting`) rather than in front of it. The test that says so is in `mvu`:
pasting `+55 21 99999-0000` into the phone field leaves `5521999990000`.

The one inconsistency found and deliberately kept: the code screen's "Voltar" clears the number, while
every other route back to the phone screen pre-fills it from the last one used. That is the original's
behaviour, a pixel comparison cannot see it, and it is written down in `Login::back_to_phone` for
whoever decides to fix it on both paths at once.

### `tg` — Conversation — **done**

Thirteen scenes, compared and identical: focus in each half, an older message selected, a focused
link, media, wrapped text, the note line, an empty chat, text in the composer with focus in the
transcript, and both palettes over the two scenes with the most colour in them.

Two things about this one are worth knowing before the next screen.

**The comparison lives in `src/conv_decl.rs`, not in `examples/`.** This screen's state cannot be
assembled from outside the crate — the screen enum is private, and a link cursor is reached by walking
the transcript with keys — so an example would have needed a public constructor per scene. Inventing
API so that a test can exist is how a test starts deciding what the code looks like. It runs under
`cargo test` like the other two and writes the same pictures.

**The action key is routed by the application, by focus.** `Screen` offers a key to its softkey bar
before its content, so a labelled action can never reach the transcript — leaving it unclaimed would
make Select do nothing at all whenever there was text in the composer. So `conv_decl::on_key` claims
it and sends `Activate` or `Send` depending on which half has focus, and *what* the activation means
is still `Conversation::activate`. That function lost its font in the process: the note it used to
write — `abrindo [🖼 47 KB]…`, built by `media_label`, which asks the atlas which glyphs it has — was
never visible, because every path that follows it reports again within the same keypress. `update` has
no theme by design, and this is the first place that mattered.

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

1. `cargo test --workspace` in all three repos. Baseline at handoff: SDK 1103, tg 267, home 97. After
   the adapter, the dialog list, the login screens and the conversation: **SDK 1154, tg 297, home 97** — and the parity example is
   registered `test = true`, so a plain `cargo test` runs the comparison and a divergence fails the
   build rather than waiting to be asked.
2. `cargo test -p <app> --example <screen>_parity` green, with the scene count asserted.
3. Device build (`../SDK/tools/epoc build .`) and the migrated screen exercised by hand — keyboard,
   softkeys, paste into a field. `epoc logcat` for what it reports.
4. Per stage, one measurement: binary size and allocations per frame. `tests/screen.rs` already
   counts allocations with its own global allocator; "≤ 5% bigger" was the original acceptance
   criterion and the first stage came in at **+5.2%** — see the measurement note above for what is in
   that number before anyone reacts to it.

## The standing risk

`docs/decl-ui.md` argues against this migration: *"a screen that already works does not need
porting; a working screen rewritten declaratively is a working screen with new bugs in it."* All
eight screens work today. The owner chose to migrate anyway, and what makes that defensible is the
harness: every screen is compared against the one it replaces, in the states that have branches.
Without a scene, a claim of "identical" is worth what the first one turned out to be worth.

**What these screens have not had: a phone.** The device build is green and the binary is measured,
and every behaviour is asserted on the host — the cursor, the green key, pagination, leaving a
conversation, a batch of keys, typing and pasting into the number field. None of that is the same as
the list under a thumb. Step 3 of the verification above is owed on both migrated screens: install it,
hold Down through the end of the list, open a chat with the green key, type a number, paste one, reveal
a password with the left softkey, walk a transcript onto a link and press Select, type a message and
send it, and read `epoc logcat`. What to watch for is timing rather than
layout, because timing is the thing the host cannot show — every path that changes the model now drops
and rebuilds the screen description, and the adapter rebuilds it after every key it consumes.
