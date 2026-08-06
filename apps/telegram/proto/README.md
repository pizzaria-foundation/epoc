# tg-proto

MTProto 2.0: the auth-key handshake, the encrypted session, and the calls a login needs.

Verified against Telegram's own servers, not against itself.

```
$ cargo run --release -p tg-proto --example live
connected to 149.154.167.51:443
  modpow: 37 ms on the host
  modpow: 37 ms on the host
auth key negotiated
  auth_key_id: 9718eecb07fc7f41
update: NewSession { salt: 12178839425488107297, ... }
result for 6a740da100000000: 968 bytes, constructor cc1a241e

help.getConfig answered. The protocol works.
```

`cc1a241e` is `config#cc1a241e` from `api.tl`.

## No I/O, and that is the whole design

Nothing here opens a socket, reads a file, asks the clock or generates a random number.
Bytes come in, bytes go out, and everything unpredictable is passed in.

That is not architectural taste. It is the only way this could be debugged. Telegram
answers a malformed request by closing the connection — no error, no log, nothing to
inspect. A wrong slice bound in the key derivation and a wrong byte order in `p` produce
exactly the same symptom, which is silence. Debugging against the server means a socket
round trip for a one-bit answer.

Because there is no I/O, `vendor/research/mtproto/handshake.py` can perform a real
handshake, record every byte, and the tests replay it. A failure then names the step and
the offset.

| | |
|---|---|
| `cargo test -p tg-proto` | 84 tests, no network |
| `tests/handshake_differential.rs` | replays a recorded negotiation, byte for byte |
| `tests/session_differential.rs` | decrypts ciphertext Telegram actually produced |
| `examples/live.rs` | the one thing a fixture cannot check: does the server accept it |

## Layers

```
 client.rs      sequences everything; the only object an app touches
 rpc.rs         containers, gzip, rpc_result, and the login calls
 session.rs     auth_key_id, msg_key, AES-256-IGE, msg_id, seq_no
 handshake.rs   req_pq → factor → RSA → DH → auth_key
 transport.rs   length framing over TCP
 tl.rs          the wire format
 crypto.rs      RSA_PAD, the DH key derivation, hash-prefixed IGE
 pq.rs          Pollard-Brent over Montgomery
 keys.rs        Telegram's RSA key and its fingerprint
```

## What the reply to one call actually looks like

Recorded from a real `help.getConfig`:

```
msg_container(0x73f1f8dc)      an ack and new_session_created
rpc_result(0xf35c6d01)         req_msg_id, then
  gzip_packed(0x3072cfa1)        the Config, deflated
```

Three unwrappings before a single field is visible. A client that handles only the outer
layer sees nothing at all. That is why `rpc::unwrap` flattens all of it, and why
`symbian-crypto` carries an inflate — the server compresses whatever benefits, unannounced,
at any nesting level.

## The exponentiations leave the crate

Twice per login, `Step::ModPow` asks the caller for `base^exp mod m`. It measures **815 ms**
on an E72, and 815 ms inside `rust_step` freezes the window server — the whole phone, with
no watchdog. The caller puts it on the worker thread, which is proven on that hardware:
1933 ms of wall time with 27 GUI ticks served through it.

Everything else runs inline. AES and SHA-256 are microseconds; only the exponentiation is
worth another thread.

## Bugs the differentials found

**A field named for what it wasn't.** `AuthKey` had `time_offset` holding the server's
*absolute* clock, so `msg_id`s came out 56 years ahead and the server answered
`bad_msg_notification` code 16. No offline test could have caught it — every fixture has the
same wrong clock on both sides of the comparison. The live run found it in one line. The
field is now `server_time`, and the subtraction happens in `client.rs`, which is the only
layer given a local time.

**A round-trip test that tested nothing.** `session.rs` originally encrypted and decrypted
with the same code and passed. It should not have: the two directions draw different key
material from one auth key (`x = 0` outbound, `x = 8` inbound), so a client *cannot* decrypt
its own output. The test needed `encrypt_as_server`, not a fix to the code — and a version
with those slices transposed would round-trip perfectly and talk to nobody.

## Endianness, which is where the hours go

TL is little-endian. The cryptography is big-endian, because that is how the RFCs define
RSA, DH and the hashes. Both appear within fifty lines of `handshake.rs`.

Every conversion is marked. A number written in the wrong order is not a crash and not a
parse error: it is a well-formed request the server rejects by hanging up.

## Constructors are hand-written

`api.tl` describes several thousand and a generator is the obvious answer. It is not the
answer here: login needs about fifteen, the generator would be more code than the fifteen,
and every constructor it emitted would be unread code in a 150 KB image on a phone with
45 MB of RAM.

Each one carries the line from `api.tl` it came from, so the source is checkable. The layer
is pinned at **228**; raising it means rechecking every constructor against the new schema.

## What is not here

**Reading chats.** `messages.getDialogs` and the user and chat constructors are a large
surface and belong after a session exists and is trusted.

**More than one data centre.** A real client is told by `help.getConfig` which DC holds the
account and migrates. This connects to DC2 and stays there.

**Retries and reconnection.** `dh_gen_retry` restarts the handshake rather than retrying
with `retry_id`, and a dropped connection is the caller's problem.

**`api_id` and `api_hash`.** They identify the application, not the user, and come from
my.telegram.org. There is deliberately no default: Telegram bans the pairs that leak into
public clients, so a hardcoded one is a client that stops working without warning.

## Recording a new fixture

```
python3 vendor/research/mtproto/handshake.py \
    --fixture apps/telegram/proto/tests/fixtures/handshake.json

python3 vendor/research/mtproto/handshake.py --probe \
    --fixture apps/telegram/proto/tests/fixtures/session.json
```

Both negotiate a real key and throw it away. The keys in the committed fixtures authenticate
nothing: `help.getConfig` needs no login and neither was ever signed in with.
