# Declarative UI

## The problem

`symbian-ui` gives a screen the hard parts and leaves the assembly to the author. Scrolling,
selection clamping, char-boundary-safe editing and scrollbar geometry are all correct, pure and
unit-tested; placing them on a 320×240 screen and routing keys to them is hand work, repeated per
screen.

That is the right trade for a handful of screens. It stops being the right trade at a hundred,
for two reasons that are not the same:

**The placement arithmetic is identical every time.** Subtract the title bar, subtract the softkey
bar, divide what is left. Written out per screen, it is a dozen lines that are nearly the same and
occasionally not.

**The routing is where the same bug keeps being written.** A screen draws its softkey bar in
`draw` and routes keys in `handle_key`, and nothing connects the two. Both halves can be perfectly
consistent with themselves and still disagree with each other, and the compiler has nothing to say
about it because there is no shared declaration to check.

`symbian-decl-ui` is the layer above. A screen is a tree built with plain method calls, measured
when it changes and drawn every frame.

```rust
Screen::new()
    .title("Recent")
    .content(ScrollList::new(slots, chats.len(), 38)
        .selected(model.selected)
        .row(|i, sel| Node::leaf(Text::new(&names[i]))))
    .on_options("Refresh", Msg::Refresh)
    .on_action("Open", Msg::Open)
    .on_back("Back", Msg::Back)
```

## The shape

```
   model ──update(msg)──► model'
     │                      │
     │                      ▼
     │                  view(model')  ──► Node tree           rebuilt on change
     │                                        │
     │                                        ▼
     │                                   measure_tree   ──► UiCache    cached by content hash
     │                                        │
     │                                        ▼
     │                                   layout_tree    ──► rects      derived, per frame
     │                                        │
     │                                        ▼
     │                                   draw_tree      ──► pixels     never cached
     │
     └── keys ──► Softkeys::dispatch ──► Option<Msg>
                                            │
                                     None = nothing happened:
                                     no update, no rebuild, no repaint
```

Three things persist across frames, and they are deliberately in three different places:

| what | lives in | why there |
|---|---|---|
| application state | the app's `Model` | it is what `update` is for |
| measured sizes | `UiCache`, on the bridge | needs a lifetime longer than a frame |
| caret, scroll offset | `SlotTable`, on the bridge | not application state, must survive a rebuild |

The last two are in the same place for the same reason, and it is worth stating once: **the bridge
is the only thing in this crate that outlives a frame.** So it stores both, runs both their frames,
and owns the meaning of neither — `layout` knows what a cache entry means, the widgets know what
their slots hold.

## Two types, because structure is not decoration

The tree is a `Node`, and a `Node` is one of two things:

```
Node ─┬─ Leaf(Box<dyn Widget>)   draws its own pixels, knows only its own size
      └─ Group(Group)            axis, gap, padding, children — the engine places these
```

`Widget` is the *leaf* trait: measure, draw, handle a key. `Group` owns structure, and it owns it
as data rather than as trait methods.

The split is not tidiness. The cache is slot-indexed, so the layout pass needs a structural walk
with a deterministic subtree size — `Node::slot_count()` — and an enum gives that by construction
where a trait object's `children()` gives it only by convention. A group's axis, gap, padding and
`Length` are container properties; hanging them off every leaf was the first design and it was
wrong, because it left every leaf implementing methods that nothing would ever call on it.

An app author meets `Node` in exactly one place: the return type of `view`, usually as
`Node::leaf(Screen::new()...)`. `Group` they never name at all — `Row` and `Column` build it. One
type in one signature is the whole tax for a tree the engine can actually see into.

### The `Group: Widget` trap

`Group` implements `Widget`. It looks, therefore, as though the two worlds are interchangeable and
a tree could just as well be passed around as a `Box<dyn Widget>`. **It cannot, and the reason is
worth reading before you reach for it**, because everything about the mistake looks fine.

Here is that impl, entire:

```rust
fn draw(&self, c: &mut Canvas, rect: Rect, theme: &Theme) {
    let mut scratch = UiCache::with_capacity(self.slots);   // built here
    let offer = Constraints::tight(rect.width(), rect.height());
    measure_group(self, 0, offer, theme, &mut scratch);
    layout_group(self, 0, rect, &mut scratch);
    draw_group(self, 0, &scratch, c, theme);
}                                                           // and dropped here
```

A cache built and dropped inside one call cannot hold anything between two of them. So a group
reached through `&dyn Widget` re-measures its whole subtree, from nothing, on every single frame.

The trap is what happens when that group is the *root*. Hand the bridge a `Box<dyn Widget>` and the
engine sees one opaque leaf: the persistent `UiCache` ends up holding exactly one entry, and the
entire screen below it falls onto the scratch path. The frame is still correct. Every pixel is in
the right place. The only symptom is that a still screen is doing the work of a changed one, for
ever — and there is nothing in the picture to look at.

That impl exists for the case it is honest about: a group nested inside something that speaks only
`&dyn Widget`, where the alternative is not working at all. It is a compatibility path, its own doc
comment says so, and it is not the one to build on.

**How you would notice.** `DeclarativeAppBridge::measure_calls()`, which `begin_frame` resets, so
it reads as the count for the frame just drawn. On a still screen it is **1** — the root's
deliberate clamp, and nothing else, with two hundred rows cached behind it. A number proportional
to the size of the tree means a subtree has fallen through to the scratch path. `tests/screen.rs`
asserts it, and it is what caught this mistake the first time: the screen looked perfect and the
number did not.

## Softkeys are declared with their messages

This is the one structural fix the layer makes to a defect that actually shipped.

The launcher's task manager drew a bar reading `Sort` in the middle slot and bound its handler to
`Softkey::Middle` — an event S60 never sends. The middle slot is wired to the selection key and
arrives as `Key::Select`. So the label promised one thing and the key did another, and the code was
consistent with itself in both places. Two more screens in this repo had the action on the left
softkey while handling `Select`, which is the same defect with the halves swapped.

Here a softkey is a label *and* the message it sends, declared together:

```rust
.on_action("Open", Msg::Open)   // the label and the binding are one thing
```

You cannot label a key you do not handle, and you cannot handle one you did not label, because
there is only one place to say either. `Softkeys::dispatch` absorbs the platform trivia — `Select`,
`Enter` and `Softkey::Middle` all mean the action; `Softkey::Right` and `End` both mean back — so
no screen has to know it and no screen can get it wrong.

The middle slot is not a softkey. It is the D-pad centre wearing a label.

### Keys the convention does not cover

The three softkeys are not every key. The recent-apps drawer kills an app on `Delete`, the icon
probe cycles on `Left`, a composer wants `Backspace` first. `OnKey` is where those go, and the
order is:

```
  1. the softkey bar          always, and unconditionally
  2. the innermost hatch      then outward, one enclosing scope at a time
  3. the widget itself        a text field's own editing keys
```

**The bar does not win by being asked first — it wins because a hatch cannot bind its keys at
all.** `OnKey::on` refuses `Select`, `Enter`, `Softkey(..)` and `End`, and counts the refusal in
`OnKey::refused()`.

**Assert `refused() == 0` in a test of any screen that uses a hatch.** The refusal is silent by
design — it does not panic, because a panic here is a dead application on a phone whose whole
failure report is a dialog with a number in it. The cost of that choice is that a developer who
binds `Select` sees their handler never fire and has nothing to tell them why; the natural
conclusion is that the crate is broken rather than that it declined them. The counter is the only
thing standing between those two readings, and a counter nobody reads is a comment.
Ordering alone would have been enough to make the bar work today and not enough to keep it
working: an ordering rule is something a later refactor can get backwards, and the failure there is
a screen the user cannot leave. There is no order of evaluation that traps a user, because the
trapping binding does not exist. Which keys those are is asked of `Softkeys` rather than listed, so
the hatch cannot drift away from the convention it is protecting.

**Below the bar, innermost first.** A text field that eats `Backspace` must not have to know what
encloses it. If the outer scope won, every container would have to enumerate the keys its children
might want and carefully avoid them — coupling that grows with the tree.

Nesting is flattening: two hatches around one subtree are one hatch with its bindings in order, so
there is no chain to walk and no second resolution rule to remember.

### Step three, and how a key actually reaches a widget

Steps 1 and 2 both happen inside `DeclarativeApp::on_key`, which turns a key into a message. Step 3
is `layout::dispatch_key`, a pre-order walk of the tree that offers the key to each widget at the
rect the last layout gave it, stopping at the first `Consumed`. The bridge runs it only after
`on_key` has declined — a text field must never swallow a key the bar promised on its label.

Three properties are worth knowing, because each one is load-bearing:

**It reuses the last frame's rects, and a tree that has never been drawn takes no keys.** Rects live
in `UiCache`, whose generation advances only in `draw_frame`, so between two frames every rect is
still there to read. Before the first frame there are none, and the walk answers `Ignored` rather
than asking widgets about positions they have never occupied.

**It does not rebuild the tree.** A key answered by a widget changed slot state — a caret, a scroll
offset — and not the model. The tree in hand is still correct, and rebuilding it would allocate on a
keystroke to produce an identical one. This is also why an app reads a field through
`TextField::buffer()` rather than waiting for the next `view`: no view runs.

**A widget answers only when it is focused**, and focus comes from the model. `TextField::focused`,
`Button::focused`, `ScrollList::focused` and `Grid::focused` all veto otherwise, so the broadcast walk behaves like
focused dispatch without a focus registry. `ScrollList` defaults to **off**: an app that maps Up and
Down in `update` would otherwise have its selection moved twice by one press, once by the message
and once by the list, and the two would part company the first time one of them clamped.

What a widget needs beyond the key travels in a `KeyCtx` — the theme, because `Screen` cannot find
its own content band without it, and a `Clipboard`, because a field cannot paste without one. One
context rather than a parameter per want: the alternative was editing the trait and its four
implementations twice, and again for the third thing.

## What is cached, and what is not

**`measure` is cached. `draw` is not.**

Measuring is where the expensive work is: text metrics, wrapping, and the arithmetic that divides
a row among flexible children. It only changes when the content does, so it is keyed by a
`content_hash` — a digest of exactly those properties that could change a widget's size.

Drawing is not cached, and a cached draw would be a stale screen. At 320×240 a full repaint is
76,800 pixels; tracking dirty regions costs more than it saves until the frame budget is actually
exceeded, and this one is not.

`content_hash` defaults to `0`, which means *always recompute*. That default is the safe one and
the slow one. The alternative — assume nothing changed — produces a screen that silently stops
updating, which is far harder to notice than a screen that is merely slower than it could be.

**Who stores the cache is not who owns its meaning.** The `UiCache` is a field on
`DeclarativeAppBridge`, because the bridge is the only thing in this crate that outlives a frame.
A cache constructed inside `draw_tree` would be born and dead within the same call, every hash
would miss, and `measure` would run on every widget on every frame — deleting the reason it exists.
But the bridge never reads into it: what an entry means, when it is invalidated and when a frame
begins are all `layout`'s. Storage and knowledge are separate questions and they got separate
answers.

## The slot table, honestly

A caret and a scroll offset are not application state — a caret is a consequence of having drawn a
field there last frame — and nobody wants them in the model. But the tree is rebuilt from scratch
every frame, so they cannot live in the widget either, or typing would send the caret back to zero
mid-word.

The slot table holds them. A view asks for state and gets back the state it was given last frame:

```rust
let field = slots.use_state(TextField::new);
```

**Identity is position**, exactly as React's hooks do it, and for the same reason: the alternative
is making every caller invent a unique string, and callers do not, they copy-paste.

Positional identity is only stable while the call order is, and this is where the documentation
must not oversell. Put a `use_state` behind an `if`, and the frame the condition flips, every call
after it shifts by one:

* **When the types differ, it is detected.** The slot is re-initialised rather than reinterpreted,
  and `type_mismatches()` counts it so a test can assert it never happened. It does not panic — a
  panic here is a dead application on a phone whose whole failure report is a dialog with a number
  in it, and a text field that forgot its contents once is a bug you can survive long enough to
  read the counter.
* **When the types are the same, it is undetectable and the state is simply wrong.** The next
  widget silently adopts its neighbour's value. `slot.rs` has a test named
  `an_unkeyed_conditional_hands_the_next_widget_the_wrong_state` that asserts this happens,
  precisely so that nobody can quietly claim otherwise.

`begin_group` is the fix, and it is a real one. A group has a key, ordinals restart inside it, and
a conditional wrapped in its own group cannot shift anything after it. Keys matter most in lists:
twenty chats keyed by position, sorted by most recent, and every row's draft text and caret slides
one row up the screen with the chat that used to be there. Keyed by UID, the state follows the row.

**Nothing accumulates.** A group not entered this frame is dropped at the end of it, with
everything under it. The cost of that rule is that state does not survive a disappearance: hide a
panel for one frame and its scroll position is gone. That is the right trade — state that must
survive being off-screen belongs in the model, where it can be reasoned about.

## Where the scroll offset lives

The plan had a list take both its selection and its scroll offset from the model. Only one of those
belongs there.

**Selection is the app's.** It is what `Cmd::PushScreen(Detail(i))` is made of; `update` moves it
and the view renders it.

**Scroll is the slot's.** A scroll offset cannot be computed without the viewport height, which the
model does not know and should not — it changes when a title bar appears or a softkey label wraps.
An `update` that set `scroll` would be guessing at a number only `draw` can know. So the offset is
derived by `ListState::ensure_visible` from the selection and kept in a slot.

A `ScrollList` builds row widgets only for the rows on screen — about six of a two-hundred-row
list. Building all two hundred would allocate two hundred boxes per frame to draw six.

**A row builder gets no slot table.** The plan's sketch passed one to every row, and it must not:
rows are built during `draw`, so the only identity available to key their slots by is *screen
position*. State keyed that way slides one row up the list every time the list scrolls — the same
defect keyed groups exist to cure, arriving through a different door. Per-row state belongs in the
model, keyed by whatever the row itself is keyed by.

## The grid, and when a widget earns its place in this crate

`Grid` is `ScrollList` with a second axis: `cols` cells across, a cursor that moves in four
directions, and `on_edge` reporting *which* side it ran out of rather than only that it did.

It is worth reading as the worked example of when something belongs here rather than in the app
that wanted it. The rule this crate follows is that an abstraction needs **two callers that already
disagree**, and this one had them: the launcher's home draws a `cols`×`rows` block of shortcuts with
its own `grid_cells` helper and its own four arrow arms, and a calendar's month view is six rows of
seven days with the same cursor and the same edges. Both had to answer the same four questions —
where cell `i` lands, what `Right` does in the last column, what `Down` does out of a full row into
a half-filled one, and how a cursor stays on screen — and the second was about to answer them
differently. One caller would have been an app's widget; two that were already drifting is the
signal.

The split follows `ScrollList`'s exactly, and that is the other half of the lesson. The arithmetic
went to **`symbian_ui::grid`** — pure, `no_std`, thirteen tests — beside `list.rs`, where it is
reachable from a hand-written screen that never touches this crate. What lives here is only *where
the state lives* and *when the cells are built*. A widget that grew its own `i32` calculations would
be a second implementation of the same bugs, arriving later and diverging quietly.

Two things it does that a list does not, both forced by the calendar:

**`Grid::fitted(slots, cols, count, rows)` divides the band by the row count** instead of taking a
cell height. A month view with a constant cell height leaves a strip of background at the bottom of
a 176-pixel band — and, worse, a grid one pixel too tall silently starts scrolling, so the top row
creeps under the title bar as the cursor moves down. Six rows always, whatever February does, because
a grid that changed shape between months makes the whole screen twitch when the user pages.

**Its edges have four names.** A list's `Edge` is `Top | Bottom`, which is enough to ask a server for
another page. `Left` on the first column of a month means "the previous month" and `Right` on the
last means "the next", and a cursor that merely clamped could report neither — nor tell either apart
from a key that did nothing. It is exported as `GridEdge` rather than `Edge` because `widgets::Edge`
is already the list's, and two enums sharing a name in one module is how a caller matches on the
wrong one.

## What is deliberately not here

**No virtual DOM and no diffing.** The screen is 320×240 and draws in a few hundred microseconds;
the tree is a dozen nodes. Diffing a dozen nodes to avoid drawing a dozen nodes costs more than it
saves, and every frame of it would allocate. What is cached is `measure`, because that is where the
cost actually is.

**No retained tree of element objects.** The tree is values, built and dropped. That is what makes
`view` a pure function of the model and what makes a screen testable without a window.

**No touch and no gestures.** There is no touchscreen. One D-pad and a keyboard, dispatched
directly. A hit-testing pass would be dead code with a maintenance cost.

**No closures in widgets.** A softkey holds a *value* — the message it sends — not a callback. A
screen therefore stays a plain description that can be built, compared and tested without running
anything, and every model change still goes through `update`.

**No effects in `update`.** `update` returns a `Cmd` describing what should happen. It cannot reach
the platform, which means it cannot make the one mistake that matters: asking Avkon to exit from
inside an event callback, when the framework owns the loop and expects to be told afterwards.
`Cmd::Exit` becomes the flag `should_exit()` reads.

## What it costs today

Two numbers, measured rather than estimated, so that whoever revisits starts from evidence.

**A still frame allocates 17 times.** Not zero, and deliberately so: a `ScrollList` builds a widget
per visible row, which is about six of them. The alternative is building all two hundred, and the
frame time on this handset is dominated by the blit rather than by the allocator. What matters is
that the number is *constant* — `tests/screen.rs` asserts that twelve consecutive still frames cost
exactly the same, so nothing accumulates. A per-row widget cache keyed by index would take it to
near zero and is a **Phase 7 candidate**, not a defect.

**A still frame measures once.** That one is the root: `Screen::content_hash` returns zero on
purpose, because a screen's size is a function of the offer rather than of any property, and a
constant digest would make a screen handed a different rect keep the old size. Measuring a screen
is a clamp. Everything below it hits the cache — the same test asserts the per-frame measure count
stays at most two while two hundred rows sit behind it.

## What the parity exercise found

The plan's acceptance criterion for the chrome widgets was "visually identical to plain
`symbian-ui`", and for a while nothing checked it. Every test in the crate proved arithmetic —
three bands summing to 240, a measured size inside its offer, a digest moving when a string does —
and not one of them could fail if this layer drew a *correct* screen that was not the *same* screen.

`examples/compare.rs` is that check. It renders one screen — a title bar with a right-hand detail,
twelve chat rows scrolled to a selection, a scrollbar, all three softkeys — twice over, with the
device's real atlases into two 320×240 RGB565 buffers, and compares them byte for byte. One side is
a `Screen` with a `ScrollList`; the other is `Frame::split` by hand, `chrome::title_bar`, a
`ListState` driven directly and a label array. Nothing structural is shared: two independent routes
to the same pixels.

Both scenes are identical today, including through the bridge on a second frame drawn against a
warm cache. What is worth writing down is that **the exercise did not find a single defect in the
thing it was testing.** It found two elsewhere: one in the layout engine, since fixed, and one in
`symbian-ui` that is years old, ships on a phone today and is still open.

### A scrolled list draws its top row over the title bar

924 pixels of it, in this scene. The first partially-visible row is index 3 at a scroll offset
of 137, so its rect begins 23 pixels above the content band — which starts at y=18, under an
18-pixel title bar. The row's name and timestamp are painted straight through the title and its
detail. On a dark theme with a dark bar it reads as a rendering artefact, which is why it survived
years of use.

`ListState::for_visible` is not wrong to hand out that rect. A partially-visible row *is* partly
above the viewport, and a caller that wanted to draw the visible sliver of it needs to know where
the whole row would have gone. The missing piece is that nobody clips: neither the toolkit's own
list drawing nor `ScrollList` wraps the row loop in a `Canvas::enter(band)`, so the sliver and
everything above it are painted alike.

The declarative layer reproduced it exactly, which is why scene A passed while both were wrong.

**Fixed**, and the shape of the fix is the point. The first attempt was a `save`/`clip_to`/`restore`
around `ScrollList`'s row loop — which fixed this list, broke parity with the hand-written reference,
and left everything else alone. Counting the other callers is what settled it: **eight row loops
across the SDK and the launcher with no clip at all** — three in `symbian-ui` itself, two in
`bootctl`, three in the launcher's settings screens — and two in the Telegram client that had each
hand-rolled the same `clip_to` at their own call site. A defect that every caller has to remember not
to have is not a fixed defect; it is a defect with a workaround.

So the clip went into the primitive. `ListState::draw_visible` is `for_visible` with the canvas
passed through and the band clipped for the whole loop:

```rust
state.draw_visible(c, &rows, band, |c, i, row| { .. });
```

The canvas is a parameter of the closure rather than something it captures, because the method needs
it too. `for_visible` stays public and unchanged — a hit test, a measurement and a test all want
geometry without a canvas — with its doc comment now saying plainly that the rect it hands out can
start above the viewport and that a drawing caller wants the other method.

`neither_layer_lets_a_scrolled_row_draw_over_the_title_bar` is the test, and it carries a negative
control: the same scene drawn through the unclipped walk, asserted to bleed. Without it the whole
test would keep passing if the row loop stopped drawing anything at all.

### A row could not centre its text

Before `CrossAlign` existed, a `Group` left every child at the cross-axis size it measured and
anchored it to the start of the line. In a 38-pixel list row that put a 17-pixel line of text at
y=0..17 instead of centred at y=10..27 — every row on the screen drawn ten pixels high. The
horizontal arithmetic agreed with the hand-written row to the pixel from the first run: 5px padding,
a timestamp sized to its own text, a name taking the remainder. Only the cross axis disagreed, and
only in a way that had to be rendered to be seen.

This was defensible in the code that produced it. A line's cross size is what its children measured,
and imposing a size on them is a decision, not a default — the wrong one for a row of icons of
different heights. What was missing was any way to *say* the other thing.

`Group::align(CrossAlign)` is that way, and `.align(CrossAlign::Stretch)` on a list row is what
makes the two routes to an S60 row produce the same pixels rather than two nearly-right answers ten
pixels apart. `a_row_built_out_of_widgets_is_the_hand_written_row_pixel_for_pixel` is the parity
assertion; `the_row_reaches_parity_because_of_its_cross_axis_alignment_and_not_by_luck` is the one
that matters six months from now, because it asserts *both* that stretching gives the child the whole
band and that the default still anchors to the start. Delete the `.align` call as decoration and
that test goes red with a readable message instead of a pixel diff.

### Two habits the file is built on

**A parity test that cannot fail reads as a proof and is a constant.**
`the_comparison_would_notice_if_it_were_lied_to` feeds the comparison a screen with the middle and
right softkey labels transposed — the exact defect `keys.rs` exists to prevent — and requires it to
notice, in the softkey band specifically.

**A fixture can be too easy, and saying so out loud is worth a test.**
`a_name_in_the_scene_is_long_enough_to_be_truncated` computes the room a name actually gets, after
the scrollbar gutter, the padding and the timestamp, and fails if no name overflows it. It did fail,
on its first run, and the scene got a longer name rather than the assertion getting a smaller number.

## Migrating a real screen: the Telegram dialog list

`compare.rs` builds its own scene. A scene written to be compared is a scene written by someone who
already knows what the layer can express, and it agrees a little too readily. The second exercise was
to take a screen that shipped before this crate existed — `tg::chats`, the Telegram dialog list —
translate it, and require the two to be **byte-identical** on the same `Store`.

They are. `tg/examples/chats_parity.rs` renders both and asserts it. Getting there cost six findings,
and five of them are about the translation rather than about the layer.

### The number copied by eye

`PAD` was written as `4`. `theme.metrics.pad` is `5`. Every line of text in the list came out one
pixel to the left — while every avatar landed exactly, because an avatar is sized from the row height
and never touches the constant. **A number copied by eye agrees with the original everywhere it is
not used.** Nothing but a pixel comparison finds this; it is invisible at a glance and it is wrong on
every row.

### Measured wider than drawn

`Badge::measure` returned `text + line_height`; `chrome::badge` draws `(text + 8).max(h)`. The pill
was therefore *measured* about half a pill too wide, and the symptom was not a fat badge — the draw
uses chrome's number, so the badge looked right. The symptom was **the preview truncating a character
early**, because the measured width is what the row divides by. A widget that wraps an imperative
draw function has to ask that function for its size rather than reconstruct it.

### The row is two lines, not three columns

The obvious reading of the hand-written row is `avatar | text column | time-and-badge column`. It is
wrong in a way only the pixels show: nothing in the original constrains the preview to a column, so
it is allowed to run *under* the timestamp. Modelled as two columns the preview stops short.
Modelled as two stacked lines — a line with the name and the time, a line with the preview and the
unread count — it is both faithful and, arguably, what the design always was.

### CSS was the right place to look

Three of the differences closed by adding the analogue of a CSS property rather than by nudging a
number, which is the outcome worth having: the same shape is now expressible for the next screen.

| The original does | Expressed as | CSS |
|---|---|---|
| name at `r.y0 + 3`, preview baseline at `r.y1 - 4` | `Column::justify(MainAlign::SpaceBetween)` + `padding` | `justify-content` |
| an `hline` at the row's bottom, skipped when selected | `Group::border_bottom(ink, inset)` | `border-bottom` |
| a badge that reaches into the line above | `overflow_visible` | `overflow: visible` |

The first two replace anchoring arithmetic with a declaration. Four rects anchored to two different
edges is correct and is also four places to get an inset wrong.

### A box does not clip itself

The last forty pixels were the top two rows of the unread pill, on the two rows that have one.

An unread badge is `small.line_height() + 2` tall. The line it sits in is as tall as the small text
in it — two pixels shorter — because the column above it has already spent the slack, and there is no
arrangement of paddings that makes 33 pixels of content fit in 31. **The hand-written row genuinely
overlaps**: it anchors the pill at `r.y1 - 4 - (lh + 2)` and nothing stops it reaching into the
name's line box. That overlap is the design.

The declarative engine clipped every leaf to its own rect, so the pill lost its top two rows: a flat
lid on a circle, invisible in a glance and forty pixels in a comparison. `Group::overflow_visible`
did not help, because the group was not the thing clipping.

`Widget::overflow_visible` is the fix, and it is CSS's `overflow` declared by the box itself — with
the opposite default. In a browser `visible` is the initial value and a box never clips its own
painting. Here the default clips, because a widget whose draw runs a pixel wide would otherwise eat
its neighbour and the only evidence would be a photograph of a handset. Ancestors' clips still
apply, which is what keeps a row overlapping its own lines and not the title bar.

### The instrument had the loudest bug

`chats_parity.rs` reports differences by band — title, list, softkeys — and the band boundaries were
hardcoded as `26` and `214`. The real values are `18` and `223`. An hour went into investigating a
difference "in the chrome" that was list content wearing the wrong label. **An instrument that
guesses does not fail; it misdirects, which is worse than being silent.** The boundaries now come
from `theme.metrics`, with a comment saying why.

## When not to use this

**A screen that already works does not need porting.** The imperative toolkit is not deprecated
and is not going anywhere — this crate is built *on* it, and every widget here is a shell over
`symbian-ui` code that was already correct. A working screen rewritten declaratively is a working
screen with new bugs in it.

Reach for this layer when a screen is new, when it has more than a handful of pieces to place, or
when its key routing has become the part you are afraid to change. Stay imperative when the screen
is a single custom drawing, when it needs pixel control the widget catalogue does not offer, or
when it is already written and behaving.

The crate earns its place on the screens that do not exist yet. It does not earn it by making the
ones that do exist look consistent.
