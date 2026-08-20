# mbedtls-symbian (vendored)

TLS 1.2/1.3 for the E72, so the phone can speak modern HTTPS despite its 2009 native stack.

- **Upstream:** shinovon's MBed TLS port for Symbian — https://github.com/shinovon/mbedtls-symbian
  (mbedTLS 3.4.1), distributed via https://nnproject.cc/tls/
- **License:** Apache-2.0 (mbedTLS upstream).
- **What is vendored here (for building against — the runtime DLL is installed on the device from
  the project's mbedtls.sis):**
  - `sdk/epoc32/release/armv5/lib/mbedtls.dso` — armv5/gcce import library (prebuilt).
    sha256 = 7d48c36df28bc5a6cb897899ae1a58f61dd4a52a7b5bb7a64cd0b375b646ac90
  - `sdk/epoc32/include/mbedtls/`, `sdk/epoc32/include/psa/` — headers from the same source tree.
- The `.dso` is a link-time import stub (no code, no cert, no SID); the actual `mbedtls.dll`
  ships to the phone via `mbedtls.sis` from nnproject.cc/tls.
