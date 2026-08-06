//! Times the two primitives that are slow enough to shape a design: a 2048-bit modular
//! exponentiation, and AES.
//!
//!     cargo run --release -p symbian-crypto --example bench
//!
//! On the host, which is the only place it can run. Host numbers are not device numbers —
//! `examples/selftest` measures those on the E72 and they are in `docs/device-notes.md`.
//! What the host is good for is a **ratio**, and the AES section below computes one against
//! a byte-at-a-time reference so the effect of the table rewrite is a measurement rather
//! than a claim.

use std::time::Instant;

use symbian_crypto::{bignum, Aes};

/// The cipher as it was before the T-tables: FIPS 197 section 5 done a byte at a time.
///
/// Duplicated here rather than imported because the library no longer contains it outside
/// of tests. It exists so the speedup has a denominator; without one, "faster" is a feeling.
mod slow {
    const SBOX: [u8; 256] = {
        // Derived from the definition — the multiplicative inverse in GF(2^8) followed by
        // the affine transform — rather than pasted, so this reference cannot disagree with
        // the library because of a typo in the copy.
        let mut inv = [0u8; 256];
        let mut i = 1u16;
        while i < 256 {
            let mut j = 1u16;
            while j < 256 {
                if gf(i as u8, j as u8) == 1 {
                    inv[i as usize] = j as u8;
                    break;
                }
                j += 1;
            }
            i += 1;
        }
        let mut s = [0u8; 256];
        let mut x = 0usize;
        while x < 256 {
            let b = inv[x];
            s[x] = b ^ b.rotate_left(1) ^ b.rotate_left(2) ^ b.rotate_left(3) ^ b.rotate_left(4)
                ^ 0x63;
            x += 1;
        }
        s
    };

    pub const fn gf(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0u8;
        let mut i = 0u8;
        while i < 8 {
            if b & 1 != 0 {
                p ^= a;
            }
            let hi = a & 0x80;
            a <<= 1;
            if hi != 0 {
                a ^= 0x1b;
            }
            b >>= 1;
            i += 1;
        }
        p
    }

    pub struct Slow {
        w: [u32; 60],
        rounds: usize,
    }

    impl Slow {
        pub fn new(key: &[u8]) -> Self {
            let nk = key.len() / 4;
            let rounds = nk + 6;
            let mut w = [0u32; 60];
            for (i, word) in w.iter_mut().enumerate().take(nk) {
                *word = u32::from_be_bytes([
                    key[4 * i],
                    key[4 * i + 1],
                    key[4 * i + 2],
                    key[4 * i + 3],
                ]);
            }
            let mut rcon = 1u8;
            for i in nk..4 * (rounds + 1) {
                let mut t = w[i - 1];
                if i % nk == 0 {
                    t = sub_word(t.rotate_left(8)) ^ ((rcon as u32) << 24);
                    rcon = gf(rcon, 2);
                } else if nk > 6 && i % nk == 4 {
                    t = sub_word(t);
                }
                w[i] = w[i - nk] ^ t;
            }
            Slow { w, rounds }
        }

        pub fn encrypt_block(&self, b: &mut [u8; 16]) {
            self.ark(b, 0);
            for round in 1..self.rounds {
                for x in b.iter_mut() {
                    *x = SBOX[*x as usize];
                }
                shift_rows(b);
                for col in 0..4 {
                    let c = [b[col * 4], b[col * 4 + 1], b[col * 4 + 2], b[col * 4 + 3]];
                    b[col * 4] = gf(c[0], 2) ^ gf(c[1], 3) ^ c[2] ^ c[3];
                    b[col * 4 + 1] = c[0] ^ gf(c[1], 2) ^ gf(c[2], 3) ^ c[3];
                    b[col * 4 + 2] = c[0] ^ c[1] ^ gf(c[2], 2) ^ gf(c[3], 3);
                    b[col * 4 + 3] = gf(c[0], 3) ^ c[1] ^ c[2] ^ gf(c[3], 2);
                }
                self.ark(b, round);
            }
            for x in b.iter_mut() {
                *x = SBOX[*x as usize];
            }
            shift_rows(b);
            self.ark(b, self.rounds);
        }

        fn ark(&self, b: &mut [u8; 16], round: usize) {
            for col in 0..4 {
                let k = self.w[round * 4 + col].to_be_bytes();
                for r in 0..4 {
                    b[col * 4 + r] ^= k[r];
                }
            }
        }
    }

    fn sub_word(w: u32) -> u32 {
        let b = w.to_be_bytes();
        u32::from_be_bytes([
            SBOX[b[0] as usize],
            SBOX[b[1] as usize],
            SBOX[b[2] as usize],
            SBOX[b[3] as usize],
        ])
    }

    fn shift_rows(b: &mut [u8; 16]) {
        for row in 1..4 {
            let mut tmp = [0u8; 4];
            for col in 0..4 {
                tmp[col] = b[((col + row) % 4) * 4 + row];
            }
            for col in 0..4 {
                b[col * 4 + row] = tmp[col];
            }
        }
    }
}

fn bench_aes() {
    const KEY: [u8; 32] = [0x5au8; 32];
    const BLOCKS: u32 = 40_000; // 640 KB

    let fast = Aes::new(&KEY).unwrap();
    let slow = slow::Slow::new(&KEY);

    // Correctness before speed: a benchmark of two things that disagree measures nothing.
    let mut a = [0x11u8; 16];
    let mut b = a;
    fast.encrypt_block(&mut a);
    slow.encrypt_block(&mut b);
    assert_eq!(a, b, "the reference and the library disagree; the ratio below is meaningless");

    let mut block = [0u8; 16];
    let t = Instant::now();
    for _ in 0..BLOCKS {
        fast.encrypt_block(&mut block);
    }
    let fast_s = t.elapsed().as_secs_f64();

    let mut block = [0u8; 16];
    let t = Instant::now();
    for _ in 0..BLOCKS {
        slow.encrypt_block(&mut block);
    }
    let slow_s = t.elapsed().as_secs_f64();

    let kb = f64::from(BLOCKS) * 16.0 / 1024.0;
    println!("AES-256, host:");
    println!("  tables:    {:>9.0} KB/s", kb / fast_s);
    println!("  byte-wise: {:>9.0} KB/s", kb / slow_s);
    println!("  ratio:     {:>9.1}x", slow_s / fast_s);
    println!("  the E72 measured 169 KB/s byte-wise, so expect roughly {:.0} KB/s there",
             169.0 * slow_s / fast_s);
    println!();
}

fn main() {
    bench_aes();

    // A full-width odd modulus: the top bit set so it is genuinely 2048 bits, and the low
    // bit set because Montgomery reduction requires an odd modulus.
    let n: Vec<u8> = {
        let mut v = vec![0u8; 256];
        let mut s = 0xC0DEu32 | 1;
        for b in v.iter_mut() {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *b = s as u8;
        }
        v[0] |= 0x80;
        v[255] |= 1;
        v
    };
    let m = bignum::Modulus::new(&n).unwrap();
    let base = vec![0x03u8];
    // A full-width exponent, which is the worst case and the realistic one: the ladder runs
    // once per bit of the buffer handed in, and a DH secret should be passed at fixed width.
    let exp = vec![0xA5u8; 256];
    let mut out = vec![0u8; 256];

    bignum::modpow(&base, &exp, &m, &mut out).unwrap(); // warm the caches
    let t = Instant::now();
    const N: u32 = 10;
    for _ in 0..N {
        bignum::modpow(&base, &exp, &m, &mut out).unwrap();
    }
    let per = t.elapsed().as_secs_f64() / f64::from(N);

    println!("2048-bit modpow, 2048-bit exponent: {:.1} ms/op", per * 1000.0);
    println!("  4096 mont_mul (2048 squarings + 2048 multiplies, one of each per bit)");
    println!("  {:.1} us per mont_mul", per * 1e6 / 4096.0);
    println!("  ~8200 limb products per mont_mul, so ~{:.1} ns per product", per * 1e9 / 4096.0 / 8200.0);
}
