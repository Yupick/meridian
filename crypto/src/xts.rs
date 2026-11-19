use crate::aes::Aes;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XtsError {
    InvalidKeyLength,
    DataTooShort,
}

pub struct XtsAes {
    _key1: Aes,
    _key2: Aes,
}

#[inline]
fn xor_assign(block: &mut [u8; 16], tweak: &[u8; 16]) {
    for i in 0..16 {
        block[i] ^= tweak[i];
    }
}

/// Multiplicación por α en GF(2^128) con representación little-endian y
/// polinomio x^128 + x^7 + x^2 + x + 1 (x^128 ≡ 0x87).
#[inline]
fn mul_alpha(tweak: [u8; 16]) -> [u8; 16] {
    let carry = tweak[15] >> 7;
    let mut out = [0u8; 16];
    out[0] = (tweak[0] << 1) ^ if carry != 0 { 0x87 } else { 0 };
    for i in 1..16 {
        out[i] = (tweak[i] << 1) | (tweak[i - 1] >> 7);
    }
    out
}

#[inline]
fn xex_encrypt(aes: &Aes, block: &[u8; 16], tweak: &[u8; 16]) -> [u8; 16] {
    let mut input = *block;
    xor_assign(&mut input, tweak);
    let mut output = aes.encrypt_block(&input);
    xor_assign(&mut output, tweak);
    output
}

#[inline]
fn xex_decrypt(aes: &Aes, block: &[u8; 16], tweak: &[u8; 16]) -> [u8; 16] {
    let mut input = *block;
    xor_assign(&mut input, tweak);
    let mut output = aes.decrypt_block(&input);
    xor_assign(&mut output, tweak);
    output
}

impl XtsAes {
    pub fn new(key: &[u8]) -> Result<Self, XtsError> {
        if key.len() != 32 && key.len() != 64 {
            return Err(XtsError::InvalidKeyLength);
        }
        Ok(Self {
            _key1: Aes::new(&key[..key.len() / 2]).map_err(|_| XtsError::InvalidKeyLength)?,
            _key2: Aes::new(&key[key.len() / 2..]).map_err(|_| XtsError::InvalidKeyLength)?,
        })
    }

    /// Cifra XTS-AES de una unidad de datos (IEEE 1619 / NIST SP 800-38E).
    /// El tweak de entrada se interpreta como entero little-endian y se cifra
    /// con Key2; el robo de ciphertext sigue el orden de IEEE 1619-2007.
    pub fn encrypt(&self, tweak: &[u8; 16], data: &mut [u8]) -> Result<(), XtsError> {
        if data.len() < 16 {
            return Err(XtsError::DataTooShort);
        }

        let mut running_tweak = self._key2.encrypt_block(tweak);
        let blocks = data.len() / 16;
        let partial = data.len() % 16;
        let full_blocks = if partial == 0 { blocks } else { blocks - 1 };

        for i in 0..full_blocks {
            let mut block = [0u8; 16];
            block.copy_from_slice(&data[16 * i..16 * (i + 1)]);
            let ciphertext = xex_encrypt(&self._key1, &block, &running_tweak);
            data[16 * i..16 * (i + 1)].copy_from_slice(&ciphertext);
            running_tweak = mul_alpha(running_tweak);
        }

        if partial == 0 {
            return Ok(());
        }

        // Ahora running_tweak == T_{n-1}, el tweak del último bloque completo.
        let stolen_start = 16 * (blocks - 1);
        let mut prev_block = [0u8; 16];
        prev_block.copy_from_slice(&data[stolen_start..stolen_start + 16]);
        // CC = XTS-blockEnc(P_{n-1}, T_{n-1}).
        let cc = xex_encrypt(&self._key1, &prev_block, &running_tweak);

        // PP = P_n | CC[partial..], cifrado con T_n = α · T_{n-1}.
        let mut composite = [0u8; 16];
        composite[..partial].copy_from_slice(&data[16 * blocks..]);
        composite[partial..].copy_from_slice(&cc[partial..]);
        let next_tweak = mul_alpha(running_tweak);
        let stolen_ciphertext = xex_encrypt(&self._key1, &composite, &next_tweak);

        data[stolen_start..stolen_start + 16].copy_from_slice(&stolen_ciphertext);
        data[16 * blocks..].copy_from_slice(&cc[..partial]);
        Ok(())
    }

    /// Descifra XTS-AES de una unidad de datos, inverso exacto de `encrypt`.
    pub fn decrypt(&self, tweak: &[u8; 16], data: &mut [u8]) -> Result<(), XtsError> {
        if data.len() < 16 {
            return Err(XtsError::DataTooShort);
        }

        let mut running_tweak = self._key2.encrypt_block(tweak);
        let blocks = data.len() / 16;
        let partial = data.len() % 16;
        let full_blocks = if partial == 0 { blocks } else { blocks - 1 };

        for i in 0..full_blocks {
            let mut block = [0u8; 16];
            block.copy_from_slice(&data[16 * i..16 * (i + 1)]);
            let plaintext = xex_decrypt(&self._key1, &block, &running_tweak);
            data[16 * i..16 * (i + 1)].copy_from_slice(&plaintext);
            running_tweak = mul_alpha(running_tweak);
        }

        if partial == 0 {
            return Ok(());
        }

        // running_tweak == T_{n-1}. Se recupera primero el bloque compuesto
        // cifrado con T_n: PP = P_n | CP.
        let stolen_start = 16 * (blocks - 1);
        let mut stolen = [0u8; 16];
        stolen.copy_from_slice(&data[stolen_start..stolen_start + 16]);
        let next_tweak = mul_alpha(running_tweak);
        let composite = xex_decrypt(&self._key1, &stolen, &next_tweak);

        // CC = C_n | CP, descifrado con T_{n-1} para obtener P_{n-1}.
        let mut cc = [0u8; 16];
        cc[..partial].copy_from_slice(&data[16 * blocks..]);
        cc[partial..].copy_from_slice(&composite[partial..]);
        let prev_plaintext = xex_decrypt(&self._key1, &cc, &running_tweak);

        data[stolen_start..stolen_start + 16].copy_from_slice(&prev_plaintext);
        data[16 * blocks..].copy_from_slice(&composite[..partial]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{XtsAes, XtsError};
    use std::format;
    use std::vec::Vec;

    fn from_hex(value: &str) -> Vec<u8> {
        let bytes = value.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks(2) {
            let nibble = |c: u8| -> u8 {
                match c {
                    b'0'..=b'9' => c - b'0',
                    b'a'..=b'f' => c - b'a' + 10,
                    b'A'..=b'F' => c - b'A' + 10,
                    _ => panic!("hex inválido"),
                }
            };
            out.push((nibble(pair[0]) << 4) | nibble(pair[1]));
        }
        out
    }

    #[test]
    fn key_and_length_validation() {
        for len in [0usize, 16, 31, 33, 48, 63, 65] {
            assert!(matches!(
                XtsAes::new(&[0u8; 65][..len]),
                Err(XtsError::InvalidKeyLength)
            ));
        }
        assert!(XtsAes::new(&[0u8; 32]).is_ok());
        assert!(XtsAes::new(&[0u8; 64]).is_ok());

        let xts = XtsAes::new(&[0u8; 32]).unwrap();
        for len in [0usize, 1, 15] {
            assert!(matches!(
                xts.encrypt(&[0u8; 16], &mut [0u8; 16][..len]),
                Err(XtsError::DataTooShort)
            ));
            assert!(matches!(
                xts.decrypt(&[0u8; 16], &mut [0u8; 16][..len]),
                Err(XtsError::DataTooShort)
            ));
        }
    }

    #[test]
    fn ieee_1619_vector_1() {
        let xts = XtsAes::new(&[0u8; 32]).unwrap();
        let mut data = [0u8; 32];
        xts.encrypt(&[0u8; 16], &mut data).unwrap();
        assert_eq!(
            from_hex(
                &data
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<std::string::String>()
            ),
            from_hex("917cf69ebd68b2ec9b9fe9a3eadda692cd43d2f59598ed858c02c2652fbf922e")
        );
        xts.decrypt(&[0u8; 16], &mut data).unwrap();
        assert_eq!(data, [0u8; 32]);
    }

    #[test]
    fn ieee_1619_cts_vectors() {
        let key = from_hex("fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0bfbebdbcbbbab9b8b7b6b5b4b3b2b1b0");
        let tweak = from_hex("9a785634120000000000000000000000");
        let xts = XtsAes::new(&key).unwrap();
        let mut tweak_array = [0u8; 16];
        tweak_array.copy_from_slice(&tweak);

        let cases = [
            (
                "000102030405060708090a0b0c0d0e0f10",
                "6c1625db4671522d3d7599601de7ca09ed",
            ),
            (
                "000102030405060708090a0b0c0d0e0f1011",
                "d069444b7a7e0cab09e24447d24deb1fedbf",
            ),
            (
                "000102030405060708090a0b0c0d0e0f101112",
                "e5df1351c0544ba1350b3363cd8ef4beedbf9d",
            ),
        ];
        for (plain, cipher) in cases.iter() {
            let mut data = from_hex(plain);
            xts.encrypt(&tweak_array, &mut data).unwrap();
            assert_eq!(
                data.iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<std::string::String>(),
                *cipher
            );
            xts.decrypt(&tweak_array, &mut data).unwrap();
            assert_eq!(data, from_hex(plain));
        }
    }

    #[test]
    fn roundtrip_all_lengths_both_key_sizes() {
        for key_len in [32usize, 64] {
            let xts = XtsAes::new(&[0x5au8; 64][..key_len]).unwrap();
            let tweak = [0x11u8; 16];
            let mut lengths = Vec::new();
            lengths.extend(16..=48);
            lengths.extend([64usize, 80, 96, 512]);
            for len in lengths {
                let original: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
                let mut data = original.clone();
                xts.encrypt(&tweak, &mut data).unwrap();
                assert_ne!(data, original, "length {len}");
                xts.decrypt(&tweak, &mut data).unwrap();
                assert_eq!(data, original, "length {len}");
            }
        }
    }
}
