//! Ed25519 (RFC 8032), implementado directamente sobre la aritmética de
//! Curve25519. Sin dependencias externas, sin `alloc`, sin `unsafe`.
//!
//! El campo se representa en 5 limbs de radix 51 (`u128` para los productos),
//! los escalares como 4 palabras de 64 bits reducidas módulo `L`, y los puntos
//! en coordenadas extendidas de Edwards.

use core::ops::{Add, Sub};

use crate::sha512::{hash, Sha512};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ed25519Error {
    InvalidLength,
}

pub const SECRET_KEY_LEN: usize = 32;
pub const PUBLIC_KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;

const MASK51: u128 = (1u128 << 51) - 1;

/// p = 2^255 - 19 en limbs de radix 51.
const P: [u128; 5] = [MASK51 - 18, MASK51, MASK51, MASK51, MASK51];

/// d = -121665/121666 mod p.
const D_BYTES: [u8; 32] = [
    0xa3, 0x78, 0x59, 0x13, 0xca, 0x4d, 0xeb, 0x75, 0xab, 0xd8, 0x41, 0x41, 0x4d, 0x0a, 0x70, 0x00,
    0x98, 0xe8, 0x79, 0x77, 0x79, 0x40, 0xc7, 0x8c, 0x73, 0xfe, 0x6f, 0x2b, 0xee, 0x6c, 0x03, 0x52,
];

/// Base point B = (Bx, By).
const BX_BYTES: [u8; 32] = [
    0x1a, 0xd5, 0x25, 0x8f, 0x60, 0x2d, 0x56, 0xc9, 0xb2, 0xa7, 0x25, 0x95, 0x60, 0xc7, 0x2c, 0x69,
    0x5c, 0xdc, 0xd6, 0xfd, 0x31, 0xe2, 0xa4, 0xc0, 0xfe, 0x53, 0x6e, 0xcd, 0xd3, 0x36, 0x69, 0x21,
];

const BY_BYTES: [u8; 32] = [
    0x58, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
];

/// sqrt(-1) = 2^((p-1)/4).
const SQRT_M1_BYTES: [u8; 32] = [
    0xb0, 0xa0, 0x0e, 0x4a, 0x27, 0x1b, 0xee, 0xc4, 0x78, 0xe4, 0x2f, 0xad, 0x06, 0x18, 0x43, 0x2f,
    0xa7, 0xd7, 0xfb, 0x3d, 0x99, 0x00, 0x4d, 0x2b, 0x0b, 0xdf, 0xc1, 0x4f, 0x80, 0x24, 0x83, 0x2b,
];

/// p - 2 (para inversión por Fermat).
const EXP_INV: [u8; 32] = [
    0xeb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
];

/// (p + 3) / 8 = 2^252 - 2 (para raíz cuadrada).
const EXP_SQRT: [u8; 32] = [
    0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x0f,
];

/// L en little-endian.
const L_BYTES: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x81, 0x65, 0xd6, 0x9c, 0x79, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
];

// ---------------------------------------------------------------------------
// Aritmética de campo: GF(2^255 - 19), 5 limbs de 51 bits.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Fe([u128; 5]);

impl Add for Fe {
    type Output = Fe;

    fn add(self, rhs: Fe) -> Fe {
        Fe::add(self, rhs)
    }
}

impl Sub for Fe {
    type Output = Fe;

    fn sub(self, rhs: Fe) -> Fe {
        Fe::sub(self, rhs)
    }
}

impl Fe {
    fn zero() -> Self {
        Fe([0; 5])
    }

    fn one() -> Self {
        let mut x = [0u128; 5];
        x[0] = 1;
        Fe(x)
    }

    /// Reduce carries hasta que no quede exceso en el limb alto.
    fn carry(mut self) -> Self {
        loop {
            let mut c = 0u128;
            for limb in self.0.iter_mut() {
                let v = *limb + c;
                *limb = v & MASK51;
                c = v >> 51;
            }
            if c == 0 {
                return self;
            }
            // 2^255 = 19 (mod p).
            self.0[0] += c * 19;
        }
    }

    /// Interpreta 32 bytes little-endian (bit 255 descartado) como elemento de campo.
    fn from_bytes(bytes: &[u8; 32]) -> Self {
        let mut limbs = [0u128; 5];
        for (i, b) in bytes.iter().enumerate() {
            limbs[(i * 8) / 51] |= (*b as u128) << ((i * 8) % 51);
        }
        limbs[4] &= MASK51;
        Fe(limbs).carry()
    }

    /// Versión canónica: rechaza representaciones >= p.
    fn from_bytes_canonical(bytes: &[u8; 32]) -> Option<Self> {
        let f = Fe::from_bytes(bytes);
        if f.ge(Fe(P)) {
            None
        } else {
            Some(f)
        }
    }

    fn to_bytes(self) -> [u8; 32] {
        let mut f = self.carry();
        if f.ge(Fe(P)) {
            f = sub_raw(f, Fe(P));
        }
        let mut out = [0u8; 32];
        for i in 0..5 {
            let mut v = f.0[i];
            for k in 0..51 {
                if v & 1 != 0 {
                    let bit = i * 51 + k;
                    out[bit / 8] |= 1 << (bit % 8);
                }
                v >>= 1;
            }
        }
        out
    }

    fn add(self, rhs: Fe) -> Fe {
        let mut out = [0u128; 5];
        for i in 0..5 {
            out[i] = self.0[i] + rhs.0[i];
        }
        Fe(out).carry()
    }

    fn double(self) -> Fe {
        self.add(self)
    }

    /// Resta módulo p usando 2p para evitar subdesbordamiento.
    fn sub(self, rhs: Fe) -> Fe {
        let mut out = [0u128; 5];
        for i in 0..5 {
            out[i] = self.0[i] + 2 * P[i] - rhs.0[i];
        }
        Fe(out).carry()
    }

    fn neg(self) -> Fe {
        Fe::zero().sub(self)
    }

    fn mul(self, rhs: Fe) -> Fe {
        let mut t = [0u128; 9];
        for i in 0..5 {
            for j in 0..5 {
                t[i + j] += self.0[i] * rhs.0[j];
            }
        }
        // x^5 = 2^255 = 19 (mod p).
        for i in (5..9).rev() {
            t[i - 5] += t[i] * 19;
        }
        let mut out = [0u128; 5];
        out.copy_from_slice(&t[..5]);
        Fe(out).carry()
    }

    fn square(self) -> Fe {
        self.mul(self)
    }

    fn pow(self, exp: &[u8; 32]) -> Fe {
        let mut r = Fe::one();
        for i in (0..256).rev() {
            r = r.square();
            if (exp[i / 8] >> (i % 8)) & 1 != 0 {
                r = r.mul(self);
            }
        }
        r
    }

    fn inv(self) -> Fe {
        self.pow(&EXP_INV)
    }

    fn sqrt(self) -> Option<Fe> {
        let mut x = self.pow(&EXP_SQRT);
        if x.square().ct_eq(self) {
            return Some(x);
        }
        x = x.mul(Fe::from_bytes(&SQRT_M1_BYTES));
        if x.square().ct_eq(self) {
            Some(x)
        } else {
            None
        }
    }

    fn is_zero(self) -> bool {
        self.0.iter().all(|x| *x == 0)
    }

    fn is_negative(self) -> bool {
        self.carry().0[0] & 1 == 1
    }

    fn ct_eq(self, rhs: Fe) -> bool {
        let mut diff = 0u128;
        for i in 0..5 {
            diff |= self.0[i] ^ rhs.0[i];
        }
        diff == 0
    }

    /// Comparación de valores representados, limb alto primero.
    fn ge(self, rhs: Fe) -> bool {
        for i in (0..5).rev() {
            if self.0[i] > rhs.0[i] {
                return true;
            }
            if self.0[i] < rhs.0[i] {
                return false;
            }
        }
        true
    }
}

/// Resta sin envolvente (asume self >= rhs), valores ya normalizados.
fn sub_raw(a: Fe, b: Fe) -> Fe {
    let mut out = [0u128; 5];
    let mut borrow = 0i128;
    for i in 0..5 {
        let d = a.0[i] as i128 - b.0[i] as i128 - borrow;
        if d < 0 {
            out[i] = (d + (1i128 << 51)) as u128;
            borrow = 1;
        } else {
            out[i] = d as u128;
            borrow = 0;
        }
    }
    Fe(out).carry()
}

// ---------------------------------------------------------------------------
// Escalares módulo L.
// ---------------------------------------------------------------------------

const L_LIMBS: [u64; 4] = [
    0x5812_631a_5cf5_d3ed,
    0x14de_f9de_a2f7_9cd6,
    0x0000_0000_0000_0000,
    0x1000_0000_0000_0000,
];

#[derive(Clone, Copy, Debug)]
struct Scalar([u64; 4]);

impl Scalar {
    fn zero() -> Self {
        Scalar([0; 4])
    }

    /// Reduce una entrada de 32 o 64 bytes módulo L, bit a bit (MSB first).
    fn reduce_bytes(bytes: &[u8]) -> Self {
        let mut r = Scalar::zero();
        for i in (0..bytes.len() * 8).rev() {
            r.double();
            if (bytes[i / 8] >> (i % 8)) & 1 != 0 {
                r.add_one();
            }
            if r.ge(&Scalar(L_LIMBS)) {
                r.sub_assign_l();
            }
        }
        r
    }

    /// Seed con clamp RFC 8032 (bit 255 a 0, bit 254 a 1, 3 bits bajos a 0).
    fn from_clamped(h: &[u8; 32]) -> Self {
        let mut b = [0u8; 32];
        b.copy_from_slice(h);
        b[0] &= 0xf8;
        b[31] &= 0x3f;
        b[31] |= 0x40;
        Scalar::reduce_bytes(&b)
    }

    /// S ya verificado como canónico (< L).
    fn from_canonical_bytes(bytes: &[u8; 32]) -> Self {
        let mut limbs = [0u64; 4];
        for i in 0..4 {
            let mut v = 0u64;
            for j in 0..8 {
                v |= (bytes[i * 8 + j] as u64) << (8 * j);
            }
            limbs[i] = v;
        }
        Scalar(limbs)
    }

    fn to_bytes(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for i in 0..4 {
            for j in 0..8 {
                out[i * 8 + j] = (self.0[i] >> (8 * j)) as u8;
            }
        }
        out
    }

    fn bit(self, i: usize) -> u64 {
        (self.0[i / 64] >> (i % 64)) & 1
    }

    /// Duplicar seguro: el valor siempre es < L < 2^253.
    fn double(&mut self) {
        let mut carry = 0u128;
        for word in self.0.iter_mut() {
            let v = (*word as u128) * 2 + carry;
            *word = v as u64;
            carry = v >> 64;
        }
    }

    fn add_one(&mut self) {
        for word in self.0.iter_mut() {
            let (v, overflow) = word.overflowing_add(1);
            *word = v;
            if !overflow {
                break;
            }
        }
    }

    fn ge(&self, rhs: &Scalar) -> bool {
        for i in (0..4).rev() {
            if self.0[i] > rhs.0[i] {
                return true;
            }
            if self.0[i] < rhs.0[i] {
                return false;
            }
        }
        true
    }

    fn sub_assign_l(&mut self) {
        let mut borrow = 0u128;
        for i in 0..4 {
            let v = (self.0[i] as u128)
                .wrapping_sub(L_LIMBS[i] as u128)
                .wrapping_sub(borrow);
            self.0[i] = v as u64;
            borrow = v >> 127;
        }
    }

    fn add(self, rhs: Scalar) -> Scalar {
        let mut r = [0u64; 4];
        let mut carry = 0u128;
        for i in 0..4 {
            let v = self.0[i] as u128 + rhs.0[i] as u128 + carry;
            r[i] = v as u64;
            carry = v >> 64;
        }
        let mut s = Scalar(r);
        if s.ge(&Scalar(L_LIMBS)) {
            s.sub_assign_l();
        }
        s
    }

    fn mul(self, rhs: Scalar) -> Scalar {
        let mut wide = [0u64; 8];
        for i in 0..4 {
            let mut carry = 0u128;
            for j in 0..4 {
                let v = wide[i + j] as u128 + self.0[i] as u128 * rhs.0[j] as u128 + carry;
                wide[i + j] = v as u64;
                carry = v >> 64;
            }
            wide[i + 4] = carry as u64;
        }
        let mut bytes = [0u8; 64];
        for i in 0..8 {
            for j in 0..8 {
                bytes[i * 8 + j] = (wide[i] >> (8 * j)) as u8;
            }
        }
        Scalar::reduce_bytes(&bytes)
    }
}

/// `s >= L` de forma conspícua (sin corte anticipado).
fn s_not_canonical(s: &[u8; 32]) -> bool {
    let mut borrow = 0u16;
    for i in 0..32 {
        let v = (s[i] as u16)
            .wrapping_sub(L_BYTES[i] as u16)
            .wrapping_sub(borrow);
        borrow = (v >> 15) & 1;
    }
    borrow == 0
}

fn ct_eq_bytes(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Curva de Edwards retorcida, coordenadas extendidas (X:Y:Z:T).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

impl Point {
    fn identity() -> Self {
        Point {
            x: Fe::zero(),
            y: Fe::one(),
            z: Fe::one(),
            t: Fe::zero(),
        }
    }

    fn base() -> Self {
        Point {
            x: Fe::from_bytes(&BX_BYTES),
            y: Fe::from_bytes(&BY_BYTES),
            z: Fe::one(),
            t: Fe::from_bytes(&BX_BYTES).mul(Fe::from_bytes(&BY_BYTES)),
        }
    }

    /// Fórmula completa de adición para a = -1 (Hisil et al.).
    fn add(self, rhs: Point) -> Point {
        let d = Fe::from_bytes(&D_BYTES);
        let a = (self.y - self.x).mul(rhs.y - rhs.x);
        let b = (self.y + self.x).mul(rhs.y + rhs.x);
        let c = d.double().mul(self.t).mul(rhs.t);
        let dd = self.z.mul(rhs.z).double();
        let e = b - a;
        let f = dd - c;
        let g = dd + c;
        let h = b + a;
        Point {
            x: e.mul(f),
            y: g.mul(h),
            t: e.mul(h),
            z: f.mul(g),
        }
    }

    fn mul(self, scalar: Scalar) -> Point {
        let mut r = Point::identity();
        for i in (0..256).rev() {
            r = r.add(r);
            if scalar.bit(i) == 1 {
                r = r.add(self);
            }
        }
        r
    }

    fn to_bytes(self) -> [u8; 32] {
        let zinv = self.z.inv();
        let x = self.x.mul(zinv);
        let y = self.y.mul(zinv);
        let mut out = y.to_bytes();
        if x.is_negative() {
            out[31] |= 0x80;
        }
        out
    }

    fn decompress(bytes: &[u8; 32]) -> Option<Point> {
        let sign = bytes[31] >> 7;
        let mut yb = [0u8; 32];
        yb.copy_from_slice(bytes);
        yb[31] &= 0x7f;
        let y = Fe::from_bytes_canonical(&yb)?;

        let d = Fe::from_bytes(&D_BYTES);
        let y2 = y.square();
        let u = y2 - Fe::one();
        let v = d.mul(y2) + Fe::one();
        let x2 = u.mul(v.inv());
        let mut x = x2.sqrt()?;

        if x.is_negative() != (sign == 1) {
            x = x.neg();
        }
        if x.is_zero() && sign == 1 {
            return None;
        }

        let p = Point {
            x,
            y,
            z: Fe::one(),
            t: x.mul(y),
        };
        if !p.is_on_curve() {
            return None;
        }
        Some(p)
    }

    fn is_on_curve(self) -> bool {
        let d = Fe::from_bytes(&D_BYTES);
        let lhs = self.y.square() - self.x.square();
        let rhs = self.z.square() + d.mul(self.t.square());
        lhs.ct_eq(rhs) && self.x.mul(self.y).ct_eq(self.z.mul(self.t))
    }

    /// Igualdad projectiva, sin invertir: X1*Z2 == X2*Z1 e Y1*Z2 == Y2*Z1.
    fn ct_eq(self, rhs: Point) -> bool {
        self.x.mul(rhs.z).ct_eq(rhs.x.mul(self.z)) && self.y.mul(rhs.z).ct_eq(rhs.y.mul(self.z))
    }
}

// ---------------------------------------------------------------------------
// API pública.
// ---------------------------------------------------------------------------

pub fn secret_to_public(secret_key: &[u8; SECRET_KEY_LEN]) -> [u8; PUBLIC_KEY_LEN] {
    let h = hash(secret_key);
    let mut a_bytes = [0u8; 32];
    a_bytes.copy_from_slice(&h[..32]);
    let a = Scalar::from_clamped(&a_bytes);
    Point::base().mul(a).to_bytes()
}

pub fn sign(secret_key: &[u8; SECRET_KEY_LEN], message: &[u8]) -> [u8; SIGNATURE_LEN] {
    let h = hash(secret_key);
    let mut a_bytes = [0u8; 32];
    a_bytes.copy_from_slice(&h[..32]);
    let a = Scalar::from_clamped(&a_bytes);
    let prefix = &h[32..];

    let a_point = Point::base().mul(a);
    let encoded_a = a_point.to_bytes();

    let mut hasher = Sha512::new();
    hasher.update(prefix);
    hasher.update(message);
    let mut r_digest = [0u8; 64];
    hasher.finalize(&mut r_digest);
    let r = Scalar::reduce_bytes(&r_digest);
    let encoded_r = Point::base().mul(r).to_bytes();

    let mut hasher = Sha512::new();
    hasher.update(&encoded_r);
    hasher.update(&encoded_a);
    hasher.update(message);
    let mut k_digest = [0u8; 64];
    hasher.finalize(&mut k_digest);
    let k = Scalar::reduce_bytes(&k_digest);

    let s = r.add(k.mul(a));

    let mut signature = [0u8; SIGNATURE_LEN];
    signature[..32].copy_from_slice(&encoded_r);
    signature[32..].copy_from_slice(&s.to_bytes());
    signature
}

pub fn verify(
    public_key: &[u8; PUBLIC_KEY_LEN],
    message: &[u8],
    signature: &[u8; SIGNATURE_LEN],
) -> bool {
    let mut encoded_r = [0u8; PUBLIC_KEY_LEN];
    encoded_r.copy_from_slice(&signature[..PUBLIC_KEY_LEN]);
    let mut encoded_s = [0u8; SECRET_KEY_LEN];
    encoded_s.copy_from_slice(&signature[PUBLIC_KEY_LEN..]);

    if s_not_canonical(&encoded_s) {
        return false;
    }

    let a = match Point::decompress(public_key) {
        Some(p) => p,
        None => return false,
    };
    let r_point = match Point::decompress(&encoded_r) {
        Some(p) => p,
        None => return false,
    };

    let mut hasher = Sha512::new();
    hasher.update(&encoded_r);
    hasher.update(public_key);
    hasher.update(message);
    let mut k_digest = [0u8; 64];
    hasher.finalize(&mut k_digest);
    let k = Scalar::reduce_bytes(&k_digest);

    let s = Scalar::from_canonical_bytes(&encoded_s);
    let lhs = Point::base().mul(s);
    let rhs = r_point.add(a.mul(k));
    lhs.ct_eq(rhs) && ct_eq_bytes(public_key, &a.to_bytes())
}
