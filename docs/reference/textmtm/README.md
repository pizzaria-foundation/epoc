# TextMTM — the reference MTM implementation, extracted

Symbian's own four-component MTM example, in full. Extracted from the SDK's documentation
jar, which is the only place it exists: the example is **not** shipped as buildable source
anywhere in `sdk/`, and `sdk/s60cppexamples/messaging/` is an ordinary GUI app that merely
*uses* messaging APIs — it is not an MTM.

Regenerate with the snippet at the bottom.

## Why this is here rather than being read from the jar each time

Because `apps/devdump` proved the platform's messaging stack is reachable and the DLL track
works, so an MTM is now real work rather than a question — and this is the only concrete
implementation of one that we have. Grepping a 71 MB jar for `ContextIcon` every time is
not a workflow, and the extraction has a trap in it (below) that is worth doing once.

## The one thing the extraction gets right on purpose

The HTML must be de-tagged with **block-level tags becoming a newline**, not with every tag
becoming the empty string. Strip them all and each `<pre>` collapses onto a single enormous
line, which is what makes these pages look unusable — every code listing in the example is
inside a `<pre>` whose lines are separated by markup, not by `\n`.

## What is in each file

| file | what |
|---|---|
| `mtmexampleintro.txt` | what the example is: a fake transport over the local filesystem |
| `building.txt` | build and install steps, including where resources and icons go |
| `csm.txt` | **Client MTM** (`txtc`) — `CBaseMtm` subclass, full source |
| `ssm.txt` | **Server MTM** (`txts`) — `CBaseServerMtm` subclass, full source, and the registration `.rss` at line ~1710 |
| `uim.txt` | **UI MTM** (`txtu`) — `CBaseMtmUi` subclass, full source |
| `udm.txt` | **UI Data MTM** (`txti`) — `CBaseMtmUiData` subclass, full source, icons |
| `utils.txt` | the shared utility DLL, and `txin`, the installer that calls `InstallMtmGroup` |

## The caveat the example carries itself

> *Note: This example is designed to work with Techview and there is no guarantee that it
> will work on other interfaces*

Techview was Symbian's own reference UI, not S60. So the framework half (registration,
`CBaseMtm*` subclassing, the message store) transfers directly; anything about how the
message *appears* does not, and there is no S60-specific MTM documentation anywhere in the
SDK. That gap is what `apps/devdump`'s `mtm` probe exists to measure.

Two divergences already known:

- The `.rss` here installs DLLs to `z:\system\libs\` — a pre-9.0 path. On 9.x they go to
  `\sys\bin\`, and the registration may name a bare filename, which the loader resolves
  there. The E72's own `sms.rsc` and `mms.rsc` use bare filenames; only `btmtm.rsc` still
  carries the legacy full path.
- Every component here is its own DLL at ordinal 1. On S60 3.2 that is not required:
  `uni.rsc` registers one DLL at ordinals 1–4 and `sms.rsc` shares `smum.dll` between UI
  and UI Data. One DLL with several exports is cheaper for us, and the loader reads the
  ordinal rather than the DLL's UID2.

## Regenerating

```sh
# from the SDK root; see the de-tagging note above for why this is not a one-liner
python3 - <<'EOF'
import html, re, zipfile, pathlib
jar  = "sdk/series60doc/com.nokia.s60.sdk.cppapi_3.2_1.4.2.jar"
base = ("GUID-DF051F0D-7C20-441D-A667-FD18E04FA54F/html/SDL_93"
        "/doc_source/examples/MessagingEx/TextMTMEx")
BLOCK = re.compile(r"</?(p|br|div|tr|li|h[1-6]|pre|table|thead|tbody|dt|dd|dl|ul|ol|blockquote)\b[^>]*>", re.I)
with zipfile.ZipFile(jar) as z:
    for n in sorted(x for x in z.namelist() if x.startswith(base) and x.endswith(".html")):
        raw = re.sub(r"(?is)<(head|script|style)\b.*?</\1>", "", z.read(n).decode("utf-8", "replace"))
        txt = html.unescape(re.sub(r"<[^>]+>", "", BLOCK.sub("\n", raw)))
        keep, blank = [], 0
        for l in (x.rstrip() for x in txt.splitlines()):
            blank = 0 if l.strip() else blank + 1
            if l.strip() or blank == 1:
                keep.append(l)
        dst = pathlib.Path("docs/reference/textmtm") / pathlib.PurePosixPath(n).name.replace(".guide.html", ".txt").replace(".html", ".txt")
        dst.write_text("\n".join(keep).strip() + "\n")
EOF
```
