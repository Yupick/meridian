//! Cifrado de volumen sobre el sistema de objetos en disco (T1.5).
//!
//! `EncryptedVolume` envuelve un [`ObjectStore`] y una clave XTS de 64 bytes
//! derivada con Argon2id a partir de una passphrase. Todo el contenido de los
//! objetos de datos viaja en un *frame* autenticado y se cifra con XTS-AES-256
//! antes de entregarlo al almacén; por tanto el `data_sha256` del objeto cubre
//! **ciphertext**, nunca plaintext.
//!
//! # Formato en disco
//!
//! - Objeto 0: cabecera `MRDVOL01` + versión + parámetros KDF + sal + SHA-256
//!   sobre el resto de la cabecera. La sal es dev-only y determinista
//!   (`sha256(b"meridian-volume-dev-salt-v1")`).
//! - Objeto 1: verificador cifrado con un frame conocido. Sirve para rechazar
//!   passphrases incorrectas (`WrongKey`) al abrir.
//! - Objetos `2 + key`: datos de usuario. Las claves 0 y 1 quedan reservadas;
//!   el identificador de objeto es estable aunque el COW cambie el LBA.
//!
//! # Tweak XTS
//!
//! El tweak de 16 bytes es `LE64(object_key) || "MRDVOL01"`. No incluye ni la
//! clave ni el LBA: `ObjectStore::write` asigna el LBA internamente y `locate`
//! puede devolver otro tras un commit COW, así que usar el LBA rompería la
//! lectura de versiones anteriores y haría el tweak impredecible para QA.
//!
//! # Seguridad / alcance
//!
//! La passphrase embebida es **solo de desarrollo/QA**. Producción debe
//! suministrar una passphrase segura desde fuera y sustituir la sal fija por una
//! sal aleatoria por volumen. La clave derivada se guarda en un `KeyMaterial`
//! que se rellena con ceros al caer; los bytes de clave/ciphertext nunca se
//! escriben en la consola.
//!
//! Sin heap: el workspace de Argon2 (8192 bytes) es un `static` con el patrón
//! `UnsafeCell` + `unsafe impl Sync` del resto del kernel (mono-CPU, IRQs off).

use core::cell::UnsafeCell;
use core::fmt;

use meridian_crypto::argon2::{argon2id, blocks_required, Argon2Error};
use meridian_crypto::sha256;
use meridian_crypto::xts::{XtsAes, XtsError};

use crate::blk::{BlockDeviceId, BlockError};
use crate::cap::CapTable;
use crate::obj::{ObjError, ObjectStore, MAX_VALUE_LEN};

pub mod selftest;

/// Objeto reservado para la cabecera del volumen.
pub const HEADER_KEY: u64 = 0;
/// Objeto reservado para el verificador cifrado.
pub const VERIFIER_KEY: u64 = 1;
/// Primer objeto de datos de usuario: `object_key = DATA_KEY_BASE + key`.
pub const DATA_KEY_BASE: u64 = 2;
/// Longitud de la clave XTS derivada.
pub const KEY_LEN: usize = 64;
/// Longitud de la sal Argon2.
pub const SALT_LEN: usize = 32;
/// Máximo plaintext por objeto (deja 48 bytes de margen bajo los 4096 del store).
pub const MAX_PLAINTEXT_LEN: usize = 4000;
/// Longitud total de la cabecera en disco.
pub const HEADER_LEN: usize = HEADER_HASH_OFFSET + sha256::DIGEST_LEN;

const VOLUME_MAGIC: &[u8; 8] = b"MRDVOL01";
const VOLUME_VERSION: u16 = 1;
const OBJECT_MAGIC: &[u8; 8] = b"MRDVOBJ1";

const HEADER_VERSION: usize = 8;
const HEADER_ITERATIONS: usize = 10;
const HEADER_LANES: usize = 14;
const HEADER_MEMORY_KIB: usize = 18;
const HEADER_RESERVED: usize = 22;
const HEADER_SALT: usize = 24;
const HEADER_HASH_OFFSET: usize = HEADER_SALT + SALT_LEN;

const FRAME_LEN_OFFSET: usize = 8;
const FRAME_RESERVED_OFFSET: usize = 12;
const FRAME_PAYLOAD_OFFSET: usize = 16;
const FRAME_OVERHEAD: usize = FRAME_PAYLOAD_OFFSET + sha256::DIGEST_LEN;
pub const MAX_FRAME_LEN: usize = MAX_PLAINTEXT_LEN + FRAME_OVERHEAD;

const _: () = assert!(MAX_FRAME_LEN <= MAX_VALUE_LEN);

const KDF_ITERATIONS: u32 = 2;
const KDF_LANES: u32 = 1;
const KDF_MEMORY_KIB: u32 = 8;
/// `blocks_required(8, 1) == 8` bloques de 1024 bytes.
const KDF_WORKSPACE_BYTES: usize = 8 * 1024;
const SALT_DOMAIN: &[u8] = b"meridian-volume-dev-salt-v1";
const KDF_ASSOCIATED_DATA: &[u8] = b"meridian-volume-key-v1";

/// Passphrase de QA. NUNCA usar en producción; ver la nota del módulo.
const DEV_PASSPHRASE: &[u8] = b"meridian-alpha-volume-key-v1";
const VERIFIER_PLAINTEXT: &[u8] = b"meridian-volume-verifier-v1";

/// Errores del volumen cifrado.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolError {
    Block(BlockError),
    Object(ObjError),
    Kdf(Argon2Error),
    Xts(XtsError),
    BadHeader,
    UnsupportedVersion,
    HeaderHashMismatch,
    BadFrame,
    FrameHashMismatch,
    WrongKey,
    PlaintextTooLarge,
    BufferTooSmall,
    InvalidKey,
}

impl fmt::Display for VolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Block(error) => write!(f, "bloque: {error}"),
            Self::Object(error) => write!(f, "objeto: {error}"),
            Self::Kdf(error) => write!(f, "argon2id: {error:?}"),
            Self::Xts(error) => write!(f, "xts: {error:?}"),
            Self::BadHeader => write!(f, "cabecera de volumen inválida"),
            Self::UnsupportedVersion => write!(f, "versión de volumen no soportada"),
            Self::HeaderHashMismatch => write!(f, "checksum de cabecera incorrecto"),
            Self::BadFrame => write!(f, "frame cifrado inválido"),
            Self::FrameHashMismatch => write!(f, "checksum de frame incorrecto"),
            Self::WrongKey => write!(f, "passphrase incorrecta"),
            Self::PlaintextTooLarge => write!(f, "plaintext mayor que 4000 bytes"),
            Self::BufferTooSmall => write!(f, "búfer de salida demasiado pequeño"),
            Self::InvalidKey => write!(f, "identificador de clave inválido"),
        }
    }
}

impl From<BlockError> for VolError {
    fn from(error: BlockError) -> Self {
        Self::Block(error)
    }
}

impl From<ObjError> for VolError {
    fn from(error: ObjError) -> Self {
        Self::Object(error)
    }
}

/// Clave derivada que se rellena con ceros al salir de ámbito.
struct KeyMaterial([u8; KEY_LEN]);

impl KeyMaterial {
    fn zero(&mut self) {
        self.0.fill(0);
    }
}

impl Drop for KeyMaterial {
    fn drop(&mut self) {
        self.zero();
    }
}

struct GlobalWorkspace(UnsafeCell<[u8; KDF_WORKSPACE_BYTES]>);

// SAFETY: kernel mono-CPU con interrupciones desactivadas. La derivación es
// secuencial y no se reentrantan (mismo modelo que `blk`/`obj`).
unsafe impl Sync for GlobalWorkspace {}

static WORKSPACE: GlobalWorkspace = GlobalWorkspace(UnsafeCell::new([0u8; KDF_WORKSPACE_BYTES]));

fn workspace() -> &'static mut [u8; KDF_WORKSPACE_BYTES] {
    // SAFETY: ver `GlobalWorkspace`.
    unsafe { &mut *WORKSPACE.0.get() }
}

fn put_u16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(buf: &[u8], offset: usize) -> u16 {
    let mut bytes = [0u8; 2];
    bytes.copy_from_slice(&buf[offset..offset + 2]);
    u16::from_le_bytes(bytes)
}

fn get_u32(buf: &[u8], offset: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buf[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

/// Sal fija de desarrollo: `sha256` produce exactamente 32 bytes.
fn dev_salt() -> [u8; SALT_LEN] {
    sha256::hash(SALT_DOMAIN)
}

fn encode_header() -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..8].copy_from_slice(VOLUME_MAGIC);
    put_u16(&mut header, HEADER_VERSION, VOLUME_VERSION);
    put_u32(&mut header, HEADER_ITERATIONS, KDF_ITERATIONS);
    put_u32(&mut header, HEADER_LANES, KDF_LANES);
    put_u32(&mut header, HEADER_MEMORY_KIB, KDF_MEMORY_KIB);
    put_u16(&mut header, HEADER_RESERVED, 0);
    header[HEADER_SALT..HEADER_HASH_OFFSET].copy_from_slice(&dev_salt());
    let sum = sha256::hash(&header[..HEADER_HASH_OFFSET]);
    header[HEADER_HASH_OFFSET..].copy_from_slice(&sum);
    header
}

fn decode_header(header: &[u8; HEADER_LEN]) -> Result<[u8; SALT_LEN], VolError> {
    if &header[..8] != VOLUME_MAGIC {
        return Err(VolError::BadHeader);
    }
    if get_u16(header, HEADER_VERSION) != VOLUME_VERSION {
        return Err(VolError::UnsupportedVersion);
    }
    if get_u32(header, HEADER_ITERATIONS) != KDF_ITERATIONS
        || get_u32(header, HEADER_LANES) != KDF_LANES
        || get_u32(header, HEADER_MEMORY_KIB) != KDF_MEMORY_KIB
        || get_u16(header, HEADER_RESERVED) != 0
    {
        return Err(VolError::BadHeader);
    }
    let sum = sha256::hash(&header[..HEADER_HASH_OFFSET]);
    if &sum[..] != &header[HEADER_HASH_OFFSET..] {
        return Err(VolError::HeaderHashMismatch);
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&header[HEADER_SALT..HEADER_HASH_OFFSET]);
    if salt != dev_salt() {
        return Err(VolError::BadHeader);
    }
    Ok(salt)
}

fn derive_key(passphrase: &[u8], salt: &[u8; SALT_LEN]) -> Result<KeyMaterial, VolError> {
    let blocks = blocks_required(KDF_MEMORY_KIB, KDF_LANES)
        .ok_or(VolError::Kdf(Argon2Error::InvalidParameters))?;
    if blocks * 1024 > KDF_WORKSPACE_BYTES {
        return Err(VolError::Kdf(Argon2Error::WorkspaceTooSmall));
    }
    let mut key = KeyMaterial([0u8; KEY_LEN]);
    let result = argon2id(
        passphrase,
        salt,
        &[],
        KDF_ASSOCIATED_DATA,
        KDF_ITERATIONS,
        KDF_LANES,
        KDF_MEMORY_KIB,
        workspace(),
        &mut key.0,
    );
    if result.is_err() {
        key.zero();
    }
    result.map_err(VolError::Kdf)?;
    Ok(key)
}

/// Tweak estable por objeto: `LE64(object_key) || "MRDVOL01"`.
/// Sin LBA (el COW lo cambia) y sin clave (el tweak es público).
fn tweak(object_key: u64) -> [u8; 16] {
    let mut tweak = [0u8; 16];
    tweak[..8].copy_from_slice(&object_key.to_le_bytes());
    tweak[8..].copy_from_slice(VOLUME_MAGIC);
    tweak
}

fn object_key(key: u64) -> Result<u64, VolError> {
    DATA_KEY_BASE.checked_add(key).ok_or(VolError::InvalidKey)
}

/// Construye `magic || len || reserved || payload || sha256(magic||len||payload)`.
fn encode_frame(plaintext: &[u8], out: &mut [u8; MAX_FRAME_LEN]) -> Result<usize, VolError> {
    if plaintext.len() > MAX_PLAINTEXT_LEN {
        return Err(VolError::PlaintextTooLarge);
    }
    let total = FRAME_OVERHEAD + plaintext.len();
    out[..8].copy_from_slice(OBJECT_MAGIC);
    put_u32(out, FRAME_LEN_OFFSET, plaintext.len() as u32);
    put_u32(out, FRAME_RESERVED_OFFSET, 0);
    out[FRAME_PAYLOAD_OFFSET..FRAME_PAYLOAD_OFFSET + plaintext.len()].copy_from_slice(plaintext);
    let mut hasher = sha256::Sha256::new();
    hasher.update(&out[..8]);
    hasher.update(&out[FRAME_LEN_OFFSET..FRAME_LEN_OFFSET + 4]);
    hasher.update(&out[FRAME_PAYLOAD_OFFSET..FRAME_PAYLOAD_OFFSET + plaintext.len()]);
    let mut sum = [0u8; sha256::DIGEST_LEN];
    hasher.finalize(&mut sum);
    out[FRAME_PAYLOAD_OFFSET + plaintext.len()..total].copy_from_slice(&sum);
    Ok(total)
}

/// Valida un frame ya descifrado y devuelve su payload.
fn decode_frame<'a>(frame: &'a [u8], expected_len: Option<usize>) -> Result<&'a [u8], VolError> {
    if frame.len() < FRAME_OVERHEAD || &frame[..8] != OBJECT_MAGIC {
        return Err(VolError::BadFrame);
    }
    let payload_len = get_u32(frame, FRAME_LEN_OFFSET) as usize;
    if payload_len > MAX_PLAINTEXT_LEN || get_u32(frame, FRAME_RESERVED_OFFSET) != 0 {
        return Err(VolError::BadFrame);
    }
    if frame.len() != FRAME_OVERHEAD + payload_len {
        return Err(VolError::BadFrame);
    }
    if expected_len.map_or(false, |expected| payload_len != expected) {
        return Err(VolError::BadFrame);
    }
    let payload_end = FRAME_PAYLOAD_OFFSET + payload_len;
    let mut hasher = sha256::Sha256::new();
    hasher.update(&frame[..8]);
    hasher.update(&frame[FRAME_LEN_OFFSET..FRAME_LEN_OFFSET + 4]);
    hasher.update(&frame[FRAME_PAYLOAD_OFFSET..payload_end]);
    let mut sum = [0u8; sha256::DIGEST_LEN];
    hasher.finalize(&mut sum);
    if &sum[..] != &frame[payload_end..] {
        return Err(VolError::FrameHashMismatch);
    }
    Ok(&frame[FRAME_PAYLOAD_OFFSET..payload_end])
}

/// Volumen cifrado: estado del store + clave XTS derivada con Argon2id.
pub struct EncryptedVolume {
    store: ObjectStore,
    key: KeyMaterial,
}

impl Drop for EncryptedVolume {
    fn drop(&mut self) {
        // El campo `key` también se limpia solo; reforzamos el contrato.
        self.key.zero();
    }
}

impl EncryptedVolume {
    /// Formatea el disco, escribe cabecera y verificador y deriva la clave.
    pub fn format(
        caps: &CapTable,
        slot: usize,
        device: BlockDeviceId,
        passphrase: &[u8],
    ) -> Result<Self, VolError> {
        let mut store = ObjectStore::format(caps, slot, device)?;
        let header = encode_header();
        store.write(caps, slot, HEADER_KEY, &header)?;

        let salt = decode_header(&header)?;
        let key = derive_key(passphrase, &salt)?;
        let cipher = XtsAes::new(&key.0).map_err(VolError::Xts)?;

        let mut frame = [0u8; MAX_FRAME_LEN];
        let frame_len = encode_frame(VERIFIER_PLAINTEXT, &mut frame)?;
        cipher
            .encrypt(&tweak(VERIFIER_KEY), &mut frame[..frame_len])
            .map_err(VolError::Xts)?;
        store.write(caps, slot, VERIFIER_KEY, &frame[..frame_len])?;

        Ok(Self { store, key })
    }

    /// Abre un volumen existente. Una passphrase incorrecta devuelve
    /// [`VolError::WrongKey`] tras derivar clave y validar el verificador.
    pub fn open(
        caps: &CapTable,
        slot: usize,
        device: BlockDeviceId,
        passphrase: &[u8],
    ) -> Result<Self, VolError> {
        let store = ObjectStore::open(caps, slot, device)?;
        let mut header = [0u8; HEADER_LEN];
        let header_len = store.read(caps, slot, HEADER_KEY, &mut header)?;
        if header_len != HEADER_LEN {
            return Err(VolError::BadHeader);
        }
        let salt = decode_header(&header)?;
        let key = derive_key(passphrase, &salt)?;
        let cipher = XtsAes::new(&key.0).map_err(VolError::Xts)?;

        let mut frame = [0u8; MAX_VALUE_LEN];
        let frame_len = store.read(caps, slot, VERIFIER_KEY, &mut frame)?;
        cipher
            .decrypt(&tweak(VERIFIER_KEY), &mut frame[..frame_len])
            .map_err(VolError::Xts)?;
        // Con una clave distinta el frame no puede validar: se reporta
        // WrongKey sin filtrar ni la clave ni el frame descifrado.
        decode_frame(&frame[..frame_len], Some(VERIFIER_PLAINTEXT.len()))
            .map_err(|_| VolError::WrongKey)?;

        Ok(Self { store, key })
    }

    /// Lee y descifra un objeto de usuario; devuelve bytes de plaintext.
    pub fn read(
        &self,
        caps: &CapTable,
        slot: usize,
        key: u64,
        out: &mut [u8],
    ) -> Result<usize, VolError> {
        let object_key = object_key(key)?;
        let mut buffer = [0u8; MAX_VALUE_LEN];
        let len = self.store.read(caps, slot, object_key, &mut buffer)?;
        let cipher = XtsAes::new(&self.key.0).map_err(VolError::Xts)?;
        cipher
            .decrypt(&tweak(object_key), &mut buffer[..len])
            .map_err(VolError::Xts)?;
        let payload = decode_frame(&buffer[..len], None)?;
        if out.len() < payload.len() {
            return Err(VolError::BufferTooSmall);
        }
        out[..payload.len()].copy_from_slice(payload);
        Ok(payload.len())
    }

    /// Cifra y escribe un objeto de usuario con copy-on-write.
    pub fn write(
        &mut self,
        caps: &CapTable,
        slot: usize,
        key: u64,
        data: &[u8],
    ) -> Result<(), VolError> {
        let object_key = object_key(key)?;
        let mut frame = [0u8; MAX_FRAME_LEN];
        let frame_len = encode_frame(data, &mut frame)?;
        let cipher = XtsAes::new(&self.key.0).map_err(VolError::Xts)?;
        cipher
            .encrypt(&tweak(object_key), &mut frame[..frame_len])
            .map_err(VolError::Xts)?;
        self.store
            .write(caps, slot, object_key, &frame[..frame_len])?;
        Ok(())
    }

    /// Solo QA/inspección: ciphertext del ObjectStore sin descifrar.
    pub fn raw_read(
        &self,
        caps: &CapTable,
        slot: usize,
        key: u64,
        out: &mut [u8],
    ) -> Result<usize, VolError> {
        let object_key = object_key(key)?;
        Ok(self.store.read(caps, slot, object_key, out)?)
    }
}
