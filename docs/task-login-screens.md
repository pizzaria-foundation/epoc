# Task: the three login screens

A self-contained piece of work for someone who has not seen this repository before.
Everything it depends on exists and is tested; nothing it depends on needs a phone.

---

## What this is

The Telegram client for the Nokia E72 can negotiate a session, stay logged in, read a chat
list and run the whole login exchange — but there is nowhere to **type a phone number**.
This task builds that: three screens and the state that moves between them.

The protocol is done and is not your problem. `tg_proto::auth::Login` is a state machine
that emits requests and consumes replies; you drive it and draw what it says.

## What already exists

| | where | state |
|---|---|---|
| the login state machine | `apps/telegram/proto/src/auth.rs` | done, 9 tests |
| the connection | `apps/telegram/src/link.rs` | done |
| session persistence | `apps/telegram/src/session_store.rs` | done, 8 tests |
| a text field with cursor, insert, backspace | `crates/symbian-ui/src/edit.rs` | done |
| the chat screens this hands off to | `apps/telegram/src/{chats,conv}.rs` | done, on device |
| credentials | `apps/telegram/api.conf` | present, gitignored |

Run `cargo test --workspace` first. 418 tests should pass. If they do not, that is a
different problem and not this one.

## What to build

### `apps/telegram/src/login.rs`, new

Three screens over `symbian_ui::TextField`, and an enum that moves between them:

```
Phone ──code sent──▶ Code ──┬──▶ done, hand off to the chat list
                            └── SESSION_PASSWORD_NEEDED ──▶ Password
```

Each screen is: a title, a `TextField`, a hint line, and an error line. Look at
`apps/telegram/src/chats.rs` for how a screen is laid out here — `draw(&mut self, c: &mut
Canvas, theme: &Theme)` and `handle_key(&mut self, ev: KeyEvent) -> Handled`. Follow it
rather than inventing a second style.

### `apps/telegram/src/lib.rs`

`App` gains `Screen::Login(Login)`. Enter it when `session_store::load` returns `None`;
leave it for `Screen::Chats` when the machine reports `Action::Authorized`.

### Wiring the machine

`Login::send_code` and friends return an `Action`. Send `Action::Call { body, tag }`
through `Link::call(&body, tag, unix_time)`, and route what comes back:

- `Progress::Reply { tag, body }` → `login.on_reply(tag, &body, rng)`
- `Progress::Failed { tag, text, .. }` → `login.on_error(tag, &text)`

Two actions do not go to the network:

- **`Action::Kdf`** — 100,000 PBKDF2 iterations. Measured at roughly nine seconds on this
  handset, so it **must** go to the worker thread (`symbian::work::Job`) or the phone
  freezes. `tg::link::work` is the dispatcher; add an opcode next to the existing modpow
  one.
- **`Action::ModPow`** — the same, three times, for the two-factor exchange.

- **`Action::Migrate(dc)`** — call `Link::migrate(dc)` and start the login again with the
  same number. `login.phone()` still has it. **This will happen on the first try for a
  Brazilian number**; it is the normal path, not an error.

### On success

`Link::persist()`. Without it the next launch logs in again, which is the whole point of
the session store.

## Three things that will bite

**The `+` cannot be typed.** The E72's Fn layer does not produce symbols yet — Fn+Q gives
`q`. Show a fixed `+` before the field and accept digits only; the number is
`+` followed by what was typed. (Someone is fixing the keyboard in parallel. Do not wait
for it and do not build around it.)

**The password field must be masked.** Add a `masked` flag to `TextField` rather than
keeping a shadow copy in the screen — a shadow copy is how a password ends up in two places
and only one gets cleared.

**`FLOOD_WAIT_n` carries a number and the user needs it.** Telegram's waits run to hours.
`AuthError::FloodWait(seconds)` already has it; show it. A screen that says only "try
again" is telling someone to wait an unknown length of time.

## Errors to show

`AuthError` is already classified, so match on the type and never on a string:

| | what to say |
|---|---|
| `PhoneNumberInvalid` | the number is not one Telegram knows |
| `PhoneCodeInvalid` | wrong code, let them retype |
| `PhoneCodeExpired` | offer `login.resend_code()` |
| `PasswordInvalid` | wrong password |
| `FloodWait(n)` | wait `n` seconds, with the number |
| `SignUpRequired` | this number has no account; this client cannot create one |
| `ApiIdInvalid` | the build has no credentials — a configuration problem, not the user's |
| `Other(s)` | show `s`; it is the server's own words |

## How to check it without a phone

The simulator draws every screen on the host:

```
cargo run -p preview          # renders each screen to preview-out/*.png
cargo run -p sim -- telegram  # interactive, arrow keys and typing
```

`crates/symbian-ui/src/testing.rs` has the helpers the existing screen tests use. There is
a test in `apps/telegram/src/lib.rs` called `drawing_every_screen_stays_inside_the
_framebuffer` — add the login screens to it. It has caught real bugs.

For the state machine, write tests the way `auth.rs` does: feed replies, assert on
`Action`. No network.

## Building for the device

```
tools/symbuild apps/telegram        # → apps/telegram/build/telegram.sis
python3 tools/e32dump.py apps/telegram/build/telegram.exe
```

**Check the import count before and after your change.** It is 286 across 13 DLLs today.
New imports are a deployment risk rather than a link-time question: an ordinal this handset
does not export stops the image loading, and that failure produces no error, no log and no
file — it looks exactly like the icon doing nothing. `docs/device-notes.md` has the whole
account of the time that happened.

## Conventions here

Read `docs/device-notes.md` and one existing module before starting. Two habits matter:

- **Comments say why, not what.** Every non-obvious line in this tree explains the failure
  it prevents. That is not decoration — it is most of what makes the codebase navigable
  when the platform gives no diagnostics.
- **A test asserts a behaviour someone could get wrong**, not that a getter returns what
  was set. If you cannot name the bug a test catches, it is not earning its place.

## Definition of done

- [ ] `cargo test --workspace` green, with new tests for the screens and the flow
- [ ] `cargo run -p preview` renders all three, and they fit 320×240
- [ ] the framebuffer-bounds test covers them
- [ ] `tools/symbuild apps/telegram` builds and the import count is unchanged
- [ ] a login with a real number reaches at least `CodeSent`
