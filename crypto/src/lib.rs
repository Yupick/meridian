#![no_std]

#[cfg(test)]
extern crate std;

pub mod aes;
pub mod argon2;
pub mod blake2b;
pub mod ed25519;
pub mod gcm;
pub mod sha256;
pub mod sha512;
pub mod xts;

pub use aes::Aes;
pub use argon2::{argon2id, blocks_required, Argon2Error};
pub use blake2b::{Blake2bError, Blake2bHasher};
pub use ed25519::{secret_to_public, sign, verify, Ed25519Error};
pub use gcm::{Aes128Gcm, GcmError};
pub use sha256::{hash as sha256, Sha256};
pub use sha512::{hash as sha512, Sha512};
pub use xts::{XtsAes, XtsError};
