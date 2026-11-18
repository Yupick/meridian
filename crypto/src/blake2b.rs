//! BLAKE2b (RFC 7693) y la función de salida variable H' usada por Argon2id.

/// Longitud máxima de salida de BLAKE2b en bytes.
pub const OUT_LEN_MAX: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blake2bError {
    InvalidOutputLength,
    OutputMismatch,
}

const BLOCK_LEN: usize = 128;

/// IV de BLAKE2b (el mismo que el de SHA-512).
const IV: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

/// Permutaciones de palabras de mensaje para las 12 rondas (las dos últimas repiten 0 y 1).
const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

/// Estado incremental de BLAKE2b.
pub struct Blake2bHasher {
    state: [u64; 8],
    buffer: [u8; BLOCK_LEN],
    buffered: usize,
    /// Bytes comprimidos hasta ahora (el bloque de clave cuenta como 128).
    count: u128,
    out_len: usize,
}

impl Blake2bHasher {
    pub fn new(out_len: usize) -> Self {
        Self::with_state(out_len, &[])
    }

    pub fn with_key(out_len: usize, key: &[u8]) -> Self {
        if key.len() <= OUT_LEN_MAX {
            Self::with_state(out_len, key)
        } else {
            // RFC 7693 admite claves de hasta 64 bytes. Para claves más largas se
            // reduce la clave a 64 bytes con BLAKE2b-512 antes de usarla.
            let mut folded = [0u8; OUT_LEN_MAX];
            let mut key_hasher = Blake2bHasher::new(OUT_LEN_MAX);
            key_hasher.update(key);
            let _ = key_hasher.finalize(&mut folded);
            Self::with_state(out_len, &folded)
        }
    }

    fn with_state(out_len: usize, key: &[u8]) -> Self {
        let mut state = IV;
        state[0] ^= 0x0101_0000u64 ^ ((key.len() as u64) << 8) ^ (out_len as u64);

        let mut hasher = Self {
            state,
            buffer: [0u8; BLOCK_LEN],
            buffered: 0,
            count: 0,
            out_len,
        };

        if !key.is_empty() {
            // La clave ocupa el primer bloque, relleno de ceros a 128 bytes.
            // No se comprime aquí: si el mensaje está vacío, RFC 7693 exige
            // comprimir este bloque como ÚLTIMO (t = 128, flag last = 1).
            hasher.buffer[..key.len()].copy_from_slice(key);
            hasher.buffered = BLOCK_LEN;
        }

        hasher
    }

    pub fn update(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut rest = data;

        while !rest.is_empty() {
            // El bloque lleno se comprime solo cuando llega más datos; si el
            // mensaje termina en este límite, finalize() lo comprime como último.
            if self.buffered == BLOCK_LEN {
                let block = self.buffer;
                self.count = self.count.wrapping_add(BLOCK_LEN as u128);
                compress(&mut self.state, &block, self.count, false);
                self.buffered = 0;
            }

            let take = core::cmp::min(BLOCK_LEN - self.buffered, rest.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&rest[..take]);
            self.buffered += take;
            rest = &rest[take..];
        }
    }

    pub fn finalize(self, out: &mut [u8]) -> Result<(), Blake2bError> {
        if self.out_len == 0 || self.out_len > OUT_LEN_MAX {
            return Err(Blake2bError::InvalidOutputLength);
        }
        if out.len() != self.out_len {
            return Err(Blake2bError::OutputMismatch);
        }

        let mut me = self;
        me.count = me.count.wrapping_add(me.buffered as u128);
        // El búfer puede contener restos de bloques anteriores: solo se copian
        // los bytes realmente pendientes y se rellena el resto con ceros.
        let mut block = [0u8; BLOCK_LEN];
        block[..me.buffered].copy_from_slice(&me.buffer[..me.buffered]);
        compress(&mut me.state, &block, me.count, true);

        for (i, byte) in out.iter_mut().enumerate() {
            *byte = (me.state[i / 8] >> (8 * (i % 8))) as u8;
        }
        Ok(())
    }
}

pub fn hash64(data: &[u8]) -> [u8; OUT_LEN_MAX] {
    let mut hasher = Blake2bHasher::new(OUT_LEN_MAX);
    hasher.update(data);
    let mut out = [0u8; OUT_LEN_MAX];
    let _ = hasher.finalize(&mut out);
    out
}

/// BLAKE2b directo con una longitud de salida de 1 a 64 bytes.
pub fn hash(out_len: usize, data: &[u8], out: &mut [u8]) -> Result<(), Blake2bError> {
    if out_len == 0 || out_len > OUT_LEN_MAX {
        return Err(Blake2bError::InvalidOutputLength);
    }
    if out.len() != out_len {
        return Err(Blake2bError::OutputMismatch);
    }

    let mut hasher = Blake2bHasher::new(out_len);
    hasher.update(data);
    hasher.finalize(out)
}

/// Función de salida variable H' de Argon2 (RFC 9106, sección 3.3).
///
/// Para T <= 64: H'(X) = H^T(LE32(T) || X). Para T > 64 se encadenan
/// bloques de 64 bytes y se toman los primeros 32 bytes de cada uno.
pub fn variable(out_len: usize, data: &[u8], out: &mut [u8]) -> Result<(), Blake2bError> {
    if out_len == 0 || out_len > u32::MAX as usize {
        return Err(Blake2bError::InvalidOutputLength);
    }
    if out.len() != out_len {
        return Err(Blake2bError::OutputMismatch);
    }

    let len_le = (out_len as u32).to_le_bytes();

    if out_len <= OUT_LEN_MAX {
        let mut hasher = Blake2bHasher::new(out_len);
        hasher.update(&len_le);
        hasher.update(data);
        return hasher.finalize(out);
    }

    // V1 = H^64(LE32(T) || X).
    let mut v = [0u8; OUT_LEN_MAX];
    let mut first = Blake2bHasher::new(OUT_LEN_MAX);
    first.update(&len_le);
    first.update(data);
    first.finalize(&mut v)?;

    let blocks = (out_len + 31) / 32 - 2;
    let mut pos = 0usize;

    for i in 0..blocks {
        out[pos..pos + 32].copy_from_slice(&v[..32]);
        pos += 32;
        if i + 1 < blocks {
            v = hash64(&v);
        }
    }

    // V_{r+1} = H^(T - 32r)(V_r).
    hash(out_len - pos, &v, &mut out[pos..])
}

fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

fn compress(state: &mut [u64; 8], block: &[u8; BLOCK_LEN], count: u128, last: bool) {
    let mut m = [0u64; 16];
    for (i, word) in m.iter_mut().enumerate() {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&block[i * 8..i * 8 + 8]);
        *word = u64::from_le_bytes(bytes);
    }

    let mut v = [0u64; 16];
    v[..8].copy_from_slice(state);
    v[8..].copy_from_slice(&IV);
    v[12] ^= count as u64;
    v[13] ^= (count >> 64) as u64;
    if last {
        v[14] = !v[14];
    }

    for row in SIGMA.iter() {
        let s = row;
        g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }

    for i in 0..8 {
        state[i] ^= v[i] ^ v[i + 8];
    }
}
