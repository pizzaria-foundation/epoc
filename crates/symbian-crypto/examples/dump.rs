//! Prints digests and ciphertexts for a deterministic input series, so they can be
//! diffed against an independent implementation.
//!
//! Not a test, because a test cannot call Python. Fixed vectors pin the algorithm at a
//! handful of lengths; this exists to compare hundreds of them against `hashlib`, which is
//! where a partial-block or padding bug that only appears at one length shows up.
//!
//!     cargo run -p symbian-crypto --example dump > /tmp/ours.txt
//!
//! See the header it prints for the reference command.

use symbian_crypto::{aes::Aes, bignum, hmac, ige, sha1::sha1, sha256::sha256};

/// xorshift32, so the inputs vary without needing a random source and the series is the
/// same on both sides of the comparison.
fn series(len: usize, seed: u32) -> Vec<u8> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s as u8
        })
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    println!("# reference: crates/symbian-crypto/examples/reference.py");

    for len in 0..=300usize {
        let d = series(len, 0x1234_5678);
        println!("sha256 {len} {}", hex(&sha256(&d)));
        println!("sha1 {len} {}", hex(&sha1(&d)));
    }

    // HMAC over key lengths either side of the 64-byte block boundary, which is the case
    // RFC 2104 treats specially and the one most often got wrong.
    for klen in [0usize, 1, 20, 63, 64, 65, 100, 131] {
        let k = series(klen, 0xABCD_0001);
        for dlen in [0usize, 1, 55, 64, 200] {
            let d = series(dlen, 0x0000_BEEF);
            println!("hmac256 {klen} {dlen} {}", hex(&hmac::hmac_sha256(&k, &d)));
            println!("hmac1 {klen} {dlen} {}", hex(&hmac::hmac_sha1(&k, &d)));
        }
    }

    // AES-ECB single blocks, per key length.
    for klen in [16usize, 24, 32] {
        let k = series(klen, 0x5555_0003);
        let aes = Aes::new(&k).unwrap();
        for i in 0..8u32 {
            let mut b = [0u8; 16];
            b.copy_from_slice(&series(16, 0x9000_0000 + i));
            let pt = b;
            aes.encrypt_block(&mut b);
            println!("aes {klen} {} {}", hex(&pt), hex(&b));
        }
    }

    // AES-IGE, which no OpenSSL still has — so the reference side computes it from the
    // recurrence over its own AES.
    let k = series(32, 0x7777_0005);
    let aes = Aes::new(&k).unwrap();
    for blocks in 1..=8usize {
        let pt = series(blocks * 16, 0x2222_0000 + blocks as u32);
        let mut iv = series(32, 0x3333_0000);
        let iv0 = iv.clone();
        let mut ct = pt.clone();
        ige::encrypt(&aes, &mut iv, &mut ct).unwrap();
        println!("ige {} {} {} {}", hex(&k), hex(&iv0), hex(&pt), hex(&ct));
    }

    // modpow, across sizes. The odd-modulus requirement is why the low bit is forced: an
    // even modulus is refused, and Montgomery reduction is the reason.
    for bytes in [4usize, 8, 16, 32, 64, 128, 256] {
        for trial in 0..3u32 {
            let mut n = series(bytes, 0xC0DE_0000 + bytes as u32 * 16 + trial);
            n[0] |= 0x80; // keep it full width
            let last = n.len() - 1;
            n[last] |= 1; // odd
            let base = series(bytes, 0xBA5E_0000 + trial);
            let exp = series(bytes.min(32), 0xE7E7_0000 + trial);

            let m = bignum::Modulus::new(&n).unwrap();
            let mut out = vec![0u8; bytes];
            bignum::modpow(&base, &exp, &m, &mut out).unwrap();
            println!("modpow {} {} {} {}", hex(&n), hex(&base), hex(&exp), hex(&out));
        }
    }
}
