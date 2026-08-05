//! Times a 2048-bit modular exponentiation.
//!
//!     cargo run --release -p symbian-crypto --example bench
//!
//! On the host, which is the only place it can run. The device number in the module docs is
//! this measurement scaled, and the scaling factor is an estimate — stated as one there
//! rather than presented as a measurement.

use std::time::Instant;

use symbian_crypto::bignum;

fn main() {
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
