use crate::aes::Aes;

/// Error de autenticación o parámetros inválidos en AES-128-GCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcmError {
    InvalidKeyLength,
    InvalidTag,
}

/// AES-128-GCM (NIST SP 800-38D) con IV de 96 bits.
pub struct Aes128Gcm {
    aes: Aes,
    h: [u8; 16],
}

impl Aes128Gcm {
    /// Crea una instancia a partir de una clave de 16 bytes.
    pub fn new(key: &[u8; 16]) -> Self {
        let aes = Aes::new(key).expect("AES-128 acepta claves de 16 bytes");
        let h = aes.encrypt_block(&[0u8; 16]);
        Self { aes, h }
    }

    /// Cifra y autentica. `out_ciphertext` debe tener la misma longitud que
    /// `plaintext`. Escribe el tag de 16 bytes en `out_tag`.
    pub fn encrypt(
        &self,
        iv: &[u8; 12],
        aad: &[u8],
        plaintext: &[u8],
        out_ciphertext: &mut [u8],
        out_tag: &mut [u8; 16],
    ) {
        assert_eq!(plaintext.len(), out_ciphertext.len());
        let j0 = j0_from_iv(iv);
        gctr(&self.aes, &inc32(&j0), plaintext, out_ciphertext);
        let s = ghash(
            &self.h,
            aad,
            out_ciphertext,
        );
        let mut tag_mask = [0u8; 16];
        gctr(&self.aes, &j0, &s, &mut tag_mask);
        *out_tag = tag_mask;
    }

    /// Descifra y verifica el tag. Devuelve `true` si la autenticación es
    /// correcta y escribe el plaintext en `out_plaintext` (misma longitud que
    /// `ciphertext`). Si falla, devuelve `false` y no garantiza el contenido
    /// de `out_plaintext`.
    pub fn decrypt(
        &self,
        iv: &[u8; 12],
        aad: &[u8],
        ciphertext: &[u8],
        tag: &[u8; 16],
        out_plaintext: &mut [u8],
    ) -> bool {
        assert_eq!(ciphertext.len(), out_plaintext.len());
        let j0 = j0_from_iv(iv);
        let s = ghash(&self.h, aad, ciphertext);
        let mut expected = [0u8; 16];
        gctr(&self.aes, &j0, &s, &mut expected);
        if !ct_eq(tag, &expected) {
            return false;
        }
        gctr(&self.aes, &inc32(&j0), ciphertext, out_plaintext);
        true
    }
}

fn j0_from_iv(iv: &[u8; 12]) -> [u8; 16] {
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(iv);
    j0[15] = 1;
    j0
}

fn inc32(block: &[u8; 16]) -> [u8; 16] {
    let mut out = *block;
    let mut c: u32 = 1;
    let mut i = 16;
    while i > 12 {
        i -= 1;
        let sum = out[i] as u32 + c;
        out[i] = (sum & 0xff) as u8;
        c = sum >> 8;
    }
    out
}

fn gctr(aes: &Aes, icb: &[u8; 16], input: &[u8], output: &mut [u8]) {
    if input.is_empty() {
        return;
    }
    let mut counter = *icb;
    let mut off = 0usize;
    while off < input.len() {
        let ks = aes.encrypt_block(&counter);
        counter = inc32(&counter);
        let take = core::cmp::min(16, input.len() - off);
        let mut i = 0;
        while i < take {
            output[off + i] = input[off + i] ^ ks[i];
            i += 1;
        }
        off += take;
    }
}

fn ghash(h: &[u8; 16], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
    let mut y = [0u8; 16];

    // AAD blocks
    let mut off = 0usize;
    while off < aad.len() {
        let mut block = [0u8; 16];
        let take = core::cmp::min(16, aad.len() - off);
        block[..take].copy_from_slice(&aad[off..off + take]);
        xor_block(&mut y, &block);
        y = gf_mul(&y, h);
        off += take;
    }

    // Ciphertext blocks
    off = 0;
    while off < ciphertext.len() {
        let mut block = [0u8; 16];
        let take = core::cmp::min(16, ciphertext.len() - off);
        block[..take].copy_from_slice(&ciphertext[off..off + take]);
        xor_block(&mut y, &block);
        y = gf_mul(&y, h);
        off += take;
    }

    // Length block: bit lengths as 64-bit big-endian
    let mut len_block = [0u8; 16];
    let aad_bits = (aad.len() as u64) * 8;
    let ct_bits = (ciphertext.len() as u64) * 8;
    len_block[0..8].copy_from_slice(&aad_bits.to_be_bytes());
    len_block[8..16].copy_from_slice(&ct_bits.to_be_bytes());
    xor_block(&mut y, &len_block);
    y = gf_mul(&y, h);
    y
}

fn xor_block(dst: &mut [u8; 16], src: &[u8; 16]) {
    let mut i = 0;
    while i < 16 {
        dst[i] ^= src[i];
        i += 1;
    }
}

fn gf_mul(x: &[u8; 16], y: &[u8; 16]) -> [u8; 16] {
    // NIST SP 800-38D Algorithm 1, bit 0 = MSB.
    let mut z = [0u8; 16];
    let mut v = *y;
    let mut i = 0usize;
    while i < 128 {
        let byte = i / 8;
        let bit = 7 - (i % 8);
        if ((x[byte] >> bit) & 1) != 0 {
            xor_block(&mut z, &v);
        }
        let lsb = v[15] & 1;
        // V = V >> 1 (big-endian 128-bit right shift)
        let mut j = 15usize;
        while j > 0 {
            v[j] = (v[j] >> 1) | ((v[j - 1] & 1) << 7);
            j -= 1;
        }
        v[0] >>= 1;
        if lsb != 0 {
            v[0] ^= 0xe1;
        }
        i += 1;
    }
    z
}

fn ct_eq(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut diff = 0u8;
    let mut i = 0;
    while i < 16 {
        diff |= a[i] ^ b[i];
        i += 1;
    }
    diff == 0
}
